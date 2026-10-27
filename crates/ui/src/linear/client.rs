//! Typed Linear RPCs and Linear Work Outline operations on the shared
//! [`WorkOutlineActions`] facade (same transport, retry, and test fakes).
//!
//! Failures map to [`LinearUiError`]: `linear/<code>: <message>` host errors
//! keep their code; a host that predates the Linear RPCs (`UnknownMethod`,
//! or an unknown `op`) or no engine connection reads as
//! [`LinearFailure::NotAvailable`], which the UI shows as a calm "not
//! available on this host" state. The API key is only ever a parameter of
//! [`WorkOutlineActions::linear_set_api_key`] and is never logged.
use gpui::Task;
use zeron_proto::linear::{
    LinearClearApiKeyParams, LinearConnectionStatus, LinearError, LinearErrorCode,
    LinearIssueParams, LinearIssueSummary, LinearMyIssuesParams, LinearMyProjectsParams,
    LinearProjectIssuesParams, LinearProjectSummary, LinearRefreshLinkedItemsParams,
    LinearRefreshLinkedItemsReply, LinearSetApiKeyParams, LinearStatusParams,
};
use zeron_proto::{
    CreateWorkItemFromLinearParams, LinkLinearProjectParams, SetSpaceLinearEnabledParams,
    WorkOutlineError, WorkOutlineErrorCode, WorkOutlineMutateReply, WorkOutlineMutation,
};
use zeron_rpc::methods;

use crate::work_outline::WorkOutlineActions;
use crate::work_outline::actions::is_transport_failure;

/// Why a Linear call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinearFailure {
    /// The host's coded Linear error.
    Linear(LinearErrorCode),
    /// The host predates Linear (or no engine is connected).
    NotAvailable,
    /// Anything else (the message is shown as is).
    Other,
}

/// A failed Linear call, ready for display. Never carries the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinearUiError {
    pub failure: LinearFailure,
    pub message: String,
}

impl LinearUiError {
    pub(crate) fn new(failure: LinearFailure, message: impl Into<String>) -> Self {
        Self {
            failure,
            message: message.into(),
        }
    }

    pub(crate) fn from_outline(error: WorkOutlineError) -> Self {
        if let Some(linear) = LinearError::from_wire_message(&error.message) {
            return Self::new(LinearFailure::Linear(linear.code), linear.message);
        }
        if is_transport_failure(&error) {
            return Self::new(
                LinearFailure::Linear(LinearErrorCode::Network),
                error.message,
            );
        }
        let unknown_op = error.message.contains("unknown variant")
            || error.message.contains("unknown method")
            || error.message.contains("does not support");
        if error.code == WorkOutlineErrorCode::Unsupported || unknown_op {
            return Self::new(LinearFailure::NotAvailable, error.message);
        }
        Self::new(LinearFailure::Other, error.message)
    }

    pub(crate) fn code(&self) -> Option<LinearErrorCode> {
        match self.failure {
            LinearFailure::Linear(code) => Some(code),
            _ => None,
        }
    }

    pub(crate) fn is(&self, code: LinearErrorCode) -> bool {
        self.code() == Some(code)
    }

    pub(crate) fn not_available(&self) -> bool {
        self.failure == LinearFailure::NotAvailable
    }

    /// One sentence for the user.
    pub(crate) fn user_text(&self) -> String {
        match self.failure {
            LinearFailure::NotAvailable => {
                "Linear isn\u{2019}t available on this host yet".to_string()
            }
            LinearFailure::Other => {
                if self.message.is_empty() {
                    "Linear request failed".to_string()
                } else {
                    self.message.clone()
                }
            }
            LinearFailure::Linear(code) => match code {
                LinearErrorCode::MissingKey => "Connect Linear to use it here".to_string(),
                LinearErrorCode::Unauthorized => {
                    "Linear rejected the API key. Reconnect in Settings \u{2192} Linear."
                        .to_string()
                }
                LinearErrorCode::RateLimited => {
                    "Linear is rate-limiting requests. Wait a moment, then retry.".to_string()
                }
                LinearErrorCode::NotFound => {
                    "Not found in Linear (or this key can\u{2019}t see it)".to_string()
                }
                LinearErrorCode::Network => {
                    if self.message.is_empty() {
                        "Couldn\u{2019}t reach Linear".to_string()
                    } else {
                        format!("Couldn\u{2019}t reach Linear: {}", self.message)
                    }
                }
                LinearErrorCode::DisabledForSpace => {
                    "Linear is turned off for this project".to_string()
                }
                LinearErrorCode::Invalid => {
                    if self.message.is_empty() {
                        "Linear rejected the request".to_string()
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
            format!("malformed Linear reply: {error}"),
        )
    })
}

fn decode_mutate(value: serde_json::Value) -> Result<WorkOutlineMutateReply, WorkOutlineError> {
    if value.is_null() {
        return Ok(WorkOutlineMutateReply::default());
    }
    decode(value)
}

/// How a list read treats the engine's list cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListRead {
    /// Served from the cache while it is fresh, else fetched.
    Cached,
    /// Always fetched (the Refresh button).
    Force,
    /// Whatever is stored, however old; an error when nothing is. Never
    /// asks Linear.
    Stored,
}

impl ListRead {
    fn force(self) -> bool {
        self == Self::Force
    }

    fn stored_only(self) -> bool {
        self == Self::Stored
    }
}

impl WorkOutlineActions {
    fn linear_call<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        method: &'static str,
        params: impl serde::Serialize,
    ) -> LinearTask<T> {
        let params = match serde_json::to_value(params) {
            Ok(params) => params,
            Err(error) => {
                return LinearTask::failed(LinearUiError::new(
                    LinearFailure::Other,
                    error.to_string(),
                ));
            }
        };
        let task = self.call(method, params, decode::<T>);
        map_task(task)
    }

