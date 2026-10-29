//! Work Outline user action requests and instruction proposals (Milestone 4):
//! `userActionRequests` (registry audit 7.6) and
//! `workItemInstructionProposals` (7.7), plus the narrow agent policy
//! (contract section 8).
//!
//! Mechanism layer, same write rules as the earlier kinds (create-if-absent
//! that never revives a tombstone, `update` ops without immutable fields,
//! clock observation, size gate, one batch per logical mutation):
//!
//! - requests: id `derive_id(operationId, "request")`; `spaceId`,
//!   `workItemId`, `chatId`, `messageId`, `toolCallId`, `text`, `info`,
//!   `dedupeKey`, `createdBy`, `createdAt`, `inputsHash` are written once by
//!   the create upsert, with `state: "open"`. Resolution is ONE op writing
//!   `state: "resolved"` and the ONE-field `resolution {by, at, note?}`; no
//!   op ever writes `open` again, and the reader treats a present resolution
//!   as resolved, so resolution is monotone. Resolving a request resolves
//!   every open duplicate of it (same `(workItemId, dedupeKey)`) in the same
//!   batch: duplicates are one logical request.
//! - proposals: id `derive_id(operationId, "proposal")`; `spaceId`, `target`,
//!   `text`, `proposedBy`, `sourceRef`, `createdAt`, `inputsHash` written
//!   once; `review` is the only LWW field. Approval writes the instruction
//!   `derive_id(proposalId, "instruction")` (with `sourceProposalId` and the
//!   source provenance) and the review in ONE batch; the reader lets an
//!   existing approval instruction win over a concurrent reject.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use mast_proto::work_outline::{WorkItemStatus, WorkOutlineActorKind, WorkOutlineMutation};
use mast_proto::work_outline_instructions::{
    InstructionScope, InstructionSource, WorkItemInstructionView,
};
use mast_proto::work_outline_user_actions::{
    ApproveWorkItemInstructionProposalParams, COMPLETION_RESOLUTION_NOTE,
    CreateUserActionRequestParams, InstructionProposalReview, InstructionProposalStatus,
    InstructionProposalTarget, ProposalReviewState, ProposalTargetScope,
    ProposeWorkItemInstructionParams, RejectWorkItemInstructionProposalParams,
    ResolveUserActionRequestParams, UserActionOriginKind, UserActionRequestState,
    UserActionRequestView, UserActionResolution, WorkItemInstructionProposalView,
    user_action_limits,
};
use serde_json::{Value, json};

use super::instructions::{NewInstruction, check_instruction_text, checked_source};
use super::materialize::{actor_field, decode_instruction, i64_field, space_of, str_field};
use super::*;

pub const KIND_USER_ACTION_REQUESTS: &str = "userActionRequests";
pub const KIND_WORK_ITEM_INSTRUCTION_PROPOSALS: &str = "workItemInstructionProposals";

fn unauthorized(message: impl Into<String>) -> WorkOutlineError {
    WorkOutlineError::unauthorized(message)
}

fn check_origin_id(what: &str, value: Option<&str>) -> Outcome<()> {
    if let Some(value) = value
        && (value.trim().is_empty()
            || value.chars().count() > user_action_limits::MAX_ORIGIN_ID_CHARS)
    {
        return Err(invalid(format!("Invalid origin {what} id")));
    }
    Ok(())
}

fn check_note(what: &str, note: Option<&str>) -> Outcome<()> {
    if let Some(note) = note {
        check_text(what, note, user_action_limits::MAX_NOTE_CHARS, false)?;
    }
    Ok(())
}

/// The first `max` Unicode scalar values of `text`.
fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

// ── lenient decoding (shared by the materializer and the mutators) ──────────

/// One decoded request plus the clock of its creation (the dedupe order).
#[derive(Debug, Clone)]
pub(crate) struct DecodedRequest {
    pub view: UserActionRequestView,
    pub created_clock: Option<String>,
}

/// Decode one `userActionRequests` row leniently (R7): only an undecidable
/// `spaceId` drops it. `deduped` and `originChatAvailable` are filled by
/// [`finish_requests`].
pub(crate) fn decode_request(row: &RegistryRow) -> Result<DecodedRequest, WorkOutlineRepair> {
    let space_id = space_of(row, KIND_USER_ACTION_REQUESTS)?;
    let resolution = row
        .fields
        .get("resolution")
        .filter(|v| v.is_object())
        .and_then(|v| serde_json::from_value::<UserActionResolution>(v.clone()).ok());
    let resolved =
        resolution.is_some() || row.fields.get("state").and_then(Value::as_str) == Some("resolved");
    Ok(DecodedRequest {
        view: UserActionRequestView {
            id: row.id.clone(),
            space_id,
            work_item_id: str_field(row, "workItemId").unwrap_or_default(),
            chat_id: str_field(row, "chatId").unwrap_or_default(),
            origin_kind: match row.fields.get("originKind").and_then(Value::as_str) {
                Some("system") => UserActionOriginKind::System,
                _ => UserActionOriginKind::Chat,
            },
            message_id: str_field(row, "messageId").filter(|s| !s.is_empty()),
            tool_call_id: str_field(row, "toolCallId").filter(|s| !s.is_empty()),
            text: str_field(row, "text").unwrap_or_default(),
            info: str_field(row, "info").filter(|s| !s.is_empty()),
            dedupe_key: str_field(row, "dedupeKey").filter(|s| !s.is_empty()),
            created_by: actor_field(row, "createdBy"),
            created_at: i64_field(row, "createdAt").unwrap_or(0),
            state: if resolved {
                UserActionRequestState::Resolved
            } else {
                UserActionRequestState::Open
            },
            resolution,
            deduped: false,
            origin_chat_available: false,
        },
        created_clock: row.clocks.get("createdAt").cloned(),
    })
}

