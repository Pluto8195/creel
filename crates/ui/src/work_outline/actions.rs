//! Seam 3: the mutation facade over the `WorkOutlineMutate` RPC.
//!
//! The facade exposes one typed method per operation and returns a gpui
//! [`Task`], so views never touch the RPC client. Every call takes the wire
//! params struct, which carries the client-generated `operation_id`: callers
//! mint one id per logical user action with [`new_operation_id`] and reuse it
//! on every retry of that action, so retries replay idempotently. Optimistic
//! echo and rollback (the `persist_chat_title` pattern in `shell.rs`) and the
//! single in-flight guard per item belong to the callers (they own the
//! optimistic overlay).
//!
//! Transport: one `WorkOutlineMutate` call per mutation. Transient transport
//! errors (`Transport`, `Closed`) are retried a bounded number of times with
//! the SAME serialized mutation, so every attempt carries the SAME
//! `operationId` and the host replays idempotently. Coded host errors
//! (`work-outline/<code>: <message>`) and `unknown method` are final and map to
//! [`WorkOutlineError`]; they are never retried.
use std::future::Future;
use std::time::Duration;

use gpui::{BackgroundExecutor, Task};
use zeron_proto::{
    ArchiveWorkItemParams, AssignChatToWorkItemParams, CreateWorkItemExternalReferenceParams,
    CreateWorkItemParams, MoveWorkItemParams, RemoveChatFromWorkItemParams,
    RemoveWorkItemExternalReferenceParams, RepairWorkOutlineParams, RestoreWorkItemParams,
    UpdateWorkItemParams, WorkOutlineError, WorkOutlineErrorCode, WorkOutlineMutateReply,
    WorkOutlineMutation,
};
use zeron_rpc::{RpcError, methods};

use crate::state::EngineHandle;

/// A new idempotency key for one logical user action (UUID v4).
pub(crate) fn new_operation_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Every operation resolves to the shared reply (`{}` unless the operation
/// mints an id) or a coded [`WorkOutlineError`].
pub(crate) type ActionTask = Task<Result<WorkOutlineMutateReply, WorkOutlineError>>;

/// Total attempts per mutation (the first try plus retries).
pub(crate) const MAX_ATTEMPTS: u32 = 4;

/// Message prefix of the error returned when every attempt failed in
/// transport. The code is `Unsupported` (the lenient catch-all); use
/// [`is_transport_failure`] to tell "could not reach the host, safe to retry
/// with the same operation id" apart from a host that lacks the feature.
pub(crate) const TRANSPORT_FAILURE_PREFIX: &str = "Couldn't reach the host";

pub(crate) fn is_transport_failure(error: &WorkOutlineError) -> bool {
    error.message.starts_with(TRANSPORT_FAILURE_PREFIX)
}

/// Delay before retry number `retry` (0-based): 250 ms doubling.
fn retry_delay(retry: u32) -> Duration {
    Duration::from_millis(250u64.saturating_mul(1u64 << retry.min(6)))
}

/// How one failed attempt is treated.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    /// Retry with the same operation id.
    Transient(String),
    /// Report to the caller.
    Final(WorkOutlineError),
}

fn classify(error: RpcError) -> Failure {
    match error {
        RpcError::UnknownMethod(method) => Failure::Final(WorkOutlineError::new(
            WorkOutlineErrorCode::Unsupported,
            format!("This host does not support Work Outline ({method})"),
        )),
        RpcError::Transport(message) => Failure::Transient(message),
        RpcError::Closed => Failure::Transient("connection closed".to_owned()),
        RpcError::BadParams(message) => Failure::Final(WorkOutlineError::new(
            WorkOutlineErrorCode::Invalid,
            message,
        )),
        RpcError::Failed(message) => Failure::Final(parse_failed_message(&message)),
    }
}

/// A host-reported failure string to a coded error. Strings without the
/// `work-outline/` prefix (an older or foreign host) read as `Invalid` with
/// the text preserved for display.
pub(crate) fn parse_failed_message(message: &str) -> WorkOutlineError {
    WorkOutlineError::from_wire_message(message)
        .unwrap_or_else(|| WorkOutlineError::new(WorkOutlineErrorCode::Invalid, message))
}

