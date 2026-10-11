//! Work Outline user action requests and proposals (Milestone 4 registry
//! backend) against a real (offline) engine: the five new operations over
//! `WorkOutlineMutate`, snapshot state over `WatchWorkOutline`, the
//! completion gate on the wire, replay across a restart, and the agent
//! surface (`authorize_agent`, `agent_context`, `mutate_as`).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_proto::{
    HarnessId, InstructionProposalStatus, WorkItemEffectiveStatus, WorkOutlineError,
    WorkOutlineErrorCode, WorkOutlineMutation, WorkOutlineSnapshot, derive_id, roles,
};
use zeron_rpc::methods;

fn assemble(dir: &std::path::Path) -> EngineCore {
    std::fs::create_dir_all(dir).expect("create data dir");
    std::fs::write(dir.join("device-id"), "dev-a").expect("write device id");
    EngineCore::assemble(dir, Arc::new(HarnessRegistry::new()), HarnessId::Mock, None)
        .expect("engine core assembles")
}

fn outline_error(text: String) -> WorkOutlineError {
    text.find("work-outline/")
        .and_then(|at| WorkOutlineError::from_wire_message(&text[at..]))
        .unwrap_or_else(|| panic!("not a work outline error: {text}"))
}

async fn mutate(client: &zeron_rpc::RpcClient, params: Value) -> Result<Value, WorkOutlineError> {
    client
        .call(methods::WORK_OUTLINE_MUTATE, params)
        .await
        .map_err(|e| outline_error(e.to_string()))
}

async fn frame_until(
    stream: &mut tokio::sync::mpsc::Receiver<Value>,
    mut predicate: impl FnMut(&WorkOutlineSnapshot) -> bool,
) -> WorkOutlineSnapshot {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let value = tokio::time::timeout_at(deadline, stream.recv())
            .await
            .expect("frame before timeout")
            .expect("stream alive");
        let snapshot: WorkOutlineSnapshot = serde_json::from_value(value).expect("snapshot");
        if predicate(&snapshot) {
            return snapshot;
        }
    }
}

/// Space `space-1` with items A > B, `chat-1` assigned to B, `chat-2`
/// unassigned.
async fn seed(client: &zeron_rpc::RpcClient) -> (String, String) {
    client
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": "space-1", "deviceId": "dev-a", "path": "/tmp" }),
        )
        .await
        .unwrap();
    for chat in ["chat-1", "chat-2"] {
        client
            .call(
                methods::MUTATE,
                json!({ "op": "createChat", "chatId": chat, "deviceId": "dev-a", "spaceId": "space-1" }),
            )
            .await
            .unwrap();
    }
    let a = derive_id("op-a", roles::ITEM);
    let b = derive_id("op-b", roles::ITEM);
    mutate(
        client,
        json!({ "op": "createWorkItem", "spaceId": "space-1", "title": "A", "operationId": "op-a" }),
    )
    .await
    .unwrap();
    mutate(
        client,
        json!({
            "op": "createWorkItem", "spaceId": "space-1", "parentWorkItemId": a,
            "title": "B", "operationId": "op-b",
        }),
    )
    .await
    .unwrap();
    mutate(
        client,
        json!({ "op": "assignChatToWorkItem", "chatId": "chat-1", "workItemId": b, "operationId": "op-assign" }),
    )
    .await
    .unwrap();
    (a, b)
}

