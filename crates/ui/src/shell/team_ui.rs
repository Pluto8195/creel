//! The side panel's Team tab: one row per worker of an orchestrating chat,
//! a roll-up line on top, and the Item tab's compact "top workers" block. Both
//! read one [`TeamFacts`] sheet (`crate::team`), so they cannot disagree.
//!
//! ⌥⌘T opens the tab from anywhere; a plain `t` does the same while the side
//! panel itself (not a text field inside it) holds focus. Both do nothing for
//! a chat without workers.
use super::*;

use crate::shell::chat_item_ui::{ChipTone, chip_palette};
use crate::team::{
    ITEM_TAB_WORKERS, TeamFacts, TeamInput, WorkerFacts, WorkerPr, WorkerState, team,
};

impl Shell {
    /// The selected chat's workers. Link statuses (PR checks, diff stats) come
    /// from the header-link cache; missing ones are requested and the tab
    /// repaints when they land.
    pub(super) fn team_facts(&mut self, chat_id: &str, cx: &mut Context<Self>) -> TeamFacts {
        let facts = {
            let state = self.state.read(cx);
            let Some(chat) = state.chats.iter().find(|chat| chat.id == chat_id) else {
                return TeamFacts::default();
            };
            let now = chrono::Utc::now();
            let indicators = state
                .chats
                .iter()
                .map(|chat| (chat.id.clone(), state.display_status_for(chat, now)))
                .collect();
            let snapshot = chat
                .space_id
                .as_deref()
                .and_then(|id| state.work_outline(id).snapshot().cloned());
            let links = self
                .chat_header_links
                .iter()
                .filter_map(|(id, entry)| Some((id.clone(), entry.status.clone()?)))
                .collect();
            team(&TeamInput {
                chat_id,
                chats: &state.chats,
                indicators: &indicators,
                snapshot: snapshot.as_deref(),
                links: &links,
            })
        };
        for worker in &facts.workers {
            self.ensure_chat_header_links(worker.chat_id.clone(), cx);
        }
        facts
    }

    /// ⌥⌘T / `t`: open the Team tab; nothing happens without workers.
    pub(crate) fn open_team_tab(&mut self, cx: &mut Context<Self>) {
        if self.active_chat.is_empty() {
            return;
        }
        let chat_id = self.active_chat.clone();
        if self.team_facts(&chat_id, cx).is_empty() {
            return;
        }
        self.leave_overview_for_chat(cx);
        self.set_surfaces_open(true, cx);
        self.set_right_active(RightSurface::Team, cx);
        cx.notify();
    }

    /// The focus target of the Item and Team tabs' bodies.
    pub(super) fn team_panel_focus(&mut self, cx: &mut Context<Self>) -> FocusHandle {
        self.team_panel_focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone()
    }

    fn state_color(theme: &Theme, state: WorkerState) -> gpui::Hsla {
        match state {
            WorkerState::Waiting => theme.warning,
            WorkerState::Errored => theme.danger,
            WorkerState::Working => theme.accent,
            WorkerState::Idle => theme.text_muted,
            WorkerState::Done => theme.success,
        }
    }

