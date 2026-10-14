//! Linked-item sync: an issue-linked work item's title
//! (`"ENG-123 · <title>"`) and description follow Linear until the user
//! edits that field (`externalSyncModifiedFields`). Only Spaces with Linear
//! on; only live (non-archived) items; only those two fields, written
//! through the ordinary mutation path as the system actor (the registry
//! re-checks the user's marks under its lock). Status, parent, order,
//! archive state, and the set of items are never touched.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use zeron_proto::linear::{
    LinearError, LinearErrorCode, LinearIssueSummary, SYNC_FIELD_DESCRIPTION, SYNC_FIELD_TITLE,
    linear_item_description, linear_item_title,
};
use zeron_proto::work_outline::{FieldPatch, UpdateWorkItemParams, WorkItemChanges};
use zeron_proto::{WorkOutlineMutation, WorkOutlineSnapshot};

use super::LinearService;
use crate::work_outline::WorkOutlineService;
use crate::workspace_host::WorkspaceHost;

/// Background cadence.
pub const SYNC_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// First background pass after boot (lets the registry sync first).
const FIRST_SYNC_DELAY: Duration = Duration::from_secs(60);

/// The changes that bring `snapshot`'s item `work_item_id` in line with
/// `issue`, honoring the item's modified fields. `None` when in line.
pub fn changes_for(
    snapshot: &WorkOutlineSnapshot,
    work_item_id: &str,
    issue: &LinearIssueSummary,
) -> Option<WorkItemChanges> {
    let item = snapshot.item(work_item_id)?;
    let mut changes = WorkItemChanges::default();
    if item.follows_external_sync(SYNC_FIELD_TITLE) {
        let title = linear_item_title(&issue.identifier, &issue.title);
        if !title.trim().is_empty() && title != item.title {
            changes.title = Some(title);
        }
    }
    if item.follows_external_sync(SYNC_FIELD_DESCRIPTION) {
        let description = linear_item_description(issue.description.as_deref());
        if description != item.description {
            changes.description = match description {
                Some(text) => FieldPatch::Set(text),
                None => FieldPatch::Clear,
            };
        }
    }
    (changes.title.is_some() || !changes.description.is_unchanged()).then_some(changes)
}

/// Sync one Space's issue-linked items. `force` bypasses the issue cache
/// (manual refresh). Returns how many items changed. `disabledForSpace`
/// when the Space has Linear off; `missingKey` / `unauthorized` abort.
pub async fn sync_space(
    linear: &LinearService,
    work_outline: &Arc<WorkOutlineService>,
    space_id: &str,
    force: bool,
) -> Result<u32, LinearError> {
    let snapshot = {
        let service = work_outline.clone();
        let space = space_id.to_owned();
        tokio::task::spawn_blocking(move || service.snapshot(&space))
            .await
            .map_err(|e| LinearError::new(LinearErrorCode::Network, e.to_string()))?
    };
    if !snapshot.available || !snapshot.space_settings.linear_enabled {
        return Err(LinearError::disabled_for_space());
    }
    // Item -> identifier for every live issue-linked item.
    let linked: Vec<(String, String)> = snapshot
        .items
        .iter()
        .filter(|item| item.archived_at.is_none())
        .filter_map(|item| {
            snapshot
                .linear_issue_reference(&item.id)
                .map(|r| (item.id.clone(), r.external_id.clone()))
        })
        .collect();
    if linked.is_empty() {
        return Ok(0);
    }
    let mut issues: BTreeMap<String, Option<LinearIssueSummary>> = BTreeMap::new();
    for (_, identifier) in &linked {
        if issues.contains_key(identifier) {
            continue;
        }
        let fetched = match linear.issue(identifier, force).await {
            Ok(issue) => Some(issue),
            Err(error)
                if matches!(
                    error.code,
                    LinearErrorCode::MissingKey | LinearErrorCode::Unauthorized
                ) =>
            {
                return Err(error);
            }
            Err(error) => {
                tracing::debug!(code = error.code.as_str(), "linear sync: issue skipped");
                None
            }
        };
        issues.insert(identifier.clone(), fetched);
    }
    let mut updated = 0;
    for (work_item_id, identifier) in &linked {
        let Some(Some(issue)) = issues.get(identifier) else {
            continue;
        };
        let Some(changes) = changes_for(&snapshot, work_item_id, issue) else {
            continue;
        };
        let mutation = WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: work_item_id.clone(),
            changes,
            operation_id: uuid::Uuid::new_v4().to_string(),
        });
        let service = work_outline.clone();
        let result = tokio::task::spawn_blocking(move || {
            let actor = service.system_actor();
            service.mutate_as(&mutation, &actor)
        })
        .await;
        match result {
            Ok(Ok(_)) => updated += 1,
            Ok(Err(error)) => {
                tracing::warn!(code = error.code.as_str(), "linear sync: update failed")
            }
            Err(error) => tracing::warn!(%error, "linear sync: update task failed"),
        }
    }
    Ok(updated)
}

/// Sync every Linear-enabled Space of this device's replica (no network
/// and no key read when no Space has Linear on).
pub async fn sync_enabled_spaces(
    linear: &LinearService,
    work_outline: &Arc<WorkOutlineService>,
    workspace: &WorkspaceHost,
) {
    let spaces = workspace.linear_enabled_space_ids();
    if spaces.is_empty() || !linear.has_key().await {
        return;
    }
    for space in spaces {
        match sync_space(linear, work_outline, &space, false).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(space = %space, updated = n, "linear sync: items updated"),
            Err(error) if error.code == LinearErrorCode::DisabledForSpace => {}
            Err(error) => {
                tracing::info!(code = error.code.as_str(), "linear sync: pass stopped");
                return;
            }
        }
    }
}

/// The background loop (held by `EngineCore` for the process lifetime).
pub fn spawn_sync_loop(
    linear: LinearService,
    workspace: WorkspaceHost,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let work_outline = Arc::new(WorkOutlineService::new(workspace.clone()));
        tokio::time::sleep(FIRST_SYNC_DELAY).await;
        loop {
            sync_enabled_spaces(&linear, &work_outline, &workspace).await;
            tokio::time::sleep(SYNC_INTERVAL).await;
        }
    })
}
