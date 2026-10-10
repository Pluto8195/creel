//! `WatchWorkOutline` + `WorkOutlineMutate` against a real (offline) engine,
//! over the in-memory RPC transport with the UI's exact wire shapes.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_proto::{
    HarnessId, WorkOutlineError, WorkOutlineErrorCode, WorkOutlineSnapshot, derive_id, roles,
};
use zeron_rpc::methods;

fn assemble(dir: &std::path::Path) -> EngineCore {
    std::fs::create_dir_all(dir).expect("create data dir");
    std::fs::write(dir.join("device-id"), "dev-a").expect("write device id");
    EngineCore::assemble(dir, Arc::new(HarnessRegistry::new()), HarnessId::Mock, None)
        .expect("engine core assembles")
}

async fn mutate(client: &zeron_rpc::RpcClient, params: Value) -> Result<Value, WorkOutlineError> {
    client
        .call(methods::WORK_OUTLINE_MUTATE, params)
        .await
        .map_err(|e| {
            let text = e.to_string();
            text.find("work-outline/")
                .and_then(|at| WorkOutlineError::from_wire_message(&text[at..]))
                .unwrap_or_else(|| panic!("not a work outline error: {text}"))
        })
}

/// Read frames until `predicate` holds (frames are deduped, so each one is a
/// content change).
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

