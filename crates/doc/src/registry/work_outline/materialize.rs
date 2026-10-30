//! Pure Work Outline materialization (registry audit section 8, steps 0-6, 8,
//! 10 for the Milestone 1 kinds; steps 7 and 9 for instructions, revisions,
//! and applications in Milestone 2). No I/O and no lock: the engine clones the
//! inputs under the registry lock and materializes after releasing it.
//!
//! Output is fully deterministic (every list sorted, no hash-map iteration
//! order leaks), so two materializations of the same inputs are `==` in any
//! input order, and the watch stream can dedupe frames.
//!
//! Rollup counting rule (documented contract of [`WorkItemRollup`]): the
//! pending/active/completed counts cover the item and every descendant that
//! is not archived (an archived item is not counted, its non-archived
//! descendants still are). `associated_chat_count` is the item's own Chats in
//! state `associated`. Milestone 4: an item with at least one unresolved
//! (open, non-deduped) user action request is `effectiveStatus:
//! userActionRequired` (its stored status is unchanged and still counted);
//! `userActionRequiredCount` counts such non-archived items in the subtree,
//! `unresolvedUserActionCount` is the item's own unresolved requests (also
//! for archived items) and `subtreeUnresolvedUserActionCount` sums it over
//! the item and its non-archived descendants (archived items contribute
//! nothing, like the status counts).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use mast_proto::work_outline::{
    WorkItemArchiveCause, WorkItemChatAssignmentState, WorkItemChatAssignmentView,
    WorkItemEffectiveStatus, WorkItemExternalReferenceView, WorkItemRollup, WorkItemStatus,
    WorkItemView, WorkOutlineActor, WorkOutlineRepair, WorkOutlineSnapshot,
};
use mast_proto::work_outline_instructions::{
    ChatInstructionState, ExecutionSessionReport, INSTRUCTION_ENVELOPE_VERSION,
    InstructionApplicationReason, InstructionApplicationResult, InstructionRevisionApplicationView,
    InstructionRevisionBody, InstructionRevisionIntegrity, InstructionRevisionView,
    InstructionTurnBoundary, SelectedRevisionIntegrity, WorkItemInstructionView,
    canonical_revision_hash, instruction_content_key, resolve_effective_instructions,
    verify_revision_body,
};
use mast_proto::work_outline_order::valid_order_key;
use serde_json::Value;

use super::{
    KIND_WORK_ITEM_CHAT_ASSIGNMENTS, KIND_WORK_ITEM_EXTERNAL_REFERENCES,
    KIND_WORK_ITEM_INSTRUCTIONS, KIND_WORK_ITEMS,
};
use crate::registry::RegistryRow;

/// The slice of a Chat row the outline needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineChat {
    pub id: String,
    /// `None` = projectless.
    pub space_id: Option<String>,
    /// The Chat is archived (Milestone 4: an archived origin Chat makes a
    /// request's `originChatAvailable` false).
    pub archived: bool,
}

/// Everything [`materialize_work_outline`] reads, cloned from the overlay
/// view (rows keep their per-field clocks; the cycle rule needs them).
#[derive(Debug, Clone, Default)]
pub struct WorkOutlineInputs {
    /// The Space row is live in the overlay (step 0).
    pub space_live: bool,
    /// Authoritative registry state has been received this boot.
    pub synced: bool,
    /// Live `workItems` rows of EVERY Space (the parent classifier needs the
    /// global index to tell "missing" from "cross-Space").
    pub items: Vec<RegistryRow>,
    pub assignments: Vec<RegistryRow>,
    pub references: Vec<RegistryRow>,
    /// Live Chats of every Space (dangling detection is by id).
    pub chats: Vec<OutlineChat>,
    /// Live `workItemInstructions` rows (every Space; the reader filters).
    pub instructions: Vec<RegistryRow>,
    /// Live `instructionRevisionApplications` rows of this Space.
    pub applications: Vec<RegistryRow>,
    /// Live `instructionRevisions` rows referenced by `applications` or by a
    /// session row's `executionSession` (not every revision: they are only
    /// written, never pruned, and each can be ~14 KB).
    pub revisions: Vec<RegistryRow>,
    /// Live session-status rows that carry an `executionSession` report.
    pub sessions: Vec<RegistryRow>,
    /// Live `executionConfigurations` rows of this Space (Milestone 3).
    pub execution_configurations: Vec<RegistryRow>,
    /// Live `chatExecutionBindings` rows of this Space (Milestone 3).
    pub chat_execution_bindings: Vec<RegistryRow>,
    /// The Space's path and owning device (implicit execution root).
    pub execution_space: Option<mast_proto::work_outline_execution::ExecutionSpace>,
    /// Live `userActionRequests` rows of this Space (Milestone 4).
    pub user_action_requests: Vec<RegistryRow>,
    /// Live `workItemInstructionProposals` rows of this Space (Milestone 4).
    pub instruction_proposals: Vec<RegistryRow>,
    /// The Space's live `workOutlineSpaceSettings` row (Linear), if any.
    pub space_settings: Option<RegistryRow>,
    /// Live `workItemTemplates` rows of this Space (project templates).
    pub templates: Vec<RegistryRow>,
}

// ── lenient row decoding ────────────────────────────────────────────────────

