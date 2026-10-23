//! The "Diff branch / PR…" picker of the Changes pane (`m0-contract.md` §
//! "Diff a branch or PR without a checkout"): a filter input over the chat's
//! linked PRs and ticket branches that also accepts free text (a PR number or
//! URL, or a branch name). Choosing a row writes `SetChatDiffTarget
//! {bindRef}`; the engine fetches the refs read-only and the next
//! `WatchChatDiffTargets` frame selects the new pill, which puts the pane in
//! [`DiffScope::Range`].

use super::*;
use crate::diff_targets::{FreeText, RefRow, ref_rows};
use zeron_proto::{RefDiffCandidate, RefDiffKind};

/// The open picker. Same keyboard shape as [`CheckoutMenu`].
pub(super) struct RefTargetMenu {
    pub(super) search: Entity<ComposerInput>,
    /// Keyboard highlight within the filtered rows.
    active: usize,
    focus: FocusHandle,
    list_scroll: gpui::ScrollHandle,
    candidates: popover::Loadable<Vec<RefDiffCandidate>>,
    /// While a bind is fetching: what it is fetching. The list is replaced by
    /// this note and keys other than Escape are ignored.
    busy: Option<SharedString>,
    _search_events: Subscription,
    _load: Task<()>,
}

impl Changes {
    pub(super) fn close_ref_target_menu(&mut self, cx: &mut Context<Self>) {
        if self.ref_target_menu.begin_close() {
            popover::reap_popup(cx, |changes: &mut Self| &mut changes.ref_target_menu);
        }
    }

