//! Work Outline wire contract: snapshot view types, operation requests and
//! replies, error codes, limits, and the operation-id derivation helper.
//!
//! Binding references: `docs/plans/work-outline/m0-contract.md` (sections 2-4),
//! `m0-registry-persistence.md` (sections 7-9), `m0-idempotency-command-plane.md`
//! (section 3). The Work Outline is a *view* over additive registry rows;
//! these types are what `WatchWorkOutline` emits and what the mutation RPC
//! accepts. They are not the registry row schemas (the doc crate owns those).
//!
//! Wire conventions (house style, see `entities.rs`):
//! - field names are camelCase; enums are camelCase strings (or an internal
//!   tag, named in each type's docs);
//! - every non-identity field decodes when absent (`#[serde(default)]`), so a
//!   snapshot from an older or newer peer never fails to parse;
//! - enums that appear in snapshots are *lenient*: an unknown value decodes to
//!   a documented fallback instead of failing the frame. Request enums
//!   (placements, archive policies) are strict on purpose, a malformed
//!   request must be a `BadParams` error rather than a guess;
//! - timestamps are epoch milliseconds (`i64`), matching the registry rows;
//!   they are display-only, conflict resolution uses registry HLCs.
//!
//! Milestone scope: this is the Milestone 1 surface. Later milestones append
//! fields to [`WorkOutlineSnapshot`] and variants to [`WorkOutlineMutation`]
//! and [`WorkOutlineRepair`]; because everything is lenient, appending is
//! backward compatible. The planned additions are listed on
//! [`WorkOutlineSnapshot`].
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use crate::work_outline_order::MAX_ORDER_KEY_LEN;

/// Hard limits enforced by typed mutators (doc crate) and the engine service,
/// and again by the edge as a backstop. Registry audit section 6.
pub mod limits {
    pub use super::MAX_ORDER_KEY_LEN;

    /// Work item title, in Unicode scalar values.
    pub const MAX_TITLE_CHARS: usize = 256;
    /// Work item description, in Unicode scalar values.
    pub const MAX_DESCRIPTION_CHARS: usize = 8_000;
    /// Children of one parent (or of the Space root). A rebalance of a full
    /// group fits in two registry batches.
    pub const MAX_CHILDREN_PER_PARENT: usize = 2_000;
    /// External reference field limits.
    pub const MAX_REFERENCE_URL_CHARS: usize = 2_048;
    pub const MAX_REFERENCE_TITLE_CHARS: usize = 256;
    pub const MAX_REFERENCE_PROVIDER_CHARS: usize = 64;
    pub const MAX_REFERENCE_TYPE_CHARS: usize = 64;
    pub const MAX_EXTERNAL_ID_CHARS: usize = 512;
    /// Operation ids are UUIDs; anything longer is rejected as `Invalid`.
    pub const MAX_OPERATION_ID_CHARS: usize = 64;
    /// Client-side budget for one serialized registry op, in UTF-16 code
    /// units (JS `JSON.stringify(op).length`). The edge rejects at 16,384 and
    /// a rejected batch wedges every later registry write, so an op over this
    /// budget must never enter the queue (registry audit section 6, rule R4).
    /// Serialized, not visible, length: JSON escaping can double text.
    pub const MAX_OP_UTF16_UNITS: usize = 15_000;
}

/// Environment kill switch: when set (any non-empty value other than `0`),
/// every mutation fails with [`WorkOutlineErrorCode::WritesDisabled`] while
/// reads keep working (contract section 3.6).
pub const DISABLE_WRITES_ENV: &str = "ZERON_DISABLE_WORK_OUTLINE_WRITES";

/// Roles accepted by [`derive_id`] (contract section 4). Not an enum so a
/// later milestone can add a role without a wire change.
pub mod roles {
    pub const CHAT: &str = "chat";
    pub const MESSAGE: &str = "message";
    pub const COMMAND: &str = "command";
    pub const ASSIGNMENT: &str = "assignment";
    pub const REVISION: &str = "revision";
    pub const APPLICATION: &str = "application";
    pub const BINDING: &str = "binding";
    pub const REQUEST: &str = "request";
    pub const INSTRUCTION: &str = "instruction";
    pub const ITEM: &str = "item";
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// `hex(sha256(operationId ‖ 0x00 ‖ role))[..32]`: a deterministic record id
/// derived from a client operation id. Every retry of one logical action
/// re-derives the same ids, so multi-step workflows converge without
/// duplicates (contract section 4). 32 hex characters fit the registry id
/// charset (`[A-Za-z0-9_.:@/-]{1,256}`).
pub fn derive_id(operation_id: &str, role: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(operation_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(role.as_bytes());
    hex(&hasher.finalize())[..32].to_owned()
}

/// Row id of a `workItemExternalReferences` row:
/// `hex(sha256(workItemId ‖ 0 ‖ provider ‖ 0 ‖ referenceType ‖ 0 ‖ externalId))`
/// (64 hex characters, registry audit section 7.5). Content-derived so a
/// duplicate add converges to one row; it is also the `referenceId` returned
/// by [`CreateWorkItemExternalReferenceResult`].
pub fn external_reference_id(
    work_item_id: &str,
    provider: &str,
    reference_type: &str,
    external_id: &str,
) -> String {
    let mut hasher = Sha256::new();
    for (i, part) in [work_item_id, provider, reference_type, external_id]
        .into_iter()
        .enumerate()
    {
        if i > 0 {
            hasher.update([0u8]);
        }
        hasher.update(part.as_bytes());
    }
    hex(&hasher.finalize())
}

// --------------------------------------------------------------------------
// Shared small types
// --------------------------------------------------------------------------

/// Who performed a write. Provenance only; never an authorization input on
/// the client. On the registry row this is one JSON field (`k` is accepted as
/// an alias of `kind`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkOutlineActor {
    #[serde(default, alias = "k")]
    pub kind: WorkOutlineActorKind,
    #[serde(default)]
    pub device_id: String,
    /// The originating Chat when `kind` is `agent` (or a user acting from a Chat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    #[serde(default, alias = "msgId", skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

/// Lenient: an unknown kind from a newer peer reads as `System`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkOutlineActorKind {
    User,
    Agent,
    #[default]
    #[serde(other)]
    System,
}

/// Stored work-item status. Lenient: unknown reads as `Pending` (the
/// materializer also emits [`WorkOutlineRepair::UnknownStatus`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkItemStatus {
    Active,
    Completed,
    /// Declared last because `#[serde(other)]` must sit on the last variant.
    #[default]
    #[serde(other)]
    Pending,
}

/// Status shown to the user: the stored status, overridden to
/// `UserActionRequired` while unresolved action requests exist.
/// Lenient: unknown reads as `Pending`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkItemEffectiveStatus {
    Active,
    UserActionRequired,
    Completed,
    /// Declared last because `#[serde(other)]` must sit on the last variant.
    #[default]
    #[serde(other)]
    Pending,
}

