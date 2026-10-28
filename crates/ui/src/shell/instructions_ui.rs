//! Shell-owned instruction surfaces (Work Outline Milestone 2):
//!
//! - the Chat drift line in the full-chat header metadata ("Instructions
//!   changed since this Chat's applied revision." with Compare / Apply, and
//!   "Selected r… · Delivered r…" when the host reports a different
//!   delivery — never inferred from selection);
//! - the revision comparison modal (entry-level summary plus the generic
//!   line diff of `work_outline::revision_diff`) with Apply;
//! - Apply: `createInstructionRevision` then `applyInstructionRevision`
//!   (reason `explicit`), with operation ids that stay stable across retries
//!   of the same inputs ([`ApplyState`]). On a harness whose replace
//!   strategy is `FreshSession` (Codex) with an existing native session,
//!   Apply first states that the next message starts a fresh session and
//!   writes nothing until the user confirms;
//! - Keep current session: when the selection differs from what a
//!   `FreshSession` Chat's session received, `keepInstructionSession`
//!   reverts the selection (the drift notice stays so Apply remains);
//! - Create Instruction from Chat: excerpt from the transcript selection, or
//!   the last assistant message when nothing is selected; target this item /
//!   parent workstream / Space root; explicit Save.
//!
//! Every surface reads the Work Outline snapshot from `AppState` and works
//! locally (pure resolver, local compare); RPC is used only for writes and
//! for an applied revision missing from the snapshot. An engine that still
//! answers instruction operations with `unsupported` shows a calm "not
//! available on this host" message.
use super::work_outline_ui::WorkOutlineModal;
use super::*;

use std::sync::Arc;

use mast_proto::{
    ApplyInstructionRevisionParams, ChatIndicator, ChatInstructionState,
    CreateInstructionRevisionParams, CreateWorkItemInstructionParams, InstructionApplicationReason,
    InstructionRevisionComparison, InstructionScope, InstructionSource, WorkOutlineQueryRequest,
    WorkOutlineSnapshot,
};

use crate::composer::skill_completion::{SkillCompletion, SkillInsertion, SkillSource};
use crate::work_outline::actions::{OperationSlot, expect_revision, instruction_error_text};
use crate::work_outline::hints::{HintKey, WithHint};
use crate::work_outline::instructions::{self, CompareUnavailable};
use crate::work_outline::{model, revision_diff};

/// Tooltip for both Apply buttons (chat header, Compare dialog).
const APPLY_HINT: &str =
    "Use the current instructions from the next turn (Codex confirms a fresh session first)";

/// The consequence of Apply on a Chat whose harness can only change
/// instructions in a fresh native session (replace strategy `FreshSession`)
/// and that already has one: shown, and confirmed, before anything is
/// written. `None` when Apply keeps the conversation.
pub(crate) fn fresh_session_apply_warning(state: &AppState, chat_id: &str) -> Option<String> {
    let chat = state.chats.iter().find(|chat| chat.id == chat_id)?;
    let harness = chat.config.as_ref()?.harness;
    if !needs_fresh_session(harness) || !has_native_session(chat) {
        return None;
    }
    let label = harness_display_name(harness)
        .unwrap_or_else(|| crate::settings::harnesses::cli_name(harness));
    Some(format!(
        "Applying starts a fresh {label} session on your next message (this conversation\u{2019}s \
         context won\u{2019}t carry over). Apply anyway?"
    ))
}

fn harness_display_name(harness: mast_proto::HarnessId) -> Option<&'static str> {
    match harness {
        mast_proto::HarnessId::Codex => Some("Codex"),
        mast_proto::HarnessId::ClaudeCode => Some("Claude Code"),
        _ => None,
    }
}

fn needs_fresh_session(harness: mast_proto::HarnessId) -> bool {
    mast_harness::declared_replace_strategy(harness)
        == Some(mast_harness::InstructionReplaceStrategy::FreshSession)
}

fn has_native_session(chat: &mast_proto::Chat) -> bool {
    chat.harness_session_id
        .as_deref()
        .is_some_and(|id| !id.trim().is_empty())
}

/// Whether "Keep current session" is offered: the Chat's selected revision
/// differs from what its Execution Session received (or nothing was
/// delivered), on a `FreshSession` harness with an existing native session,
/// so the next send would need a context reset.
pub(crate) fn can_keep_current_session(
    state: &AppState,
    chat_id: &str,
    chat_state: &ChatInstructionState,
) -> bool {
    let Some(selected) = chat_state.selected_revision_id.as_deref() else {
        return false;
    };
    if chat_state.delivered_revision_id.as_deref() == Some(selected) {
        return false;
    }
    state
        .chats
        .iter()
        .find(|chat| chat.id == chat_id)
        .is_some_and(|chat| {
            chat.config
                .as_ref()
                .is_some_and(|config| needs_fresh_session(config.harness))
                && has_native_session(chat)
        })
}

/// Tooltip of Keep current session.
const KEEP_SESSION_HINT: &str = "Continue this conversation with the instructions its session \
     already has. The new instructions stay available to Apply later.";

/// Where a Chat stands in its Space's outline.
pub(crate) struct ChatInstructionContext {
    pub item_id: Option<String>,
    pub state: Option<ChatInstructionState>,
    pub snapshot: Arc<WorkOutlineSnapshot>,
}

pub(crate) fn chat_instruction_context(
    state: &AppState,
    chat_id: &str,
) -> Option<ChatInstructionContext> {
    let space_id = state
        .chats
        .iter()
        .find(|chat| chat.id == chat_id)?
        .space_id
        .clone()?;
    let load = state.work_outline(&space_id);
    let snapshot = load.snapshot()?.clone();
    let item_id = instructions::assigned_chats(&snapshot).remove(chat_id);
    let chat_state = snapshot.chat_instruction_state(chat_id).cloned();
    Some(ChatInstructionContext {
        item_id,
        state: chat_state,
        snapshot,
    })
}

/// Why the Chat's host cannot take Work Outline instructions (with an
/// upgrade hint), or `None` when its device row advertises
/// `work-outline-instruction-envelope-v1` and the per-harness token for the
/// Chat's harness. Apply is disabled while this is `Some`.
pub(crate) fn instruction_host_blocker(state: &AppState, chat_id: &str) -> Option<String> {
    use crate::state::InstructionHostSupport;
    let support = state.chat_host_instruction_support(chat_id);
    if support == InstructionHostSupport::Supported {
        return None;
    }
    let device_id = state
        .chats
        .iter()
        .find(|chat| chat.id == chat_id)
        .map(|chat| chat.device_id.as_str())
        .unwrap_or_default();
    let host = if state.is_local_device(device_id) {
        "this device".to_owned()
    } else {
        state
            .device_name(device_id)
            .map(str::to_owned)
            .unwrap_or_else(|| "its host".to_owned())
    };
    Some(match support {
        InstructionHostSupport::Supported => unreachable!(),
        InstructionHostSupport::UnknownDevice => format!(
            "This Chat\u{2019}s host hasn\u{2019}t synced to this workspace yet, so it can\u{2019}t \
             take Work Outline instructions. Open Mast on {host} to apply them."
        ),
        InstructionHostSupport::MastUpgradeRequired => format!(
            "This Chat\u{2019}s host can\u{2019}t deliver Work Outline instructions. \
             Update Mast on {host} to apply them."
        ),
        InstructionHostSupport::HarnessUnsupported(harness) => {
            let cli = crate::settings::harnesses::cli_name(harness);
            format!(
                "This Chat\u{2019}s agent ({cli}) on {host} can\u{2019}t receive Work Outline \
                 instructions. Update {cli} on {host} to apply them."
            )
        }
    })
}

fn chat_is_working(state: &AppState, chat_id: &str) -> bool {
    state
        .chats
        .iter()
        .find(|chat| chat.id == chat_id)
        .is_some_and(|chat| state.display_status_for(chat, Utc::now()) == ChatIndicator::Working)
}

fn chat_title(state: &AppState, chat_id: &str) -> String {
    state
        .chats
        .iter()
        .find(|chat| chat.id == chat_id)
        .and_then(|chat| chat.title.clone())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| "Chat".into())
}

/// Per-Chat Apply progress. The two operation ids are reused while the
/// inputs are the same (the item and its current effective hash; the Chat
/// and the minted revision), so a retry after a lost reply replays instead
/// of writing twice.
#[derive(Debug, Default)]
pub(crate) struct ApplyState {
    revision_op: OperationSlot,
    apply_op: OperationSlot,
    pub(crate) in_flight: bool,
    pub(crate) error: Option<String>,
    pub(crate) done: Option<&'static str>,
    /// The fresh-session consequence awaiting the user's Apply / Cancel
    /// (nothing has been written yet).
    pub(crate) confirm: Option<String>,
    keep_op: OperationSlot,
    pub(crate) keeping: bool,
}

