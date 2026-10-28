//! The linked Linear item (read-only from Linear): one work item per Space
//! (`setLinearAssignedDestination`) whose children are created automatically
//! for Linear tickets that are assigned to the viewer, not started (state type
//! backlog, unstarted or triage) and in no project.
//!
//! Runs inside the ordinary Linear sync pass (`linear::sync::sync_space`), so
//! it shares its cadence (15 minutes, on refresh, when the destination is set)
//! and the SWR-cached list (`LinearService::assigned_unstarted_issues`), and
//! only for Spaces with Linear on. Every write is the `system` actor through
//! the ordinary mutation path; nothing is written to Linear.
//!
//! Per matching ticket:
//! - linked to any item that is not this sync's own (a feature item, a manual
//!   link, an item the user archived): nothing, so no duplicate;
//! - not linked: a child `"ENG-123 · title"` is created last under the
//!   destination (`createWorkItemFromLinear`, id derived from Space and
//!   identifier) with a `linear`/`issue` reference and a sync record;
//! - already ours and live: its marker is cleared; ours and archived by this
//!   sync: restored (the ticket matches again).
//!
//! Per own child whose ticket stopped matching (started, completed, canceled,
//! reassigned, moved to a project, as read from `issue`):
//! - **touched** (any Chat, child item, instruction, title/description edit,
//!   or a status other than the one the sync wrote): never archived or
//!   deleted; its record gets a marker ("Started in Linear", "Moved to
//!   project X", …) that the outline shows;
//! - **untouched**: archived (`archiveWorkItem`, system actor), with the
//!   reason and `archivedByLinearSync` recorded.
//!
//! A failed list fetch changes nothing. Items the user archived or that
//! carry no sync record are never touched.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use zeron_proto::linear::{
    LINEAR_ISSUE_REFERENCE, LINEAR_PROVIDER, LinearError, LinearErrorCode, LinearIssueLink,
    LinearIssueSummary, LinearStateType,
};
use zeron_proto::pr_review_sync::WorkItemExternalSyncState;
use zeron_proto::work_outline::{
    ArchiveWorkItemParams, CreateWorkItemFromLinearParams, DescendantArchivePolicy,
    RestoreWorkItemParams, UpdateWorkItemParams, WorkItemArchiveCause, WorkItemChanges,
    WorkItemStatus, WorkOutlineActorKind, WorkOutlinePlacement, derive_id, roles,
};
use zeron_proto::{WorkItemView, WorkOutlineMutation, WorkOutlineSnapshot};

use super::LinearService;
use crate::work_outline::WorkOutlineService;

/// Operation id of the child created for `identifier` in `space_id`
/// (hashed: a Space id plus an identifier could exceed the id cap).
fn create_operation(space_id: &str, identifier: &str) -> String {
    format!(
        "linear-assigned-{}",
        derive_id(&format!("{space_id}\0{identifier}"), "linear-assigned-item")
    )
}

fn archive_operation(space_id: &str, identifier: &str, reason: &str) -> String {
    format!(
        "linear-assigned-{}",
        derive_id(
            &format!("{space_id}\0{identifier}\0{reason}"),
            "linear-assigned-archive"
        )
    )
}

/// The derived id of the child the sync creates for `identifier`.
pub fn assigned_item_id(space_id: &str, identifier: &str) -> String {
    derive_id(&create_operation(space_id, identifier), roles::ITEM)
}

fn key(identifier: &str) -> String {
    identifier.trim().to_ascii_uppercase()
}

/// Why `issue` is no longer in the set, `None` while it still matches.
pub fn leave_reason(issue: &LinearIssueSummary, viewer_id: &str) -> Option<String> {
    if issue.is_assigned_unstarted(viewer_id) {
        return None;
    }
    Some(match issue.state.state_type {
        LinearStateType::Started => "Started in Linear".to_owned(),
        LinearStateType::Completed => "Completed in Linear".to_owned(),
        LinearStateType::Canceled => "Canceled in Linear".to_owned(),
        _ if issue.assignee.as_ref().is_none_or(|a| a.id != viewer_id) => {
            "Reassigned in Linear".to_owned()
        }
        _ => match &issue.project {
            Some(project) if !project.name.trim().is_empty() => {
                format!("Moved to project {}", project.name.trim())
            }
            Some(_) => "Moved to a project".to_owned(),
            None => "No longer matches in Linear".to_owned(),
        },
    })
}

