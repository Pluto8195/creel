//! Work Outline instructions (Milestone 2): wire types for instructions,
//! effective instructions, immutable revisions, revision applications, and
//! harness delivery, plus the PURE functions every layer shares:
//!
//! - [`resolve_effective_instructions`]: Space-root, then inherited, then
//!   local instructions over the materialized (repaired) tree;
//! - [`render_instruction_envelope`]: the versioned, golden-tested text a
//!   harness receives (envelope version 1, format documented on the function);
//! - [`canonical_revision_hash`] / [`envelope_sha256`] /
//!   [`build_revision_body`] / [`verify_revision_body`]: revision integrity;
//! - [`compare_instruction_revisions`]: entry-level and line-level diff.
//!
//! Binding references: `docs/plans/work-outline/m0-contract.md` section 5 and
//! "Milestone 2 contract notes", `m0-registry-persistence.md` 7.2 and 7.7-7.9,
//! `m0-instruction-delivery.md` sections 5-7. Wire conventions match
//! `work_outline.rs`: camelCase fields, defaulted non-identity fields,
//! lenient snapshot enums, strict request enums.
//!
//! Nothing here performs I/O or reads a clock: two calls with equal inputs
//! return equal outputs on every device.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::agent::HarnessId;
use crate::work_outline::{
    WorkItemView, WorkOutlineActor, WorkOutlineError, WorkOutlineErrorCode, limits,
};

/// The only envelope version this build renders.
pub const INSTRUCTION_ENVELOPE_VERSION: u32 = 1;

/// The precedence rule shown by the Effective Instructions view (fixed text;
/// [`EffectiveInstructions::precedence_rule`]).
pub const INSTRUCTION_PRECEDENCE_RULE: &str = "Space-root instructions apply first, then \
inherited work-item instructions from the inheritance boundary down to the parent, then this \
item's own instructions. Later, more specific instructions take precedence over earlier ones. \
Work Outline instructions supplement, and never override, repository, global, skill, or \
runtime-safety instructions.";

/// `scopeTitle` of Space-root entries (the Space name is not part of the
/// outline; the label is fixed so revisions do not drift on a Space rename).
pub const SPACE_ROOT_SCOPE_TITLE: &str = "Space root";

/// Title used for a work item whose title is missing or blank.
const UNTITLED: &str = "Untitled";

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

// --------------------------------------------------------------------------
// Instructions
// --------------------------------------------------------------------------

/// Where an instruction lives. Internally tagged by `type`:
/// `{"type":"spaceRoot"}` or `{"type":"workItem","workItemId":"…"}`. Strict
/// (request enum). The Space of a Space-root instruction is carried next to
/// the scope (for example [`crate::CreateWorkItemInstructionParams::space_id`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum InstructionScope {
    SpaceRoot,
    #[serde(rename_all = "camelCase")]
    WorkItem {
        work_item_id: String,
    },
}

/// Chat excerpt provenance of an instruction (immutable once set at
/// creation). `excerpt` is a copy of the selected source text (at most
/// [`limits::MAX_INSTRUCTION_EXCERPT_CHARS`]); `contentHash` is
/// `sha256(excerpt)` hex, informational.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
}

impl InstructionSource {
    /// The same provenance without the copied excerpt: what revisions and
    /// effective entries carry (the excerpt would blow the revision budget;
    /// `contentHash` still identifies it).
    pub fn without_excerpt(&self) -> Self {
        Self {
            excerpt: None,
            ..self.clone()
        }
    }
}

/// One materialized `workItemInstructions` row (archived ones included,
/// flagged by `archivedAt`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemInstructionView {
    pub id: String,
    #[serde(default)]
    pub space_id: String,
    /// `None` = a Space-root instruction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub order_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<WorkOutlineActor>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<InstructionSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_proposal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_by: Option<WorkOutlineActor>,
}

impl WorkItemInstructionView {
    pub fn archived(&self) -> bool {
        self.archived_at.is_some()
    }

    pub fn scope(&self) -> InstructionScope {
        match &self.work_item_id {
            None => InstructionScope::SpaceRoot,
            Some(work_item_id) => InstructionScope::WorkItem {
                work_item_id: work_item_id.clone(),
            },
        }
    }

    /// Provenance as carried by effective entries and revisions.
    pub fn provenance(&self) -> InstructionProvenance {
        InstructionProvenance {
            created_by: self.created_by.clone(),
            source: self.source.as_ref().map(InstructionSource::without_excerpt),
            source_proposal_id: self.source_proposal_id.clone(),
        }
    }
}

/// Whether an entry comes from the Space root or a work item. Lenient:
/// unknown reads as `workItem` (a revision whose entry changed this way then
/// fails its hash check and reads as corrupt, which is the fail-closed
/// outcome).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InstructionSourceKind {
    SpaceRoot,
    /// Declared last because `#[serde(other)]` must sit on the last variant.
    #[default]
    #[serde(other)]
    WorkItem,
}

