//! Work Outline instruction delivery in the composer (Milestone 2, Part B).
//!
//! - **Context reset.** A harness that can only apply a new instruction
//!   revision in a fresh native session (Codex, replace strategy
//!   `FreshSession`) makes the host refuse the send with
//!   `instructionContextResetRequired` and write no user message. The refusal
//!   arrives on `WatchQueue` frames (`rejectedSends`, kept in
//!   `AppState::rejected_sends`). The composer asks; Confirm resends the same
//!   request as `runWithInstructions { …, contextResetConfirmed: true }` under
//!   the SAME message id (the host then drops it from `rejectedSends`);
//!   Cancel puts the text back in the draft.
//! - **Queued rows.** A queued row the host gated with
//!   `QueueDeliveryGate::InstructionContextResetRequired` shows the reason with
//!   Confirm (the `confirmInstructionContextReset` command: the host clears the
//!   gate, marks the row confirmed, and drains it) and Cancel (the row leaves
//!   the queue and its text returns to the draft).
//! - **Host unsupported.** An ingestion-time refusal
//!   (`instruction-delivery/{…}` on the RPC failure,
//!   `instructionHostUnsupported`) shows the host's own actionable message;
//!   the ordinary failed-send path keeps the draft.
use std::collections::{HashMap, HashSet, VecDeque};

use gpui::{Context, SharedString, div, prelude::*, px};
use zeron_doc::{MessagePart, SessionCommandPayload, SessionMessageEntry};
use zeron_proto::{
    InstructionDeliveryOutcome, InstructionDeliveryRejection, InstructionRejectedSend,
    InstructionRevisionPin,
};
use zeron_rpc::methods;

use super::Composer;
use crate::theme::Theme;

/// The question the composer asks before a context reset.
pub(crate) const CONTEXT_RESET_PROMPT: &str = "Start a fresh session with the new instructions? \
     The agent loses this conversation\u{2019}s context.";

/// The reason a gated queued row shows ahead of its text.
pub(crate) const QUEUED_CONTEXT_RESET_REASON: &str = "New instructions need a fresh session";

/// How many recent sends keep their typed text for a Cancel hand-back.
const SENT_DRAFTS_CAP: usize = 32;

/// Composer-local bookkeeping for refused sends.
#[derive(Default)]
pub(crate) struct InstructionResetState {
    /// `messageId → typed text` of recent sends: Cancel restores the user's
    /// own words rather than the folded prompt.
    sent_drafts: HashMap<String, String>,
    sent_order: VecDeque<String>,
    /// Refusals answered here (by command id): hidden until the next frame
    /// drops them (a confirmed resend supersedes the command host-side).
    answered: HashSet<String>,
    /// Queued rows whose context-reset confirmation is in flight.
    confirming_rows: HashSet<String>,
}

impl InstructionResetState {
    pub(crate) fn remember_send(&mut self, message_id: &str, typed: &str) {
        if self
            .sent_drafts
            .insert(message_id.to_owned(), typed.to_owned())
            .is_none()
        {
            self.sent_order.push_back(message_id.to_owned());
        }
        while self.sent_order.len() > SENT_DRAFTS_CAP {
            if let Some(oldest) = self.sent_order.pop_front() {
                self.sent_drafts.remove(&oldest);
            }
        }
    }
}

/// The text a failed send shows: an instruction-delivery refusal's own
/// message (it names the upgrade), else the generic `Send failed: …`.
pub(crate) fn send_failure_text(raw: &str) -> String {
    match InstructionDeliveryRejection::from_wire_message(raw) {
        Some(rejection) => rejection.message,
        None => format!("Send failed: {raw}"),
    }
}

/// The `runWithInstructions` resend that confirms a context reset.
pub(crate) fn confirmed_resend(
    rejected: &InstructionRejectedSend,
) -> Option<SessionCommandPayload> {
    let InstructionDeliveryOutcome::InstructionContextResetRequired {
        revision_id,
        revision_hash,
        ..
    } = &rejected.outcome
    else {
        return None;
    };
    let mut request = rejected.request.clone();
    request.instructions = None;
    Some(SessionCommandPayload::RunWithInstructions {
        request,
        message_id: rejected.message_id.clone(),
        instructions: InstructionRevisionPin {
            revision_id: revision_id.clone(),
            revision_hash: revision_hash.clone(),
        },
        context_reset_confirmed: true,
    })
}