/// Why an item is archived, recorded so restore can undo exactly that:
/// `self` (the user archived this item) or `cascade` (an ancestor's
/// `archiveDescendants` policy). Lenient: unknown reads as `self`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkItemArchiveCause {
    Cascade,
    /// Declared last because `#[serde(other)]` must sit on the last variant.
    #[default]
    #[serde(rename = "self", other)]
    SelfArchived,
}

/// Where to put an item among its new siblings: a boundary or a neighbor,
/// never an integer index. Internally tagged by `type`:
/// `{"type":"first"}`, `{"type":"last"}`,
/// `{"type":"before","siblingWorkItemId":"…"}`,
/// `{"type":"after","siblingWorkItemId":"…"}`. Strict (request enum).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WorkOutlinePlacement {
    First,
    #[default]
    Last,
    #[serde(rename_all = "camelCase")]
    Before {
        sibling_work_item_id: String,
    },
    #[serde(rename_all = "camelCase")]
    After {
        sibling_work_item_id: String,
    },
}

/// What happens to descendants when an item is archived. Internally tagged by
/// `type`: `{"type":"archiveDescendants"}` or
/// `{"type":"moveDescendants","destinationParentWorkItemId":null}` (`null` =
/// the Space root). Strict (request enum).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DescendantArchivePolicy {
    ArchiveDescendants,
    #[serde(rename_all = "camelCase")]
    MoveDescendants {
        #[serde(default)]
        destination_parent_work_item_id: Option<String>,
    },
}

/// A tri-state field in a patch: absent (leave alone), `null` (clear), or a
/// value (set). Use with
/// `#[serde(default, skip_serializing_if = "FieldPatch::is_unchanged")]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum FieldPatch<T> {
    #[default]
    Unchanged,
    Clear,
    Set(T),
}

impl<T> FieldPatch<T> {
    pub fn is_unchanged(&self) -> bool {
        matches!(self, Self::Unchanged)
    }
}

impl<T: Serialize> Serialize for FieldPatch<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Unchanged | Self::Clear => serializer.serialize_none(),
            Self::Set(value) => value.serialize(serializer),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for FieldPatch<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Option::<T>::deserialize(deserializer)? {
            None => Self::Clear,
            Some(value) => Self::Set(value),
        })
    }
}

// --------------------------------------------------------------------------
// Snapshot
// --------------------------------------------------------------------------

/// Subtree rollup for one work item. The three status counts and
/// `user_action_required_count` cover the item and all of its descendants
/// (the item itself included); which descendants are counted (for example
/// whether archived items are) is fixed by the materializer and documented
/// there. `unresolved_user_action_count` is the item's own open requests and
/// `associated_chat_count` its own associated Chats (both 0 until their
/// milestones).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemRollup {
    #[serde(default)]
    pub pending_count: u32,
    #[serde(default)]
    pub active_count: u32,
    #[serde(default)]
    pub completed_count: u32,
    #[serde(default)]
    pub user_action_required_count: u32,
    #[serde(default)]
    pub unresolved_user_action_count: u32,
    #[serde(default)]
    pub associated_chat_count: u32,
}

fn default_true() -> bool {
    true
}

/// One materialized work item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemView {
    pub id: String,
    pub space_id: String,
    /// The *effective* (repaired) parent; `None` = a Space-level root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_work_item_id: Option<String>,
    /// The parent as stored on the row, before repair. Differs from
    /// `parent_work_item_id` exactly when a repair applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_parent_work_item_id: Option<String>,
    /// Stored order key (possibly invalid, see `WorkOutlineRepair::BadOrderKey`).
    #[serde(default)]
    pub order_key: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub status: WorkItemStatus,
    #[serde(default)]
    pub effective_status: WorkItemEffectiveStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_by: Option<WorkOutlineActor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_cause: Option<WorkItemArchiveCause>,
    /// Missing on the row reads as `true`.
    #[serde(default = "default_true")]
    pub inherit_parent_instructions: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<WorkOutlineActor>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    /// Effective children in `(orderKey, id)` order (invalid keys last).
    #[serde(default)]
    pub child_ids: Vec<String>,
    /// Distance from the Space root; a root item has depth 0.
    #[serde(default)]
    pub depth: u32,
    /// Chats assigned to this item, sorted by Chat id: assignments in state
    /// `associated`, plus `danglingChat` ones (the Chat row is gone but the
    /// assignment is kept so the UI can render a "missing chat" row).
    #[serde(default)]
    pub chat_ids: Vec<String>,
    /// External references of this item, sorted by reference id.
    #[serde(default)]
    pub external_reference_ids: Vec<String>,
    #[serde(default)]
    pub rollup: WorkItemRollup,
}

/// How a Chat relates to the outline. Lenient: unknown reads as `unassociated`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkItemChatAssignmentState {
    /// Assigned to a live work item in this Space.
    Associated,
    /// The assignment survives but its Chat row is gone; kept inspectable.
    DanglingChat,
    /// The assignment names a work item that is missing or in another Space;
    /// the Chat is shown as unassociated.
    DanglingWorkItem,
    /// The assignment and the Chat disagree on Space; shown as unassociated.
    SpaceMismatch,
    /// No assignment, or explicitly disassociated (`workItemId` is null).
    /// Declared last because `#[serde(other)]` must sit on the last variant.
    #[default]
    #[serde(other)]
    Unassociated,
}

/// One Chat-to-work-item assignment (registry row keyed by Chat id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemChatAssignmentView {
    pub chat_id: String,
    #[serde(default)]
    pub space_id: String,
    /// `None` = explicit disassociation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_by: Option<WorkOutlineActor>,
    #[serde(default)]
    pub assigned_at: i64,
    #[serde(default)]
    pub state: WorkItemChatAssignmentState,
}

/// One external reference (issue, PR, doc link) on a work item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemExternalReferenceView {
    /// [`external_reference_id`].
    pub id: String,
    #[serde(default)]
    pub space_id: String,
    #[serde(default)]
    pub work_item_id: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub reference_type: String,
    #[serde(default)]
    pub external_id: String,
    #[serde(default)]
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<WorkOutlineActor>,
    #[serde(default)]
    pub created_at: i64,
}

