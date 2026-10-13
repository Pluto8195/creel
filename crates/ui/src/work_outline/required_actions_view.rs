//! The Required Actions list (Milestone 4): one entity used for the
//! Space-level views (embedded in Work Item Details and the Space-root
//! details) and the global surface (the Overview's right slot, opened by the
//! palette command / shortcut `OpenRequiredActions`).
//!
//! Each row shows the requested action, the work item path, the originating
//! Chat (or "Chat unavailable" when it was archived or deleted; the request
//! stays inspectable and resolvable), and the age. Optional detail expands
//! on demand. The note box (auto-growing) offers **Reply & resolve** (the
//! primary action while the origin Chat is available and the note is not
//! empty; Enter or Cmd+Enter): the note goes to the origin Chat as a normal
//! user message (`Re: "<request>": <note>`), and only once the host accepted
//! it does the request resolve with the same note. **Resolve** closes the
//! request without messaging (note optional). Cancel or Escape drops the
//! note. Open goes straight to the origin Chat when it can be opened; a
//! request answered by Reply & resolve offers Open chat on its resolved row.
//! Resolved history (the snapshot's recent window) is collapsed until opened.
//! Attention uses the theme's attention token (`chips::attention_color`).
//!
//! Reply delivery takes the composer's paths: an idle Chat gets its next Turn
//! through `ContinueChatForWorkItem` (message and command ids derived from
//! the reply's operation id, so a replay never sends twice; the host pins
//! the Chat's selected instruction revision and refuses a host that can't
//! deliver it), a busy one gets a `QueueMessage` row held for the turn end
//! (the composer's queue send). A queued Turn is re-read a few times so a
//! rejection (a context reset waiting on confirmation, an invalid execution
//! setup) keeps the request open instead of resolving it.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Subscription,
    Window, div, prelude::*, px,
};
use zeron_proto::work_outline::derive_id;
use zeron_proto::{
    ContinueChatForWorkItemParams, ResolveUserActionRequestParams, WorkItemLaunchState,
    WorkItemPrompt, WorkOutlineError, WorkOutlineErrorCode,
};

use super::actions::OperationSlot;
use super::chips::{self, TextTooltip};
use super::model::OutlineChat;
use super::required_actions::{
    self, RequestScope, RequiredActionRow, RequiredActionRows, SpaceSnapshot,
};
use crate::composer::instruction_reset::send_failure_text;
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::state::{AppState, Indicator};
use crate::theme::Theme;

/// Re-reads of a reply Turn the host still lists as queued before it counts
/// as accepted (a remote or offline host drains it later).
const REPLY_PROBES: usize = 6;
const REPLY_PROBE_DELAY: Duration = Duration::from_millis(500);

/// One Reply & resolve intent. The reply goes out at most once: the message
/// id derives from `operation_id`, and once the host accepted the send a
/// retry only resolves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplyIntent {
    pub operation_id: String,
    pub chat_id: String,
    pub note: String,
    pub message: String,
    /// The host accepted the send (queued or written).
    pub sent: bool,
}

/// A Reply & resolve that finished: the resolved row offers Open chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepliedRequest {
    pub request_id: String,
    pub chat_id: String,
    pub chat_label: String,
}

/// A reply Turn the host rejected (a context reset waiting on confirmation,
/// an execution setup that no longer validates, ...). The request stays open.
fn rejected_reply_text(detail: Option<&str>) -> String {
    let detail = detail
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
        .unwrap_or("the Chat refused it");
    format!(
        "Reply not delivered: {}. The request stays open.",
        detail.trim_end_matches('.')
    )
}