/// The command that confirms a gated queued row's context reset.
pub(crate) fn confirm_queued_reset_command(
    queued_message_id: &str,
    revision_id: &str,
) -> SessionCommandPayload {
    SessionCommandPayload::ConfirmInstructionContextReset {
        queued_message_id: queued_message_id.to_owned(),
        revision_id: revision_id.to_owned(),
    }
}

impl Composer {
    /// Put `text` back in the draft (appended when the user has typed
    /// something since, never twice).
    fn restore_into_draft(&mut self, text: String, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            let current = input.text().to_owned();
            let restored = if current.trim().is_empty() {
                text
            } else if current.contains(text.as_str()) {
                current
            } else {
                format!("{current}\n\n{text}")
            };
            input.set_text(restored, cx);
        });
    }

    /// Confirm a gated queued row: `confirmInstructionContextReset`.
    pub(crate) fn confirm_queued_context_reset(
        &mut self,
        id: String,
        revision_id: String,
        cx: &mut Context<Self>,
    ) {
        let (Some(engine), Some(chat_id)) = (
            self.state.read(cx).engine().cloned(),
            self.state.read(cx).selected_chat.clone(),
        ) else {
            return;
        };
        if !self.instruction_reset.confirming_rows.insert(id.clone()) {
            return;
        }
        let Ok(command) = serde_json::to_value(confirm_queued_reset_command(&id, &revision_id))
        else {
            return;
        };
        cx.notify();
        let params = serde_json::json!({ "chatId": chat_id, "command": command });
        cx.spawn(async move |this, cx| {
            let result = crate::attachments::call_with_timeout(
                &engine,
                cx.background_executor(),
                methods::QUEUE_COMMAND,
                params,
                std::time::Duration::from_secs(30),
            )
            .await;
            this.update(cx, |composer, cx| {
                composer.instruction_reset.confirming_rows.remove(&id);
                if let Err(raw) = result {
                    composer.failure =
                        Some(format!("Couldn\u{2019}t start a fresh session: {raw}").into());
                    composer.failure_key = Some(chat_id.clone());
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Cancel a gated queued row: it leaves the queue and its text returns
    /// to the draft (attachments stay on the host).
    pub(crate) fn cancel_queued_context_reset(
        &mut self,
        id: String,
        text: String,
        cx: &mut Context<Self>,
    ) {
        let supported = {
            let state = self.state.read(cx);
            state.selected_chat.as_deref().is_some_and(|chat_id| {
                state.chat_host_supports(
                    chat_id,
                    zeron_proto::capabilities::MESSAGE_QUEUE_ACTIONS_V1,
                )
            })
        };
        if supported && text != crate::attachments::ATTACHMENT_ONLY_TEXT {
            self.restore_into_draft(text, cx);
        }
        self.remove_queued(id, cx);
    }

    /// Confirm / Cancel for a row gated on a context reset.
    pub(crate) fn queued_context_reset_actions(
        &self,
        id: &str,
        revision_id: &str,
        text: String,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let confirming = self.instruction_reset.confirming_rows.contains(id);
        let removing = self.queue_removing.contains(id);
        let busy = confirming || removing;
        let button = |selector: String, label: &'static str, primary: bool| {
            let debug = selector.clone();
            div()
                .id(SharedString::from(selector))
                .debug_selector(move || debug)
                .flex_none()
                .px(px(8.0))
                .py(px(3.0))
                .rounded(px(5.0))
                .text_size(px(11.5))
                .when(primary, |el| {
                    el.bg(theme.accent.opacity(0.15)).text_color(theme.accent)
                })
                .when(!primary, |el| el.text_color(theme.text_muted))
                .when(busy, |el| el.opacity(0.5))
                .when(!busy, |el| {
                    el.cursor_pointer()
                        .hover(|el| el.bg(theme.element_hover))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                })
                .child(label)
        };
        let confirm_id = id.to_owned();
        let confirm_revision = revision_id.to_owned();
        let cancel_id = id.to_owned();
        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .child(
                button(format!("queue-reset-cancel-{id}"), "Cancel", false).when(!busy, |el| {
                    el.on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.cancel_queued_context_reset(cancel_id.clone(), text.clone(), cx);
                    }))
                }),
            )
            .child(
                button(
                    format!("queue-reset-confirm-{id}"),
                    if confirming {
                        "Starting\u{2026}"
                    } else {
                        "Start fresh session"
                    },
                    true,
                )
                .tooltip(|_, cx| {
                    cx.new(|_| crate::work_outline::chips::TextTooltip(CONTEXT_RESET_PROMPT.into()))
                        .into()
                })
                .when(!busy, |el| {
                    el.on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.confirm_queued_context_reset(
                            confirm_id.clone(),
                            confirm_revision.clone(),
                            cx,
                        );
                    }))
                }),
            )
            .into_any_element()
    }

    /// The newest refused send of the selected chat that waits on a context
    /// reset confirmation, unless already answered here.
    pub(crate) fn pending_context_reset(&self, cx: &gpui::App) -> Option<InstructionRejectedSend> {
        let state = self.state.read(cx);
        state.selected_chat.as_ref()?;
        state
            .rejected_sends
            .iter()
            .rev()
            .find(|rejected| {
                matches!(
                    rejected.outcome,
                    InstructionDeliveryOutcome::InstructionContextResetRequired { .. }
                ) && !self
                    .instruction_reset
                    .answered
                    .contains(&rejected.command_id)
            })
            .cloned()
    }

    /// Confirm: resend under the same message id with
    /// `contextResetConfirmed: true`. The echo and Working overlay return
    /// while the host starts the fresh session.
    pub(crate) fn confirm_context_reset(&mut self, command_id: &str, cx: &mut Context<Self>) {
        let Some(rejected) = self
            .state
            .read(cx)
            .rejected_sends
            .iter()
            .find(|rejected| rejected.command_id == command_id)
            .cloned()
        else {
            return;
        };
        let Some(command) = confirmed_resend(&rejected) else {
            return;
        };
        let (Some(engine), Some(chat_id)) = (
            self.state.read(cx).engine().cloned(),
            self.state.read(cx).selected_chat.clone(),
        ) else {
            return;
        };
        let Ok(command) = serde_json::to_value(&command) else {
            return;
        };
        self.instruction_reset
            .answered
            .insert(command_id.to_owned());
        let message_id = rejected.message_id.clone();
        self.state.update(cx, |state, cx| {
            state.push_echo(
                &chat_id,
                SessionMessageEntry {
                    id: message_id.clone(),
                    role: zeron_doc::MessageRole::User,
                    parts: vec![MessagePart::Text {
                        id: "t0".into(),
                        text: rejected.request.prompt.clone(),
                    }],
                    created_at: chrono::Utc::now().timestamp_millis(),
                    device_id: "local".into(),
                    status: None,
                    continuation_of: None,
                },
            );
            state.begin_pending_send(&chat_id, &message_id, chrono::Utc::now());
            cx.notify();
        });
        cx.notify();
        let params = serde_json::json!({ "chatId": chat_id, "command": command });
        let command_id = command_id.to_owned();
        cx.spawn(async move |this, cx| {
            let result = crate::attachments::call_with_timeout(
                &engine,
                cx.background_executor(),
                methods::QUEUE_COMMAND,
                params,
                std::time::Duration::from_secs(30),
            )
            .await;
            if let Err(raw) = result {
                this.update(cx, |composer, cx| {
                    // Ask again: the refusal is still listed.
                    composer.instruction_reset.answered.remove(&command_id);
                    composer.failure = Some(send_failure_text(&raw).into());
                    composer.failure_key = Some(chat_id.clone());
                    composer.state.update(cx, |state, cx| {
                        state.remove_echo(&chat_id, &message_id);
                        state.end_pending_send(&chat_id, &message_id);
                        cx.notify();
                    });
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Cancel: nothing is sent; the message's text returns to the draft
    /// (appended when the user has typed something since).
    pub(crate) fn cancel_context_reset(&mut self, command_id: &str, cx: &mut Context<Self>) {
        let Some(rejected) = self
            .state
            .read(cx)
            .rejected_sends
            .iter()
            .find(|rejected| rejected.command_id == command_id)
            .cloned()
        else {
            return;
        };
        self.instruction_reset
            .answered
            .insert(command_id.to_owned());
        let text = self
            .instruction_reset
            .sent_drafts
            .get(&rejected.message_id)
            .cloned()
            .unwrap_or_else(|| rejected.request.prompt.clone());
        self.restore_into_draft(text, cx);
        cx.notify();
    }

    /// The confirmation card above the composer box.
    pub(super) fn render_context_reset_prompt(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let rejected = self.pending_context_reset(cx)?;
        let confirm_id = rejected.command_id.clone();
        let cancel_id = rejected.command_id.clone();
        let button = |id: &'static str, label: &'static str, primary: bool| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .flex_none()
                .px(px(10.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .cursor_pointer()
                .text_size(px(12.0))
                .when(primary, |el| {
                    el.bg(theme.accent.opacity(0.15))
                        .text_color(theme.accent)
                        .hover(|el| el.bg(theme.accent.opacity(0.25)))
                })
                .when(!primary, |el| {
                    el.text_color(theme.text_muted)
                        .hover(|el| el.bg(theme.element_hover))
                })
                .child(label)
        };
        Some(
            div()
                .id("composer-context-reset")
                .debug_selector(|| "composer-context-reset".into())
                .mx(px(4.0))
                .mt(px(6.0))
                .px(px(12.0))
                .py(px(8.0))
                .flex()
                .flex_col()
                .gap(px(6.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(theme.warning.opacity(0.4))
                .bg(theme.surface_raised)
                .text_size(px(12.5))
                .text_color(theme.text)
                .child(SharedString::from(CONTEXT_RESET_PROMPT))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            button("composer-context-reset-cancel", "Cancel", false).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.cancel_context_reset(&cancel_id, cx)
                                }),
                            ),
                        )
                        .child(
                            button(
                                "composer-context-reset-confirm",
                                "Start fresh session",
                                true,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| this.confirm_context_reset(&confirm_id, cx),
                            )),
                        ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::state::AppState;
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use zeron_proto::{HarnessId, InstructionHostUnsupportedReason, RunRequest, SandboxLevel};

    pub(crate) fn rejected(command_id: &str, message_id: &str) -> InstructionRejectedSend {
        InstructionRejectedSend {
            command_id: command_id.into(),
            message_id: message_id.into(),
            request: RunRequest {
                prompt: format!("prompt {message_id}"),
                harness: Some(HarnessId::Codex),
                model: None,
                reasoning: None,
                model_options: Default::default(),
                cwd: "/work".into(),
                sandbox: SandboxLevel::WorkspaceWrite,
                auto_approve: false,
                resume: None,
                attachments: Vec::new(),
                worktree: None,
                instructions: None,
            },
            issued_at: 1,
            outcome: InstructionDeliveryOutcome::InstructionContextResetRequired {
                harness: HarnessId::Codex,
                message_id: Some(message_id.into()),
                revision_id: "rev-b".into(),
                revision_hash: "hash-b".into(),
                previous_revision_id: Some("rev-a".into()),
            },
            message: "Codex must start a fresh session".into(),
        }
    }

    pub(crate) struct Harness {
        pub _runtime: tokio::runtime::Runtime,
        pub requests: tokio::sync::mpsc::Receiver<String>,
        pub replies: tokio::sync::mpsc::Sender<String>,
        pub state: Entity<AppState>,
        pub composer: Entity<Composer>,
        pub _dir: tempfile::TempDir,
    }

    /// A drawn Composer over an `AppState` with a scripted engine whose
    /// requests land in `requests`.
    pub(crate) fn harness(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let guard = runtime.enter();
        let (out, requests) = tokio::sync::mpsc::channel::<String>(16);
        let (replies, inbound) = tokio::sync::mpsc::channel::<String>(16);
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::app_menus::init(cx);
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
            crate::settings::init(crate::settings::UiSettings::default(), &data_dir, cx);
        });
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| {
            state.set_test_engine(crate::state::EngineHandle::from_test_client(
                zeron_rpc::RpcClient::new(out, inbound),
            ));
            state.selected_chat = Some("c".into());
        });
        let window_state = state.clone();
        let (composer, vcx) = cx.add_window_view(move |_, cx| Composer::new(window_state, cx));
        drop(guard);
        (
            Harness {
                _runtime: runtime,
                requests,
                replies,
                state,
                composer,
                _dir: dir,
            },
            vcx,
        )
    }

    pub(crate) fn draw(vcx: &mut VisualTestContext) {
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
    }

    impl Harness {
        /// Every request sent so far with `method` (others, e.g. the
        /// composer's own harness listing, are dropped).
        pub(crate) fn sent(&mut self, method: &str) -> Vec<serde_json::Value> {
            let mut out = Vec::new();
            while let Ok(frame) = self.requests.try_recv() {
                let request: serde_json::Value = serde_json::from_str(&frame).unwrap();
                if request["method"] == method {
                    out.push(request);
                }
            }
            out
        }
    }

    fn click(vcx: &mut VisualTestContext, selector: &'static str) {
        let at = vcx.debug_bounds(selector).expect(selector).center();
        vcx.simulate_click(at, gpui::Modifiers::default());
        draw(vcx);
    }

    #[gpui::test]
    fn context_reset_confirm_resends_with_the_same_message_id(cx: &mut TestAppContext) {
        let (mut h, vcx) = harness(cx);
        let _guard = h._runtime.enter();
        h.state.update(vcx, |state, _| {
            state.push_echo(
                "c",
                SessionMessageEntry {
                    id: "m1".into(),
                    role: zeron_doc::MessageRole::User,
                    parts: Vec::new(),
                    created_at: 1,
                    device_id: "local".into(),
                    status: None,
                    continuation_of: None,
                },
            );
            state.begin_pending_send("c", "m1", chrono::Utc::now());
            // The refusal arrives on the queue frame: no message landed.
            state.apply_queue_frame("c", Vec::new(), vec![rejected("cmd-1", "m1")]);
            assert!(state.pending_echoes().is_empty());
            assert!(!state.send_pending("c", chrono::Utc::now()));
        });
        draw(vcx);
        assert!(vcx.debug_bounds("composer-context-reset").is_some());

        click(vcx, "composer-context-reset-confirm");
        assert!(vcx.debug_bounds("composer-context-reset").is_none());
        let sent = h.sent(methods::QUEUE_COMMAND);
        assert_eq!(sent.len(), 1, "one resend");
        let request = &sent[0];
        assert_eq!(request["params"]["chatId"], "c");
        let command = &request["params"]["command"];
        assert_eq!(command["kind"], "runWithInstructions");
        assert_eq!(command["messageId"], "m1");
        assert_eq!(command["contextResetConfirmed"], true);
        assert_eq!(command["instructions"]["revisionId"], "rev-b");
        assert_eq!(command["instructions"]["revisionHash"], "hash-b");
        assert_eq!(command["request"]["prompt"], "prompt m1");
        assert!(command["request"].get("instructions").is_none());
        h.state.read_with(vcx, |state, _| {
            assert_eq!(state.pending_echoes().len(), 1, "the echo is back");
        });
    }

    #[gpui::test]
    fn context_reset_cancel_restores_the_typed_draft(cx: &mut TestAppContext) {
        let (mut h, vcx) = harness(cx);
        let _guard = h._runtime.enter();
        h.composer.update(vcx, |composer, _| {
            composer
                .instruction_reset
                .remember_send("m2", "my own words");
        });
        h.state.update(vcx, |state, _| {
            state.apply_queue_frame("c", Vec::new(), vec![rejected("cmd-2", "m2")]);
        });
        draw(vcx);
        click(vcx, "composer-context-reset-cancel");
        assert!(vcx.debug_bounds("composer-context-reset").is_none());
        assert!(
            h.sent(methods::QUEUE_COMMAND).is_empty(),
            "nothing was sent"
        );
        h.composer.read_with(vcx, |composer, cx| {
            assert_eq!(composer.input.read(cx).text(), "my own words");
        });
        // A refusal of another kind never asks.
        h.state.update(vcx, |state, _| {
            let mut other = rejected("cmd-3", "m3");
            other.outcome = InstructionDeliveryOutcome::InstructionHostUnsupported {
                host_device_id: "d".into(),
                harness: HarnessId::Codex,
                required_capability: "work-outline-instruction-envelope-v1/codex".into(),
                reason: InstructionHostUnsupportedReason::HarnessUnsupported,
                device_name: None,
                device_version: None,
            };
            state.apply_queue_frame("c", Vec::new(), vec![other]);
        });
        draw(vcx);
        assert!(vcx.debug_bounds("composer-context-reset").is_none());
    }

    #[gpui::test]
    fn gated_queued_rows_confirm_or_cancel_the_context_reset(cx: &mut TestAppContext) {
        let (mut h, vcx) = harness(cx);
        let _guard = h._runtime.enter();
        let gate = zeron_doc::QueueDeliveryGate::InstructionContextResetRequired {
            harness: HarnessId::Codex,
            revision_id: "rev-b".into(),
            revision_hash: "hash-b".into(),
            previous_revision_id: Some("rev-a".into()),
            since_ms: 1,
        };
        h.state.update(vcx, |state, _| {
            state.chats = vec![
                serde_json::from_value(serde_json::json!({
                    "id": "c", "deviceId": "d", "archived": false,
                    "createdAt": "2026-01-01T00:00:00Z",
                }))
                .unwrap(),
            ];
            state.devices = vec![
                serde_json::from_value(serde_json::json!({
                    "id": "d", "name": "Studio", "platform": "macos", "lastSeenAt": null,
                    "capabilities": [zeron_proto::capabilities::MESSAGE_QUEUE_ACTIONS_V1],
                }))
                .unwrap(),
            ];
            let mut first = zeron_doc::QueuedMessage::new("q1", "first message", "d");
            first.delivery_gate = Some(gate.clone());
            let mut second = zeron_doc::QueuedMessage::new("q2", "second message", "d");
            second.delivery_gate = Some(gate);
            let plain = zeron_doc::QueuedMessage::new("q3", "plain", "d");
            state.queue = vec![first, second, plain];
        });
        draw(vcx);
        assert!(vcx.debug_bounds("queue-reset-confirm-q1").is_some());
        assert!(vcx.debug_bounds("queue-reset-cancel-q1").is_some());
        assert!(
            vcx.debug_bounds("queue-reset-confirm-q3").is_none(),
            "ungated rows keep their usual actions"
        );
        h.sent(methods::QUEUE_COMMAND);

        click(vcx, "queue-reset-confirm-q1");
        let sent = h.sent(methods::QUEUE_COMMAND);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["params"]["chatId"], "c");
        assert_eq!(
            sent[0]["params"]["command"],
            serde_json::json!({
                "kind": "confirmInstructionContextReset",
                "queuedMessageId": "q1",
                "revisionId": "rev-b",
            })
        );
        // In flight: a second click sends nothing.
        click(vcx, "queue-reset-confirm-q1");
        assert!(h.sent(methods::QUEUE_COMMAND).is_empty());

        // Cancel: the row is removed and its text returns to the draft.
        click(vcx, "queue-reset-cancel-q2");
        let removed = h.sent(methods::REMOVE_QUEUED_MESSAGE);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0]["params"]["id"], "q2");
        h.composer.read_with(vcx, |composer, cx| {
            assert_eq!(composer.input.read(cx).text(), "second message");
        });
    }
}
