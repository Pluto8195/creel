//! ⇧⌘P with a PR review sync destination: when the current Space (the
//! composer's project) has one, the page lists the destination item's PR
//! items (live PR chip, Review / Fix CI filed under the item, Open item)
//! with their review Chats nested under each, instead of the flat list of
//! launcher review Chats (kept as the fallback while no destination is set).
use std::sync::Arc;

use chrono::{DateTime, Utc};
use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};
use zeron_proto::{Chat, PrReviewAction, PullRequestRef, WorkItemView, WorkOutlineSnapshot};

use super::{ChatTypeView, ReviewStatus, review_button, status_label, status_line};
use crate::state::format_time_ago;
use crate::theme::Theme;

/// One PR item of the destination, with its review Chats (newest first).
#[derive(Clone)]
pub(super) struct DestinationItem {
    pub item: WorkItemView,
    pub pr: PullRequestRef,
    pub chats: Vec<Chat>,
}

/// The current Space's destination list, when it has a live destination.
#[derive(Clone)]
pub(super) struct DestinationView {
    pub space_id: String,
    pub destination_title: String,
    pub items: Vec<DestinationItem>,
}

/// The destination view of `snapshot` with Chats from `chats`.
pub(super) fn destination_view(
    snapshot: &WorkOutlineSnapshot,
    chats: &[Chat],
    show_archived: bool,
) -> Option<DestinationView> {
    let destination = crate::github::live_destination(snapshot)?;
    let destination_title = snapshot.item(destination)?.title.clone();
    let items = crate::github::destination_pr_items(snapshot)
        .into_iter()
        .map(|(item, pr)| {
            let mut item_chats: Vec<Chat> = item
                .chat_ids
                .iter()
                .filter_map(|id| chats.iter().find(|chat| &chat.id == id))
                .filter(|chat| show_archived || !chat.archived)
                .cloned()
                .collect();
            item_chats.sort_by_key(|chat| {
                std::cmp::Reverse(chat.last_message_at.unwrap_or(chat.created_at))
            });
            DestinationItem {
                item,
                pr,
                chats: item_chats,
            }
        })
        .collect();
    Some(DestinationView {
        space_id: snapshot.space_id.clone(),
        destination_title,
        items,
    })
}

impl ChatTypeView {
    /// The current Space's destination list, if it has a destination and
    /// its Outline is loaded.
    pub(super) fn destination(&self, cx: &gpui::App) -> Option<DestinationView> {
        let state = self.state.read(cx);
        let space = state.selected_space_row()?;
        let snapshot: Arc<WorkOutlineSnapshot> = state.work_outline(&space.id).snapshot()?.clone();
        destination_view(&snapshot, &state.chats, self.show_archived)
    }