/// Provenance of an effective entry or revision entry: who created the
/// instruction and from where (excerpt omitted, see
/// [`InstructionSource::without_excerpt`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<WorkOutlineActor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<InstructionSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_proposal_id: Option<String>,
}

// --------------------------------------------------------------------------
// Effective instructions
// --------------------------------------------------------------------------

/// One instruction in effective order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveInstructionEntry {
    pub instruction_id: String,
    #[serde(default)]
    pub source_kind: InstructionSourceKind,
    /// The owning work item id, or the Space id for Space-root entries.
    #[serde(default)]
    pub scope_id: String,
    /// The owning item's title, or [`SPACE_ROOT_SCOPE_TITLE`].
    #[serde(default)]
    pub scope_title: String,
    #[serde(default)]
    pub text: String,
    /// 0-based position in the effective list.
    #[serde(default)]
    pub order: u32,
    /// `false` only for the item's own instructions (Space-root entries are
    /// inherited).
    #[serde(default)]
    pub inherited: bool,
    #[serde(default)]
    pub provenance: InstructionProvenance,
}

impl EffectiveInstructionEntry {
    pub fn to_revision_entry(&self) -> InstructionRevisionEntry {
        InstructionRevisionEntry {
            instruction_id: self.instruction_id.clone(),
            source_kind: self.source_kind,
            scope_id: self.scope_id.clone(),
            scope_title: self.scope_title.clone(),
            text: self.text.clone(),
            order: self.order,
            provenance: self.provenance.clone(),
        }
    }
}

/// The limits a revision must satisfy (echoed so the UI can render "N of M").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionBudget {
    pub max_chars: u32,
    pub max_entries: u32,
}

impl Default for InstructionBudget {
    fn default() -> Self {
        Self {
            max_chars: limits::MAX_EFFECTIVE_INSTRUCTION_CHARS as u32,
            max_entries: limits::MAX_REVISION_ENTRIES as u32,
        }
    }
}

/// `resolveEffectiveInstructions(workItemId)`: the ordered instructions a
/// Chat launched from this item would receive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveInstructions {
    pub work_item_id: String,
    /// The nearest ancestor-or-self with `inheritParentInstructions == false`
    /// (inclusive); `None` = inherits up to the Space root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary_work_item_id: Option<String>,
    #[serde(default)]
    pub entries: Vec<EffectiveInstructionEntry>,
    /// [`INSTRUCTION_PRECEDENCE_RULE`].
    #[serde(default)]
    pub precedence_rule: String,
    /// Effective instruction characters: the sum of entry text lengths in
    /// Unicode scalar values (the quantity
    /// [`limits::MAX_EFFECTIVE_INSTRUCTION_CHARS`] limits; the envelope
    /// preamble and labels are not counted).
    #[serde(default)]
    pub rendered_chars: u32,
    #[serde(default)]
    pub entry_count: u32,
    /// Whether [`build_revision_body`] accepts these entries.
    #[serde(default)]
    pub within_budget: bool,
    /// The actionable reason when `within_budget` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_error: Option<String>,
    #[serde(default)]
    pub budget: InstructionBudget,
}

impl EffectiveInstructions {
    /// The entries a revision of this item would snapshot.
    pub fn revision_entries(&self) -> Vec<InstructionRevisionEntry> {
        self.entries
            .iter()
            .map(EffectiveInstructionEntry::to_revision_entry)
            .collect()
    }
}

fn scope_title_of(item: &WorkItemView) -> String {
    let title = item.title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        UNTITLED.to_owned()
    } else {
        title
    }
}

fn sorted_scope<'a>(
    instructions: &'a [WorkItemInstructionView],
    matches: impl Fn(&WorkItemInstructionView) -> bool,
) -> Vec<&'a WorkItemInstructionView> {
    let mut scope: Vec<_> = instructions
        .iter()
        .filter(|i| !i.archived() && matches(i))
        .collect();
    scope.sort_by(|a, b| (&a.order_key, &a.id).cmp(&(&b.order_key, &b.id)));
    scope
}

