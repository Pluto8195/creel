//! Work Outline service: the policy layer over the registry mutators in
//! `zeron_doc::registry::work_outline` (contract `docs/plans/work-outline/
//! m0-contract.md`, registry audit section 7.11).
//!
//! - `WorkOutlineMutate`: kill switch, operation-id validation, a
//!   device-local operation replay cache (same id + same inputs returns the
//!   first reply without writing; same id + other inputs is
//!   `OperationConflict`), actor derivation, then one registry write. The
//!   mechanism checks (live Space, same Space, projectless, cycles, sizes,
//!   create-if-absent) run in the doc layer under the registry lock, so the
//!   check and the write are atomic.
//! - `WatchWorkOutline(spaceId)`: driven by the registry change counter
//!   (WATCH_PREVIEWS pattern): clone inputs under the lock, materialize off
//!   the async runtime, coalesce bursts, emit only frames whose content
//!   changed, stamp a per-stream monotone `revision`.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use zeron_proto::work_outline::{DISABLE_WRITES_ENV, WorkOutlineErrorCode};
use zeron_proto::{
    WorkOutlineActor, WorkOutlineActorKind, WorkOutlineError, WorkOutlineMutateReply,
    WorkOutlineMutation, WorkOutlineSnapshot,
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

/// The engine's Work Outline surface (one per `EngineRpc`).
pub struct WorkOutlineService {
    workspace: WorkspaceHost,
    recent: Mutex<RecentOperations>,
}

impl WorkOutlineService {
    pub fn new(workspace: WorkspaceHost) -> Self {
        Self {
            workspace,
            recent: Mutex::new(RecentOperations::default()),
        }
    }

    /// Handle one `WorkOutlineMutate` call.
    pub fn mutate(
        &self,
        mutation: &WorkOutlineMutation,
    ) -> Result<WorkOutlineMutateReply, WorkOutlineError> {
        if writes_disabled() {
            return Err(WorkOutlineError::new(
                WorkOutlineErrorCode::WritesDisabled,
                "Work Outline changes are turned off on this device",
            ));
        }
        let operation_id = mutation.operation_id().to_owned();
        let hash = zeron_doc::registry::work_outline::inputs_hash(mutation);
        {
            let recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((stored, reply)) = recent.get(&operation_id) {
                return if *stored == hash {
                    Ok(reply.clone())
                } else {
                    Err(WorkOutlineError::new(
                        WorkOutlineErrorCode::OperationConflict,
                        "operationId was already used for a different change",
                    ))
                };
            }
        }
        // The RPC surface cannot tell agents from users yet (Milestone 4 adds
        // per-run tokens); every Milestone 1 write is the local user's.
        let actor = WorkOutlineActor {
            kind: WorkOutlineActorKind::User,
            device_id: self.workspace.device_id().to_owned(),
            chat_id: None,
            message_id: None,
        };
        let reply = self.workspace.work_outline_mutate(mutation, &actor)?;
        self.recent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(operation_id, hash, reply.clone());
        Ok(reply)
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