pub(crate) fn str_field(row: &RegistryRow, key: &str) -> Option<String> {
    row.fields
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub(crate) fn i64_field(row: &RegistryRow, key: &str) -> Option<i64> {
    row.fields.get(key).and_then(Value::as_i64)
}

pub(crate) fn actor_field(row: &RegistryRow, key: &str) -> Option<WorkOutlineActor> {
    row.fields
        .get(key)
        .filter(|v| v.is_object())
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// The row's `spaceId`: `Err(Malformed)` when present but not a string,
/// `Err(MissingSpace)` when absent.
pub(crate) fn space_of(row: &RegistryRow, kind: &str) -> Result<String, WorkOutlineRepair> {
    match row.fields.get("spaceId") {
        Some(Value::String(space)) if !space.is_empty() => Ok(space.clone()),
        None | Some(Value::Null) => Err(WorkOutlineRepair::MissingSpace {
            row_kind: kind.into(),
            id: row.id.clone(),
        }),
        Some(_) => Err(WorkOutlineRepair::Malformed {
            row_kind: kind.into(),
            id: row.id.clone(),
        }),
    }
}

/// One `workItems` row, decoded leniently (rule R7): every field degrades on
/// its own; only an undecidable `spaceId` drops the row.
#[derive(Debug, Clone)]
pub(crate) struct DecodedItem {
    pub id: String,
    pub space_id: String,
    pub raw_parent: Option<String>,
    /// Clock of the `parentWorkItemId` field (the cycle-victim key).
    pub parent_clock: Option<String>,
    /// The stored key when it is a string (possibly invalid).
    pub order_key: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: WorkItemStatus,
    pub status_unknown: bool,
    pub inherit_parent_instructions: bool,
    pub archived_at: Option<i64>,
    pub archived_by: Option<WorkOutlineActor>,
    pub archive_cause: Option<WorkItemArchiveCause>,
    pub created_by: Option<WorkOutlineActor>,
    pub created_at: i64,
    pub updated_at: i64,
    /// `externalSyncModifiedFields`, sorted and deduplicated (Linear).
    pub external_sync_modified_fields: Vec<String>,
    /// `externalSyncState` (PR review sync).
    pub external_sync_state: Option<mast_proto::pr_review_sync::WorkItemExternalSyncState>,
    /// `isProject` (project templates).
    pub is_project: bool,
    /// `workerDestinationId` (project templates).
    pub worker_destination_id: Option<String>,
    /// `templateId` / `templateRevision` (project templates).
    pub template_id: Option<String>,
    pub template_revision: Option<u32>,
}

impl DecodedItem {
    pub fn archived(&self) -> bool {
        self.archived_at.is_some()
    }

    /// The stored key if it is a valid order key.
    pub fn valid_key(&self) -> Option<&str> {
        self.order_key.as_deref().filter(|k| valid_order_key(k))
    }
}

pub(crate) fn decode_item(row: &RegistryRow) -> Result<DecodedItem, WorkOutlineRepair> {
    let space_id = space_of(row, KIND_WORK_ITEMS)?;
    let (status, status_unknown) = match row.fields.get("status") {
        None | Some(Value::Null) => (WorkItemStatus::Pending, false),
        Some(Value::String(s)) => match s.as_str() {
            "pending" => (WorkItemStatus::Pending, false),
            "active" => (WorkItemStatus::Active, false),
            "completed" => (WorkItemStatus::Completed, false),
            _ => (WorkItemStatus::Pending, true),
        },
        Some(_) => (WorkItemStatus::Pending, true),
    };
    let archived_at = i64_field(row, "archivedAt");
    let created_at = i64_field(row, "createdAt").unwrap_or(0);
    Ok(DecodedItem {
        id: row.id.clone(),
        space_id,
        raw_parent: str_field(row, "parentWorkItemId").filter(|p| !p.is_empty()),
        parent_clock: row.clocks.get("parentWorkItemId").cloned(),
        order_key: str_field(row, "orderKey"),
        title: str_field(row, "title").filter(|t| !t.trim().is_empty()),
        description: str_field(row, "description"),
        status,
        status_unknown,
        inherit_parent_instructions: row
            .fields
            .get("inheritParentInstructions")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        archived_at,
        archived_by: archived_at.and_then(|_| actor_field(row, "archivedBy")),
        archive_cause: archived_at.map(|_| {
            row.fields
                .get("archiveCause")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default()
        }),
        created_by: actor_field(row, "createdBy"),
        created_at,
        updated_at: i64_field(row, "updatedAt").unwrap_or(created_at),
        external_sync_modified_fields: super::linear::decode_modified_fields(row),
        external_sync_state: super::pr_review_sync::decode_sync_state(row),
        is_project: row
            .fields
            .get("isProject")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        worker_destination_id: str_field(row, "workerDestinationId").filter(|id| !id.is_empty()),
        template_id: str_field(row, "templateId").filter(|id| !id.is_empty()),
        template_revision: row
            .fields
            .get("templateRevision")
            .and_then(Value::as_u64)
            .and_then(|r| u32::try_from(r).ok()),
    })
}

// ── the repaired tree (shared with the write path) ──────────────────────────

/// The repaired hierarchy of one Space: steps 1-5 of the algorithm.
#[derive(Debug, Clone, Default)]
pub(crate) struct OutlineTree {
    /// Items of this Space, by id.
    pub items: BTreeMap<String, DecodedItem>,
    /// Effective (repaired) parent per item.
    pub parent: BTreeMap<String, Option<String>>,
    /// Effective children per parent (`None` = Space root), sorted
    /// `(orderKeyValid? 0 : 1, orderKey, id)`.
    pub children: BTreeMap<Option<String>, Vec<String>>,
    pub repairs: Vec<WorkOutlineRepair>,
}

/// Global `(id -> spaceId)` index entry of an item row (any Space).
pub(crate) fn item_space_index(rows: &[RegistryRow]) -> BTreeMap<String, String> {
    rows.iter()
        .filter_map(|row| {
            space_of(row, KIND_WORK_ITEMS)
                .ok()
                .map(|s| (row.id.clone(), s))
        })
        .collect()
}

/// The deterministic cycle victim: greatest `(clock of parentWorkItemId, id)`
/// (an absent clock sorts lowest). Shared by the reader and the write-time
/// raw-graph validation so both cut the same edge.
pub(crate) fn cycle_victim<'a>(
    members: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
) -> Option<&'a str> {
    members
        .into_iter()
        .max_by(|a, b| (a.1, a.0).cmp(&(b.1, b.0)))
        .map(|(id, _)| id)
}

