//! Work Outline user action requests and instruction proposals (Milestone 4):
//! wire types for the `userActionRequests` and `workItemInstructionProposals`
//! registry kinds, their `WorkOutlineMutate` operations, and the read-only
//! context an agent acting for a Chat may see.
//!
//! Binding references: `docs/plans/work-outline/m0-contract.md` (sections 4
//! and 8, "Milestone 4 registry discoveries"), `m0-registry-persistence.md`
//! 7.6 and 7.7, and the specification's "Status and user actions" and
//! "Create instructions from Chats" sections. Wire conventions match
//! `work_outline.rs`: camelCase fields, defaulted non-identity fields,
//! lenient snapshot enums, strict request enums.
//!
//! Semantics the materializer guarantees (the doc crate implements them):
//! - a request is resolved iff its row carries a `resolution` (or a
//!   `resolved` state); resolution is monotone, there is no reopen;
//! - among OPEN requests with equal `(workItemId, dedupeKey)` the one with the
//!   smallest `(creation clock, id)` is canonical and the others are flagged
//!   `deduped`; only canonical open requests make an item
//!   `userActionRequired` and count in rollups;
//! - a proposal whose approval instruction (`derive_id(proposalId,
//!   "instruction")`) exists reads `approved`, whatever a concurrent reject
//!   wrote (approval has durable side effects); otherwise its `review`
//!   decides, and no review means `pending`.

use serde::{Deserialize, Serialize};

use crate::work_outline::{InstructionPlacement, WorkItemView, WorkOutlineActor};
use crate::work_outline_instructions::EffectiveInstructions;

/// Budgets of requests and proposals (registry audit 7.6 / 7.7).
pub mod user_action_limits {
    /// The requested action or question, in Unicode scalar values.
    pub const MAX_REQUEST_TEXT_CHARS: usize = 1_000;
    /// Optional supporting detail of a request.
    pub const MAX_REQUEST_INFO_CHARS: usize = 4_000;
    /// A request's deduplication key.
    pub const MAX_DEDUPE_KEY_CHARS: usize = 256;
    /// A resolution note or a rejection reason.
    pub const MAX_NOTE_CHARS: usize = 1_000;
    /// Origin Chat, message, and tool-call identifiers.
    pub const MAX_ORIGIN_ID_CHARS: usize = 256;
    /// Proposed instruction text (the instruction text limit).
    pub const MAX_PROPOSAL_TEXT_CHARS: usize =
        crate::work_outline::limits::MAX_INSTRUCTION_TEXT_CHARS;
    /// Resolved requests listed per Space in a snapshot (newest first by
    /// resolution time); every open request is always listed.
    pub const MAX_RECENT_RESOLVED_REQUESTS: usize = 50;
    /// Reviewed proposals listed per Space in a snapshot (newest first by
    /// review time); every pending proposal is always listed.
    pub const MAX_RECENT_REVIEWED_PROPOSALS: usize = 50;
}

/// The resolution note recorded for every open request when a work item is
/// completed with `resolveOpenRequests: true`.
pub const COMPLETION_RESOLUTION_NOTE: &str = "Resolved by completing the work item";

// --------------------------------------------------------------------------
// User action requests
// --------------------------------------------------------------------------

/// Where a request came from: the Chat (required) and, optionally, the
/// message and tool call that raised it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserActionRequestOrigin {
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Lenient: an unknown state reads as `open` (the materializer derives the
/// state from the presence of a resolution anyway).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserActionRequestState {
    Resolved,
    /// Declared last because `#[serde(other)]` must sit on the last variant.
    #[default]
    #[serde(other)]
    Open,
}

/// Who resolved a request, when, and why. Written in ONE registry field in
/// the same op as `state: "resolved"`, so it never tears.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserActionResolution {
    #[serde(default)]
    pub by: WorkOutlineActor,
    #[serde(default)]
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One materialized `userActionRequests` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserActionRequestView {
    pub id: String,
    #[serde(default)]
    pub space_id: String,
    #[serde(default)]
    pub work_item_id: String,
    /// The originating Chat (kept when the Chat is archived or deleted).
    #[serde(default)]
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// The concise requested action or question.
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<WorkOutlineActor>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub state: UserActionRequestState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<UserActionResolution>,
    /// An open request that duplicates an earlier open request with the same
    /// `(workItemId, dedupeKey)`; it never counts and resolves with it.
    #[serde(default)]
    pub deduped: bool,
    /// The origin Chat row is live and not archived. `false` means the
    /// request stays inspectable but cannot open its Chat.
    #[serde(default)]
    pub origin_chat_available: bool,
}

impl UserActionRequestView {
    pub fn is_open(&self) -> bool {
        self.state == UserActionRequestState::Open
    }

    /// Open and canonical: the requests that make an item
    /// `userActionRequired` and that count in rollups.
    pub fn is_unresolved(&self) -> bool {
        self.is_open() && !self.deduped
    }
}