    /// Review / Fix CI of a destination item's PR, filed under the item.
    pub(super) fn start_item_review(
        &mut self,
        space_id: String,
        item_id: String,
        pr: PullRequestRef,
        action: PrReviewAction,
        cx: &mut Context<Self>,
    ) {
        let url = pr.url.clone();
        if matches!(
            self.review_status.get(&url),
            Some(ReviewStatus::Starting(_))
        ) {
            return;
        }
        let reference = crate::github::launch_reference(&pr);
        let Ok(task) = self.shell.update(cx, |shell, cx| {
            shell.start_pr_review_in_item(reference, action, Some((space_id, item_id)), cx)
        }) else {
            return;
        };
        self.review_status
            .insert(url.clone(), ReviewStatus::Starting(action));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(_) => {
                        this.review_status.remove(&url);
                    }
                    Err(error) => {
                        this.review_status.insert(
                            url,
                            ReviewStatus::Failed(crate::pr_review_launch::failure_text(&error)),
                        );
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// ↑/↓ move over the PR items; ↩ opens the item's newest review Chat,
    /// else its details.
    pub(super) fn destination_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        view: DestinationView,
        cx: &mut Context<Self>,
    ) {
        let count = view.items.len();
        match event.keystroke.key.as_str() {
            "down" if count > 0 => {
                self.active = super::move_selection(self.active, count, 1);
                self.scroll.scroll_to_item(self.active);
            }
            "up" if count > 0 => {
                self.active = super::move_selection(self.active, count, -1);
                self.scroll.scroll_to_item(self.active);
            }
            "enter" if count > 0 => {
                let entry = &view.items[self.active.min(count - 1)];
                match entry.chats.first() {
                    Some(chat) => self.open_chat(chat.id.clone(), cx),
                    None => {
                        self.open_destination_item(view.space_id.clone(), entry.item.id.clone(), cx)
                    }
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn open_destination_item(&self, space_id: String, item_id: String, cx: &mut Context<Self>) {
        let _ = self.shell.update(cx, |shell, cx| {
            shell.open_work_item_in_overview(space_id, item_id, cx)
        });
    }

    /// The destination list (replaces the launcher review chat table).
    pub(super) fn render_destination(
        &mut self,
        view: DestinationView,
        theme: &Theme,
        now: DateTime<Utc>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut list = div()
            .id("pr-destination-rows")
            .debug_selector(|| "pr-destination-rows".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px(px(14.0))
                    .py(px(8.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .bg(theme.element_hover.opacity(0.45))
                    .text_size(crate::typography::ui_rems(10.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text_muted)
                    .child(SharedString::from(format!(
                        "SYNCED INTO \u{201c}{}\u{201d} ({})",
                        view.destination_title.to_uppercase(),
                        view.items.len()
                    ))),
            );
        if view.items.is_empty() {
            list =
                list.child(div().p(px(14.0)).text_color(theme.text_muted).child(
                    "No PRs are synced yet. Requested reviews land here within a few minutes.",
                ));
        }
        for (index, entry) in view.items.into_iter().enumerate() {
            list = list.child(self.render_destination_item(
                &view.space_id,
                index,
                entry,
                theme,
                now,
                cx,
            ));
        }
        list.into_any_element()
    }

    fn render_destination_item(
        &mut self,
        space_id: &str,
        index: usize,
        entry: DestinationItem,
        theme: &Theme,
        now: DateTime<Utc>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let DestinationItem { item, pr, chats } = entry;
        let active = index == self.active;
        crate::github::store::request_status(&self.state, &pr, false, cx);
        let status = self.state.read(cx).github.status(&pr).cloned();
        let chip = crate::github::chip::pr_chip(
            format!("pr-destination-chip-{}", item.id),
            &pr,
            status.as_ref(),
            Some(self.shell.clone()),
            theme,
        );
        let review_status = self.review_status.get(&pr.url).cloned();
        let busy = matches!(review_status, Some(ReviewStatus::Starting(_)));
        let action = |label: &'static str, kind: &'static str, action: PrReviewAction, primary| {
            let space_id = space_id.to_string();
            let item_id = item.id.clone();
            let pr = pr.clone();
            review_button(
                format!("pr-destination-{kind}-{}", item.id).into(),
                label,
                primary,
                busy,
                theme,
            )
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.start_item_review(space_id.clone(), item_id.clone(), pr.clone(), action, cx);
            }))
        };
        let review = action("Review", "review", PrReviewAction::CodeReview, true);
        let fix_ci = action("Fix CI", "fix-ci", PrReviewAction::FixCi, false);
        let open_space = space_id.to_string();
        let open_item = item.id.clone();
        let (glyph, _, color) =
            crate::work_outline::chips::status_style(item.effective_status, theme);
        let row_selector = format!("pr-destination-row-{}", item.id);
        let mut block = div()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(theme.border.opacity(0.65))
            .when(active, |el| el.bg(theme.element_active))
            .child(
                div()
                    .id(SharedString::from(row_selector.clone()))
                    .debug_selector(move || row_selector.clone())
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(14.0))
                    .py(px(8.0))
                    .cursor_pointer()
                    .hover(|el| el.bg(theme.element_hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active = index;
                        this.open_destination_item(open_space.clone(), open_item.clone(), cx);
                    }))
                    .child(div().flex_none().w(px(12.0)).text_color(color).child(glyph))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(SharedString::from(item.title.clone())),
                            )
                            .child(div().flex().child(chip)),
                    )
                    .child(review)
                    .child(fix_ci),
            )
            .when_some(review_status, |el, status| {
                el.child(div().px(px(36.0)).pb(px(4.0)).child(status_line(
                    format!("pr-destination-status-{}", item.id),
                    &status,
                    theme,
                )))
            });
        for chat in chats {
            let chat_id = chat.id.clone();
            let selector = format!("pr-destination-chat-{}", chat.id);
            let state = self.state.read(cx);
            let indicator = state.display_status_for(&chat, now);
            let updated = chat.last_message_at.unwrap_or(chat.created_at);
            block = block.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .pl(px(40.0))
                    .pr(px(14.0))
                    .py(px(4.0))
                    .cursor_pointer()
                    .hover(|el| el.bg(theme.element_hover))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.open_chat(chat_id.clone(), cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(
                                chat.title.clone().unwrap_or_else(|| "New session".into()),
                            )),
                    )
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(status_label(indicator))),
                    )
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(format_time_ago(updated, now))),
                    ),
            );
        }
        block.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_outline::store::fixture_ids::*;

    #[test]
    fn the_destination_lists_its_pr_children_with_their_chats() {
        let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
        assert!(
            destination_view(&snapshot, &[], false).is_none(),
            "no destination"
        );
        snapshot.space_settings.pr_review_sync_work_item_id = Some(LAUNCH.into());
        // The fixture links acme/launch#42 on API (a child of Launch v2).
        let view = destination_view(&snapshot, &[], false).expect("destination");
        assert_eq!(view.destination_title, "Launch v2");
        let ids: Vec<_> = view
            .items
            .iter()
            .map(|entry| entry.item.id.as_str())
            .collect();
        assert_eq!(ids, [API]);
        assert_eq!(view.items[0].pr.number, 42);
    }
}