/// A refused reply send, worded like the composer's failed send.
fn reply_error_text(error: &WorkOutlineError) -> String {
    if super::actions::is_unsupported(error) {
        "Replying isn\u{2019}t available on this host yet. Resolve still records your note.".into()
    } else if error.code == WorkOutlineErrorCode::Capability {
        // The host's own actionable instruction-delivery message.
        error.message.clone()
    } else {
        send_failure_text(&error.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RequiredActionsEvent {
    /// Open the origin Chat (only emitted when it is available).
    OpenChat(String),
    /// Open the request's work item in details.
    OpenItem { space_id: String, item_id: String },
    /// The global surface's close button.
    Close,
}

pub(crate) struct RequiredActionsList {
    state: Entity<AppState>,
    pub(crate) scope: RequestScope,
    /// The global surface draws its own frame (title, count, close).
    global: bool,
    spaces: Vec<SpaceSnapshot>,
    chats: Arc<Vec<OutlineChat>>,
    note_input: Entity<ComposerInput>,
    /// The request whose resolve note is open (one at a time).
    pub(crate) resolving: Option<String>,
    info_open: HashSet<String>,
    pub(crate) history_open: bool,
    /// One operation id per request, reused across retries of the same note.
    ops: HashMap<String, OperationSlot>,
    in_flight: HashSet<String>,
    /// Reply & resolve intents by request id (kept until the resolve lands).
    pub(crate) replies: HashMap<String, ReplyIntent>,
    /// Requests answered by Reply & resolve here, newest last.
    pub(crate) replied: Vec<RepliedRequest>,
    /// Show "Replied to <Chat> · Open chat" for the latest reply (until the
    /// next note opens).
    reply_notice: bool,
    pub(crate) error: Option<String>,
    /// The Chat the error is about (its Open chat link), e.g. a reply the
    /// host held for a context-reset confirmation.
    error_chat: Option<String>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<RequiredActionsEvent> for RequiredActionsList {}

impl RequiredActionsList {
    pub(crate) fn new(
        state: Entity<AppState>,
        scope: RequestScope,
        global: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        // The generic composer context, like the other editors: Enter (or
        // Cmd+Enter) submits, Shift+Enter breaks a line; the box grows.
        let note_input = cx.new(|cx| {
            ComposerInput::new("Optional note", cx)
                .with_text_metrics(12.0, 18.0)
                .with_auto_grow()
        });
        let subscriptions =
            vec![
                cx.subscribe(&note_input, |this: &mut Self, _, event, cx| match event {
                    ComposerInputEvent::Submitted | ComposerInputEvent::ModifiedSubmitted => {
                        this.submit_note(cx)
                    }
                    // Reply & resolve enables as soon as a note is typed.
                    ComposerInputEvent::Edited => cx.notify(),
                    _ => {}
                }),
            ];
        Self {
            state,
            scope,
            global,
            spaces: Vec::new(),
            chats: Arc::default(),
            note_input,
            resolving: None,
            info_open: HashSet::new(),
            history_open: false,
            ops: HashMap::new(),
            in_flight: HashSet::new(),
            replies: HashMap::new(),
            replied: Vec::new(),
            reply_notice: false,
            error: None,
            error_chat: None,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// Fed by the host every frame it renders the list.
    pub(crate) fn set_data(&mut self, spaces: Vec<SpaceSnapshot>, chats: Arc<Vec<OutlineChat>>) {
        self.spaces = spaces;
        self.chats = chats;
    }

    pub(crate) fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub(crate) fn rows(&self) -> RequiredActionRows {
        required_actions::collect_rows(&self.spaces, &self.chats, &self.scope)
    }

    /// Open the note field for `request_id`.
    pub(crate) fn begin_resolve(
        &mut self,
        request_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A reply that was sent but not yet resolved keeps its note.
        let note = self
            .replies
            .get(&request_id)
            .map(|intent| intent.note.clone())
            .unwrap_or_default();
        let can_reply = self
            .open_row(&request_id)
            .is_some_and(|row| row.chat_available);
        let placeholder = if can_reply {
            "Reply to the agent, or a note"
        } else {
            "Optional note"
        };
        self.resolving = Some(request_id);
        self.reply_notice = false;
        self.clear_error();
        self.note_input.update(cx, |input, cx| {
            input.set_placeholder(placeholder, cx);
            input.set_text(note, cx);
        });
        let handle = self.note_input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    pub(crate) fn cancel_resolve(&mut self, cx: &mut Context<Self>) {
        self.resolving = None;
        cx.notify();
    }

    /// Tests drive the note through this.
    #[cfg(test)]
    pub(crate) fn set_note(&mut self, text: &str, cx: &mut Context<Self>) {
        self.note_input
            .update(cx, |input, cx| input.set_text(text, cx));
    }

    fn open_row(&self, request_id: &str) -> Option<RequiredActionRow> {
        self.rows()
            .open
            .into_iter()
            .find(|row| row.request_id == request_id)
    }

    fn clear_error(&mut self) {
        self.error = None;
        self.error_chat = None;
    }

    /// The typed note, trimmed and capped like a resolution note.
    fn typed_note(&self, cx: &gpui::App) -> String {
        self.note_input
            .read(cx)
            .text()
            .trim()
            .chars()
            .take(zeron_proto::user_action_limits::MAX_NOTE_CHARS)
            .collect()
    }

    /// Enter / Cmd+Enter in the note box: Reply & resolve when it is
    /// available (origin Chat open, note typed), else Resolve.
    pub(crate) fn submit_note(&mut self, cx: &mut Context<Self>) {
        let Some(request_id) = self.resolving.clone() else {
            return;
        };
        let can_reply = self.open_row(&request_id).is_some_and(|row| {
            required_actions::reply_unavailable_reason(&row, &self.typed_note(cx)).is_none()
        });
        if can_reply {
            self.submit_reply(cx);
        } else {
            self.submit_resolve(cx);
        }
    }

    /// Resolve the request whose note is open, with the typed note.
    pub(crate) fn submit_resolve(&mut self, cx: &mut Context<Self>) {
        let Some(request_id) = self.resolving.clone() else {
            return;
        };
        let note = self.typed_note(cx);
        self.resolve(request_id, (!note.is_empty()).then_some(note), cx);
    }

    /// Reply & resolve the request whose note is open.
    pub(crate) fn submit_reply(&mut self, cx: &mut Context<Self>) {
        let Some(request_id) = self.resolving.clone() else {
            return;
        };
        let note = self.typed_note(cx);
        self.reply_and_resolve(request_id, note, cx);
    }

    /// `resolveUserActionRequest` (idempotent per request + note).
    pub(crate) fn resolve(
        &mut self,
        request_id: String,
        note: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if !self.in_flight.insert(request_id.clone()) {
            return;
        }
        let operation_id = self
            .ops
            .entry(request_id.clone())
            .or_default()
            .id_for(note.as_deref().unwrap_or(""));
        self.send_resolve(request_id, note, operation_id, None, cx);
    }

    /// Reply & resolve: send `note` to the request's origin Chat as a user
    /// message, then resolve with the same note once the host accepted it.
    /// One operation id per intent (same note): a retry never sends twice,
    /// and after "sent but not resolved" it only resolves.
    pub(crate) fn reply_and_resolve(
        &mut self,
        request_id: String,
        note: String,
        cx: &mut Context<Self>,
    ) {
        if self.in_flight.contains(&request_id) {
            return;
        }
        let pending = self.replies.get(&request_id).cloned();
        let intent = match pending {
            // Sent already: the message went out with that note.
            Some(intent) if intent.sent => intent,
            Some(intent) if intent.note == note => intent,
            _ => {
                let Some(row) = self.open_row(&request_id) else {
                    return;
                };
                if required_actions::reply_unavailable_reason(&row, &note).is_some() {
                    return;
                }
                ReplyIntent {
                    operation_id: super::actions::new_operation_id(),
                    chat_id: row.chat_id.clone(),
                    message: required_actions::reply_message(&row.text, &note),
                    note,
                    sent: false,
                }
            }
        };
        self.replies.insert(request_id.clone(), intent.clone());
        self.in_flight.insert(request_id.clone());
        self.clear_error();
        cx.notify();
        if intent.sent {
            self.resolve_reply(request_id, intent, cx);
        } else {
            self.send_reply(request_id, intent, cx);
        }
    }

    /// The composer's notion of a busy Chat: its message queues.
    fn chat_busy(&self, chat_id: &str, cx: &gpui::App) -> bool {
        matches!(
            self.state
                .read(cx)
                .indicator_for(chat_id, chrono::Utc::now()),
            Indicator::Working | Indicator::AwaitingInput
        )
    }

    /// Whether the busy-Chat queue send can reach `chat_id`'s host (the
    /// composer's check; without an engine the call itself reports it).
    fn queue_supported(&self, chat_id: &str, cx: &gpui::App) -> bool {
        let capability = zeron_proto::capabilities::MESSAGE_QUEUE_V1;
        let state = self.state.read(cx);
        state.engine().is_none_or(|engine| {
            engine.engine_info().supports(capability)
                && state.chat_host_supports(chat_id, capability)
        })
    }

    fn send_reply(&mut self, request_id: String, intent: ReplyIntent, cx: &mut Context<Self>) {
        let busy = self.chat_busy(&intent.chat_id, cx);
        if busy && !self.queue_supported(&intent.chat_id, cx) {
            self.fail_reply(
                &request_id,
                "Update the chat\u{2019}s engine to queue messages during a response.".into(),
                None,
                cx,
            );
            return;
        }
        let actions = self.state.read(cx).work_outline_actions();
        let chat_id = intent.chat_id.clone();
        cx.spawn(async move |this, cx| {
            let outcome: Result<(), String> = if busy {
                actions
                    .queue_message(&intent.chat_id, &intent.message)
                    .await
                    .map(drop)
                    .map_err(|error| send_failure_text(&error.message))
            } else {
                let params = ContinueChatForWorkItemParams {
                    chat_id: intent.chat_id.clone(),
                    prompt: WorkItemPrompt {
                        text: intent.message.clone(),
                        attachments: Vec::new(),
                    },
                    operation_id: intent.operation_id.clone(),
                };
                let mut reply = actions.continue_chat_for_work_item(params.clone()).await;
                // The same operation id replays without writing: re-read a
                // queued Turn so a prompt rejection keeps the request open.
                for _ in 0..REPLY_PROBES {
                    if !matches!(&reply, Ok(r) if r.state == WorkItemLaunchState::Queued) {
                        break;
                    }
                    cx.background_executor().timer(REPLY_PROBE_DELAY).await;
                    reply = actions.continue_chat_for_work_item(params.clone()).await;
                }
                match reply {
                    Ok(reply) if reply.state == WorkItemLaunchState::Rejected => {
                        Err(rejected_reply_text(reply.detail.as_deref()))
                    }
                    Ok(_) => Ok(()),
                    Err(error) => Err(reply_error_text(&error)),
                }
            };
            this.update(cx, |this, cx| match outcome {
                Ok(()) => {
                    let mut intent = intent;
                    intent.sent = true;
                    this.replies.insert(request_id.clone(), intent.clone());
                    this.resolve_reply(request_id, intent, cx);
                }
                Err(message) => this.fail_reply(&request_id, message, Some(chat_id), cx),
            })
            .ok();
        })
        .detach();
    }

    fn fail_reply(
        &mut self,
        request_id: &str,
        message: String,
        chat_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.in_flight.remove(request_id);
        self.error = Some(message);
        self.error_chat = chat_id.filter(|chat| {
            self.open_row(request_id)
                .is_some_and(|row| row.chat_available && row.chat_id == *chat)
        });
        cx.notify();
    }

    /// The resolve half of Reply & resolve: its own operation id, derived
    /// from the reply's.
    fn resolve_reply(&mut self, request_id: String, intent: ReplyIntent, cx: &mut Context<Self>) {
        let operation_id = derive_id(&intent.operation_id, required_actions::REPLY_RESOLVE_ROLE);
        let chat_label = self
            .chats
            .iter()
            .find(|chat| chat.id == intent.chat_id)
            .map(|chat| chat.title.clone())
            .unwrap_or_default();
        let replied = RepliedRequest {
            request_id: request_id.clone(),
            chat_id: intent.chat_id.clone(),
            chat_label,
        };
        self.send_resolve(
            request_id,
            Some(intent.note),
            operation_id,
            Some(replied),
            cx,
        );
    }

    /// Send one resolve (the request is already marked in flight).
    fn send_resolve(
        &mut self,
        request_id: String,
        note: Option<String>,
        operation_id: String,
        replied: Option<RepliedRequest>,
        cx: &mut Context<Self>,
    ) {
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .resolve_user_action_request(ResolveUserActionRequestParams {
                user_action_request_id: request_id.clone(),
                note,
                operation_id,
            });
        self.clear_error();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.in_flight.remove(&request_id);
                match result {
                    Ok(_) => {
                        this.ops.remove(&request_id);
                        this.replies.remove(&request_id);
                        if let Some(replied) = replied {
                            this.replied.retain(|r| r.request_id != request_id);
                            this.replied.push(replied);
                            this.reply_notice = true;
                        }
                        if this.resolving.as_deref() == Some(request_id.as_str()) {
                            this.resolving = None;
                            this.note_input
                                .update(cx, |input, cx| input.set_text("", cx));
                        }
                    }
                    Err(error) => {
                        let message = if super::actions::is_unsupported(&error) {
                            "Required Actions aren\u{2019}t available on this host yet".into()
                        } else {
                            error.message
                        };
                        this.error = Some(if replied.is_some() {
                            format!(
                                "Your reply was sent, but the request couldn\u{2019}t be \
                                 resolved: {message}. Retrying only resolves it."
                            )
                        } else {
                            message
                        });
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The most recent Reply & resolve of `request_id`, if it happened here.
    fn replied_to(&self, request_id: &str) -> Option<&RepliedRequest> {
        self.replied.iter().find(|r| r.request_id == request_id)
    }

    /// Whether a Reply & resolve finished here (hosts keep the list open so
    /// its Open chat link stays reachable).
    pub(crate) fn has_replied(&self) -> bool {
        !self.replied.is_empty()
    }

    /// Open: the origin Chat when it can be opened, else nothing (the row
    /// stays inspectable in place).
    pub(crate) fn open(&mut self, row: &RequiredActionRow, cx: &mut Context<Self>) {
        if row.chat_available {
            cx.emit(RequiredActionsEvent::OpenChat(row.chat_id.clone()));
        }
    }

    // ----------------------------------------------------------------------
    // Rendering
    // ----------------------------------------------------------------------

    fn button(
        id: impl Into<gpui::ElementId>,
        label: &'static str,
        enabled: bool,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        let button = div()
            .id(id)
            .flex_none()
            .px(px(6.0))
            .py(px(1.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(theme.border)
            .text_size(crate::typography::ui_rems(10.5))
            .when(enabled, |el| {
                el.cursor_pointer()
                    .text_color(theme.text_muted)
                    .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
            })
            .when(!enabled, |el| el.text_color(theme.text_faint))
            .child(label);
        match super::hints::details_button_hint(label) {
            Some((tip, key)) => super::hints::WithHint::hint(button, tip, key),
            None => button,
        }
    }

    /// The primary Reply & resolve button: accent-tinted; when disabled its
    /// tooltip says why.
    fn reply_button(
        id: &str,
        label: &'static str,
        unavailable: Option<&'static str>,
        busy: bool,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        let enabled = unavailable.is_none() && !busy;
        let selector = id.to_owned();
        let tip: SharedString = unavailable
            .unwrap_or("Send your note to the Chat that asked, then resolve (\u{21a9})")
            .into();
        div()
            .id(SharedString::from(id.to_owned()))
            .debug_selector(move || selector.clone())
            .flex_none()
            .px(px(6.0))
            .py(px(1.0))
            .rounded(px(4.0))
            .border_1()
            .text_size(crate::typography::ui_rems(10.5))
            .when(enabled, |el| {
                el.cursor_pointer()
                    .border_color(theme.accent.opacity(0.5))
                    .bg(theme.accent.opacity(0.12))
                    .text_color(theme.accent)
                    .hover(|el| el.bg(theme.accent.opacity(0.22)))
            })
            .when(!enabled, |el| {
                el.border_color(theme.border).text_color(theme.text_faint)
            })
            .child(label)
            .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
    }

    /// A subtle "Open chat" link.
    fn open_chat_link(
        &self,
        selector: String,
        chat_id: String,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let debug = selector.clone();
        div()
            .id(SharedString::from(selector))
            .debug_selector(move || debug)
            .flex_none()
            .cursor_pointer()
            .text_color(theme.text_muted)
            .underline()
            .hover(|el| el.text_color(theme.text))
            .child("Open chat")
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(RequiredActionsEvent::OpenChat(chat_id.clone()))
            }))
    }

    fn chat_open(&self, chat_id: &str) -> bool {
        self.chats
            .iter()
            .any(|chat| chat.id == chat_id && !chat.archived)
    }

    fn render_row(
        &self,
        row: RequiredActionRow,
        now: chrono::DateTime<chrono::Utc>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = row.request_id.clone();
        let open = row.resolution.is_none();
        let info_open = self.info_open.contains(&id);
        let resolving = open && self.resolving.as_deref() == Some(id.as_str());
        let busy = self.in_flight.contains(&id);
        let attention = chips::attention_color(theme);
        let age = required_actions::age_label(row.created_at, now);
        let chat_tip: SharedString = if row.chat_available {
            format!("Open {}", row.chat_label).into()
        } else {
            "The originating Chat was archived or deleted. The request stays here until you \
             resolve it."
                .into()
        };
        let path_space = row.space_id.clone();
        let path_item = row.work_item_id.clone();
        let mut meta = div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .min_w_0()
            .text_size(crate::typography::ui_rems(10.5))
            .text_color(theme.text_muted)
            .child(
                div()
                    .id(SharedString::from(format!("required-actions-path-{id}")))
                    .min_w_0()
                    .max_w(px(150.0))
                    .truncate()
                    .cursor_pointer()
                    .hover(|el| el.text_color(theme.text))
                    .child(SharedString::from(row.path.clone()))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(RequiredActionsEvent::OpenItem {
                            space_id: path_space.clone(),
                            item_id: path_item.clone(),
                        })
                    })),
            )
            .child(div().flex_none().child("\u{b7}"))
            .child(
                div()
                    .id(SharedString::from(format!("required-actions-chat-{id}")))
                    .debug_selector({
                        let selector = format!("required-actions-chat-{id}");
                        move || selector.clone()
                    })
                    .min_w_0()
                    .max_w(px(120.0))
                    .truncate()
                    .when(!row.chat_available, |el| {
                        el.italic().text_color(theme.text_faint)
                    })
                    .child(SharedString::from(row.chat_label.clone()))
                    .tooltip(move |_, cx| cx.new(|_| TextTooltip(chat_tip.clone())).into()),
            )
            .child(div().flex_none().child("\u{b7}"))
            .child(div().flex_none().child(SharedString::from(age)));
        if row.duplicates > 0 {
            meta = meta.child(
                div()
                    .flex_none()
                    .text_color(theme.text_faint)
                    .child(SharedString::from(format!("+{} retried", row.duplicates))),
            );
        }
        let actions = open.then(|| {
            let open_row = row.clone();
            let resolve_id = id.clone();
            div()
                .flex()
                .items_center()
                .gap(px(4.0))
                .child(div().flex_1())
                .when(row.info.is_some(), |el| {
                    let toggle = id.clone();
                    el.child(
                        Self::button(
                            SharedString::from(format!("required-actions-info-toggle-{id}")),
                            if info_open { "Hide details" } else { "Details" },
                            true,
                            theme,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.info_open.remove(&toggle) {
                                this.info_open.insert(toggle.clone());
                            }
                            cx.notify();
                        })),
                    )
                })
                .child(
                    Self::button(
                        SharedString::from(format!("required-actions-open-{id}")),
                        "Open",
                        row.chat_available,
                        theme,
                    )
                    .debug_selector({
                        let selector = format!("required-actions-open-{id}");
                        move || selector.clone()
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.open(&open_row, cx))),
                )
                .when(!resolving, |el| {
                    el.child(
                        Self::button(
                            SharedString::from(format!("required-actions-resolve-{id}")),
                            if busy { "Resolving\u{2026}" } else { "Resolve" },
                            !busy,
                            theme,
                        )
                        .debug_selector({
                            let selector = format!("required-actions-resolve-{id}");
                            move || selector.clone()
                        })
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.begin_resolve(resolve_id.clone(), window, cx)
                            },
                        )),
                    )
                    .child({
                        let reply_id = id.clone();
                        let unavailable = (!row.chat_available).then(|| {
                            required_actions::reply_unavailable_reason(&row, "x")
                                .unwrap_or_default()
                        });
                        let button = Self::reply_button(
                            &format!("required-actions-reply-{id}"),
                            "Reply & resolve",
                            unavailable,
                            busy,
                            theme,
                        );
                        if unavailable.is_none() && !busy {
                            button.on_click(cx.listener(move |this, _, window, cx| {
                                this.begin_resolve(reply_id.clone(), window, cx)
                            }))
                        } else {
                            button
                        }
                    })
                })
        });
        let resolve_form = resolving.then(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        this.cancel_resolve(cx);
                        cx.stop_propagation();
                    }
                }))
                .child(
                    div()
                        .debug_selector(|| "required-actions-note".into())
                        .w_full()
                        .min_h(px(24.0))
                        .px(px(6.0))
                        .py(px(3.0))
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(theme.accent)
                        .bg(theme.surface)
                        .child(self.note_input.clone()),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(6.0))
                        .child({
                            let note = self.note_input.read(cx).text().to_owned();
                            let intent = self.replies.get(&id);
                            // A reply already sent only needs its resolve.
                            let sent = intent.is_some_and(|intent| intent.sent);
                            let unavailable = if sent {
                                None
                            } else {
                                required_actions::reply_unavailable_reason(&row, &note)
                            };
                            let label = match (busy, sent) {
                                (true, false) if intent.is_some() => "Sending\u{2026}",
                                (true, _) => "Resolving\u{2026}",
                                _ => "Reply & resolve",
                            };
                            let button = Self::reply_button(
                                "required-actions-reply-submit",
                                label,
                                unavailable,
                                busy,
                                theme,
                            );
                            if unavailable.is_none() && !busy {
                                button.on_click(cx.listener(|this, _, _, cx| this.submit_reply(cx)))
                            } else {
                                button
                            }
                        })
                        .child(
                            Self::button(
                                "required-actions-resolve-submit",
                                "Resolve",
                                !busy,
                                theme,
                            )
                            .debug_selector(|| "required-actions-resolve-submit".into())
                            .on_click(cx.listener(|this, _, _, cx| this.submit_resolve(cx))),
                        )
                        .child(
                            Self::button("required-actions-resolve-cancel", "Cancel", true, theme)
                                .on_click(cx.listener(|this, _, _, cx| this.cancel_resolve(cx))),
                        ),
                )
        });
        // Answered by Reply & resolve here: the resolved row links the Chat.
        let replied_chat = self
            .replied_to(&id)
            .filter(|_| row.chat_available)
            .map(|replied| replied.chat_id.clone());
        let resolution = row.resolution.as_ref().map(|resolution| {
            let replied = replied_chat.is_some();
            let mut line = format!(
                "{} {} by {}",
                if replied {
                    "Replied and resolved"
                } else {
                    "Resolved"
                },
                required_actions::age_label(resolution.at, now),
                resolution.by
            );
            if let Some(note) = &resolution.note {
                line.push_str(" \u{b7} ");
                line.push_str(note);
            }
            div()
                .flex()
                .items_start()
                .gap(px(6.0))
                .text_size(crate::typography::ui_rems(10.5))
                .text_color(theme.text_faint)
                .child(div().min_w_0().child(SharedString::from(line)))
                .when_some(replied_chat.clone(), |el, chat_id| {
                    el.child(self.open_chat_link(
                        format!("required-actions-history-open-chat-{id}"),
                        chat_id,
                        theme,
                        cx,
                    ))
                })
        });
        let selector = if open {
            format!("required-actions-row-{id}")
        } else {
            format!("required-actions-history-row-{id}")
        };
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .flex()
            .flex_col()
            .gap(px(2.0))
            .px(px(6.0))
            .py(px(4.0))
            .rounded(px(4.0))
            .hover(|el| el.bg(theme.element_hover.opacity(0.5)))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex_none()
                            .mt(px(5.0))
                            .size(px(6.0))
                            .rounded_full()
                            .bg(if open { attention } else { theme.text_faint }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(if open { theme.text } else { theme.text_muted })
                            .child(SharedString::from(row.text.clone())),
                    ),
            )
            .child(div().pl(px(12.0)).child(meta))
            .when(info_open, |el| {
                el.child(
                    div()
                        .debug_selector({
                            let selector = format!("required-actions-info-{id}");
                            move || selector.clone()
                        })
                        .pl(px(12.0))
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(row.info.clone().unwrap_or_default())),
                )
            })
            .when_some(resolution, |el, line| {
                el.child(div().pl(px(12.0)).child(line))
            })
            .when_some(actions, |el, actions| el.child(actions))
            .when_some(resolve_form, |el, form| {
                el.child(div().pl(px(12.0)).child(form))
            })
            .into_any_element()
    }

    /// The list body: open rows, then the collapsed history.
    fn render_body(
        &self,
        rows: RequiredActionRows,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let now = chrono::Utc::now();
        let history_count = rows.resolved.len();
        let mut body = div().flex().flex_col().gap(px(2.0));
        if rows.open.is_empty() {
            body = body.child(
                div()
                    .debug_selector(|| "required-actions-empty".into())
                    .px(px(6.0))
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.text_faint)
                    .child("Nothing needs you right now"),
            );
        }
        for row in rows.open {
            body = body.child(self.render_row(row, now, theme, cx));
        }
        if let Some(replied) = self
            .replied
            .last()
            .filter(|replied| self.reply_notice && self.chat_open(&replied.chat_id))
        {
            let label = if replied.chat_label.is_empty() {
                "Reply sent to the Chat".to_owned()
            } else {
                format!("Reply sent to {}", replied.chat_label)
            };
            body = body.child(
                div()
                    .debug_selector(|| "required-actions-replied".into())
                    .px(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .min_w_0()
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_faint)
                    .child(div().min_w_0().truncate().child(SharedString::from(label)))
                    .child(div().flex_none().child("\u{b7}"))
                    .child(self.open_chat_link(
                        "required-actions-replied-open-chat".into(),
                        replied.chat_id.clone(),
                        theme,
                        cx,
                    )),
            );
        }
        if history_count > 0 {
            body = body.child(
                div()
                    .id("required-actions-history-toggle")
                    .debug_selector(|| "required-actions-history-toggle".into())
                    .mt(px(4.0))
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_muted)
                    .hover(|el| el.text_color(theme.text))
                    .child(
                        crate::icons::icon(if self.history_open {
                            crate::icons::ALT_ARROW_DOWN
                        } else {
                            crate::icons::ALT_ARROW_RIGHT
                        })
                        .size(px(10.0))
                        .text_color(theme.text_muted),
                    )
                    .child(SharedString::from(format!("Resolved ({history_count})")))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.history_open = !this.history_open;
                        cx.notify();
                    })),
            );
            if self.history_open {
                for row in rows.resolved {
                    body = body.child(self.render_row(row, now, theme, cx));
                }
            }
        }
        if let Some(error) = self.error.clone() {
            let error_chat = self
                .error_chat
                .clone()
                .filter(|chat_id| self.chat_open(chat_id));
            body = body.child(
                div()
                    .debug_selector(|| "required-actions-error".into())
                    .px(px(6.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.warning)
                    .child(SharedString::from(error))
                    .when_some(error_chat, |el, chat_id| {
                        el.child(self.open_chat_link(
                            "required-actions-error-open-chat".into(),
                            chat_id,
                            theme,
                            cx,
                        ))
                    }),
            );
        }
        body
    }
}