pub(super) enum ComparePhase {
    Loading,
    Ready(InstructionRevisionComparison),
    Failed(String),
}

pub(super) struct CompareDialog {
    pub(super) chat_id: String,
    title: String,
    phase: ComparePhase,
    delivery: Option<String>,
    working: bool,
}

fn compare_failure_text(error: CompareUnavailable) -> String {
    match error {
        CompareUnavailable::NothingToCompare => "This Chat has no applied revision yet".into(),
        CompareUnavailable::MissingRevision(id) => {
            format!(
                "Revision {} is not available",
                instructions::revision_label(&id)
            )
        }
        CompareUnavailable::Failed(message) => message,
    }
}

/// Which scope a Chat-derived instruction goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FromChatTarget {
    Item,
    Parent,
    SpaceRoot,
}

pub(super) struct FromChatDialog {
    chat_id: String,
    message_id: Option<String>,
    excerpt: String,
    space_id: String,
    item: Option<(String, String)>,
    parent: Option<(String, String)>,
    pub(super) target: FromChatTarget,
    snapshot: Arc<WorkOutlineSnapshot>,
    pub(super) input: Entity<ComposerInput>,
    /// `/` skill autocomplete inserting plain "`name` skill" text, listed
    /// for the chosen target's resolved execution root.
    pub(super) skills: Entity<SkillCompletion>,
    /// The Space's folder and owning device (the root's fallback).
    space_path: String,
    space_device: String,
    op: OperationSlot,
    saving: bool,
    error: Option<String>,
    _input_events: Subscription,
}

impl FromChatDialog {
    pub(super) fn input_focus(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }

    fn skill_source(&self) -> SkillSource {
        SkillSource::for_outline_scope(
            &self.snapshot,
            &self.space_id,
            self.target_item().map(|(id, _)| id.as_str()),
            &self.space_path,
            &self.space_device,
        )
    }

    fn target_item(&self) -> Option<&(String, String)> {
        match self.target {
            FromChatTarget::Item => self.item.as_ref(),
            FromChatTarget::Parent => self.parent.as_ref(),
            FromChatTarget::SpaceRoot => None,
        }
    }
}

/// One transcript row as the excerpt picker sees it.
pub(crate) struct ExcerptRow<'a> {
    pub row_id: &'a str,
    pub message_id: &'a str,
    pub user: bool,
    pub copy_text: Option<&'a str>,
}

/// The excerpt for Create Instruction from Chat: the transcript selection
/// when it is anchored in one of `rows` (message id = the anchor row's
/// message), else the last assistant message's text. Pure.
pub(crate) fn excerpt_from_transcript(
    rows: &[ExcerptRow<'_>],
    selection: Option<(&str, &str)>,
) -> Option<(Option<String>, String)> {
    if let Some((anchor, text)) = selection.filter(|(_, text)| !text.trim().is_empty()) {
        let owner = rows
            .iter()
            .filter(|row| {
                anchor
                    .strip_prefix(row.row_id)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '-']))
            })
            .max_by_key(|row| row.row_id.len());
        if let Some(row) = owner {
            return Some((Some(row.message_id.to_owned()), text.trim().to_owned()));
        }
    }
    rows.iter().rev().filter(|row| !row.user).find_map(|row| {
        let text = row.copy_text?.trim();
        (!text.is_empty()).then(|| (Some(row.message_id.to_owned()), text.to_owned()))
    })
}

impl Shell {
    // ----------------------------------------------------------------------
    // Drift line in the chat header
    // ----------------------------------------------------------------------