/// Send `mutation` through `call`, retrying transient failures. The mutation
/// is serialized ONCE, so every attempt carries identical params and hence the
/// same `operationId`. `sleep` is the backoff timer (injected so tests run
/// without a clock).
async fn mutate_with_retry<Call, CallFut, Sleep, SleepFut>(
    mutation: &WorkOutlineMutation,
    mut call: Call,
    mut sleep: Sleep,
) -> Result<WorkOutlineMutateReply, WorkOutlineError>
where
    Call: FnMut(serde_json::Value) -> CallFut,
    CallFut: Future<Output = Result<serde_json::Value, RpcError>>,
    Sleep: FnMut(Duration) -> SleepFut,
    SleepFut: Future<Output = ()>,
{
    let params = serde_json::to_value(mutation)
        .map_err(|error| WorkOutlineError::new(WorkOutlineErrorCode::Invalid, error.to_string()))?;
    let mut last_transport = String::new();
    for attempt in 0..MAX_ATTEMPTS {
        if attempt > 0 {
            sleep(retry_delay(attempt - 1)).await;
        }
        match call(params.clone()).await {
            Ok(value) => {
                return if value.is_null() {
                    Ok(WorkOutlineMutateReply::default())
                } else {
                    serde_json::from_value(value).map_err(|error| {
                        WorkOutlineError::new(
                            WorkOutlineErrorCode::Invalid,
                            format!("malformed Work Outline reply: {error}"),
                        )
                    })
                };
            }
            Err(error) => match classify(error) {
                Failure::Final(error) => return Err(error),
                Failure::Transient(message) => last_transport = message,
            },
        }
    }
    Err(WorkOutlineError::new(
        WorkOutlineErrorCode::Unsupported,
        format!("{TRANSPORT_FAILURE_PREFIX}: {last_transport}"),
    ))
}

/// Mutation facade. Cheap to clone; holds only the engine handle and the
/// executor its tasks run on.
#[derive(Clone)]
pub(crate) struct WorkOutlineActions {
    engine: Option<EngineHandle>,
    executor: Option<BackgroundExecutor>,
}

impl WorkOutlineActions {
    pub(crate) fn new(engine: Option<EngineHandle>) -> Self {
        Self {
            engine,
            executor: None,
        }
    }

    /// Bind the executor tasks run on (`AppState::work_outline_actions` does
    /// this once the app handle is known). Without one every call reports that
    /// the engine is not connected.
    pub(crate) fn with_executor(mut self, executor: BackgroundExecutor) -> Self {
        self.executor = Some(executor);
        self
    }

    /// Send one mutation and decode the reply; see the module docs for retry
    /// semantics.
    fn mutate(&self, mutation: WorkOutlineMutation) -> ActionTask {
        let (Some(engine), Some(executor)) = (self.engine.clone(), self.executor.clone()) else {
            return Task::ready(Err(WorkOutlineError::new(
                WorkOutlineErrorCode::Unsupported,
                "Engine not connected",
            )));
        };
        let timer = executor.clone();
        executor.spawn(async move {
            mutate_with_retry(
                &mutation,
                |params| {
                    let engine = engine.clone();
                    async move {
                        engine
                            .client()
                            .call(methods::WORK_OUTLINE_MUTATE, params)
                            .await
                    }
                },
                |delay| timer.timer(delay),
            )
            .await
        })
    }