impl Render for RequiredActionsList {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let rows = self.rows();
        if !self.global {
            return div()
                .id("required-actions-list")
                .debug_selector(|| "required-actions-list".into())
                .child(self.render_body(rows, &theme, cx));
        }
        let count = rows.open.len();
        let close = div()
            .id("required-actions-close")
            .debug_selector(|| "required-actions-close".into())
            .flex_none()
            .size(px(22.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .cursor_pointer()
            .hover(|el| el.bg(theme.element_hover))
            .child(
                crate::icons::icon(crate::icons::CLOSE)
                    .size(px(12.0))
                    .text_color(theme.text_muted),
            )
            .on_click(cx.listener(|_, _, _, cx| cx.emit(RequiredActionsEvent::Close)));
        div()
            .id("required-actions-panel")
            .debug_selector(|| "required-actions-panel".into())
            .track_focus(&self.focus)
            .flex_none()
            .w(px(super::details_panel::DETAILS_PANEL_W))
            .h_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key != "escape" {
                    return;
                }
                if this.resolving.is_some() {
                    this.cancel_resolve(cx);
                } else {
                    cx.emit(RequiredActionsEvent::Close);
                }
                cx.stop_propagation();
            }))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(14.0))
                    .pt(px(10.0))
                    .pb(px(6.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(crate::typography::ui_rems(15.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child("Required Actions"),
                    )
                    .when(count > 0, |el| {
                        el.child(
                            div()
                                .debug_selector(|| "required-actions-count".into())
                                .flex_none()
                                .text_size(crate::typography::ui_rems(11.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(chips::attention_color(&theme))
                                .child(SharedString::from(count.to_string())),
                        )
                    })
                    .child(close),
            )
            .child(
                div()
                    .id("required-actions-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(8.0))
                    .pb(px(10.0))
                    .child(self.render_body(rows, &theme, cx)),
            )
    }
}
