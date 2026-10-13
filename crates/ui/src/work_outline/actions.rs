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
    ApplyInstructionRevisionParams, ArchiveWorkItemInstructionParams, ArchiveWorkItemParams,
    AssignChatToWorkItemParams, ConfigureExecutionParams, CreateInstructionRevisionParams,
    CreateWorkItemExternalReferenceParams, CreateWorkItemInstructionParams, CreateWorkItemParams,
    DescribeExecutionRepositoryParams, EffectiveInstructions, ExecutionRepositoryIdentity,
    InstructionRevisionComparison, InstructionRevisionView, MoveWorkItemInstructionParams,
    MoveWorkItemParams, RemoveChatFromWorkItemParams, RemoveWorkItemExternalReferenceParams,
    RepairWorkOutlineParams, RestoreWorkItemParams, UpdateWorkItemInstructionParams,
    UpdateWorkItemParams, ValidateExecutionConfigurationParams,
    ValidateExecutionConfigurationReply, WorkOutlineError, WorkOutlineErrorCode,
    WorkOutlineMutateReply, WorkOutlineMutation, WorkOutlineQueryRequest, WorkOutlineQueryResponse,
};
use zeron_rpc::{RpcError, methods};

use crate::state::EngineHandle;

/// A new idempotency key for one logical user action (UUID v4).
pub(crate) fn new_operation_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// The operation id of one logical user action, reused on every retry of
/// the SAME inputs (idempotent replay) and re-minted once the inputs change
/// after a failed attempt (a host that recorded the first attempt would
/// otherwise answer `operationConflict`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OperationSlot {
    id: Option<String>,
    inputs: Option<String>,
}

