//! Pure Work Outline materialization (registry audit section 8, steps 0-6, 8,
//! 10 for the Milestone 1 kinds). No I/O and no lock: the engine clones the
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
//! state `associated`. Action-request counts stay 0 until Milestone 4.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use zeron_proto::work_outline::{
    WorkItemArchiveCause, WorkItemChatAssignmentState, WorkItemChatAssignmentView,
    WorkItemEffectiveStatus, WorkItemExternalReferenceView, WorkItemRollup, WorkItemStatus,
    WorkItemView, WorkOutlineActor, WorkOutlineRepair, WorkOutlineSnapshot,
};
use zeron_proto::work_outline_order::valid_order_key;

use super::{KIND_WORK_ITEM_CHAT_ASSIGNMENTS, KIND_WORK_ITEM_EXTERNAL_REFERENCES, KIND_WORK_ITEMS};
use crate::registry::RegistryRow;

/// The slice of a Chat row the outline needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineChat {
    pub id: String,
    /// `None` = projectless.
    pub space_id: Option<String>,
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
}

// ── lenient row decoding ────────────────────────────────────────────────────

fn str_field(row: &RegistryRow, key: &str) -> Option<String> {
    row.fields
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn i64_field(row: &RegistryRow, key: &str) -> Option<i64> {
    row.fields.get(key).and_then(Value::as_i64)
}

fn actor_field(row: &RegistryRow, key: &str) -> Option<WorkOutlineActor> {
    row.fields
        .get(key)
        .filter(|v| v.is_object())
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// The row's `spaceId`: `Err(Malformed)` when present but not a string,
/// `Err(MissingSpace)` when absent.
fn space_of(row: &RegistryRow, kind: &str) -> Result<String, WorkOutlineRepair> {
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
        let mut rollup = WorkItemRollup {
            associated_chat_count: item_associated.get(item.id.as_str()).copied().unwrap_or(0),
            ..WorkItemRollup::default()
        };
        if !item.archived() {
            match item.status {
                WorkItemStatus::Pending => rollup.pending_count = 1,
                WorkItemStatus::Active => rollup.active_count = 1,
                WorkItemStatus::Completed => rollup.completed_count = 1,
            }
        }
        views.push(WorkItemView {
            id: item.id.clone(),
            space_id: item.space_id.clone(),
            parent_work_item_id: tree.parent[id].clone(),
            raw_parent_work_item_id: item.raw_parent.clone(),
            order_key: item.order_key.clone().unwrap_or_default(),
            title: item.title.clone().unwrap_or_else(|| "Untitled".into()),
            description: item.description.clone(),
            status: item.status,
            effective_status: status_effective(item.status),
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
    }
}
