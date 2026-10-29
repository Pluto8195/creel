//! Typed PR review sync RPCs on the shared [`WorkOutlineActions`] facade
//! (same transport, retry, and test fakes as the Work Outline and Linear).
//!
//! Failures map to [`GithubUiError`]: `github/<code>: <message>` host errors
//! keep their code; a host that predates the RPCs (`UnknownMethod`, or an
//! unknown `op`) or no engine connection reads as
//! [`GithubFailure::NotAvailable`], which every surface shows as a calm "not
//! available on this host" state.
use gpui::Task;
use mast_proto::{
    CreateWorkItemExternalReferenceParams, CreateWorkItemParams, GithubError, GithubErrorCode,
    PrMyOpenParams, PrReviewRequestedParams, PrReviewSyncNowParams, PrReviewSyncNowReply,
    PrStatusParams, PrViewerReply, PullRequestRef, PullRequestStatusSummary,
    SetPrReviewSyncDestinationParams, WorkOutlineError, WorkOutlineErrorCode,
    WorkOutlineMutateReply, WorkOutlineMutation,
};
use mast_rpc::methods;

use crate::work_outline::WorkOutlineActions;
use crate::work_outline::actions::is_transport_failure;

/// Why a GitHub call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GithubFailure {
    /// The host's coded GitHub error.
    Github(GithubErrorCode),
    /// The host predates the PR review sync (or no engine is connected).
    NotAvailable,
    /// Anything else (the message is shown as is).
    Other,
}

/// A failed GitHub call, ready for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GithubUiError {
    pub failure: GithubFailure,
    pub message: String,
}

impl GithubUiError {
    pub(crate) fn new(failure: GithubFailure, message: impl Into<String>) -> Self {
        Self {
            failure,
            message: message.into(),
        }
    }

    pub(crate) fn from_outline(error: WorkOutlineError) -> Self {
        if let Some(github) = GithubError::from_wire_message(&error.message) {
            return Self::new(GithubFailure::Github(github.code), github.message);
        }
        if is_transport_failure(&error) {
            return Self::new(
                GithubFailure::Github(GithubErrorCode::Network),
                error.message,
            );
        }
        let unknown_op = error.message.contains("unknown variant")
            || error.message.contains("unknown method")
            || error.message.contains("does not support");
        if error.code == WorkOutlineErrorCode::Unsupported || unknown_op {
            return Self::new(GithubFailure::NotAvailable, error.message);
        }
        Self::new(GithubFailure::Other, error.message)
    }

    pub(crate) fn code(&self) -> Option<GithubErrorCode> {
        match self.failure {
            GithubFailure::Github(code) => Some(code),
            _ => None,
        }
    }

    pub(crate) fn not_available(&self) -> bool {
        self.failure == GithubFailure::NotAvailable
    }

    /// Account-wide failures every list shares (no `gh`, signed out, no
    /// PR review sync on this host).
    pub(crate) fn is_blocking(&self) -> bool {
        self.not_available()
            || matches!(
                self.code(),
                Some(GithubErrorCode::GhMissing | GithubErrorCode::Unauthorized)
            )
    }

    /// One sentence for the user.
    pub(crate) fn user_text(&self) -> String {
        match self.failure {
            GithubFailure::NotAvailable => {
                "PR review sync isn\u{2019}t available on this host yet".to_string()
            }
            GithubFailure::Other => {
                if self.message.is_empty() {
                    "GitHub request failed".to_string()
                } else {
                    self.message.clone()
                }
            }
            GithubFailure::Github(code) => match code {
                GithubErrorCode::GhMissing => {
                    "Install the GitHub CLI (gh) on this device to use GitHub here".to_string()
                }
                GithubErrorCode::Unauthorized => {
                    "The GitHub CLI isn\u{2019}t signed in. Run `gh auth login`, then retry."
                        .to_string()
                }
                GithubErrorCode::RateLimited => {
                    "GitHub is rate-limiting requests. Wait a moment, then retry.".to_string()
                }
                GithubErrorCode::NotFound => {
                    "Not found on GitHub (or this account can\u{2019}t see it)".to_string()
                }
                GithubErrorCode::Network => {
                    if self.message.is_empty() {
                        "Couldn\u{2019}t reach GitHub".to_string()
                    } else {
                        format!("Couldn\u{2019}t reach GitHub: {}", self.message)
                    }
                }
                GithubErrorCode::Invalid => {
                    if self.message.is_empty() {
                        "GitHub rejected the request".to_string()
                    } else {
                        self.message.clone()
                    }
                }
            },
        }
    }
}

fn decode<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, WorkOutlineError> {
    serde_json::from_value(value).map_err(|error| {
        WorkOutlineError::new(
            WorkOutlineErrorCode::Invalid,
            format!("malformed GitHub reply: {error}"),
        )
    })
}

fn decode_mutate(value: serde_json::Value) -> Result<WorkOutlineMutateReply, WorkOutlineError> {
    if value.is_null() {
        return Ok(WorkOutlineMutateReply::default());
    }
    decode(value)
}

