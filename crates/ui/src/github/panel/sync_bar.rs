//! The GitHub section's sync bar, the primary flow: PR review requests
//! sync into ONE work item per project (`setPrReviewSyncDestination`; the
//! host then keeps a child per PR, every 3 minutes and right after the
//! destination is set).
//!
//! - Off: "Auto-sync review requests into… [Choose item ▾]" (the
//!   Destination menu: search, Create "PR reviews" item, every live item).
//! - On: "Syncing into <item> · last synced <age> · Sync now · Change ▾ ·
//!   Stop".
//! - The destination archived or deleted: "Sync paused" with Change ▾ and
//!   Stop.
use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};
use mast_proto::{
    CreateWorkItemParams, GITHUB_PROVIDER, SetPrReviewSyncDestinationParams, WorkOutlinePlacement,
    WorkOutlineSnapshot,
};

use super::menus::{DESTINATION_TITLE, PanelMenu};
use super::{GithubPanel, GithubPanelEvent};
use crate::github::store::{self, SyncResult};
use crate::linear::panel::LinearPanel;
use crate::theme::Theme;
use crate::work_outline::actions::OperationSlot;
use crate::work_outline::chips::TextTooltip;

/// The newest `syncedAt` (epoch ms) the sync wrote on the destination's
/// children.
pub(crate) fn last_synced_at(snapshot: &WorkOutlineSnapshot, destination: &str) -> Option<i64> {
    snapshot
        .items
        .iter()
        .filter(|item| item.parent_work_item_id.as_deref() == Some(destination))
        .filter_map(|item| item.external_sync_state.as_ref())
        .filter(|state| state.provider == GITHUB_PROVIDER && state.synced_at > 0)
        .map(|state| state.synced_at)
        .max()
}

/// "last synced 5m ago" / "last synced just now" / "not synced yet".
pub(crate) fn last_synced_text(at: Option<i64>, now: chrono::DateTime<chrono::Utc>) -> String {
    match at {
        None => "not synced yet".to_string(),
        Some(at) => match crate::work_outline::required_actions::age_label(at, now).as_str() {
            "now" => "last synced just now".to_string(),
            age => format!("last synced {age} ago"),
        },
    }
}

