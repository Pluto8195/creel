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

use super::super::chips::TextTooltip;
use super::super::model::ItemRow;
use super::linear_outline::join_chips;
use super::{OutlineEvent, OutlineView};
use crate::github::store;
use crate::theme::Theme;

/// The drag payload of a GitHub panel PR row (`OutlineDrag::PullRequest`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrDrag {
    /// The panel's Space: only its items accept the drop.
    pub space_id: String,
    pub pr: Box<PullRequestStatusSummary>,
}

impl OutlineView {
    /// Whether the GitHub surfaces apply to `space_id` here.
    fn github_available(&self, space_id: &str, cx: &gpui::App) -> bool {
        self.snapshot(space_id).is_some_and(|snapshot| {
            crate::github::space_available(self.state.read(cx), space_id, snapshot)
        })
    }

    /// Linear chips, then the PR chip and the destination marker.
    pub(super) fn external_chips(
        &self,
        key: &str,
        item: &ItemRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let linear = self.linear_chips(key, item, theme, cx);
        join_chips(linear, self.pr_sync_chips(key, item, theme, cx))
    }

    fn pr_sync_chips(
        &self,
        key: &str,
        item: &ItemRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.github_available(&item.space_id, cx) {
            return None;
        }
        let snapshot = self.snapshot(&item.space_id)?;
        let destination = (snapshot.pr_review_sync_destination() == Some(item.item_id.as_str()))
            .then(|| destination_marker(format!("outline-pr-destination-{key}"), theme));
        let chip = crate::github::item_pull_request(snapshot, &item.item_id).map(|pr| {
            store::request_status(&self.state, &pr, false, cx);
            let entry = self.state.read(cx).github.status(&pr).cloned();
            crate::github::chip::pr_chip(
                format!("outline-pr-{key}"),
                &pr,
                entry.as_ref(),
                self.shell.clone(),
                theme,
            )
        });
        join_chips(chip, destination)
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