/// A derived repair the materializer applied (or that `repairWorkOutline`
/// persists). Registry audit section 8. Internally tagged by `type` (the tag
/// cannot be `kind`: `Malformed` carries a row kind). Lenient: an unknown
/// variant from a newer peer decodes to `Unknown`. Ordered so a snapshot's
/// repair list can be sorted deterministically.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WorkOutlineRepair {
    /// A row could not be decoded and was dropped.
    #[serde(rename_all = "camelCase")]
    Malformed {
        #[serde(default)]
        row_kind: String,
        #[serde(default)]
        id: String,
    },
    /// A row has no `spaceId` and was dropped.
    #[serde(rename_all = "camelCase")]
    MissingSpace {
        #[serde(default)]
        row_kind: String,
        #[serde(default)]
        id: String,
    },
    /// The raw parent is absent (missing, tombstoned, or not yet synced); the
    /// item is shown at the root.
    #[serde(rename_all = "camelCase")]
    ParentMissing {
        #[serde(default)]
        work_item_id: String,
        #[serde(default)]
        parent_work_item_id: String,
    },
    /// The raw parent lives in another Space; the item is shown at the root.
    #[serde(rename_all = "camelCase")]
    CrossSpaceParent {
        #[serde(default)]
        work_item_id: String,
        #[serde(default)]
        parent_work_item_id: String,
    },
    /// A parent cycle was cut at `victim` (greatest parent-field clock, then
    /// id); `members` is the whole cycle, sorted.
    #[serde(rename_all = "camelCase")]
    CycleBroken {
        #[serde(default)]
        victim: String,
        #[serde(default)]
        former_parent: String,
        #[serde(default)]
        members: Vec<String>,
    },
    /// Missing or invalid order key; the item sorts last, by id.
    #[serde(rename_all = "camelCase")]
    BadOrderKey {
        #[serde(default)]
        work_item_id: String,
    },
    /// Unknown stored status read as `pending`.
    #[serde(rename_all = "camelCase")]
    UnknownStatus {
        #[serde(default)]
        work_item_id: String,
    },
    /// Missing title shown as "Untitled".
    #[serde(rename_all = "camelCase")]
    MissingTitle {
        #[serde(default)]
        work_item_id: String,
    },
    /// The assignment and its Chat disagree on Space; shown unassociated.
    #[serde(rename_all = "camelCase")]
    AssignmentSpaceMismatch {
        #[serde(default)]
        chat_id: String,
    },
    /// The assignment's Chat row is gone.
    #[serde(rename_all = "camelCase")]
    DanglingChat {
        #[serde(default)]
        chat_id: String,
    },
    /// The assignment's work item is missing or in another Space.
    #[serde(rename_all = "camelCase")]
    DanglingWorkItem {
        #[serde(default)]
        chat_id: String,
        #[serde(default)]
        work_item_id: String,
    },
    /// An external reference whose work item is missing; dropped from the view.
    #[serde(rename_all = "camelCase")]
    OrphanReference {
        #[serde(default)]
        reference_id: String,
        #[serde(default)]
        work_item_id: String,
    },
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

/// The composed, deterministic Work Outline of one Space (the item of the
/// `WatchWorkOutline` stream). Everything is sorted; two materializations of
/// the same inputs are `==`, so the stream can dedupe frames.
///
/// Reserved for later milestones (do NOT add before their milestone; each is
/// an additive, defaulted field): `instructions` / `effectiveInstructions`
/// (M2), `executionConfigurations` and `chatExecutionBindings` (M3),
/// `userActionRequests`, `instructionProposals`, `instructionRevisions` and
/// `selectedRevisions` per Chat (M2/M4). Counts in [`WorkItemRollup`] that
/// depend on them are 0 until then.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkOutlineSnapshot {
    pub space_id: String,
    /// `false` when the Space row is not live (absent or tombstoned): the
    /// outline is suppressed and every list is empty.
    #[serde(default)]
    pub available: bool,
    /// Whether authoritative registry state has been received. `available ==
    /// false && synced == false` means "not heard yet", not "deleted".
    #[serde(default)]
    pub synced: bool,
    /// Per-stream monotone counter; lets the UI drop a stale frame that
    /// arrives after a mutation reply.
    #[serde(default)]
    pub revision: u64,
    /// Effective roots in `(orderKey, id)` order.
    #[serde(default)]
    pub root_work_item_ids: Vec<String>,
    /// Every item (archived included, nothing filtered) in depth-first
    /// pre-order of the repaired tree, roots first in root order.
    #[serde(default)]
    pub items: Vec<WorkItemView>,
    /// Every assignment row of this Space with its materialized state,
    /// sorted by Chat id.
    #[serde(default)]
    pub assignments: Vec<WorkItemChatAssignmentView>,
    /// Live Chats of this Space with no effective association (no assignment,
    /// an explicit disassociation, or a `danglingWorkItem` / `spaceMismatch`
    /// one), sorted. Chats themselves come from `WatchChats`; the snapshot
    /// only carries ids. Projectless Chats are never listed here (the "All
    /// spaces" Outline view adds them client-side).
    #[serde(default)]
    pub unassociated_chat_ids: Vec<String>,
    /// Sorted by reference id.
    #[serde(default)]
    pub external_references: Vec<WorkItemExternalReferenceView>,
    /// Sorted.
    #[serde(default)]
    pub repairs: Vec<WorkOutlineRepair>,
}

impl WorkOutlineSnapshot {
    /// A snapshot with no content for a Space that is not (yet) available.
    pub fn unavailable(space_id: impl Into<String>, synced: bool, revision: u64) -> Self {
        Self {
            space_id: space_id.into(),
            available: false,
            synced,
            revision,
            ..Self::default()
        }
    }

    /// Equal in everything except the per-stream `revision` counter: the
    /// frame-dedupe comparison.
    pub fn same_content_as(&self, other: &Self) -> bool {
        self.space_id == other.space_id
            && self.available == other.available
            && self.synced == other.synced
            && self.root_work_item_ids == other.root_work_item_ids
            && self.items == other.items
            && self.assignments == other.assignments
            && self.unassociated_chat_ids == other.unassociated_chat_ids
            && self.external_references == other.external_references
            && self.repairs == other.repairs
    }

    pub fn item(&self, id: &str) -> Option<&WorkItemView> {
        self.items.iter().find(|item| item.id == id)
    }
}

// --------------------------------------------------------------------------
// Requests and replies
// --------------------------------------------------------------------------
//
// Transport (idempotency audit section 3): `WatchWorkOutline` is a stream and
// every mutation is one `WorkOutlineMutate` call carrying a [`WorkOutlineMutation`]
// (tag `op`). Both are pure registry operations served by the local replica;
// neither is device-forwardable. Every mutation carries a client-generated
// `operationId` (UUID) and replays idempotently. The originating actor is
// derived by the engine, never sent by the client.

/// Params of `WatchWorkOutline`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchWorkOutlineParams {
    pub space_id: String,
}

/// `createWorkItem`. Reply: [`CreateWorkItemResult`]. The new item's id is
/// `derive_id(operationId, "item")`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorkItemParams {
    pub space_id: String,
    /// `None` = a Space-level root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_work_item_id: Option<String>,
    #[serde(default)]
    pub placement: WorkOutlinePlacement,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<WorkItemStatus>,
    pub operation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorkItemResult {
    pub work_item_id: String,
}

/// The editable subset of a work item. `title`, `status`, and
/// `inheritParentInstructions` are "absent = unchanged"; `description` is
/// tri-state (absent = unchanged, `null` = clear, value = set).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemChanges {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "FieldPatch::is_unchanged")]
    pub description: FieldPatch<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<WorkItemStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherit_parent_instructions: Option<bool>,
}

/// `updateWorkItem`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateWorkItemParams {
    pub work_item_id: String,
    #[serde(default)]
    pub changes: WorkItemChanges,
    pub operation_id: String,
}

/// `moveWorkItem`. A move within the same parent is a reorder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveWorkItemParams {
    pub work_item_id: String,
    /// `None` = the Space root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_parent_work_item_id: Option<String>,
    #[serde(default)]
    pub placement: WorkOutlinePlacement,
    pub operation_id: String,
}

