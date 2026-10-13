//! Work Outline service: the policy layer over the registry mutators in
//! `zeron_doc::registry::work_outline` (contract `docs/plans/work-outline/
//! m0-contract.md`, registry audit section 7.11).
//!
//! - `WorkOutlineMutate`: kill switch, operation-id replay protection (same
//!   id + same inputs returns the first reply without writing; same id +
//!   other inputs is `OperationConflict`), actor derivation, then one
//!   registry write. The mechanism checks (live Space, same Space,
//!   projectless, cycles, sizes, create-if-absent) run in the doc layer under
//!   the registry lock, so the check and the write are atomic.
//!
//! Replay protection is durable. An in-memory cache is the fast path; the
//! source of truth is the device-local processed-command ledger
//! (`processed_commands` in the profile `DocsStore`, shared with the chat
//! command plane, keyed globally, never pruned) under two reserved keys per
//! operation: [`ledger_operation_key`] (`work-outline:op:{operationId}`) and
//! [`ledger_inputs_key`] (`work-outline:in:{inputsHash}:{operationId}`). They
//! are claimed in the same SQLite transaction as the registry snapshot that
//! contains the write (`WorkspaceHost::work_outline_mutate`), so the ledger
//! and the registry can never disagree after a crash. Ledger TTL: none; the
//! ledger is never pruned, so an operation id is consumed for the lifetime of
//! the profile store on this device. It is device-local: the same id replayed
//! against another device's engine is guarded only by the doc layer's
//! replay-safe mutators (create-if-absent, compare-before-write).
//! - `WorkOutlineQuery`: read-only instruction questions answered from the
//!   local replica (effective instructions over the materialized tree,
//!   revision lookup with integrity, revision comparison of two intact
//!   revisions) and, from Milestone 3, `resolveExecutionConfiguration`
//!   (whole-row resolution; validation is the device-routed
//!   `ValidateExecutionConfiguration` method in `crate::execution_scope`).
//!   Not gated by the kill switch.
//! - `configureExecution` (Milestone 3) is an ordinary `WorkOutlineMutate`
//!   operation: kill switch, replay ledger, shape-only checks in the doc
//!   layer (paths may live on another device, so no filesystem access).
//! - [`WorkOutlineService::capture_chat_execution_binding`]: the engine
//!   entry point the Milestone 3 start workflow calls once per launch.
//! - Milestone 4: every mutation takes its originating actor
//!   ([`WorkOutlineService::mutate_as`]); `WorkOutlineMutate` over RPC is the
//!   local user's ([`WorkOutlineService::mutate`]). Agent actors are checked
//!   against the narrow agent policy ([`WorkOutlineService::authorize_agent`],
//!   contract section 8) inside the registry lock, after the replay check (a
//!   replay writes nothing). [`WorkOutlineService::agent_context`] is what an
//!   agent may read.
//! - `WatchWorkOutline(spaceId)`: driven by the registry change counter
//!   (WATCH_PREVIEWS pattern): coalesce bursts, skip the recompute when no
//!   row kind the outline reads changed (a per-kind registry generation;
//!   when unsure, recompute), clone inputs under the lock and materialize
//!   off the async runtime (both in `spawn_blocking`), emit only frames
//!   whose content changed, stamp a per-stream monotone `revision`.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use zeron_proto::work_outline::{
    DISABLE_WRITES_ENV, WorkOutlineErrorCode, derive_id, external_reference_id, roles,
};
use zeron_proto::work_outline_instructions::{
    InstructionRevisionIntegrity, compare_instruction_revisions,
};
use zeron_proto::{
    WorkOutlineActor, WorkOutlineActorKind, WorkOutlineError, WorkOutlineMutateReply,
    WorkOutlineMutation, WorkOutlineQueryRequest, WorkOutlineQueryResponse, WorkOutlineSnapshot,
};

use crate::workspace_host::WorkspaceHost;

/// How many recent operation ids the replay cache remembers.
const RECENT_OPERATIONS: usize = 4_096;
/// Coalescing window after a registry change: a burst of `rows` frames or
/// seen-marker writes becomes one recompute.
const COALESCE: Duration = Duration::from_millis(75);

/// The kill switch is on when the env var is set to anything but empty/`0`.
pub fn writes_disabled() -> bool {
    std::env::var(DISABLE_WRITES_ENV)
        .map(|value| {
            let value = value.trim();
            !value.is_empty() && value != "0"
        })
        .unwrap_or(false)
}