    /// Reply carries `work_item_id` (`derive_id(operationId, "item")`);
    /// decode with `WorkOutlineMutateReply::into_create_work_item`.
    pub(crate) fn create_work_item(&self, params: CreateWorkItemParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::CreateWorkItem(params))
    }

    /// Rename, retitle, change status, edit the description, or toggle
    /// instruction inheritance (`WorkItemChanges`).
    pub(crate) fn update_work_item(&self, params: UpdateWorkItemParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::UpdateWorkItem(params))
    }

    /// Reparent and/or reorder (`MoveWorkItemParams::placement`).
    pub(crate) fn move_work_item(&self, params: MoveWorkItemParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::MoveWorkItem(params))
    }

    pub(crate) fn archive_work_item(&self, params: ArchiveWorkItemParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::ArchiveWorkItem(params))
    }

    pub(crate) fn restore_work_item(&self, params: RestoreWorkItemParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::RestoreWorkItem(params))
    }

    /// Associate a Chat with a work item of the same Space.
    pub(crate) fn assign_chat(&self, params: AssignChatToWorkItemParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::AssignChatToWorkItem(params))
    }

    /// Dissociate a Chat (writes an explicit disassociation, not a delete).
    pub(crate) fn remove_chat(&self, params: RemoveChatFromWorkItemParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::RemoveChatFromWorkItem(params))
    }

    /// Reply carries `reference_id`; decode with
    /// `WorkOutlineMutateReply::into_create_external_reference`.
    pub(crate) fn create_external_reference(
        &self,
        params: CreateWorkItemExternalReferenceParams,
    ) -> ActionTask {
        self.mutate(WorkOutlineMutation::CreateWorkItemExternalReference(params))
    }

    pub(crate) fn remove_external_reference(
        &self,
        params: RemoveWorkItemExternalReferenceParams,
    ) -> ActionTask {
        self.mutate(WorkOutlineMutation::RemoveWorkItemExternalReference(params))
    }

    /// Persist derived repairs (cycle victims, bad order keys) for a Space.
    pub(crate) fn repair(&self, params: RepairWorkOutlineParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::RepairWorkOutline(params))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use zeron_proto::WorkOutlinePlacement;

    fn move_mutation(operation_id: &str) -> WorkOutlineMutation {
        WorkOutlineMutation::MoveWorkItem(MoveWorkItemParams {
            work_item_id: "w1".into(),
            destination_parent_work_item_id: None,
            placement: WorkOutlinePlacement::Last,
            operation_id: operation_id.into(),
        })
    }

    /// Run `mutation` against a scripted sequence of replies; returns the
    /// outcome, every params value the "host" saw, and the sleeps taken.
    fn run(
        mutation: &WorkOutlineMutation,
        script: Vec<Result<serde_json::Value, RpcError>>,
    ) -> (
        Result<WorkOutlineMutateReply, WorkOutlineError>,
        Vec<serde_json::Value>,
        Vec<Duration>,
    ) {
        let script = RefCell::new(script.into_iter());
        let seen = RefCell::new(Vec::new());
        let sleeps = RefCell::new(Vec::new());
        let outcome = futures::executor::block_on(mutate_with_retry(
            mutation,
            |params| {
                seen.borrow_mut().push(params);
                let next = script.borrow_mut().next().expect("scripted reply");
                async move { next }
            },
            |delay| {
                sleeps.borrow_mut().push(delay);
                async {}
            },
        ));
        (outcome, seen.into_inner(), sleeps.into_inner())
    }

    #[test]
    fn coded_wire_errors_parse_to_work_outline_errors() {
        let error = parse_failed_message("work-outline/cycle: would create a cycle");
        assert_eq!(error.code, WorkOutlineErrorCode::Cycle);
        assert_eq!(error.message, "would create a cycle");
        let bare = parse_failed_message("work-outline/archived");
        assert_eq!(bare.code, WorkOutlineErrorCode::Archived);
        assert_eq!(bare.message, "");
        let unknown_code = parse_failed_message("work-outline/future: later");
        assert_eq!(unknown_code.code, WorkOutlineErrorCode::Unsupported);
        // Foreign text keeps its message and reads as invalid.
        let foreign = parse_failed_message("disk full");
        assert_eq!(foreign.code, WorkOutlineErrorCode::Invalid);
        assert_eq!(foreign.message, "disk full");
    }

    #[test]
    fn rpc_errors_classify_into_final_and_transient() {
        assert!(matches!(
            classify(RpcError::UnknownMethod("WorkOutlineMutate".into())),
            Failure::Final(error) if error.code == WorkOutlineErrorCode::Unsupported
        ));
        assert!(matches!(
            classify(RpcError::Failed("work-outline/notFound: gone".into())),
            Failure::Final(error) if error.code == WorkOutlineErrorCode::NotFound
        ));
        assert!(matches!(
            classify(RpcError::BadParams("x".into())),
            Failure::Final(error) if error.code == WorkOutlineErrorCode::Invalid
        ));
        assert_eq!(
            classify(RpcError::Transport("reset".into())),
            Failure::Transient("reset".into())
        );
        assert!(matches!(classify(RpcError::Closed), Failure::Transient(_)));
    }

    #[test]
    fn transient_errors_retry_with_the_same_operation_id() {
        let mutation = move_mutation("op-1");
        let (outcome, seen, sleeps) = run(
            &mutation,
            vec![
                Err(RpcError::Transport("reset".into())),
                Err(RpcError::Closed),
                Ok(serde_json::json!({})),
            ],
        );
        assert_eq!(outcome.unwrap(), WorkOutlineMutateReply::default());
        assert_eq!(seen.len(), 3);
        assert!(seen.iter().all(|params| params == &seen[0]));
        assert!(seen.iter().all(|params| params["operationId"] == "op-1"));
        assert_eq!(seen[0]["op"], "moveWorkItem");
        assert_eq!(
            sleeps,
            [Duration::from_millis(250), Duration::from_millis(500)]
        );
    }

    #[test]
    fn retries_are_bounded_and_report_a_transport_failure() {
        let mutation = move_mutation("op-2");
        let script = (0..MAX_ATTEMPTS)
            .map(|_| Err(RpcError::Transport("reset".into())))
            .collect();
        let (outcome, seen, sleeps) = run(&mutation, script);
        let error = outcome.unwrap_err();
        assert!(is_transport_failure(&error));
        assert_eq!(seen.len(), MAX_ATTEMPTS as usize);
        assert_eq!(sleeps.len(), MAX_ATTEMPTS as usize - 1);
        assert!(seen.iter().all(|params| params["operationId"] == "op-2"));
    }

    #[test]
    fn coded_and_unknown_method_errors_are_not_retried() {
        let mutation = move_mutation("op-3");
        let (outcome, seen, sleeps) = run(
            &mutation,
            vec![Err(RpcError::Failed(
                "work-outline/operationConflict: reused".into(),
            ))],
        );
        assert_eq!(
            outcome.unwrap_err().code,
            WorkOutlineErrorCode::OperationConflict
        );
        assert_eq!(seen.len(), 1);
        assert!(sleeps.is_empty());

        let (outcome, seen, _) = run(
            &mutation,
            vec![Err(RpcError::UnknownMethod("WorkOutlineMutate".into()))],
        );
        let error = outcome.unwrap_err();
        assert_eq!(error.code, WorkOutlineErrorCode::Unsupported);
        assert!(!is_transport_failure(&error));
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn reply_decodes_minted_ids_and_tolerates_null() {
        let mutation = move_mutation("op-4");
        let (outcome, _, _) = run(&mutation, vec![Ok(serde_json::json!({"workItemId": "w9"}))]);
        assert_eq!(
            outcome
                .unwrap()
                .into_create_work_item()
                .unwrap()
                .work_item_id,
            "w9"
        );
        let (outcome, _, _) = run(&mutation, vec![Ok(serde_json::Value::Null)]);
        assert_eq!(outcome.unwrap(), WorkOutlineMutateReply::default());
    }

    #[test]
    fn operation_ids_are_unique_per_intent() {
        assert_ne!(new_operation_id(), new_operation_id());
    }

    #[test]
    fn facade_without_an_engine_reports_not_connected() {
        let actions = WorkOutlineActions::new(None);
        let task = actions.repair(RepairWorkOutlineParams {
            space_id: "s".into(),
            operation_id: "op".into(),
        });
        let error = futures::executor::block_on(task).unwrap_err();
        assert_eq!(error.message, "Engine not connected");
    }
}