/// Duplicate-open collapse (registry audit 7.6): among OPEN requests with
/// equal `(workItemId, dedupeKey)`, the minimal `(creation clock, id)` is
/// canonical and the rest are `deduped`. Also stamps
/// `originChatAvailable` from `available_chat`.
pub(crate) fn finish_requests(
    requests: &mut [DecodedRequest],
    available_chat: impl Fn(&str) -> bool,
) {
    let mut canonical: BTreeMap<(String, String), (Option<String>, String)> = BTreeMap::new();
    for request in requests.iter() {
        let Some(key) = request.view.dedupe_key.as_ref() else {
            continue;
        };
        if !request.view.is_open() {
            continue;
        }
        let group = (request.view.work_item_id.clone(), key.clone());
        let candidate = (request.created_clock.clone(), request.view.id.clone());
        canonical
            .entry(group)
            .and_modify(|kept| {
                if candidate < *kept {
                    *kept = candidate.clone();
                }
            })
            .or_insert(candidate);
    }
    for request in requests.iter_mut() {
        request.view.origin_chat_available = request.view.origin_kind.is_chat()
            && !request.view.chat_id.is_empty()
            && available_chat(&request.view.chat_id);
        if let Some(key) = request.view.dedupe_key.as_ref()
            && request.view.is_open()
        {
            let group = (request.view.work_item_id.clone(), key.clone());
            request.view.deduped = canonical
                .get(&group)
                .is_some_and(|(_, id)| *id != request.view.id);
        }
    }
}

/// Decode one `workItemInstructionProposals` row leniently. `approval`
/// looks up the approval instruction (`derive_id(id, "instruction")`) among
/// live instruction rows: when it exists the proposal reads approved (its
/// review synthesized from the instruction if a concurrent reject won the
/// register).
pub(crate) fn decode_proposal(
    row: &RegistryRow,
    approval: impl Fn(&str) -> Option<WorkItemInstructionView>,
) -> Result<WorkItemInstructionProposalView, WorkOutlineRepair> {
    let space_id = space_of(row, KIND_WORK_ITEM_INSTRUCTION_PROPOSALS)?;
    let stored: Option<InstructionProposalReview> = row
        .fields
        .get("review")
        .filter(|v| v.is_object())
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    let instruction_id = derive_id(&row.id, roles::INSTRUCTION);
    let (review, status) = match approval(&instruction_id) {
        Some(instruction) => {
            let review = match stored {
                Some(review) if review.state == ProposalReviewState::Approved => review,
                _ => InstructionProposalReview {
                    state: ProposalReviewState::Approved,
                    by: instruction.created_by.clone().unwrap_or_default(),
                    at: instruction.created_at,
                    reason: None,
                    instruction_id: Some(instruction_id.clone()),
                },
            };
            (Some(review), InstructionProposalStatus::Approved)
        }
        None => {
            let status = match stored.as_ref().map(|r| r.state) {
                Some(ProposalReviewState::Approved) => InstructionProposalStatus::Approved,
                Some(ProposalReviewState::Rejected) => InstructionProposalStatus::Rejected,
                _ => InstructionProposalStatus::Pending,
            };
            (stored, status)
        }
    };
    Ok(WorkItemInstructionProposalView {
        id: row.id.clone(),
        space_id,
        target: row
            .fields
            .get("target")
            .filter(|v| v.is_object())
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(InstructionProposalTarget {
                scope: ProposalTargetScope::Unknown,
                work_item_id: None,
            }),
        text: str_field(row, "text").unwrap_or_default(),
        proposed_by: actor_field(row, "proposedBy"),
        source_ref: row
            .fields
            .get("sourceRef")
            .filter(|v| v.is_object())
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default(),
        created_at: i64_field(row, "createdAt").unwrap_or(0),
        review,
        status,
    })
}

/// The materialized request and proposal lists of one Space.
pub(crate) struct MaterializedUserActions {
    /// Unresolved (open, canonical) requests per work item.
    pub unresolved_by_item: BTreeMap<String, u32>,
    pub requests: Vec<UserActionRequestView>,
    pub proposals: Vec<WorkItemInstructionProposalView>,
}