#[derive(Default)]
struct RecentOperations {
    order: VecDeque<String>,
    replies: HashMap<String, (String, WorkOutlineMutateReply)>,
}

impl RecentOperations {
    fn get(&self, operation_id: &str) -> Option<&(String, WorkOutlineMutateReply)> {
        self.replies.get(operation_id)
    }

    fn insert(&mut self, operation_id: String, hash: String, reply: WorkOutlineMutateReply) {
        if self.replies.contains_key(&operation_id) {
            return;
        }
        if self.order.len() >= RECENT_OPERATIONS
            && let Some(oldest) = self.order.pop_front()
        {
            self.replies.remove(&oldest);
        }
        self.order.push_back(operation_id.clone());
        self.replies.insert(operation_id, (hash, reply));
    }
}

/// Prefix of every Work Outline key in the processed-command ledger.
pub const LEDGER_KEY_PREFIX: &str = "work-outline:";

/// Ledger key recording that `operation_id` was applied on this device.
pub fn ledger_operation_key(operation_id: &str) -> String {
    format!("{LEDGER_KEY_PREFIX}op:{operation_id}")
}

/// Ledger key recording the inputs `operation_id` was applied with (the hash
/// is fixed-width hex, so the key is unambiguous).
pub fn ledger_inputs_key(inputs_hash: &str, operation_id: &str) -> String {
    format!("{LEDGER_KEY_PREFIX}in:{inputs_hash}:{operation_id}")
}

/// The reply of an already-applied mutation, recomputed without touching the
/// registry: every id a mutation returns is derived from its inputs.
fn replay_reply(mutation: &WorkOutlineMutation) -> WorkOutlineMutateReply {
    match mutation {
        WorkOutlineMutation::CreateWorkItem(p) => WorkOutlineMutateReply {
            work_item_id: Some(derive_id(&p.operation_id, roles::ITEM)),
            ..Default::default()
        },
        WorkOutlineMutation::CreateWorkItemExternalReference(p) => WorkOutlineMutateReply {
            reference_id: Some(external_reference_id(
                &p.work_item_id,
                &p.provider,
                &p.reference_type,
                &p.external_id,
            )),
            ..Default::default()
        },
        WorkOutlineMutation::CreateWorkItemInstruction(p) => WorkOutlineMutateReply {
            instruction_id: Some(derive_id(&p.operation_id, roles::INSTRUCTION)),
            ..Default::default()
        },
        WorkOutlineMutation::CreateInstructionRevision(p) => WorkOutlineMutateReply {
            revision_id: Some(derive_id(&p.operation_id, roles::REVISION)),
            ..Default::default()
        },
        WorkOutlineMutation::ApplyInstructionRevision(p) => WorkOutlineMutateReply {
            application_id: Some(derive_id(&p.operation_id, roles::APPLICATION)),
            ..Default::default()
        },
        WorkOutlineMutation::KeepInstructionSession(p) => WorkOutlineMutateReply {
            application_id: Some(derive_id(&p.operation_id, roles::APPLICATION)),
            ..Default::default()
        },
        WorkOutlineMutation::CreateUserActionRequest(p) => WorkOutlineMutateReply {
            user_action_request_id: Some(derive_id(&p.operation_id, roles::REQUEST)),
            ..Default::default()
        },
        WorkOutlineMutation::ProposeWorkItemInstruction(p) => WorkOutlineMutateReply {
            proposal_id: Some(derive_id(&p.operation_id, roles::PROPOSAL)),
            ..Default::default()
        },
        WorkOutlineMutation::ApproveWorkItemInstructionProposal(p) => WorkOutlineMutateReply {
            instruction_id: Some(derive_id(&p.proposal_id, roles::INSTRUCTION)),
            ..Default::default()
        },
        _ => WorkOutlineMutateReply::default(),
    }
}

/// This device's delivery record for a Chat it hosts (what
/// `keepInstructionSession` keeps): `None` = not hosted here or no record.
pub type KeptSessionResolver = std::sync::Arc<
    dyn Fn(&str) -> Option<Option<zeron_proto::InstructionRevisionPin>> + Send + Sync,
>;

/// The engine's Work Outline surface (one per `EngineRpc`).
pub struct WorkOutlineService {
    workspace: WorkspaceHost,
    /// Serializes check-then-apply so two concurrent calls with one
    /// operation id cannot both pass the replay check.
    gate: Mutex<RecentOperations>,
    kept_session: Mutex<Option<KeptSessionResolver>>,
}