    /// The sticky chip after "Item": present only while the chat has workers.
    pub(super) fn render_team_tab_chip(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let chat_id = self.active_chat.clone();
        let team = self.team_facts(&chat_id, cx);
        if team.is_empty() {
            return None;
        }
        let theme = Theme::of(cx).clone();
        let active = self.resolved_right_active(cx) == RightSurface::Team;
        let waiting = team.waiting() > 0;
        Some(
            div()
                .id("right-surface-team-tab")
                .debug_selector(|| "right-surface-team-tab".into())
                .h(px(24.0))
                .flex_none()
                .px(px(8.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .gap(px(5.0))
                .cursor_pointer()
                .role(gpui::Role::Button)
                .aria_label("Team")
                .block_mouse_except_scroll()
                .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
                    window.prevent_default()
                })
                .when(active, |el| el.bg(crate::theme::wash(0.10)))
                .when(!active, |el| el.hover(|s| s.bg(crate::theme::wash(0.06))))
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.set_right_active(RightSurface::Team, cx);
                }))
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(11.5))
                        .text_color(if active { theme.text } else { theme.text_muted })
                        .child(SharedString::from(format!("Team {}", team.workers.len()))),
                )
                .when(waiting, |el| {
                    el.child(
                        div()
                            .debug_selector(|| "right-surface-team-dot".into())
                            .flex_none()
                            .size(px(6.0))
                            .rounded_full()
                            .bg(theme.warning),
                    )
                })
                .into_any_element(),
        )
    }

    /// The Team tab's body: roll-up line, then one row per worker.
    pub(super) fn render_team_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let chat_id = self.active_chat.clone();
        let team = self.team_facts(&chat_id, cx);
        let focus = self.team_panel_focus(cx);
        let mut col = div().flex().flex_col().gap(px(8.0));
        if team.is_empty() {
            col = col.child(
                div()
                    .debug_selector(|| "team-empty".into())
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.text_faint)
                    .child("This chat has no workers."),
            );
        } else {
            col = col.child(
                div()
                    .debug_selector(|| "team-rollup".into())
                    .text_size(crate::typography::ui_rems(12.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(SharedString::from(team.roll_up())),
            );
            for worker in &team.workers {
                col = col.child(self.render_worker_row(worker, false, &theme, cx));
            }
        }
        self.panel_root("team-tab", focus, col, cx)
    }

    /// The scrolling, focusable root shared by the Item and Team tabs.
    pub(super) fn panel_root(
        &mut self,
        id: &'static str,
        focus: FocusHandle,
        body: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tracked = focus.clone();
        div()
            .id(id)
            .debug_selector(move || id.into())
            .track_focus(&focus)
            .size_full()
            .overflow_y_scroll()
            .p(px(14.0))
            .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                let key = &event.keystroke;
                // Plain `t` only while the panel itself is focused: a text
                // field inside it owns its own focus handle.
                if key.key == "t"
                    && !key.modifiers.modified()
                    && tracked.is_focused(window)
                    && this.resolved_right_active(cx) != RightSurface::Team
                {
                    this.open_team_tab(cx);
                    cx.stop_propagation();
                }
            }))
            .child(body)
            .into_any_element()
    }

    /// The Item tab's block: the roll-up line, the first workers, and a link
    /// to the Team tab.
    pub(super) fn render_item_tab_team(
        &mut self,
        team: &TeamFacts,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut block = div()
            .debug_selector(|| "chat-item-team".into())
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .id("chat-item-team-rollup")
                    .debug_selector(|| "chat-item-team-rollup".into())
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(12.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .hover(|el| el.text_color(theme.accent))
                    .child(SharedString::from(format!("Team \u{b7} {}", team.roll_up())))
                    .on_click(cx.listener(|this, _, _, cx| this.open_team_tab(cx))),
            );
        for worker in team.workers.iter().take(ITEM_TAB_WORKERS) {
            block = block.child(self.render_worker_row(worker, true, theme, cx));
        }
        if team.workers.len() > ITEM_TAB_WORKERS {
            block = block.child(
                div()
                    .id("chat-item-team-all")
                    .debug_selector(|| "chat-item-team-all".into())
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.accent)
                    .child(SharedString::from(format!(
                        "All {} workers \u{2192} Team",
                        team.workers.len()
                    )))
                    .on_click(cx.listener(|this, _, _, cx| this.open_team_tab(cx))),
            );
        }
        block.into_any_element()
    }

    fn chip(theme: &Theme, id: String, label: String, tone: ChipTone) -> gpui::Stateful<gpui::Div> {
        let (border, text) = chip_palette(theme, tone);
        let selector = id.clone();
        div()
            .id(SharedString::from(id))
            .debug_selector(move || selector.clone())
            .flex_none()
            .h(px(18.0))
            .px(px(6.0))
            .flex()
            .items_center()
            .rounded(px(5.0))
            .border_1()
            .border_color(border)
            .text_size(crate::typography::ui_rems(10.5))
            .text_color(text)
            .child(SharedString::from(label))
    }

    fn pr_tone(pr: &WorkerPr) -> ChipTone {
        use mast_engine::pr_ticket_cache::ChecksStatus;
        match (pr.checks, pr.review) {
            (Some(ChecksStatus::Failing), _) | (_, Some("changes requested")) => ChipTone::Warn,
            _ => ChipTone::Link,
        }
    }

    /// One worker. `compact` (the Item tab) is title, state and age plus the
    /// activity line; the Team tab adds chips, diff stats and the
    /// Ready-to-merge jump.
    fn render_worker_row(
        &mut self,
        worker: &WorkerFacts,
        compact: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let prefix = if compact { "chat-item-worker" } else { "team-worker" };
        let id = worker.chat_id.clone();
        let selector = format!("{prefix}-{id}");
        let now = chrono::Utc::now();
        let age = worker
            .last_active
            .map(|at| mast_proto::view::format_time_ago(at, now))
            .unwrap_or_default();
        let color = Self::state_color(theme, worker.state);
        let open_id = id.clone();
        let mut row = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .flex()
            .flex_col()
            .gap(px(2.0))
            .px(px(6.0))
            .py(px(5.0))
            .rounded(px(5.0))
            .border_1()
            .border_color(theme.border)
            .cursor_pointer()
            .hover(|el| el.bg(theme.element_hover.opacity(0.5)))
            .on_click(cx.listener(move |this, _, _, cx| this.open_chat(open_id.clone(), cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(div().flex_none().size(px(7.0)).rounded_full().bg(color))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(12.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(SharedString::from(worker.title.clone())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(crate::typography::ui_rems(10.5))
                            .text_color(color)
                            .child(SharedString::from(worker.state.label())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(crate::typography::ui_rems(10.5))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(age)),
                    ),
            );
        if let Some(activity) = &worker.activity {
            row = row.child(
                div()
                    .debug_selector({
                        let selector = format!("{prefix}-activity-{id}");
                        move || selector.clone()
                    })
                    .pl(px(13.0))
                    .truncate()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(activity.clone())),
            );
        }
        if !compact {
            let mut chips = div().pl(px(13.0)).flex().flex_wrap().items_center().gap(px(4.0));
            let mut any = false;
            if let Some(branch) = &worker.branch {
                any = true;
                chips = chips.child(Self::chip(
                    theme,
                    format!("team-branch-{id}"),
                    format!("\u{2387} {branch}"),
                    ChipTone::Dim,
                ));
            }
            for (ix, pr) in worker.prs.iter().enumerate() {
                any = true;
                let url = pr.url.clone();
                chips = chips.child(
                    Self::chip(theme, format!("team-pr-{id}-{ix}"), pr.label(), Self::pr_tone(pr))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            if !url.is_empty() {
                                let external = window.modifiers().secondary();
                                this.open_web_reference(&url, external, window, cx)
                            }
                        })),
                );
            }
            if let Some(ticket) = &worker.ticket {
                any = true;
                let url = ticket.url.clone();
                chips = chips.child(
                    Self::chip(
                        theme,
                        format!("team-ticket-{id}"),
                        ticket.identifier.clone(),
                        ChipTone::Link,
                    )
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        if let Some(url) = &url {
                            let external = window.modifiers().secondary();
                            this.open_web_reference(url, external, window, cx)
                        }
                    })),
                );
            }
            if let Some((added, removed)) = worker.diff {
                any = true;
                chips = chips.child(
                    div()
                        .debug_selector({
                            let selector = format!("team-diff-{id}");
                            move || selector.clone()
                        })
                        .flex_none()
                        .flex()
                        .gap(px(4.0))
                        .text_size(crate::typography::ui_rems(10.5))
                        .child(
                            div()
                                .text_color(theme.success)
                                .child(SharedString::from(format!("+{added}"))),
                        )
                        .child(
                            div()
                                .text_color(theme.danger)
                                .child(SharedString::from(format!("\u{2212}{removed}"))),
                        ),
                );
            }
            if any {
                row = row.child(chips);
            }
        }
        if let Some(ready) = &worker.ready {
            let text = ready.text.clone();
            row = row.child(
                div()
                    .pl(px(13.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.success)
                            .child(SharedString::from(text)),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("{prefix}-jump-{id}")))
                            .debug_selector({
                                let selector = format!("{prefix}-jump-{id}");
                                move || selector.clone()
                            })
                            .flex_none()
                            .px(px(6.0))
                            .rounded(px(4.0))
                            .border_1()
                            .border_color(theme.success.opacity(0.5))
                            .text_size(crate::typography::ui_rems(10.5))
                            .text_color(theme.success)
                            .cursor_pointer()
                            .hover(|el| el.bg(theme.success.opacity(0.12)))
                            .child("Jump")
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.open_item_tab(cx);
                            })),
                    ),
            );
        }
        row.into_any_element()
    }
}
