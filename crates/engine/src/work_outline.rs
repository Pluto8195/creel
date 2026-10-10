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
//! - `WatchWorkOutline(spaceId)`: driven by the registry change counter
//!   (WATCH_PREVIEWS pattern): clone inputs under the lock, materialize off
//!   the async runtime, coalesce bursts, emit only frames whose content
//!   changed, stamp a per-stream monotone `revision`.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use zeron_proto::work_outline::{
    DISABLE_WRITES_ENV, WorkOutlineErrorCode, derive_id, external_reference_id, roles,
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

/// The answer to instruction operations and queries until the instruction
/// service ships.
fn instructions_unsupported() -> WorkOutlineError {
    WorkOutlineError::new(
        WorkOutlineErrorCode::Unsupported,
        "Work Outline instructions are not supported by this version of Zeron yet",
    )
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
        _ => WorkOutlineMutateReply::default(),
    }
}

/// The engine's Work Outline surface (one per `EngineRpc`).
pub struct WorkOutlineService {
    workspace: WorkspaceHost,
    /// Serializes check-then-apply so two concurrent calls with one
    /// operation id cannot both pass the replay check.
    gate: Mutex<RecentOperations>,
}

impl WorkOutlineService {
    pub fn new(workspace: WorkspaceHost) -> Self {
        Self {
            workspace,
            gate: Mutex::new(RecentOperations::default()),
        }
    }

    /// Handle one `WorkOutlineMutate` call.
    pub fn mutate(
        &self,
        mutation: &WorkOutlineMutation,
    ) -> Result<WorkOutlineMutateReply, WorkOutlineError> {
        // Milestone 2 instruction operations: the wire contract exists, the
        // service does not yet. Refuse before touching the replay ledger so a
        // later build can apply the same operation id.
        if mutation.is_instruction_operation() {
            return Err(instructions_unsupported());
        }
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
        // The RPC surface cannot tell agents from users yet (Milestone 4 adds
        // per-run tokens); every Milestone 1 write is the local user's.
        let actor = WorkOutlineActor {
            kind: WorkOutlineActorKind::User,
            device_id: self.workspace.device_id().to_owned(),
            chat_id: None,
            message_id: None,
        };
        let reply =
            self.workspace
                .work_outline_mutate(mutation, &actor, &[operation_key, inputs_key])?;
        recent.insert(operation_id, hash, reply.clone());
        Ok(reply)
    }

    /// Handle one `WorkOutlineQuery` call. Not served yet (Milestone 2 stage
    /// 1 ships the wire contract only).
    pub fn query(
        &self,
        _query: &WorkOutlineQueryRequest,
    ) -> Result<WorkOutlineQueryResponse, WorkOutlineError> {
        Err(instructions_unsupported())
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
            revision: u64,
        }
        let state = State {
            changes: self.workspace.watch_changes(),
            workspace: self.workspace.clone(),
            space_id,
            last: None,
            revision: 0,
        };
        futures::stream::unfold(state, |mut state| async move {
            loop {
                if state.last.is_some() {
                    state.changes.changed().await.ok()?;
                    tokio::time::sleep(COALESCE).await;
                    state.changes.borrow_and_update();
                }
                let inputs = state.workspace.work_outline_inputs(&state.space_id);
                let space_id = state.space_id.clone();
                let mut snapshot = tokio::task::spawn_blocking(move || {
                    zeron_doc::materialize_work_outline(&space_id, &inputs)
                })
                .await
                .ok()?;
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
