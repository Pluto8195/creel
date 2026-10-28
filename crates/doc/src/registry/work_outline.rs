//! Work Outline registry rows: `workItems`, `workItemChatAssignments`,
//! `workItemExternalReferences` (Milestone 1); in [`instructions`],
//! `workItemInstructions`, `instructionRevisions`,
//! `instructionRevisionApplications` (Milestone 2); and in [`execution`],
//! `executionConfigurations` and `chatExecutionBindings` (Milestone 3); and in
//! [`user_actions`], `userActionRequests` and `workItemInstructionProposals`
//! plus the agent policy (Milestone 4).
//!
//! Mechanism layer (registry audit section 7.11): sizes, formats, immutable
//! field omission, same-Space and cycle checks on the RAW parent graph,
//! create-if-absent, order-key placement, and clock observation. Policy that
//! needs engine state (kill switch, actor derivation, operation-id replay
//! cache) lives in `crates/engine/src/work_outline.rs`.
//!
//! Row schemas: `docs/plans/work-outline/m0-registry-persistence.md` 7.1, 7.3,
//! 7.5. Write rules R1-R8 (section 10):
//! - creation is one `upsert` after a [`RowState`] check that never revives a
//!   tombstone; every later write is an `update` without immutable fields;
//! - placement group P (`parentWorkItemId` + `orderKey`), archive group A
//!   (`archivedAt` + `archivedBy` + `archiveCause`) and assignment group S
//!   (`workItemId` + `assignedBy` + `assignedAt`) are always written in one op;
//! - every touched row (and the destination parent) is observed into the
//!   local HLC before any clock is minted;
//! - every op passes the size gate before anything is enqueued, and one
//!   logical mutation is one `enqueue_ops` call (one batch when it fits);
//! - Chat, Space, and session rows are never written.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use mast_proto::work_outline::{
    ArchiveWorkItemParams, AssignChatToWorkItemParams, CreateWorkItemExternalReferenceParams,
    CreateWorkItemParams, DescendantArchivePolicy, FieldPatch, MoveWorkItemParams,
    RemoveChatFromWorkItemParams, RemoveWorkItemExternalReferenceParams, RepairWorkOutlineParams,
    RestoreWorkItemParams, UpdateWorkItemParams, WorkItemArchiveCause, WorkItemStatus,
    WorkOutlineActor, WorkOutlineActorKind, WorkOutlineError, WorkOutlineErrorCode,
    WorkOutlineMutateReply, WorkOutlineMutation, WorkOutlinePlacement, WorkOutlineRepair,
    WorkOutlineSnapshot, derive_id, external_reference_id, limits, roles,
};
use mast_proto::work_outline_order::order_keys_between;

use super::*;

pub mod execution;
pub mod instructions;
pub mod linear;
mod materialize;
pub mod pr_review_sync;
pub mod user_actions;

use materialize::{DecodedItem, OutlineTree, build_tree, cycle_victim, decode_item};
pub use materialize::{OutlineChat, WorkOutlineInputs, materialize_work_outline};

pub const KIND_WORK_ITEMS: &str = "workItems";
pub const KIND_WORK_ITEM_CHAT_ASSIGNMENTS: &str = "workItemChatAssignments";
pub const KIND_WORK_ITEM_EXTERNAL_REFERENCES: &str = "workItemExternalReferences";
pub const KIND_WORK_ITEM_INSTRUCTIONS: &str = "workItemInstructions";
pub const KIND_INSTRUCTION_REVISIONS: &str = "instructionRevisions";
pub const KIND_INSTRUCTION_REVISION_APPLICATIONS: &str = "instructionRevisionApplications";
pub use execution::{KIND_CHAT_EXECUTION_BINDINGS, KIND_EXECUTION_CONFIGURATIONS};
pub use linear::KIND_WORK_OUTLINE_SPACE_SETTINGS;
pub use user_actions::{KIND_USER_ACTION_REQUESTS, KIND_WORK_ITEM_INSTRUCTION_PROPOSALS};

/// Every outline kind this build writes: the `delete_space` cascade and the
/// orphan sweep cover exactly these. Later milestones append their kinds.
pub const WORK_OUTLINE_KINDS: &[&str] = &[
    KIND_WORK_ITEMS,
    KIND_WORK_ITEM_CHAT_ASSIGNMENTS,
    KIND_WORK_ITEM_EXTERNAL_REFERENCES,
    KIND_WORK_ITEM_INSTRUCTIONS,
    KIND_INSTRUCTION_REVISIONS,
    KIND_INSTRUCTION_REVISION_APPLICATIONS,
    KIND_EXECUTION_CONFIGURATIONS,
    KIND_CHAT_EXECUTION_BINDINGS,
    KIND_USER_ACTION_REQUESTS,
    KIND_WORK_ITEM_INSTRUCTION_PROPOSALS,
    KIND_WORK_OUTLINE_SPACE_SETTINGS,
    // Planning artifacts carry `spaceId` too: the Space cascade and the
    // orphan sweep cover them.
    mast_proto::artifacts::KIND_ARTIFACTS,
];

/// Additive schema version stamped on every outline row (`v`).
pub const WORK_OUTLINE_ROW_VERSION: u32 = 1;

type Outcome<T> = Result<T, WorkOutlineError>;

fn err(code: WorkOutlineErrorCode, message: impl Into<String>) -> WorkOutlineError {
    WorkOutlineError::new(code, message)
}

fn invalid(message: impl Into<String>) -> WorkOutlineError {
    err(WorkOutlineErrorCode::Invalid, message)
}

fn not_found(message: impl Into<String>) -> WorkOutlineError {
    err(WorkOutlineErrorCode::NotFound, message)
}

/// The registry id charset (`[A-Za-z0-9_.:@/-]{1,256}`, edge `ID_RE`).
pub fn valid_registry_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:@/-".contains(&b))
}

fn check_id(what: &str, id: &str) -> Outcome<()> {
    if valid_registry_id(id) {
        Ok(())
    } else {
        Err(invalid(format!("Invalid {what} id")))
    }
}

fn check_operation_id(id: &str) -> Outcome<()> {
    if id.trim().is_empty() || id.chars().count() > limits::MAX_OPERATION_ID_CHARS {
        return Err(invalid("operationId must be a non-empty UUID"));
    }
    Ok(())
}

fn check_title(title: &str) -> Outcome<String> {
    let title = title.trim();
    if title.is_empty() {
        return Err(invalid("A work item needs a title"));
    }
    if title.chars().count() > limits::MAX_TITLE_CHARS {
        return Err(err(
            WorkOutlineErrorCode::LimitExceeded,
            format!(
                "Titles are limited to {} characters",
                limits::MAX_TITLE_CHARS
            ),
        ));
    }
    Ok(title.to_owned())
}

fn check_description(description: &str) -> Outcome<()> {
    if description.chars().count() > limits::MAX_DESCRIPTION_CHARS {
        return Err(err(
            WorkOutlineErrorCode::LimitExceeded,
            format!(
                "Descriptions are limited to {} characters",
                limits::MAX_DESCRIPTION_CHARS
            ),
        ));
    }
    Ok(())
}