/// Resolve the effective instructions of `work_item_id` over the
/// materialized (repaired) tree. `items_by_id` looks up a materialized item
/// (its `parentWorkItemId` is the EFFECTIVE parent); `instructions` may hold
/// any instructions (other Spaces, archived) and is filtered here.
///
/// Order (spec "Inherited instructions", registry audit section 8 step 7):
/// 1. Space-root instructions of the item's Space, always (a boundary never
///    removes them);
/// 2. work-item instructions from the inheritance boundary (the nearest
///    ancestor-or-self with `inheritParentInstructions == false`, inclusive)
///    down to the parent, root-most first;
/// 3. the item's own instructions.
///
/// Archived instructions and instructions whose `spaceId` differs from their
/// item's are excluded; within one scope the order is
/// `(orderKey, id)`. An unknown item yields only Space-root entries of no
/// Space (an empty list), never a panic; a parent cycle (impossible after
/// repair) is cut at the first repeat.
pub fn resolve_effective_instructions<'a>(
    work_item_id: &str,
    items_by_id: impl Fn(&str) -> Option<&'a WorkItemView>,
    instructions: &[WorkItemInstructionView],
) -> EffectiveInstructions {
    // Walk self -> boundary (inclusive).
    let mut chain: Vec<&WorkItemView> = Vec::new();
    let mut boundary = None;
    let mut seen = BTreeSet::new();
    let mut cursor = items_by_id(work_item_id);
    while let Some(item) = cursor {
        if !seen.insert(item.id.as_str()) {
            break;
        }
        chain.push(item);
        if !item.inherit_parent_instructions {
            boundary = Some(item.id.clone());
            break;
        }
        cursor = item.parent_work_item_id.as_deref().and_then(&items_by_id);
    }
    let space_id = chain.first().map(|item| item.space_id.clone());

    let mut entries = Vec::new();
    let mut push = |instruction: &WorkItemInstructionView,
                    source_kind: InstructionSourceKind,
                    scope_id: &str,
                    scope_title: String,
                    inherited: bool| {
        entries.push(EffectiveInstructionEntry {
            instruction_id: instruction.id.clone(),
            source_kind,
            scope_id: scope_id.to_owned(),
            scope_title,
            text: instruction.text.clone(),
            order: 0,
            inherited,
            provenance: instruction.provenance(),
        });
    };
    if let Some(space_id) = &space_id {
        for instruction in sorted_scope(instructions, |i| {
            i.work_item_id.is_none() && i.space_id == *space_id
        }) {
            push(
                instruction,
                InstructionSourceKind::SpaceRoot,
                space_id,
                SPACE_ROOT_SCOPE_TITLE.to_owned(),
                true,
            );
        }
    }
    for item in chain.iter().rev() {
        let inherited = item.id != work_item_id;
        for instruction in sorted_scope(instructions, |i| {
            // A row naming this item from another Space is an orphan
            // (`OrphanInstruction`), never effective.
            i.work_item_id.as_deref() == Some(item.id.as_str()) && i.space_id == item.space_id
        }) {
            push(
                instruction,
                InstructionSourceKind::WorkItem,
                &item.id,
                scope_title_of(item),
                inherited,
            );
        }
    }
    for (index, entry) in entries.iter_mut().enumerate() {
        entry.order = index as u32;
    }

    let rendered_chars = entries
        .iter()
        .map(|e| e.text.chars().count())
        .sum::<usize>();
    let budget_error = build_revision_body(
        entries
            .iter()
            .map(EffectiveInstructionEntry::to_revision_entry)
            .collect(),
    )
    .err()
    .map(|error| error.message);
    EffectiveInstructions {
        work_item_id: work_item_id.to_owned(),
        boundary_work_item_id: boundary,
        entry_count: entries.len() as u32,
        entries,
        precedence_rule: INSTRUCTION_PRECEDENCE_RULE.to_owned(),
        rendered_chars: rendered_chars as u32,
        within_budget: budget_error.is_none(),
        budget_error,
        budget: InstructionBudget::default(),
    }
}

/// [`resolve_effective_instructions`] over a slice of materialized items.
pub fn resolve_effective_instructions_in(
    work_item_id: &str,
    items: &[WorkItemView],
    instructions: &[WorkItemInstructionView],
) -> EffectiveInstructions {
    let by_id: HashMap<&str, &WorkItemView> =
        items.iter().map(|item| (item.id.as_str(), item)).collect();
    resolve_effective_instructions(work_item_id, |id| by_id.get(id).copied(), instructions)
}

// --------------------------------------------------------------------------
// Revisions
// --------------------------------------------------------------------------

/// One entry of an immutable revision (registry audit 7.8 plus `scopeTitle`,
/// see the "Milestone 2 contract notes": the rendered envelope labels each
/// scope and must be reproducible from the stored entries alone).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionRevisionEntry {
    pub instruction_id: String,
    #[serde(default)]
    pub source_kind: InstructionSourceKind,
    #[serde(default)]
    pub scope_id: String,
    #[serde(default)]
    pub scope_title: String,
    #[serde(default)]
    pub text: String,
    /// 0-based; equals the entry's index (checked by [`verify_revision_body`]).
    #[serde(default)]
    pub order: u32,
    #[serde(default)]
    pub provenance: InstructionProvenance,
}