/// Whether anyone but the sync has changed `item`: a Chat, a child item, an
/// instruction, an edited title or description, or a status other than the
/// one the sync wrote. Touched items are never archived by the sync.
pub fn is_touched(
    snapshot: &WorkOutlineSnapshot,
    item: &WorkItemView,
    record: &WorkItemExternalSyncState,
) -> bool {
    !item.chat_ids.is_empty()
        || !item.child_ids.is_empty()
        || !item.external_sync_modified_fields.is_empty()
        || item.status != record.synced_status
        || snapshot
            .instructions
            .iter()
            .any(|i| i.work_item_id.as_deref() == Some(item.id.as_str()) && i.archived_at.is_none())
}

/// The sync's own children: items with a Linear sync record, or the id the
/// sync derives for their ticket. Keyed by identifier.
fn own_items(snapshot: &WorkOutlineSnapshot, space_id: &str) -> BTreeMap<String, String> {
    let mut own = BTreeMap::new();
    for reference in &snapshot.external_references {
        if reference.provider != LINEAR_PROVIDER
            || reference.reference_type != LINEAR_ISSUE_REFERENCE
        {
            continue;
        }
        let Some(item) = snapshot.item(&reference.work_item_id) else {
            continue;
        };
        let recorded = item
            .external_sync_state
            .as_ref()
            .is_some_and(|s| s.provider == LINEAR_PROVIDER);
        let identifier = key(&reference.external_id);
        if recorded || assigned_item_id(space_id, &identifier) == item.id {
            own.insert(identifier, item.id.clone());
        }
    }
    own
}

/// Every identifier linked by any live or archived item.
fn linked_identifiers(snapshot: &WorkOutlineSnapshot) -> BTreeSet<String> {
    snapshot
        .external_references
        .iter()
        .filter(|r| r.provider == LINEAR_PROVIDER && r.reference_type == LINEAR_ISSUE_REFERENCE)
        .filter(|r| snapshot.item(&r.work_item_id).is_some())
        .map(|r| key(&r.external_id))
        .collect()
}

fn is_fatal(error: &LinearError) -> bool {
    matches!(
        error.code,
        LinearErrorCode::MissingKey | LinearErrorCode::Unauthorized
    )
}

struct Pass<'a> {
    work_outline: &'a Arc<WorkOutlineService>,
    space_id: &'a str,
}

impl Pass<'_> {
    async fn mutate(&self, mutation: WorkOutlineMutation) -> bool {
        let service = self.work_outline.clone();
        let result = tokio::task::spawn_blocking(move || {
            let actor = service.system_actor();
            service.mutate_as(&mutation, &actor)
        })
        .await;
        match result {
            Ok(Ok(_)) => true,
            Ok(Err(error)) => {
                tracing::warn!(code = error.code.as_str(), "linear assigned: write failed");
                false
            }
            Err(error) => {
                tracing::warn!(%error, "linear assigned: write task failed");
                false
            }
        }
    }

    async fn record(&self, item_id: &str, state: WorkItemExternalSyncState) -> bool {
        self.mutate(WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: item_id.to_owned(),
            changes: WorkItemChanges {
                external_sync_state: Some(state),
                ..Default::default()
            },
            operation_id: uuid::Uuid::new_v4().to_string(),
        }))
        .await
    }
}

fn record_for(identifier: &str, now: i64) -> WorkItemExternalSyncState {
    WorkItemExternalSyncState {
        provider: LINEAR_PROVIDER.into(),
        external_id: identifier.to_owned(),
        round: 1,
        synced_status: WorkItemStatus::Pending,
        round_started_at: now,
        synced_at: now,
        ..Default::default()
    }
}