/// `LimitExceeded` for an op over the unit budget, naming the field that
/// dominates it: a value within its character limit can still exceed the op
/// budget once JSON-escaped (for example 8,000 quote characters).
fn op_too_large(op: &RowOp) -> WorkOutlineError {
    let units = op_utf16_units(op);
    let largest = op.set.iter().flatten().max_by_key(|(_, value)| {
        serde_json::to_string(value).map_or(0, |s| s.encode_utf16().count())
    });
    let label = match largest.map(|(field, _)| field.as_str()) {
        Some("description") => "Description",
        Some("title") => "Title",
        Some("cachedTitle") => "Reference title",
        Some("text") => "Instruction",
        Some("source") => "Source excerpt",
        Some("body") => "Instruction revision",
        Some("configuration") | Some("resolved") => "Execution configuration",
        Some("info") => "Supporting detail",
        Some("resolution") => "Resolution note",
        Some("review") => "Review",
        Some(other) => other,
        None => "The change",
    };
    err(
        WorkOutlineErrorCode::LimitExceeded,
        format!(
            "{label} is too long to save ({units} of {} units after encoding)",
            limits::MAX_OP_UTF16_UNITS
        ),
    )
}

fn check_text(what: &str, value: &str, max: usize, required: bool) -> Outcome<()> {
    if required && value.trim().is_empty() {
        return Err(invalid(format!("{what} is required")));
    }
    if value.chars().count() > max {
        return Err(err(
            WorkOutlineErrorCode::LimitExceeded,
            format!("{what} is limited to {max} characters"),
        ));
    }
    Ok(())
}

fn status_str(status: WorkItemStatus) -> &'static str {
    match status {
        WorkItemStatus::Pending => "pending",
        WorkItemStatus::Active => "active",
        WorkItemStatus::Completed => "completed",
    }
}

/// The registry row form of an actor: one JSON field
/// `{k, deviceId, chatId?, msgId?}` (registry audit section 7).
pub fn actor_row_value(actor: &WorkOutlineActor) -> Value {
    let kind = match actor.kind {
        WorkOutlineActorKind::User => "user",
        WorkOutlineActorKind::Agent => "agent",
        WorkOutlineActorKind::System => "system",
    };
    let mut value = json!({ "k": kind, "deviceId": actor.device_id });
    if let Some(chat_id) = &actor.chat_id {
        value["chatId"] = json!(chat_id);
    }
    if let Some(message_id) = &actor.message_id {
        value["msgId"] = json!(message_id);
    }
    value
}

/// `hex(sha256(canonical JSON of the request))`: stamped on records derived
/// from an operation id so a replay with different inputs is detectable
/// (`OperationConflict`, contract section 4).
pub fn inputs_hash<T: serde::Serialize>(params: &T) -> String {
    let bytes = serde_json::to_vec(params).unwrap_or_default();
    format!("{:x}", Sha256::digest(&bytes))
}

/// One planned write; clocks are minted only after every touched row has
/// been observed.
struct Planned {
    kind: &'static str,
    id: String,
    op: OpKind,
    set: Option<BTreeMap<String, Value>>,
    /// Existing clocks to keep for specific fields of `set` instead of the
    /// op's fresh clock (re-keys keep the sibling's parent at its old clock).
    keep_clocks: BTreeMap<String, String>,
    /// A field set to the op's own HLC when the clock is minted
    /// (`appliedHlc`, registry audit 7.9).
    stamp_hlc: Option<&'static str>,
}

impl Planned {
    fn update(kind: &'static str, id: &str, set: BTreeMap<String, Value>) -> Self {
        Self {
            kind,
            id: id.to_owned(),
            op: OpKind::Update,
            set: Some(set),
            keep_clocks: BTreeMap::new(),
            stamp_hlc: None,
        }
    }

    fn upsert(kind: &'static str, id: &str, set: BTreeMap<String, Value>) -> Self {
        Self {
            kind,
            id: id.to_owned(),
            op: OpKind::Upsert,
            set: Some(set),
            keep_clocks: BTreeMap::new(),
            stamp_hlc: None,
        }
    }

    fn delete(kind: &'static str, id: &str) -> Self {
        Self {
            kind,
            id: id.to_owned(),
            op: OpKind::Delete,
            set: None,
            keep_clocks: BTreeMap::new(),
            stamp_hlc: None,
        }
    }
}

/// The inputs of one new item ([`RegistryDoc::plan_new_item`]).
struct NewItem<'a> {
    id: &'a str,
    space_id: &'a str,
    parent: Option<&'a str>,
    placement: &'a WorkOutlinePlacement,
    /// Already checked with [`check_title`].
    title: &'a str,
    description: Option<&'a str>,
    status: WorkItemStatus,
    inputs_hash: &'a str,
}

/// Raw placement link of one item (any Space), mutable while a batch is
/// planned so a persisted cycle repair is visible to later steps.
#[derive(Debug, Clone)]
struct RawLink {
    space_id: String,
    parent: Option<String>,
    parent_clock: Option<String>,
    order_key: Option<String>,
}

fn opt_value(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |v| json!(v))
}

/// Group P write for an item: both fields, one op.
fn placement_set(parent: Option<&str>, order_key: &str) -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("parentWorkItemId".to_owned(), opt_value(parent)),
        ("orderKey".to_owned(), json!(order_key)),
    ])
}

/// Where the inserted items land and which existing siblings get new keys.
struct Placement {
    keys: Vec<String>,
    rekeys: Vec<(String, String)>,
}

/// Mint keys for `count` items inserted at `index` of `siblings` (sorted
/// `(validKey?, key, id)`, the inserted items already removed). Normal case:
/// one key strictly between the neighbors. A tie group (equal neighbor keys),
/// invalid neighbor keys, or a key that would exceed the length cap widen the
/// window, and every sibling inside it is re-keyed in the same batch; the
/// last resort is a full rebalance of the group.
fn place(siblings: &[(String, Option<String>)], index: usize, count: usize) -> Outcome<Placement> {
    let n = siblings.len();
    let index = index.min(n);
    let (mut a, mut b) = (index, index);
    let keys = loop {
        if a > 0 && siblings[a - 1].1.is_none() {
            a -= 1;
            continue;
        }
        if b < n && siblings[b].1.is_none() {
            b += 1;
            continue;
        }
        let lo = if a == 0 {
            None
        } else {
            siblings[a - 1].1.as_deref()
        };
        let hi = if b == n {
            None
        } else {
            siblings[b].1.as_deref()
        };
        if let (Some(l), Some(h)) = (lo, hi)
            && l >= h
        {
            a = a.saturating_sub(1);
            b = (b + 1).min(n);
            continue;
        }
        match order_keys_between(lo, hi, (b - a) + count) {
            Ok(keys) => break keys,
            Err(_) if !(a == 0 && b == n) => {
                a = 0;
                b = n;
            }
            Err(e) => {
                return Err(err(
                    WorkOutlineErrorCode::LimitExceeded,
                    format!("Could not place the item: {e}"),
                ));
            }
        }
    };
    let before = index - a;
    let mut rekeys = Vec::new();
    for (offset, sibling) in siblings[a..b].iter().enumerate() {
        let key = if offset < before {
            &keys[offset]
        } else {
            &keys[offset + count]
        };
        if sibling.1.as_deref() != Some(key.as_str()) {
            rekeys.push((sibling.0.clone(), key.clone()));
        }
    }
    Ok(Placement {
        keys: keys[before..before + count].to_vec(),
        rekeys,
    })
}

impl RegistryDoc {
    // ── reads ───────────────────────────────────────────────────────────────

