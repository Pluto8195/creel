//! PR review sync in Work Item Details.
//!
//! - Space-root variant, worded like the GitHub section's sync bar:
//!   "Auto-sync review requests into… [Choose item…]" while off, "Syncing
//!   into <item> [Change…] [Stop]" once set (an item picker over every live
//!   work item of the Space: `setPrReviewSyncDestination`; Stop clears
//!   it), and Sync now (`PrReviewSyncNow`) with the last result.
//! - Items of a Space where the sync applies that link a PR: the **Pull
//!   request** section (live chip, title, ↻ = `PrStatus` with
//!   `forceRefresh`, Review / Fix CI = `StartPrReview` filed under this
//!   item, Open on GitHub) and the sync's own record ("Synced from GitHub ·
//!   round 2"). The status refreshes (bypassing caches) when Details open.
//!
//! A child module of `details_panel` (like the Linear section), so it uses
//! the surface's private state directly.
use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};
use mast_proto::{
    PrReviewAction, PrReviewRequestState, SetPrReviewSyncDestinationParams, WorkItemView,
    WorkOutlineSnapshot,
};

use super::super::actions::OperationSlot;
use super::super::chips::TextTooltip;
use super::super::model;
use super::{DetailsEvent, WorkItemDetails};
use crate::github::store::{self, SyncResult};
use crate::linear::panel::LinearPanel;
use crate::theme::Theme;

/// Per-surface PR review sync state.
#[derive(Debug, Default)]
pub(crate) struct PrSyncDetailsState {
    /// The destination picker is open (Space-root variant).
    pub(crate) picker_open: bool,
    /// A `setPrReviewSyncDestination` is in flight.
    pub(crate) setting: bool,
    pub(crate) error: Option<String>,
    /// The item whose PR status was force-refreshed on open.
    refreshed_for: Option<String>,
    destination_op: OperationSlot,
}

fn request_state_label(state: PrReviewRequestState) -> &'static str {
    match state {
        PrReviewRequestState::Requested => "review requested",
        PrReviewRequestState::Submitted => "review submitted",
        PrReviewRequestState::Withdrawn => "request withdrawn",
        PrReviewRequestState::Merged => "merged",
        PrReviewRequestState::Closed => "closed",
    }
}