pub(crate) fn build_tree(space_id: &str, rows: &[RegistryRow]) -> OutlineTree {
    let mut tree = OutlineTree::default();
    let index = item_space_index(rows);
    // Step 1: decode; keep this Space's items. Undecidable rows are listed
    // (they cannot be attributed to a Space, so every snapshot lists them).
    for row in rows {
        match decode_item(row) {
            Ok(item) if item.space_id == space_id => {
                tree.items.insert(item.id.clone(), item);
            }
            Ok(_) => {}
            Err(repair) => tree.repairs.push(repair),
        }
    }
    // Step 2: effective parents.
    for item in tree.items.values() {
        let parent = match item.raw_parent.as_deref() {
            None => None,
            Some(p) => match index.get(p) {
                None => {
                    tree.repairs.push(WorkOutlineRepair::ParentMissing {
                        work_item_id: item.id.clone(),
                        parent_work_item_id: p.into(),
                    });
                    None
                }
                Some(space) if space != space_id => {
                    tree.repairs.push(WorkOutlineRepair::CrossSpaceParent {
                        work_item_id: item.id.clone(),
                        parent_work_item_id: p.into(),
                    });
                    None
                }
                Some(_) => Some(p.to_owned()),
            },
        };
        tree.parent.insert(item.id.clone(), parent);
    }
    // Steps 3-4: cycles on the functional graph, visited in id order.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Color {
        White,
        Grey,
        Black,
    }
    let mut color: BTreeMap<&str, Color> = tree
        .items
        .keys()
        .map(|id| (id.as_str(), Color::White))
        .collect();
    let mut cuts: Vec<(String, String, Vec<String>)> = Vec::new();
    let ids: Vec<&str> = tree.items.keys().map(String::as_str).collect();
    for start in ids {
        if color[start] != Color::White {
            continue;
        }
        let mut path: Vec<&str> = Vec::new();
        let mut cur = start;
        loop {
            color.insert(cur, Color::Grey);
            path.push(cur);
            let Some(next) = tree.parent.get(cur).and_then(|p| p.as_deref()) else {
                break;
            };
            let next = tree.items.get_key_value(next).map(|(k, _)| k.as_str());
            let Some(next) = next else { break };
            match color[next] {
                Color::White => cur = next,
                Color::Black => break,
                Color::Grey => {
                    let at = path.iter().position(|id| *id == next).unwrap_or(0);
                    let members: Vec<&str> = path[at..].to_vec();
                    let victim = cycle_victim(
                        members
                            .iter()
                            .map(|id| (*id, tree.items[*id].parent_clock.as_deref())),
                    )
                    .unwrap_or(next);
                    let former = tree.parent[victim].clone().unwrap_or_default();
                    let mut sorted: Vec<String> = members.iter().map(|s| (*s).to_owned()).collect();
                    sorted.sort();
                    cuts.push((victim.to_owned(), former, sorted));
                    break;
                }
            }
        }
        for id in path {
            color.insert(id, Color::Black);
        }
    }
    for (victim, former_parent, members) in cuts {
        tree.parent.insert(victim.clone(), None);
        tree.repairs.push(WorkOutlineRepair::CycleBroken {
            victim,
            former_parent,
            members,
        });
    }
    // Step 5: children sorted.
    for item in tree.items.values() {
        if item.valid_key().is_none() {
            tree.repairs.push(WorkOutlineRepair::BadOrderKey {
                work_item_id: item.id.clone(),
            });
        }
    }
    for (id, parent) in &tree.parent {
        tree.children
            .entry(parent.clone())
            .or_default()
            .push(id.clone());
    }
    for siblings in tree.children.values_mut() {
        siblings.sort_by(|a, b| sibling_key(&tree.items[a]).cmp(&sibling_key(&tree.items[b])));
    }
    tree.children.entry(None).or_default();
    tree
}

fn sibling_key(item: &DecodedItem) -> (u8, &str, &str) {
    match item.valid_key() {
        Some(key) => (0, key, item.id.as_str()),
        None => (1, "", item.id.as_str()),
    }
}