    /// Clone everything the materializer needs (call under the registry lock,
    /// materialize after releasing it).
    pub fn work_outline_inputs(&self, space_id: &str, synced: bool) -> WorkOutlineInputs {
        let (applications, revisions, sessions) = self.instruction_inputs(space_id);
        let (execution_configurations, chat_execution_bindings, execution_space) =
            self.execution_inputs(space_id);
        let (user_action_requests, instruction_proposals) = self.user_action_inputs(space_id);
        WorkOutlineInputs {
            user_action_requests,
            instruction_proposals,
            execution_configurations,
            chat_execution_bindings,
            execution_space,
            space_live: self.row_state(KIND_SPACES, space_id) == RowState::Live,
            synced,
            items: self.overlay_rows(KIND_WORK_ITEMS),
            assignments: self.overlay_rows(KIND_WORK_ITEM_CHAT_ASSIGNMENTS),
            references: self.overlay_rows(KIND_WORK_ITEM_EXTERNAL_REFERENCES),
            instructions: self.overlay_rows(KIND_WORK_ITEM_INSTRUCTIONS),
            space_settings: self.overlay_row(KIND_WORK_OUTLINE_SPACE_SETTINGS, space_id),
            applications,
            revisions,
            sessions,
            chats: self
                .read_chats()
                .unwrap_or_default()
                .into_iter()
                .map(|chat| OutlineChat {
                    id: chat.id,
                    space_id: chat.space_id,
                    archived: chat.archived,
                })
                .collect(),
        }
    }

    /// Change marker of everything [`Self::work_outline_inputs`] reads (the
    /// outline kinds plus Spaces, Chats, and session rows): unchanged between
    /// two reads ⇒ every Space's inputs are unchanged too (`synced` aside).
    pub fn work_outline_inputs_generation(&self) -> u64 {
        let base = self.kinds_generation(WORK_OUTLINE_KINDS);
        base.max(self.kinds_generation(&[KIND_SPACES, KIND_CHATS, KIND_SESSIONS]))
    }

    /// Change marker of the outline kinds alone (work items, Required Actions,
    /// planning artifacts; no chat or session churn).
    pub fn work_outline_kinds_generation(&self) -> u64 {
        self.kinds_generation(WORK_OUTLINE_KINDS)
    }

    /// Convenience: inputs + materialization in one call (tests, small reads).
    pub fn work_outline_snapshot(&self, space_id: &str, synced: bool) -> WorkOutlineSnapshot {
        materialize_work_outline(space_id, &self.work_outline_inputs(space_id, synced))
    }

    /// `(kind, id)` of every live outline row whose `spaceId` is `space_id`.
    pub(crate) fn work_outline_rows_of_space(&self, space_id: &str) -> Vec<(&'static str, String)> {
        let mut keys = Vec::new();
        for kind in WORK_OUTLINE_KINDS {
            for row in self.overlay_rows(kind) {
                if row.fields.get("spaceId").and_then(Value::as_str) == Some(space_id) {
                    keys.push((*kind, row.id));
                }
            }
        }
        keys.sort();
        keys
    }

    /// Live outline rows whose Space row is not live (sorted), computed
    /// without writing anything.
    pub fn work_outline_orphans(&self) -> Vec<(&'static str, String)> {
        let live: HashSet<String> = self
            .overlay_rows(KIND_SPACES)
            .into_iter()
            .map(|row| row.id)
            .collect();
        let mut doomed: Vec<(&'static str, String)> = Vec::new();
        for kind in WORK_OUTLINE_KINDS {
            for row in self.overlay_rows(kind) {
                if let Some(space) = row.fields.get("spaceId").and_then(Value::as_str)
                    && !live.contains(space)
                {
                    doomed.push((*kind, row.id));
                }
            }
        }
        doomed.sort();
        doomed
    }

    /// Orphan cleanup (contract section 3.5): tombstone live outline rows
    /// whose Space row is not live. Destructive on a stale view, so the caller
    /// must gate on `registry_synced()`. Idempotent: only live rows get a
    /// delete, and nothing is written (no clock minted) when there are none.
    /// Returns how many rows were tombstoned.
    pub fn sweep_work_outline_orphans(&mut self) -> usize {
        let doomed = self.work_outline_orphans();
        if doomed.is_empty() {
            return 0;
        }
        let keys: Vec<(&str, &str)> = doomed.iter().map(|(k, id)| (*k, id.as_str())).collect();
        self.delete_row_ops(&keys);
        doomed.len()
    }

    fn live_item(&self, id: &str) -> Outcome<DecodedItem> {
        let row = self
            .overlay_row(KIND_WORK_ITEMS, id)
            .ok_or_else(|| not_found("Work item not found"))?;
        decode_item(&row).map_err(|_| invalid("Work item row is malformed"))
    }

    fn require_space(&self, space_id: &str) -> Outcome<()> {
        check_id("Space", space_id)?;
        if self.row_state(KIND_SPACES, space_id) != RowState::Live {
            return Err(not_found("Space not found"));
        }
        Ok(())
    }

    fn raw_links(&self) -> BTreeMap<String, RawLink> {
        self.overlay_rows(KIND_WORK_ITEMS)
            .into_iter()
            .filter_map(|row| {
                let item = decode_item(&row).ok()?;
                Some((
                    item.id.clone(),
                    RawLink {
                        space_id: item.space_id,
                        parent: item.raw_parent,
                        parent_clock: item.parent_clock,
                        order_key: item.order_key,
                    },
                ))
            })
            .collect()
    }

    fn outline_tree(&self, space_id: &str) -> OutlineTree {
        build_tree(space_id, &self.overlay_rows(KIND_WORK_ITEMS))
    }

    /// Commit a planned mutation: observe every touched row (and `extra`
    /// rows) into the local clock (R3), mint one fresh clock per op, gate
    /// every op's size (R4), then enqueue all ops as one logical batch (R6).
    fn commit(&mut self, plan: Vec<Planned>, extra: &[(&str, &str)]) -> Outcome<()> {
        if plan.is_empty() {
            return Ok(());
        }
        for (kind, id) in extra {
            self.observe_row(kind, id);
        }
        for planned in &plan {
            self.observe_row(planned.kind, &planned.id);
        }
        let mut ops = Vec::with_capacity(plan.len());
        for planned in plan {
            let hlc = self.next_hlc();
            let mut set = planned.set;
            if let (Some(field), Some(set)) = (planned.stamp_hlc, set.as_mut()) {
                set.insert(field.to_owned(), json!(hlc));
            }
            ops.push(RowOp {
                kind: planned.kind.to_owned(),
                id: planned.id,
                op: planned.op,
                set,
                hlc,
                clocks: (!planned.keep_clocks.is_empty()).then_some(planned.keep_clocks),
            });
        }
        for op in &ops {
            if check_op_size(op).is_err() {
                return Err(op_too_large(op));
            }
        }
        self.enqueue_ops(ops);
        Ok(())
    }

    /// Walk the RAW parent chain from `start` (stopping at a root, a missing
    /// parent, or a parent in another Space). A raw cycle on the chain is
    /// repaired exactly as the reader would (same victim) and the repair is
    /// planned into the same batch, then the walk restarts (registry audit
    /// section 8: validating on the repaired graph would let a later move
    /// silently undo the user's move). Returns the chain.
    fn raw_chain_with_repairs(
        links: &mut BTreeMap<String, RawLink>,
        space_id: &str,
        start: &str,
        plan: &mut Vec<Planned>,
    ) -> Vec<String> {
        'walk: loop {
            let mut chain: Vec<String> = Vec::new();
            let mut cur = start.to_owned();
            loop {
                chain.push(cur.clone());
                let Some(parent) = links.get(&cur).and_then(|l| l.parent.clone()) else {
                    return chain;
                };
                if links.get(&parent).is_none_or(|l| l.space_id != space_id) {
                    return chain;
                }
                if let Some(at) = chain.iter().position(|id| *id == parent) {
                    let members = &chain[at..];
                    let victim = cycle_victim(
                        members
                            .iter()
                            .map(|id| (id.as_str(), links[id].parent_clock.as_deref())),
                    )
                    .unwrap_or(parent.as_str())
                    .to_owned();
                    let link = links.get_mut(&victim).expect("cycle member is indexed");
                    link.parent = None;
                    // The repair op carries a fresh clock; keep the victim's
                    // clock maximal for any later in-batch comparison.
                    link.parent_clock = Some("~".into());
                    let mut set = BTreeMap::from([("parentWorkItemId".to_owned(), Value::Null)]);
                    if let Some(key) = &link.order_key {
                        set.insert("orderKey".into(), json!(key));
                    }
                    plan.push(Planned::update(KIND_WORK_ITEMS, &victim, set));
                    continue 'walk;
                }
                cur = parent;
            }
        }
    }