/// Step 9 for Milestone 4: decode, dedupe, and bound the lists. Every open
/// request (deduped ones flagged) and every pending proposal is listed; of
/// the rest only the newest `MAX_RECENT_*` by resolution / review time
/// (ties by id). Both lists are sorted `(createdAt, id)`. A malformed row is
/// reported only when its work item lives here.
pub(crate) fn materialize_user_actions(
    space_id: &str,
    inputs: &WorkOutlineInputs,
    has_item: impl Fn(&str) -> bool,
    repairs: &mut BTreeSet<WorkOutlineRepair>,
) -> MaterializedUserActions {
    let available: BTreeSet<&str> = inputs
        .chats
        .iter()
        .filter(|chat| !chat.archived)
        .map(|chat| chat.id.as_str())
        .collect();
    let mut decoded: Vec<DecodedRequest> = Vec::new();
    for row in &inputs.user_action_requests {
        match decode_request(row) {
            Ok(request) if request.view.space_id == space_id => decoded.push(request),
            Ok(_) => {}
            Err(repair) => {
                if str_field(row, "workItemId").is_some_and(|w| has_item(&w)) {
                    repairs.insert(repair);
                }
            }
        }
    }
    finish_requests(&mut decoded, |chat| available.contains(chat));
    let mut unresolved_by_item: BTreeMap<String, u32> = BTreeMap::new();
    let (mut open, mut resolved): (Vec<UserActionRequestView>, Vec<UserActionRequestView>) =
        decoded
            .into_iter()
            .map(|request| request.view)
            .partition(UserActionRequestView::is_open);
    for request in &open {
        if request.is_unresolved() {
            *unresolved_by_item
                .entry(request.work_item_id.clone())
                .or_default() += 1;
        }
    }
    resolved.sort_by(|a, b| {
        let at = |r: &UserActionRequestView| r.resolution.as_ref().map_or(0, |res| res.at);
        at(b).cmp(&at(a)).then_with(|| a.id.cmp(&b.id))
    });
    resolved.truncate(user_action_limits::MAX_RECENT_RESOLVED_REQUESTS);
    open.extend(resolved);
    open.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));

    let instruction_rows: HashMap<&str, &RegistryRow> = inputs
        .instructions
        .iter()
        .map(|row| (row.id.as_str(), row))
        .collect();
    let approval = |id: &str| {
        instruction_rows
            .get(id)
            .and_then(|row| decode_instruction(row).ok())
    };
    let mut pending: Vec<WorkItemInstructionProposalView> = Vec::new();
    let mut reviewed: Vec<WorkItemInstructionProposalView> = Vec::new();
    for row in &inputs.instruction_proposals {
        match decode_proposal(row, approval) {
            Ok(proposal) if proposal.space_id != space_id => {}
            Ok(proposal) if proposal.status == InstructionProposalStatus::Pending => {
                pending.push(proposal);
            }
            Ok(proposal) => reviewed.push(proposal),
            Err(repair) => {
                let target_here = row
                    .fields
                    .get("target")
                    .and_then(|t| t.get("workItemId"))
                    .and_then(Value::as_str)
                    .is_some_and(&has_item);
                if target_here {
                    repairs.insert(repair);
                }
            }
        }
    }
    reviewed.sort_by(|a, b| {
        let at = |p: &WorkItemInstructionProposalView| p.review.as_ref().map_or(0, |r| r.at);
        at(b).cmp(&at(a)).then_with(|| a.id.cmp(&b.id))
    });
    reviewed.truncate(user_action_limits::MAX_RECENT_REVIEWED_PROPOSALS);
    pending.extend(reviewed);
    pending.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
    MaterializedUserActions {
        unresolved_by_item,
        requests: open,
        proposals: pending,
    }
}

impl RegistryDoc {
    // ── reads ───────────────────────────────────────────────────────────────

    /// Request and proposal rows of one Space (materializer inputs).
    pub(super) fn user_action_inputs(
        &self,
        space_id: &str,
    ) -> (Vec<RegistryRow>, Vec<RegistryRow>) {
        let of_space = |kind: &str| {
            self.overlay_rows(kind)
                .into_iter()
                .filter(|row| row.fields.get("spaceId").and_then(Value::as_str) == Some(space_id))
                .collect::<Vec<_>>()
        };
        (
            of_space(KIND_USER_ACTION_REQUESTS),
            of_space(KIND_WORK_ITEM_INSTRUCTION_PROPOSALS),
        )
    }

    fn chat_available(&self, chat_id: &str) -> bool {
        self.chat(chat_id)
            .ok()
            .flatten()
            .is_some_and(|chat| !chat.archived)
    }

    /// Every live request on `work_item_id`, deduped.
    fn requests_of_item(&self, work_item_id: &str) -> Vec<DecodedRequest> {
        let mut requests: Vec<DecodedRequest> = self
            .overlay_rows(KIND_USER_ACTION_REQUESTS)
            .iter()
            .filter(|row| {
                row.fields.get("workItemId").and_then(Value::as_str) == Some(work_item_id)
            })
            .filter_map(|row| decode_request(row).ok())
            .collect();
        finish_requests(&mut requests, |chat| self.chat_available(chat));
        requests
    }

    /// One live request, with its dedupe flag and origin availability.
    pub fn user_action_request(&self, id: &str) -> Option<UserActionRequestView> {
        let row = self.overlay_row(KIND_USER_ACTION_REQUESTS, id)?;
        let request = decode_request(&row).ok()?;
        self.requests_of_item(&request.view.work_item_id)
            .into_iter()
            .find(|r| r.view.id == id)
            .map(|r| r.view)
    }

    /// One live proposal with its effective status.
    pub fn instruction_proposal(&self, id: &str) -> Option<WorkItemInstructionProposalView> {
        let row = self.overlay_row(KIND_WORK_ITEM_INSTRUCTION_PROPOSALS, id)?;
        decode_proposal(&row, |instruction_id| {
            self.overlay_row(KIND_WORK_ITEM_INSTRUCTIONS, instruction_id)
                .and_then(|row| decode_instruction(&row).ok())
        })
        .ok()
    }

