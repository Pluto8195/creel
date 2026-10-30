//! Wire-shape tests for Milestone 4: request and proposal views, the five new
//! `WorkOutlineMutate` operations, replies, error codes, and leniency.

use serde_json::json;

use super::*;
use crate::work_outline::{
    UpdateWorkItemParams, WorkItemChanges, WorkItemStatus, WorkOutlineActorKind, WorkOutlineError,
    WorkOutlineErrorCode, WorkOutlineMutateReply, WorkOutlineMutation, WorkOutlineSnapshot,
    derive_id, roles,
};

fn round_trip<T>(value: &T, expected: serde_json::Value)
where
    T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_eq!(serde_json::to_value(value).unwrap(), expected);
    assert_eq!(&serde_json::from_value::<T>(expected).unwrap(), value);
}

fn agent() -> WorkOutlineActor {
    WorkOutlineActor {
        kind: WorkOutlineActorKind::Agent,
        device_id: "dev-1".into(),
        chat_id: Some("chat-1".into()),
        message_id: None,
    }
}

fn agent_json() -> serde_json::Value {
    json!({"kind":"agent","deviceId":"dev-1","chatId":"chat-1"})
}

#[test]
fn proposal_role_is_pinned() {
    assert_eq!(roles::PROPOSAL, "proposal");
    assert_eq!(roles::REQUEST, "request");
    assert_ne!(
        derive_id("op-1", roles::PROPOSAL),
        derive_id("op-1", roles::REQUEST)
    );
}

#[test]
fn user_action_request_view_field_names_are_pinned() {
    let view = UserActionRequestView {
        id: "r-1".into(),
        space_id: "sp".into(),
        work_item_id: "w-1".into(),
        chat_id: "chat-1".into(),
        origin_kind: UserActionOriginKind::Chat,
        message_id: Some("m-1".into()),
        tool_call_id: Some("t-1".into()),
        text: "Approve the migration?".into(),
        info: Some("It drops a column".into()),
        dedupe_key: Some("migration".into()),
        created_by: Some(agent()),
        created_at: 10,
        state: UserActionRequestState::Resolved,
        resolution: Some(UserActionResolution {
            by: agent(),
            at: 20,
            note: Some("done".into()),
        }),
        deduped: true,
        origin_chat_available: true,
    };
    round_trip(
        &view,
        json!({
            "id": "r-1",
            "spaceId": "sp",
            "workItemId": "w-1",
            "chatId": "chat-1",
            "messageId": "m-1",
            "toolCallId": "t-1",
            "text": "Approve the migration?",
            "info": "It drops a column",
            "dedupeKey": "migration",
            "createdBy": agent_json(),
            "createdAt": 10,
            "state": "resolved",
            "resolution": {"by": agent_json(), "at": 20, "note": "done"},
            "deduped": true,
            "originChatAvailable": true
        }),
    );
    // Sparse rows decode; unknown state reads open.
    let sparse: UserActionRequestView =
        serde_json::from_value(json!({"id":"r","state":"snoozed"})).unwrap();
    assert!(sparse.is_open() && sparse.is_unresolved());
    assert!(!sparse.origin_chat_available);
}

#[test]
fn proposal_view_field_names_are_pinned() {
    let view = WorkItemInstructionProposalView {
        id: "p-1".into(),
        space_id: "sp".into(),
        target: InstructionProposalTarget::work_item("w-1"),
        text: "Always run the linter".into(),
        proposed_by: Some(agent()),
        source_ref: InstructionProposalOrigin {
            chat_id: Some("chat-1".into()),
            message_id: Some("m-1".into()),
            tool_call_id: Some("t-1".into()),
        },
        created_at: 10,
        action: ProposalAction::Edit,
        instruction_id: Some("i-1".into()),
        base_text: Some("Always lint".into()),
        base_updated_at: 5,
        rationale: Some("clearer".into()),
        review: Some(InstructionProposalReview {
            state: ProposalReviewState::Approved,
            by: agent(),
            at: 20,
            reason: None,
            instruction_id: Some("i-1".into()),
        }),
        status: InstructionProposalStatus::Approved,
    };
    round_trip(
        &view,
        json!({
            "id": "p-1",
            "spaceId": "sp",
            "target": {"scope": "workItem", "workItemId": "w-1"},
            "text": "Always run the linter",
            "proposedBy": agent_json(),
            "sourceRef": {"chatId": "chat-1", "messageId": "m-1", "toolCallId": "t-1"},
            "createdAt": 10,
            "action": "edit",
            "instructionId": "i-1",
            "baseText": "Always lint",
            "baseUpdatedAt": 5,
            "rationale": "clearer",
            "review": {"state": "approved", "by": agent_json(), "at": 20, "instructionId": "i-1"},
            "status": "approved"
        }),
    );
    round_trip(
        &InstructionProposalTarget::space_root(),
        json!({"scope": "spaceRoot"}),
    );
    // Lenient snapshot enums.
    let sparse: WorkItemInstructionProposalView = serde_json::from_value(json!({
        "id": "p",
        "target": {"scope": "instruction"},
        "review": {"state": "deferred"},
        "status": "withdrawn"
    }))
    .unwrap();
    assert_eq!(sparse.target.scope, ProposalTargetScope::Unknown);
    assert_eq!(sparse.review.unwrap().state, ProposalReviewState::Unknown);
    assert_eq!(sparse.status, InstructionProposalStatus::Pending);
}