    pub(super) fn open_ref_target_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat_id) = self.owner_chat_id_owned() else {
            return;
        };
        let search = cx.new(|cx| {
            ComposerInput::with_context(
                "PR number, PR URL or branch\u{2026}",
                "PaletteSearch",
                cx,
            )
        });
        let search_events = cx.subscribe(&search, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                if let Some(menu) = this.ref_target_menu.open_mut() {
                    menu.active = 0;
                }
                cx.notify();
            }
        });
        let handle = search.read(cx).focus_handle(cx);
        let list = self
            .state
            .update(cx, |state, cx| state.list_chat_ref_candidates(&chat_id, cx));
        let load = cx.spawn(async move |this, cx| {
            let result = list.await;
            this.update(cx, |this, cx| {
                if let Some(menu) = this.ref_target_menu.open_mut() {
                    menu.candidates = match result {
                        Ok(candidates) => popover::Loadable::Ready(candidates),
                        Err(message) => popover::Loadable::Error(message),
                    };
                    menu.active = 0;
                }
                cx.notify();
            })
            .ok();
        });
        self.ref_target_menu.open(RefTargetMenu {
            search,
            active: 0,
            focus: cx.focus_handle(),
            list_scroll: gpui::ScrollHandle::new(),
            candidates: popover::Loadable::Loading,
            busy: None,
            _search_events: search_events,
            _load: load,
        });
        window.focus(&handle, cx);
        cx.notify();
    }

    /// The open picker's rows for what is typed. While the candidate list is
    /// still loading (or failed) typed text is already usable.
    pub(super) fn ref_target_rows(&self, cx: &App) -> Vec<RefRow> {
        let Some(menu) = self.ref_target_menu.get() else {
            return Vec::new();
        };
        let query = menu.search.read(cx).text().to_string();
        let empty = Vec::new();
        let candidates = menu.candidates.ready().unwrap_or(&empty);
        ref_rows(candidates, &query)
    }

    pub(super) fn ref_target_menu_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        if !self.ref_target_menu.is_open() {
            return;
        }
        let key = popover::classify_key(
            event.keystroke.key.as_str(),
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        );
        if key == popover::MenuKey::Escape {
            self.close_ref_target_menu(cx);
            cx.stop_propagation();
            return;
        }
        if self
            .ref_target_menu
            .get()
            .is_some_and(|menu| menu.busy.is_some())
        {
            return;
        }
        match key {
            popover::MenuKey::Up | popover::MenuKey::Down => {
                let count = self.ref_target_rows(cx).len();
                let delta = if key == popover::MenuKey::Up { -1 } else { 1 };
                if let Some(menu) = self.ref_target_menu.open_mut() {
                    menu.active = popover::menu_step(Some(menu.active), count, delta).unwrap_or(0);
                    menu.list_scroll.scroll_to_item(menu.active);
                    cx.notify();
                }
            }
            popover::MenuKey::Enter | popover::MenuKey::ModEnter => {
                let active = self.ref_target_menu.get().map(|m| m.active).unwrap_or(0);
                if let Some(row) = self.ref_target_rows(cx).get(active).cloned() {
                    self.choose_ref_row(row, cx);
                }
            }
            _ => {}
        }
    }

    /// Bind the chosen row: the engine resolves, fetches (read-only) and
    /// selects it. The picker stays open with a "Fetching…" note until the
    /// write returns; a failure surfaces as the pane's error notice
    /// (`AppState::chat_diff_target_error`).
    pub(super) fn choose_ref_row(&mut self, row: RefRow, cx: &mut Context<Self>) {
        let Some(chat_id) = self.owner_chat_id_owned() else {
            return;
        };
        let (label, input, repo_path) = match &row {
            RefRow::Candidate(ix) => {
                let Some(candidate) = self
                    .ref_target_menu
                    .get()
                    .and_then(|menu| menu.candidates.ready())
                    .and_then(|candidates| candidates.get(*ix))
                else {
                    return;
                };
                (
                    candidate.label.clone(),
                    candidate.input.clone(),
                    // A branch name can exist in several repositories; the
                    // picker knows which one it listed.
                    (candidate.kind == RefDiffKind::Branch).then(|| candidate.path.clone()),
                )
            }
            RefRow::FreeText(FreeText { input, label, .. }) => {
                (label.clone(), input.clone(), None)
            }
        };
        if let Some(menu) = self.ref_target_menu.open_mut() {
            menu.busy = Some(format!("Fetching {label}\u{2026}").into());
        }
        let write = self.state.update(cx, |state, cx| {
            state.clear_chat_diff_member_pick(&chat_id);
            state.bind_chat_ref_target(&chat_id, input, repo_path, cx)
        });
        cx.spawn(async move |this, cx| {
            let _ = write.await;
            this.update(cx, |this, cx| {
                this.close_ref_target_menu(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// The "Diff branch / PR…" button of the context chrome and, while open,
    /// its picker (`None` when the chat cannot pick a checkout).
    pub(super) fn render_ref_target_picker(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.checkout_picker_available(cx) {
            return None;
        }
        let trigger = div()
            .id("changes-ref-target-trigger")
            .debug_selector(|| "changes-ref-target-trigger".into())
            .flex_none()
            .h(px(20.0))
            .px(px(6.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .rounded(px(5.0))
            .cursor_pointer()
            .text_size(px(11.0))
            .text_color(theme.text_muted)
            .bg(motion::hover_blend(
                "changes-ref-target-trigger",
                crate::theme::wash(0.0),
                crate::theme::wash(0.12),
            ))
            .on_hover(motion::hover_listener("changes-ref-target-trigger"))
            .occlude()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    window.prevent_default();
                    this.ref_target_menu.note_trigger_press();
                }),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                if this.ref_target_menu.take_press_was_open() {
                    this.close_ref_target_menu(cx);
                    cx.notify();
                } else {
                    this.open_ref_target_menu(window, cx);
                }
            }))
            .child(
                crate::icons::icon(crate::icons::PULL_REQUEST)
                    .size(px(11.0))
                    .flex_none()
                    .text_color(theme.text_muted),
            )
            .child(SharedString::from("Diff branch / PR\u{2026}"));
        let trigger = if self.ref_target_menu.get().is_some() {
            let closing = self.ref_target_menu.closing_since();
            let menu = self.render_ref_target_menu(theme, cx);
            trigger.relative().child(popover::anchored_menu_below_end(
                "changes-ref-target-menu",
                menu,
                closing,
            ))
        } else {
            trigger
        };
        Some(trigger.into_any_element())
    }

    fn render_ref_target_menu(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let theme = &theme.for_popup();
        let (search, active, focus, list_scroll, state, busy) = {
            let Some(menu) = self.ref_target_menu.get() else {
                return div().into_any_element();
            };
            (
                menu.search.clone(),
                menu.active,
                menu.focus.clone(),
                menu.list_scroll.clone(),
                menu.candidates.clone(),
                menu.busy.clone(),
            )
        };
        let rows = self.ref_target_rows(cx);
        let note = |text: SharedString| {
            div()
                .px(px(8.0))
                .py(px(6.0))
                .text_size(px(12.0))
                .text_color(theme.text_faint)
                .child(text)
                .into_any_element()
        };
        let list: AnyElement = if let Some(busy) = busy {
            div()
                .debug_selector(|| "changes-ref-target-busy".into())
                .child(note(busy))
                .into_any_element()
        } else if rows.is_empty() {
            match &state {
                popover::Loadable::Idle | popover::Loadable::Loading => {
                    note("Looking for PRs and branches\u{2026}".into())
                }
                popover::Loadable::Error(message) => div()
                    .p(px(4.0))
                    .child(crate::error_notice::ErrorNotice::from_text(
                        "changes-ref-target-list-error",
                        theme,
                        message,
                    ))
                    .into_any_element(),
                popover::Loadable::Ready(_) => note(
                    "Type a PR number, a PR URL or a branch name to diff it".into(),
                ),
            }
        } else {
            let candidates: Vec<RefDiffCandidate> = state.ready().cloned().unwrap_or_default();
            div()
                .id("changes-ref-target-list")
                .flex()
                .flex_col()
                .gap(px(2.0))
                .max_h(px(280.0))
                .overflow_y_scroll()
                .track_scroll(&list_scroll)
                .children(rows.into_iter().enumerate().map(|(row_ix, row)| {
                    let (title, detail, kind_tag, suggested, bound) = match &row {
                        RefRow::Candidate(ix) => {
                            let c = &candidates[*ix];
                            (
                                c.label.clone(),
                                c.detail.clone(),
                                if c.linked { "linked PR" } else { "branch" },
                                c.suggested,
                                c.bound,
                            )
                        }
                        RefRow::FreeText(free) => (
                            format!("Diff {}", free.label),
                            "from what you typed".to_owned(),
                            if free.kind == RefDiffKind::Pr {
                                "PR"
                            } else {
                                "branch"
                            },
                            false,
                            false,
                        ),
                    };
                    let tip: SharedString = detail.clone().into();
                    let picked = row.clone();
                    popover::menu_row_nav(
                        theme,
                        false,
                        row_ix == active,
                        format!("changes-ref-target-row-{row_ix}"),
                    )
                    .id(("changes-ref-target-row", row_ix))
                    .debug_selector(move || format!("changes-ref-target-row-{row_ix}"))
                    .tooltip(move |_, cx| cx.new(|_| DiffHeaderTooltip(tip.clone())).into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.choose_ref_row(picked.clone(), cx);
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(px(12.0))
                                            .child(SharedString::from(title)),
                                    )
                                    .when(suggested, |el| {
                                        el.child(
                                            div()
                                                .flex_none()
                                                .px(px(5.0))
                                                .rounded(px(4.0))
                                                .bg(theme.accent.opacity(0.14))
                                                .text_size(px(9.5))
                                                .text_color(theme.accent)
                                                .child("suggested"),
                                        )
                                    })
                                    .when(bound, |el| {
                                        el.child(
                                            div()
                                                .flex_none()
                                                .text_size(px(9.5))
                                                .text_color(theme.text_faint)
                                                .child("open"),
                                        )
                                    })
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_size(px(9.5))
                                            .text_color(theme.text_faint)
                                            .child(kind_tag),
                                    ),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(theme.font_mono.clone())
                                    .text_size(px(10.5))
                                    .text_color(theme.text_faint)
                                    .child(SharedString::from(detail)),
                            ),
                    )
                }))
                .into_any_element()
        };

        popover::popover_card(theme)
            .w(px(360.0))
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                this.ref_target_menu_key(event, cx)
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_ref_target_menu(cx)))
            .flex()
            .flex_col()
            .child(popover::search_input_frame(
                theme,
                search.into_any_element(),
            ))
            .child(list)
            .into_any_element()
    }
}