    /// The compact instruction status under the chat header metadata, when
    /// the Chat drifted or its delivery differs from its selection.
    pub(super) fn render_chat_instruction_status(
        &mut self,
        chat_id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let context = chat_instruction_context(self.state.read(cx), chat_id)?;
        let chat_state = context.state?;
        let delivery = instructions::delivery_line(&chat_state);
        if !chat_state.drift && delivery.is_none() {
            return None;
        }
        let blocker = instruction_host_blocker(self.state.read(cx), chat_id);
        let keep = can_keep_current_session(self.state.read(cx), chat_id, &chat_state);
        let apply = self.instruction_apply.get(chat_id);
        let in_flight = apply.is_some_and(|apply| apply.in_flight);
        let keeping = apply.is_some_and(|apply| apply.keeping);
        let confirm = apply.and_then(|apply| apply.confirm.clone());
        let error = apply.and_then(|apply| apply.error.clone());
        let done = apply.and_then(|apply| apply.done);
        let link = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .flex_none()
                .px(px(4.0))
                .rounded(px(3.0))
                .cursor_pointer()
                .text_color(theme.accent)
                .hover(|el| el.bg(theme.element_hover))
                .child(label)
        };
        let compare_chat = chat_id.to_owned();
        let apply_chat = chat_id.to_owned();
        let keep_chat = chat_id.to_owned();
        let confirm_chat = chat_id.to_owned();
        let cancel_chat = chat_id.to_owned();
        Some(
            div()
                .debug_selector(|| "chat-header-instruction-status".into())
                .mt(px(2.0))
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.0))
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_muted)
                .when(chat_state.drift, |el| {
                    el.child(
                        icon(icons::DANGER_TRIANGLE)
                            .size(px(10.0))
                            .text_color(theme.warning),
                    )
                    .child(
                        div()
                            .debug_selector(|| "chat-header-instruction-drift".into())
                            .child(
                                "Instructions changed since this Chat\u{2019}s applied revision.",
                            ),
                    )
                    .child(
                        link("chat-header-instructions-compare", "Compare")
                            .hint(
                                "Compare the applied and current instructions",
                                HintKey::None,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_instruction_compare(&compare_chat, cx)
                            })),
                    )
                    .child(if blocker.is_some() {
                        // The host can't deliver instructions: visibly inert.
                        div()
                            .id("chat-header-instructions-apply-disabled")
                            .debug_selector(|| "chat-header-instructions-apply-disabled".into())
                            .flex_none()
                            .px(px(4.0))
                            .text_color(theme.text_faint)
                            .child("Apply")
                    } else {
                        link(
                            "chat-header-instructions-apply",
                            if in_flight {
                                "Applying\u{2026}"
                            } else {
                                "Apply"
                            },
                        )
                        .hint(APPLY_HINT, HintKey::None)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.apply_chat_instructions(&apply_chat, cx)
                        }))
                    })
                })
                .when_some(blocker, |el, blocker| {
                    el.child(
                        div()
                            .debug_selector(|| "chat-header-instruction-host-unsupported".into())
                            .text_color(theme.warning)
                            .child(SharedString::from(blocker)),
                    )
                })
                .when_some(delivery, |el, line| {
                    el.child(
                        div()
                            .debug_selector(|| "chat-header-instruction-delivery".into())
                            .text_color(theme.text_faint)
                            .child(SharedString::from(line)),
                    )
                })
                .when(keep, |el| {
                    el.child(
                        link(
                            "chat-header-instructions-keep",
                            if keeping {
                                "Keeping\u{2026}"
                            } else {
                                "Keep current session"
                            },
                        )
                        .hint(KEEP_SESSION_HINT, HintKey::None)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.keep_chat_instruction_session(&keep_chat, cx)
                        })),
                    )
                })
                .when_some(confirm, |el, warning| {
                    el.child(
                        div()
                            .debug_selector(|| "chat-header-instruction-apply-warning".into())
                            .text_color(theme.warning)
                            .child(SharedString::from(warning)),
                    )
                    .child(
                        link("chat-header-instructions-apply-confirm", "Apply").on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.confirm_apply_chat_instructions(&confirm_chat, cx)
                            }),
                        ),
                    )
                    .child(
                        link("chat-header-instructions-apply-cancel", "Cancel").on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.cancel_apply_chat_instructions(&cancel_chat, cx)
                            }),
                        ),
                    )
                })
                .when_some(error, |el, error| {
                    el.child(
                        div()
                            .text_color(theme.warning)
                            .child(SharedString::from(error)),
                    )
                })
                .when_some(done, |el, done| el.child(div().child(done)))
                .into_any_element(),
        )
    }

    // ----------------------------------------------------------------------
    // Apply
    // ----------------------------------------------------------------------

    /// Apply, unless the Chat's harness would need a fresh session for it:
    /// then the consequence is shown first (Apply / Cancel) and nothing is
    /// written until [`Self::confirm_apply_chat_instructions`].
    pub(crate) fn apply_chat_instructions(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let warning = fresh_session_apply_warning(self.state.read(cx), chat_id);
        if let Some(warning) = warning {
            let entry = self
                .instruction_apply
                .entry(chat_id.to_owned())
                .or_default();
            if entry.in_flight {
                return;
            }
            entry.confirm = Some(warning);
            entry.error = None;
            entry.done = None;
            cx.notify();
            return;
        }
        self.run_apply_chat_instructions(chat_id, cx);
    }

    /// Apply anyway, after the fresh-session warning.
    pub(crate) fn confirm_apply_chat_instructions(
        &mut self,
        chat_id: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(entry) = self.instruction_apply.get_mut(chat_id) {
            entry.confirm = None;
        }
        self.run_apply_chat_instructions(chat_id, cx);
    }

    /// Cancel the fresh-session warning: nothing was written.
    pub(crate) fn cancel_apply_chat_instructions(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        if let Some(entry) = self.instruction_apply.get_mut(chat_id) {
            entry.confirm = None;
        }
        cx.notify();
    }

    /// Keep current session: `keepInstructionSession` reverts the Chat's
    /// selection to what its Execution Session received (or to none), so
    /// the next message continues the conversation. The drift notice stays.
    pub(crate) fn keep_chat_instruction_session(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let selected = chat_instruction_context(self.state.read(cx), chat_id)
            .and_then(|context| context.state?.selected_revision_id)
            .unwrap_or_default();
        let actions = self.state.read(cx).work_outline_actions();
        let entry = self
            .instruction_apply
            .entry(chat_id.to_owned())
            .or_default();
        if entry.in_flight || entry.keeping {
            return;
        }
        let operation_id = entry.keep_op.id_for(&format!("{chat_id}\n{selected}"));
        entry.keeping = true;
        entry.error = None;
        entry.done = None;
        entry.confirm = None;
        let task = actions.keep_instruction_session(mast_proto::KeepInstructionSessionParams {
            chat_id: chat_id.to_owned(),
            operation_id,
        });
        cx.notify();
        let chat_id = chat_id.to_owned();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let entry = this.instruction_apply.entry(chat_id.clone()).or_default();
                entry.keeping = false;
                match result {
                    Ok(_) => {
                        entry.keep_op = OperationSlot::default();
                        entry.done = Some("Kept \u{b7} this session continues as it is");
                    }
                    Err(error) => entry.error = Some(instruction_error_text(&error)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Apply the Chat's item's current effective instructions to the Chat:
    /// `createInstructionRevision`, then `applyInstructionRevision` with
    /// reason `explicit`. A running Turn is never touched; the revision takes
    /// effect at the next Turn (contract 5.7).
    fn run_apply_chat_instructions(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let (context, working) = {
            let state = self.state.read(cx);
            (
                chat_instruction_context(state, chat_id),
                chat_is_working(state, chat_id),
            )
        };
        let Some((snapshot, item_id)) =
            context.and_then(|context| Some((context.snapshot, context.item_id?)))
        else {
            let entry = self
                .instruction_apply
                .entry(chat_id.to_owned())
                .or_default();
            entry.error = Some("This Chat is not associated with a work item".into());
            cx.notify();
            return;
        };
        let blocker = instruction_host_blocker(self.state.read(cx), chat_id);
        let effective = instructions::effective_hash(&snapshot, Some(&item_id));
        let entry = self
            .instruction_apply
            .entry(chat_id.to_owned())
            .or_default();
        if entry.in_flight {
            return;
        }
        if let Some(blocker) = blocker {
            entry.error = Some(blocker);
            cx.notify();
            return;
        }
        let revision_op = entry.revision_op.id_for(&format!("{item_id}\n{effective}"));
        entry.in_flight = true;
        entry.error = None;
        entry.done = None;
        let create = self
            .state
            .read(cx)
            .work_outline_actions()
            .create_instruction_revision(CreateInstructionRevisionParams {
                work_item_id: item_id,
                operation_id: revision_op,
            });
        cx.notify();
        let chat_id = chat_id.to_owned();
        cx.spawn(async move |this, cx| {
            let revision_id = match create.await.and_then(|reply| reply.into_revision_id()) {
                Ok(id) => id,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.finish_apply(&chat_id, Err(error), working, cx)
                    })
                    .ok();
                    return;
                }
            };
            let Ok(apply) = this.update(cx, |this, cx| {
                let entry = this.instruction_apply.entry(chat_id.clone()).or_default();
                let operation_id = entry.apply_op.id_for(&format!("{chat_id}\n{revision_id}"));
                this.state
                    .read(cx)
                    .work_outline_actions()
                    .apply_instruction_revision(ApplyInstructionRevisionParams {
                        chat_id: chat_id.clone(),
                        revision_id: revision_id.clone(),
                        reason: InstructionApplicationReason::Explicit,
                        turn_boundary: None,
                        operation_id,
                    })
            }) else {
                return;
            };
            let result = apply.await.map(|_| ());
            this.update(cx, |this, cx| {
                this.finish_apply(&chat_id, result, working, cx)
            })
            .ok();
        })
        .detach();
    }

    fn finish_apply(
        &mut self,
        chat_id: &str,
        result: Result<(), mast_proto::WorkOutlineError>,
        working: bool,
        cx: &mut Context<Self>,
    ) {
        let entry = self
            .instruction_apply
            .entry(chat_id.to_owned())
            .or_default();
        entry.in_flight = false;
        match result {
            Ok(()) => {
                entry.revision_op = OperationSlot::default();
                entry.apply_op = OperationSlot::default();
                entry.done = Some(if working {
                    "Applied \u{b7} takes effect at the next Turn"
                } else {
                    "Applied"
                });
            }
            Err(error) => entry.error = Some(instruction_error_text(&error)),
        }
        cx.notify();
    }

    // ----------------------------------------------------------------------
    // Compare
    // ----------------------------------------------------------------------

    /// Open the revision comparison for an assigned Chat: its applied
    /// revision against its item's current effective instructions.
    pub(crate) fn open_instruction_compare(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        let (context, working, title) = {
            let state = self.state.read(cx);
            (
                chat_instruction_context(state, chat_id),
                chat_is_working(state, chat_id),
                chat_title(state, chat_id),
            )
        };
        let Some(context) = context else {
            return;
        };
        let delivery = context.state.as_ref().and_then(instructions::delivery_line);
        let phase = match &context.item_id {
            None => ComparePhase::Failed("This Chat is not associated with a work item".into()),
            Some(item_id) => match instructions::chat_comparison(&context.snapshot, chat_id) {
                Ok(comparison) => ComparePhase::Ready(comparison),
                Err(CompareUnavailable::MissingRevision(revision_id)) => {
                    self.fetch_revision_for_compare(
                        chat_id.to_owned(),
                        item_id.clone(),
                        revision_id,
                        context.snapshot.clone(),
                        cx,
                    );
                    ComparePhase::Loading
                }
                Err(error) => ComparePhase::Failed(compare_failure_text(error)),
            },
        };
        self.begin_work_outline_modal(
            WorkOutlineModal::Compare(CompareDialog {
                chat_id: chat_id.to_owned(),
                title,
                phase,
                delivery,
                working,
            }),
            cx,
        );
    }

    /// The applied revision is not in the snapshot (only selected and
    /// delivered revisions are): fetch it with `getInstructionRevision`.
    fn fetch_revision_for_compare(
        &mut self,
        chat_id: String,
        item_id: String,
        revision_id: String,
        snapshot: Arc<WorkOutlineSnapshot>,
        cx: &mut Context<Self>,
    ) {
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .query(WorkOutlineQueryRequest::GetInstructionRevision { revision_id });
        cx.spawn(async move |this, cx| {
            let result = task.await.and_then(expect_revision);
            this.update(cx, |this, cx| {
                let Some(WorkOutlineModal::Compare(dialog)) = this.work_outline_modal.as_mut()
                else {
                    return;
                };
                if dialog.chat_id != chat_id {
                    return;
                }
                dialog.phase = match result {
                    Ok(revision) => {
                        match instructions::compare_with_current(&snapshot, &item_id, &revision) {
                            Ok(comparison) => ComparePhase::Ready(comparison),
                            Err(error) => ComparePhase::Failed(compare_failure_text(error)),
                        }
                    }
                    Err(error) => ComparePhase::Failed(instruction_error_text(&error)),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ----------------------------------------------------------------------
    // Create Instruction from Chat
    // ----------------------------------------------------------------------

    /// Palette "Create Instruction from Chat": the active Chat's transcript
    /// selection, or its last assistant message.
    pub(crate) fn open_instruction_from_chat(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let chat_id = if matches!(self.route, Route::Chat) && !self.active_chat.is_empty() {
            self.active_chat.clone()
        } else {
            self.state
                .read(cx)
                .selected_chat
                .clone()
                .ok_or("Open a Chat first")?
        };
        let picked = {
            let transcript = self.transcript.read(cx);
            let rows: Vec<ExcerptRow<'_>> = if transcript.chat_id() == Some(chat_id.as_str()) {
                transcript
                    .rows()
                    .iter()
                    .map(|row| ExcerptRow {
                        row_id: row.id.as_ref(),
                        message_id: row.entry_id.as_ref(),
                        user: matches!(row.kind, transcript::RowKind::User { .. }),
                        copy_text: row.copy_text.as_deref(),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let anchor = crate::markdown::selection::anchor_key();
            let selected = crate::markdown::selection::selected_text();
            let selection = anchor.as_deref().zip(selected.as_deref());
            excerpt_from_transcript(&rows, selection)
        };
        let (message_id, excerpt) = picked.ok_or("Select text in the Chat first")?;
        self.open_instruction_from_chat_with(&chat_id, message_id, excerpt, window, cx)
    }

    /// Open the Create Instruction from Chat dialog with a known excerpt.
    pub(crate) fn open_instruction_from_chat_with(
        &mut self,
        chat_id: &str,
        message_id: Option<String>,
        excerpt: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let (space_id, snapshot) = {
            let state = self.state.read(cx);
            let chat = state
                .chats
                .iter()
                .find(|chat| chat.id == chat_id)
                .ok_or("That Chat is not available")?;
            let space_id = chat
                .space_id
                .clone()
                .ok_or("Chats without a project cannot have instructions")?;
            let load = state.work_outline(&space_id);
            let snapshot = load
                .snapshot()
                .ok_or("This project's outline is not loaded")?
                .clone();
            (space_id, snapshot)
        };
        let live = |id: &str| {
            snapshot
                .item(id)
                .filter(|item| item.archived_at.is_none())
                .map(|item| (item.id.clone(), item.title.clone()))
        };
        let item = instructions::assigned_chats(&snapshot)
            .remove(chat_id)
            .and_then(|id| live(&id));
        let parent = item
            .as_ref()
            .and_then(|(id, _)| snapshot.item(id)?.parent_work_item_id.clone())
            .and_then(|id| live(&id));
        let excerpt: String = excerpt
            .chars()
            .take(mast_proto::limits::MAX_INSTRUCTION_EXCERPT_CHARS)
            .collect();
        let draft: String = excerpt
            .chars()
            .take(mast_proto::limits::MAX_INSTRUCTION_TEXT_CHARS)
            .collect();
        let input = cx.new(|cx| {
            let mut input = ComposerInput::new("Instruction", cx)
                .with_text_metrics(12.5, 18.0)
                .with_auto_grow();
            input.set_text(draft, cx);
            input
        });
        let skills = cx.new(|cx| {
            SkillCompletion::new(
                self.state.clone(),
                input.clone(),
                SkillInsertion::InstructionText,
                cx,
            )
            .over_modal()
            .surface("instruction-from-chat")
        });
        let (space_path, space_device) = self
            .state
            .read(cx)
            .space_row(&space_id)
            .map(|space| (space.path.clone(), space.device_id.clone()))
            .unwrap_or_default();
        let input_events = cx.subscribe_in(&input, window, |this, _, event, window, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.save_instruction_from_chat(window, cx);
            }
        });
        let dialog = FromChatDialog {
            chat_id: chat_id.to_owned(),
            message_id,
            excerpt,
            space_id,
            target: if item.is_some() {
                FromChatTarget::Item
            } else {
                FromChatTarget::SpaceRoot
            },
            item,
            parent,
            snapshot,
            input,
            skills,
            space_path,
            space_device,
            op: OperationSlot::default(),
            saving: false,
            error: None,
            _input_events: input_events,
        };
        let source = dialog.skill_source();
        dialog
            .skills
            .update(cx, |skills, cx| skills.set_source(source, cx));
        self.begin_work_outline_modal(WorkOutlineModal::FromChat(dialog), cx);
        Ok(())
    }

    pub(super) fn set_instruction_from_chat_target(
        &mut self,
        target: FromChatTarget,
        cx: &mut Context<Self>,
    ) {
        if let Some(WorkOutlineModal::FromChat(dialog)) = self.work_outline_modal.as_mut() {
            dialog.target = target;
            let source = dialog.skill_source();
            let skills = dialog.skills.clone();
            skills.update(cx, |skills, cx| skills.set_source(source, cx));
            cx.notify();
        }
    }

    /// Save: create the instruction with Chat provenance (`chatId`,
    /// `messageId`, the copied excerpt and its sha256).
    pub(super) fn save_instruction_from_chat(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let actions = self.state.read(cx).work_outline_actions();
        let Some(WorkOutlineModal::FromChat(dialog)) = self.work_outline_modal.as_mut() else {
            return;
        };
        if dialog.saving {
            return;
        }
        let text = dialog.input.read(cx).text().trim().to_owned();
        if text.is_empty() {
            dialog.error = Some("Enter the instruction text".into());
            cx.notify();
            return;
        }
        let max = mast_proto::limits::MAX_INSTRUCTION_TEXT_CHARS;
        if text.chars().count() > max {
            dialog.error = Some(format!("Instructions are limited to {max} characters"));
            cx.notify();
            return;
        }
        let scope = match dialog.target_item() {
            Some((id, _)) => InstructionScope::WorkItem {
                work_item_id: id.clone(),
            },
            None => InstructionScope::SpaceRoot,
        };
        let operation_id = dialog.op.id_for(&format!("{scope:?}\n{text}"));
        let task = actions.create_instruction(CreateWorkItemInstructionParams {
            space_id: dialog.space_id.clone(),
            scope,
            text,
            placement: None,
            source: Some(InstructionSource {
                chat_id: Some(dialog.chat_id.clone()),
                message_id: dialog.message_id.clone(),
                content_hash: Some(instructions::excerpt_content_hash(&dialog.excerpt)),
                excerpt: Some(dialog.excerpt.clone()),
            }),
            operation_id,
        });
        dialog.saving = true;
        dialog.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                let Some(WorkOutlineModal::FromChat(dialog)) = this.work_outline_modal.as_mut()
                else {
                    return;
                };
                dialog.saving = false;
                match result {
                    Ok(_) => {
                        this.resolve_work_outline_modal(true, window, cx);
                    }
                    Err(error) => {
                        dialog.error = Some(instruction_error_text(&error));
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }
}

fn small_note(selector: &'static str, text: String, color: gpui::Hsla) -> gpui::Div {
    div()
        .debug_selector(move || selector.into())
        .text_size(crate::typography::ui_rems(11.5))
        .text_color(color)
        .child(SharedString::from(text))
}

/// The comparison modal card.
pub(super) fn compare_card(
    dialog: &CompareDialog,
    shell: &Shell,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let apply = shell.instruction_apply.get(&dialog.chat_id);
    let in_flight = apply.is_some_and(|apply| apply.in_flight);
    let keeping = apply.is_some_and(|apply| apply.keeping);
    let confirm = apply.and_then(|apply| apply.confirm.clone());
    let blocker = instruction_host_blocker(shell.state.read(cx), &dialog.chat_id);
    let keep = chat_instruction_context(shell.state.read(cx), &dialog.chat_id)
        .and_then(|context| context.state)
        .is_some_and(|chat_state| {
            can_keep_current_session(shell.state.read(cx), &dialog.chat_id, &chat_state)
        });
    // The Apply-time error repeats the blocker; show it once.
    let apply_error = apply
        .and_then(|apply| apply.error.clone())
        .filter(|error| blocker.as_ref() != Some(error));
    let body: AnyElement = match &dialog.phase {
        ComparePhase::Loading => small_note(
            "instruction-compare-loading",
            "Loading\u{2026}".into(),
            theme.text_muted,
        )
        .into_any_element(),
        ComparePhase::Failed(message) => {
            small_note("instruction-compare-error", message.clone(), theme.warning)
                .into_any_element()
        }
        ComparePhase::Ready(comparison) => revision_diff::render_comparison(comparison, theme),
    };
    let apply_chat = dialog.chat_id.clone();
    let keep_chat = dialog.chat_id.clone();
    let confirm_chat = dialog.chat_id.clone();
    let cancel_chat = dialog.chat_id.clone();
    popover::dialog_card(theme)
        .w(px(560.0))
        .debug_selector(|| "instruction-compare".into())
        .child(popover::dialog_title(theme, "Instructions changed"))
        .child(small_note(
            "instruction-compare-chat",
            format!("{} \u{b7} applied revision \u{2192} current", dialog.title),
            theme.text_muted,
        ))
        .when_some(dialog.delivery.clone(), |el, line| {
            el.child(small_note(
                "instruction-compare-delivery",
                line,
                theme.text_faint,
            ))
        })
        .child(
            div()
                .id("instruction-compare-body")
                .mt(px(10.0))
                .max_h(px(360.0))
                .overflow_y_scroll()
                .child(body),
        )
        .when(dialog.working, |el| {
            el.child(small_note(
                "instruction-compare-working",
                "This Chat is working; applying takes effect at the next Turn.".into(),
                theme.text_muted,
            ))
        })
        .when_some(blocker.clone(), |el, blocker| {
            el.child(small_note(
                "instruction-compare-host-unsupported",
                blocker,
                theme.warning,
            ))
        })
        .when_some(apply_error, |el, error| {
            el.child(small_note(
                "instruction-compare-apply-error",
                error,
                theme.warning,
            ))
        })
        .when_some(apply.and_then(|apply| apply.done), |el, done| {
            el.child(small_note(
                "instruction-compare-applied",
                done.into(),
                theme.text_muted,
            ))
        })
        .when_some(confirm.clone(), |el, warning| {
            el.child(small_note(
                "instruction-compare-apply-warning",
                warning,
                theme.warning,
            ))
        })
        .child(
            div()
                .mt(px(16.0))
                .flex()
                .justify_end()
                .gap(px(8.0))
                .child(
                    popover::btn_ghost(theme, "Close", "instruction-compare-close")
                        .id("instruction-compare-close")
                        .debug_selector(|| "instruction-compare-close".into())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.resolve_work_outline_modal(false, window, cx);
                        })),
                )
                .when(keep, |el| {
                    el.child(
                        popover::btn_ghost(
                            theme,
                            if keeping {
                                "Keeping\u{2026}"
                            } else {
                                "Keep current session"
                            },
                            "instruction-compare-keep",
                        )
                        .id("instruction-compare-keep")
                        .debug_selector(|| "instruction-compare-keep".into())
                        .hint(KEEP_SESSION_HINT, HintKey::None)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.keep_chat_instruction_session(&keep_chat, cx)
                        })),
                    )
                })
                .child(if confirm.is_some() {
                    // The fresh-session consequence is showing: confirm or
                    // cancel it (nothing was written yet).
                    div()
                        .flex()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(theme, "Cancel", "instruction-compare-apply-cancel")
                                .id("instruction-compare-apply-cancel")
                                .debug_selector(|| "instruction-compare-apply-cancel".into())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.cancel_apply_chat_instructions(&cancel_chat, cx)
                                })),
                        )
                        .child(
                            popover::btn_primary(theme, "Apply")
                                .id("instruction-compare-apply-confirm")
                                .debug_selector(|| "instruction-compare-apply-confirm".into())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.confirm_apply_chat_instructions(&confirm_chat, cx)
                                })),
                        )
                        .into_any_element()
                } else if blocker.is_some() {
                    // The host can't deliver instructions: visibly inert.
                    popover::btn_primary(theme, "Apply")
                        .id("instruction-compare-apply-disabled")
                        .debug_selector(|| "instruction-compare-apply-disabled".into())
                        .opacity(0.4)
                        .cursor_default()
                        .into_any_element()
                } else {
                    popover::btn_primary(
                        theme,
                        if in_flight {
                            "Applying\u{2026}"
                        } else {
                            "Apply"
                        },
                    )
                    .id("instruction-compare-apply")
                    .debug_selector(|| "instruction-compare-apply".into())
                    .hint(APPLY_HINT, HintKey::None)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.apply_chat_instructions(&apply_chat, cx)
                    }))
                    .into_any_element()
                }),
        )
        .into_any_element()
}