impl WorkOutlineService {
    pub fn new(workspace: WorkspaceHost) -> Self {
        Self {
            workspace,
            gate: Mutex::new(RecentOperations::default()),
            kept_session: Mutex::new(None),
        }
    }

    /// Wire the host's delivery record (engine assembly). Without it,
    /// `keepInstructionSession` keeps the reported Execution Session.
    pub fn set_kept_session_resolver(&self, resolver: KeptSessionResolver) {
        *self.kept_session.lock().unwrap_or_else(|e| e.into_inner()) = Some(resolver);
    }

    /// The local user on this device (every RPC `WorkOutlineMutate` call).
    pub fn local_user(&self) -> WorkOutlineActor {
        WorkOutlineActor {
            kind: WorkOutlineActorKind::User,
            device_id: self.workspace.device_id().to_owned(),
            chat_id: None,
            message_id: None,
        }
    }

    /// An agent acting for `chat_id` on this device (optionally from one
    /// message), the actor agent tools pass to [`Self::mutate_as`].
    pub fn agent_actor(&self, chat_id: &str, message_id: Option<&str>) -> WorkOutlineActor {
        WorkOutlineActor {
            kind: WorkOutlineActorKind::Agent,
            device_id: self.workspace.device_id().to_owned(),
            chat_id: Some(chat_id.to_owned()),
            message_id: message_id.map(str::to_owned),
        }
    }

    /// Handle one `WorkOutlineMutate` RPC call: the local user's write.
    pub fn mutate(
        &self,
        mutation: &WorkOutlineMutation,
    ) -> Result<WorkOutlineMutateReply, WorkOutlineError> {
        self.mutate_as(mutation, &self.local_user())
    }

    /// Whether an agent acting for Chat `actor_chat_id` may perform
    /// `mutation` (contract section 8), from the local replica: its assigned
    /// item and descendants for children and status (`active` / `completed`
    /// only, never `resolveOpenRequests`), associating only its own Chat
    /// within its Space, requests on its assigned item with itself as origin
    /// and resolving requests it raised, and proposals for its item, its
    /// parent, or its Space root. Everything else is `unauthorized`.
    /// [`Self::mutate_as`] re-checks under the registry lock, so this is a
    /// preflight for agent tools, not a substitute.
    pub fn authorize_agent(
        &self,
        actor_chat_id: &str,
        mutation: &WorkOutlineMutation,
    ) -> Result<(), WorkOutlineError> {
        self.workspace
            .authorize_agent_work_outline(actor_chat_id, mutation)
    }

    /// What an agent acting for `chat_id` may read: its assigned item (as
    /// materialized, with effective status and rollups), the item's effective
    /// instructions, and the open requests the Chat raised. `unauthorized`
    /// when the Chat is missing, projectless, or not associated with a live
    /// item.
    pub fn agent_context(
        &self,
        chat_id: &str,
    ) -> Result<zeron_proto::AgentWorkItemContext, WorkOutlineError> {
        let denied = |message: &str| WorkOutlineError::unauthorized(message);
        let chat = self
            .workspace
            .chat(chat_id)
            .ok()
            .flatten()
            .ok_or_else(|| denied("The agent's Chat no longer exists"))?;
        let space_id = chat
            .space_id
            .filter(|s| !s.is_empty())
            .ok_or_else(|| denied("The agent's Chat is not in a project"))?;
        let snapshot = self.snapshot(&space_id);
        self.agent_context_in(chat_id, &space_id, &snapshot)
    }

    /// [`Self::agent_context`] over a snapshot the caller already built for
    /// `space_id` (the Chat's Space), so one read materializes once.
    pub fn agent_context_in(
        &self,
        chat_id: &str,
        space_id: &str,
        snapshot: &WorkOutlineSnapshot,
    ) -> Result<zeron_proto::AgentWorkItemContext, WorkOutlineError> {
        let denied = |message: &str| WorkOutlineError::unauthorized(message);
        let work_item_id = snapshot
            .assignments
            .iter()
            .find(|a| {
                a.chat_id == chat_id
                    && a.state == zeron_proto::WorkItemChatAssignmentState::Associated
            })
            .and_then(|a| a.work_item_id.clone())
            .ok_or_else(|| denied("The agent's Chat is not associated with a work item"))?;
        let work_item = snapshot
            .item(&work_item_id)
            .cloned()
            .ok_or_else(|| denied("The agent's Chat is not associated with a work item"))?;
        Ok(zeron_proto::AgentWorkItemContext {
            chat_id: chat_id.to_owned(),
            effective_instructions: snapshot.effective_instructions(&work_item_id),
            open_user_action_requests: snapshot
                .user_action_requests
                .iter()
                .filter(|r| r.chat_id == chat_id && r.is_open())
                .cloned()
                .collect(),
            space_id: space_id.to_owned(),
            work_item,
        })
    }

