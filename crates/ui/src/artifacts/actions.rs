//! Typed facade over the artifact RPCs (`ArtifactContent`, `ArtifactRegister`,
//! `ArtifactPromote`, `ArtifactPin`, `ArtifactRenderResult`).
//!
//! Transport and test scripting reuse [`WorkOutlineActions::call`] (bounded
//! retries of transient failures; `AppState::work_outline_actions` is the
//! test seam). Errors come back as [`ArtifactUiError`]: `unknown method` is
//! "not available on this host yet", `artifact/<code>: …` keeps its code.
use gpui::Task;
use serde::de::DeserializeOwned;
use zeron_proto::{
    ArtifactAnswerQuestionParams, ArtifactAnswerQuestionReply, ArtifactContentParams,
    ArtifactContentReply, ArtifactDriftParams, ArtifactDriftReply, ArtifactError,
    ArtifactErrorCode, ArtifactPinParams, ArtifactPinReply, ArtifactPromoteParams,
    ArtifactPromoteReply, ArtifactPublishReply, ArtifactRegisterParams, ArtifactRenderResultParams,
    ArtifactRenderResultReply, WorkOutlineError, WorkOutlineErrorCode,
};
use zeron_rpc::methods;

use super::store::UNSUPPORTED_TEXT;
use crate::work_outline::WorkOutlineActions;

/// A failed artifact call, ready to display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArtifactUiError {
    /// The host predates the artifact RPCs (`unknown method`).
    pub(crate) unsupported: bool,
    pub(crate) code: Option<ArtifactErrorCode>,
    pub(crate) message: String,
}

impl ArtifactUiError {
    pub(crate) fn from_work_outline(error: WorkOutlineError) -> Self {
        if crate::work_outline::actions::is_unsupported(&error) {
            return Self {
                unsupported: true,
                code: None,
                message: UNSUPPORTED_TEXT.to_owned(),
            };
        }
        match ArtifactError::from_wire_message(&error.message) {
            Some(coded) => Self {
                unsupported: false,
                code: Some(coded.code),
                message: coded.message,
            },
            None => Self {
                unsupported: false,
                code: None,
                message: error.message,
            },
        }
    }

    /// What the user reads.
    pub(crate) fn text(&self) -> String {
        if self.unsupported {
            return UNSUPPORTED_TEXT.to_owned();
        }
        match (self.code, self.message.is_empty()) {
            (Some(ArtifactErrorCode::NotFound), true) => "Artifact not found".to_owned(),
            (Some(code), true) => code.as_str().to_owned(),
            _ => self.message.clone(),
        }
    }
}

/// A pending call; await it inside a `cx.spawn`.
pub(crate) type ArtifactTask<T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, ArtifactUiError>> + 'static>>;

fn decode<T: DeserializeOwned>(value: serde_json::Value) -> Result<T, WorkOutlineError> {
    serde_json::from_value(value).map_err(|error| {
        WorkOutlineError::new(
            WorkOutlineErrorCode::Invalid,
            format!("malformed artifact reply: {error}"),
        )
    })
}

/// Cheap to clone.
#[derive(Clone)]
pub(crate) struct ArtifactActions {
    inner: WorkOutlineActions,
}

impl ArtifactActions {
    pub(crate) fn new(inner: WorkOutlineActions) -> Self {
        Self { inner }
    }

    fn call<P: serde::Serialize, T: DeserializeOwned + Send + 'static>(
        &self,
        method: &'static str,
        params: &P,
    ) -> ArtifactTask<T> {
        let params = match serde_json::to_value(params) {
            Ok(params) => params,
            Err(error) => {
                let error = ArtifactUiError {
                    unsupported: false,
                    code: Some(ArtifactErrorCode::Invalid),
                    message: error.to_string(),
                };
                return Box::pin(async move { Err(error) });
            }
        };
        let task: Task<Result<T, WorkOutlineError>> = self.inner.call(method, params, decode::<T>);
        Box::pin(async move { task.await.map_err(ArtifactUiError::from_work_outline) })
    }

    pub(crate) fn content(
        &self,
        params: ArtifactContentParams,
    ) -> ArtifactTask<ArtifactContentReply> {
        self.call(methods::ARTIFACT_CONTENT, &params)
    }

    pub(crate) fn register(
        &self,
        params: ArtifactRegisterParams,
    ) -> ArtifactTask<ArtifactPublishReply> {
        self.call(methods::ARTIFACT_REGISTER, &params)
    }

    pub(crate) fn promote(
        &self,
        params: ArtifactPromoteParams,
    ) -> ArtifactTask<ArtifactPromoteReply> {
        self.call(methods::ARTIFACT_PROMOTE, &params)
    }

    pub(crate) fn pin(&self, params: ArtifactPinParams) -> ArtifactTask<ArtifactPinReply> {
        self.call(methods::ARTIFACT_PIN, &params)
    }

    pub(crate) fn answer_question(
        &self,
        params: ArtifactAnswerQuestionParams,
    ) -> ArtifactTask<ArtifactAnswerQuestionReply> {
        self.call(methods::ARTIFACT_ANSWER_QUESTION, &params)
    }

    pub(crate) fn drift(&self, params: ArtifactDriftParams) -> ArtifactTask<ArtifactDriftReply> {
        self.call(methods::ARTIFACT_DRIFT, &params)
    }

    pub(crate) fn render_result(
        &self,
        params: ArtifactRenderResultParams,
    ) -> ArtifactTask<ArtifactRenderResultReply> {
        self.call(methods::ARTIFACT_RENDER_RESULT, &params)
    }
}