impl OutlineTree {
    pub fn children_of(&self, parent: Option<&str>) -> &[String] {
        self.children
            .get(&parent.map(str::to_owned))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Every effective descendant of `id` in depth-first pre-order.
    pub fn descendants(&self, id: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack: Vec<&str> = self
            .children_of(Some(id))
            .iter()
            .rev()
            .map(String::as_str)
            .collect();
        while let Some(cur) = stack.pop() {
            out.push(cur.to_owned());
            stack.extend(self.children_of(Some(cur)).iter().rev().map(String::as_str));
        }
        out
    }

    /// Depth-first pre-order of the whole repaired tree.
    fn preorder(&self) -> Vec<(String, u32)> {
        let mut out = Vec::with_capacity(self.items.len());
        let mut stack: Vec<(&str, u32)> = self
            .children_of(None)
            .iter()
            .rev()
            .map(|id| (id.as_str(), 0))
            .collect();
        while let Some((cur, depth)) = stack.pop() {
            out.push((cur.to_owned(), depth));
            stack.extend(
                self.children_of(Some(cur))
                    .iter()
                    .rev()
                    .map(|id| (id.as_str(), depth + 1)),
            );
        }
        out
    }
}

// ── instruction rows (Milestone 2) ──────────────────────────────────────────

/// One `workItemInstructions` row, decoded leniently (R7): only an
/// undecidable `spaceId` drops the row.
pub(crate) fn decode_instruction(
    row: &RegistryRow,
) -> Result<WorkItemInstructionView, WorkOutlineRepair> {
    let space_id = space_of(row, KIND_WORK_ITEM_INSTRUCTIONS)?;
    let archived_at = i64_field(row, "archivedAt");
    let created_at = i64_field(row, "createdAt").unwrap_or(0);
    Ok(WorkItemInstructionView {
        id: row.id.clone(),
        space_id,
        work_item_id: str_field(row, "workItemId").filter(|w| !w.is_empty()),
        text: str_field(row, "text").unwrap_or_default(),
        order_key: str_field(row, "orderKey").unwrap_or_default(),
        created_by: actor_field(row, "createdBy"),
        created_at,
        updated_at: i64_field(row, "updatedAt").unwrap_or(created_at),
        source: row
            .fields
            .get("source")
            .filter(|v| v.is_object())
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
        source_proposal_id: str_field(row, "sourceProposalId").filter(|s| !s.is_empty()),
        archived_at,
        archived_by: archived_at.and_then(|_| actor_field(row, "archivedBy")),
    })
}

fn empty_revision_body() -> InstructionRevisionBody {
    InstructionRevisionBody {
        entries: Vec::new(),
        envelope_version: 0,
        envelope_sha256: String::new(),
        hash: String::new(),
    }
}

/// One `instructionRevisions` row with its integrity verified (registry
/// audit 7.8: recompute on every read). An undecodable body reads as an
/// empty, `corrupt` revision (fail closed).
pub(crate) fn decode_revision(row: &RegistryRow) -> InstructionRevisionView {
    let body = row
        .fields
        .get("body")
        .filter(|v| v.is_object())
        .and_then(|v| serde_json::from_value::<InstructionRevisionBody>(v.clone()).ok());
    let integrity = match &body {
        Some(body) if verify_revision_body(body).is_ok() => InstructionRevisionIntegrity::Ok,
        _ => InstructionRevisionIntegrity::Corrupt,
    };
    InstructionRevisionView {
        id: row.id.clone(),
        space_id: str_field(row, "spaceId").unwrap_or_default(),
        work_item_id: str_field(row, "workItemId").unwrap_or_default(),
        body: body.unwrap_or_else(empty_revision_body),
        created_by: actor_field(row, "createdBy"),
        created_at: i64_field(row, "createdAt").unwrap_or(0),
        integrity,
    }
}

/// One `instructionRevisionApplications` row plus the revision hash the
/// writer verified (`revisionHash`, an additive field: it lets the selection
/// be pinned before the revision row itself has synced).
#[derive(Debug, Clone)]
pub(crate) struct DecodedApplication {
    pub view: InstructionRevisionApplicationView,
    pub revision_hash: Option<String>,
}

/// `None` when the row cannot name a Chat and a revision. A `keepSession`
/// row may name no revision: it selects "no Work Outline instructions".
pub(crate) fn decode_application(row: &RegistryRow) -> Option<DecodedApplication> {
    let chat_id = str_field(row, "chatId").filter(|c| !c.is_empty())?;
    let reason = row
        .fields
        .get("reason")
        .and_then(|v| serde_json::from_value::<InstructionApplicationReason>(v.clone()).ok())
        .unwrap_or_default();
    let revision_id = str_field(row, "revisionId").unwrap_or_default();
    if revision_id.is_empty() && reason != InstructionApplicationReason::KeepSession {
        return None;
    }
    let turn_boundary = row
        .fields
        .get("turnBoundary")
        .filter(|v| v.is_object())
        .and_then(|v| serde_json::from_value::<InstructionTurnBoundary>(v.clone()).ok())
        .unwrap_or_default();
    Some(DecodedApplication {
        view: InstructionRevisionApplicationView {
            id: row.id.clone(),
            space_id: str_field(row, "spaceId").unwrap_or_default(),
            chat_id,
            work_item_id: str_field(row, "workItemId").unwrap_or_default(),
            revision_id,
            session_ref: str_field(row, "sessionRef").filter(|s| !s.is_empty()),
            turn_boundary,
            reason,
            // Only `accepted` is ever written; every stored value reads so.
            result: InstructionApplicationResult::Accepted,
            idempotency_key: str_field(row, "idempotencyKey").unwrap_or_default(),
            applied_hlc: str_field(row, "appliedHlc").unwrap_or_default(),
            created_by: actor_field(row, "createdBy"),
            created_at: i64_field(row, "createdAt").unwrap_or(0),
        },
        revision_hash: str_field(row, "revisionHash").filter(|h| !h.is_empty()),
    })
}

/// What a reader knows about the selected application's revision.
pub(crate) enum RevisionCheck {
    /// Present, in the Chat's Space, and integrity-ok: its canonical hash.
    Valid(String),
    /// Present but corrupt or in another Space: its stored (unverified) hash.
    Corrupt(String),
    /// Not synced (yet) to this replica.
    Missing,
}

/// The newest accepted application of a Chat by `(appliedHlc, id)`.
pub(crate) fn newest_application<'a>(
    applications: impl IntoIterator<Item = &'a DecodedApplication>,
) -> Option<&'a DecodedApplication> {
    applications
        .into_iter()
        .max_by(|a, b| (&a.view.applied_hlc, &a.view.id).cmp(&(&b.view.applied_hlc, &b.view.id)))
}