    /// Apply one mutation as `actor` (kill switch, replay ledger, then one
    /// registry write). Agent actors (`kind: agent`, `chatId` required) are
    /// authorized under the registry lock by the doc layer; user actors are
    /// unchanged.
    pub fn mutate_as(
        &self,
        mutation: &WorkOutlineMutation,
        actor: &WorkOutlineActor,
    ) -> Result<WorkOutlineMutateReply, WorkOutlineError> {
        if writes_disabled() {
            return Err(WorkOutlineError::new(
                WorkOutlineErrorCode::WritesDisabled,
                "Work Outline changes are turned off on this device",
            ));
        }
        let operation_id = mutation.operation_id().to_owned();
        let hash = zeron_doc::registry::work_outline::inputs_hash(mutation);
        let conflict = || {
            WorkOutlineError::new(
                WorkOutlineErrorCode::OperationConflict,
                "operationId was already used for a different change",
            )
        };
        let mut recent = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((stored, reply)) = recent.get(&operation_id) {
            return if *stored == hash {
                Ok(reply.clone())
            } else {
                Err(conflict())
            };
        }
        let operation_key = ledger_operation_key(&operation_id);
        let inputs_key = ledger_inputs_key(&hash, &operation_id);
        if self.workspace.ledger_holds(&operation_key) {
            if !self.workspace.ledger_holds(&inputs_key) {
                return Err(conflict());
            }
            let reply = replay_reply(mutation);
            recent.insert(operation_id, hash, reply.clone());
            return Ok(reply);
        }
        let keys = [operation_key, inputs_key];
        let reply = match mutation {
            // The target is the host's delivery record, read before the
            // registry lock (it may touch the journal sidecar on disk).
            WorkOutlineMutation::KeepInstructionSession(p) => {
                let resolver = self
                    .kept_session
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let kept = resolver.and_then(|resolve| resolve(&p.chat_id));
                self.workspace
                    .work_outline_keep_session(p, kept, actor, &keys)?
            }
            _ => self.workspace.work_outline_mutate(mutation, actor, &keys)?,
        };
        recent.insert(operation_id, hash, reply.clone());
        Ok(reply)
    }

    /// Handle one `WorkOutlineQuery` call.
    ///
    /// - `resolveEffectiveInstructions`: the item's effective instructions
    ///   over this Space's materialized (repaired) tree; `notFound` for an
    ///   unknown item or a suppressed Space.
    /// - `getInstructionRevision`: the revision with its integrity flag (a
    ///   corrupt revision is returned flagged, not as an error).
    /// - `compareInstructionRevisions`: both revisions must exist and verify
    ///   (`revisionIntegrity` otherwise).
    pub fn query(
        &self,
        query: &WorkOutlineQueryRequest,
    ) -> Result<WorkOutlineQueryResponse, WorkOutlineError> {
        let not_found = |what: &str| {
            WorkOutlineError::new(WorkOutlineErrorCode::NotFound, format!("{what} not found"))
        };
        let revision = |id: &str| {
            self.workspace
                .instruction_revision(id)
                .ok_or_else(|| not_found("Instruction revision"))
        };
        match query {
            WorkOutlineQueryRequest::ResolveEffectiveInstructions { work_item_id } => {
                let space_id = self
                    .workspace
                    .work_item_space(work_item_id)
                    .ok_or_else(|| not_found("Work item"))?;
                let snapshot = self.snapshot(&space_id);
                if snapshot.item(work_item_id).is_none() {
                    return Err(not_found("Work item"));
                }
                Ok(WorkOutlineQueryResponse::ResolveEffectiveInstructions(
                    snapshot.effective_instructions(work_item_id),
                ))
            }
            WorkOutlineQueryRequest::ResolveExecutionConfiguration { scope } => {
                Ok(WorkOutlineQueryResponse::ResolveExecutionConfiguration(
                    self.workspace.resolve_execution(scope)?,
                ))
            }
            WorkOutlineQueryRequest::GetInstructionRevision { revision_id } => Ok(
                WorkOutlineQueryResponse::GetInstructionRevision(revision(revision_id)?),
            ),
            WorkOutlineQueryRequest::CompareInstructionRevisions {
                source_revision_id,
                target_revision_id,
            } => {
                let source = revision(source_revision_id)?;
                let target = revision(target_revision_id)?;
                for view in [&source, &target] {
                    if view.integrity != InstructionRevisionIntegrity::Ok {
                        return Err(WorkOutlineError::new(
                            WorkOutlineErrorCode::RevisionIntegrity,
                            format!(
                                "Instruction revision {} failed its integrity check",
                                view.id
                            ),
                        ));
                    }
                }
                let comparison = compare_instruction_revisions(&source.body, &target.body)?;
                Ok(WorkOutlineQueryResponse::CompareInstructionRevisions {
                    source_revision_id: source_revision_id.clone(),
                    target_revision_id: target_revision_id.clone(),
                    comparison,
                })
            }
        }
    }

