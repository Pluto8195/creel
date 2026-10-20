//! PR review sync in the Outline tree (Spaces where it applies only:
//! `github::space_available`): the live PR chip on items with a `github` /
//! `pullRequest` reference (click opens the PR per the web-link
//! preference), the destination marker on the Space's sync destination, and
//! the drop of a GitHub panel PR row onto a work item (same "into"
//! indicator as a Linear issue; creates a work item for the PR there).
//!
//! A child module of `outline_view` (like `linear_outline`) so it reads the
//! Outline's snapshots without widening that view's API.
use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};
use zeron_proto::PullRequestStatusSummary;

use super::super::chips::{self, TextTooltip};
use super::super::model::ItemRow;
use super::linear_outline::join_chips;
use super::{OutlineEvent, OutlineView};
use crate::github::{row_icons, store};
use crate::theme::Theme;

/// The drag payload of a GitHub panel PR row (`OutlineDrag::PullRequest`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrDrag {
    /// The panel's Space: only its items accept the drop.
    pub space_id: String,
    pub pr: Box<PullRequestStatusSummary>,
}

/// What an item row gets from the external integrations.
pub(super) struct ExternalRow {
    /// Linear chips, the destination marker and further PR chips.
    pub chips: Option<AnyElement>,
    /// The first PR's icon cluster; the row then drops its own attention
    /// indicator, totals text, chat count and references icon (the cluster
    /// carries them).
    pub pr_slots: Option<AnyElement>,
}

impl OutlineView {
    /// Whether the GitHub surfaces apply to `space_id` here.
    fn github_available(&self, space_id: &str, cx: &gpui::App) -> bool {
        self.snapshot(space_id).is_some_and(|snapshot| {
            crate::github::space_available(self.state.read(cx), space_id, snapshot)
        })
    }

