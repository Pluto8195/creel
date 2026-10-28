//! Work Outline instructions (Milestone 2 registry backend) against a real
//! (offline) engine over the in-memory RPC transport: the instruction
//! mutations, `WorkOutlineQuery`, snapshot instruction state, durable replay
//! across a restart, and the `InstructionRegistry` seam.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use mast_engine::instruction_registry::InstructionRegistry;
use mast_engine::{EngineCore, HarnessRegistry};
use mast_proto::{
    ExecutionSessionReport, HarnessId, InstructionRevisionIntegrity, WorkOutlineError,
    WorkOutlineErrorCode, WorkOutlineQueryResponse, WorkOutlineSnapshot, derive_id, roles,
};
use mast_rpc::methods;

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

async fn mutate(client: &mast_rpc::RpcClient, params: Value) -> Result<Value, WorkOutlineError> {
    client
        .call(methods::WORK_OUTLINE_MUTATE, params)
        .await
        .map_err(|e| outline_error(e.to_string()))
}

async fn query(
    client: &mast_rpc::RpcClient,
    params: Value,
) -> Result<WorkOutlineQueryResponse, WorkOutlineError> {
    let value: Value = client
        .call(methods::WORK_OUTLINE_QUERY, params)
        .await
        .map_err(|e| outline_error(e.to_string()))?;
    Ok(serde_json::from_value(value).expect("query response"))
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

/// Space `space-1` with items A > B and `chat-1` assigned to B.
async fn seed(client: &mast_rpc::RpcClient) -> (String, String) {
    client
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": "space-1", "deviceId": "dev-a", "path": "/tmp" }),
        )
        .await
        .unwrap();
    client
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": "chat-1", "deviceId": "dev-a", "spaceId": "space-1" }),
        )
        .await
        .unwrap();
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