impl OperationSlot {
    /// The id to send with `inputs` (a stable rendering of the request).
    pub(crate) fn id_for(&mut self, inputs: &str) -> String {
        match (&self.id, &self.inputs) {
            (Some(id), Some(sent)) if sent == inputs => id.clone(),
            (Some(id), None) => {
                let id = id.clone();
                self.inputs = Some(inputs.to_owned());
                id
            }
            _ => {
                let id = new_operation_id();
                self.id = Some(id.clone());
                self.inputs = Some(inputs.to_owned());
                id
            }
        }
    }
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

/// Send `params` through `call`, retrying transient failures with the SAME
/// params (so a mutation keeps its `operationId`). `sleep` is the backoff
/// timer (injected so tests run without a clock).
async fn call_with_retry<Call, CallFut, Sleep, SleepFut>(
    params: serde_json::Value,
    mut call: Call,
    mut sleep: Sleep,
) -> Result<serde_json::Value, WorkOutlineError>
where
    Call: FnMut(serde_json::Value) -> CallFut,
    CallFut: Future<Output = Result<serde_json::Value, RpcError>>,
    Sleep: FnMut(Duration) -> SleepFut,
    SleepFut: Future<Output = ()>,
{
    let mut last_transport = String::new();
    for attempt in 0..MAX_ATTEMPTS {
        if attempt > 0 {
            sleep(retry_delay(attempt - 1)).await;
        }
        match call(params.clone()).await {
            Ok(value) => return Ok(value),
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

fn decode_reply(value: serde_json::Value) -> Result<WorkOutlineMutateReply, WorkOutlineError> {
    if value.is_null() {
        return Ok(WorkOutlineMutateReply::default());
    }
    serde_json::from_value(value).map_err(|error| {
        WorkOutlineError::new(
            WorkOutlineErrorCode::Invalid,
            format!("malformed Work Outline reply: {error}"),
        )
    })
}

/// Decode a typed reply of a non-mutation method.
fn decode_json<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
) -> Result<T, WorkOutlineError> {
    serde_json::from_value(value).map_err(|error| {
        WorkOutlineError::new(
            WorkOutlineErrorCode::Invalid,
            format!("malformed reply: {error}"),
        )
    })
}

fn decode_query(value: serde_json::Value) -> Result<WorkOutlineQueryResponse, WorkOutlineError> {
    serde_json::from_value(value).map_err(|error| {
        WorkOutlineError::new(
            WorkOutlineErrorCode::Invalid,
            format!("malformed Work Outline query reply: {error}"),
        )
    })
}

/// Send `mutation` through `call`, retrying transient failures. The mutation
/// is serialized ONCE, so every attempt carries identical params and hence the
/// same `operationId`.
#[cfg(test)]
async fn mutate_with_retry<Call, CallFut, Sleep, SleepFut>(
    mutation: &WorkOutlineMutation,
    call: Call,
    sleep: Sleep,
) -> Result<WorkOutlineMutateReply, WorkOutlineError>
where
    Call: FnMut(serde_json::Value) -> CallFut,
    CallFut: Future<Output = Result<serde_json::Value, RpcError>>,
    Sleep: FnMut(Duration) -> SleepFut,
    SleepFut: Future<Output = ()>,
{
    let params = serde_json::to_value(mutation)
        .map_err(|error| WorkOutlineError::new(WorkOutlineErrorCode::Invalid, error.to_string()))?;
    decode_reply(call_with_retry(params, call, sleep).await?)
}

/// Whether an error means "this host cannot do instruction work yet" (an
/// engine that answers `unsupported`, or predates the method): the UI shows
/// a calm "not available on this host" state instead of a failure.
pub(crate) fn is_unsupported(error: &WorkOutlineError) -> bool {
    error.code == WorkOutlineErrorCode::Unsupported && !is_transport_failure(error)
}

/// Displayable text for a failed instruction operation.
pub(crate) fn instruction_error_text(error: &WorkOutlineError) -> String {
    if is_unsupported(error) {
        "Instructions aren\u{2019}t available on this host yet".to_owned()
    } else if error.message.is_empty() {
        error.code.as_str().to_owned()
    } else {
        error.message.clone()
    }
}

/// Scripted transport for tests: `(method, params) -> reply`.
#[cfg(test)]
pub(crate) type FakeCall = std::sync::Arc<
    dyn Fn(&str, serde_json::Value) -> Result<serde_json::Value, RpcError> + Send + Sync,
>;

/// Holds scripted replies back until the test releases them, oldest first
/// (a mutation "in flight"). The scripted call itself still runs at send
/// time, so recorded params show what was sent.
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct ReplyGate(
    std::sync::Arc<
        std::sync::Mutex<std::collections::VecDeque<futures::channel::oneshot::Sender<()>>>,
    >,
);

#[cfg(test)]
impl ReplyGate {
    /// Let the oldest held reply through; `false` when none is held.
    pub(crate) fn release_one(&self) -> bool {
        let next = self.0.lock().unwrap().pop_front();
        next.is_some_and(|tx| tx.send(()).is_ok())
    }

    /// Replies sent but not yet released.
    pub(crate) fn held(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

/// Every query resolves to the typed reply or a coded [`WorkOutlineError`].
pub(crate) type QueryTask = Task<Result<WorkOutlineQueryResponse, WorkOutlineError>>;

/// Mutation facade. Cheap to clone; holds only the engine handle and the
/// executor its tasks run on.
#[derive(Clone)]
pub(crate) struct WorkOutlineActions {
    engine: Option<EngineHandle>,
    executor: Option<BackgroundExecutor>,
    #[cfg(test)]
    fake: Option<FakeCall>,
    #[cfg(test)]
    gate: Option<ReplyGate>,
}

impl WorkOutlineActions {
    pub(crate) fn new(engine: Option<EngineHandle>) -> Self {
        Self {
            engine,
            executor: None,
            #[cfg(test)]
            fake: None,
            #[cfg(test)]
            gate: None,
        }
    }

    /// A facade whose calls are answered synchronously by `call` (tests).
    #[cfg(test)]
    pub(crate) fn scripted(call: FakeCall) -> Self {
        Self {
            engine: None,
            executor: None,
            fake: Some(call),
            gate: None,
        }
    }

    /// [`Self::scripted`] whose replies wait for `gate` (spawned on
    /// `executor`).
    #[cfg(test)]
    pub(crate) fn scripted_gated(
        call: FakeCall,
        gate: ReplyGate,
        executor: BackgroundExecutor,
    ) -> Self {
        Self {
            engine: None,
            executor: Some(executor),
            fake: Some(call),
            gate: Some(gate),
        }
    }

    /// Bind the executor tasks run on (`AppState::work_outline_actions` does
    /// this once the app handle is known). Without one every call reports that
    /// the engine is not connected.
    pub(crate) fn with_executor(mut self, executor: BackgroundExecutor) -> Self {
        self.executor = Some(executor);
        self
    }

    /// Send `params` to `method` with transient-failure retries; see the
    /// module docs for retry semantics.
    fn call<T: Send + 'static>(
        &self,
        method: &'static str,
        params: serde_json::Value,
        decode: fn(serde_json::Value) -> Result<T, WorkOutlineError>,
    ) -> Task<Result<T, WorkOutlineError>> {
        #[cfg(test)]
        if let Some(fake) = self.fake.clone() {
            let reply = futures::executor::block_on(call_with_retry(
                params,
                move |params| {
                    let reply = fake(method, params);
                    async move { reply }
                },
                |_| async {},
            ))
            .and_then(decode);
            if let (Some(gate), Some(executor)) = (&self.gate, &self.executor) {
                let (tx, rx) = futures::channel::oneshot::channel();
                gate.0.lock().unwrap().push_back(tx);
                return executor.spawn(async move {
                    rx.await.ok();
                    reply
                });
            }
            return Task::ready(reply);
        }
        let (Some(engine), Some(executor)) = (self.engine.clone(), self.executor.clone()) else {
            return Task::ready(Err(WorkOutlineError::new(
                WorkOutlineErrorCode::Unsupported,
                "Engine not connected",
            )));
        };
        let timer = executor.clone();
        executor.spawn(async move {
            call_with_retry(
                params,
                |params| {
                    let engine = engine.clone();
                    async move { engine.client().call(method, params).await }
                },
                |delay| timer.timer(delay),
            )
            .await
            .and_then(decode)
        })
    }

    /// Send one mutation and decode the reply. The mutation is serialized
    /// once, so every transport retry carries the same `operationId`.
    fn mutate(&self, mutation: WorkOutlineMutation) -> ActionTask {
        let params = match serde_json::to_value(&mutation) {
            Ok(params) => params,
            Err(error) => {
                return Task::ready(Err(WorkOutlineError::new(
                    WorkOutlineErrorCode::Invalid,
                    error.to_string(),
                )));
            }
        };
        self.call(methods::WORK_OUTLINE_MUTATE, params, decode_reply)
    }

    /// One read-only `WorkOutlineQuery` (local, never forwarded; no
    /// operation id). Transient transport failures retry like mutations.
    pub(crate) fn query(&self, request: WorkOutlineQueryRequest) -> QueryTask {
        let params = match serde_json::to_value(&request) {
            Ok(params) => params,
            Err(error) => {
                return Task::ready(Err(WorkOutlineError::new(
                    WorkOutlineErrorCode::Invalid,
                    error.to_string(),
                )));
            }
        };
        self.call(methods::WORK_OUTLINE_QUERY, params, decode_query)
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

    // ── Milestone 2: instructions ──

    /// Reply carries `instruction_id`
    /// (`WorkOutlineMutateReply::into_instruction_id`).
    pub(crate) fn create_instruction(&self, params: CreateWorkItemInstructionParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::CreateWorkItemInstruction(params))
    }

    /// Replace an instruction's text (scope and provenance are immutable).
    pub(crate) fn update_instruction(&self, params: UpdateWorkItemInstructionParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::UpdateWorkItemInstruction(params))
    }

    /// Reorder within the instruction's own scope.
    pub(crate) fn move_instruction(&self, params: MoveWorkItemInstructionParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::MoveWorkItemInstruction(params))
    }

    pub(crate) fn archive_instruction(
        &self,
        params: ArchiveWorkItemInstructionParams,
    ) -> ActionTask {
        self.mutate(WorkOutlineMutation::ArchiveWorkItemInstruction(params))
    }

    /// Snapshot an item's current effective instructions; reply carries
    /// `revision_id` (`WorkOutlineMutateReply::into_revision_id`).
    pub(crate) fn create_instruction_revision(
        &self,
        params: CreateInstructionRevisionParams,
    ) -> ActionTask {
        self.mutate(WorkOutlineMutation::CreateInstructionRevision(params))
    }

    /// Select a revision for a Chat; reply carries `application_id`.
    pub(crate) fn apply_instruction_revision(
        &self,
        params: ApplyInstructionRevisionParams,
    ) -> ActionTask {
        self.mutate(WorkOutlineMutation::ApplyInstructionRevision(params))
    }

    // ── Milestone 4: user action requests and instruction proposals ──

    /// Resolve one request (and its open duplicates); reply `{}`.
    pub(crate) fn resolve_user_action_request(
        &self,
        params: zeron_proto::ResolveUserActionRequestParams,
    ) -> ActionTask {
        self.mutate(WorkOutlineMutation::ResolveUserActionRequest(params))
    }

    /// Approve a proposal with the reviewed text; reply carries
    /// `instruction_id` (`derive_id(proposalId, "instruction")`).
    pub(crate) fn approve_instruction_proposal(
        &self,
        params: zeron_proto::ApproveWorkItemInstructionProposalParams,
    ) -> ActionTask {
        self.mutate(WorkOutlineMutation::ApproveWorkItemInstructionProposal(
            params,
        ))
    }

    /// Reject a proposal (optional reason); reply `{}`.
    pub(crate) fn reject_instruction_proposal(
        &self,
        params: zeron_proto::RejectWorkItemInstructionProposalParams,
    ) -> ActionTask {
        self.mutate(WorkOutlineMutation::RejectWorkItemInstructionProposal(
            params,
        ))
    }

    // ── Milestone 3: execution configuration ──

    /// Write (`configuration`) or reset (`None`) one scope's configuration.
    /// Shape-only on the host; reply `{}`.
    pub(crate) fn configure_execution(&self, params: ConfigureExecutionParams) -> ActionTask {
        self.mutate(WorkOutlineMutation::ConfigureExecution(params))
    }

    /// `ValidateExecutionConfiguration` (forwardable; the engine forwards to
    /// the Space's owning device itself, so the caller never sets
    /// `targetDeviceId`). Validation failures are a normal reply.
    pub(crate) fn validate_execution(
        &self,
        params: ValidateExecutionConfigurationParams,
    ) -> Task<Result<ValidateExecutionConfigurationReply, WorkOutlineError>> {
        match serde_json::to_value(&params) {
            Ok(value) => self.call(
                methods::VALIDATE_EXECUTION_CONFIGURATION,
                value,
                decode_json::<ValidateExecutionConfigurationReply>,
            ),
            Err(error) => Task::ready(Err(WorkOutlineError::new(
                WorkOutlineErrorCode::Invalid,
                error.to_string(),
            ))),
        }
    }

    /// `CreateChatForWorkItem` (forwardable; the engine forwards to the
    /// Space's owner itself). Transport retries resend the same params, so
    /// the same `operationId`; the host completes missing steps and returns
    /// the same ids.
    pub(crate) fn create_chat_for_work_item(
        &self,
        params: zeron_proto::CreateChatForWorkItemParams,
    ) -> Task<Result<zeron_proto::CreateChatForWorkItemReply, WorkOutlineError>> {
        match serde_json::to_value(&params) {
            Ok(value) => self.call(
                methods::CREATE_CHAT_FOR_WORK_ITEM,
                value,
                decode_json::<zeron_proto::CreateChatForWorkItemReply>,
            ),
            Err(error) => Task::ready(Err(WorkOutlineError::new(
                WorkOutlineErrorCode::Invalid,
                error.to_string(),
            ))),
        }
    }

    /// `ListCommands` (the chat composer's slash-command/skill discovery) for
    /// the editors' `/` skill autocomplete; `params` come from the composer's
    /// own builder, so the folder and device targeting match a chat's.
    pub(crate) fn list_commands(
        &self,
        params: serde_json::Value,
    ) -> Task<Result<Vec<zeron_proto::SlashCommand>, WorkOutlineError>> {
        self.call(
            methods::LIST_COMMANDS,
            params,
            decode_json::<Vec<zeron_proto::SlashCommand>>,
        )
    }

    /// `DescribeExecutionRepository`: the identity a binding records for the
    /// checkout at `path`. The caller sets `targetDeviceId` when the Space
    /// lives on another device (only the owner can fingerprint it).
    pub(crate) fn describe_repository(
        &self,
        params: DescribeExecutionRepositoryParams,
    ) -> Task<Result<ExecutionRepositoryIdentity, WorkOutlineError>> {
        match serde_json::to_value(&params) {
            Ok(value) => self.call(
                methods::DESCRIBE_EXECUTION_REPOSITORY,
                value,
                decode_json::<ExecutionRepositoryIdentity>,
            ),
            Err(error) => Task::ready(Err(WorkOutlineError::new(
                WorkOutlineErrorCode::Invalid,
                error.to_string(),
            ))),
        }
    }
}

/// Typed results of the [`WorkOutlineActions::query`] facade.
pub(crate) fn expect_effective(
    response: WorkOutlineQueryResponse,
) -> Result<EffectiveInstructions, WorkOutlineError> {
    match response {
        WorkOutlineQueryResponse::ResolveEffectiveInstructions(effective) => Ok(effective),
        _ => Err(mismatched_query()),
    }
}

pub(crate) fn expect_comparison(
    response: WorkOutlineQueryResponse,
) -> Result<InstructionRevisionComparison, WorkOutlineError> {
    match response {
        WorkOutlineQueryResponse::CompareInstructionRevisions { comparison, .. } => Ok(comparison),
        _ => Err(mismatched_query()),
    }
}

pub(crate) fn expect_revision(
    response: WorkOutlineQueryResponse,
) -> Result<InstructionRevisionView, WorkOutlineError> {
    match response {
        WorkOutlineQueryResponse::GetInstructionRevision(revision) => Ok(revision),
        _ => Err(mismatched_query()),
    }
}

fn mismatched_query() -> WorkOutlineError {
    WorkOutlineError::new(
        WorkOutlineErrorCode::Invalid,
        "Work Outline query reply does not match the request",
    )
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
    fn operation_slots_reuse_ids_for_the_same_inputs_only() {
        let mut slot = OperationSlot::default();
        let first = slot.id_for("a");
        assert_eq!(slot.id_for("a"), first);
        let second = slot.id_for("b");
        assert_ne!(second, first);
        assert_eq!(slot.id_for("b"), second);
    }

    #[test]
    fn operation_ids_are_unique_per_intent() {
        assert_ne!(new_operation_id(), new_operation_id());
    }

    #[test]
    fn instruction_mutations_and_queries_use_their_wire_shape() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(
            Vec::<(String, serde_json::Value)>::new(),
        ));
        let sink = seen.clone();
        let actions = WorkOutlineActions::scripted(std::sync::Arc::new(move |method, params| {
            sink.lock()
                .unwrap()
                .push((method.to_owned(), params.clone()));
            match params.get("query").and_then(|q| q.as_str()) {
                Some(_) => Err(RpcError::Failed(
                    "work-outline/unsupported: Instructions are not available yet".into(),
                )),
                None => Ok(serde_json::json!({"revisionId": "r1"})),
            }
        }));
        let reply = futures::executor::block_on(actions.create_instruction_revision(
            CreateInstructionRevisionParams {
                work_item_id: "w1".into(),
                operation_id: "op-r".into(),
            },
        ))
        .unwrap();
        assert_eq!(reply.into_revision_id().unwrap(), "r1");
        let error = futures::executor::block_on(actions.query(
            WorkOutlineQueryRequest::GetInstructionRevision {
                revision_id: "r1".into(),
            },
        ))
        .unwrap_err();
        assert!(is_unsupported(&error));
        assert_eq!(
            instruction_error_text(&error),
            "Instructions aren\u{2019}t available on this host yet"
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0, methods::WORK_OUTLINE_MUTATE);
        assert_eq!(seen[0].1["op"], "createInstructionRevision");
        assert_eq!(seen[0].1["operationId"], "op-r");
        assert_eq!(seen[1].0, methods::WORK_OUTLINE_QUERY);
        assert_eq!(seen[1].1["query"], "getInstructionRevision");
    }

