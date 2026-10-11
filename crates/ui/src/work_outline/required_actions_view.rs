//! The Required Actions list (Milestone 4): one entity used for the
//! Space-level views (embedded in Work Item Details and the Space-root
//! details) and the global surface (the Overview's right slot, opened by the
//! palette command / shortcut `OpenRequiredActions`).
//!
//! Each row shows the requested action, the work item path, the originating
//! Chat (or "Chat unavailable" when it was archived or deleted; the request
//! stays inspectable and resolvable), and the age. Optional detail expands
//! on demand. Resolve takes an optional note (explicit Resolve; Cancel or
//! Escape drops it); Open goes straight to the origin Chat when it can be
//! opened. Resolved history (the snapshot's recent window) is collapsed until
//! opened. Attention uses the theme's attention token (`chips::attention_color`).
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Subscription,
    Window, div, prelude::*, px,
};
use zeron_proto::ResolveUserActionRequestParams;

use super::actions::OperationSlot;
use super::chips::{self, TextTooltip};
use super::model::OutlineChat;
use super::required_actions::{
    self, RequestScope, RequiredActionRow, RequiredActionRows, SpaceSnapshot,
};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::state::AppState;
use crate::theme::Theme;

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
    pub(crate) error: Option<String>,
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
        let note_input = cx.new(|cx| {
            ComposerInput::with_context(
                "Optional note",
                crate::composer::PALETTE_SEARCH_CONTEXT,
                cx,
            )
            .with_single_line()
            .with_text_metrics(12.0, 18.0)
        });
        let subscriptions = vec![cx.subscribe(&note_input, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_resolve(cx);
            }
        })];
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
            error: None,
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
        self.resolving = Some(request_id);
        self.error = None;
        self.note_input
            .update(cx, |input, cx| input.set_text("", cx));
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

    /// Resolve the request whose note is open, with the typed note.
    pub(crate) fn submit_resolve(&mut self, cx: &mut Context<Self>) {
        let Some(request_id) = self.resolving.clone() else {
            return;
        };
        let note: String = self
            .note_input
            .read(cx)
            .text()
            .trim()
            .chars()
            .take(zeron_proto::user_action_limits::MAX_NOTE_CHARS)
            .collect();
        self.resolve(request_id, (!note.is_empty()).then_some(note), cx);
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
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .resolve_user_action_request(ResolveUserActionRequestParams {
                user_action_request_id: request_id.clone(),
                note,
                operation_id,
            });
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.in_flight.remove(&request_id);
                match result {
                    Ok(_) => {
                        this.ops.remove(&request_id);
                        if this.resolving.as_deref() == Some(request_id.as_str()) {
                            this.resolving = None;
                        }
                    }
                    Err(error) => {
                        this.error = Some(if super::actions::is_unsupported(&error) {
                            "Required Actions aren\u{2019}t available on this host yet".into()
                        } else {
                            error.message
                        })
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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
        div()
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
            .child(label)
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
        let resolution = row.resolution.as_ref().map(|resolution| {
            let mut line = format!(
                "Resolved {} by {}",
                required_actions::age_label(resolution.at, now),
                resolution.by
            );
            if let Some(note) = &resolution.note {
                line.push_str(" \u{b7} ");
                line.push_str(note);
            }
            div()
                .text_size(crate::typography::ui_rems(10.5))
                .text_color(theme.text_faint)
                .child(SharedString::from(line))
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
            body = body.child(
                div()
                    .debug_selector(|| "required-actions-error".into())
                    .px(px(6.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.warning)
                    .child(SharedString::from(error)),
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