/// The single `body` JSON field of an `instructionRevisions` row. The
/// rendered envelope is NOT stored (contract 5.2); delivery re-renders and
/// checks `envelopeSha256`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionRevisionBody {
    #[serde(default)]
    pub entries: Vec<InstructionRevisionEntry>,
    #[serde(default)]
    pub envelope_version: u32,
    /// [`envelope_sha256`] of the rendered envelope.
    #[serde(default)]
    pub envelope_sha256: String,
    /// [`canonical_revision_hash`] of `(entries, envelopeVersion)`.
    #[serde(default)]
    pub hash: String,
}

/// Reader integrity of a revision. Lenient: unknown reads as `corrupt`
/// (fail closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InstructionRevisionIntegrity {
    Ok,
    /// Declared last because `#[serde(other)]` must sit on the last variant.
    #[serde(other)]
    Corrupt,
}

fn corrupt() -> InstructionRevisionIntegrity {
    InstructionRevisionIntegrity::Corrupt
}

/// One materialized `instructionRevisions` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionRevisionView {
    pub id: String,
    #[serde(default)]
    pub space_id: String,
    #[serde(default)]
    pub work_item_id: String,
    pub body: InstructionRevisionBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<WorkOutlineActor>,
    #[serde(default)]
    pub created_at: i64,
    /// Missing reads as `corrupt` (fail closed).
    #[serde(default = "corrupt")]
    pub integrity: InstructionRevisionIntegrity,
}

/// Why a revision was selected for a Chat. Wire: `launch`, `explicit`,
/// `forkInherit` (the registry row spelling `fork-inherit` is accepted).
/// Lenient: unknown reads as `explicit`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InstructionApplicationReason {
    Launch,
    #[serde(alias = "fork-inherit")]
    ForkInherit,
    /// Declared last because `#[serde(other)]` must sit on the last variant.
    #[default]
    #[serde(other)]
    Explicit,
}

/// Outcome of an application row. Only `accepted` is written (contract 5.6;
/// delivery is reported separately by the host). Lenient: unknown (and the
/// audit-era `success`) read as `accepted`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InstructionApplicationResult {
    #[default]
    #[serde(other)]
    Accepted,
}

/// The Turn at which an application takes effect: the user message id of the
/// first Turn that uses it (absent = the next Turn).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionTurnBoundary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

/// One materialized `instructionRevisionApplications` row (immutable, id
/// `derive_id(operationId, "application")`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionRevisionApplicationView {
    pub id: String,
    #[serde(default)]
    pub space_id: String,
    #[serde(default)]
    pub chat_id: String,
    #[serde(default)]
    pub work_item_id: String,
    #[serde(default)]
    pub revision_id: String,
    /// Target Execution Session id, when one was targeted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ref: Option<String>,
    #[serde(default)]
    pub turn_boundary: InstructionTurnBoundary,
    #[serde(default)]
    pub reason: InstructionApplicationReason,
    #[serde(default)]
    pub result: InstructionApplicationResult,
    #[serde(default)]
    pub idempotency_key: String,
    /// The writing op's HLC; the Chat's selected revision is the latest
    /// accepted application by `(appliedHlc, id)`.
    #[serde(default)]
    pub applied_hlc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<WorkOutlineActor>,
    #[serde(default)]
    pub created_at: i64,
}

/// Whether the Chat's selected revision can be delivered as selected
/// (Milestone 2 registry addition). The selection is never substituted: a
/// missing or corrupt selected revision is reported here and delivery fails
/// closed. Lenient: unknown reads as `corrupt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SelectedRevisionIntegrity {
    /// The revision row is present, in the Chat's Space, and verifies.
    Ok,
    /// The revision row has not synced to this replica (yet).
    Missing,
    /// The revision row fails its integrity check or belongs to another
    /// Space. Declared last because `#[serde(other)]` must sit on the last
    /// variant.
    #[serde(other)]
    Corrupt,
}

/// Per-Chat instruction state in the snapshot: what is selected, whether the
/// item's current effective instructions drifted from it, and what the host
/// reports as actually delivered (never inferred from the selection).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatInstructionState {
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_revision_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_revision_hash: Option<String>,
    /// Integrity of the selected revision (present iff a selection exists).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_revision_integrity: Option<SelectedRevisionIntegrity>,
    /// [`canonical_revision_hash`] of the assigned item's current effective
    /// entries (absent when the Chat has no live assigned item).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_effective_hash: Option<String>,
    /// "Instructions changed since this Chat's applied revision": a selection
    /// exists and its hash differs from `currentEffectiveHash`.
    #[serde(default)]
    pub drift: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_revision_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_revision_hash: Option<String>,
}

// --------------------------------------------------------------------------
// Delivery (m0-instruction-delivery.md section 5)
// --------------------------------------------------------------------------

/// What commands, queue rows, and journals carry: the identity of an
/// immutable revision, never its content.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionRevisionPin {
    pub revision_id: String,
    /// The revision's canonical `hash`.
    pub revision_hash: String,
}