/// The Chat's selected revision (contract 5.6): the newest accepted
/// application by `(appliedHlc, id)`, never substituted (contract 5.3). Its
/// revision's state is reported instead: `ok`, `missing` (not synced yet),
/// or `corrupt` (fails integrity or belongs to another Space). The pinned
/// hash is the verified revision hash when `ok`, otherwise the hash the
/// application recorded when it was written (falling back to the stored
/// one), so delivery of a missing or corrupt revision fails closed on the
/// integrity check. Returns the application, its integrity, and the hash.
/// A newest `keepSession` application without a revision means the Chat has
/// NO selected revision (`None`), whatever earlier applications selected.
pub(crate) fn select_application<'a>(
    applications: impl IntoIterator<Item = &'a DecodedApplication>,
    check: impl FnOnce(&str) -> RevisionCheck,
) -> Option<(&'a DecodedApplication, SelectedRevisionIntegrity, String)> {
    let application = newest_application(applications)?;
    if application.view.revision_id.is_empty() {
        return None;
    }
    let recorded = application.revision_hash.clone();
    let (integrity, hash) = match check(&application.view.revision_id) {
        RevisionCheck::Valid(hash) => (SelectedRevisionIntegrity::Ok, hash),
        RevisionCheck::Missing => (
            SelectedRevisionIntegrity::Missing,
            recorded.unwrap_or_default(),
        ),
        RevisionCheck::Corrupt(stored) => (
            SelectedRevisionIntegrity::Corrupt,
            recorded.unwrap_or(stored),
        ),
    };
    Some((application, integrity, hash))
}

/// The fields of a materialized item the instruction resolver reads
/// (effective parent, title, inheritance flag), identical to the snapshot's
/// views so a revision written now hashes like the snapshot's current
/// effective hash.
pub(crate) fn resolver_views(tree: &OutlineTree) -> Vec<WorkItemView> {
    tree.items
        .values()
        .map(|item| {
            bare_view(
                &item.id,
                &item.space_id,
                tree.parent.get(&item.id).cloned().flatten(),
                item.title.clone().unwrap_or_else(|| "Untitled".into()),
                item.inherit_parent_instructions,
            )
        })
        .collect()
}

/// A view carrying only what the resolver reads.
pub(crate) fn bare_view(
    id: &str,
    space_id: &str,
    parent_work_item_id: Option<String>,
    title: String,
    inherit_parent_instructions: bool,
) -> WorkItemView {
    WorkItemView {
        id: id.to_owned(),
        space_id: space_id.to_owned(),
        parent_work_item_id,
        raw_parent_work_item_id: None,
        order_key: String::new(),
        title,
        description: None,
        status: WorkItemStatus::Pending,
        effective_status: WorkItemEffectiveStatus::Pending,
        archived_at: None,
        archived_by: None,
        archive_cause: None,
        inherit_parent_instructions,
        created_by: None,
        created_at: 0,
        updated_at: 0,
        child_ids: Vec::new(),
        depth: 0,
        chat_ids: Vec::new(),
        external_reference_ids: Vec::new(),
        rollup: WorkItemRollup::default(),
        external_sync_modified_fields: Vec::new(),
        external_sync_state: None,
        is_project: false,
        worker_destination_id: None,
        template_id: None,
        template_revision: None,
    }
}

/// `(canonical hash, entry count, instruction-content key)` of
/// `work_item_id`'s current effective instructions: the hash a revision
/// created now would carry, and the key drift compares
/// ([`instruction_content_key`]).
pub(crate) fn effective_hash<'a>(
    work_item_id: &str,
    items_by_id: impl Fn(&str) -> Option<&'a WorkItemView>,
    instructions: &[WorkItemInstructionView],
) -> (String, usize, String) {
    let effective = resolve_effective_instructions(work_item_id, items_by_id, instructions);
    let entries = effective.revision_entries();
    (
        canonical_revision_hash(&entries, INSTRUCTION_ENVELOPE_VERSION),
        effective.entries.len(),
        instruction_content_key(&entries),
    )
}

// ── the snapshot ────────────────────────────────────────────────────────────

fn status_effective(status: WorkItemStatus) -> WorkItemEffectiveStatus {
    match status {
        WorkItemStatus::Pending => WorkItemEffectiveStatus::Pending,
        WorkItemStatus::Active => WorkItemEffectiveStatus::Active,
        WorkItemStatus::Completed => WorkItemEffectiveStatus::Completed,
    }
}