/// Run one pass for `space_id` (`snapshot` is its current outline). `Ok(0)`
/// and no Linear call when the Space has no linked Linear item. Returns how
/// many items were created, restored, marked or archived.
pub async fn sync(
    linear: &LinearService,
    work_outline: &Arc<WorkOutlineService>,
    snapshot: &WorkOutlineSnapshot,
    space_id: &str,
    force: bool,
) -> Result<u32, LinearError> {
    let Some(destination) = snapshot.linear_assigned_destination() else {
        return Ok(0);
    };
    if !snapshot
        .item(destination)
        .is_some_and(|item| item.archived_at.is_none())
    {
        return Ok(0);
    }
    let Some(viewer) = linear.status(force).await?.viewer else {
        return Ok(0);
    };
    let matching = match linear.assigned_unstarted_issues(force).await {
        Ok(issues) => issues,
        Err(error) if is_fatal(&error) => return Err(error),
        Err(error) => {
            tracing::debug!(code = error.code.as_str(), "linear assigned: list skipped");
            return Ok(0);
        }
    };
    let matching: Vec<LinearIssueSummary> = matching
        .into_iter()
        .filter(|issue| issue.is_assigned_unstarted(&viewer.id))
        .collect();
    let matching_keys: BTreeSet<String> = matching.iter().map(|i| key(&i.identifier)).collect();
    let own = own_items(snapshot, space_id);
    let linked = linked_identifiers(snapshot);
    let pass = Pass {
        work_outline,
        space_id,
    };
    let now = crate::now_ms();
    let mut changed = 0;

    for issue in &matching {
        let identifier = key(&issue.identifier);
        match own.get(&identifier).and_then(|id| snapshot.item(id)) {
            Some(item) => {
                changed += u32::from(restore_or_clear(&pass, item, &identifier, now).await);
            }
            None if linked.contains(&identifier) => {}
            None => changed += u32::from(create(&pass, destination, issue, now).await),
        }
    }

    for (identifier, item_id) in &own {
        if matching_keys.contains(identifier) {
            continue;
        }
        let Some(item) = snapshot.item(item_id) else {
            continue;
        };
        if item.archived_at.is_some() {
            continue;
        }
        let issue = match linear.issue(identifier, force).await {
            Ok(issue) => issue,
            Err(error) if is_fatal(&error) => return Err(error),
            Err(_) => continue,
        };
        let Some(reason) = leave_reason(&issue, &viewer.id) else {
            continue;
        };
        changed += u32::from(leave(&pass, snapshot, item, identifier, &reason, now).await);
    }
    Ok(changed)
}

async fn create(pass: &Pass<'_>, destination: &str, issue: &LinearIssueSummary, now: i64) -> bool {
    let identifier = key(&issue.identifier);
    let created = pass
        .mutate(WorkOutlineMutation::CreateWorkItemFromLinear(
            CreateWorkItemFromLinearParams {
                space_id: pass.space_id.to_owned(),
                parent_work_item_id: Some(destination.to_owned()),
                placement: WorkOutlinePlacement::Last,
                issue: LinearIssueLink {
                    id: issue.id.clone(),
                    identifier: identifier.clone(),
                    title: issue.title.clone(),
                    description: issue.description.clone(),
                    url: issue.url.clone(),
                },
                operation_id: create_operation(pass.space_id, &identifier),
            },
        ))
        .await;
    if !created {
        return false;
    }
    let item_id = assigned_item_id(pass.space_id, &identifier);
    pass.record(&item_id, record_for(&identifier, now)).await;
    true
}

/// A matching own child: restore one this sync archived, clear a marker.
async fn restore_or_clear(
    pass: &Pass<'_>,
    item: &WorkItemView,
    identifier: &str,
    now: i64,
) -> bool {
    let record = item
        .external_sync_state
        .clone()
        .unwrap_or_else(|| record_for(identifier, now));
    if item.archived_at.is_some() {
        let by_sync = record.archived_by_linear_sync
            && item.archive_cause == Some(WorkItemArchiveCause::SelfArchived)
            && item
                .archived_by
                .as_ref()
                .is_some_and(|actor| actor.kind == WorkOutlineActorKind::System);
        if !by_sync {
            return false;
        }
        let restored = pass
            .mutate(WorkOutlineMutation::RestoreWorkItem(
                RestoreWorkItemParams {
                    work_item_id: item.id.clone(),
                    operation_id: uuid::Uuid::new_v4().to_string(),
                },
            ))
            .await;
        if !restored {
            return false;
        }
    } else if record.marker.is_none() && item.external_sync_state.is_some() {
        return false;
    }
    let cleared = WorkItemExternalSyncState {
        marker: None,
        archived_by_linear_sync: false,
        synced_at: now,
        ..record
    };
    pass.record(&item.id, cleared).await;
    true
}