    /// Capture the resolved execution configuration a Chat launches with
    /// (contract 6.2; written once, keyed by Chat id). The start workflow
    /// calls this with its `operationId` before queueing the first Turn.
    /// Replay-safe without the ledger: the row itself records the operation
    /// id and an inputs hash, so a retry with the same id and snapshot is a
    /// no-op on any device, while another id or snapshot is
    /// `operationConflict`. Honors the kill switch. Returns whether a write
    /// was enqueued.
    pub fn capture_chat_execution_binding(
        &self,
        chat_id: &str,
        resolved: &zeron_proto::ResolvedExecutionConfiguration,
        operation_id: &str,
    ) -> Result<bool, WorkOutlineError> {
        if writes_disabled() {
            return Err(WorkOutlineError::new(
                WorkOutlineErrorCode::WritesDisabled,
                "Work Outline changes are turned off on this device",
            ));
        }
        let actor = WorkOutlineActor {
            kind: WorkOutlineActorKind::User,
            device_id: self.workspace.device_id().to_owned(),
            chat_id: None,
            message_id: None,
        };
        self.workspace
            .capture_chat_execution_binding(chat_id, resolved, operation_id, &actor)
    }

    /// The current snapshot of one Space (revision 0).
    pub fn snapshot(&self, space_id: &str) -> WorkOutlineSnapshot {
        let inputs = self.workspace.work_outline_inputs(space_id);
        zeron_doc::materialize_work_outline(space_id, &inputs)
    }

    /// `WatchWorkOutline`: the current snapshot first, then one frame per
    /// content change. Ends when the registry host goes away.
    pub fn watch(&self, space_id: String) -> BoxStream<'static, serde_json::Value> {
        struct State {
            workspace: WorkspaceHost,
            changes: tokio::sync::watch::Receiver<u64>,
            space_id: String,
            last: Option<WorkOutlineSnapshot>,
            /// Inputs marker of the last recompute.
            marker: Option<(u64, bool)>,
            revision: u64,
        }
        let state = State {
            changes: self.workspace.watch_changes(),
            workspace: self.workspace.clone(),
            space_id,
            last: None,
            marker: None,
            revision: 0,
        };
        futures::stream::unfold(state, |mut state| async move {
            loop {
                if state.last.is_some() {
                    state.changes.changed().await.ok()?;
                    tokio::time::sleep(COALESCE).await;
                    state.changes.borrow_and_update();
                    // Nothing the outline reads changed (devices, pins,
                    // seen markers, …): no recompute.
                    if state.marker == Some(state.workspace.work_outline_inputs_marker()) {
                        continue;
                    }
                }
                let workspace = state.workspace.clone();
                let space_id = state.space_id.clone();
                let (marker, mut snapshot) = tokio::task::spawn_blocking(move || {
                    let (marker, inputs) = workspace.work_outline_inputs_marked(&space_id);
                    (
                        marker,
                        zeron_doc::materialize_work_outline(&space_id, &inputs),
                    )
                })
                .await
                .ok()?;
                state.marker = Some(marker);
                if state
                    .last
                    .as_ref()
                    .is_some_and(|last| last.same_content_as(&snapshot))
                {
                    continue;
                }
                state.revision += 1;
                snapshot.revision = state.revision;
                let value = serde_json::to_value(&snapshot).ok()?;
                state.last = Some(snapshot);
                return Some((value, state));
            }
        })
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_operations_evict_oldest_first() {
        let mut recent = RecentOperations::default();
        for i in 0..RECENT_OPERATIONS + 1 {
            recent.insert(
                format!("op-{i}"),
                "h".into(),
                WorkOutlineMutateReply::default(),
            );
        }
        assert!(recent.get("op-0").is_none());
        assert!(recent.get("op-1").is_some());
        assert_eq!(recent.replies.len(), RECENT_OPERATIONS);
    }
}