impl GithubPanel {
    /// `setPrReviewSyncDestination` for the panel's project (`None` stops
    /// the sync). The host runs a first pass as soon as it is set.
    pub(crate) fn set_destination(&mut self, item_id: Option<String>, cx: &mut Context<Self>) {
        if self.destination_busy {
            return;
        }
        let space_id = self.space_id.clone();
        let operation_id = self
            .destination_op
            .id_for(&format!("{space_id}:{item_id:?}"));
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .set_pr_review_sync_destination(SetPrReviewSyncDestinationParams {
                space_id,
                work_item_id: item_id.clone(),
                operation_id,
            });
        self.destination_busy = true;
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.destination_busy = false;
                match result {
                    Ok(_) => {
                        this.destination_op = OperationSlot::default();
                        if item_id.is_none() {
                            this.set_notice(
                                "Stopped syncing PR reviews. Synced items stay where they are.",
                                false,
                            );
                        }
                    }
                    Err(error) => this.set_notice(
                        format!(
                            "Couldn\u{2019}t change the PR review destination: {}",
                            error.user_text()
                        ),
                        true,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Create a top-level "PR reviews" item, then make it the destination.
    pub(crate) fn create_destination(&mut self, cx: &mut Context<Self>) {
        if self.destination_busy {
            return;
        }
        let space_id = self.space_id.clone();
        let operation_id = self.create_destination_op.id_for(&space_id);
        let actions = self.state.read(cx).work_outline_actions();
        let create = actions.create_pr_work_item(CreateWorkItemParams {
            space_id: space_id.clone(),
            parent_work_item_id: None,
            placement: WorkOutlinePlacement::Last,
            title: DESTINATION_TITLE.to_string(),
            description: None,
            status: None,
            operation_id: operation_id.clone(),
        });
        self.destination_busy = true;
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = match create.await {
                Ok(reply) => {
                    let item_id = reply.work_item_id.unwrap_or_else(|| {
                        mast_proto::work_outline::derive_id(&operation_id, "item")
                    });
                    let set =
                        actions.set_pr_review_sync_destination(SetPrReviewSyncDestinationParams {
                            space_id: space_id.clone(),
                            work_item_id: Some(item_id.clone()),
                            operation_id: mast_proto::work_outline::derive_id(
                                &operation_id,
                                "pr-review-destination",
                            ),
                        });
                    set.await.map(|_| item_id)
                }
                Err(error) => Err(error),
            };
            this.update(cx, |this, cx| {
                this.destination_busy = false;
                match result {
                    Ok(item_id) => {
                        this.create_destination_op = OperationSlot::default();
                        cx.emit(GithubPanelEvent::Created {
                            space_id: space_id.clone(),
                            item_id,
                            parent_id: None,
                        });
                    }
                    Err(error) => this.set_notice(
                        format!(
                            "Couldn\u{2019}t create the \u{201c}{DESTINATION_TITLE}\u{201d} item: {}",
                            error.user_text()
                        ),
                        true,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn sync_now(&mut self, cx: &mut Context<Self>) {
        store::sync_now(&self.state, &self.space_id, cx);
    }

    /// A Choose item ▾ / Change ▾ trigger with its menu.
    fn destination_trigger(
        &self,
        id: &'static str,
        label: &str,
        primary: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = if self.destination_busy {
            "Saving\u{2026}".to_string()
        } else {
            format!("{label} \u{25be}")
        };
        let button = LinearPanel::small_button(id, label, theme)
            .debug_selector(move || id.into())
            .when(primary, |el| {
                el.border_color(theme.accent)
                    .text_color(theme.accent)
                    .font_weight(gpui::FontWeight::MEDIUM)
            })
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, _| this.note_menu_press(&PanelMenu::Destination)),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.toggle_menu(PanelMenu::Destination, window, cx)
            }));
        div()
            .relative()
            .flex_none()
            .child(button)
            .children(self.render_menu_for(&PanelMenu::Destination, theme, cx))
            .into_any_element()
    }

    /// The sync bar under the section header.
    pub(crate) fn render_sync_bar(
        &self,
        snapshot: &WorkOutlineSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = |text: SharedString, color| {
            div()
                .flex_none()
                .text_size(crate::typography::ui_rems(11.5))
                .text_color(color)
                .child(text)
        };
        let bar = div()
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.0))
            .mx(px(14.0))
            .mt(px(6.0))
            .px(px(8.0))
            .py(px(5.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .child(
                crate::icons::icon(crate::icons::PULL_REQUEST)
                    .size(px(12.0))
                    .text_color(theme.accent),
            );
        let destination = snapshot
            .pr_review_sync_destination()
            .map(|id| (id.to_string(), snapshot.item(id)));
        let bar = match destination {
            None => bar
                .debug_selector(|| "github-sync-cta".into())
                .child(text(
                    "Auto-sync review requests into\u{2026}".into(),
                    theme.text,
                ))
                .child(self.destination_trigger(
                    "github-sync-choose",
                    "Choose item",
                    true,
                    theme,
                    cx,
                )),
            Some((_, Some(item))) if item.archived_at.is_none() => {
                let result = self
                    .state
                    .read(cx)
                    .github
                    .sync_results
                    .get(&self.space_id)
                    .cloned();
                let running = matches!(result, Some(SyncResult::Running));
                let status = match &result {
                    Some(SyncResult::Running) => ("syncing\u{2026}".to_string(), theme.text_faint),
                    Some(SyncResult::Failed(error)) => (error.user_text(), theme.warning),
                    _ => (
                        last_synced_text(last_synced_at(snapshot, &item.id), chrono::Utc::now()),
                        theme.text_faint,
                    ),
                };
                let item_id = item.id.clone();
                let space_id = self.space_id.clone();
                bar.debug_selector(|| "github-sync-status".into())
                    .child(text("Syncing into".into(), theme.text_muted))
                    .child(
                        div()
                            .id("github-sync-destination")
                            .debug_selector(|| "github-sync-destination".into())
                            .min_w(px(40.0))
                            .max_w(px(180.0))
                            .truncate()
                            .cursor_pointer()
                            .text_size(crate::typography::ui_rems(11.5))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .hover(|el| el.text_color(theme.accent))
                            .child(SharedString::from(item.title.clone()))
                            .tooltip(|_, cx| {
                                cx.new(|_| TextTooltip("Reveal it in the Outline".into()))
                                    .into()
                            })
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(GithubPanelEvent::Reveal {
                                    space_id: space_id.clone(),
                                    item_id: item_id.clone(),
                                })
                            })),
                    )
                    .child(
                        div()
                            .debug_selector(|| "github-sync-last".into())
                            .flex_none()
                            .text_size(crate::typography::ui_rems(10.5))
                            .text_color(status.1)
                            .child(SharedString::from(format!("\u{00b7} {}", status.0))),
                    )
                    .child(div().flex_1())
                    .child(
                        LinearPanel::small_button(
                            "github-sync-now",
                            if running {
                                "Syncing\u{2026}"
                            } else {
                                "Sync now"
                            },
                            theme,
                        )
                        .debug_selector(|| "github-sync-now".into())
                        .on_click(cx.listener(|this, _, _, cx| this.sync_now(cx))),
                    )
                    .child(self.destination_trigger(
                        "github-sync-change",
                        "Change",
                        false,
                        theme,
                        cx,
                    ))
                    .child(self.stop_button(theme, cx))
            }
            Some((_, item)) => bar
                .debug_selector(|| "github-sync-paused".into())
                .child(text(
                    if item.is_some() {
                        "Sync paused: the destination is archived".into()
                    } else {
                        "Sync paused: the destination was deleted".into()
                    },
                    theme.warning,
                ))
                .child(div().flex_1())
                .child(self.destination_trigger("github-sync-change", "Change", true, theme, cx))
                .child(self.stop_button(theme, cx)),
        };
        bar.into_any_element()
    }

    fn stop_button(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        LinearPanel::small_button("github-sync-stop", "Stop", theme)
            .debug_selector(|| "github-sync-stop".into())
            .tooltip(|_, cx| {
                cx.new(|_| TextTooltip("Stop syncing. Synced items stay where they are.".into()))
                    .into()
            })
            .on_click(cx.listener(|this, _, _, cx| this.set_destination(None, cx)))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_synced_reads_naturally() {
        let now = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(10 * 60_000).unwrap();
        assert_eq!(last_synced_text(None, now), "not synced yet");
        assert_eq!(
            last_synced_text(Some(10 * 60_000 - 5_000), now),
            "last synced just now"
        );
        assert_eq!(
            last_synced_text(Some(5 * 60_000), now),
            "last synced 5m ago"
        );
    }
}