/// An own, live child whose ticket stopped matching.
async fn leave(
    pass: &Pass<'_>,
    snapshot: &WorkOutlineSnapshot,
    item: &WorkItemView,
    identifier: &str,
    reason: &str,
    now: i64,
) -> bool {
    let record = item
        .external_sync_state
        .clone()
        .unwrap_or_else(|| record_for(identifier, now));
    if is_touched(snapshot, item, &record) {
        if record.marker.as_deref() == Some(reason) {
            return false;
        }
        let marked = WorkItemExternalSyncState {
            marker: Some(reason.to_owned()),
            synced_at: now,
            ..record
        };
        return pass.record(&item.id, marked).await;
    }
    let archived = pass
        .mutate(WorkOutlineMutation::ArchiveWorkItem(
            ArchiveWorkItemParams {
                work_item_id: item.id.clone(),
                descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
                operation_id: archive_operation(pass.space_id, identifier, reason),
            },
        ))
        .await;
    if !archived {
        return false;
    }
    let marked = WorkItemExternalSyncState {
        marker: Some(reason.to_owned()),
        archived_by_linear_sync: true,
        synced_at: now,
        ..record
    };
    pass.record(&item.id, marked).await;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeron_proto::linear::{LinearProjectRef, LinearState, LinearUser};

    fn issue(state: LinearStateType, assignee: &str, project: Option<&str>) -> LinearIssueSummary {
        LinearIssueSummary {
            identifier: "ENG-1".into(),
            state: LinearState {
                state_type: state,
                ..Default::default()
            },
            assignee: Some(LinearUser {
                id: assignee.into(),
                ..Default::default()
            }),
            project: project.map(|name| LinearProjectRef {
                id: "p".into(),
                name: name.into(),
                url: String::new(),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn matching_needs_me_an_unstarted_state_and_no_project() {
        for state in [
            LinearStateType::Backlog,
            LinearStateType::Unstarted,
            LinearStateType::Triage,
        ] {
            assert!(issue(state, "me", None).is_assigned_unstarted("me"));
            assert!(leave_reason(&issue(state, "me", None), "me").is_none());
        }
        for state in [
            LinearStateType::Started,
            LinearStateType::Completed,
            LinearStateType::Canceled,
            LinearStateType::Unknown,
        ] {
            assert!(!issue(state, "me", None).is_assigned_unstarted("me"));
        }
        assert!(!issue(LinearStateType::Backlog, "you", None).is_assigned_unstarted("me"));
        assert!(!issue(LinearStateType::Backlog, "me", Some("P")).is_assigned_unstarted("me"));
        let mut unassigned = issue(LinearStateType::Backlog, "me", None);
        unassigned.assignee = None;
        assert!(!unassigned.is_assigned_unstarted("me"));
    }

    #[test]
    fn leave_reasons_name_what_changed() {
        let reason = |i: LinearIssueSummary| leave_reason(&i, "me").unwrap();
        assert_eq!(
            reason(issue(LinearStateType::Started, "me", None)),
            "Started in Linear"
        );
        assert_eq!(
            reason(issue(LinearStateType::Completed, "me", None)),
            "Completed in Linear"
        );
        assert_eq!(
            reason(issue(LinearStateType::Canceled, "me", None)),
            "Canceled in Linear"
        );
        assert_eq!(
            reason(issue(LinearStateType::Backlog, "you", None)),
            "Reassigned in Linear"
        );
        assert_eq!(
            reason(issue(LinearStateType::Backlog, "me", Some("Auth"))),
            "Moved to project Auth"
        );
    }

    #[test]
    fn derived_ids_fit_the_operation_id_cap() {
        let long = "s".repeat(200);
        assert!(create_operation(&long, "ENG-123456").len() <= 64);
        assert!(archive_operation(&long, "ENG-123456", "Moved to project Something").len() <= 64);
    }
}