    /// Sibling list of `parent` in effective order as `(id, valid key)`,
    /// without `exclude`.
    fn siblings(
        tree: &OutlineTree,
        parent: Option<&str>,
        exclude: &BTreeSet<&str>,
    ) -> Vec<(String, Option<String>)> {
        tree.children_of(parent)
            .iter()
            .filter(|id| !exclude.contains(id.as_str()))
            .map(|id| (id.clone(), tree.items[id].valid_key().map(str::to_owned)))
            .collect()
    }

    fn placement_index(
        siblings: &[(String, Option<String>)],
        placement: &WorkOutlinePlacement,
    ) -> Outcome<usize> {
        let find = |sibling: &str| {
            siblings
                .iter()
                .position(|(id, _)| id == sibling)
                .ok_or_else(|| not_found("The placement sibling is not a child of the destination"))
        };
        Ok(match placement {
            WorkOutlinePlacement::First => 0,
            WorkOutlinePlacement::Last => siblings.len(),
            WorkOutlinePlacement::Before {
                sibling_work_item_id,
            } => find(sibling_work_item_id)?,
            WorkOutlinePlacement::After {
                sibling_work_item_id,
            } => find(sibling_work_item_id)? + 1,
        })
    }

    /// Plan re-keys for siblings other than the placed item. Each is an
    /// order-only write: `orderKey` gets a fresh clock while
    /// `parentWorkItemId` is carried at the sibling's EXISTING value and
    /// clock (a derived repair stays derived). This device's view of a
    /// sibling's parent may be stale, so stamping it with a fresh clock could
    /// silently revert a concurrent offline move of that sibling; with the
    /// old clock a concurrent move always wins and a torn pair can only
    /// misorder.
    fn plan_rekeys(
        links: &BTreeMap<String, RawLink>,
        rekeys: &[(String, String)],
        plan: &mut Vec<Planned>,
    ) {
        for (id, key) in rekeys {
            let mut set = BTreeMap::from([("orderKey".to_owned(), json!(key))]);
            let mut keep = BTreeMap::new();
            if let Some(link) = links.get(id)
                // `~` marks a victim repaired earlier in this batch; its
                // parent write is already planned, and a never-written
                // parent has no clock to keep.
                && let Some(clock) = link.parent_clock.as_deref().filter(|c| *c != "~")
            {
                set.insert("parentWorkItemId".into(), opt_value(link.parent.as_deref()));
                keep.insert("parentWorkItemId".to_owned(), clock.to_owned());
            }
            let mut planned = Planned::update(KIND_WORK_ITEMS, id, set);
            planned.keep_clocks = keep;
            plan.push(planned);
        }
    }

    /// Validate a destination parent for create/move/move-descendants: live,
    /// same Space, not archived.
    fn destination(&self, space_id: &str, parent: Option<&str>) -> Outcome<()> {
        let Some(parent) = parent else {
            return Ok(());
        };
        check_id("work item", parent)?;
        let item = self
            .live_item(parent)
            .map_err(|_| not_found("Destination work item not found"))?;
        if item.space_id != space_id {
            return Err(err(
                WorkOutlineErrorCode::CrossSpace,
                "The destination belongs to another Space",
            ));
        }
        if item.archived() {
            return Err(err(
                WorkOutlineErrorCode::Archived,
                "The destination work item is archived",
            ));
        }
        Ok(())
    }

    // ── mutations ───────────────────────────────────────────────────────────