/// The exact text handed to a harness, re-rendered by the host from the
/// registry revision. `envelopeSha256` must equal the revision's stored value
/// or dispatch fails closed (`revisionIntegrity`). Never part of the prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionEnvelope {
    pub revision_id: String,
    pub revision_hash: String,
    pub envelope_version: u32,
    pub envelope_sha256: String,
    pub rendered: String,
}

impl InstructionEnvelope {
    /// Verify `body` and render its envelope.
    pub fn from_revision(
        revision_id: impl Into<String>,
        body: &InstructionRevisionBody,
    ) -> Result<Self, IntegrityError> {
        verify_revision_body(body)?;
        let rendered = render_instruction_envelope(body.envelope_version, &body.entries)?;
        Ok(Self {
            revision_id: revision_id.into(),
            revision_hash: body.hash.clone(),
            envelope_version: body.envelope_version,
            envelope_sha256: body.envelope_sha256.clone(),
            rendered,
        })
    }

    pub fn pin(&self) -> InstructionRevisionPin {
        InstructionRevisionPin {
            revision_id: self.revision_id.clone(),
            revision_hash: self.revision_hash.clone(),
        }
    }
}

/// How an envelope relates to the Execution Session it is delivered into.
/// Internally tagged by `kind`: `{"kind":"fresh"}`, `{"kind":"continue"}`,
/// `{"kind":"replace","previousRevisionId":"…"}`. Strict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum InstructionTransition {
    /// New native session: deliver before the first Turn.
    Fresh,
    /// Native resume or respawn of a session that already carries this exact
    /// revision (crash recovery, idle reaper, retries).
    Continue,
    /// Explicit application at a Turn boundary, replacing the session's
    /// previous revision.
    #[serde(rename_all = "camelCase")]
    Replace { previous_revision_id: String },
}

/// `RunRequest.instructions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionDelivery {
    pub envelope: InstructionEnvelope,
    pub transition: InstructionTransition,
}

/// Host-reported Execution Session (contract 5.6): an additive
/// `executionSession` field on the host-written session-status row, written on
/// `SessionStarted`. The UI shows delivery from this, never from selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionSessionReport {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_resume_id: Option<String>,
    pub harness: HarnessId,
    #[serde(default)]
    pub execution_root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_hash: Option<String>,
}

// --------------------------------------------------------------------------
// Integrity
// --------------------------------------------------------------------------

/// Why a stored revision cannot be delivered. Maps to
/// [`WorkOutlineErrorCode::RevisionIntegrity`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityError {
    UnsupportedEnvelopeVersion(u32),
    /// `entries[index].order != index`.
    EntryOrder {
        index: usize,
    },
    HashMismatch {
        stored: String,
        computed: String,
    },
    EnvelopeMismatch {
        stored: String,
        computed: String,
    },
}

impl std::fmt::Display for IntegrityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedEnvelopeVersion(version) => write!(
                f,
                "Instruction envelope version {version} is not supported by this version of Zeron"
            ),
            Self::EntryOrder { index } => {
                write!(f, "Instruction revision entry {index} is out of order")
            }
            Self::HashMismatch { .. } => {
                f.write_str("Instruction revision content does not match its hash")
            }
            Self::EnvelopeMismatch { .. } => {
                f.write_str("Rendered instructions do not match the revision's envelope hash")
            }
        }
    }
}

impl std::error::Error for IntegrityError {}

impl From<IntegrityError> for WorkOutlineError {
    fn from(error: IntegrityError) -> Self {
        WorkOutlineError::new(WorkOutlineErrorCode::RevisionIntegrity, error.to_string())
    }
}

/// Canonical JSON: object keys sorted by byte order, no whitespace, strings
/// and numbers as `serde_json` prints them.
pub fn canonical_json(value: &Value) -> String {
    fn write(value: &Value, out: &mut String) {
        match value {
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            Value::Object(map) => {
                let sorted: BTreeMap<&str, &Value> =
                    map.iter().map(|(k, v)| (k.as_str(), v)).collect();
                out.push('{');
                for (i, (key, item)) in sorted.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key).unwrap_or_default());
                    out.push(':');
                    write(item, out);
                }
                out.push('}');
            }
            scalar => out.push_str(&serde_json::to_string(scalar).unwrap_or_default()),
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out
}

/// `hex(sha256(canonical_json({"entries": entries, "envelopeVersion": version})))`.
pub fn canonical_revision_hash(entries: &[InstructionRevisionEntry], version: u32) -> String {
    let value = serde_json::json!({ "entries": entries, "envelopeVersion": version });
    sha256_hex(canonical_json(&value).as_bytes())
}