impl WorkItemDetails {
    /// `setPrReviewSyncDestination` (`None` turns the sync off).
    pub(crate) fn set_pr_review_destination(
        &mut self,
        item_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.pr_sync.setting {
            return;
        }
        let operation_id = self
            .pr_sync
            .destination_op
            .id_for(&format!("{}:{item_id:?}", self.space_id));
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .set_pr_review_sync_destination(SetPrReviewSyncDestinationParams {
                space_id: self.space_id.clone(),
                work_item_id: item_id,
                operation_id,
            });
        self.pr_sync.setting = true;
        self.pr_sync.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.pr_sync.setting = false;
                match result {
                    Ok(_) => {
                        this.pr_sync.destination_op = OperationSlot::default();
                        this.pr_sync.picker_open = false;
                    }
                    Err(error) => this.pr_sync.error = Some(error.user_text()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Sync now for this Space.
    pub(crate) fn pr_sync_now(&mut self, cx: &mut Context<Self>) {
        store::sync_now(&self.state, &self.space_id, cx);
    }

    /// The Space-root "PR review sync" section.
    pub(super) fn render_space_pr_sync(
        &mut self,
        snapshot: &WorkOutlineSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let destination = snapshot
            .pr_review_sync_destination()
            .map(|id| (id.to_string(), snapshot.item(id).cloned()));
        let target_label = match &destination {
            None => "no item chosen".to_string(),
            Some((_, Some(item))) if item.archived_at.is_none() => model::path_label(
                model::item_path(snapshot, &item.id)
                    .iter()
                    .map(|item| item.title.as_str()),
            ),
            Some((_, Some(_))) => "An archived work item (sync paused)".to_string(),
            Some((_, None)) => "A deleted work item (sync paused)".to_string(),
        };
        let set = destination.is_some();
        let picker_open = self.pr_sync.picker_open;
        let busy = self.pr_sync.setting;
        let mut section = div()
            .debug_selector(|| "space-pr-sync".into())
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text_muted)
                    .child("PR review sync"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex_none()
                            .text_size(crate::typography::ui_rems(11.5))
                            .text_color(theme.text_muted)
                            .child(if set {
                                "Syncing into"
                            } else {
                                "Auto-sync review requests into\u{2026}"
                            }),
                    )
                    .child(
                        div()
                            .debug_selector(|| "space-pr-sync-destination".into())
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(11.5))
                            .text_color(if set { theme.text } else { theme.text_faint })
                            .child(SharedString::from(target_label)),
                    )
                    .child(
                        LinearPanel::small_button(
                            "space-pr-sync-choose",
                            if busy {
                                "Saving\u{2026}"
                            } else if set {
                                "Change\u{2026}"
                            } else {
                                "Choose item\u{2026}"
                            },
                            theme,
                        )
                        .debug_selector(|| "space-pr-sync-choose".into())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.pr_sync.picker_open = !this.pr_sync.picker_open;
                            cx.notify();
                        })),
                    )
                    .when(set, |el| {
                        el.child(
                            LinearPanel::small_button("space-pr-sync-clear", "Stop", theme)
                                .debug_selector(|| "space-pr-sync-clear".into())
                                .tooltip(|_, cx| {
                                    cx.new(|_| {
                                        TextTooltip(
                                            "Stop syncing. Synced items stay where they are."
                                                .into(),
                                        )
                                    })
                                    .into()
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.set_pr_review_destination(None, cx)
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_faint)
                    .child(if set {
                        "Every open PR that requests your review in this project\u{2019}s \
                         repositories gets an item here, with a Required Action per review \
                         round. Kept in step every 3 minutes; nothing is written to GitHub."
                    } else {
                        "Off: choose a work item and every PR that requests your review gets \
                         an item there automatically."
                    }),
            );
        if picker_open {
            section = section.child(self.render_destination_picker(snapshot, theme, cx));
        }
        if set {
            let result = self
                .state
                .read(cx)
                .github
                .sync_results
                .get(&self.space_id)
                .cloned();
            let running = matches!(result, Some(SyncResult::Running));
            section = section.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        LinearPanel::small_button(
                            "space-pr-sync-now",
                            if running {
                                "Syncing\u{2026}"
                            } else {
                                "\u{21bb} Sync now"
                            },
                            theme,
                        )
                        .debug_selector(|| "space-pr-sync-now".into())
                        .on_click(cx.listener(|this, _, _, cx| this.pr_sync_now(cx))),
                    )
                    .when_some(result.filter(|_| !running), |el, result| {
                        let failed = matches!(result, SyncResult::Failed(_));
                        el.child(
                            div()
                                .debug_selector(|| "space-pr-sync-result".into())
                                .flex_1()
                                .min_w_0()
                                .text_size(crate::typography::ui_rems(10.5))
                                .text_color(if failed {
                                    theme.warning
                                } else {
                                    theme.text_faint
                                })
                                .child(SharedString::from(result.line())),
                        )
                    }),
            );
        }
        if let Some(error) = self.pr_sync.error.clone() {
            section = section.child(
                div()
                    .debug_selector(|| "space-pr-sync-error".into())
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.warning)
                    .child(SharedString::from(error)),
            );
        }
        section.into_any_element()
    }

    /// Every live item of the Space (path labels, Outline order); click
    /// picks it.
    fn render_destination_picker(
        &self,
        snapshot: &WorkOutlineSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = snapshot.pr_review_sync_destination().map(str::to_owned);
        let rows = snapshot
            .items
            .iter()
            .filter(|item| item.archived_at.is_none())
            .map(|item| {
                let label = model::path_label(
                    model::item_path(snapshot, &item.id)
                        .iter()
                        .map(|item| item.title.as_str()),
                );
                let id = item.id.clone();
                let checked = current.as_deref() == Some(id.as_str());
                let selector = format!("space-pr-sync-pick-{id}");
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .h(px(22.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(4.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(if checked {
                        theme.text
                    } else {
                        theme.text_muted
                    })
                    .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
                    .child(
                        div()
                            .flex_none()
                            .w(px(10.0))
                            .text_color(theme.accent)
                            .child(if checked { "\u{2713}" } else { "" }),
                    )
                    .child(div().min_w_0().truncate().child(SharedString::from(label)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_pr_review_destination(Some(id.clone()), cx)
                    }))
            })
            .collect::<Vec<_>>();
        div()
            .id("space-pr-sync-picker")
            .debug_selector(|| "space-pr-sync-picker".into())
            .max_h(px(220.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .pl(px(8.0))
            .when(rows.is_empty(), |el| {
                el.child(
                    div()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_faint)
                        .child("No work items in this project yet"),
                )
            })
            .children(rows)
            .into_any_element()
    }

    /// The item's Pull request section (linked items of Spaces where the
    /// sync applies only).
    pub(super) fn render_pr_section(
        &mut self,
        item: &WorkItemView,
        snapshot: &WorkOutlineSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !crate::github::space_available(self.state.read(cx), &self.space_id, snapshot) {
            return None;
        }
        let pr = crate::github::item_pull_request(snapshot, &item.id)?;
        // Refresh (bypassing caches) once per open of this item.
        let fresh_open = self.pr_sync.refreshed_for.as_deref() != Some(item.id.as_str());
        if fresh_open {
            self.pr_sync.refreshed_for = Some(item.id.clone());
        }
        store::request_status(&self.state, &pr, fresh_open, cx);
        let entry = self.state.read(cx).github.status(&pr).cloned();
        let title = entry
            .as_ref()
            .and_then(|entry| entry.ready())
            .map(|status| status.title.clone())
            .or_else(|| {
                snapshot
                    .github_pull_request_reference(&item.id)
                    .and_then(|reference| reference.cached_title.clone())
            })
            .unwrap_or_else(|| pr.external_id());
        let chip = crate::github::chip::pr_chip(
            "work-item-pr-chip".into(),
            &pr,
            entry.as_ref(),
            Some(self.shell.clone()),
            theme,
        );
        // The item's other PRs (e.g. a ticket's UI and Flagship PRs).
        let others: Vec<AnyElement> = crate::github::item_pull_requests(snapshot, &item.id)
            .into_iter()
            .skip(1)
            .enumerate()
            .map(|(index, other)| {
                store::request_status(&self.state, &other, fresh_open, cx);
                let entry = self.state.read(cx).github.status(&other).cloned();
                crate::github::chip::pr_chip(
                    format!("work-item-pr-chip-{}", index + 1),
                    &other,
                    entry.as_ref(),
                    Some(self.shell.clone()),
                    theme,
                )
            })
            .collect();
        let loading = self.state.read(cx).github.is_loading(&pr);
        let synced = item
            .external_sync_state
            .as_ref()
            .filter(|state| state.provider == mast_proto::GITHUB_PROVIDER)
            .map(|state| {
                format!(
                    "Synced from GitHub \u{00b7} round {} \u{00b7} {}",
                    state.round.max(1),
                    request_state_label(state.request_state)
                )
            });
        let review = |id: &'static str, label: &'static str, action: PrReviewAction| {
            let space_id = self.space_id.clone();
            let item_id = item.id.clone();
            LinearPanel::small_button(id, label, theme)
                .debug_selector(move || id.into())
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(DetailsEvent::StartPrReview {
                        space_id: space_id.clone(),
                        item_id: item_id.clone(),
                        action,
                    })
                }))
        };
        let review_button = review("work-item-pr-review", "Review", PrReviewAction::CodeReview);
        let fix_button = review("work-item-pr-fix-ci", "Fix CI", PrReviewAction::FixCi);
        let refresh_pr = pr.clone();
        let open_url = pr.url.clone();
        let shell = self.shell.clone();
        let section = div()
            .debug_selector(|| "work-item-pr".into())
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex_1()
                            .text_size(crate::typography::ui_rems(11.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text_muted)
                            .child("Pull request"),
                    )
                    .child(
                        LinearPanel::small_button(
                            "work-item-pr-refresh",
                            if loading {
                                "Refreshing\u{2026}"
                            } else {
                                "\u{21bb} Refresh"
                            },
                            theme,
                        )
                        .debug_selector(|| "work-item-pr-refresh".into())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            store::request_status(&this.state, &refresh_pr, true, cx)
                        })),
                    ),
            )
            .child(
                div().flex().items_center().gap(px(6.0)).child(chip).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(crate::typography::ui_rems(11.5))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(title)),
                ),
            )
            .when(!others.is_empty(), |el| {
                el.child(
                    div()
                        .debug_selector(|| "work-item-pr-others".into())
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(6.0))
                        .children(others),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(review_button)
                    .child(fix_button)
                    .child(
                        LinearPanel::small_button("work-item-pr-open", "Open on GitHub", theme)
                            .debug_selector(|| "work-item-pr-open".into())
                            .on_click(crate::github::chip::open_pr(Some(shell), open_url)),
                    ),
            )
            .when_some(synced, |el, line| {
                el.child(
                    div()
                        .debug_selector(|| "work-item-pr-synced".into())
                        .text_size(crate::typography::ui_rems(10.5))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(line)),
                )
            });
        Some(section.into_any_element())
    }
}