/// `createUserActionRequest`. Reply: `userActionRequestId` =
/// `derive_id(operationId, "request")`. The origin Chat must exist and
/// belong to the item's Space; agents may only name their own Chat and
/// their assigned item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateUserActionRequestParams {
    pub work_item_id: String,
    pub origin: UserActionRequestOrigin,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
    pub operation_id: String,
}

/// `resolveUserActionRequest`: records an explicit resolution (also on every
/// open duplicate of the request). Resolving a resolved request is a no-op.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveUserActionRequestParams {
    pub user_action_request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub operation_id: String,
}

// --------------------------------------------------------------------------
// Instruction proposals
// --------------------------------------------------------------------------

/// The scope a proposal targets. Lenient in snapshots (`unknown`); the
/// mutators reject `unknown`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProposalTargetScope {
    SpaceRoot,
    #[default]
    WorkItem,
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

/// `{scope: "spaceRoot"}` or `{scope: "workItem", workItemId}`. Immutable on
/// the row (modification proposals are deferred, spec L487).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionProposalTarget {
    #[serde(default)]
    pub scope: ProposalTargetScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
}

impl InstructionProposalTarget {
    pub fn space_root() -> Self {
        Self {
            scope: ProposalTargetScope::SpaceRoot,
            work_item_id: None,
        }
    }

    pub fn work_item(work_item_id: impl Into<String>) -> Self {
        Self {
            scope: ProposalTargetScope::WorkItem,
            work_item_id: Some(work_item_id.into()),
        }
    }
}

/// Provenance of a proposal: the proposing Chat and, optionally, the
/// message and tool call (the row's `sourceRef`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionProposalOrigin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Lenient: an unknown review state reads as `unknown` (the proposal then
/// reads `pending` unless its approval instruction exists).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProposalReviewState {
    Approved,
    Rejected,
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

/// The review decision (ONE registry field).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionProposalReview {
    pub state: ProposalReviewState,
    #[serde(default)]
    pub by: WorkOutlineActor,
    #[serde(default)]
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The created instruction (approval only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction_id: Option<String>,
}

/// Lenient: unknown reads as `pending`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InstructionProposalStatus {
    Approved,
    Rejected,
    /// Declared last because `#[serde(other)]` must sit on the last variant.
    #[default]
    #[serde(other)]
    Pending,
}

/// One materialized `workItemInstructionProposals` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemInstructionProposalView {
    pub id: String,
    #[serde(default)]
    pub space_id: String,
    #[serde(default)]
    pub target: InstructionProposalTarget,
    /// The proposed text (immutable; the reviewer's final text lives on the
    /// created instruction).
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_by: Option<WorkOutlineActor>,
    #[serde(default)]
    pub source_ref: InstructionProposalOrigin,
    #[serde(default)]
    pub created_at: i64,
    /// The effective review: when the approval instruction exists this is an
    /// approval (synthesized from the instruction when a concurrent reject
    /// won the register).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<InstructionProposalReview>,
    #[serde(default)]
    pub status: InstructionProposalStatus,
}

/// `proposeWorkItemInstruction`. Reply: `proposalId` =
/// `derive_id(operationId, "proposal")`. `spaceId` is optional: a
/// work-item target implies its Space, and a Space-root target falls back to
/// the origin Chat's Space; it is required for a Space-root target without
/// an origin Chat. Agents may target only their item, its parent, or the
/// Space root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposeWorkItemInstructionParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
    pub target: InstructionProposalTarget,
    #[serde(default)]
    pub origin: InstructionProposalOrigin,
    pub text: String,
    pub operation_id: String,
}

/// `approveWorkItemInstructionProposal`: creates the instruction
/// `derive_id(proposalId, "instruction")` from `approvedText` in the
/// proposal's target scope, with `sourceProposalId` and source provenance
/// (`chatId`, `messageId`, `excerpt` = the proposed text, `contentHash`), in
/// the same batch as the review. Reply: `instructionId`. Approving an
/// approved proposal returns the same id without writing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApproveWorkItemInstructionProposalParams {
    pub proposal_id: String,
    pub approved_text: String,
    /// Absent = last in the target scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<InstructionPlacement>,
    pub operation_id: String,
}

/// `rejectWorkItemInstructionProposal`. Rejecting an approved proposal is
/// `invalid`; rejecting a rejected one is a no-op.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectWorkItemInstructionProposalParams {
    pub proposal_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub operation_id: String,
}

// --------------------------------------------------------------------------
// Agent read context
// --------------------------------------------------------------------------

/// What an agent acting for a Chat may read (contract section 8): the
/// Chat's assigned work item, its effective instructions, and the open
/// requests that Chat raised.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorkItemContext {
    pub chat_id: String,
    pub space_id: String,
    pub work_item: WorkItemView,
    pub effective_instructions: EffectiveInstructions,
    #[serde(default)]
    pub open_user_action_requests: Vec<UserActionRequestView>,
}

#[cfg(test)]
mod tests;