/// The composed Work Outline of `space_id` (`revision` is left 0; the
/// stream stamps it).
pub fn materialize_work_outline(space_id: &str, inputs: &WorkOutlineInputs) -> WorkOutlineSnapshot {
    // Step 0: Space gate (suppression covers tombstoned, GC'd, and not yet
    // synced Spaces alike).
    if !inputs.space_live {
        return WorkOutlineSnapshot::unavailable(space_id, inputs.synced, 0);
    }
    let tree = build_tree(space_id, &inputs.items);
    let mut repairs: BTreeSet<WorkOutlineRepair> = tree.repairs.iter().cloned().collect();
    for item in tree.items.values() {
        if item.status_unknown {
            repairs.insert(WorkOutlineRepair::UnknownStatus {
                work_item_id: item.id.clone(),
            });
        }
        if item.title.is_none() {
            repairs.insert(WorkOutlineRepair::MissingTitle {
                work_item_id: item.id.clone(),
            });
        }
    }

    // Step 8: assignments joined with Chats.
    let chats: BTreeMap<&str, Option<&str>> = inputs
        .chats
        .iter()
        .map(|c| (c.id.as_str(), c.space_id.as_deref()))
        .collect();
    let mut assignments: Vec<WorkItemChatAssignmentView> = Vec::new();
    let mut item_chats: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut item_associated: BTreeMap<&str, u32> = BTreeMap::new();
    let mut associated_chats: BTreeSet<&str> = BTreeSet::new();
    let mut associated_item: BTreeMap<&str, &str> = BTreeMap::new();
    for row in &inputs.assignments {
        let chat_id = row.id.as_str();
        let chat_space = chats.get(chat_id).copied();
        let assignment_space = match space_of(row, KIND_WORK_ITEM_CHAT_ASSIGNMENTS) {
            Ok(space) => space,
            Err(repair) => {
                // Only attributable when the Chat lives here.
                if chat_space == Some(Some(space_id)) {
                    repairs.insert(repair);
                }
                continue;
            }
        };
        if assignment_space != space_id && chat_space != Some(Some(space_id)) {
            continue;
        }
        let work_item_id = str_field(row, "workItemId").filter(|w| !w.is_empty());
        let state = match (chat_space, work_item_id.as_deref()) {
            (None, Some(_)) => {
                repairs.insert(WorkOutlineRepair::DanglingChat {
                    chat_id: chat_id.into(),
                });
                WorkItemChatAssignmentState::DanglingChat
            }
            (None, None) => WorkItemChatAssignmentState::Unassociated,
            (Some(chat_space), _)
                if chat_space != Some(space_id) || assignment_space != space_id =>
            {
                repairs.insert(WorkOutlineRepair::AssignmentSpaceMismatch {
                    chat_id: chat_id.into(),
                });
                WorkItemChatAssignmentState::SpaceMismatch
            }
            (Some(_), None) => WorkItemChatAssignmentState::Unassociated,
            (Some(_), Some(item)) if !tree.items.contains_key(item) => {
                repairs.insert(WorkOutlineRepair::DanglingWorkItem {
                    chat_id: chat_id.into(),
                    work_item_id: item.into(),
                });
                WorkItemChatAssignmentState::DanglingWorkItem
            }
            (Some(_), Some(_)) => WorkItemChatAssignmentState::Associated,
        };
        if let Some(item) = work_item_id.as_deref()
            && let Some((key, _)) = tree.items.get_key_value(item)
            && matches!(
                state,
                WorkItemChatAssignmentState::Associated | WorkItemChatAssignmentState::DanglingChat
            )
        {
            item_chats
                .entry(key.as_str())
                .or_default()
                .push(chat_id.into());
            if state == WorkItemChatAssignmentState::Associated {
                *item_associated.entry(key.as_str()).or_default() += 1;
                associated_chats.insert(chat_id);
                associated_item.insert(chat_id, key.as_str());
            }
        }
        assignments.push(WorkItemChatAssignmentView {
            chat_id: chat_id.into(),
            space_id: assignment_space,
            work_item_id,
            assigned_by: actor_field(row, "assignedBy"),
            assigned_at: i64_field(row, "assignedAt").unwrap_or(0),
            state,
        });
    }
    assignments.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));

    // External references (dropped from the view when the item is gone).
    let mut references: Vec<WorkItemExternalReferenceView> = Vec::new();
    let mut item_references: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for row in &inputs.references {
        let work_item_id = str_field(row, "workItemId").unwrap_or_default();
        let reference_space = match space_of(row, KIND_WORK_ITEM_EXTERNAL_REFERENCES) {
            Ok(space) => space,
            Err(repair) => {
                if tree.items.contains_key(&work_item_id) {
                    repairs.insert(repair);
                }
                continue;
            }
        };
        if reference_space != space_id {
            continue;
        }
        let Some((key, _)) = tree.items.get_key_value(&work_item_id) else {
            repairs.insert(WorkOutlineRepair::OrphanReference {
                reference_id: row.id.clone(),
                work_item_id,
            });
            continue;
        };
        item_references
            .entry(key.as_str())
            .or_default()
            .push(row.id.clone());
        references.push(WorkItemExternalReferenceView {
            id: row.id.clone(),
            space_id: reference_space,
            work_item_id: work_item_id.clone(),
            provider: str_field(row, "provider").unwrap_or_default(),
            reference_type: str_field(row, "refType").unwrap_or_default(),
            external_id: str_field(row, "externalId").unwrap_or_default(),
            url: str_field(row, "url").unwrap_or_default(),
            cached_title: str_field(row, "cachedTitle"),
            created_by: actor_field(row, "createdBy"),
            created_at: i64_field(row, "createdAt").unwrap_or(0),
        });
    }
    references.sort_by(|a, b| a.id.cmp(&b.id));

    // Milestone 4: requests and proposals (needed for effective status).
    let user_actions = super::user_actions::materialize_user_actions(
        space_id,
        inputs,
        |id| tree.items.contains_key(id),
        &mut repairs,
    );

    // Items in pre-order, then rollups bottom-up (reverse pre-order).
    let order = tree.preorder();
    let mut views: Vec<WorkItemView> = Vec::with_capacity(order.len());
    let mut position: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, (id, depth)) in order.iter().enumerate() {
        let item = &tree.items[id];
        position.insert(item.id.as_str(), index);
        let mut chat_ids = item_chats.remove(item.id.as_str()).unwrap_or_default();
        chat_ids.sort();
        let mut external_reference_ids =
            item_references.remove(item.id.as_str()).unwrap_or_default();
        external_reference_ids.sort();
        let unresolved = user_actions
            .unresolved_by_item
            .get(item.id.as_str())
            .copied()
            .unwrap_or(0);
        let mut rollup = WorkItemRollup {
            associated_chat_count: item_associated.get(item.id.as_str()).copied().unwrap_or(0),
            unresolved_user_action_count: unresolved,
            ..WorkItemRollup::default()
        };
        if !item.archived() {
            match item.status {
                WorkItemStatus::Pending => rollup.pending_count = 1,
                WorkItemStatus::Active => rollup.active_count = 1,
                WorkItemStatus::Completed => rollup.completed_count = 1,
            }
            rollup.user_action_required_count = u32::from(unresolved > 0);
            rollup.subtree_unresolved_user_action_count = unresolved;
        }
        let effective_status = if unresolved > 0 {
            WorkItemEffectiveStatus::UserActionRequired
        } else {
            status_effective(item.status)
        };
        views.push(WorkItemView {
            id: item.id.clone(),
            space_id: item.space_id.clone(),
            parent_work_item_id: tree.parent[id].clone(),
            raw_parent_work_item_id: item.raw_parent.clone(),
            order_key: item.order_key.clone().unwrap_or_default(),
            title: item.title.clone().unwrap_or_else(|| "Untitled".into()),
            description: item.description.clone(),
            status: item.status,
            effective_status,
            archived_at: item.archived_at,
            archived_by: item.archived_by.clone(),
            archive_cause: item.archive_cause,
            inherit_parent_instructions: item.inherit_parent_instructions,
            created_by: item.created_by.clone(),
            created_at: item.created_at,
            updated_at: item.updated_at,
            child_ids: tree.children_of(Some(id)).to_vec(),
            depth: *depth,
            chat_ids,
            external_reference_ids,
            rollup,
            external_sync_modified_fields: item.external_sync_modified_fields.clone(),
            external_sync_state: item.external_sync_state.clone(),
            is_project: item.is_project,
            worker_destination_id: item.worker_destination_id.clone(),
            template_id: item.template_id.clone(),
            template_revision: item.template_revision,
        });
    }
    for index in (0..views.len()).rev() {
        let Some(parent) = views[index].parent_work_item_id.clone() else {
            continue;
        };
        let Some(&parent_index) = position.get(parent.as_str()) else {
            continue;
        };
        let child = views[index].rollup.clone();
        let total = &mut views[parent_index].rollup;
        total.pending_count += child.pending_count;
        total.active_count += child.active_count;
        total.completed_count += child.completed_count;
        total.user_action_required_count += child.user_action_required_count;
        total.subtree_unresolved_user_action_count += child.subtree_unresolved_user_action_count;
    }

    // Unassociated Chats of this Space: live Chats with no effective
    // association (archived and child Chats included; projectless never).
    let unassociated_chat_ids: Vec<String> = inputs
        .chats
        .iter()
        .filter(|c| c.space_id.as_deref() == Some(space_id))
        .filter(|c| !associated_chats.contains(c.id.as_str()))
        .map(|c| c.id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let (instructions, chat_instruction_states, revisions) = materialize_instructions(
        space_id,
        inputs,
        &tree,
        &views,
        &associated_item,
        &mut repairs,
    );

    let execution = super::execution::materialize_execution(
        space_id,
        inputs.execution_space.as_ref(),
        &inputs.execution_configurations,
        &inputs.chat_execution_bindings,
        &views,
        &associated_item,
        &chats,
        &mut repairs,
    );

    let templates = super::templates::decode_templates(space_id, &inputs.templates, &mut repairs);
    WorkOutlineSnapshot {
        space_id: space_id.into(),
        available: true,
        synced: inputs.synced,
        revision: 0,
        root_work_item_ids: tree.children_of(None).to_vec(),
        items: views,
        assignments,
        unassociated_chat_ids,
        external_references: references,
        repairs: repairs.into_iter().collect(),
        instructions,
        chat_instruction_states,
        revisions,
        execution_space: execution.space,
        execution_configurations: execution.configurations,
        chat_execution_bindings: execution.bindings,
        execution_exceptions: execution.exceptions,
        user_action_requests: user_actions.requests,
        instruction_proposals: user_actions.proposals,
        space_settings: super::linear::decode_space_settings(inputs.space_settings.as_ref()),
        templates,
    }
}