/// `archiveWorkItem`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveWorkItemParams {
    pub work_item_id: String,
    pub descendant_archive_policy: DescendantArchivePolicy,
    pub operation_id: String,
}

/// `restoreWorkItem`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreWorkItemParams {
    pub work_item_id: String,
    pub operation_id: String,
}

/// `assignChatToWorkItem`. The Chat's Space must equal the item's Space;
/// projectless Chats are rejected with `Projectless`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignChatToWorkItemParams {
    pub chat_id: String,
    pub work_item_id: String,
    pub operation_id: String,
}

/// `removeChatFromWorkItem` (writes an explicit disassociation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveChatFromWorkItemParams {
    pub chat_id: String,
    pub operation_id: String,
}

/// `createWorkItemExternalReference`. Reply:
/// [`CreateWorkItemExternalReferenceResult`] (the content-derived
/// [`external_reference_id`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorkItemExternalReferenceParams {
    pub work_item_id: String,
    pub provider: String,
    pub reference_type: String,
    pub external_id: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_title: Option<String>,
    pub operation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorkItemExternalReferenceResult {
    pub reference_id: String,
}

/// `removeWorkItemExternalReference`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveWorkItemExternalReferenceParams {
    pub reference_id: String,
    pub operation_id: String,
}

/// `repairWorkOutline`: persist the derived repairs (cycle victims, bad order
/// keys) for one Space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairWorkOutlineParams {
    pub space_id: String,
    pub operation_id: String,
}

/// The params of `WorkOutlineMutate`: one tagged enum (`op`, camelCase) so a
/// peer that predates an operation answers a single, recognizable error.
/// Each variant's fields sit beside `op`, e.g.
/// `{"op":"moveWorkItem","workItemId":"…","placement":{"type":"last"},"operationId":"…"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum WorkOutlineMutation {
    CreateWorkItem(CreateWorkItemParams),
    UpdateWorkItem(UpdateWorkItemParams),
    MoveWorkItem(MoveWorkItemParams),
    ArchiveWorkItem(ArchiveWorkItemParams),
    RestoreWorkItem(RestoreWorkItemParams),
    AssignChatToWorkItem(AssignChatToWorkItemParams),
    RemoveChatFromWorkItem(RemoveChatFromWorkItemParams),
    CreateWorkItemExternalReference(CreateWorkItemExternalReferenceParams),
    RemoveWorkItemExternalReference(RemoveWorkItemExternalReferenceParams),
    RepairWorkOutline(RepairWorkOutlineParams),
}

impl WorkOutlineMutation {
    /// The idempotency key every variant carries.
    pub fn operation_id(&self) -> &str {
        match self {
            Self::CreateWorkItem(p) => &p.operation_id,
            Self::UpdateWorkItem(p) => &p.operation_id,
            Self::MoveWorkItem(p) => &p.operation_id,
            Self::ArchiveWorkItem(p) => &p.operation_id,
            Self::RestoreWorkItem(p) => &p.operation_id,
            Self::AssignChatToWorkItem(p) => &p.operation_id,
            Self::RemoveChatFromWorkItem(p) => &p.operation_id,
            Self::CreateWorkItemExternalReference(p) => &p.operation_id,
            Self::RemoveWorkItemExternalReference(p) => &p.operation_id,
            Self::RepairWorkOutline(p) => &p.operation_id,
        }
    }
}

/// The reply of `WorkOutlineMutate`. Operations that mint an id set exactly
/// one of the fields (`createWorkItem` -> `workItemId`,
/// `createWorkItemExternalReference` -> `referenceId`); every other operation
/// replies `{}`. [`CreateWorkItemResult`] and
/// [`CreateWorkItemExternalReferenceResult`] decode from the same reply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkOutlineMutateReply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_id: Option<String>,
}

impl WorkOutlineMutateReply {
    /// The `createWorkItem` result; `Invalid` if the host replied without an id.
    pub fn into_create_work_item(self) -> Result<CreateWorkItemResult, WorkOutlineError> {
        self.work_item_id
            .map(|work_item_id| CreateWorkItemResult { work_item_id })
            .ok_or_else(|| {
                WorkOutlineError::new(
                    WorkOutlineErrorCode::Invalid,
                    "createWorkItem reply is missing workItemId",
                )
            })
    }

    /// The `createWorkItemExternalReference` result; `Invalid` if the host
    /// replied without an id.
    pub fn into_create_external_reference(
        self,
    ) -> Result<CreateWorkItemExternalReferenceResult, WorkOutlineError> {
        self.reference_id
            .map(|reference_id| CreateWorkItemExternalReferenceResult { reference_id })
            .ok_or_else(|| {
                WorkOutlineError::new(
                    WorkOutlineErrorCode::Invalid,
                    "createWorkItemExternalReference reply is missing referenceId",
                )
            })
    }
}

// --------------------------------------------------------------------------
// Errors
// --------------------------------------------------------------------------

/// Machine-readable failure code shared by every Work Outline operation.
/// Lenient: an unknown code from a newer peer reads as `unsupported`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkOutlineErrorCode {
    /// The work item, Chat, Space, or reference does not exist (or is tombstoned).
    NotFound,
    /// The operation would link rows of different Spaces.
    CrossSpace,
    /// The move would make an item its own ancestor.
    Cycle,
    /// The Chat has no Space and cannot be assigned.
    Projectless,
    /// The target is archived and the operation needs it live.
    Archived,
    /// A size, count, or op-byte limit in [`limits`] was exceeded.
    LimitExceeded,
    /// The `operationId` was already used with different inputs.
    OperationConflict,
    /// Malformed or out-of-range input.
    Invalid,
    /// The [`DISABLE_WRITES_ENV`] kill switch is on.
    WritesDisabled,
    /// The host lacks the Work Outline capability or operation.
    #[serde(other)]
    Unsupported,
}

impl WorkOutlineErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "notFound",
            Self::CrossSpace => "crossSpace",
            Self::Cycle => "cycle",
            Self::Projectless => "projectless",
            Self::Archived => "archived",
            Self::LimitExceeded => "limitExceeded",
            Self::OperationConflict => "operationConflict",
            Self::Invalid => "invalid",
            Self::WritesDisabled => "writesDisabled",
            Self::Unsupported => "unsupported",
        }
    }

    pub fn parse(code: &str) -> Self {
        match code {
            "notFound" => Self::NotFound,
            "crossSpace" => Self::CrossSpace,
            "cycle" => Self::Cycle,
            "projectless" => Self::Projectless,
            "archived" => Self::Archived,
            "limitExceeded" => Self::LimitExceeded,
            "operationConflict" => Self::OperationConflict,
            "invalid" => Self::Invalid,
            "writesDisabled" => Self::WritesDisabled,
            _ => Self::Unsupported,
        }
    }
}

/// A failed Work Outline operation. RPC errors travel as plain strings
/// (`ServerFrame.err`), so the engine renders this with
/// [`WorkOutlineError::to_wire_message`] and clients recover it with
/// [`WorkOutlineError::from_wire_message`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkOutlineError {
    pub code: WorkOutlineErrorCode,
    #[serde(default)]
    pub message: String,
}

