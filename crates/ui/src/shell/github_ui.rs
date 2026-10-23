//! Shell-owned PR review sync surfaces: the GitHub entries of the Outline's
//! right-click menus (`crate::github::menu`) and the destination setting
//! they change.
use super::*;

use crate::github::menu::GithubMenuAction;

impl Shell {
    /// `setPrReviewSyncDestination` for `space_id` (`None` turns the sync
    /// off). A refusal shows in the sidebar notice.
    pub(crate) fn set_pr_review_destination(
        &mut self,
        space_id: String,
        item_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .set_pr_review_sync_destination(zeron_proto::SetPrReviewSyncDestinationParams {
                space_id,
                work_item_id: item_id,
                operation_id: crate::work_outline::actions::new_operation_id(),
            });
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                this.update(cx, |shell, cx| {
                    shell.sidebar_notice = Some(
                        format!(
                            "Couldn\u{2019}t change the PR review destination: {}",
                            error.user_text()
                        )
                        .into(),
                    );
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Open the Overview on Work Item Details of `item_id` (⇧⌘P's
    /// destination rows).
    pub(crate) fn open_work_item_in_overview(
        &mut self,
        space_id: String,
        item_id: String,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.route, Route::Overview) {
            self.open_overview(cx);
        }
        let page = self.ensure_overview_page(cx);
        page.update(cx, |overview, cx| {
            overview.open_work_item_details(space_id, item_id, cx)
        });
    }

    /// Run a GitHub entry of an Outline right-click menu.
    pub(super) fn run_github_menu_action(
        &mut self,
        action: GithubMenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            GithubMenuAction::SetDestination { space_id, item_id } => {
                self.set_pr_review_destination(space_id, item_id, cx)
            }
            GithubMenuAction::Review {
                space_id,
                item_id,
                action,
            } => {
                let page = self.ensure_overview_page(cx);
                let result = page.update(cx, |overview, cx| {
                    overview.start_pr_review_for_item(space_id, item_id, action, cx)
                });
                if let Err(notice) = result {
                    self.sidebar_notice = Some(notice.into());
                    cx.notify();
                }
            }
            GithubMenuAction::OpenOnGithub(url) => self.open_work_item_reference(&url, window, cx),
            GithubMenuAction::OpenOnGithubExternal(url) => {
                self.open_web_reference(&url, true, window, cx)
            }
            GithubMenuAction::Refresh(pr) => {
                crate::github::store::request_status(&self.state, &pr, true, cx)
            }
        }
    }
}