/// The Create Instruction from Chat card.
pub(super) fn from_chat_card(
    dialog: &FromChatDialog,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let option =
        |id: &'static str, label: String, target: FromChatTarget, cx: &mut Context<Shell>| {
            let selected = dialog.target == target;
            popover::menu_row(theme, selected, id)
                .id(id)
                .debug_selector(move || id.into())
                .role(gpui::Role::RadioButton)
                .aria_label(label.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_instruction_from_chat_target(target, cx)
                }))
                .child(
                    icon(icons::CHECK)
                        .size(px(14.0))
                        .flex_none()
                        .text_color(if selected {
                            theme.text
                        } else {
                            gpui::transparent_black()
                        }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(label)),
                )
        };
    let mut targets = div().mt(px(10.0)).flex().flex_col().gap(px(2.0));
    if let Some((_, title)) = &dialog.item {
        targets = targets.child(option(
            "instruction-from-chat-target-item",
            format!("This item \u{b7} {title}"),
            FromChatTarget::Item,
            cx,
        ));
    }
    if let Some((_, title)) = &dialog.parent {
        targets = targets.child(option(
            "instruction-from-chat-target-parent",
            format!("Parent workstream \u{b7} {title}"),
            FromChatTarget::Parent,
            cx,
        ));
    }
    targets = targets.child(option(
        "instruction-from-chat-target-root",
        "Space root".into(),
        FromChatTarget::SpaceRoot,
        cx,
    ));
    let target_id = dialog.target_item().map(|(id, _)| id.clone());
    let target_label = match &target_id {
        Some(id) => model::path_label(
            model::item_path(&dialog.snapshot, id)
                .iter()
                .map(|item| item.title.as_str()),
        ),
        None => "Space root".into(),
    };
    let descendants =
        instructions::affected_descendant_count(&dialog.snapshot, target_id.as_deref());
    let reach = match target_id {
        Some(_) => format!(
            "{target_label} \u{b7} also reaches {}",
            instructions::plural(descendants, "descendant item", "descendant items")
        ),
        None => format!(
            "{target_label} \u{b7} reaches {}",
            instructions::plural(descendants, "work item", "work items")
        ),
    };
    popover::dialog_card(theme)
        .w(px(480.0))
        .debug_selector(|| "instruction-from-chat".into())
        .child(popover::dialog_title(theme, "Create instruction from Chat"))
        .child(targets)
        .child(small_note("instruction-from-chat-reach", reach, theme.text_muted).mt(px(8.0)))
        .child(
            // Auto-grows with the text (the input owns the cap and scrolls
            // past it); no height cap here, which would clip.
            div()
                .relative()
                .debug_selector(|| "instruction-from-chat-text".into())
                .mt(px(8.0))
                .w_full()
                .px(px(8.0))
                .py(px(6.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.input_bg)
                .child(dialog.input.clone())
                .child(dialog.skills.clone()),
        )
        .when_some(dialog.error.clone(), |el, error| {
            el.child(small_note("instruction-from-chat-error", error, theme.warning).mt(px(6.0)))
        })
        .child(
            div()
                .mt(px(16.0))
                .flex()
                .justify_end()
                .gap(px(8.0))
                .child(
                    popover::btn_ghost(theme, "Cancel", "instruction-from-chat-cancel")
                        .id("instruction-from-chat-cancel")
                        .debug_selector(|| "instruction-from-chat-cancel".into())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.resolve_work_outline_modal(false, window, cx);
                        })),
                )
                .child(
                    popover::btn_primary(
                        theme,
                        if dialog.saving {
                            "Saving\u{2026}"
                        } else {
                            "Save"
                        },
                    )
                    .id("instruction-from-chat-save")
                    .debug_selector(|| "instruction-from-chat-save".into())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.save_instruction_from_chat(window, cx)
                    })),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_outline::WorkOutlineActions;
    use crate::work_outline::instructions::fixture;
    use crate::work_outline::store::fixture_ids::*;
    use std::sync::Mutex;

    type Calls = Arc<Mutex<Vec<serde_json::Value>>>;

    fn test_shell<'a>(
        cx: &'a mut gpui::TestAppContext,
        dir: &std::path::Path,
    ) -> (Entity<Shell>, &'a mut gpui::VisualTestContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let data_dir = dir.to_path_buf();
        cx.add_window_view(move |_, cx| {
            let state = cx.new(|_| AppState::new());
            state.update(cx, |state, _| {
                state.connection = crate::state::ConnectionStatus::Ready;
                state.workspace_scope = Some(WorkspaceScope::Local);
                state.auth = Some(mast_proto::AuthState::SignedOut);
            });
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: data_dir.clone(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: mast_proto::HarnessId::Mock,
                },
                cx,
            )
        })
    }

    /// Seed one Space with the instruction fixture and a scripted engine
    /// that records every call and answers through `reply`.
    fn seed(
        shell: &Entity<Shell>,
        cx: &mut gpui::VisualTestContext,
        snapshot: WorkOutlineSnapshot,
        reply: impl Fn(&serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError>
        + Send
        + Sync
        + 'static,
    ) -> Calls {
        let calls: Calls = Arc::default();
        let sink = calls.clone();
        let actions = WorkOutlineActions::scripted(Arc::new(move |_, params| {
            sink.lock().unwrap().push(params.clone());
            reply(&params)
        }));
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.spaces = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "s1", "deviceId": "d", "path": "/work/alpha",
                        "createdAt": "2026-01-01T00:00:00Z"
                    }))
                    .unwrap(),
                ];
                state.chats = [CHAT_API, CHAT_AUTH, CHAT_UI]
                    .iter()
                    .map(|id| {
                        serde_json::from_value(serde_json::json!({
                            "id": id, "deviceId": "d", "archived": false,
                            "createdAt": "2026-01-01T00:00:00Z", "spaceId": "s1",
                            "title": format!("Chat {id}"),
                        }))
                        .unwrap()
                    })
                    .collect();
                // The host advertises instruction delivery on its device row.
                state.devices = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "d", "name": "Studio", "platform": "macos",
                        "lastSeenAt": null,
                        "capabilities": [
                            mast_proto::capabilities::WORK_OUTLINE_INSTRUCTION_ENVELOPE_V1
                        ],
                    }))
                    .unwrap(),
                ];
                state.set_work_outline_visible(vec!["s1".into()]);
                assert!(state.apply_work_outline_frame("s1", snapshot));
                state.work_outline_actions_override = Some(actions);
            })
        });
        calls
    }

    fn draw(cx: &mut gpui::VisualTestContext) {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear());
    }

    fn ok_ids(params: &serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError> {
        Ok(match params["op"].as_str() {
            Some("createInstructionRevision") => serde_json::json!({"revisionId": "r-new"}),
            Some("applyInstructionRevision") => serde_json::json!({"applicationId": "a-new"}),
            Some("createWorkItemInstruction") => serde_json::json!({"instructionId": "i-new"}),
            _ => serde_json::json!({}),
        })
    }

    #[gpui::test]
    fn drift_status_shows_only_for_drifted_or_undelivered_chats(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let mut snapshot = fixture::snapshot("s1");
        // chat-auth: delivered == selected, no drift.
        snapshot.chat_instruction_states[1].delivered_revision_id = Some(fixture::R_AUTH.into());
        seed(&shell, cx, snapshot, ok_ids);
        shell.update(cx, |shell, cx| {
            let theme = Theme::of(cx).clone();
            assert!(
                shell
                    .render_chat_instruction_status(CHAT_API, &theme, cx)
                    .is_some()
            );
            assert!(
                shell
                    .render_chat_instruction_status(CHAT_AUTH, &theme, cx)
                    .is_none()
            );
            assert!(
                shell
                    .render_chat_instruction_status(CHAT_UI, &theme, cx)
                    .is_none()
            );
        });
    }

    /// The Overview List row's drift mark opens Compare without toggling
    /// the row.
    #[gpui::test]
    fn overview_drift_mark_opens_compare(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        seed(&shell, cx, fixture::snapshot("s1"), ok_ids);
        shell.update(cx, |shell, cx| shell.open_overview(cx));
        draw(cx);
        assert!(
            cx.debug_bounds("overview-row-instruction-drift-chat-auth")
                .is_none()
        );
        let mark = cx
            .debug_bounds("overview-row-instruction-drift-chat-api")
            .expect("drifted Chat is marked")
            .center();
        cx.simulate_click(mark, gpui::Modifiers::default());
        draw(cx);
        assert!(cx.debug_bounds("instruction-compare").is_some());
        let page = shell.read_with(cx, |shell, _| shell.overview_page.clone().unwrap());
        page.read_with(cx, |overview, _| {
            assert!(overview.expanded_is_empty(), "the row did not toggle")
        });
    }

    #[gpui::test]
    fn apply_creates_then_applies_with_stable_operation_ids(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let failures = Arc::new(Mutex::new(1));
        let calls = seed(&shell, cx, fixture::snapshot("s1"), move |params| {
            if params["op"] == "applyInstructionRevision" {
                let mut left = failures.lock().unwrap();
                if *left > 0 {
                    *left -= 1;
                    return Err(mast_rpc::RpcError::Failed(
                        "work-outline/invalid: try again".into(),
                    ));
                }
            }
            ok_ids(params)
        });
        shell.update(cx, |shell, cx| shell.apply_chat_instructions(CHAT_API, cx));
        cx.run_until_parked();
        shell.read_with(cx, |shell, _| {
            let apply = &shell.instruction_apply[CHAT_API];
            assert!(!apply.in_flight);
            assert_eq!(apply.error.as_deref(), Some("try again"));
        });
        // Retry: same operation ids for the same inputs.
        shell.update(cx, |shell, cx| shell.apply_chat_instructions(CHAT_API, cx));
        cx.run_until_parked();
        let calls = calls.lock().unwrap().clone();
        let ops: Vec<_> = calls.iter().map(|c| c["op"].as_str().unwrap()).collect();
        assert_eq!(
            ops,
            [
                "createInstructionRevision",
                "applyInstructionRevision",
                "createInstructionRevision",
                "applyInstructionRevision"
            ]
        );
        assert_eq!(calls[0]["workItemId"], API);
        assert_eq!(calls[0]["operationId"], calls[2]["operationId"]);
        assert_eq!(calls[1]["operationId"], calls[3]["operationId"]);
        assert_ne!(calls[0]["operationId"], calls[1]["operationId"]);
        assert_eq!(calls[1]["chatId"], CHAT_API);
        assert_eq!(calls[1]["revisionId"], "r-new");
        assert_eq!(calls[1]["reason"], "explicit");
        shell.read_with(cx, |shell, _| {
            let apply = &shell.instruction_apply[CHAT_API];
            assert_eq!(apply.error, None);
            assert_eq!(apply.done, Some("Applied"));
        });
    }

    #[gpui::test]
    fn apply_against_an_unsupported_engine_explains_calmly(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let calls = seed(&shell, cx, fixture::snapshot("s1"), |_| {
            Err(mast_rpc::RpcError::Failed(
                "work-outline/unsupported: instruction operations are not available yet".into(),
            ))
        });
        shell.update(cx, |shell, cx| shell.apply_chat_instructions(CHAT_API, cx));
        cx.run_until_parked();
        assert_eq!(calls.lock().unwrap().len(), 1, "apply is not attempted");
        shell.read_with(cx, |shell, _| {
            assert_eq!(
                shell.instruction_apply[CHAT_API].error.as_deref(),
                Some("Instructions aren\u{2019}t available on this host yet")
            );
        });
    }

    /// A host whose device row lacks `work-outline-instruction-envelope-v1`
    /// (an older engine, or a local engine before its restamp) explains why
    /// with an upgrade hint, and Apply is inert.
    #[gpui::test]
    fn hosts_without_instruction_delivery_disable_apply(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let calls = seed(&shell, cx, fixture::snapshot("s1"), ok_ids);
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.devices[0].capabilities =
                    vec![mast_proto::capabilities::WORK_OUTLINE_V1.into()];
            });
        });
        let blocker = shell.read_with(cx, |shell, cx| {
            instruction_host_blocker(shell.state.read(cx), CHAT_API)
        });
        assert_eq!(
            blocker.as_deref(),
            Some(
                "This Chat\u{2019}s host can\u{2019}t deliver Work Outline instructions. \
                 Update Mast on Studio to apply them."
            )
        );
        // The local engine's device row decides too (not its static
        // EngineInfo), with a local hint.
        shell.update(cx, |shell, cx| {
            shell
                .state
                .update(cx, |state, _| state.local_device_id = Some("d".into()));
        });
        let local = shell.read_with(cx, |shell, cx| {
            instruction_host_blocker(shell.state.read(cx), CHAT_API).unwrap()
        });
        assert!(local.ends_with("Update Mast on this device to apply them."));

        shell.update(cx, |shell, cx| shell.open_instruction_compare(CHAT_API, cx));
        draw(cx);
        assert!(
            cx.debug_bounds("instruction-compare-host-unsupported")
                .is_some()
        );
        assert!(cx.debug_bounds("instruction-compare-apply").is_none());
        let disabled = cx
            .debug_bounds("instruction-compare-apply-disabled")
            .expect("Apply is shown inert")
            .center();
        cx.simulate_click(disabled, gpui::Modifiers::default());
        draw(cx);
        // Apply is refused before any write, even when called directly.
        shell.update(cx, |shell, cx| shell.apply_chat_instructions(CHAT_API, cx));
        cx.run_until_parked();
        assert!(calls.lock().unwrap().is_empty(), "nothing was written");
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.instruction_apply[CHAT_API].error, Some(local.clone()));
        });

        // An unknown host is unsupported as well; a capable one is not.
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| state.devices.clear());
        });
        assert!(shell.read_with(cx, |shell, cx| {
            instruction_host_blocker(shell.state.read(cx), CHAT_API).is_some()
        }));
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.devices = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "d", "name": "Studio", "platform": "macos",
                        "lastSeenAt": null,
                        "capabilities": [
                            mast_proto::capabilities::WORK_OUTLINE_INSTRUCTION_ENVELOPE_V1
                        ],
                    }))
                    .unwrap(),
                ];
            });
        });
        assert!(shell.read_with(cx, |shell, cx| {
            instruction_host_blocker(shell.state.read(cx), CHAT_API).is_none()
        }));
    }

    /// The per-harness token decides too: a host whose Mast delivers
    /// instructions but not for this Chat's harness says so and keeps Apply
    /// inert until it advertises `work-outline-instruction-envelope-v1/<harness>`.
    #[gpui::test]
    fn harness_without_instruction_delivery_disables_apply(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        seed(&shell, cx, fixture::snapshot("s1"), ok_ids);
        let blocker = |shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext| {
            shell.read_with(cx, |shell, cx| {
                instruction_host_blocker(shell.state.read(cx), CHAT_API)
            })
        };
        // No harness known (no config): the base token is enough.
        assert_eq!(blocker(&shell, cx), None);
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.chats[0].config = Some(mast_proto::ChatConfig {
                    harness: mast_proto::HarnessId::Codex,
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    sandbox: mast_proto::SandboxLevel::WorkspaceWrite,
                    auto_approve: false,
                });
            });
        });
        assert_eq!(
            blocker(&shell, cx).as_deref(),
            Some(
                "This Chat\u{2019}s agent (codex) on Studio can\u{2019}t receive Work Outline \
                 instructions. Update codex on Studio to apply them."
            )
        );
        shell.update(cx, |shell, cx| shell.open_instruction_compare(CHAT_API, cx));
        draw(cx);
        assert!(
            cx.debug_bounds("instruction-compare-apply-disabled")
                .is_some()
        );
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.devices[0].capabilities.push(
                    mast_proto::capabilities::work_outline_instruction_envelope_for(
                        mast_proto::HarnessId::Codex,
                    ),
                );
            });
        });
        assert_eq!(blocker(&shell, cx), None);
        draw(cx);
        assert!(cx.debug_bounds("instruction-compare-apply").is_some());
    }

    /// Make `CHAT_API` a Codex Chat with an existing native session on a
    /// host that delivers instructions to Codex.
    fn codex_with_session(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext, session: bool) {
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.chats[0].config = Some(mast_proto::ChatConfig {
                    harness: mast_proto::HarnessId::Codex,
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    sandbox: mast_proto::SandboxLevel::WorkspaceWrite,
                    auto_approve: false,
                });
                state.chats[0].harness_session_id = session.then(|| "thread-1".into());
                state.devices[0].capabilities.push(
                    mast_proto::capabilities::work_outline_instruction_envelope_for(
                        mast_proto::HarnessId::Codex,
                    ),
                );
            });
        });
    }

    fn click(cx: &mut gpui::VisualTestContext, selector: &'static str) {
        let at = cx.debug_bounds(selector).expect(selector).center();
        cx.simulate_click(at, gpui::Modifiers::default());
        draw(cx);
    }

    #[gpui::test]
    fn apply_on_a_codex_chat_with_a_session_warns_before_writing(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let calls = seed(&shell, cx, fixture::snapshot("s1"), ok_ids);
        codex_with_session(&shell, cx, true);
        shell.update(cx, |shell, cx| shell.open_instruction_compare(CHAT_API, cx));
        draw(cx);
        click(cx, "instruction-compare-apply");
        assert!(calls.lock().unwrap().is_empty(), "nothing written yet");
        let warning = shell.read_with(cx, |shell, _| {
            shell.instruction_apply[CHAT_API].confirm.clone().unwrap()
        });
        assert_eq!(
            warning,
            "Applying starts a fresh Codex session on your next message (this conversation\u{2019}s \
             context won\u{2019}t carry over). Apply anyway?"
        );
        assert!(
            cx.debug_bounds("instruction-compare-apply-warning")
                .is_some()
        );
        assert!(cx.debug_bounds("instruction-compare-apply").is_none());

        // Cancel writes nothing.
        click(cx, "instruction-compare-apply-cancel");
        assert!(calls.lock().unwrap().is_empty());
        assert!(
            cx.debug_bounds("instruction-compare-apply-warning")
                .is_none()
        );

        // Apply anyway writes the revision and the application.
        click(cx, "instruction-compare-apply");
        click(cx, "instruction-compare-apply-confirm");
        let ops: Vec<String> = calls
            .lock()
            .unwrap()
            .iter()
            .map(|c| c["op"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            ops,
            ["createInstructionRevision", "applyInstructionRevision"]
        );
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.instruction_apply[CHAT_API].done, Some("Applied"));
        });

        // Without a native session there is nothing to lose: no warning.
        codex_with_session(&shell, cx, false);
        shell.update(cx, |shell, cx| shell.apply_chat_instructions(CHAT_API, cx));
        cx.run_until_parked();
        assert_eq!(calls.lock().unwrap().len(), 4);
        // Claude keeps the conversation: no warning either.
        assert!(!needs_fresh_session(mast_proto::HarnessId::ClaudeCode));
    }

    #[gpui::test]
    fn keep_current_session_recovers_a_stuck_codex_chat(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let mut snapshot = fixture::snapshot("s1");
        // Selected r-api-old, but the session never received instructions.
        snapshot.chat_instruction_states[0].delivered_revision_id = None;
        snapshot.chat_instruction_states[0].delivered_revision_hash = None;
        let calls = seed(&shell, cx, snapshot, ok_ids);
        // Not on a Chat without a native session.
        codex_with_session(&shell, cx, false);
        shell.update(cx, |shell, cx| shell.open_instruction_compare(CHAT_API, cx));
        draw(cx);
        assert!(cx.debug_bounds("instruction-compare-keep").is_none());

        codex_with_session(&shell, cx, true);
        draw(cx);
        click(cx, "instruction-compare-keep");
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["op"], "keepInstructionSession");
        assert_eq!(calls[0]["chatId"], CHAT_API);
        assert!(
            calls[0]["operationId"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        );
        shell.read_with(cx, |shell, _| {
            let apply = &shell.instruction_apply[CHAT_API];
            assert!(!apply.keeping);
            assert_eq!(
                apply.done,
                Some("Kept \u{b7} this session continues as it is")
            );
        });
        // The header keeps the drift notice (Apply stays available) and
        // offers Keep current session too.
        shell.update(cx, |shell, cx| {
            let theme = Theme::of(cx).clone();
            assert!(
                shell
                    .render_chat_instruction_status(CHAT_API, &theme, cx)
                    .is_some()
            );
            let state = shell.state.read(cx);
            let chat_state = chat_instruction_context(state, CHAT_API)
                .unwrap()
                .state
                .unwrap();
            assert!(chat_state.drift);
            assert!(can_keep_current_session(state, CHAT_API, &chat_state));
            // Delivered == selected: nothing to keep.
            let mut delivered = chat_state.clone();
            delivered.delivered_revision_id = delivered.selected_revision_id.clone();
            assert!(!can_keep_current_session(state, CHAT_API, &delivered));
        });
    }

    #[gpui::test]
    fn compare_renders_the_generic_diff_and_fetches_a_missing_revision(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        seed(&shell, cx, fixture::snapshot("s1"), ok_ids);
        shell.update(cx, |shell, cx| shell.open_instruction_compare(CHAT_API, cx));
        draw(cx);
        assert!(cx.debug_bounds("instruction-compare").is_some());
        assert!(cx.debug_bounds("instruction-comparison-changed").is_some());
        assert!(cx.debug_bounds("instruction-comparison-diff").is_some());
        shell.update_in(cx, |shell, window, cx| {
            shell.cancel_work_outline_modal(window, cx);
        });
        draw(cx);
        assert!(cx.debug_bounds("instruction-compare").is_none());

        // The applied revision is not in the snapshot: fetched by query.
        let full = fixture::snapshot("s1");
        let old = full.revision(fixture::R_API_OLD).unwrap().clone();
        let mut missing = full.clone();
        missing.revisions.clear();
        let reply =
            serde_json::to_value(mast_proto::WorkOutlineQueryResponse::GetInstructionRevision(old))
                .unwrap();
        let calls = seed(&shell, cx, missing, move |params| {
            if params.get("query").is_some() {
                Ok(reply.clone())
            } else {
                ok_ids(params)
            }
        });
        shell.update(cx, |shell, cx| shell.open_instruction_compare(CHAT_API, cx));
        draw(cx);
        assert_eq!(calls.lock().unwrap()[0]["query"], "getInstructionRevision");
        assert!(cx.debug_bounds("instruction-comparison-diff").is_some());
    }

    #[gpui::test]
    fn create_from_chat_saves_with_provenance_to_the_chosen_target(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let calls = seed(&shell, cx, fixture::snapshot("s1"), ok_ids);
        let excerpt = "We must never log bearer tokens.";
        shell.update_in(cx, |shell, window, cx| {
            shell
                .open_instruction_from_chat_with(
                    CHAT_AUTH,
                    Some("m-7".into()),
                    excerpt.into(),
                    window,
                    cx,
                )
                .unwrap()
        });
        draw(cx);
        assert!(cx.debug_bounds("instruction-from-chat").is_some());
        assert!(
            cx.debug_bounds("instruction-from-chat-target-item")
                .is_some()
        );
        assert!(
            cx.debug_bounds("instruction-from-chat-target-parent")
                .is_some()
        );
        assert!(
            cx.debug_bounds("instruction-from-chat-target-root")
                .is_some()
        );
        assert!(cx.debug_bounds("instruction-from-chat-reach").is_some());
        let parent = cx
            .debug_bounds("instruction-from-chat-target-parent")
            .unwrap()
            .center();
        cx.simulate_click(parent, gpui::Modifiers::default());
        draw(cx);
        let save = cx
            .debug_bounds("instruction-from-chat-save")
            .unwrap()
            .center();
        cx.simulate_click(save, gpui::Modifiers::default());
        draw(cx);
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call["op"], "createWorkItemInstruction");
        assert_eq!(call["spaceId"], "s1");
        assert_eq!(call["scope"]["type"], "workItem");
        assert_eq!(call["scope"]["workItemId"], API);
        assert_eq!(call["text"], excerpt);
        assert_eq!(call["source"]["chatId"], CHAT_AUTH);
        assert_eq!(call["source"]["messageId"], "m-7");
        assert_eq!(call["source"]["excerpt"], excerpt);
        assert_eq!(
            call["source"]["contentHash"],
            instructions::excerpt_content_hash(excerpt)
        );
        shell.read_with(cx, |shell, _| assert!(shell.work_outline_modal.is_none()));
    }

    /// `/` in Create Instruction from Chat lists skills for the chosen
    /// target's resolved execution root (re-listed when the target changes)
    /// and inserts plain "`name` skill" text; Esc closes only the list;
    /// Enter accepts instead of saving. The text grows to the cap, then
    /// scrolls with the caret in view.
    #[gpui::test]
    fn from_chat_text_offers_skills_as_plain_text_and_grows(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let calls = seed(
            &shell,
            cx,
            crate::work_outline::execution::fixture::snapshot("s1"),
            |params| {
                if params.get("harness").is_some() {
                    return Ok(serde_json::json!([
                        {"name": "code-review", "description": "Review a diff"},
                        {"name": "open-pr"},
                        {"name": "compact", "description": "Compact the conversation"},
                    ]));
                }
                ok_ids(params)
            },
        );
        let lists = |calls: &Calls| -> Vec<serde_json::Value> {
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|params| params.get("harness").is_some())
                .cloned()
                .collect()
        };
        shell.update_in(cx, |shell, window, cx| {
            shell
                .open_instruction_from_chat_with(
                    CHAT_AUTH,
                    None,
                    "Never log tokens.".into(),
                    window,
                    cx,
                )
                .unwrap()
        });
        draw(cx);
        let (input, skills) = shell.read_with(cx, |shell, _| {
            let Some(WorkOutlineModal::FromChat(dialog)) = &shell.work_outline_modal else {
                panic!("dialog");
            };
            (dialog.input.clone(), dialog.skills.clone())
        });
        let focus = input.read_with(cx, |input, cx| input.focus_handle(cx));
        cx.update(|window, cx| {
            window.activate_window();
            window.focus(&focus, cx);
        });
        draw(cx);
        let names = |cx: &mut gpui::VisualTestContext| {
            skills.read_with(cx, |skills, _| {
                if skills.is_open() {
                    skills.visible_names()
                } else {
                    Vec::new()
                }
            })
        };
        // Chat Auth's item inherits API's root.
        cx.simulate_input(" Use /");
        draw(cx);
        assert_eq!(lists(&calls)[0]["cwd"], "/work/alpha/flagship");
        assert_eq!(names(cx), vec!["code-review", "open-pr"]);
        cx.simulate_keystrokes("escape");
        draw(cx);
        assert!(names(cx).is_empty());
        assert!(
            cx.debug_bounds("instruction-from-chat").is_some(),
            "dialog stays"
        );

        // Space root target: its folder's list.
        let root = cx
            .debug_bounds("instruction-from-chat-target-root")
            .unwrap()
            .center();
        cx.simulate_click(root, gpui::Modifiers::default());
        cx.update(|window, cx| window.focus(&focus, cx));
        draw(cx);
        cx.simulate_input("c");
        draw(cx);
        assert_eq!(lists(&calls).last().unwrap()["cwd"], "/work/alpha");
        assert_eq!(names(cx), vec!["code-review"]);
        cx.simulate_keystrokes("enter");
        draw(cx);
        assert_eq!(
            input.read_with(cx, |input, _| input.text().to_owned()),
            "Never log tokens. Use `code-review` skill "
        );
        assert!(
            calls.lock().unwrap().iter().all(|c| c.get("op").is_none()),
            "Enter accepted the skill, nothing saved"
        );

        // Auto-grow up to the cap, then scroll.
        let window_height = cx.update(|window, _| f32::from(window.viewport_size().height));
        let long = (0..200)
            .map(|n| format!("rule {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        input.update(cx, |input, cx| input.set_text(long, cx));
        draw(cx);
        let frame = cx.debug_bounds("instruction-from-chat-text").unwrap();
        assert!(
            f32::from(frame.size.height)
                <= window_height * crate::composer::AUTO_GROW_MAX_FRACTION + 16.0
        );
        assert!(f32::from(frame.size.height) > 100.0);
        input.read_with(cx, |input, _| {
            assert!(input.scroll_top() > 0.0);
            assert!(input.caret_in_view());
        });
    }

    #[test]
    fn excerpt_prefers_an_anchored_selection_then_the_last_assistant_message() {
        let rows = [
            ExcerptRow {
                row_id: "m1",
                message_id: "m1",
                user: true,
                copy_text: Some("question"),
            },
            ExcerptRow {
                row_id: "m2:0",
                message_id: "m2",
                user: false,
                copy_text: None,
            },
            ExcerptRow {
                row_id: "m2:1",
                message_id: "m2",
                user: false,
                copy_text: Some("Answer text"),
            },
        ];
        assert_eq!(
            excerpt_from_transcript(&rows, Some(("m2:1:3", "  chosen  "))),
            Some((Some("m2".into()), "chosen".into()))
        );
        // A selection anchored elsewhere (another surface, or a row id that
        // merely shares a prefix) is ignored.
        assert_eq!(
            excerpt_from_transcript(&rows, Some(("m2:10:0", "x"))),
            Some((Some("m2".into()), "Answer text".into()))
        );
        assert_eq!(
            excerpt_from_transcript(&rows, Some(("other:0", "x"))),
            Some((Some("m2".into()), "Answer text".into()))
        );
        assert_eq!(
            excerpt_from_transcript(&rows[..1], None),
            None,
            "user rows are never the fallback"
        );
    }
}