fn request_params(op: &str, work_item: &str, chat: &str) -> Value {
    json!({
        "op": "createUserActionRequest", "workItemId": work_item,
        "origin": { "chatId": chat, "toolCallId": "t-1" },
        "text": "Approve the schema change?", "dedupeKey": "schema",
        "operationId": op,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_and_proposals_round_trip_over_rpc() {
    let tmp = tempfile::tempdir().unwrap();
    let core = assemble(&tmp.path().join("data"));
    let client = zeron_rpc::memory_client(core.rpc_service());
    let (a, b) = seed(&client).await;
    let mut stream = client
        .subscribe(methods::WATCH_WORK_OUTLINE, json!({ "spaceId": "space-1" }))
        .await
        .unwrap();

    let reply = mutate(&client, request_params("op-r1", &b, "chat-1"))
        .await
        .unwrap();
    let r1 = derive_id("op-r1", roles::REQUEST);
    assert_eq!(reply, json!({ "userActionRequestId": r1 }));
    // A retried tool call with a new operation id is deduped.
    mutate(&client, request_params("op-r2", &b, "chat-1"))
        .await
        .unwrap();
    let snapshot = frame_until(&mut stream, |s| s.user_action_requests.len() == 2).await;
    let item = snapshot.item(&b).unwrap();
    assert_eq!(
        item.effective_status,
        WorkItemEffectiveStatus::UserActionRequired
    );
    assert_eq!(item.rollup.unresolved_user_action_count, 1);
    assert_eq!(
        snapshot.item(&a).unwrap().rollup.user_action_required_count,
        1
    );

    // Completing without confirmation fails with the count on the wire.
    let error = mutate(
        &client,
        json!({ "op": "updateWorkItem", "workItemId": b, "changes": { "status": "completed" }, "operationId": "op-c1" }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, WorkOutlineErrorCode::OpenUserActionRequests);
    assert_eq!(error.open_request_count(), Some(1));
    mutate(
        &client,
        json!({
            "op": "updateWorkItem", "workItemId": b,
            "changes": { "status": "completed", "resolveOpenRequests": true },
            "operationId": "op-c2",
        }),
    )
    .await
    .unwrap();
    let snapshot = frame_until(&mut stream, |s| {
        s.user_action_requests.iter().all(|r| !r.is_open())
    })
    .await;
    assert_eq!(
        snapshot.item(&b).unwrap().effective_status,
        WorkItemEffectiveStatus::Completed
    );

    // Proposal: propose, approve, and the instruction carries provenance.
    let reply = mutate(
        &client,
        json!({
            "op": "proposeWorkItemInstruction",
            "target": { "scope": "workItem", "workItemId": b },
            "origin": { "chatId": "chat-1", "messageId": "m-1" },
            "text": "Keep migrations reversible", "operationId": "op-p",
        }),
    )
    .await
    .unwrap();
    let proposal = derive_id("op-p", roles::PROPOSAL);
    assert_eq!(reply, json!({ "proposalId": proposal }));
    let reply = mutate(
        &client,
        json!({
            "op": "approveWorkItemInstructionProposal", "proposalId": proposal,
            "approvedText": "Keep every migration reversible", "operationId": "op-ap",
        }),
    )
    .await
    .unwrap();
    let instruction = derive_id(&proposal, roles::INSTRUCTION);
    assert_eq!(reply, json!({ "instructionId": instruction }));
    let snapshot = frame_until(&mut stream, |s| {
        s.instruction_proposal(&proposal)
            .is_some_and(|p| p.status == InstructionProposalStatus::Approved)
    })
    .await;
    let created = snapshot
        .instructions
        .iter()
        .find(|i| i.id == instruction)
        .unwrap();
    assert_eq!(
        created.source_proposal_id.as_deref(),
        Some(proposal.as_str())
    );
    assert_eq!(
        created.source.as_ref().unwrap().excerpt.as_deref(),
        Some("Keep migrations reversible")
    );
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn request_and_approval_replays_after_restart_do_not_rewrite() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let (request, propose, approve);
    {
        let core = assemble(&data);
        let client = zeron_rpc::memory_client(core.rpc_service());
        let (_a, b) = seed(&client).await;
        request = request_params("op-r", &b, "chat-1");
        propose = json!({
            "op": "proposeWorkItemInstruction", "spaceId": "space-1",
            "target": { "scope": "spaceRoot" }, "text": "Small PRs", "operationId": "op-p",
        });
        approve = json!({
            "op": "approveWorkItemInstructionProposal",
            "proposalId": derive_id("op-p", roles::PROPOSAL),
            "approvedText": "Prefer small PRs", "operationId": "op-ap",
        });
        for params in [&request, &propose, &approve] {
            mutate(&client, params.clone()).await.unwrap();
        }
        core.workspace.flush();
        core.shutdown().await;
    }
    let core = assemble(&data);
    let client = zeron_rpc::memory_client(core.rpc_service());
    let pending = core.workspace.registry_pending_len();
    assert_eq!(
        mutate(&client, request.clone()).await.unwrap(),
        json!({ "userActionRequestId": derive_id("op-r", roles::REQUEST) })
    );
    assert_eq!(
        mutate(&client, propose).await.unwrap(),
        json!({ "proposalId": derive_id("op-p", roles::PROPOSAL) })
    );
    assert_eq!(
        mutate(&client, approve).await.unwrap(),
        json!({ "instructionId": derive_id(&derive_id("op-p", roles::PROPOSAL), roles::INSTRUCTION) })
    );
    assert_eq!(
        core.workspace.registry_pending_len(),
        pending,
        "no op enqueued"
    );
    let mut other = request;
    other["text"] = json!("Different");
    assert_eq!(
        mutate(&client, other).await.unwrap_err().code,
        WorkOutlineErrorCode::OperationConflict
    );
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_surface_authorizes_reads_and_writes_as_the_chat() {
    let tmp = tempfile::tempdir().unwrap();
    let core = assemble(&tmp.path().join("data"));
    let client = zeron_rpc::memory_client(core.rpc_service());
    let (a, b) = seed(&client).await;
    let service = core.rpc_service().work_outline();
    let parse = |value: Value| -> WorkOutlineMutation { serde_json::from_value(value).unwrap() };

    // Preflight: allowed and denied.
    service
        .authorize_agent("chat-1", &parse(request_params("x", &b, "chat-1")))
        .unwrap();
    let denied = service
        .authorize_agent(
            "chat-1",
            &parse(json!({
                "op": "moveWorkItem", "workItemId": b, "operationId": "x",
                "placement": { "type": "last" },
            })),
        )
        .unwrap_err();
    assert_eq!(denied.code, WorkOutlineErrorCode::Unauthorized);
    assert_eq!(
        service
            .authorize_agent("chat-2", &parse(request_params("x", &b, "chat-2")))
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::Unauthorized,
        "an unassigned Chat cannot raise requests"
    );

    // Writes as the agent: a child under its item and a request.
    let actor = service.agent_actor("chat-1", Some("m-7"));
    let child = service
        .mutate_as(
            &parse(json!({
                "op": "createWorkItem", "spaceId": "space-1", "parentWorkItemId": b,
                "title": "Agent child", "operationId": "op-child",
            })),
            &actor,
        )
        .unwrap()
        .work_item_id
        .unwrap();
    service
        .mutate_as(&parse(request_params("op-ar", &b, "chat-1")), &actor)
        .unwrap();
    // mutate_as re-checks: an agent cannot write outside its subtree even
    // without the preflight.
    let error = service
        .mutate_as(
            &parse(json!({
                "op": "updateWorkItem", "workItemId": a,
                "changes": { "status": "active" }, "operationId": "op-up",
            })),
            &actor,
        )
        .unwrap_err();
    assert_eq!(error.code, WorkOutlineErrorCode::Unauthorized);

    // Read context: the assigned item, effective instructions, own requests.
    let context = service.agent_context("chat-1").unwrap();
    assert_eq!(context.work_item.id, b);
    assert_eq!(context.space_id, "space-1");
    assert_eq!(context.work_item.child_ids, vec![child.clone()]);
    assert_eq!(context.open_user_action_requests.len(), 1);
    assert_eq!(context.effective_instructions.work_item_id, b);
    assert_eq!(
        service.agent_context("chat-2").unwrap_err().code,
        WorkOutlineErrorCode::Unauthorized
    );
    let snapshot = service.snapshot("space-1");
    let created_by = snapshot.item(&child).unwrap().created_by.clone().unwrap();
    assert_eq!(created_by.chat_id.as_deref(), Some("chat-1"));
    assert_eq!(created_by.message_id.as_deref(), Some("m-7"));
    core.shutdown().await;
}