    /// An invisible element that records the Outline's width while it paints
    /// (and repaints the rows when it changes), so the PR icon cluster can
    /// drop slots on a narrow Outline.
    pub(super) fn width_probe(&self, cx: &mut Context<Self>) -> AnyElement {
        let measured = self.row_width.clone();
        let view = cx.entity_id();
        gpui::canvas(
            move |bounds, _, cx| {
                let width = f32::from(bounds.size.width);
                if (measured.get() - width).abs() >= 1.0 {
                    measured.set(width);
                    cx.notify(view);
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
        .into_any_element()
    }

    /// The width the rows lay out in; unlimited until it is measured.
    fn outline_width(&self) -> f32 {
        let width = self.row_width_override.unwrap_or_else(|| self.row_width.get());
        if width > 0.0 { width } else { f32::INFINITY }
    }

    /// Pin the measured width (headless tests).
    #[cfg(test)]
    pub(crate) fn set_row_width_for_test(&mut self, width: Option<f32>, cx: &mut Context<Self>) {
        self.row_width_override = width;
        cx.notify();
    }

    /// The external chips of an item row: Linear's, the destination marker
    /// and any PR beyond the first as chips, and the first PR as the icon
    /// cluster (`pr_slots`). `depth` and `other_width` (an estimate of the
    /// row's other chips) decide which cluster slots fit.
    pub(super) fn external_chips(
        &self,
        key: &str,
        item: &ItemRow,
        depth: u16,
        other_width: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> ExternalRow {
        let linear = self.linear_chips(key, item, theme, cx);
        let linear_width = if linear.is_some() { 30.0 } else { 0.0 };
        let (pr_chips, pr_slots) =
            self.pr_sync_chips(key, item, depth, other_width + linear_width, theme, cx);
        ExternalRow {
            chips: join_chips(linear, pr_chips),
            pr_slots,
        }
    }

    /// `(destination marker + chips of further PRs, the first PR's cluster)`.
    fn pr_sync_chips(
        &self,
        key: &str,
        item: &ItemRow,
        depth: u16,
        other_width: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> (Option<AnyElement>, Option<AnyElement>) {
        if !self.github_available(&item.space_id, cx) {
            return (None, None);
        }
        let Some(snapshot) = self.snapshot(&item.space_id) else {
            return (None, None);
        };
        let destination = (snapshot.pr_review_sync_destination() == Some(item.item_id.as_str()))
            .then(|| destination_marker(format!("outline-pr-destination-{key}"), theme));
        let mut other_width = other_width + if destination.is_some() { 28.0 } else { 0.0 };
        let mut slots = None;
        let mut chips = None;
        for (index, pr) in crate::github::item_pull_requests(snapshot, &item.item_id)
            .into_iter()
            .enumerate()
        {
            store::request_status(&self.state, &pr, false, cx);
            let entry = self.state.read(cx).github.status(&pr).cloned();
            if index == 0 {
                // The first PR is the compact cluster; further PRs of a
                // ticket keep their chip (and take room from the slots).
                slots = Some((pr, entry));
                continue;
            }
            let id = format!("outline-pr-{key}-{index}");
            let chip =
                crate::github::chip::pr_chip(id, &pr, entry.as_ref(), self.shell.clone(), theme);
            other_width += 90.0;
            chips = join_chips(chips, Some(chip));
        }
        let slots = slots.map(|(pr, entry)| {
            let totals = (!item.archived)
                .then(|| super::super::required_actions::status_totals(&item.rollup))
                .flatten();
            let room = row_icons::slot_room(
                self.outline_width(),
                super::OutlineView::indent(depth),
                other_width,
            );
            row_icons::pr_row_slots(
                row_icons::PrRowArgs {
                    key,
                    pr: &pr,
                    entry: entry.as_ref(),
                    shell: self.shell.clone(),
                    // The viewer's GitHub login isn't exposed to the UI (only
                    // the engine's `gh api user` knows it), so the author
                    // always shows as the plain login for now.
                    viewer: None,
                    attention: chips::attention_indicator(
                        format!("outline-attention-{key}"),
                        item.needs_user_action(),
                        item.unresolved_actions,
                        theme,
                        1.0,
                    ),
                    chat_count: item.chat_count,
                    totals_line: totals
                        .map(|totals| format!("Including descendants: {}", totals.label())),
                    visible: row_icons::visible_slots(room),
                },
                theme,
            )
        });
        (join_chips(chips, destination), slots)
    }

    /// Whether dropping `drag` on item `item_id` of `space_id` creates a
    /// work item there: same Space, live target, nothing being edited.
    pub(super) fn accepts_pr_drop(&self, drag: &PrDrag, space_id: &str, item_id: &str) -> bool {
        if self.is_editing() || drag.space_id != space_id {
            return false;
        }
        self.snapshot(space_id).is_some_and(|snapshot| {
            snapshot
                .item(item_id)
                .is_some_and(|item| item.archived_at.is_none())
        })
    }

    /// Hand an accepted drop to the Overview (the GitHub panel creates it).
    pub(super) fn drop_pull_request(
        &mut self,
        drag: &PrDrag,
        target: (&str, &str),
        cx: &mut Context<Self>,
    ) {
        if !self.accepts_pr_drop(drag, target.0, target.1) {
            return;
        }
        cx.emit(OutlineEvent::PullRequestDropped {
            space_id: target.0.to_string(),
            item_id: target.1.to_string(),
            pr: drag.pr.clone(),
        });
    }
}

/// The small marker on the Space's PR review sync destination.
pub(crate) fn destination_marker(id: String, theme: &Theme) -> AnyElement {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(2.0))
        .px(px(3.0))
        .rounded(px(3.0))
        .text_size(crate::typography::ui_rems(10.0))
        .text_color(theme.accent)
        .child(
            crate::icons::icon(crate::icons::PULL_REQUEST)
                .size(px(10.0))
                .text_color(theme.accent),
        )
        .child("\u{2193}")
        .tooltip(|_, cx| {
            cx.new(|_| TextTooltip("PR review requests sync here (every 3 minutes)".into()))
                .into()
        })
        .into_any_element()
}