/// Prefix of a wire-encoded [`WorkOutlineError`].
const ERROR_PREFIX: &str = "work-outline/";

impl WorkOutlineError {
    pub fn new(code: WorkOutlineErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// `work-outline/<code>: <message>`.
    pub fn to_wire_message(&self) -> String {
        format!("{ERROR_PREFIX}{}: {}", self.code.as_str(), self.message)
    }

    /// Inverse of [`Self::to_wire_message`]; `None` for any other error string
    /// (for example `unknown method: …` from a host that predates the feature).
    pub fn from_wire_message(text: &str) -> Option<Self> {
        let rest = text.strip_prefix(ERROR_PREFIX)?;
        let (code, message) = rest.split_once(": ").unwrap_or((rest, ""));
        Some(Self {
            code: WorkOutlineErrorCode::parse(code),
            message: message.to_owned(),
        })
    }
}

impl std::fmt::Display for WorkOutlineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WorkOutlineError {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round_trip<T>(value: &T, expected: serde_json::Value)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        assert_eq!(serde_json::to_value(value).unwrap(), expected);
        assert_eq!(&serde_json::from_value::<T>(expected).unwrap(), value);
    }

    fn actor() -> WorkOutlineActor {
        WorkOutlineActor {
            kind: WorkOutlineActorKind::Agent,
            device_id: "dev-1".into(),
            chat_id: Some("chat-1".into()),
            message_id: Some("msg-1".into()),
        }
    }

    #[test]
    fn derive_id_is_pinned_and_role_separated() {
        // hex(sha256("op-1" 0x00 "chat"))[..32]
        assert_eq!(
            derive_id("op-1", roles::CHAT),
            "1eebfbaab6f831f687fd763a12a083da"
        );
        assert_eq!(
            derive_id("op-1", roles::MESSAGE),
            "acc82841aa3d4961136a928cb097f57a"
        );
        assert_ne!(derive_id("op-1", "chat"), derive_id("op-2", "chat"));
        assert_eq!(derive_id("op-1", "chat").len(), 32);
        // The NUL separator prevents `("a","bc")` colliding with `("ab","c")`.
        assert_ne!(derive_id("a", "bc"), derive_id("ab", "c"));
    }

    #[test]
    fn external_reference_id_is_pinned_content_derived_and_nul_separated() {
        assert_eq!(
            external_reference_id("wi-1", "github", "pull_request", "123"),
            "1d7c3d35d998cefd274c944e5546c8a5f3c7a042eeb937f9c077f8b27daec570"
        );
        assert_ne!(
            external_reference_id("a", "bc", "d", "e"),
            external_reference_id("ab", "c", "d", "e")
        );
    }

    #[test]
    fn actor_wire_shape_and_registry_aliases() {
        round_trip(
            &actor(),
            json!({"kind":"agent","deviceId":"dev-1","chatId":"chat-1","messageId":"msg-1"}),
        );
        // The registry row spells the fields `k` and `msgId`.
        let from_row: WorkOutlineActor =
            serde_json::from_value(json!({"k":"user","deviceId":"d","msgId":"m"})).unwrap();
        assert_eq!(from_row.kind, WorkOutlineActorKind::User);
        assert_eq!(from_row.message_id.as_deref(), Some("m"));
        // Unknown kind degrades to system; everything is optional.
        let odd: WorkOutlineActor = serde_json::from_value(json!({"kind":"robot"})).unwrap();
        assert_eq!(odd.kind, WorkOutlineActorKind::System);
        assert_eq!(odd.device_id, "");
    }

    #[test]
    fn enums_use_exact_camel_case_spellings() {
        let v = |x: &dyn erased::Ser| x.to_value();
        mod erased {
            pub trait Ser {
                fn to_value(&self) -> serde_json::Value;
            }
            impl<T: serde::Serialize> Ser for T {
                fn to_value(&self) -> serde_json::Value {
                    serde_json::to_value(self).unwrap()
                }
            }
        }
        assert_eq!(v(&WorkItemStatus::Pending), json!("pending"));
        assert_eq!(v(&WorkItemStatus::Active), json!("active"));
        assert_eq!(v(&WorkItemStatus::Completed), json!("completed"));
        assert_eq!(
            v(&WorkItemEffectiveStatus::UserActionRequired),
            json!("userActionRequired")
        );
        assert_eq!(v(&WorkItemArchiveCause::SelfArchived), json!("self"));
        assert_eq!(v(&WorkItemArchiveCause::Cascade), json!("cascade"));
        assert_eq!(v(&WorkOutlineActorKind::System), json!("system"));
        for (state, name) in [
            (WorkItemChatAssignmentState::Associated, "associated"),
            (WorkItemChatAssignmentState::Unassociated, "unassociated"),
            (WorkItemChatAssignmentState::DanglingChat, "danglingChat"),
            (
                WorkItemChatAssignmentState::DanglingWorkItem,
                "danglingWorkItem",
            ),
            (WorkItemChatAssignmentState::SpaceMismatch, "spaceMismatch"),
        ] {
            assert_eq!(v(&state), json!(name));
            assert_eq!(
                serde_json::from_value::<WorkItemChatAssignmentState>(json!(name)).unwrap(),
                state
            );
        }
    }

    #[test]
    fn snapshot_enums_are_lenient() {
        let status: WorkItemStatus = serde_json::from_value(json!("blocked")).unwrap();
        assert_eq!(status, WorkItemStatus::Pending);
        let effective: WorkItemEffectiveStatus = serde_json::from_value(json!("snoozed")).unwrap();
        assert_eq!(effective, WorkItemEffectiveStatus::Pending);
        let cause: WorkItemArchiveCause = serde_json::from_value(json!("merged")).unwrap();
        assert_eq!(cause, WorkItemArchiveCause::SelfArchived);
        let state: WorkItemChatAssignmentState = serde_json::from_value(json!("moved")).unwrap();
        assert_eq!(state, WorkItemChatAssignmentState::Unassociated);
        let repair: WorkOutlineRepair =
            serde_json::from_value(json!({"type":"futureRepair","x":1})).unwrap();
        assert_eq!(repair, WorkOutlineRepair::Unknown);
        let code: WorkOutlineErrorCode = serde_json::from_value(json!("brandNew")).unwrap();
        assert_eq!(code, WorkOutlineErrorCode::Unsupported);
    }

    #[test]
    fn request_enums_are_strict() {
        assert!(serde_json::from_value::<WorkOutlinePlacement>(json!({"type":"middle"})).is_err());
        assert!(
            serde_json::from_value::<DescendantArchivePolicy>(json!({"type":"delete"})).is_err()
        );
    }

    #[test]
    fn placement_wire_shapes() {
        round_trip(&WorkOutlinePlacement::First, json!({"type":"first"}));
        round_trip(&WorkOutlinePlacement::Last, json!({"type":"last"}));
        round_trip(
            &WorkOutlinePlacement::Before {
                sibling_work_item_id: "w-1".into(),
            },
            json!({"type":"before","siblingWorkItemId":"w-1"}),
        );
        round_trip(
            &WorkOutlinePlacement::After {
                sibling_work_item_id: "w-2".into(),
            },
            json!({"type":"after","siblingWorkItemId":"w-2"}),
        );
    }