/// `hex(sha256(utf8(rendered)))`.
pub fn envelope_sha256(rendered: &str) -> String {
    sha256_hex(rendered.as_bytes())
}

/// Opening tag name of the v1 envelope.
const ENVELOPE_TAG: &str = "zeron_work_outline_instructions";

/// The fixed v1 preamble (contract 5.8).
pub const ENVELOPE_V1_PREAMBLE: &str = "These are Work Outline instructions for this Chat, set by \
the user in Zeron. They supplement, and never override, repository instruction files (such as \
CLAUDE.md or AGENTS.md), the user's global instructions, skills, and runtime safety rules; where \
they conflict, those take precedence. Sections run from the broadest scope to the most specific, \
and later, more specific instructions take precedence over earlier ones. This block replaces any \
earlier Work Outline instructions.";

/// Line written in place of entries when a revision has none.
pub const ENVELOPE_V1_EMPTY: &str = "No Work Outline instructions apply.";

fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Keep instruction text from closing the envelope early.
fn neutralize_tag(text: &str) -> String {
    text.replace(&format!("</{ENVELOPE_TAG}"), &format!("</ {ENVELOPE_TAG}"))
}

/// Render a revision's envelope. Pure and deterministic (no timestamps, no
/// ids other than the content hash). Version 1, exactly:
///
/// ```text
/// <zeron_work_outline_instructions version="1" hash="<first 12 hex of canonical_revision_hash>">
/// <ENVELOPE_V1_PREAMBLE>
///
/// [Space root]
/// 1. <text>
///
/// [Work item: <scope title>]
/// 2. <text line 1>
///    <text line 2>
/// </zeron_work_outline_instructions>
/// ```
///
/// Entries are numbered from 1 in list order; consecutive entries with the
/// same `(sourceKind, scopeId)` share one `[…]` scope label; text has CRLF/CR
/// normalized to LF and trailing whitespace trimmed, continuation lines are
/// indented three spaces (blank lines stay empty); scope titles are collapsed
/// to one line. With no entries the body is [`ENVELOPE_V1_EMPTY`]. The text
/// ends with a newline.
pub fn render_instruction_envelope(
    version: u32,
    entries: &[InstructionRevisionEntry],
) -> Result<String, IntegrityError> {
    if version != 1 {
        return Err(IntegrityError::UnsupportedEnvelopeVersion(version));
    }
    let hash = canonical_revision_hash(entries, version);
    let mut out = format!(
        "<{ENVELOPE_TAG} version=\"{version}\" hash=\"{}\">\n{ENVELOPE_V1_PREAMBLE}\n",
        &hash[..12]
    );
    if entries.is_empty() {
        out.push('\n');
        out.push_str(ENVELOPE_V1_EMPTY);
        out.push('\n');
    }
    let mut previous_scope: Option<(InstructionSourceKind, &str)> = None;
    for (index, entry) in entries.iter().enumerate() {
        let scope = (entry.source_kind, entry.scope_id.as_str());
        if previous_scope != Some(scope) {
            previous_scope = Some(scope);
            out.push('\n');
            match entry.source_kind {
                InstructionSourceKind::SpaceRoot => out.push_str("[Space root]\n"),
                InstructionSourceKind::WorkItem => {
                    let title = single_line(&neutralize_tag(&entry.scope_title));
                    let title = if title.is_empty() { UNTITLED } else { &title };
                    out.push_str(&format!("[Work item: {title}]\n"));
                }
            }
        }
        let text = neutralize_tag(&entry.text.replace("\r\n", "\n").replace('\r', "\n"));
        for (line_index, line) in text.trim_end().split('\n').enumerate() {
            let line = line.trim_end();
            if line_index == 0 {
                out.push_str(&format!("{}. {line}\n", index + 1));
            } else if line.is_empty() {
                out.push('\n');
            } else {
                out.push_str(&format!("   {line}\n"));
            }
        }
    }
    out.push_str(&format!("</{ENVELOPE_TAG}>\n"));
    Ok(out)
}

fn limit_exceeded(message: String) -> WorkOutlineError {
    WorkOutlineError::new(WorkOutlineErrorCode::LimitExceeded, message)
}