    #[test]
    fn query_replies_decode_and_mismatches_are_invalid() {
        let actions = WorkOutlineActions::scripted(std::sync::Arc::new(|_, _| {
            Ok(serde_json::json!({
                "query": "resolveEffectiveInstructions",
                "workItemId": "w1",
                "entries": [],
            }))
        }));
        let response = futures::executor::block_on(actions.query(
            WorkOutlineQueryRequest::ResolveEffectiveInstructions {
                work_item_id: "w1".into(),
            },
        ))
        .unwrap();
        assert!(expect_revision(response.clone()).is_err());
        assert_eq!(expect_effective(response).unwrap().work_item_id, "w1");
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

    #[test]
    fn execution_calls_use_their_own_methods_and_decode_replies() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let actions = WorkOutlineActions::scripted(std::sync::Arc::new(move |method, params| {
            sink.lock()
                .unwrap()
                .push((method.to_owned(), params.clone()));
            Ok(match method {
                methods::VALIDATE_EXECUTION_CONFIGURATION => {
                    serde_json::json!({"ok": false, "errors": [{
                        "error": {"code": "deviceOffline", "deviceId": "d"},
                        "message": "Device d is offline", "remediation": "Bring it online."
                    }]})
                }
                methods::DESCRIBE_EXECUTION_REPOSITORY => {
                    serde_json::json!({"repoId": "repo:x", "repoPath": "/x"})
                }
                _ => serde_json::json!({}),
            })
        }));
        let reply = futures::executor::block_on(actions.validate_execution(
            ValidateExecutionConfigurationParams {
                chat_id: Some("chat".into()),
                ..Default::default()
            },
        ))
        .unwrap();
        assert!(!reply.ok);
        assert_eq!(reply.errors[0].error.code(), "deviceOffline");
        let identity = futures::executor::block_on(actions.describe_repository(
            DescribeExecutionRepositoryParams {
                path: "/x".into(),
                target_device_id: Some("d".into()),
            },
        ))
        .unwrap();
        assert_eq!(identity.repo_id, "repo:x");
        futures::executor::block_on(actions.configure_execution(ConfigureExecutionParams {
            scope: zeron_proto::ExecutionScope::Space {
                space_id: "s".into(),
            },
            configuration: None,
            operation_id: "op".into(),
        }))
        .unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0, methods::VALIDATE_EXECUTION_CONFIGURATION);
        assert_eq!(
            seen[0].1,
            serde_json::json!({"chatId": "chat", "hostCapabilities": {"additionalDirectories": false}})
        );
        assert_eq!(seen[1].0, methods::DESCRIBE_EXECUTION_REPOSITORY);
        assert_eq!(seen[1].1["targetDeviceId"], "d");
        assert_eq!(seen[2].0, methods::WORK_OUTLINE_MUTATE);
        assert_eq!(seen[2].1["op"], "configureExecution");
        assert!(seen[2].1["configuration"].is_null());
    }
}
