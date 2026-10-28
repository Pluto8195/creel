//! The Overview's side of the PR review sync: starting a code review (or
//! Fix CI) of an item's PR filed under that item (⌘↩ on a PR review item,
//! the item menu, Details, the GitHub panel), and the GitHub section of the
//! left column (`crate::github::panel`): creating it beside the Linear
//! panel, its events (Reveal, Created, Review), and the Outline drop of a
//! panel PR.
use gpui::{AppContext as _, Context, Entity};
use mast_proto::{PrReviewAction, PullRequestRef, PullRequestStatusSummary};

use super::Overview;
use crate::github::panel::{GithubPanel, GithubPanelEvent, GithubSpace};
use crate::work_outline::model::row_key;

impl Overview {
    /// Every Space where the PR review sync applies, in Outline order, with
    /// its snapshot.
    pub(crate) fn github_spaces(&self, cx: &gpui::App) -> Vec<GithubSpace> {
        let state = self.state.read(cx);
        self.outline_spaces(cx)
            .into_iter()
            .filter_map(|space| {
                let snapshot = space.load.snapshot()?.clone();
                crate::github::space_available(state, &space.id, &snapshot).then(|| GithubSpace {
                    id: space.id,
                    label: space.label,
                    snapshot,
                })
            })
            .collect()
    }

    /// Start a review of item `item_id`'s PR (`action`), filed under that
    /// item: opens the Start Chat dialog (agent picker, review prompt);
    /// the Shell opens the new Chat once Start is confirmed there.
    pub(crate) fn start_pr_review_for_item(
        &mut self,
        space_id: String,
        item_id: String,
        action: PrReviewAction,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let snapshot = self
            .outline_snapshot(&space_id, cx)
            .ok_or("This project's outline is not loaded")?;
        let pr = crate::github::item_pull_request(&snapshot, &item_id)
            .ok_or("This work item isn\u{2019}t linked to a pull request")?;
        self.start_pr_review_filed(pr, Some((space_id, item_id)), action, cx)
    }

    /// Start Chat in PR mode for `pr`, filed under `item` when given.
    pub(crate) fn start_pr_review_filed(
        &mut self,
        pr: PullRequestRef,
        item: Option<(String, String)>,
        action: PrReviewAction,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        #[cfg(test)]
        if self.shell.upgrade().is_none() {
            self.captured_pr_review = Some((pr, item, action));
            return Ok(());
        }
        let shell = self
            .shell
            .upgrade()
            .ok_or("Starting a review needs the app window")?;
        let reference = crate::github::launch_reference(&pr);
        // Deferred: the menu and palette paths run inside a Shell update.
        // The Start Chat dialog opens; nothing starts until its Start.
        cx.defer(move |cx| {
            shell.update(cx, |shell, cx| {
                if let Err(notice) = shell.open_start_pr_review(reference, action, item, cx) {
                    shell.set_sidebar_error(notice);
                    cx.notify();
                }
            });
        });
        Ok(())
    }

    /// The GitHub section, created on `target` when there is none (the
    /// caller feeds and refreshes it).
    pub(super) fn ensure_github_panel(
        &mut self,
        target: String,
        cx: &mut Context<Self>,
    ) -> Entity<GithubPanel> {
        let panel = match self.github_panel.clone() {
            Some(panel) => panel,
            None => {
                let state = self.state.clone();
                let panel = cx.new(|cx| GithubPanel::new(state, target, cx));
                self.github_panel = Some(panel.clone());
                panel
            }
        };
        if self._github_panel_events.is_none() {
            self._github_panel_events =
                Some(cx.subscribe(&panel, |this: &mut Self, _, event, cx| {
                    this.on_github_panel_event(event.clone(), cx)
                }));
        }
        panel
    }

    /// Drop the GitHub section (the column closed, or no Space applies).
    pub(super) fn drop_github_panel(&mut self) {
        self.github_panel = None;
        self._github_panel_events = None;
    }

    #[cfg(test)]
    pub(crate) fn github_panel(&self) -> Option<Entity<GithubPanel>> {
        self.github_panel.clone()
    }

    fn on_github_panel_event(&mut self, event: GithubPanelEvent, cx: &mut Context<Self>) {
        match event {
            GithubPanelEvent::Close => self.close_linear_panel(true, cx),
            GithubPanelEvent::Reveal { space_id, item_id } => {
                let key = row_key::item(&space_id, &item_id);
                let shown = self
                    .outline
                    .update(cx, |outline, cx| outline.reveal(&key, cx));
                if !shown {
                    self.outline.update(cx, |outline, cx| {
                        outline.show_notice("That item isn\u{2019}t shown in the Outline", cx)
                    });
                }
            }
            GithubPanelEvent::Created {
                space_id,
                item_id,
                parent_id,
            } => {
                let key = row_key::item(&space_id, &item_id);
                let parent = parent_id.map(|parent| row_key::item(&space_id, &parent));
                self.outline.update(cx, |outline, cx| {
                    outline.reveal_when_shown(&key, parent.as_deref(), cx)
                });
            }
            GithubPanelEvent::Review { pr, item, action } => {
                if let Err(notice) = self.start_pr_review_filed(pr, item, action, cx) {
                    self.outline
                        .update(cx, |outline, cx| outline.show_notice(notice, cx));
                }
            }
        }
    }

    /// A panel PR dropped on an Outline item: create a work item for it
    /// there.
    pub(super) fn pull_request_dropped(
        &mut self,
        space_id: String,
        item_id: String,
        pr: PullRequestStatusSummary,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.github_panel.clone() else {
            return;
        };
        panel.update(cx, |panel, cx| {
            panel.create_under(space_id, Some(item_id), pr, cx)
        });
    }
}