/// Build an immutable revision body from effective entries (renumbering
/// `order` to the list position) and enforce the budgets before anything is
/// written: each entry's text at most [`limits::MAX_INSTRUCTION_TEXT_CHARS`],
/// at most [`limits::MAX_REVISION_ENTRIES`] entries, at most
/// [`limits::MAX_EFFECTIVE_INSTRUCTION_CHARS`] effective characters, and the
/// serialized body within [`limits::MAX_REVISION_BODY_UTF16_UNITS`] so the
/// registry op fits under the edge cap. Errors are `limitExceeded` with an
/// actionable message.
pub fn build_revision_body(
    mut entries: Vec<InstructionRevisionEntry>,
) -> Result<InstructionRevisionBody, WorkOutlineError> {
    for (index, entry) in entries.iter_mut().enumerate() {
        entry.order = index as u32;
        let chars = entry.text.chars().count();
        if chars > limits::MAX_INSTRUCTION_TEXT_CHARS {
            return Err(limit_exceeded(format!(
                "An instruction in \"{}\" is {chars} characters; each instruction is limited to {}",
                single_line(&entry.scope_title),
                limits::MAX_INSTRUCTION_TEXT_CHARS
            )));
        }
    }
    if entries.len() > limits::MAX_REVISION_ENTRIES {
        return Err(limit_exceeded(format!(
            "{} instructions apply; at most {} can be sent to a Chat. Archive some, or stop \
             inheriting parent instructions on this item",
            entries.len(),
            limits::MAX_REVISION_ENTRIES
        )));
    }
    let total: usize = entries.iter().map(|e| e.text.chars().count()).sum();
    if total > limits::MAX_EFFECTIVE_INSTRUCTION_CHARS {
        return Err(limit_exceeded(format!(
            "Effective instructions total {total} characters; the limit is {}. Shorten or archive \
             instructions, or stop inheriting parent instructions on this item",
            limits::MAX_EFFECTIVE_INSTRUCTION_CHARS
        )));
    }
    let version = INSTRUCTION_ENVELOPE_VERSION;
    let rendered = render_instruction_envelope(version, &entries)
        .map_err(|e| WorkOutlineError::new(WorkOutlineErrorCode::Invalid, e.to_string()))?;
    let body = InstructionRevisionBody {
        hash: canonical_revision_hash(&entries, version),
        envelope_sha256: envelope_sha256(&rendered),
        envelope_version: version,
        entries,
    };
    let units = serde_json::to_string(&body)
        .unwrap_or_default()
        .encode_utf16()
        .count();
    if units > limits::MAX_REVISION_BODY_UTF16_UNITS {
        return Err(limit_exceeded(format!(
            "Instructions exceed the revision size limit ({units} of {} units after encoding; each \
             instruction adds about 200 units of overhead beyond its text, so this limit can be \
             reached below {} characters). Shorten, combine, or archive instructions",
            limits::MAX_REVISION_BODY_UTF16_UNITS,
            limits::MAX_EFFECTIVE_INSTRUCTION_CHARS
        )));
    }
    Ok(body)
}

/// Recompute the hash and the envelope digest of a stored body. Any mismatch
/// is a hard integrity failure: never deliver a guessed prompt.
pub fn verify_revision_body(body: &InstructionRevisionBody) -> Result<(), IntegrityError> {
    for (index, entry) in body.entries.iter().enumerate() {
        if entry.order as usize != index {
            return Err(IntegrityError::EntryOrder { index });
        }
    }
    let rendered = render_instruction_envelope(body.envelope_version, &body.entries)?;
    let computed = canonical_revision_hash(&body.entries, body.envelope_version);
    if computed != body.hash {
        return Err(IntegrityError::HashMismatch {
            stored: body.hash.clone(),
            computed,
        });
    }
    let computed = envelope_sha256(&rendered);
    if computed != body.envelope_sha256 {
        return Err(IntegrityError::EnvelopeMismatch {
            stored: body.envelope_sha256.clone(),
            computed,
        });
    }
    Ok(())
}

// --------------------------------------------------------------------------
// Comparison
// --------------------------------------------------------------------------

/// Kind of one line in a line diff. Lenient: unknown reads as `same`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextDiffKind {
    Added,
    Removed,
    /// Declared last because `#[serde(other)]` must sit on the last variant.
    #[default]
    #[serde(other)]
    Same,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDiffLine {
    #[serde(default)]
    pub kind: TextDiffKind,
    #[serde(default)]
    pub text: String,
}

/// An instruction present in both revisions with different text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionTextChange {
    pub instruction_id: String,
    #[serde(default)]
    pub before_text: String,
    #[serde(default)]
    pub after_text: String,
}

/// `compareInstructionRevisions(source, target)`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionRevisionComparison {
    #[serde(default)]
    pub source_hash: String,
    #[serde(default)]
    pub target_hash: String,
    /// Equal hashes: nothing else differs.
    #[serde(default)]
    pub identical: bool,
    /// Target entries whose instruction is not in the source, in target order.
    #[serde(default)]
    pub added: Vec<InstructionRevisionEntry>,
    /// Source entries whose instruction is not in the target, in source order.
    #[serde(default)]
    pub removed: Vec<InstructionRevisionEntry>,
    /// Instructions in both with different text, in target order.
    #[serde(default)]
    pub changed: Vec<InstructionTextChange>,
    /// Instructions in both whose relative order changed: those outside the
    /// longest common subsequence of the shared ids, in target order.
    #[serde(default)]
    pub reordered: Vec<String>,
    /// Line diff of the rendered envelopes (source to target).
    #[serde(default)]
    pub line_diff: Vec<TextDiffLine>,
}