    #[test]
    fn descendant_archive_policy_wire_shapes() {
        round_trip(
            &DescendantArchivePolicy::ArchiveDescendants,
            json!({"type":"archiveDescendants"}),
        );
        round_trip(
            &DescendantArchivePolicy::MoveDescendants {
                destination_parent_work_item_id: None,
            },
            json!({"type":"moveDescendants","destinationParentWorkItemId":null}),
        );
        round_trip(
            &DescendantArchivePolicy::MoveDescendants {
                destination_parent_work_item_id: Some("w-9".into()),
            },
            json!({"type":"moveDescendants","destinationParentWorkItemId":"w-9"}),
        );
        // A missing destination also means the Space root.
        assert_eq!(
            serde_json::from_value::<DescendantArchivePolicy>(json!({"type":"moveDescendants"}))
                .unwrap(),
            DescendantArchivePolicy::MoveDescendants {
                destination_parent_work_item_id: None
            }
        );
    }

    #[test]
    fn work_item_changes_description_is_tri_state() {
        let absent: WorkItemChanges = serde_json::from_value(json!({"title":"T"})).unwrap();
        assert_eq!(absent.description, FieldPatch::Unchanged);
        assert_eq!(serde_json::to_value(&absent).unwrap(), json!({"title":"T"}));

        let cleared: WorkItemChanges = serde_json::from_value(json!({"description":null})).unwrap();
        assert_eq!(cleared.description, FieldPatch::Clear);
        assert_eq!(
            serde_json::to_value(&cleared).unwrap(),
            json!({"description":null})
        );

        let set: WorkItemChanges = serde_json::from_value(json!({"description":"d"})).unwrap();
        assert_eq!(set.description, FieldPatch::Set("d".into()));
        assert_eq!(
            serde_json::to_value(&set).unwrap(),
            json!({"description":"d"})
        );

        let all = WorkItemChanges {
            title: Some("T".into()),
            description: FieldPatch::Set("d".into()),
            status: Some(WorkItemStatus::Completed),
            inherit_parent_instructions: Some(false),
        };
        round_trip(
            &all,
            json!({"title":"T","description":"d","status":"completed","inheritParentInstructions":false}),
        );
        assert_eq!(
            serde_json::to_value(WorkItemChanges::default()).unwrap(),
            json!({})
        );
    }

    #[test]
    fn snapshot_json_field_names_are_pinned() {
        let snapshot = WorkOutlineSnapshot {
            space_id: "space-1".into(),
            available: true,
            synced: true,
            revision: 7,
            root_work_item_ids: vec!["w-1".into()],
            items: vec![WorkItemView {
                id: "w-1".into(),
                space_id: "space-1".into(),
                parent_work_item_id: Some("w-0".into()),
                raw_parent_work_item_id: Some("w-x".into()),
                order_key: "a0".into(),
                title: "Ship".into(),
                description: Some("Ship it".into()),
                status: WorkItemStatus::Active,
                effective_status: WorkItemEffectiveStatus::UserActionRequired,
                archived_at: Some(1_700_000_000_500),
                archived_by: Some(actor()),
                archive_cause: Some(WorkItemArchiveCause::Cascade),
                inherit_parent_instructions: false,
                created_by: Some(actor()),
                created_at: 1_700_000_000_000,
                updated_at: 1_700_000_000_100,
                child_ids: vec!["w-2".into()],
                depth: 1,
                chat_ids: vec!["chat-1".into()],
                external_reference_ids: vec!["ref-1".into()],
                rollup: WorkItemRollup {
                    pending_count: 1,
                    active_count: 2,
                    completed_count: 3,
                    user_action_required_count: 4,
                    unresolved_user_action_count: 5,
                    associated_chat_count: 6,
                },
            }],
            assignments: vec![WorkItemChatAssignmentView {
                chat_id: "chat-1".into(),
                space_id: "space-1".into(),
                work_item_id: Some("w-1".into()),
                assigned_by: Some(actor()),
                assigned_at: 1_700_000_000_200,
                state: WorkItemChatAssignmentState::DanglingWorkItem,
            }],
            unassociated_chat_ids: vec!["chat-2".into()],
            external_references: vec![WorkItemExternalReferenceView {
                id: "ref-1".into(),
                space_id: "space-1".into(),
                work_item_id: "w-1".into(),
                provider: "github".into(),
                reference_type: "pullRequest".into(),
                external_id: "acme/repo#1".into(),
                url: "https://github.com/acme/repo/pull/1".into(),
                cached_title: Some("Fix".into()),
                created_by: Some(actor()),
                created_at: 1_700_000_000_300,
            }],
            repairs: vec![WorkOutlineRepair::CycleBroken {
                victim: "w-1".into(),
                former_parent: "w-2".into(),
                members: vec!["w-1".into(), "w-2".into()],
            }],
        };
        let actor_json =
            json!({"kind":"agent","deviceId":"dev-1","chatId":"chat-1","messageId":"msg-1"});
        round_trip(
            &snapshot,
            json!({
                "spaceId": "space-1",
                "available": true,
                "synced": true,
                "revision": 7,
                "rootWorkItemIds": ["w-1"],
                "items": [{
                    "id": "w-1",
                    "spaceId": "space-1",
                    "parentWorkItemId": "w-0",
                    "rawParentWorkItemId": "w-x",
                    "orderKey": "a0",
                    "title": "Ship",
                    "description": "Ship it",
                    "status": "active",
                    "effectiveStatus": "userActionRequired",
                    "archivedAt": 1_700_000_000_500i64,
                    "archivedBy": actor_json,
                    "archiveCause": "cascade",
                    "inheritParentInstructions": false,
                    "createdBy": actor_json,
                    "createdAt": 1_700_000_000_000i64,
                    "updatedAt": 1_700_000_000_100i64,
                    "childIds": ["w-2"],
                    "depth": 1,
                    "chatIds": ["chat-1"],
                    "externalReferenceIds": ["ref-1"],
                    "rollup": {
                        "pendingCount": 1,
                        "activeCount": 2,
                        "completedCount": 3,
                        "userActionRequiredCount": 4,
                        "unresolvedUserActionCount": 5,
                        "associatedChatCount": 6
                    }
                }],
                "assignments": [{
                    "chatId": "chat-1",
                    "spaceId": "space-1",
                    "workItemId": "w-1",
                    "assignedBy": actor_json,
                    "assignedAt": 1_700_000_000_200i64,
                    "state": "danglingWorkItem"
                }],
                "unassociatedChatIds": ["chat-2"],
                "externalReferences": [{
                    "id": "ref-1",
                    "spaceId": "space-1",
                    "workItemId": "w-1",
                    "provider": "github",
                    "referenceType": "pullRequest",
                    "externalId": "acme/repo#1",
                    "url": "https://github.com/acme/repo/pull/1",
                    "cachedTitle": "Fix",
                    "createdBy": actor_json,
                    "createdAt": 1_700_000_000_300i64
                }],
                "repairs": [{
                    "type": "cycleBroken",
                    "victim": "w-1",
                    "formerParent": "w-2",
                    "members": ["w-1", "w-2"]
                }]
            }),
        );
    }