    /// Resolution writes for every open request in `requests` (one op each).
    fn plan_resolutions<'a>(
        requests: impl IntoIterator<Item = &'a UserActionRequestView>,
        actor: &WorkOutlineActor,
        now_ms: i64,
        note: Option<&str>,
    ) -> Vec<Planned> {
        let mut resolution = json!({ "by": actor_row_value(actor), "at": now_ms });
        if let Some(note) = note {
            resolution["note"] = json!(note);
        }
        requests
            .into_iter()
            .filter(|request| request.is_open())
            .map(|request| {
                Planned::update(
                    KIND_USER_ACTION_REQUESTS,
                    &request.id,
                    BTreeMap::from([
                        ("state".to_owned(), json!("resolved")),
                        ("resolution".to_owned(), resolution.clone()),
                    ]),
                )
            })
            .collect()
    }

    /// The completion gate of `updateWorkItem` (status `completed`): with
    /// open requests on the item, `resolve == false` is
    /// `openUserActionRequests` (count = unresolved, non-deduped requests);
    /// `resolve == true` plans an explicit resolution for every open request
    /// (duplicates included) to commit with the status change.
    pub(super) fn plan_completion_resolutions(
        &self,
        work_item_id: &str,
        resolve: bool,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<Vec<Planned>> {
        let requests: Vec<UserActionRequestView> = self
            .requests_of_item(work_item_id)
            .into_iter()
            .map(|r| r.view)
            .filter(UserActionRequestView::is_open)
            .collect();
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        if !resolve {
            let count = requests.iter().filter(|r| r.is_unresolved()).count().max(1);
            return Err(WorkOutlineError::open_user_action_requests(count));
        }
        Ok(Self::plan_resolutions(
            &requests,
            actor,
            now_ms,
            Some(COMPLETION_RESOLUTION_NOTE),
        ))
    }

    // ── request mutations ──────────────────────────────────────────────────

    /// `createUserActionRequest`: id = `derive_id(operationId, "request")`;
    /// a replay returns the same id without writing, other inputs are
    /// `operationConflict`, a deleted id is never revived. The work item
    /// must be live and not archived; the origin Chat must exist and belong
    /// to the item's Space. A duplicate of an open request (same
    /// `dedupeKey`) is written and reads as `deduped`.
    pub fn create_user_action_request(
        &mut self,
        p: &CreateUserActionRequestParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<String> {
        check_operation_id(&p.operation_id)?;
        check_id("work item", &p.work_item_id)?;
        check_text(
            "The requested action",
            &p.text,
            user_action_limits::MAX_REQUEST_TEXT_CHARS,
            true,
        )?;
        if let Some(info) = &p.info {
            check_text(
                "Supporting detail",
                info,
                user_action_limits::MAX_REQUEST_INFO_CHARS,
                false,
            )?;
        }
        if let Some(key) = &p.dedupe_key {
            check_text(
                "The deduplication key",
                key,
                user_action_limits::MAX_DEDUPE_KEY_CHARS,
                true,
            )?;
        }
        let system_origin = p.origin.kind == UserActionOriginKind::System;
        if system_origin {
            if actor.kind != WorkOutlineActorKind::System {
                return Err(unauthorized(
                    "Only the system may raise a request without an origin Chat",
                ));
            }
            if !p.origin.chat_id.is_empty()
                || p.origin.message_id.is_some()
                || p.origin.tool_call_id.is_some()
            {
                return Err(invalid("A system request has no origin Chat"));
            }
        } else {
            check_id("origin Chat", &p.origin.chat_id)?;
        }
        check_origin_id("message", p.origin.message_id.as_deref())?;
        check_origin_id("tool call", p.origin.tool_call_id.as_deref())?;
        let id = derive_id(&p.operation_id, roles::REQUEST);
        let hash = inputs_hash(p);
        match self.row_state(KIND_USER_ACTION_REQUESTS, &id) {
            RowState::Live => {
                let stored = self
                    .overlay_row(KIND_USER_ACTION_REQUESTS, &id)
                    .and_then(|row| row.fields.get("inputsHash").cloned());
                return if stored == Some(json!(hash)) {
                    Ok(id)
                } else {
                    Err(err(
                        WorkOutlineErrorCode::OperationConflict,
                        "operationId was already used for a different request",
                    ))
                };
            }
            RowState::Tombstoned => return Err(not_found("This request was deleted")),
            RowState::Absent => {}
        }
        let item = self.live_item(&p.work_item_id)?;
        self.require_space(&item.space_id)?;
        if item.archived() {
            return Err(err(
                WorkOutlineErrorCode::Archived,
                "The work item is archived",
            ));
        }
        if !system_origin {
            let chat = self
                .chat(&p.origin.chat_id)
                .ok()
                .flatten()
                .ok_or_else(|| not_found("The origin Chat was not found"))?;
            match chat.space_id.as_deref().filter(|s| !s.is_empty()) {
                None => {
                    return Err(err(
                        WorkOutlineErrorCode::Projectless,
                        "Chats outside a project cannot raise work item requests",
                    ));
                }
                Some(space) if space != item.space_id => {
                    return Err(err(
                        WorkOutlineErrorCode::CrossSpace,
                        "The origin Chat belongs to another Space",
                    ));
                }
                Some(_) => {}
            }
        }
        let mut set = BTreeMap::from([
            ("spaceId".to_owned(), json!(item.space_id)),
            ("workItemId".to_owned(), json!(p.work_item_id)),
            ("chatId".to_owned(), json!(p.origin.chat_id)),
            ("text".to_owned(), json!(p.text)),
            ("state".to_owned(), json!("open")),
            ("createdBy".to_owned(), actor_row_value(actor)),
            ("createdAt".to_owned(), json!(now_ms)),
            ("inputsHash".to_owned(), json!(hash)),
            ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
        ]);
        if system_origin {
            set.insert("originKind".to_owned(), json!("system"));
        }
        for (field, value) in [
            ("messageId", &p.origin.message_id),
            ("toolCallId", &p.origin.tool_call_id),
            ("info", &p.info),
            ("dedupeKey", &p.dedupe_key),
        ] {
            if let Some(value) = value {
                set.insert(field.to_owned(), json!(value));
            }
        }
        self.commit(
            vec![Planned::upsert(KIND_USER_ACTION_REQUESTS, &id, set)],
            &[(KIND_WORK_ITEMS, p.work_item_id.as_str())],
        )?;
        Ok(id)
    }

    /// `resolveUserActionRequest`: one resolution op per open request of the
    /// dedupe group (the request and its open duplicates), one batch.
    /// Resolving a resolved request writes nothing (monotone, no reopen).
    pub fn resolve_user_action_request(
        &mut self,
        p: &ResolveUserActionRequestParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<()> {
        check_id("request", &p.user_action_request_id)?;
        check_note("The resolution note", p.note.as_deref())?;
        let request = self
            .user_action_request(&p.user_action_request_id)
            .ok_or_else(|| not_found("Request not found"))?;
        self.require_space(&request.space_id)?;
        if !request.is_open() {
            return Ok(());
        }
        // An agent resolves only the members of the group it may resolve: its
        // own Chat's and, for a coordinator, its direct children's (the same
        // rule `authorize_agent_mutation` applies to the target). A duplicate
        // another Chat raised is that Chat's request and stays open (the
        // user's resolve keeps whole-group behavior). The target is always in
        // the group: filtering it out by the caller's Chat id once made a
        // coordinator's resolve of its child's deduped request write nothing.
        let agent_chat = match actor.kind {
            WorkOutlineActorKind::Agent => Some(actor.chat_id.clone().unwrap_or_default()),
            _ => None,
        };
        let group: Vec<UserActionRequestView> = match &request.dedupe_key {
            None => vec![request],
            Some(key) => self
                .requests_of_item(&request.work_item_id)
                .into_iter()
                .map(|r| r.view)
                .filter(|r| r.is_open() && r.dedupe_key.as_ref() == Some(key))
                .filter(|r| {
                    r.id == p.user_action_request_id
                        || agent_chat
                            .as_ref()
                            .is_none_or(|chat| self.agent_may_resolve_origin(chat, &r.chat_id))
                })
                .collect(),
        };
        let plan = Self::plan_resolutions(&group, actor, now_ms, p.note.as_deref());
        self.commit(plan, &[])
    }

    // ── proposal mutations ─────────────────────────────────────────────────

    /// `proposeWorkItemInstruction`: id = `derive_id(operationId,
    /// "proposal")`, written once (replay and conflict rules as requests).
    /// An agent's proposal without an origin Chat records the agent's Chat.
    pub fn propose_work_item_instruction(
        &mut self,
        p: &ProposeWorkItemInstructionParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<String> {
        check_operation_id(&p.operation_id)?;
        check_instruction_text(&p.text)?;
        if let Some(chat) = &p.origin.chat_id {
            check_id("origin Chat", chat)?;
        }
        check_origin_id("message", p.origin.message_id.as_deref())?;
        check_origin_id("tool call", p.origin.tool_call_id.as_deref())?;
        let id = derive_id(&p.operation_id, roles::PROPOSAL);
        let hash = inputs_hash(p);
        match self.row_state(KIND_WORK_ITEM_INSTRUCTION_PROPOSALS, &id) {
            RowState::Live => {
                let stored = self
                    .overlay_row(KIND_WORK_ITEM_INSTRUCTION_PROPOSALS, &id)
                    .and_then(|row| row.fields.get("inputsHash").cloned());
                return if stored == Some(json!(hash)) {
                    Ok(id)
                } else {
                    Err(err(
                        WorkOutlineErrorCode::OperationConflict,
                        "operationId was already used for a different proposal",
                    ))
                };
            }
            RowState::Tombstoned => return Err(not_found("This proposal was deleted")),
            RowState::Absent => {}
        }
        let mut origin = p.origin.clone();
        if origin.chat_id.is_none() && actor.kind == WorkOutlineActorKind::Agent {
            origin.chat_id = actor.chat_id.clone();
        }
        let origin_space = match &origin.chat_id {
            None => None,
            Some(chat_id) => {
                let chat = self
                    .chat(chat_id)
                    .ok()
                    .flatten()
                    .ok_or_else(|| not_found("The origin Chat was not found"))?;
                Some(chat.space_id.filter(|s| !s.is_empty()).ok_or_else(|| {
                    err(
                        WorkOutlineErrorCode::Projectless,
                        "Chats outside a project cannot propose instructions",
                    )
                })?)
            }
        };
        let (space_id, target) = match p.target.scope {
            ProposalTargetScope::WorkItem => {
                let work_item_id = p
                    .target
                    .work_item_id
                    .as_deref()
                    .ok_or_else(|| invalid("A work-item proposal needs a workItemId"))?;
                check_id("work item", work_item_id)?;
                let item = self.live_item(work_item_id)?;
                if item.archived() {
                    return Err(err(
                        WorkOutlineErrorCode::Archived,
                        "The work item is archived",
                    ));
                }
                if p.space_id.as_ref().is_some_and(|s| *s != item.space_id) {
                    return Err(err(
                        WorkOutlineErrorCode::CrossSpace,
                        "The work item belongs to another Space",
                    ));
                }
                (
                    item.space_id,
                    InstructionProposalTarget::work_item(work_item_id),
                )
            }
            ProposalTargetScope::SpaceRoot => {
                if p.target.work_item_id.is_some() {
                    return Err(invalid("A Space-root proposal cannot name a work item"));
                }
                let space = p
                    .space_id
                    .clone()
                    .or_else(|| origin_space.clone())
                    .ok_or_else(|| {
                        invalid("A Space-root proposal needs a spaceId or an origin Chat")
                    })?;
                (space, InstructionProposalTarget::space_root())
            }
            ProposalTargetScope::Unknown => {
                return Err(invalid("Unknown proposal target scope"));
            }
        };
        self.require_space(&space_id)?;
        if origin_space.as_ref().is_some_and(|s| *s != space_id) {
            return Err(err(
                WorkOutlineErrorCode::CrossSpace,
                "The origin Chat belongs to another Space",
            ));
        }
        let set = BTreeMap::from([
            ("spaceId".to_owned(), json!(space_id)),
            (
                "target".to_owned(),
                serde_json::to_value(&target).unwrap_or(Value::Null),
            ),
            ("text".to_owned(), json!(p.text)),
            ("proposedBy".to_owned(), actor_row_value(actor)),
            (
                "sourceRef".to_owned(),
                serde_json::to_value(&origin).unwrap_or(Value::Null),
            ),
            ("createdAt".to_owned(), json!(now_ms)),
            ("inputsHash".to_owned(), json!(hash)),
            ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
        ]);
        let extra: Vec<(&str, &str)> = target
            .work_item_id
            .as_deref()
            .map(|w| (KIND_WORK_ITEMS, w))
            .into_iter()
            .collect();
        self.commit(
            vec![Planned::upsert(
                KIND_WORK_ITEM_INSTRUCTION_PROPOSALS,
                &id,
                set,
            )],
            &extra,
        )?;
        Ok(id)
    }

    /// `approveWorkItemInstructionProposal`: ONE batch with the instruction
    /// `derive_id(proposalId, "instruction")` (text = `approvedText`,
    /// `sourceProposalId`, source `{chatId, messageId, excerpt = proposal
    /// text (first 1,500 characters), contentHash = sha256(excerpt)}`,
    /// placement, budgets) and the `review`. Already approved (the
    /// instruction exists): returns its id without writing. Approving a
    /// rejected proposal is allowed (approval wins, as on concurrent
    /// review).
    pub fn approve_work_item_instruction_proposal(
        &mut self,
        p: &ApproveWorkItemInstructionProposalParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<String> {
        check_operation_id(&p.operation_id)?;
        check_id("proposal", &p.proposal_id)?;
        check_instruction_text(&p.approved_text)?;
        let proposal = self
            .instruction_proposal(&p.proposal_id)
            .ok_or_else(|| not_found("Proposal not found"))?;
        self.require_space(&proposal.space_id)?;
        let instruction_id = derive_id(&p.proposal_id, roles::INSTRUCTION);
        match self.row_state(KIND_WORK_ITEM_INSTRUCTIONS, &instruction_id) {
            RowState::Live => return Ok(instruction_id),
            RowState::Tombstoned => {
                return Err(not_found("The approved instruction was deleted"));
            }
            RowState::Absent => {}
        }
        let scope = match proposal.target.scope {
            ProposalTargetScope::SpaceRoot => InstructionScope::SpaceRoot,
            ProposalTargetScope::WorkItem => InstructionScope::WorkItem {
                work_item_id: proposal
                    .target
                    .work_item_id
                    .clone()
                    .ok_or_else(|| invalid("The proposal's target has no work item"))?,
            },
            ProposalTargetScope::Unknown => {
                return Err(invalid("The proposal's target scope is not supported"));
            }
        };
        let work_item_id = self.instruction_scope_item(&proposal.space_id, &scope)?;
        let source = checked_source(Some(&InstructionSource {
            chat_id: proposal.source_ref.chat_id.clone(),
            message_id: proposal.source_ref.message_id.clone(),
            excerpt: Some(truncate_chars(
                &proposal.text,
                limits::MAX_INSTRUCTION_EXCERPT_CHARS,
            )),
            content_hash: None,
        }))?;
        let hash = inputs_hash(p);
        let mut plan = self.plan_new_instruction(
            NewInstruction {
                id: &instruction_id,
                space_id: &proposal.space_id,
                work_item_id: work_item_id.as_deref(),
                text: &p.approved_text,
                placement: p.placement.clone().unwrap_or_default(),
                source,
                source_proposal_id: Some(&p.proposal_id),
                inputs_hash: &hash,
                action: "Approving this proposal",
            },
            actor,
            now_ms,
        )?;
        plan.push(Planned::update(
            KIND_WORK_ITEM_INSTRUCTION_PROPOSALS,
            &p.proposal_id,
            BTreeMap::from([(
                "review".to_owned(),
                json!({
                    "state": "approved",
                    "by": actor_row_value(actor),
                    "at": now_ms,
                    "instructionId": instruction_id,
                }),
            )]),
        ));
        let extra: Vec<(&str, &str)> = work_item_id
            .as_deref()
            .map(|w| (KIND_WORK_ITEMS, w))
            .into_iter()
            .collect();
        self.commit(plan, &extra)?;
        Ok(instruction_id)
    }

    /// `rejectWorkItemInstructionProposal`: writes the `review`. An approved
    /// proposal is `invalid` (the instruction exists; archive it instead); a
    /// rejected one is a no-op.
    pub fn reject_work_item_instruction_proposal(
        &mut self,
        p: &RejectWorkItemInstructionProposalParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<()> {
        check_id("proposal", &p.proposal_id)?;
        check_note("The rejection reason", p.reason.as_deref())?;
        let proposal = self
            .instruction_proposal(&p.proposal_id)
            .ok_or_else(|| not_found("Proposal not found"))?;
        self.require_space(&proposal.space_id)?;
        let instruction_id = derive_id(&p.proposal_id, roles::INSTRUCTION);
        if proposal.status == InstructionProposalStatus::Approved
            || self.row_state(KIND_WORK_ITEM_INSTRUCTIONS, &instruction_id) != RowState::Absent
        {
            return Err(invalid(
                "This proposal was already approved; archive its instruction instead",
            ));
        }
        if proposal.status == InstructionProposalStatus::Rejected {
            return Ok(());
        }
        let mut review = json!({
            "state": "rejected",
            "by": actor_row_value(actor),
            "at": now_ms,
        });
        if let Some(reason) = &p.reason {
            review["reason"] = json!(reason);
        }
        self.commit(
            vec![Planned::update(
                KIND_WORK_ITEM_INSTRUCTION_PROPOSALS,
                &p.proposal_id,
                BTreeMap::from([("review".to_owned(), review)]),
            )],
            &[],
        )
    }

    // ── agent policy (contract section 8) ──────────────────────────────────

    /// The live work item Chat `chat_id` is associated with in `space_id`.
    pub fn chat_assigned_work_item(&self, chat_id: &str, space_id: &str) -> Option<String> {
        let row = self.overlay_row(KIND_WORK_ITEM_CHAT_ASSIGNMENTS, chat_id)?;
        if row.fields.get("spaceId").and_then(Value::as_str) != Some(space_id) {
            return None;
        }
        let item_id = row.fields.get("workItemId").and_then(Value::as_str)?;
        let item = self.live_item(item_id).ok()?;
        (item.space_id == space_id).then(|| item_id.to_owned())
    }

    /// The effective (repaired) parent of `work_item_id` in `space_id`
    /// (`None` for a root item or an unknown one).
    pub fn effective_work_item_parent(&self, space_id: &str, work_item_id: &str) -> Option<String> {
        self.outline_tree(space_id)
            .parent
            .get(work_item_id)
            .cloned()
            .flatten()
    }

    /// Whether an agent acting for Chat `agent_chat` may resolve a request
    /// raised by `origin`: its own Chat, or a Chat it created (a
    /// coordinator closes what its direct children raised, e.g. "Ready to
    /// merge"); never requests from Chats a person started or another agent
    /// created.
    fn agent_may_resolve_origin(&self, agent_chat: &str, origin: &str) -> bool {
        origin == agent_chat
            || self
                .chat(origin)
                .ok()
                .flatten()
                .is_some_and(|c| c.parent_chat_id.as_deref() == Some(agent_chat))
    }

    /// The narrow agent policy: `Ok` when an agent acting for Chat
    /// `chat_id` may perform `mutation`, else `unauthorized` (with the
    /// reason). Allowed, all within the Chat's Space:
    /// - `createWorkItem` under the Chat's assigned item or a descendant;
    /// - `assignChatToWorkItem` of this Chat only: any item of its Space while
    ///   unassociated, else only within its assigned item's subtree; never
    ///   for a Chat with a captured execution binding;
    /// - `updateWorkItem` that only sets status `active` / `completed` on the
    ///   assigned item or a descendant (never `resolveOpenRequests`);
    /// - `createUserActionRequest` on the assigned item with origin Chat =
    ///   this Chat, `resolveUserActionRequest` of requests this Chat or a Chat
    ///   it created (`parentChatId` = this Chat) raised;
    /// - `proposeWorkItemInstruction` targeting the assigned item, its
    ///   parent, or the Space root, with no other origin Chat.
    ///
    /// Everything else (move, archive, restore, remove, references, repair,
    /// instruction writes, revisions, applications, execution
    /// configuration, approve / reject) is denied. Mechanism checks (live
    /// rows, archived targets, open requests) still run in the mutator.
    pub fn authorize_agent_mutation(
        &self,
        chat_id: &str,
        mutation: &WorkOutlineMutation,
    ) -> Outcome<()> {
        let chat = self
            .chat(chat_id)
            .ok()
            .flatten()
            .ok_or_else(|| unauthorized("The agent's Chat no longer exists"))?;
        let space = chat.space_id.filter(|s| !s.is_empty()).ok_or_else(|| {
            unauthorized("Agents in Chats outside a project cannot change the Work Outline")
        })?;
        let assigned = self.chat_assigned_work_item(chat_id, &space);
        let require_assigned = || {
            assigned
                .clone()
                .ok_or_else(|| unauthorized("The agent's Chat is not associated with a work item"))
        };
        // The assigned item and its effective descendants.
        let in_scope = |work_item_id: &str| -> Outcome<()> {
            let root = require_assigned()?;
            if work_item_id == root
                || self
                    .outline_tree(&space)
                    .descendants(&root)
                    .iter()
                    .any(|id| id == work_item_id)
            {
                Ok(())
            } else {
                Err(unauthorized(
                    "Agents may only change their own work item and its descendants",
                ))
            }
        };
        // Project scope (contract amendment "project scope"): the nearest
        // project at or above the assigned item and everything inside it, so
        // a Chat on a project's Planning item can create into its Tickets
        // sibling. Without a project it is exactly `in_scope`.
        let in_project_scope = |work_item_id: &str| -> Outcome<()> {
            let root = require_assigned()?;
            let tree = self.outline_tree(&space);
            let mut project = None;
            let mut cursor = Some(root.clone());
            let mut steps = 0usize;
            while let Some(id) = cursor {
                if tree.items.get(&id).is_some_and(|item| item.is_project) {
                    project = Some(id);
                    break;
                }
                steps += 1;
                if steps > tree.items.len() {
                    break;
                }
                cursor = tree.parent.get(&id).cloned().flatten();
            }
            let Some(project) = project else {
                return in_scope(work_item_id);
            };
            if in_scope(work_item_id).is_ok()
                || work_item_id == project
                || tree.descendants(&project).iter().any(|id| id == work_item_id)
            {
                Ok(())
            } else {
                Err(unauthorized(
                    "Agents may only create items inside their own project",
                ))
            }
        };
        match mutation {
            WorkOutlineMutation::CreateWorkItem(p) => {
                if p.space_id != space {
                    return Err(unauthorized(
                        "Agents may only create items in their own Space",
                    ));
                }
                let parent = p.parent_work_item_id.as_deref().ok_or_else(|| {
                    unauthorized("Agents may only create children of their own work item")
                })?;
                in_project_scope(parent)
            }
            WorkOutlineMutation::AssignChatToWorkItem(p) => {
                if p.chat_id != chat_id {
                    return Err(unauthorized("Agents may only associate their own Chat"));
                }
                // A Chat launched with a captured execution binding keeps the
                // item it was started for (re-associating with it is a no-op).
                if assigned.as_deref() != Some(p.work_item_id.as_str())
                    && self.row_state(KIND_CHAT_EXECUTION_BINDINGS, chat_id) == RowState::Live
                {
                    return Err(unauthorized(
                        "Agents cannot re-associate a Chat started for a work item",
                    ));
                }
                match self.live_item(&p.work_item_id) {
                    Ok(item) if item.space_id != space => Err(unauthorized(
                        "Agents may only associate their Chat with an item in its Space",
                    )),
                    // Unassociated: any item of its Space. Associated: only
                    // within its current item's subtree (never wider).
                    Ok(_) if assigned.is_none() => Ok(()),
                    Ok(_) => in_scope(&p.work_item_id).map_err(|_| {
                        unauthorized(
                            "Agents may only move their Chat to an item within its current \
                             work item",
                        )
                    }),
                    // Let the mutator report the missing item.
                    Err(_) => Ok(()),
                }
            }
            WorkOutlineMutation::UpdateWorkItem(p) => {
                let changes = &p.changes;
                let status_only = changes.title.is_none()
                    && changes.description.is_unchanged()
                    && changes.inherit_parent_instructions.is_none()
                    && changes.external_sync_state.is_none();
                if !status_only {
                    return Err(unauthorized("Agents may only change a work item's status"));
                }
                match changes.status {
                    Some(WorkItemStatus::Active | WorkItemStatus::Completed) => {}
                    _ => {
                        return Err(unauthorized(
                            "Agents may only mark a work item active or completed",
                        ));
                    }
                }
                if changes.resolve_open_requests {
                    return Err(unauthorized(
                        "Agents cannot resolve open user action requests by completing an item",
                    ));
                }
                in_scope(&p.work_item_id)
            }
            WorkOutlineMutation::CreateUserActionRequest(p) => {
                if p.origin.chat_id != chat_id {
                    return Err(unauthorized(
                        "Agents may only raise requests from their own Chat",
                    ));
                }
                if require_assigned()? != p.work_item_id {
                    return Err(unauthorized(
                        "Agents may only raise requests on their own work item",
                    ));
                }
                Ok(())
            }
            WorkOutlineMutation::ResolveUserActionRequest(p) => {
                let origin = self
                    .overlay_row(KIND_USER_ACTION_REQUESTS, &p.user_action_request_id)
                    .and_then(|row| str_field(&row, "chatId"));
                match origin {
                    Some(origin) if self.agent_may_resolve_origin(chat_id, &origin) => Ok(()),
                    Some(_) => Err(unauthorized(
                        "Agents may only resolve requests their own Chat or a Chat it created \
                         raised",
                    )),
                    None => Err(not_found("Request not found")),
                }
            }
            WorkOutlineMutation::ProposeWorkItemInstruction(p) => {
                if p.origin.chat_id.as_deref().is_some_and(|c| c != chat_id) {
                    return Err(unauthorized("Agents may only propose from their own Chat"));
                }
                match p.target.scope {
                    ProposalTargetScope::SpaceRoot => {
                        if p.space_id.as_ref().is_some_and(|s| *s != space) {
                            return Err(unauthorized(
                                "Agents may only propose for their own Space root",
                            ));
                        }
                        Ok(())
                    }
                    ProposalTargetScope::WorkItem => {
                        let own = require_assigned()?;
                        let target = p.target.work_item_id.as_deref().unwrap_or_default();
                        let parent = self
                            .outline_tree(&space)
                            .parent
                            .get(&own)
                            .cloned()
                            .flatten();
                        if target == own || parent.as_deref() == Some(target) {
                            Ok(())
                        } else {
                            Err(unauthorized(
                                "Agents may only propose for their work item, its parent, or the Space root",
                            ))
                        }
                    }
                    ProposalTargetScope::Unknown => Err(invalid("Unknown proposal target scope")),
                }
            }
            other => Err(unauthorized(format!(
                "Agents cannot {} (ask the user)",
                other.op_name()
            ))),
        }
    }
}

#[cfg(test)]
mod tests;