    /// Dispatch one `WorkOutlineMutate` call.
    pub fn apply_work_outline_mutation(
        &mut self,
        mutation: &WorkOutlineMutation,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<WorkOutlineMutateReply> {
        check_operation_id(mutation.operation_id())?;
        if actor.kind == WorkOutlineActorKind::Agent {
            let chat_id = actor.chat_id.as_deref().ok_or_else(|| {
                WorkOutlineError::unauthorized("Agent changes need an originating Chat")
            })?;
            self.authorize_agent_mutation(chat_id, mutation)?;
        }
        let empty = WorkOutlineMutateReply::default();
        match mutation {
            WorkOutlineMutation::CreateWorkItem(p) => {
                let id = self.create_work_item(p, actor, now_ms)?;
                Ok(WorkOutlineMutateReply {
                    work_item_id: Some(id),
                    ..Default::default()
                })
            }
            WorkOutlineMutation::UpdateWorkItem(p) => {
                self.update_work_item(p, actor, now_ms).map(|_| empty)
            }
            WorkOutlineMutation::MoveWorkItem(p) => self.move_work_item(p, now_ms).map(|_| empty),
            WorkOutlineMutation::ArchiveWorkItem(p) => {
                self.archive_work_item(p, actor, now_ms).map(|_| empty)
            }
            WorkOutlineMutation::RestoreWorkItem(p) => {
                self.restore_work_item(p, now_ms).map(|_| empty)
            }
            WorkOutlineMutation::AssignChatToWorkItem(p) => self
                .assign_chat_to_work_item(p, actor, now_ms)
                .map(|_| empty),
            WorkOutlineMutation::RemoveChatFromWorkItem(p) => self
                .remove_chat_from_work_item(p, actor, now_ms)
                .map(|_| empty),
            WorkOutlineMutation::CreateWorkItemExternalReference(p) => {
                let id = self.create_work_item_external_reference(p, actor, now_ms)?;
                Ok(WorkOutlineMutateReply {
                    reference_id: Some(id),
                    ..Default::default()
                })
            }
            WorkOutlineMutation::RemoveWorkItemExternalReference(p) => {
                self.remove_work_item_external_reference(p).map(|_| empty)
            }
            WorkOutlineMutation::RepairWorkOutline(p) => self.repair_work_outline(p).map(|_| empty),
            WorkOutlineMutation::CreateWorkItemInstruction(p) => {
                let id = self.create_work_item_instruction(p, actor, now_ms)?;
                Ok(WorkOutlineMutateReply {
                    instruction_id: Some(id),
                    ..Default::default()
                })
            }
            WorkOutlineMutation::UpdateWorkItemInstruction(p) => {
                self.update_work_item_instruction(p, now_ms).map(|_| empty)
            }
            WorkOutlineMutation::MoveWorkItemInstruction(p) => {
                self.move_work_item_instruction(p, now_ms).map(|_| empty)
            }
            WorkOutlineMutation::ArchiveWorkItemInstruction(p) => self
                .archive_work_item_instruction(p, actor, now_ms)
                .map(|_| empty),
            WorkOutlineMutation::CreateInstructionRevision(p) => {
                let id = self.create_instruction_revision(p, actor, now_ms)?;
                Ok(WorkOutlineMutateReply {
                    revision_id: Some(id),
                    ..Default::default()
                })
            }
            WorkOutlineMutation::ApplyInstructionRevision(p) => {
                let id = self.apply_instruction_revision(p, actor, now_ms)?;
                Ok(WorkOutlineMutateReply {
                    application_id: Some(id),
                    ..Default::default()
                })
            }
            WorkOutlineMutation::KeepInstructionSession(p) => {
                // The registry's view of the host's delivery record; the
                // engine passes its own record for Chats it hosts
                // (`keep_instruction_session` with `kept`).
                let id = self.keep_instruction_session(p, None, actor, now_ms)?;
                Ok(WorkOutlineMutateReply {
                    application_id: Some(id),
                    ..Default::default()
                })
            }
            WorkOutlineMutation::ConfigureExecution(p) => {
                self.configure_execution(p, actor, now_ms).map(|_| empty)
            }
            WorkOutlineMutation::CreateUserActionRequest(p) => {
                let id = self.create_user_action_request(p, actor, now_ms)?;
                Ok(WorkOutlineMutateReply {
                    user_action_request_id: Some(id),
                    ..Default::default()
                })
            }
            WorkOutlineMutation::ResolveUserActionRequest(p) => self
                .resolve_user_action_request(p, actor, now_ms)
                .map(|_| empty),
            WorkOutlineMutation::ProposeWorkItemInstruction(p) => {
                let id = self.propose_work_item_instruction(p, actor, now_ms)?;
                Ok(WorkOutlineMutateReply {
                    proposal_id: Some(id),
                    ..Default::default()
                })
            }
            WorkOutlineMutation::ApproveWorkItemInstructionProposal(p) => {
                let id = self.approve_work_item_instruction_proposal(p, actor, now_ms)?;
                Ok(WorkOutlineMutateReply {
                    instruction_id: Some(id),
                    ..Default::default()
                })
            }
            WorkOutlineMutation::RejectWorkItemInstructionProposal(p) => self
                .reject_work_item_instruction_proposal(p, actor, now_ms)
                .map(|_| empty),
            WorkOutlineMutation::SetSpaceLinearEnabled(p) => {
                self.set_space_linear_enabled(p, now_ms).map(|_| empty)
            }
            WorkOutlineMutation::CreateWorkItemFromLinear(p) => {
                let (work_item_id, reference_id) =
                    self.create_work_item_from_linear(p, actor, now_ms)?;
                Ok(WorkOutlineMutateReply {
                    work_item_id: Some(work_item_id),
                    reference_id: Some(reference_id),
                    ..Default::default()
                })
            }
            WorkOutlineMutation::LinkLinearProject(p) => {
                let id = self.link_linear_project(p, actor, now_ms)?;
                Ok(WorkOutlineMutateReply {
                    reference_id: Some(id),
                    ..Default::default()
                })
            }
            WorkOutlineMutation::SetPrReviewSyncDestination(p) => self
                .set_pr_review_sync_destination(p, now_ms)
                .map(|_| empty),
            WorkOutlineMutation::SetLinearAssignedDestination(p) => self
                .set_linear_assigned_destination(p, now_ms)
                .map(|_| empty),
        }
    }

    /// `createWorkItem`: id = `derive_id(operationId, "item")`; a replay
    /// returns the same id without writing; a replay with other inputs is
    /// `OperationConflict`; a deleted id is never revived (R1).
    pub fn create_work_item(
        &mut self,
        p: &CreateWorkItemParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<String> {
        check_operation_id(&p.operation_id)?;
        let title = check_title(&p.title)?;
        if let Some(description) = &p.description {
            check_description(description)?;
        }
        let id = derive_id(&p.operation_id, roles::ITEM);
        let hash = inputs_hash(p);
        if self.created_item_replay(&id, &hash)? {
            return Ok(id);
        }
        let parent = p.parent_work_item_id.as_deref();
        let plan = self.plan_new_item(
            NewItem {
                id: &id,
                space_id: &p.space_id,
                parent,
                placement: &p.placement,
                title: &title,
                description: p.description.as_deref(),
                status: p.status.unwrap_or_default(),
                inputs_hash: &hash,
            },
            actor,
            now_ms,
        )?;
        let extra: Vec<(&str, &str)> = parent.map(|p| (KIND_WORK_ITEMS, p)).into_iter().collect();
        self.commit(plan, &extra)?;
        Ok(id)
    }

    /// Replay check of an item derived from an operation id: `Ok(true)` when
    /// it already exists with the same inputs (nothing to write),
    /// `OperationConflict` with other inputs, `NotFound` when deleted (a
    /// deleted id is never revived, R1).
    fn created_item_replay(&self, id: &str, hash: &str) -> Outcome<bool> {
        match self.row_state(KIND_WORK_ITEMS, id) {
            RowState::Live => {
                let stored = self
                    .overlay_row(KIND_WORK_ITEMS, id)
                    .and_then(|row| row.fields.get("inputsHash").cloned());
                if stored == Some(json!(hash)) {
                    Ok(true)
                } else {
                    Err(err(
                        WorkOutlineErrorCode::OperationConflict,
                        "operationId was already used for a different work item",
                    ))
                }
            }
            RowState::Tombstoned => Err(not_found("This work item was deleted")),
            RowState::Absent => Ok(false),
        }
    }

    /// Plan the creation of one item (placement, sibling re-keys, pending
    /// raw-cycle repairs on the parent chain, and the item's upsert).
    fn plan_new_item(
        &self,
        new: NewItem<'_>,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<Vec<Planned>> {
        self.require_space(new.space_id)?;
        let parent = new.parent;
        self.destination(new.space_id, parent)?;

        let mut plan = Vec::new();
        let mut links = self.raw_links();
        if let Some(parent) = parent {
            Self::raw_chain_with_repairs(&mut links, new.space_id, parent, &mut plan);
        }
        let tree = self.outline_tree(new.space_id);
        let siblings = Self::siblings(&tree, parent, &BTreeSet::new());
        if siblings.len() >= limits::MAX_CHILDREN_PER_PARENT {
            return Err(err(
                WorkOutlineErrorCode::LimitExceeded,
                format!(
                    "A work item can hold at most {} children",
                    limits::MAX_CHILDREN_PER_PARENT
                ),
            ));
        }
        let index = Self::placement_index(&siblings, new.placement)?;
        let placed = place(&siblings, index, 1)?;
        Self::plan_rekeys(&links, &placed.rekeys, &mut plan);

        let mut set = placement_set(parent, &placed.keys[0]);
        set.extend([
            ("spaceId".to_owned(), json!(new.space_id)),
            ("title".to_owned(), json!(new.title)),
            ("status".to_owned(), json!(status_str(new.status))),
            ("inheritParentInstructions".to_owned(), json!(true)),
            ("createdBy".to_owned(), actor_row_value(actor)),
            ("createdAt".to_owned(), json!(now_ms)),
            ("updatedAt".to_owned(), json!(now_ms)),
            ("inputsHash".to_owned(), json!(new.inputs_hash)),
            ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
        ]);
        if let Some(description) = new.description {
            set.insert("description".into(), json!(description));
        }
        plan.push(Planned::upsert(KIND_WORK_ITEMS, new.id, set));
        Ok(plan)
    }

    /// `updateWorkItem`: LWW fields only (never `spaceId`, `createdBy`, …).
    /// Setting status `completed` while the item has open user action
    /// requests needs `changes.resolveOpenRequests` (Milestone 4): without it
    /// the update fails with `openUserActionRequests`; with it every open
    /// request gets an explicit resolution in the same batch. The stored
    /// status never erases request history.
    pub fn update_work_item(
        &mut self,
        p: &UpdateWorkItemParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<()> {
        check_id("work item", &p.work_item_id)?;
        let item = self.live_item(&p.work_item_id)?;
        self.require_space(&item.space_id)?;
        let mut set = BTreeMap::new();
        if let Some(title) = &p.changes.title {
            let title = check_title(title)?;
            if item.title.as_deref() != Some(title.as_str()) {
                set.insert("title".to_owned(), json!(title));
            }
        }
        match &p.changes.description {
            FieldPatch::Unchanged => {}
            FieldPatch::Clear => {
                if item.description.is_some() {
                    set.insert("description".to_owned(), Value::Null);
                }
            }
            FieldPatch::Set(description) => {
                check_description(description)?;
                if item.description.as_deref() != Some(description.as_str()) {
                    set.insert("description".to_owned(), json!(description));
                }
            }
        }
        if let Some(status) = p.changes.status
            && (item.status != status || item.status_unknown)
        {
            set.insert("status".to_owned(), json!(status_str(status)));
        }
        if let Some(inherit) = p.changes.inherit_parent_instructions
            && item.inherit_parent_instructions != inherit
        {
            set.insert("inheritParentInstructions".to_owned(), json!(inherit));
        }
        // PR review sync: only the engine's sync (system actor) records it.
        if let Some(state) = &p.changes.external_sync_state {
            if actor.kind != WorkOutlineActorKind::System {
                return Err(WorkOutlineError::unauthorized(
                    "Only the PR review sync may change an item's sync record",
                ));
            }
            let value = pr_review_sync::check_sync_state(state)?;
            if item.external_sync_state.as_ref() != Some(state) {
                set.insert(pr_review_sync::SYNC_STATE_FIELD.to_owned(), value);
            }
        }
        // Linear: a user's own title/description edit on an issue-linked item
        // stops Linear sync for that field (same op, so it can never be lost).
        if actor.kind == WorkOutlineActorKind::User {
            let edited: Vec<&str> = [
                ("title", mast_proto::linear::SYNC_FIELD_TITLE),
                ("description", mast_proto::linear::SYNC_FIELD_DESCRIPTION),
            ]
            .into_iter()
            .filter(|(field, _)| set.contains_key(*field))
            .map(|(_, name)| name)
            .collect();
            if let Some(fields) = self.modified_fields_with(&item, &edited) {
                set.insert(linear::MODIFIED_FIELDS_FIELD.to_owned(), json!(fields));
            }
        }
        // The Linear sync writes as the system actor: a field the user has
        // taken over since the sync read the item is never overwritten
        // (checked here, under the registry lock).
        if actor.kind == WorkOutlineActorKind::System {
            for field in [
                mast_proto::linear::SYNC_FIELD_TITLE,
                mast_proto::linear::SYNC_FIELD_DESCRIPTION,
            ] {
                if item.external_sync_modified_fields.iter().any(|f| f == field) {
                    set.remove(field);
                }
            }
        }
        let mut plan = if p.changes.status == Some(WorkItemStatus::Completed) {
            self.plan_completion_resolutions(
                &p.work_item_id,
                p.changes.resolve_open_requests,
                actor,
                now_ms,
            )?
        } else {
            Vec::new()
        };
        if !set.is_empty() {
            set.insert("updatedAt".to_owned(), json!(now_ms));
            plan.insert(0, Planned::update(KIND_WORK_ITEMS, &p.work_item_id, set));
        }
        self.commit(plan, &[])
    }

    /// `moveWorkItem` (a move within the same parent is a reorder). Cycle
    /// validation walks the raw parent graph from the destination, persisting
    /// any pending raw cycle repair on that chain in the same batch.
    pub fn move_work_item(&mut self, p: &MoveWorkItemParams, now_ms: i64) -> Outcome<()> {
        check_id("work item", &p.work_item_id)?;
        let item = self.live_item(&p.work_item_id)?;
        let space_id = item.space_id.clone();
        self.require_space(&space_id)?;
        let dest = p.destination_parent_work_item_id.as_deref();
        if dest == Some(p.work_item_id.as_str()) {
            return Err(err(
                WorkOutlineErrorCode::Cycle,
                "A work item cannot be its own parent",
            ));
        }
        self.destination(&space_id, dest)?;
        if matches!(&p.placement, WorkOutlinePlacement::Before { sibling_work_item_id }
            | WorkOutlinePlacement::After { sibling_work_item_id }
            if *sibling_work_item_id == p.work_item_id)
        {
            return Err(invalid("A work item cannot be placed relative to itself"));
        }

        let mut plan = Vec::new();
        let mut links = self.raw_links();
        if let Some(dest) = dest {
            let chain = Self::raw_chain_with_repairs(&mut links, &space_id, dest, &mut plan);
            if chain.contains(&p.work_item_id) {
                return Err(err(
                    WorkOutlineErrorCode::Cycle,
                    "A work item cannot move under its own descendant",
                ));
            }
        }
        let tree = self.outline_tree(&space_id);
        let exclude = BTreeSet::from([p.work_item_id.as_str()]);
        let siblings = Self::siblings(&tree, dest, &exclude);
        let index = Self::placement_index(&siblings, &p.placement)?;

        let current_parent = tree.parent.get(&p.work_item_id).cloned().flatten();
        let same_parent = current_parent.as_deref() == dest && item.raw_parent.as_deref() == dest;
        if same_parent && plan.is_empty() && item.valid_key().is_some() {
            let current_index = tree
                .children_of(dest)
                .iter()
                .position(|id| *id == p.work_item_id);
            if current_index == Some(index) {
                return Ok(()); // already there
            }
        }
        if !same_parent && siblings.len() >= limits::MAX_CHILDREN_PER_PARENT {
            return Err(err(
                WorkOutlineErrorCode::LimitExceeded,
                format!(
                    "A work item can hold at most {} children",
                    limits::MAX_CHILDREN_PER_PARENT
                ),
            ));
        }
        let placed = place(&siblings, index, 1)?;
        Self::plan_rekeys(&links, &placed.rekeys, &mut plan);
        let mut set = placement_set(dest, &placed.keys[0]);
        set.insert("updatedAt".to_owned(), json!(now_ms));
        plan.push(Planned::update(KIND_WORK_ITEMS, &p.work_item_id, set));
        let extra: Vec<(&str, &str)> = dest.map(|d| (KIND_WORK_ITEMS, d)).into_iter().collect();
        self.commit(plan, &extra)
    }

    fn archive_set(
        actor: &WorkOutlineActor,
        now_ms: i64,
        cause: WorkItemArchiveCause,
    ) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("archivedAt".to_owned(), json!(now_ms)),
            ("archivedBy".to_owned(), actor_row_value(actor)),
            (
                "archiveCause".to_owned(),
                serde_json::to_value(cause).unwrap_or(Value::Null),
            ),
            ("updatedAt".to_owned(), json!(now_ms)),
        ])
    }