/// LCS table cap; a middle section larger than this diffs as "all removed,
/// then all added" (still deterministic).
const MAX_LCS_CELLS: usize = 4_000_000;

/// Indices `(i, j)` of one longest common subsequence of `a` and `b`
/// (deterministic tie-break: prefer advancing `a`).
fn lcs_pairs<T: PartialEq>(a: &[T], b: &[T]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 || n.saturating_mul(m) > MAX_LCS_CELLS {
        return Vec::new();
    }
    // table[i][j] = LCS length of a[i..] and b[j..].
    let width = m + 1;
    let mut table = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * width + j] = if a[i] == b[j] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j, mut pairs) = (0, 0, Vec::new());
    while i < n && j < m {
        if a[i] == b[j] {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

/// A deterministic LCS line diff (common prefix and suffix trimmed first).
pub fn diff_lines(source: &str, target: &str) -> Vec<TextDiffLine> {
    let a: Vec<&str> = source.lines().collect();
    let b: Vec<&str> = target.lines().collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (mid_a, mid_b) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let line = |kind, text: &str| TextDiffLine {
        kind,
        text: text.to_owned(),
    };
    let mut out: Vec<TextDiffLine> = a[..prefix]
        .iter()
        .map(|t| line(TextDiffKind::Same, t))
        .collect();
    let (mut i, mut j) = (0, 0);
    for (pi, pj) in lcs_pairs(mid_a, mid_b) {
        out.extend(mid_a[i..pi].iter().map(|t| line(TextDiffKind::Removed, t)));
        out.extend(mid_b[j..pj].iter().map(|t| line(TextDiffKind::Added, t)));
        out.push(line(TextDiffKind::Same, mid_a[pi]));
        (i, j) = (pi + 1, pj + 1);
    }
    out.extend(mid_a[i..].iter().map(|t| line(TextDiffKind::Removed, t)));
    out.extend(mid_b[j..].iter().map(|t| line(TextDiffKind::Added, t)));
    out.extend(
        a[a.len() - suffix..]
            .iter()
            .map(|t| line(TextDiffKind::Same, t)),
    );
    out
}

/// Compare two revision bodies (source = the applied one, target = the
/// candidate). Fails only when a body's envelope version cannot be rendered.
pub fn compare_instruction_revisions(
    source: &InstructionRevisionBody,
    target: &InstructionRevisionBody,
) -> Result<InstructionRevisionComparison, IntegrityError> {
    let source_text = render_instruction_envelope(source.envelope_version, &source.entries)?;
    let target_text = render_instruction_envelope(target.envelope_version, &target.entries)?;
    let source_by_id: HashMap<&str, &InstructionRevisionEntry> = source
        .entries
        .iter()
        .map(|e| (e.instruction_id.as_str(), e))
        .collect();
    let target_ids: BTreeSet<&str> = target
        .entries
        .iter()
        .map(|e| e.instruction_id.as_str())
        .collect();

    let added = target
        .entries
        .iter()
        .filter(|e| !source_by_id.contains_key(e.instruction_id.as_str()))
        .cloned()
        .collect();
    let removed = source
        .entries
        .iter()
        .filter(|e| !target_ids.contains(e.instruction_id.as_str()))
        .cloned()
        .collect();
    let changed = target
        .entries
        .iter()
        .filter_map(|after| {
            let before = source_by_id.get(after.instruction_id.as_str())?;
            (before.text != after.text).then(|| InstructionTextChange {
                instruction_id: after.instruction_id.clone(),
                before_text: before.text.clone(),
                after_text: after.text.clone(),
            })
        })
        .collect();
    let common_source: Vec<&str> = source
        .entries
        .iter()
        .map(|e| e.instruction_id.as_str())
        .filter(|id| target_ids.contains(id))
        .collect();
    let common_target: Vec<&str> = target
        .entries
        .iter()
        .map(|e| e.instruction_id.as_str())
        .filter(|id| source_by_id.contains_key(id))
        .collect();
    let kept: BTreeSet<usize> = lcs_pairs(&common_source, &common_target)
        .into_iter()
        .map(|(_, j)| j)
        .collect();
    let reordered = common_target
        .iter()
        .enumerate()
        .filter(|(j, _)| !kept.contains(j))
        .map(|(_, id)| (*id).to_owned())
        .collect();

    Ok(InstructionRevisionComparison {
        identical: source.hash == target.hash && !source.hash.is_empty(),
        source_hash: source.hash.clone(),
        target_hash: target.hash.clone(),
        added,
        removed,
        changed,
        reordered,
        line_diff: diff_lines(&source_text, &target_text),
    })
}

#[cfg(test)]
mod tests;