/// Registry audit section 8 steps 7 and 9 (instructions, revisions,
/// selected revision per Chat) plus the host-reported delivery.
///
/// - Every instruction of this Space is listed (archived flagged); one whose
///   work item is missing or in another Space also gets `orphanInstruction`
///   and is never effective (the resolver only follows this Space's items).
/// - One [`ChatInstructionState`] per Chat of this Space that is associated
///   or has an application here. `currentEffectiveHash` is the hash of the
///   associated item's effective instructions; `drift` is "a selection exists
///   and differs", or "no selection but the item has effective
///   instructions"; a Chat without an associated item never drifts.
/// - The selection is the newest application, never substituted; its
///   revision's state is `selectedRevisionIntegrity`. Only selected and
///   delivered revisions are verified; a corrupt one gets `corruptRevision`.
/// - `revisions` lists only the currently selected and delivered revisions
///   of this Space (flagged when corrupt), so frames do not grow with
///   launches.
fn materialize_instructions(
    space_id: &str,
    inputs: &WorkOutlineInputs,
    tree: &OutlineTree,
    views: &[WorkItemView],
    associated_item: &BTreeMap<&str, &str>,
    repairs: &mut BTreeSet<WorkOutlineRepair>,
) -> (
    Vec<WorkItemInstructionView>,
    Vec<ChatInstructionState>,
    Vec<InstructionRevisionView>,
) {
    let mut instructions: Vec<WorkItemInstructionView> = Vec::new();
    for row in &inputs.instructions {
        let instruction = match decode_instruction(row) {
            Ok(instruction) => instruction,
            Err(repair) => {
                // Only attributable when its work item lives here.
                if str_field(row, "workItemId").is_some_and(|w| tree.items.contains_key(&w)) {
                    repairs.insert(repair);
                }
                continue;
            }
        };
        if instruction.space_id != space_id {
            continue;
        }
        if let Some(item) = &instruction.work_item_id
            && !tree.items.contains_key(item)
        {
            repairs.insert(WorkOutlineRepair::OrphanInstruction {
                instruction_id: instruction.id.clone(),
                work_item_id: item.clone(),
            });
        }
        instructions.push(instruction);
    }
    instructions.sort_by(|a, b| {
        (&a.work_item_id, &a.order_key, &a.id).cmp(&(&b.work_item_id, &b.order_key, &b.id))
    });

    let views_by_id: HashMap<&str, &WorkItemView> =
        views.iter().map(|view| (view.id.as_str(), view)).collect();
    let mut effective: BTreeMap<&str, (String, usize, String)> = BTreeMap::new();

    let revision_rows: BTreeMap<&str, &RegistryRow> = inputs
        .revisions
        .iter()
        .map(|row| (row.id.as_str(), row))
        .collect();
    let mut decoded: BTreeMap<String, Option<InstructionRevisionView>> = BTreeMap::new();
    // A revision of another Space is never selected or listed here (and is
    // that Space's repair to report, not this one's).
    let mut revision = |id: &str, repairs: &mut BTreeSet<WorkOutlineRepair>| {
        decoded
            .entry(id.to_owned())
            .or_insert_with(|| revision_rows.get(id).map(|row| decode_revision(row)))
            .clone()
            .inspect(|view| {
                if view.space_id == space_id && view.integrity != InstructionRevisionIntegrity::Ok {
                    repairs.insert(WorkOutlineRepair::CorruptRevision {
                        revision_id: view.id.clone(),
                    });
                }
            })
    };

    let applications: Vec<DecodedApplication> = inputs
        .applications
        .iter()
        .filter_map(decode_application)
        .filter(|a| a.view.space_id == space_id)
        .collect();
    let mut by_chat: BTreeMap<&str, Vec<&DecodedApplication>> = BTreeMap::new();
    for application in &applications {
        by_chat
            .entry(application.view.chat_id.as_str())
            .or_default()
            .push(application);
    }
    let delivered: BTreeMap<&str, ExecutionSessionReport> = inputs
        .sessions
        .iter()
        .filter_map(|row| {
            let report = row.fields.get("executionSession")?;
            Some((
                row.id.as_str(),
                serde_json::from_value(report.clone()).ok()?,
            ))
        })
        .collect();

    let chats: BTreeSet<&str> = inputs
        .chats
        .iter()
        .filter(|c| c.space_id.as_deref() == Some(space_id))
        .map(|c| c.id.as_str())
        .collect();
    let mut wanted_revisions: BTreeSet<String> = BTreeSet::new();
    let mut states = Vec::new();
    for chat_id in chats {
        let item = associated_item.get(chat_id).copied();
        let chat_applications = by_chat.remove(chat_id).unwrap_or_default();
        if item.is_none() && chat_applications.is_empty() {
            continue;
        }
        let current = item.map(|item| {
            effective
                .entry(item)
                .or_insert_with(|| {
                    effective_hash(item, |id| views_by_id.get(id).copied(), &instructions)
                })
                .clone()
        });
        let selected = select_application(chat_applications, |revision_id| {
            match revision(revision_id, repairs) {
                None => RevisionCheck::Missing,
                Some(view)
                    if view.space_id == space_id
                        && view.integrity == InstructionRevisionIntegrity::Ok =>
                {
                    RevisionCheck::Valid(view.body.hash)
                }
                Some(view) => RevisionCheck::Corrupt(view.body.hash),
            }
        });
        let (selected_revision_id, selected_revision_integrity, selected_revision_hash) =
            match selected {
                Some((application, integrity, hash)) => (
                    Some(application.view.revision_id.clone()),
                    Some(integrity),
                    Some(hash),
                ),
                None => (None, None, None),
            };
        // Drift compares instruction content, not the revision hash: a
        // renamed item or ancestor changes the hash (scope titles are part
        // of the stored entries) but is not an instruction change. A
        // selection whose revision is missing or corrupt keeps the hash
        // comparison (fail toward showing drift).
        let selected_content_key = match (&selected_revision_id, selected_revision_integrity) {
            (Some(id), Some(SelectedRevisionIntegrity::Ok)) => {
                revision(id, repairs).map(|view| instruction_content_key(&view.body.entries))
            }
            _ => None,
        };
        let drift = match (&selected_revision_hash, &current) {
            (Some(selected), Some((current_hash, _, current_key))) => match &selected_content_key {
                Some(selected_key) => selected_key != current_key,
                None => selected != current_hash,
            },
            (None, Some((_, count, _))) => *count > 0,
            (_, None) => false,
        };
        let report = delivered.get(chat_id);
        let delivered_revision_id = report.and_then(|r| r.revision_id.clone());
        if let Some(id) = &delivered_revision_id {
            // Flags a corrupt delivered revision too.
            let _ = revision(id, repairs);
            wanted_revisions.insert(id.clone());
        }
        if let Some(id) = &selected_revision_id {
            wanted_revisions.insert(id.clone());
        }
        states.push(ChatInstructionState {
            chat_id: chat_id.to_owned(),
            selected_revision_id,
            selected_revision_hash,
            selected_revision_integrity,
            current_effective_hash: current.map(|(hash, _, _)| hash),
            drift,
            delivered_revision_id,
            delivered_revision_hash: report.and_then(|r| r.revision_hash.clone()),
        });
    }
    let revisions = wanted_revisions
        .iter()
        .filter_map(|id| revision(id, repairs))
        .filter(|view| view.space_id == space_id)
        .collect();
    (instructions, states, revisions)
}