fn effective_texts(response: WorkOutlineQueryResponse) -> Vec<String> {
    match response {
        WorkOutlineQueryResponse::ResolveEffectiveInstructions(effective) => {
            effective.entries.into_iter().map(|e| e.text).collect()
        }
        other => panic!("unexpected response {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn instruction_mutations_queries_and_snapshot_state_over_rpc() {
    let tmp = tempfile::tempdir().unwrap();
    let core = assemble(&tmp.path().join("data"));
    let client = mast_rpc::memory_client(core.rpc_service());
    let (a, b) = seed(&client).await;
    let mut stream = client
        .subscribe(methods::WATCH_WORK_OUTLINE, json!({ "spaceId": "space-1" }))
        .await
        .unwrap();

    // Instructions at the Space root, on A, and on B.
    let root = mutate(
        &client,
        json!({
            "op": "createWorkItemInstruction", "spaceId": "space-1",
            "scope": { "type": "spaceRoot" }, "text": "Be concise", "operationId": "op-ir",
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        root,
        json!({ "instructionId": derive_id("op-ir", roles::INSTRUCTION) })
    );
    let on_a = derive_id("op-ia", roles::INSTRUCTION);
    let create_a = json!({
        "op": "createWorkItemInstruction", "spaceId": "space-1",
        "scope": { "type": "workItem", "workItemId": a }, "text": "Use Rust",
        "source": { "chatId": "chat-1", "messageId": "m-1", "excerpt": "use rust" },
        "operationId": "op-ia",
    });
    mutate(&client, create_a.clone()).await.unwrap();
    assert_eq!(
        mutate(&client, create_a).await.unwrap(),
        json!({ "instructionId": on_a }),
        "a replay returns the same id"
    );
    mutate(
        &client,
        json!({
            "op": "createWorkItemInstruction", "spaceId": "space-1",
            "scope": { "type": "workItem", "workItemId": b }, "text": "Write tests",
            "placement": { "type": "first" }, "operationId": "op-ib",
        }),
    )
    .await
    .unwrap();
    let texts = effective_texts(
        query(
            &client,
            json!({ "query": "resolveEffectiveInstructions", "workItemId": b }),
        )
        .await
        .unwrap(),
    );
    assert_eq!(texts, ["Be concise", "Use Rust", "Write tests"]);
    let over = mutate(
        &client,
        json!({
            "op": "createWorkItemInstruction", "spaceId": "space-1",
            "scope": { "type": "workItem", "workItemId": b }, "text": "x".repeat(6_001),
            "operationId": "op-big",
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(over.code, WorkOutlineErrorCode::LimitExceeded);

    // Capture a revision for B and select it for the Chat.
    let revision = derive_id("op-rev", roles::REVISION);
    let create_revision =
        json!({ "op": "createInstructionRevision", "workItemId": b, "operationId": "op-rev" });
    assert_eq!(
        mutate(&client, create_revision.clone()).await.unwrap(),
        json!({ "revisionId": revision })
    );
    assert_eq!(
        mutate(&client, create_revision).await.unwrap(),
        json!({ "revisionId": revision })
    );
    let application = mutate(
        &client,
        json!({
            "op": "applyInstructionRevision", "chatId": "chat-1", "revisionId": revision,
            "reason": "launch", "turnBoundary": { "messageId": "m-2" }, "operationId": "op-app",
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        application,
        json!({ "applicationId": derive_id("op-app", roles::APPLICATION) })
    );
    let selected = frame_until(&mut stream, |s| {
        s.chat_instruction_state("chat-1")
            .is_some_and(|c| c.selected_revision_id.is_some())
    })
    .await;
    let chat = selected.chat_instruction_state("chat-1").unwrap();
    assert!(!chat.drift);
    assert_eq!(
        chat.selected_revision_integrity,
        Some(mast_proto::SelectedRevisionIntegrity::Ok)
    );
    assert_eq!(selected.instructions.len(), 3);
    assert_eq!(
        selected.revision(&revision).unwrap().integrity,
        InstructionRevisionIntegrity::Ok
    );

    // Edit and archive after the revision: the revision is unchanged, the
    // Chat drifts, and the two revisions compare.
    mutate(
        &client,
        json!({ "op": "updateWorkItemInstruction", "instructionId": on_a, "text": "Use Rust 2024", "operationId": "op-edit" }),
    )
    .await
    .unwrap();
    mutate(
        &client,
        json!({ "op": "archiveWorkItemInstruction", "instructionId": derive_id("op-ib", roles::INSTRUCTION), "operationId": "op-arch" }),
    )
    .await
    .unwrap();
    mutate(
        &client,
        json!({ "op": "moveWorkItemInstruction", "instructionId": on_a, "placement": { "type": "first" }, "operationId": "op-move" }),
    )
    .await
    .unwrap();
    let drifted = frame_until(&mut stream, |s| {
        s.chat_instruction_state("chat-1").is_some_and(|c| c.drift)
    })
    .await;
    assert!(drifted.instructions.iter().any(|i| i.archived()));
    let stored = match query(
        &client,
        json!({ "query": "getInstructionRevision", "revisionId": revision }),
    )
    .await
    .unwrap()
    {
        WorkOutlineQueryResponse::GetInstructionRevision(view) => view,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(stored.integrity, InstructionRevisionIntegrity::Ok);
    let stored_texts: Vec<&str> = stored
        .body
        .entries
        .iter()
        .map(|e| e.text.as_str())
        .collect();
    assert_eq!(stored_texts, ["Be concise", "Use Rust", "Write tests"]);
    mutate(
        &client,
        json!({ "op": "createInstructionRevision", "workItemId": b, "operationId": "op-rev2" }),
    )
    .await
    .unwrap();
    let newer = derive_id("op-rev2", roles::REVISION);
    match query(
        &client,
        json!({ "query": "compareInstructionRevisions", "sourceRevisionId": revision, "targetRevisionId": newer }),
    )
    .await
    .unwrap()
    {
        WorkOutlineQueryResponse::CompareInstructionRevisions { comparison, .. } => {
            assert!(!comparison.identical);
            assert_eq!(comparison.changed.len(), 1);
            assert_eq!(comparison.removed.len(), 1);
        }
        other => panic!("unexpected {other:?}"),
    }

    // Query failures.
    let missing = query(
        &client,
        json!({ "query": "resolveEffectiveInstructions", "workItemId": "nope" }),
    )
    .await
    .unwrap_err();
    assert_eq!(missing.code, WorkOutlineErrorCode::NotFound);
    let missing = query(
        &client,
        json!({ "query": "compareInstructionRevisions", "sourceRevisionId": revision, "targetRevisionId": "nope" }),
    )
    .await
    .unwrap_err();
    assert_eq!(missing.code, WorkOutlineErrorCode::NotFound);
    let unassigned = mutate(
        &client,
        json!({
            "op": "applyInstructionRevision", "chatId": "ghost", "revisionId": revision,
            "reason": "explicit", "operationId": "op-ghost",
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(unassigned.code, WorkOutlineErrorCode::NotFound);

    // Deleting the Space removes the instruction rows with it.
    client
        .call(
            methods::MUTATE,
            json!({ "op": "deleteSpace", "spaceId": "space-1" }),
        )
        .await
        .unwrap();
    frame_until(&mut stream, |s| !s.available).await;
    assert!(core.workspace.instruction_revision(&revision).is_none());
    core.shutdown().await;
}

/// Durable replay protection for the instruction operations: after a restart
/// over the same store, replays return the derived ids without writing.
#[tokio::test(flavor = "multi_thread")]
async fn instruction_replays_after_restart_do_not_rewrite() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let (create, revise, apply);
    {
        let core = assemble(&data);
        let client = mast_rpc::memory_client(core.rpc_service());
        let (_a, b) = seed(&client).await;
        create = json!({
            "op": "createWorkItemInstruction", "spaceId": "space-1",
            "scope": { "type": "workItem", "workItemId": b }, "text": "Rule",
            "operationId": "op-i",
        });
        revise =
            json!({ "op": "createInstructionRevision", "workItemId": b, "operationId": "op-r" });
        apply = json!({
            "op": "applyInstructionRevision", "chatId": "chat-1",
            "revisionId": derive_id("op-r", roles::REVISION), "reason": "explicit",
            "operationId": "op-p",
        });
        for params in [&create, &revise, &apply] {
            mutate(&client, params.clone()).await.unwrap();
        }
        core.workspace.flush();
        assert!(core.workspace.ledger_holds("work-outline:op:op-p"));
        core.shutdown().await;
    }
    let core = assemble(&data);
    let client = mast_rpc::memory_client(core.rpc_service());
    let pending = core.workspace.registry_batches_written();
    assert_eq!(
        mutate(&client, create.clone()).await.unwrap(),
        json!({ "instructionId": derive_id("op-i", roles::INSTRUCTION) })
    );
    assert_eq!(
        mutate(&client, revise).await.unwrap(),
        json!({ "revisionId": derive_id("op-r", roles::REVISION) })
    );
    assert_eq!(
        mutate(&client, apply).await.unwrap(),
        json!({ "applicationId": derive_id("op-p", roles::APPLICATION) })
    );
    assert_eq!(
        core.workspace.registry_batches_written(),
        pending,
        "no op enqueued"
    );
    let mut other = create;
    other["text"] = json!("Different");
    assert_eq!(
        mutate(&client, other).await.unwrap_err().code,
        WorkOutlineErrorCode::OperationConflict
    );
    // The selection survived the restart.
    let pin = core
        .workspace
        .selected_instruction_revision("chat-1")
        .unwrap();
    assert_eq!(pin.revision_id, derive_id("op-r", roles::REVISION));
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_host_implements_the_instruction_registry_seam() {
    let tmp = tempfile::tempdir().unwrap();
    let core = assemble(&tmp.path().join("data"));
    let client = mast_rpc::memory_client(core.rpc_service());
    let (_a, b) = seed(&client).await;
    let registry: &dyn InstructionRegistry = &core.workspace;
    assert!(registry.selected_instruction_revision("chat-1").is_none());
    assert!(registry.instruction_revision("nope").is_none());

    mutate(
        &client,
        json!({
            "op": "createWorkItemInstruction", "spaceId": "space-1",
            "scope": { "type": "workItem", "workItemId": b }, "text": "Rule",
            "operationId": "op-i",
        }),
    )
    .await
    .unwrap();
    let revision = derive_id("op-r", roles::REVISION);
    mutate(
        &client,
        json!({ "op": "createInstructionRevision", "workItemId": b, "operationId": "op-r" }),
    )
    .await
    .unwrap();
    mutate(
        &client,
        json!({
            "op": "applyInstructionRevision", "chatId": "chat-1", "revisionId": revision,
            "reason": "launch", "operationId": "op-p",
        }),
    )
    .await
    .unwrap();
    let view = registry.instruction_revision(&revision).unwrap();
    assert_eq!(view.integrity, InstructionRevisionIntegrity::Ok);
    let pin = registry.selected_instruction_revision("chat-1").unwrap();
    assert_eq!(pin.revision_id, revision);
    assert_eq!(pin.revision_hash, view.body.hash);

    let report = ExecutionSessionReport {
        id: "es-1".into(),
        native_resume_id: Some("native".into()),
        harness: HarnessId::Mock,
        execution_root: "/tmp".into(),
        revision_id: Some(revision.clone()),
        revision_hash: Some(pin.revision_hash.clone()),
    };
    registry.report_execution_session("chat-1", report.clone());
    let pending = core.workspace.registry_batches_written();
    registry.report_execution_session("chat-1", report);
    assert_eq!(
        core.workspace.registry_batches_written(),
        pending,
        "an equal report writes nothing"
    );
    let mut stream = client
        .subscribe(methods::WATCH_WORK_OUTLINE, json!({ "spaceId": "space-1" }))
        .await
        .unwrap();
    let snapshot = frame_until(&mut stream, |s| {
        s.chat_instruction_state("chat-1")
            .is_some_and(|c| c.delivered_revision_id.is_some())
    })
    .await;
    let chat = snapshot.chat_instruction_state("chat-1").unwrap();
    assert_eq!(
        chat.delivered_revision_id.as_deref(),
        Some(revision.as_str())
    );
    assert_eq!(chat.delivered_revision_hash, chat.selected_revision_hash);
    core.shutdown().await;
}

/// `keepInstructionSession` over RPC: the selection reverts to the revision
/// the Chat's Execution Session reported, or to none when it reported none;
/// replays are idempotent and a later Apply wins.
#[tokio::test(flavor = "multi_thread")]
async fn keep_instruction_session_reverts_the_selection_to_the_session() {
    let tmp = tempfile::tempdir().unwrap();
    let core = assemble(&tmp.path().join("data"));
    let client = mast_rpc::memory_client(core.rpc_service());
    let (_a, b) = seed(&client).await;
    let registry: &dyn InstructionRegistry = &core.workspace;
    mutate(
        &client,
        json!({
            "op": "createWorkItemInstruction", "spaceId": "space-1",
            "scope": { "type": "workItem", "workItemId": b }, "text": "Rule",
            "operationId": "op-i",
        }),
    )
    .await
    .unwrap();
    let old = derive_id("op-r1", roles::REVISION);
    mutate(
        &client,
        json!({ "op": "createInstructionRevision", "workItemId": b, "operationId": "op-r1" }),
    )
    .await
    .unwrap();
    mutate(
        &client,
        json!({
            "op": "applyInstructionRevision", "chatId": "chat-1", "revisionId": old,
            "reason": "explicit", "operationId": "op-p1",
        }),
    )
    .await
    .unwrap();
    let old_pin = registry.selected_instruction_revision("chat-1").unwrap();
    registry.report_execution_session(
        "chat-1",
        ExecutionSessionReport {
            id: "es-1".into(),
            native_resume_id: Some("native".into()),
            harness: HarnessId::Codex,
            execution_root: "/tmp".into(),
            revision_id: Some(old.clone()),
            revision_hash: Some(old_pin.revision_hash.clone()),
        },
    );
    mutate(
        &client,
        json!({
            "op": "updateWorkItemInstruction", "instructionId": derive_id("op-i", roles::INSTRUCTION),
            "text": "Rule 2", "operationId": "op-edit",
        }),
    )
    .await
    .unwrap();
    let new = derive_id("op-r2", roles::REVISION);
    mutate(
        &client,
        json!({ "op": "createInstructionRevision", "workItemId": b, "operationId": "op-r2" }),
    )
    .await
    .unwrap();
    let apply_new = json!({
        "op": "applyInstructionRevision", "chatId": "chat-1", "revisionId": new,
        "reason": "explicit", "operationId": "op-p2",
    });
    mutate(&client, apply_new).await.unwrap();
    assert_eq!(
        registry
            .selected_instruction_revision("chat-1")
            .unwrap()
            .revision_id,
        new
    );

    // Keep: back to what the session received.
    let keep =
        json!({ "op": "keepInstructionSession", "chatId": "chat-1", "operationId": "op-k1" });
    let reply = mutate(&client, keep.clone()).await.unwrap();
    assert_eq!(
        reply,
        json!({ "applicationId": derive_id("op-k1", roles::APPLICATION) })
    );
    assert_eq!(
        registry.selected_instruction_revision("chat-1"),
        Some(old_pin)
    );
    let pending = core.workspace.registry_batches_written();
    assert_eq!(mutate(&client, keep).await.unwrap(), reply, "replay");
    assert_eq!(core.workspace.registry_batches_written(), pending);

    // A later Apply wins over the keep.
    mutate(
        &client,
        json!({
            "op": "applyInstructionRevision", "chatId": "chat-1", "revisionId": new,
            "reason": "explicit", "operationId": "op-p3",
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        registry
            .selected_instruction_revision("chat-1")
            .unwrap()
            .revision_id,
        new
    );

    // A session without instructions: keep selects none, drift stays visible.
    registry.report_execution_session(
        "chat-1",
        ExecutionSessionReport {
            id: "es-2".into(),
            native_resume_id: Some("native-2".into()),
            harness: HarnessId::Codex,
            execution_root: "/tmp".into(),
            revision_id: None,
            revision_hash: None,
        },
    );
    mutate(
        &client,
        json!({ "op": "keepInstructionSession", "chatId": "chat-1", "operationId": "op-k2" }),
    )
    .await
    .unwrap();
    assert!(registry.selected_instruction_revision("chat-1").is_none());
    let mut stream = client
        .subscribe(methods::WATCH_WORK_OUTLINE, json!({ "spaceId": "space-1" }))
        .await
        .unwrap();
    let snapshot = frame_until(&mut stream, |s| {
        s.chat_instruction_state("chat-1")
            .is_some_and(|c| c.selected_revision_id.is_none() && c.drift)
    })
    .await;
    assert!(snapshot.chat_instruction_state("chat-1").unwrap().drift);
    core.shutdown().await;
}