    /// `archiveWorkItem` with a descendant policy: archive descendants (cause
    /// `cascade`; already archived ones keep their own state) or move the
    /// direct children, in order, to the end of a destination parent in the
    /// same Space. One batch.
    pub fn archive_work_item(
        &mut self,
        p: &ArchiveWorkItemParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<()> {
        check_id("work item", &p.work_item_id)?;
        let item = self.live_item(&p.work_item_id)?;
        let space_id = item.space_id.clone();
        self.require_space(&space_id)?;
        if item.archived() && item.archive_cause == Some(WorkItemArchiveCause::SelfArchived) {
            return Ok(()); // already archived by the user
        }
        let tree = self.outline_tree(&space_id);
        let mut plan = Vec::new();
        let mut extra: Vec<(&str, String)> = Vec::new();
        match &p.descendant_archive_policy {
            DescendantArchivePolicy::ArchiveDescendants => {
                for id in tree.descendants(&p.work_item_id) {
                    if !tree.items[&id].archived() {
                        plan.push(Planned::update(
                            KIND_WORK_ITEMS,
                            &id,
                            Self::archive_set(actor, now_ms, WorkItemArchiveCause::Cascade),
                        ));
                    }
                }
            }
            DescendantArchivePolicy::MoveDescendants {
                destination_parent_work_item_id,
            } => {
                let dest = destination_parent_work_item_id.as_deref();
                let children: Vec<String> = tree.children_of(Some(&p.work_item_id)).to_vec();
                if !children.is_empty() {
                    if dest == Some(p.work_item_id.as_str()) {
                        return Err(invalid(
                            "Descendants cannot move under the item being archived",
                        ));
                    }
                    self.destination(&space_id, dest)?;
                    let mut links = self.raw_links();
                    if let Some(dest) = dest {
                        let chain =
                            Self::raw_chain_with_repairs(&mut links, &space_id, dest, &mut plan);
                        if chain.contains(&p.work_item_id) {
                            return Err(err(
                                WorkOutlineErrorCode::Cycle,
                                "Descendants cannot move under a descendant of the archived item",
                            ));
                        }
                        extra.push((KIND_WORK_ITEMS, dest.to_owned()));
                    }
                    let exclude: BTreeSet<&str> = children.iter().map(String::as_str).collect();
                    let siblings = Self::siblings(&tree, dest, &exclude);
                    if siblings.len() + children.len() > limits::MAX_CHILDREN_PER_PARENT {
                        return Err(err(
                            WorkOutlineErrorCode::LimitExceeded,
                            format!(
                                "A work item can hold at most {} children",
                                limits::MAX_CHILDREN_PER_PARENT
                            ),
                        ));
                    }
                    let placed = place(&siblings, siblings.len(), children.len())?;
                    Self::plan_rekeys(&links, &placed.rekeys, &mut plan);
                    for (child, key) in children.iter().zip(&placed.keys) {
                        let mut set = placement_set(dest, key);
                        set.insert("updatedAt".to_owned(), json!(now_ms));
                        plan.push(Planned::update(KIND_WORK_ITEMS, child, set));
                    }
                }
            }
        }
        plan.push(Planned::update(
            KIND_WORK_ITEMS,
            &p.work_item_id,
            Self::archive_set(actor, now_ms, WorkItemArchiveCause::SelfArchived),
        ));
        let extra: Vec<(&str, &str)> = extra.iter().map(|(k, id)| (*k, id.as_str())).collect();
        self.commit(plan, &extra)
    }

    /// `restoreWorkItem`: clear group A on the item and on every descendant
    /// that a cascade archived (stopping at descendants the user archived
    /// themselves, whose subtrees stay as they are).
    pub fn restore_work_item(&mut self, p: &RestoreWorkItemParams, now_ms: i64) -> Outcome<()> {
        check_id("work item", &p.work_item_id)?;
        let item = self.live_item(&p.work_item_id)?;
        self.require_space(&item.space_id)?;
        if !item.archived() {
            return Ok(());
        }
        let tree = self.outline_tree(&item.space_id);
        let restore_set = || {
            BTreeMap::from([
                ("archivedAt".to_owned(), Value::Null),
                ("archivedBy".to_owned(), Value::Null),
                ("archiveCause".to_owned(), Value::Null),
                ("updatedAt".to_owned(), json!(now_ms)),
            ])
        };
        let mut plan = vec![Planned::update(
            KIND_WORK_ITEMS,
            &p.work_item_id,
            restore_set(),
        )];
        let mut stack: Vec<&str> = tree
            .children_of(Some(&p.work_item_id))
            .iter()
            .rev()
            .map(String::as_str)
            .collect();
        while let Some(id) = stack.pop() {
            let child = &tree.items[id];
            if child.archived() {
                if child.archive_cause != Some(WorkItemArchiveCause::Cascade) {
                    continue;
                }
                plan.push(Planned::update(KIND_WORK_ITEMS, id, restore_set()));
            }
            stack.extend(tree.children_of(Some(id)).iter().rev().map(String::as_str));
        }
        self.commit(plan, &[])
    }

    /// `assignChatToWorkItem`: one group S op on the assignment row keyed by
    /// the Chat id. Never writes the Chat, Space, or session rows (R8).
    pub fn assign_chat_to_work_item(
        &mut self,
        p: &AssignChatToWorkItemParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<()> {
        check_id("Chat", &p.chat_id)?;
        check_id("work item", &p.work_item_id)?;
        let chat = self
            .chat(&p.chat_id)
            .ok()
            .flatten()
            .ok_or_else(|| not_found("Chat not found"))?;
        let Some(chat_space) = chat.space_id.filter(|s| !s.is_empty()) else {
            return Err(err(
                WorkOutlineErrorCode::Projectless,
                "Chats outside a project cannot be assigned to a work item",
            ));
        };
        let item = self.live_item(&p.work_item_id)?;
        if item.space_id != chat_space {
            return Err(err(
                WorkOutlineErrorCode::CrossSpace,
                "The Chat and the work item belong to different Spaces",
            ));
        }
        self.require_space(&chat_space)?;
        if item.archived() {
            return Err(err(
                WorkOutlineErrorCode::Archived,
                "The work item is archived",
            ));
        }
        let mut group_s = BTreeMap::from([
            ("workItemId".to_owned(), json!(p.work_item_id)),
            ("assignedBy".to_owned(), actor_row_value(actor)),
            ("assignedAt".to_owned(), json!(now_ms)),
        ]);
        let plan = match self.overlay_row(KIND_WORK_ITEM_CHAT_ASSIGNMENTS, &p.chat_id) {
            Some(row) => {
                let stored_space = row.fields.get("spaceId").and_then(Value::as_str);
                if stored_space.is_some_and(|s| s != chat_space) {
                    return Err(err(
                        WorkOutlineErrorCode::CrossSpace,
                        "The Chat's assignment row belongs to another Space",
                    ));
                }
                if row.fields.get("workItemId").and_then(Value::as_str)
                    == Some(p.work_item_id.as_str())
                {
                    return Ok(()); // already assigned there
                }
                Planned::update(KIND_WORK_ITEM_CHAT_ASSIGNMENTS, &p.chat_id, group_s)
            }
            // First assignment (or a re-assignment after an orphan cleanup
            // tombstoned the row: an explicit user action, so the upsert may
            // revive it). `spaceId` is written exactly once, here.
            None => {
                group_s.insert("spaceId".to_owned(), json!(chat_space));
                group_s.insert("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION));
                Planned::upsert(KIND_WORK_ITEM_CHAT_ASSIGNMENTS, &p.chat_id, group_s)
            }
        };
        self.commit(vec![plan], &[(KIND_WORK_ITEMS, p.work_item_id.as_str())])
    }

    /// `removeChatFromWorkItem`: an explicit disassociation (group S with a
    /// null `workItemId`), never a tombstone. Works for dangling assignments
    /// whose Chat is gone.
    pub fn remove_chat_from_work_item(
        &mut self,
        p: &RemoveChatFromWorkItemParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<()> {
        check_id("Chat", &p.chat_id)?;
        let Some(row) = self.overlay_row(KIND_WORK_ITEM_CHAT_ASSIGNMENTS, &p.chat_id) else {
            return Ok(());
        };
        if row
            .fields
            .get("workItemId")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return Ok(());
        }
        let set = BTreeMap::from([
            ("workItemId".to_owned(), Value::Null),
            ("assignedBy".to_owned(), actor_row_value(actor)),
            ("assignedAt".to_owned(), json!(now_ms)),
        ]);
        self.commit(
            vec![Planned::update(
                KIND_WORK_ITEM_CHAT_ASSIGNMENTS,
                &p.chat_id,
                set,
            )],
            &[],
        )
    }

    /// `createWorkItemExternalReference`: content-derived id, so duplicate
    /// adds converge to one row; re-adding after removal revives it with a
    /// newer upsert (registry audit 7.5). Never touches `chatPrLinks` or the
    /// Chat's linked PR/ticket fields.
    pub fn create_work_item_external_reference(
        &mut self,
        p: &CreateWorkItemExternalReferenceParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<String> {
        check_id("work item", &p.work_item_id)?;
        check_text(
            "Provider",
            &p.provider,
            limits::MAX_REFERENCE_PROVIDER_CHARS,
            true,
        )?;
        check_text(
            "Reference type",
            &p.reference_type,
            limits::MAX_REFERENCE_TYPE_CHARS,
            true,
        )?;
        check_text(
            "External id",
            &p.external_id,
            limits::MAX_EXTERNAL_ID_CHARS,
            true,
        )?;
        check_text("URL", &p.url, limits::MAX_REFERENCE_URL_CHARS, true)?;
        if let Some(title) = &p.cached_title {
            check_text("Title", title, limits::MAX_REFERENCE_TITLE_CHARS, false)?;
        }
        let item = self.live_item(&p.work_item_id)?;
        self.require_space(&item.space_id)?;
        let id = external_reference_id(
            &p.work_item_id,
            &p.provider,
            &p.reference_type,
            &p.external_id,
        );
        if self.row_state(KIND_WORK_ITEM_EXTERNAL_REFERENCES, &id) == RowState::Live {
            return Ok(id);
        }
        let mut set = BTreeMap::from([
            ("spaceId".to_owned(), json!(item.space_id)),
            ("workItemId".to_owned(), json!(p.work_item_id)),
            ("provider".to_owned(), json!(p.provider)),
            ("refType".to_owned(), json!(p.reference_type)),
            ("externalId".to_owned(), json!(p.external_id)),
            ("url".to_owned(), json!(p.url)),
            ("createdBy".to_owned(), actor_row_value(actor)),
            ("createdAt".to_owned(), json!(now_ms)),
            ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
        ]);
        if let Some(title) = &p.cached_title {
            set.insert("cachedTitle".into(), json!(title));
        }
        let mut plan = vec![Planned::upsert(
            KIND_WORK_ITEM_EXTERNAL_REFERENCES,
            &id,
            set,
        )];
        // Linking an issue to an item that already has the user's own
        // description keeps that description (Linear only takes the title).
        if actor.kind == WorkOutlineActorKind::User
            && linear::is_linear_issue(&p.provider, &p.reference_type)
            && item
                .description
                .as_deref()
                .is_some_and(|d| !d.trim().is_empty())
            && let Some(fields) = linear::merge_modified_fields(
                &item.external_sync_modified_fields,
                &[mast_proto::linear::SYNC_FIELD_DESCRIPTION],
            )
        {
            plan.push(Planned::update(
                KIND_WORK_ITEMS,
                &p.work_item_id,
                BTreeMap::from([(linear::MODIFIED_FIELDS_FIELD.to_owned(), json!(fields))]),
            ));
        }
        self.commit(plan, &[(KIND_WORK_ITEMS, p.work_item_id.as_str())])?;
        Ok(id)
    }

    /// `removeWorkItemExternalReference`: tombstone (registry audit 7.5).
    pub fn remove_work_item_external_reference(
        &mut self,
        p: &RemoveWorkItemExternalReferenceParams,
    ) -> Outcome<()> {
        check_id("reference", &p.reference_id)?;
        if self.row_state(KIND_WORK_ITEM_EXTERNAL_REFERENCES, &p.reference_id) != RowState::Live {
            return Ok(());
        }
        let mut plan = vec![Planned::delete(
            KIND_WORK_ITEM_EXTERNAL_REFERENCES,
            &p.reference_id,
        )];
        // Removing an item's last Linear issue reference ends the sync, so
        // its modified-field marks go with it (same batch).
        if let Some(work_item_id) = self.clears_modified_fields_on_unlink(&p.reference_id) {
            plan.push(Planned::update(
                KIND_WORK_ITEMS,
                &work_item_id,
                BTreeMap::from([(linear::MODIFIED_FIELDS_FIELD.to_owned(), json!([]))]),
            ));
        }
        self.commit(plan, &[])
    }

    /// `repairWorkOutline`: persist the derived repairs that are safe to
    /// persist — cycle victims become roots, items with invalid keys get
    /// fresh keys after their group's last valid key. Missing parents are NOT
    /// persisted (the parent may simply not have synced yet).
    pub fn repair_work_outline(&mut self, p: &RepairWorkOutlineParams) -> Outcome<()> {
        self.require_space(&p.space_id)?;
        let tree = self.outline_tree(&p.space_id);
        let links = self.raw_links();
        let victims: BTreeSet<String> = tree
            .repairs
            .iter()
            .filter_map(|r| match r {
                WorkOutlineRepair::CycleBroken { victim, .. } => Some(victim.clone()),
                _ => None,
            })
            .collect();
        let mut new_keys: BTreeMap<String, String> = BTreeMap::new();
        for siblings in tree.children.values() {
            let bad: Vec<&String> = siblings
                .iter()
                .filter(|id| tree.items[*id].valid_key().is_none())
                .collect();
            if bad.is_empty() {
                continue;
            }
            let last_valid = siblings
                .iter()
                .filter_map(|id| tree.items[id].valid_key())
                .next_back();
            if let Ok(keys) = order_keys_between(last_valid, None, bad.len()) {
                for (id, key) in bad.into_iter().zip(keys) {
                    new_keys.insert(id.clone(), key);
                }
            } else {
                // Tail keys would overflow: rebalance the whole group.
                let all = order_keys_between(None, None, siblings.len())
                    .map_err(|e| err(WorkOutlineErrorCode::LimitExceeded, e.to_string()))?;
                for (id, key) in siblings.iter().zip(all) {
                    new_keys.insert(id.clone(), key);
                }
            }
        }
        let mut plan = Vec::new();
        let touched: BTreeSet<&String> = victims.iter().chain(new_keys.keys()).collect();
        for id in touched {
            let parent = if victims.contains(id) {
                None
            } else {
                links.get(id).and_then(|l| l.parent.as_deref())
            };
            let key = new_keys
                .get(id)
                .cloned()
                .or_else(|| tree.items[id].order_key.clone());
            let mut set = BTreeMap::from([("parentWorkItemId".to_owned(), opt_value(parent))]);
            if let Some(key) = key {
                set.insert("orderKey".to_owned(), json!(key));
            }
            plan.push(Planned::update(KIND_WORK_ITEMS, id, set));
        }
        self.commit(plan, &[])
    }
}

#[cfg(test)]
mod tests;
