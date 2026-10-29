//! Client side of the "Review PR" launcher (`StartPrReview`,
//! docs/pr-review.md): request building with stable operation ids, the RPC
//! call, and the user-facing text for results and refusals. The surfaces
//! (⌘P page, the palette) go
//! through [`crate::shell::Shell::start_pr_review`].

use std::collections::HashMap;

use mast_proto::{
    ChatConfig, PrReference, PrReviewAction, PrReviewError, PrReviewErrorCode, StartPrReviewParams,
    StartPrReviewReply,
};
use mast_rpc::methods;

use crate::state::EngineHandle;

/// The operation id per logical request (PR + action + agent config):
/// reused for every retry and double-click of the same request, so the host
/// returns the same Chat instead of starting a second review.
#[derive(Debug, Default)]
pub(crate) struct PrReviewOperations {
    ids: HashMap<String, String>,
}

impl PrReviewOperations {
    pub(crate) fn params(
        &mut self,
        reference: &PrReference,
        action: PrReviewAction,
        config: Option<ChatConfig>,
    ) -> StartPrReviewParams {
        self.params_in_item(reference, action, config, None)
    }

    /// [`Self::params`] filed under a work item (`spaceId` + `workItemId`;
    /// PR review sync): the item is part of the request's identity, so the
    /// same PR reviewed from another item is another request.
    pub(crate) fn params_in_item(
        &mut self,
        reference: &PrReference,
        action: PrReviewAction,
        config: Option<ChatConfig>,
        item: Option<(String, String)>,
    ) -> StartPrReviewParams {
        let key = serde_json::json!([
            reference.owner.to_ascii_lowercase(),
            reference.name.to_ascii_lowercase(),
            reference.number,
            action,
            config,
            item,
        ])
        .to_string();
        let operation_id = self
            .ids
            .entry(key)
            .or_insert_with(|| format!("pr-review-{}", uuid::Uuid::new_v4()))
            .clone();
        let (space_id, work_item_id) = item.unzip();
        StartPrReviewParams {
            repo_owner: reference.owner.clone(),
            repo_name: reference.name.clone(),
            number: reference.number,
            action,
            space_id,
            work_item_id,
            config,
            fresh: false,
            operation_id,
            target_device_id: None,
        }
    }
}

/// Call `StartPrReview` (the engine routes it to the Space's owner).
pub(crate) async fn start(
    engine: EngineHandle,
    params: StartPrReviewParams,
) -> Result<StartPrReviewReply, PrReviewError> {
    let value = serde_json::to_value(&params)
        .map_err(|e| PrReviewError::new(PrReviewErrorCode::Invalid, e.to_string()))?;
    match engine.client().call(methods::START_PR_REVIEW, value).await {
        Ok(value) => serde_json::from_value(value).map_err(|e| {
            PrReviewError::new(
                PrReviewErrorCode::Invalid,
                format!("Unexpected reply from the engine: {e}"),
            )
        }),
        Err(error) => {
            let text = error.to_string();
            Err(PrReviewError::from_wire_message(&text).unwrap_or_else(|| {
                if text.contains("unknown method") {
                    PrReviewError::new(
                        PrReviewErrorCode::DeviceOffline,
                        "This Mast engine can't start PR reviews yet; restart Mast to update it",
                    )
                } else {
                    PrReviewError::new(PrReviewErrorCode::DeviceOffline, text)
                }
            }))
        }
    }
}

/// What the URL box says about unparseable input.
pub(crate) const REFERENCE_HINT: &str =
    "Enter a PR URL (https://github.com/owner/repo/pull/123) or owner/repo#123";

/// The prompt's opening in the Start Chat dialog: the slash command and the
/// PR link. The host appends the rest (reviewer or author wording, the
/// branch and where it is checked out) once it has checked the PR out.
pub(crate) fn prompt_preview(action: PrReviewAction, url: &str) -> String {
    let command = match action {
        PrReviewAction::CodeReview => "/code-review",
        PrReviewAction::FixCi => "/fix-ci",
    };
    format!("{command} {url}")
}

/// Where the host runs a PR chat, as the Start Chat dialog describes it.
pub(crate) const RUNS_IN: &str = "The PR's branch checkout: where it is already checked out, else the canonical checkout (on main, no changes), else a new <repo>-pr-<number> worktree.";

/// A refusal as one actionable sentence.
pub(crate) fn failure_text(error: &PrReviewError) -> String {
    let hint = match error.code {
        PrReviewErrorCode::Fork => {
            " Fork PRs aren't checked out automatically; check the branch out by hand and start a chat there."
        }
        PrReviewErrorCode::RepoNotFound => {
            " Add the folder that contains this repository (for Cofactr, the workspace superproject) as a project, then try again."
        }
        PrReviewErrorCode::GhUnavailable => {
            " Install the GitHub CLI and run `gh auth login` on the device that has the checkout."
        }
        PrReviewErrorCode::WorktreePathExists => "",
        PrReviewErrorCode::DeviceOffline => {
            " Make sure the device with the checkout is online and up to date."
        }
        _ => "",
    };
    let message = error.message.trim_end_matches('.');
    format!("{message}.{hint}")
}

/// The note shown after a start: the behind-origin warning first.
pub(crate) fn started_notice(reply: &StartPrReviewReply) -> Option<String> {
    (reply.behind_by > 0).then(|| {
        format!(
            "{}: the checkout at {} is {} commit(s) behind origin/{} — pull before you start",
            reply.title, reply.checkout_path, reply.behind_by, reply.branch
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference() -> PrReference {
        PrReference::parse("acme/app#12").unwrap()
    }

    #[test]
    fn the_same_request_keeps_its_operation_id_and_others_get_new_ones() {
        let mut ops = PrReviewOperations::default();
        let first = ops.params(&reference(), PrReviewAction::CodeReview, None);
        let again = ops.params(
            &PrReference::parse("https://github.com/ACME/app/pull/12").unwrap(),
            PrReviewAction::CodeReview,
            None,
        );
        assert_eq!(first.operation_id, again.operation_id);
        assert!(first.operation_id.starts_with("pr-review-"));
        let fix = ops.params(&reference(), PrReviewAction::FixCi, None);
        assert_ne!(fix.operation_id, first.operation_id);
        let other = ops.params(
            &PrReference::parse("acme/app#13").unwrap(),
            PrReviewAction::CodeReview,
            None,
        );
        assert_ne!(other.operation_id, first.operation_id);
        assert_eq!(
            (
                first.repo_owner.as_str(),
                first.repo_name.as_str(),
                first.number
            ),
            ("acme", "app", 12)
        );
    }

    #[test]
    fn refusals_say_what_to_do() {
        let fork = PrReviewError::new(PrReviewErrorCode::Fork, "PR 3 comes from a fork");
        assert!(failure_text(&fork).contains("check the branch out by hand"));
        let missing = PrReviewError::new(PrReviewErrorCode::RepoNotFound, "No Space holds it.");
        assert_eq!(
            failure_text(&missing),
            "No Space holds it. Add the folder that contains this repository (for Cofactr, the workspace superproject) as a project, then try again."
        );
    }
}