#[test]
fn mutation_field_names_are_pinned() {
    round_trip(
        &WorkOutlineMutation::CreateUserActionRequest(CreateUserActionRequestParams {
            work_item_id: "w-1".into(),
            origin: UserActionRequestOrigin {
                kind: UserActionOriginKind::Chat,
                chat_id: "chat-1".into(),
                message_id: Some("m-1".into()),
                tool_call_id: Some("t-1".into()),
            },
            text: "Pick a name".into(),
            info: Some("detail".into()),
            dedupe_key: Some("name".into()),
            operation_id: "op-1".into(),
        }),
        json!({
            "op": "createUserActionRequest",
            "workItemId": "w-1",
            "origin": {"chatId": "chat-1", "messageId": "m-1", "toolCallId": "t-1"},
            "text": "Pick a name",
            "info": "detail",
            "dedupeKey": "name",
            "operationId": "op-1"
        }),
    );
    round_trip(
        &WorkOutlineMutation::ResolveUserActionRequest(ResolveUserActionRequestParams {
            user_action_request_id: "r-1".into(),
            note: Some("answered".into()),
            operation_id: "op-2".into(),
        }),
        json!({
            "op": "resolveUserActionRequest",
            "userActionRequestId": "r-1",
            "note": "answered",
            "operationId": "op-2"
        }),
    );
    round_trip(
        &WorkOutlineMutation::ProposeWorkItemInstruction(ProposeWorkItemInstructionParams {
            space_id: Some("sp".into()),
            target: InstructionProposalTarget::space_root(),
            origin: InstructionProposalOrigin {
                chat_id: Some("chat-1".into()),
                message_id: None,
                tool_call_id: Some("t-1".into()),
            },
            text: "Prefer small PRs".into(),
            action: ProposalAction::Add,
            instruction_id: None,
            rationale: None,
            operation_id: "op-3".into(),
        }),
        json!({
            "op": "proposeWorkItemInstruction",
            "spaceId": "sp",
            "target": {"scope": "spaceRoot"},
            "origin": {"chatId": "chat-1", "toolCallId": "t-1"},
            "text": "Prefer small PRs",
            "operationId": "op-3"
        }),
    );
    round_trip(
        &WorkOutlineMutation::ApproveWorkItemInstructionProposal(
            ApproveWorkItemInstructionProposalParams {
                proposal_id: "p-1".into(),
                approved_text: "Prefer small pull requests".into(),
                placement: Some(InstructionPlacement::First),
                allow_stale: false,
                operation_id: "op-4".into(),
            },
        ),
        json!({
            "op": "approveWorkItemInstructionProposal",
            "proposalId": "p-1",
            "approvedText": "Prefer small pull requests",
            "placement": {"type": "first"},
            "operationId": "op-4"
        }),
    );
    round_trip(
        &WorkOutlineMutation::RejectWorkItemInstructionProposal(
            RejectWorkItemInstructionProposalParams {
                proposal_id: "p-1".into(),
                reason: Some("too broad".into()),
                operation_id: "op-5".into(),
            },
        ),
        json!({
            "op": "rejectWorkItemInstructionProposal",
            "proposalId": "p-1",
            "reason": "too broad",
            "operationId": "op-5"
        }),
    );
    // Origin of a proposal is optional on the wire.
    let bare: WorkOutlineMutation = serde_json::from_value(json!({
        "op": "proposeWorkItemInstruction",
        "target": {"scope": "workItem", "workItemId": "w"},
        "text": "x",
        "operationId": "op"
    }))
    .unwrap();
    assert_eq!(bare.op_name(), "proposeWorkItemInstruction");
    assert_eq!(bare.operation_id(), "op");
}

#[test]
fn resolve_open_requests_rides_in_changes_and_is_omitted_when_false() {
    let plain = WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
        work_item_id: "w".into(),
        changes: WorkItemChanges {
            status: Some(WorkItemStatus::Completed),
            ..Default::default()
        },
        operation_id: "op".into(),
    });
    // Absent when false, so Milestone 1-3 inputs hashes are unchanged.
    assert_eq!(
        serde_json::to_value(&plain).unwrap(),
        json!({"op":"updateWorkItem","workItemId":"w","changes":{"status":"completed"},"operationId":"op"})
    );
    round_trip(
        &WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: "w".into(),
            changes: WorkItemChanges {
                status: Some(WorkItemStatus::Completed),
                resolve_open_requests: true,
                external_sync_state: None,
                ..Default::default()
            },
            operation_id: "op".into(),
        }),
        json!({
            "op": "updateWorkItem",
            "workItemId": "w",
            "changes": {"status": "completed", "resolveOpenRequests": true},
            "operationId": "op"
        }),
    );
}