    #[test]
    fn minimal_snapshot_and_item_decode_with_defaults() {
        // Only identity fields: an older or sparser peer must still decode.
        let snapshot: WorkOutlineSnapshot =
            serde_json::from_value(json!({"spaceId":"s","items":[{"id":"w","spaceId":"s"}]}))
                .unwrap();
        assert!(!snapshot.available && !snapshot.synced);
        assert_eq!(snapshot.revision, 0);
        let item = &snapshot.items[0];
        assert_eq!(item.status, WorkItemStatus::Pending);
        assert_eq!(item.effective_status, WorkItemEffectiveStatus::Pending);
        assert!(item.inherit_parent_instructions, "missing reads as true");
        assert_eq!(item.title, "");
        assert!(item.child_ids.is_empty() && item.parent_work_item_id.is_none());
        // Unknown extra fields from a newer peer are ignored.
        let newer: WorkOutlineSnapshot =
            serde_json::from_value(json!({"spaceId":"s","instructions":[1,2],"future":{}}))
                .unwrap();
        assert_eq!(newer.space_id, "s");
    }

    #[test]
    fn same_content_ignores_only_the_revision() {
        let a = WorkOutlineSnapshot::unavailable("s", true, 1);
        let mut b = a.clone();
        b.revision = 9;
        assert!(a.same_content_as(&b) && a != b);
        b.synced = false;
        assert!(!a.same_content_as(&b));
    }

    #[test]
    fn unavailable_snapshot_serializes_empty_lists() {
        let value = serde_json::to_value(WorkOutlineSnapshot::unavailable("s", false, 3)).unwrap();
        assert_eq!(
            value,
            json!({
                "spaceId": "s",
                "available": false,
                "synced": false,
                "revision": 3,
                "rootWorkItemIds": [],
                "items": [],
                "assignments": [],
                "unassociatedChatIds": [],
                "externalReferences": [],
                "repairs": []
            })
        );
    }

    #[test]
    fn repair_wire_shapes_cover_every_variant() {
        let cases = [
            (
                WorkOutlineRepair::Malformed {
                    row_kind: "workItems".into(),
                    id: "w".into(),
                },
                json!({"type":"malformed","rowKind":"workItems","id":"w"}),
            ),
            (
                WorkOutlineRepair::MissingSpace {
                    row_kind: "workItems".into(),
                    id: "w".into(),
                },
                json!({"type":"missingSpace","rowKind":"workItems","id":"w"}),
            ),
            (
                WorkOutlineRepair::ParentMissing {
                    work_item_id: "w".into(),
                    parent_work_item_id: "p".into(),
                },
                json!({"type":"parentMissing","workItemId":"w","parentWorkItemId":"p"}),
            ),
            (
                WorkOutlineRepair::CrossSpaceParent {
                    work_item_id: "w".into(),
                    parent_work_item_id: "p".into(),
                },
                json!({"type":"crossSpaceParent","workItemId":"w","parentWorkItemId":"p"}),
            ),
            (
                WorkOutlineRepair::CycleBroken {
                    victim: "a".into(),
                    former_parent: "b".into(),
                    members: vec!["a".into(), "b".into()],
                },
                json!({"type":"cycleBroken","victim":"a","formerParent":"b","members":["a","b"]}),
            ),
            (
                WorkOutlineRepair::BadOrderKey {
                    work_item_id: "w".into(),
                },
                json!({"type":"badOrderKey","workItemId":"w"}),
            ),
            (
                WorkOutlineRepair::UnknownStatus {
                    work_item_id: "w".into(),
                },
                json!({"type":"unknownStatus","workItemId":"w"}),
            ),
            (
                WorkOutlineRepair::MissingTitle {
                    work_item_id: "w".into(),
                },
                json!({"type":"missingTitle","workItemId":"w"}),
            ),
            (
                WorkOutlineRepair::AssignmentSpaceMismatch {
                    chat_id: "c".into(),
                },
                json!({"type":"assignmentSpaceMismatch","chatId":"c"}),
            ),
            (
                WorkOutlineRepair::DanglingChat {
                    chat_id: "c".into(),
                },
                json!({"type":"danglingChat","chatId":"c"}),
            ),
            (
                WorkOutlineRepair::DanglingWorkItem {
                    chat_id: "c".into(),
                    work_item_id: "w".into(),
                },
                json!({"type":"danglingWorkItem","chatId":"c","workItemId":"w"}),
            ),
            (
                WorkOutlineRepair::OrphanReference {
                    reference_id: "r".into(),
                    work_item_id: "w".into(),
                },
                json!({"type":"orphanReference","referenceId":"r","workItemId":"w"}),
            ),
        ];
        for (repair, expected) in cases {
            round_trip(&repair, expected);
        }
    }

    #[test]
    fn watch_params_field_names_are_pinned() {
        round_trip(
            &WatchWorkOutlineParams {
                space_id: "s".into(),
            },
            json!({"spaceId":"s"}),
        );
    }