impl WorkOutlineActions {
    fn github_call<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        method: &'static str,
        params: impl serde::Serialize,
    ) -> GithubTask<T> {
        match serde_json::to_value(params) {
            Ok(params) => GithubTask::Pending(self.call(method, params, decode::<T>)),
            Err(error) => {
                GithubTask::failed(GithubUiError::new(GithubFailure::Other, error.to_string()))
            }
        }
    }

    fn github_mutate(&self, mutation: WorkOutlineMutation) -> GithubTask<WorkOutlineMutateReply> {
        match serde_json::to_value(&mutation) {
            Ok(params) => {
                GithubTask::Pending(self.call(methods::WORK_OUTLINE_MUTATE, params, decode_mutate))
            }
            Err(error) => {
                GithubTask::failed(GithubUiError::new(GithubFailure::Other, error.to_string()))
            }
        }
    }

    pub(crate) fn pr_status(
        &self,
        pr: &PullRequestRef,
        force_refresh: bool,
    ) -> GithubTask<PullRequestStatusSummary> {
        self.github_call(
            methods::PR_STATUS,
            PrStatusParams {
                owner: pr.owner.clone(),
                repo: pr.repo.clone(),
                number: pr.number,
                force_refresh,
            },
        )
    }

    /// `PrViewer`: the GitHub login `gh` is signed in as.
    pub(crate) fn pr_viewer(&self) -> GithubTask<PrViewerReply> {
        self.github_call(methods::PR_VIEWER, serde_json::json!({}))
    }

    pub(crate) fn pr_review_sync_now(&self, space_id: &str) -> GithubTask<PrReviewSyncNowReply> {
        self.github_call(
            methods::PR_REVIEW_SYNC_NOW,
            PrReviewSyncNowParams {
                space_id: space_id.to_string(),
            },
        )
    }

    pub(crate) fn pr_my_open(&self, space_id: &str) -> GithubTask<Vec<PullRequestStatusSummary>> {
        self.github_call(
            methods::PR_MY_OPEN,
            PrMyOpenParams {
                space_id: space_id.to_string(),
            },
        )
    }

    pub(crate) fn pr_review_requested(
        &self,
        space_id: &str,
    ) -> GithubTask<Vec<PullRequestStatusSummary>> {
        self.github_call(
            methods::PR_REVIEW_REQUESTED,
            PrReviewRequestedParams {
                space_id: space_id.to_string(),
            },
        )
    }

    pub(crate) fn set_pr_review_sync_destination(
        &self,
        params: SetPrReviewSyncDestinationParams,
    ) -> GithubTask<WorkOutlineMutateReply> {
        self.github_mutate(WorkOutlineMutation::SetPrReviewSyncDestination(params))
    }

    /// `createWorkItemExternalReference` (a `github` / `pullRequest` link).
    pub(crate) fn link_pull_request(
        &self,
        params: CreateWorkItemExternalReferenceParams,
    ) -> GithubTask<WorkOutlineMutateReply> {
        GithubTask::Pending(self.create_external_reference(params))
    }

    /// `createWorkItem` as a GitHub call (the manual "Create work item").
    pub(crate) fn create_pr_work_item(
        &self,
        params: CreateWorkItemParams,
    ) -> GithubTask<WorkOutlineMutateReply> {
        GithubTask::Pending(self.create_work_item(params))
    }
}

/// A facade task whose error reads as [`GithubUiError`]. Await it from a
/// `cx.spawn` like any gpui task.
pub(crate) enum GithubTask<T> {
    Pending(Task<Result<T, WorkOutlineError>>),
    Failed(Option<GithubUiError>),
}

impl<T> GithubTask<T> {
    fn failed(error: GithubUiError) -> Self {
        Self::Failed(Some(error))
    }
}

impl<T: 'static> std::future::Future for GithubTask<T> {
    type Output = Result<T, GithubUiError>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        // SAFETY: the inner task is never moved out of the pinned enum.
        let this = unsafe { self.get_unchecked_mut() };
        match this {
            Self::Pending(task) => unsafe { std::pin::Pin::new_unchecked(task) }
                .poll(cx)
                .map(|result| result.map_err(GithubUiError::from_outline)),
            Self::Failed(error) => std::task::Poll::Ready(Err(error
                .take()
                .unwrap_or_else(|| GithubUiError::new(GithubFailure::Other, "polled twice")))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_keep_their_github_code_and_unknown_methods_read_as_not_available() {
        let coded = GithubUiError::from_outline(WorkOutlineError::new(
            WorkOutlineErrorCode::Invalid,
            "github/unauthorized: gh is signed out",
        ));
        assert_eq!(coded.code(), Some(GithubErrorCode::Unauthorized));
        assert!(coded.is_blocking());
        assert!(coded.user_text().contains("gh auth login"));
        let missing = GithubUiError::from_outline(WorkOutlineError::new(
            WorkOutlineErrorCode::Unsupported,
            "This host does not support Work Outline (PrMyOpen)",
        ));
        assert!(missing.not_available());
        assert_eq!(
            missing.user_text(),
            "PR review sync isn\u{2019}t available on this host yet"
        );
        let other = GithubUiError::from_outline(WorkOutlineError::new(
            WorkOutlineErrorCode::Invalid,
            "boom",
        ));
        assert_eq!(other.failure, GithubFailure::Other);
        assert!(!other.is_blocking());
    }
}