    fn linear_mutate(&self, mutation: WorkOutlineMutation) -> LinearTask<WorkOutlineMutateReply> {
        let params = match serde_json::to_value(&mutation) {
            Ok(params) => params,
            Err(error) => {
                return LinearTask::failed(LinearUiError::new(
                    LinearFailure::Other,
                    error.to_string(),
                ));
            }
        };
        map_task(self.call(methods::WORK_OUTLINE_MUTATE, params, decode_mutate))
    }

    pub(crate) fn linear_status(&self, force_refresh: bool) -> LinearTask<LinearConnectionStatus> {
        self.linear_call(methods::LINEAR_STATUS, LinearStatusParams { force_refresh })
    }

    /// Validate and store `api_key` on this device. The key goes nowhere else.
    pub(crate) fn linear_set_api_key(&self, api_key: String) -> LinearTask<LinearConnectionStatus> {
        self.linear_call(
            methods::LINEAR_SET_API_KEY,
            LinearSetApiKeyParams { api_key },
        )
    }

    pub(crate) fn linear_clear_api_key(&self) -> LinearTask<LinearConnectionStatus> {
        self.linear_call(methods::LINEAR_CLEAR_API_KEY, LinearClearApiKeyParams {})
    }

    pub(crate) fn linear_issue(
        &self,
        identifier: &str,
        force_refresh: bool,
    ) -> LinearTask<LinearIssueSummary> {
        self.linear_call(
            methods::LINEAR_ISSUE,
            LinearIssueParams {
                identifier: identifier.to_string(),
                force_refresh,
            },
        )
    }

    pub(crate) fn linear_my_issues(&self, read: ListRead) -> LinearTask<Vec<LinearIssueSummary>> {
        self.linear_call(
            methods::LINEAR_MY_ISSUES,
            LinearMyIssuesParams {
                force: read.force(),
                cached_only: read.stored_only(),
            },
        )
    }

    pub(crate) fn linear_my_projects(
        &self,
        read: ListRead,
    ) -> LinearTask<Vec<LinearProjectSummary>> {
        self.linear_call(
            methods::LINEAR_MY_PROJECTS,
            LinearMyProjectsParams {
                force: read.force(),
                cached_only: read.stored_only(),
            },
        )
    }

    pub(crate) fn linear_project_issues(
        &self,
        project_id: &str,
        read: ListRead,
    ) -> LinearTask<Vec<LinearIssueSummary>> {
        self.linear_call(
            methods::LINEAR_PROJECT_ISSUES,
            LinearProjectIssuesParams {
                project_id: project_id.to_string(),
                force: read.force(),
                cached_only: read.stored_only(),
            },
        )
    }

    pub(crate) fn linear_refresh_linked_items(
        &self,
        space_id: &str,
    ) -> LinearTask<LinearRefreshLinkedItemsReply> {
        self.linear_call(
            methods::LINEAR_REFRESH_LINKED_ITEMS,
            LinearRefreshLinkedItemsParams {
                space_id: space_id.to_string(),
            },
        )
    }

    pub(crate) fn set_space_linear_enabled(
        &self,
        params: SetSpaceLinearEnabledParams,
    ) -> LinearTask<WorkOutlineMutateReply> {
        self.linear_mutate(WorkOutlineMutation::SetSpaceLinearEnabled(params))
    }

    /// Reply carries `work_item_id` and `reference_id`.
    pub(crate) fn create_work_item_from_linear(
        &self,
        params: CreateWorkItemFromLinearParams,
    ) -> LinearTask<WorkOutlineMutateReply> {
        self.linear_mutate(WorkOutlineMutation::CreateWorkItemFromLinear(params))
    }

    pub(crate) fn link_linear_project(
        &self,
        params: LinkLinearProjectParams,
    ) -> LinearTask<WorkOutlineMutateReply> {
        self.linear_mutate(WorkOutlineMutation::LinkLinearProject(params))
    }

    /// `createWorkItemExternalReference` as a Linear call (issue links).
    pub(crate) fn link_linear_issue(
        &self,
        params: zeron_proto::CreateWorkItemExternalReferenceParams,
    ) -> LinearTask<WorkOutlineMutateReply> {
        map_task(self.create_external_reference(params))
    }
}

fn map_task<T>(task: Task<Result<T, WorkOutlineError>>) -> LinearTask<T> {
    LinearTask::Pending(task)
}

/// A facade task whose error reads as [`LinearUiError`]. Await it from a
/// `cx.spawn` like any gpui task.
pub(crate) enum LinearTask<T> {
    Pending(Task<Result<T, WorkOutlineError>>),
    Failed(Option<LinearUiError>),
}

impl<T> LinearTask<T> {
    fn failed(error: LinearUiError) -> Self {
        Self::Failed(Some(error))
    }
}

impl<T: 'static> std::future::Future for LinearTask<T> {
    type Output = Result<T, LinearUiError>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        // SAFETY: the inner task is never moved out of the pinned enum.
        let this = unsafe { self.get_unchecked_mut() };
        match this {
            Self::Pending(task) => unsafe { std::pin::Pin::new_unchecked(task) }
                .poll(cx)
                .map(|result| result.map_err(LinearUiError::from_outline)),
            Self::Failed(error) => std::task::Poll::Ready(Err(error
                .take()
                .unwrap_or_else(|| LinearUiError::new(LinearFailure::Other, "polled twice")))),
        }
    }
}