#[test]
fn replies_carry_request_and_proposal_ids() {
    round_trip(
        &WorkOutlineMutateReply {
            user_action_request_id: Some("r".into()),
            ..Default::default()
        },
        json!({"userActionRequestId": "r"}),
    );
    round_trip(
        &WorkOutlineMutateReply {
            proposal_id: Some("p".into()),
            ..Default::default()
        },
        json!({"proposalId": "p"}),
    );
    let reply: WorkOutlineMutateReply = serde_json::from_value(json!({"proposalId":"p"})).unwrap();
    assert_eq!(reply.clone().into_proposal_id().unwrap(), "p");
    assert_eq!(
        reply.into_user_action_request_id().unwrap_err().code,
        WorkOutlineErrorCode::Invalid
    );
}

#[test]
fn new_error_codes_are_pinned_and_carry_the_count() {
    for (code, text) in [
        (
            WorkOutlineErrorCode::OpenUserActionRequests,
            "openUserActionRequests",
        ),
        (WorkOutlineErrorCode::Unauthorized, "unauthorized"),
    ] {
        assert_eq!(code.as_str(), text);
        assert_eq!(WorkOutlineErrorCode::parse(text), code);
        assert_eq!(serde_json::to_value(code).unwrap(), json!(text));
    }
    let error = WorkOutlineError::open_user_action_requests(3);
    let wire = error.to_wire_message();
    assert!(wire.starts_with("work-outline/openUserActionRequests: 3 open user action requests"));
    let back = WorkOutlineError::from_wire_message(&wire).unwrap();
    assert_eq!(back.open_request_count(), Some(3));
    assert_eq!(
        WorkOutlineError::open_user_action_requests(1).message,
        "1 open user action request: resolve it or confirm that completing the work item resolves it"
    );
    assert_eq!(
        WorkOutlineError::unauthorized("no").open_request_count(),
        None
    );
}

#[test]
fn snapshot_lists_are_additive_and_default_empty() {
    let snapshot: WorkOutlineSnapshot =
        serde_json::from_value(json!({"spaceId":"s","available":true})).unwrap();
    assert!(snapshot.user_action_requests.is_empty());
    assert!(snapshot.instruction_proposals.is_empty());
    let value = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(value["userActionRequests"], json!([]));
    assert_eq!(value["instructionProposals"], json!([]));

    let mut a = snapshot.clone();
    a.user_action_requests
        .push(serde_json::from_value(json!({"id":"r","workItemId":"w","state":"open"})).unwrap());
    assert!(!a.same_content_as(&snapshot));
    assert_eq!(a.unresolved_user_action_requests("w").count(), 1);
    assert!(a.user_action_request("r").is_some());
}

#[test]
fn agent_context_field_names_are_pinned() {
    let value = json!({
        "chatId": "c",
        "spaceId": "s",
        "workItem": {"id": "w", "spaceId": "s"},
        "effectiveInstructions": {"workItemId": "w"},
        "openUserActionRequests": []
    });
    let context: AgentWorkItemContext = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(context.work_item.id, "w");
    let back = serde_json::to_value(&context).unwrap();
    for key in [
        "chatId",
        "spaceId",
        "workItem",
        "effectiveInstructions",
        "openUserActionRequests",
    ] {
        assert!(back.get(key).is_some(), "{key}");
    }
}

#[test]
fn modification_proposals_round_trip_and_old_rows_read_as_add() {
    round_trip(
        &WorkOutlineMutation::ProposeWorkItemInstruction(ProposeWorkItemInstructionParams {
            space_id: None,
            target: InstructionProposalTarget::default(),
            origin: InstructionProposalOrigin::default(),
            text: String::new(),
            action: ProposalAction::Remove,
            instruction_id: Some("i-1".into()),
            rationale: Some("obsolete".into()),
            operation_id: "op-5".into(),
        }),
        json!({
            "op": "proposeWorkItemInstruction",
            "target": {"scope": "workItem"},
            "origin": {},
            "text": "",
            "action": "remove",
            "instructionId": "i-1",
            "rationale": "obsolete",
            "operationId": "op-5"
        }),
    );
    let old: WorkItemInstructionProposalView =
        serde_json::from_value(json!({"id": "p", "text": "t"})).unwrap();
    assert_eq!(old.action, ProposalAction::Add);
    let unknown: ProposalAction = serde_json::from_value(json!("rewrite")).unwrap();
    assert_eq!(unknown, ProposalAction::Add);
}