    /// One pin per mutation: exact `op` spelling plus every field name.
    #[test]
    fn mutation_field_names_are_pinned() {
        let create = WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: "s".into(),
            parent_work_item_id: Some("p".into()),
            placement: WorkOutlinePlacement::After {
                sibling_work_item_id: "x".into(),
            },
            title: "T".into(),
            description: Some("D".into()),
            status: Some(WorkItemStatus::Active),
            operation_id: "op".into(),
        });
        round_trip(
            &create,
            json!({
                "op":"createWorkItem","spaceId":"s","parentWorkItemId":"p",
                "placement":{"type":"after","siblingWorkItemId":"x"},
                "title":"T","description":"D","status":"active","operationId":"op"
            }),
        );
        // Optional fields absent: a minimal create defaults placement to last.
        let minimal: WorkOutlineMutation = serde_json::from_value(
            json!({"op":"createWorkItem","spaceId":"s","title":"T","operationId":"op"}),
        )
        .unwrap();
        let WorkOutlineMutation::CreateWorkItem(minimal) = minimal else {
            panic!()
        };
        assert_eq!(minimal.placement, WorkOutlinePlacement::Last);
        assert!(minimal.parent_work_item_id.is_none() && minimal.status.is_none());

        round_trip(
            &WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
                work_item_id: "w".into(),
                changes: WorkItemChanges {
                    title: Some("T".into()),
                    description: FieldPatch::Clear,
                    status: Some(WorkItemStatus::Completed),
                    inherit_parent_instructions: Some(true),
                },
                operation_id: "op".into(),
            }),
            json!({
                "op":"updateWorkItem","workItemId":"w",
                "changes":{"title":"T","description":null,"status":"completed","inheritParentInstructions":true},
                "operationId":"op"
            }),
        );
        round_trip(
            &WorkOutlineMutation::MoveWorkItem(MoveWorkItemParams {
                work_item_id: "w".into(),
                destination_parent_work_item_id: Some("p".into()),
                placement: WorkOutlinePlacement::First,
                operation_id: "op".into(),
            }),
            json!({
                "op":"moveWorkItem","workItemId":"w","destinationParentWorkItemId":"p",
                "placement":{"type":"first"},"operationId":"op"
            }),
        );
        round_trip(
            &WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
                work_item_id: "w".into(),
                descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
                operation_id: "op".into(),
            }),
            json!({
                "op":"archiveWorkItem","workItemId":"w",
                "descendantArchivePolicy":{"type":"archiveDescendants"},"operationId":"op"
            }),
        );
        round_trip(
            &WorkOutlineMutation::RestoreWorkItem(RestoreWorkItemParams {
                work_item_id: "w".into(),
                operation_id: "op".into(),
            }),
            json!({"op":"restoreWorkItem","workItemId":"w","operationId":"op"}),
        );
        round_trip(
            &WorkOutlineMutation::AssignChatToWorkItem(AssignChatToWorkItemParams {
                chat_id: "c".into(),
                work_item_id: "w".into(),
                operation_id: "op".into(),
            }),
            json!({"op":"assignChatToWorkItem","chatId":"c","workItemId":"w","operationId":"op"}),
        );
        round_trip(
            &WorkOutlineMutation::RemoveChatFromWorkItem(RemoveChatFromWorkItemParams {
                chat_id: "c".into(),
                operation_id: "op".into(),
            }),
            json!({"op":"removeChatFromWorkItem","chatId":"c","operationId":"op"}),
        );
        round_trip(
            &WorkOutlineMutation::CreateWorkItemExternalReference(
                CreateWorkItemExternalReferenceParams {
                    work_item_id: "w".into(),
                    provider: "github".into(),
                    reference_type: "pullRequest".into(),
                    external_id: "1".into(),
                    url: "https://example.com/1".into(),
                    cached_title: Some("Title".into()),
                    operation_id: "op".into(),
                },
            ),
            json!({
                "op":"createWorkItemExternalReference","workItemId":"w","provider":"github",
                "referenceType":"pullRequest","externalId":"1","url":"https://example.com/1",
                "cachedTitle":"Title","operationId":"op"
            }),
        );
        round_trip(
            &WorkOutlineMutation::RemoveWorkItemExternalReference(
                RemoveWorkItemExternalReferenceParams {
                    reference_id: "r".into(),
                    operation_id: "op".into(),
                },
            ),
            json!({"op":"removeWorkItemExternalReference","referenceId":"r","operationId":"op"}),
        );
        round_trip(
            &WorkOutlineMutation::RepairWorkOutline(RepairWorkOutlineParams {
                space_id: "s".into(),
                operation_id: "op".into(),
            }),
            json!({"op":"repairWorkOutline","spaceId":"s","operationId":"op"}),
        );
    }

    #[test]
    fn every_mutation_exposes_its_operation_id() {
        let m: WorkOutlineMutation = serde_json::from_value(
            json!({"op":"restoreWorkItem","workItemId":"w","operationId":"op-9"}),
        )
        .unwrap();
        assert_eq!(m.operation_id(), "op-9");
    }

    #[test]
    fn unknown_mutation_op_and_missing_operation_id_are_rejected() {
        assert!(serde_json::from_value::<WorkOutlineMutation>(json!({"op":"explode"})).is_err());
        assert!(
            serde_json::from_value::<WorkOutlineMutation>(
                json!({"op":"restoreWorkItem","workItemId":"w"})
            )
            .is_err()
        );
    }

    #[test]
    fn replies_are_pinned() {
        round_trip(
            &CreateWorkItemResult {
                work_item_id: "w".into(),
            },
            json!({"workItemId":"w"}),
        );
        round_trip(
            &CreateWorkItemExternalReferenceResult {
                reference_id: "r".into(),
            },
            json!({"referenceId":"r"}),
        );
        round_trip(&WorkOutlineMutateReply::default(), json!({}));
        round_trip(
            &WorkOutlineMutateReply {
                work_item_id: Some("w".into()),
                reference_id: None,
            },
            json!({"workItemId":"w"}),
        );
        assert_eq!(
            WorkOutlineMutateReply {
                work_item_id: Some("w".into()),
                reference_id: None
            }
            .into_create_work_item()
            .unwrap()
            .work_item_id,
            "w"
        );
        assert_eq!(
            WorkOutlineMutateReply::default()
                .into_create_external_reference()
                .unwrap_err()
                .code,
            WorkOutlineErrorCode::Invalid
        );
        // A combined reply decodes as each typed result.
        let reply = json!({"workItemId":"w"});
        assert_eq!(
            serde_json::from_value::<CreateWorkItemResult>(reply)
                .unwrap()
                .work_item_id,
            "w"
        );
    }

    #[test]
    fn error_codes_use_pinned_spellings_and_wire_message_round_trips() {
        let codes = [
            (WorkOutlineErrorCode::NotFound, "notFound"),
            (WorkOutlineErrorCode::CrossSpace, "crossSpace"),
            (WorkOutlineErrorCode::Cycle, "cycle"),
            (WorkOutlineErrorCode::Projectless, "projectless"),
            (WorkOutlineErrorCode::Archived, "archived"),
            (WorkOutlineErrorCode::LimitExceeded, "limitExceeded"),
            (WorkOutlineErrorCode::OperationConflict, "operationConflict"),
            (WorkOutlineErrorCode::Invalid, "invalid"),
            (WorkOutlineErrorCode::WritesDisabled, "writesDisabled"),
            (WorkOutlineErrorCode::Unsupported, "unsupported"),
        ];
        for (code, name) in codes {
            assert_eq!(serde_json::to_value(code).unwrap(), json!(name));
            assert_eq!(code.as_str(), name);
            assert_eq!(WorkOutlineErrorCode::parse(name), code);
            let error = WorkOutlineError::new(code, "because: reasons");
            let wire = error.to_wire_message();
            assert_eq!(wire, format!("work-outline/{name}: because: reasons"));
            assert_eq!(WorkOutlineError::from_wire_message(&wire), Some(error));
        }
        round_trip(
            &WorkOutlineError::new(WorkOutlineErrorCode::Cycle, "would create a cycle"),
            json!({"code":"cycle","message":"would create a cycle"}),
        );
        assert_eq!(
            WorkOutlineError::from_wire_message("unknown method: X"),
            None
        );
        assert_eq!(
            WorkOutlineError::from_wire_message("work-outline/invalid"),
            Some(WorkOutlineError::new(WorkOutlineErrorCode::Invalid, ""))
        );
    }

    #[test]
    fn limits_match_the_contract() {
        assert_eq!(limits::MAX_TITLE_CHARS, 256);
        assert_eq!(limits::MAX_DESCRIPTION_CHARS, 8_000);
        assert_eq!(limits::MAX_CHILDREN_PER_PARENT, 2_000);
        assert_eq!(limits::MAX_ORDER_KEY_LEN, 64);
        assert!(limits::MAX_OP_UTF16_UNITS < 16 * 1024);
    }
}