#[tokio::test(flavor = "multi_thread")]
async fn watch_and_mutate_round_trip_over_rpc() {
    let tmp = tempfile::tempdir().unwrap();
    let core = assemble(&tmp.path().join("data"));
    let client = zeron_rpc::memory_client(core.rpc_service());
    let info: Value = client.call(methods::ENGINE_INFO, json!({})).await.unwrap();
    assert!(
        info["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("work-outline-v1"))
    );

    // An unknown Space streams an unavailable outline (not an error).
    let mut stream = client
        .subscribe(methods::WATCH_WORK_OUTLINE, json!({ "spaceId": "space-1" }))
        .await
        .unwrap();
    let first = frame_until(&mut stream, |_| true).await;
    assert!(!first.available);
    assert!(first.synced, "an edge-less engine is its own authority");
    assert_eq!(first.revision, 1);

    client
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": "space-1", "deviceId": "dev-a", "path": "/tmp" }),
        )
        .await
        .unwrap();
    for (chat, space) in [("chat-1", Some("space-1")), ("chat-np", None)] {
        let mut params = json!({ "op": "createChat", "chatId": chat, "deviceId": "dev-a" });
        if let Some(space) = space {
            params["spaceId"] = json!(space);
        }
        client.call(methods::MUTATE, params).await.unwrap();
    }
    let open = frame_until(&mut stream, |s| {
        s.available && !s.unassociated_chat_ids.is_empty()
    })
    .await;
    assert!(open.revision > first.revision);
    assert_eq!(
        open.unassociated_chat_ids,
        ["chat-1"],
        "projectless chats never listed"
    );

    // Create (and retry) a root and a child.
    let create = json!({
        "op": "createWorkItem", "spaceId": "space-1", "title": "Ship it",
        "placement": { "type": "last" }, "operationId": "op-root",
    });
    let reply = mutate(&client, create.clone()).await.unwrap();
    let root = derive_id("op-root", roles::ITEM);
    assert_eq!(reply, json!({ "workItemId": root }));
    assert_eq!(
        mutate(&client, create).await.unwrap(),
        reply,
        "retry returns the same id"
    );
    let conflict = mutate(
        &client,
        json!({ "op": "createWorkItem", "spaceId": "space-1", "title": "Other", "operationId": "op-root" }),
    )
    .await
    .unwrap_err();
    assert_eq!(conflict.code, WorkOutlineErrorCode::OperationConflict);
    mutate(
        &client,
        json!({
            "op": "createWorkItem", "spaceId": "space-1", "parentWorkItemId": root,
            "title": "Child", "operationId": "op-child",
        }),
    )
    .await
    .unwrap();
    mutate(
        &client,
        json!({ "op": "assignChatToWorkItem", "chatId": "chat-1", "workItemId": root, "operationId": "op-assign" }),
    )
    .await
    .unwrap();
    let projectless = mutate(
        &client,
        json!({ "op": "assignChatToWorkItem", "chatId": "chat-np", "workItemId": root, "operationId": "op-np" }),
    )
    .await
    .unwrap_err();
    assert_eq!(projectless.code, WorkOutlineErrorCode::Projectless);
    let missing = mutate(
        &client,
        json!({ "op": "updateWorkItem", "workItemId": "nope", "changes": { "title": "x" }, "operationId": "op-x" }),
    )
    .await
    .unwrap_err();
    assert_eq!(missing.code, WorkOutlineErrorCode::NotFound);

    let snapshot = frame_until(&mut stream, |s| {
        s.items.len() == 2 && s.item(&root).is_some_and(|i| i.chat_ids == ["chat-1"])
    })
    .await;
    assert_eq!(snapshot.root_work_item_ids, std::slice::from_ref(&root));
    assert_eq!(snapshot.item(&root).unwrap().child_ids.len(), 1);
    assert!(snapshot.unassociated_chat_ids.is_empty());
    // The chat row itself was never touched by the assignment.
    let chat = core.workspace.chat("chat-1").unwrap().unwrap();
    assert_eq!(chat.space_id.as_deref(), Some("space-1"));

    // A no-op mutation emits no frame; the next real change does, with a
    // strictly newer revision.
    mutate(
        &client,
        json!({ "op": "moveWorkItem", "workItemId": root, "placement": { "type": "last" }, "operationId": "op-noop" }),
    )
    .await
    .unwrap();
    mutate(
        &client,
        json!({
            "op": "archiveWorkItem", "workItemId": root,
            "descendantArchivePolicy": { "type": "archiveDescendants" }, "operationId": "op-archive",
        }),
    )
    .await
    .unwrap();
    let archived = frame_until(&mut stream, |s| {
        s.items.iter().all(|i| i.archived_at.is_some())
    })
    .await;
    assert_eq!(
        archived.revision,
        snapshot.revision + 1,
        "no frame for the no-op"
    );

    // Deleting the Space suppresses the outline.
    client
        .call(
            methods::MUTATE,
            json!({ "op": "deleteSpace", "spaceId": "space-1" }),
        )
        .await
        .unwrap();
    let gone = frame_until(&mut stream, |s| !s.available).await;
    assert!(gone.items.is_empty() && gone.assignments.is_empty());
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn outline_survives_an_engine_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let root = derive_id("op-root", roles::ITEM);
    {
        let core = assemble(&data);
        let client = zeron_rpc::memory_client(core.rpc_service());
        client
            .call(
                methods::MUTATE,
                json!({ "op": "createSpace", "spaceId": "space-1", "deviceId": "dev-a", "path": "/tmp" }),
            )
            .await
            .unwrap();
        mutate(
            &client,
            json!({ "op": "createWorkItem", "spaceId": "space-1", "title": "Kept", "operationId": "op-root" }),
        )
        .await
        .unwrap();
        core.workspace.flush();
        core.shutdown().await;
    }
    let core = assemble(&data);
    let client = zeron_rpc::memory_client(core.rpc_service());
    let mut stream = client
        .subscribe(methods::WATCH_WORK_OUTLINE, json!({ "spaceId": "space-1" }))
        .await
        .unwrap();
    let snapshot = frame_until(&mut stream, |s| s.available).await;
    assert_eq!(snapshot.root_work_item_ids, std::slice::from_ref(&root));
    assert_eq!(snapshot.item(&root).unwrap().title, "Kept");
    // A retry after the restart still converges on the same row.
    let reply = mutate(
        &client,
        json!({ "op": "createWorkItem", "spaceId": "space-1", "title": "Kept", "operationId": "op-root" }),
    )
    .await
    .unwrap();
    assert_eq!(reply, json!({ "workItemId": root }));
    core.shutdown().await;
}
