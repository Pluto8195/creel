//! `WatchWorkOutline` + `WorkOutlineMutate` against a real (offline) engine,
//! over the in-memory RPC transport with the UI's exact wire shapes.

use std::sync::Arc;
use std::time::Duration;

use mast_engine::{EngineCore, HarnessRegistry};
use mast_proto::{
    HarnessId, WorkOutlineError, WorkOutlineErrorCode, WorkOutlineSnapshot, derive_id, roles,
};
use mast_rpc::methods;
use serde_json::{Value, json};

fn assemble(dir: &std::path::Path) -> EngineCore {
    std::fs::create_dir_all(dir).expect("create data dir");
    std::fs::write(dir.join("device-id"), "dev-a").expect("write device id");
    EngineCore::assemble(dir, Arc::new(HarnessRegistry::new()), HarnessId::Mock, None)
        .expect("engine core assembles")
}

async fn mutate(client: &mast_rpc::RpcClient, params: Value) -> Result<Value, WorkOutlineError> {
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
    predicate: impl FnMut(&WorkOutlineSnapshot) -> bool,
) -> WorkOutlineSnapshot {
    frame_within(stream, Duration::from_secs(10), predicate).await
}

/// [`frame_until`] with an explicit timeout.
async fn frame_within(
    stream: &mut tokio::sync::mpsc::Receiver<Value>,
    timeout: Duration,
    mut predicate: impl FnMut(&WorkOutlineSnapshot) -> bool,
) -> WorkOutlineSnapshot {
    let deadline = tokio::time::Instant::now() + timeout;
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
    let client = mast_rpc::memory_client(core.rpc_service());
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
        let client = mast_rpc::memory_client(core.rpc_service());
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
    let client = mast_rpc::memory_client(core.rpc_service());
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

/// Durable replay protection: after an engine restart over the same store, a
/// retried move or update must not re-write (it would clobber the later
/// edit with a fresh clock), and the same id with other inputs conflicts.
#[tokio::test(flavor = "multi_thread")]
async fn replay_after_restart_does_not_rewrite() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let item = |op: &str| derive_id(op, roles::ITEM);
    let first_move = json!({
        "op": "moveWorkItem", "workItemId": item("op-x"), "destinationParentWorkItemId": item("op-p"),
        "placement": { "type": "last" }, "operationId": "op-move",
    });
    let first_rename = json!({
        "op": "updateWorkItem", "workItemId": item("op-x"), "changes": { "title": "First" },
        "operationId": "op-rename",
    });
    {
        let core = assemble(&data);
        let client = mast_rpc::memory_client(core.rpc_service());
        client
            .call(
                methods::MUTATE,
                json!({ "op": "createSpace", "spaceId": "space-1", "deviceId": "dev-a", "path": "/tmp" }),
            )
            .await
            .unwrap();
        for (op, title) in [("op-p", "P"), ("op-q", "Q"), ("op-x", "X")] {
            mutate(
                &client,
                json!({ "op": "createWorkItem", "spaceId": "space-1", "title": title, "operationId": op }),
            )
            .await
            .unwrap();
        }
        mutate(&client, first_move.clone()).await.unwrap();
        mutate(&client, first_rename.clone()).await.unwrap();
        // The ledger claims ride the registry snapshot save.
        core.workspace.flush();
        assert!(core.workspace.ledger_holds("work-outline:op:op-move"));
        core.shutdown().await;
    }

    let core = assemble(&data);
    let client = mast_rpc::memory_client(core.rpc_service());
    // A later edit (as if from another device) moves and renames X again.
    mutate(
        &client,
        json!({
            "op": "moveWorkItem", "workItemId": item("op-x"), "destinationParentWorkItemId": item("op-q"),
            "placement": { "type": "first" }, "operationId": "op-later-move",
        }),
    )
    .await
    .unwrap();
    mutate(
        &client,
        json!({
            "op": "updateWorkItem", "workItemId": item("op-x"), "changes": { "title": "Later" },
            "operationId": "op-later-rename",
        }),
    )
    .await
    .unwrap();
    let row_before = core
        .workspace
        .registry_row(mast_doc::KIND_WORK_ITEMS, &item("op-x"))
        .unwrap();
    let pending_before = core.workspace.registry_batches_written();

    // Replays of the pre-restart operations: acknowledged, nothing written.
    assert_eq!(
        mutate(&client, first_move.clone()).await.unwrap(),
        json!({})
    );
    assert_eq!(mutate(&client, first_rename).await.unwrap(), json!({}));
    let created = mutate(
        &client,
        json!({ "op": "createWorkItem", "spaceId": "space-1", "title": "X", "operationId": "op-x" }),
    )
    .await
    .unwrap();
    assert_eq!(created, json!({ "workItemId": item("op-x") }));
    let row_after = core
        .workspace
        .registry_row(mast_doc::KIND_WORK_ITEMS, &item("op-x"))
        .unwrap();
    assert_eq!(row_after.clocks, row_before.clocks, "no field re-written");
    assert_eq!(row_after.fields, row_before.fields);
    assert_eq!(
        core.workspace.registry_batches_written(),
        pending_before,
        "no op enqueued"
    );
    assert_eq!(row_after.fields["title"], json!("Later"));
    assert_eq!(row_after.fields["parentWorkItemId"], json!(item("op-q")));

    // Same operation id, different inputs, after the restart.
    let mut other = first_move;
    other["placement"] = json!({ "type": "first" });
    let conflict = mutate(&client, other).await.unwrap_err();
    assert_eq!(conflict.code, WorkOutlineErrorCode::OperationConflict);
    core.shutdown().await;
}

/// The watch skips recomputes for registry changes the outline does not
/// read (a device rename), but an outline change interleaved with them is
/// never missed.
#[tokio::test(flavor = "multi_thread")]
async fn unrelated_registry_changes_never_hide_an_outline_change() {
    let tmp = tempfile::tempdir().unwrap();
    let core = assemble(&tmp.path().join("data"));
    let client = mast_rpc::memory_client(core.rpc_service());
    client
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": "space-1", "deviceId": "dev-a", "path": "/tmp" }),
        )
        .await
        .unwrap();
    let mut stream = client
        .subscribe(methods::WATCH_WORK_OUTLINE, json!({ "spaceId": "space-1" }))
        .await
        .unwrap();
    frame_until(&mut stream, |s| s.available).await;

    // A device row is not an outline input: the marker stays put.
    let marker = core.workspace.work_outline_inputs_marker();
    let device = core.device_id.clone();
    let renamed = |n: usize| {
        core.workspace
            .rename_device(&device, &format!("Laptop {n}"))
            .unwrap()
    };
    assert!(renamed(0), "the engine's own device row exists");
    assert_eq!(core.workspace.work_outline_inputs_marker(), marker);

    // Outline writes buried among unrelated ones all arrive.
    for i in 0..5 {
        renamed(10 + i);
        mutate(
            &client,
            json!({ "op": "createWorkItem", "spaceId": "space-1", "title": format!("Item {i}"),
                    "operationId": format!("op-{i}") }),
        )
        .await
        .unwrap();
        renamed(20 + i);
        if i % 2 == 0 {
            // Let one coalescing window pass on an unrelated change alone.
            tokio::time::sleep(Duration::from_millis(120)).await;
        }
    }
    let last = frame_until(&mut stream, |s| s.items.len() == 5).await;
    assert!(last.items.iter().any(|item| item.title == "Item 4"));
    assert_ne!(core.workspace.work_outline_inputs_marker(), marker);

    // And a change after a long run of unrelated ones still lands.
    for n in 30..40 {
        renamed(n);
    }
    tokio::time::sleep(Duration::from_millis(120)).await;
    mutate(
        &client,
        json!({ "op": "createWorkItem", "spaceId": "space-1", "title": "Late",
                "operationId": "op-late" }),
    )
    .await
    .unwrap();
    frame_until(&mut stream, |s| {
        s.items.iter().any(|item| item.title == "Late")
    })
    .await;
}

/// An engine over the LOCAL profile (no edge), like the installed app's.
fn assemble_local(dir: &std::path::Path) -> EngineCore {
    std::fs::create_dir_all(dir).expect("create data dir");
    std::fs::write(dir.join("device-id"), "dev-a").expect("write device id");
    EngineCore::assemble_with_profile(
        mast_engine::EngineProfile::local(dir).expect("local profile"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .expect("engine core assembles")
}

/// Bulk rows straight into the persisted registry (no per-row reads):
/// `chats` Chat rows spread over `spaces`, then `history` session-status
/// writes cycling over them, the way months of agent turns pile up on a
/// local profile.
fn seed_large_registry(data: &std::path::Path, spaces: &[String], chats: usize, history: usize) {
    use mast_doc::{OpKind, RowOp};
    let store = mast_sync::DocsStore::open(
        mast_engine::EngineProfile::local(data)
            .unwrap()
            .store_root(),
    )
    .unwrap();
    let bytes = store
        .load_snapshot(mast_doc::REGISTRY_DOC_ID)
        .unwrap()
        .unwrap();
    let mut doc = mast_doc::RegistryDoc::from_bytes(&bytes, "dev-a").unwrap();
    let set = |value: Value| -> Option<std::collections::BTreeMap<String, Value>> {
        Some(serde_json::from_value(value).unwrap())
    };
    let mut ops = Vec::new();
    for i in 0..chats {
        ops.push(RowOp {
            kind: mast_doc::KIND_CHATS.into(),
            id: format!("chat-{i}"),
            op: OpKind::Upsert,
            set: set(json!({
                "id": format!("chat-{i}"), "deviceId": "dev-a", "title": format!("Chat {i}"),
                "archived": false, "createdAt": 1_788_000_000_000i64,
                "spaceId": spaces[i % spaces.len()],
            })),
            hlc: doc.mint_hlc(),
            clocks: None,
        });
    }
    for n in 0..history {
        let chat = n % chats;
        ops.push(RowOp {
            kind: mast_doc::KIND_SESSIONS.into(),
            id: format!("chat-{chat}"),
            op: OpKind::Upsert,
            set: set(json!({
                "chatId": format!("chat-{chat}"), "deviceId": "dev-a",
                "status": if n % 2 == 0 { "working" } else { "idle" },
                "updatedAt": 1_788_000_000_000i64 + n as i64,
            })),
            hlc: doc.mint_hlc(),
            clocks: None,
        });
    }
    doc.write_ops(ops).unwrap();
    store
        .save_snapshot(mast_doc::REGISTRY_DOC_ID, &doc.to_bytes().unwrap())
        .unwrap();
}

/// Drag and drop, keyboard moves and renames must show up promptly on a
/// realistic local profile: 2,000 Chats with a long session-status history,
/// 200 work items, ten watched Spaces. Measures the `WorkOutlineMutate`
/// round trip and the time until the watch frame that shows the change.
#[tokio::test(flavor = "multi_thread")]
async fn mutations_stay_fast_on_a_large_local_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let spaces: Vec<String> = (0..10).map(|i| format!("space-{i}")).collect();
    {
        let core = assemble_local(&data);
        let client = mast_rpc::memory_client(core.rpc_service());
        for space in &spaces {
            client
                .call(
                    methods::MUTATE,
                    json!({ "op": "createSpace", "spaceId": space, "deviceId": "dev-a",
                            "path": format!("/tmp/{space}") }),
                )
                .await
                .unwrap();
        }
        core.workspace.flush();
        core.shutdown().await;
    }
    seed_large_registry(&data, &spaces, 2_000, 40_000);

    let booted = std::time::Instant::now();
    let core = assemble_local(&data);
    eprintln!("boot over the seeded registry: {:?}", booted.elapsed());
    let client = mast_rpc::memory_client(core.rpc_service());
    let mut streams = Vec::new();
    for space in &spaces {
        let mut stream = client
            .subscribe(methods::WATCH_WORK_OUTLINE, json!({ "spaceId": space }))
            .await
            .unwrap();
        frame_within(&mut stream, Duration::from_secs(600), |s| s.available).await;
        streams.push(stream);
    }
    // The UI drains every watch; an unread stream would back up the shared
    // connection.
    let mut stream = streams.remove(0);
    for mut other in streams {
        tokio::spawn(async move { while other.recv().await.is_some() {} });
    }
    // 20 roots with 9 children each in space-0, one Chat on every child.
    let mut roots = Vec::new();
    let mut children = Vec::new();
    for r in 0..20 {
        let op = format!("op-root-{r}");
        mutate(
            &client,
            json!({ "op": "createWorkItem", "spaceId": "space-0", "title": format!("Root {r}"),
                    "operationId": op }),
        )
        .await
        .unwrap();
        let root = derive_id(&op, roles::ITEM);
        for c in 0..9 {
            let op = format!("op-child-{r}-{c}");
            mutate(
                &client,
                json!({ "op": "createWorkItem", "spaceId": "space-0", "parentWorkItemId": root,
                        "title": format!("Child {r}.{c}"), "operationId": op }),
            )
            .await
            .unwrap();
            let child = derive_id(&op, roles::ITEM);
            let chat = format!("chat-{}", (r * 9 + c) * spaces.len());
            mutate(
                &client,
                json!({ "op": "assignChatToWorkItem", "chatId": chat, "workItemId": child,
                        "operationId": format!("op-assign-{r}-{c}") }),
            )
            .await
            .unwrap();
            children.push(child);
        }
        roots.push(root);
    }
    let stream = &mut stream;
    frame_within(stream, Duration::from_secs(600), |s| s.items.len() == 200).await;
    eprintln!(
        "setup (watches + 200 items + 180 assignments): {:?}",
        booted.elapsed()
    );

    let mut round_trips = Vec::new();
    let mut frames = Vec::new();
    for n in 0..6 {
        let started = std::time::Instant::now();
        let child = children[n].clone();
        if n % 2 == 0 {
            // A drop onto another root's first child.
            mutate(
                &client,
                json!({ "op": "moveWorkItem", "workItemId": child,
                        "destinationParentWorkItemId": roots[19],
                        "placement": { "type": "before", "siblingWorkItemId": children[171] },
                        "operationId": format!("op-move-{n}") }),
            )
            .await
            .unwrap();
            round_trips.push(started.elapsed());
            frame_within(stream, Duration::from_secs(600), |s| {
                s.item(&child)
                    .is_some_and(|item| item.parent_work_item_id.as_deref() == Some(&roots[19]))
            })
            .await;
        } else {
            let title = format!("Renamed {n}");
            mutate(
                &client,
                json!({ "op": "updateWorkItem", "workItemId": child, "changes": { "title": title },
                        "operationId": format!("op-rename-{n}") }),
            )
            .await
            .unwrap();
            round_trips.push(started.elapsed());
            frame_within(stream, Duration::from_secs(600), |s| {
                s.item(&child).is_some_and(|item| item.title == title)
            })
            .await;
        }
        frames.push(started.elapsed());
    }
    round_trips.sort();
    frames.sort();
    let (round_trip, frame) = (round_trips[round_trips.len() / 2], frames[frames.len() / 2]);
    eprintln!(
        "pending after boot: {}; WorkOutlineMutate round trip median {round_trip:?} \
         (all {round_trips:?}); change visible in the watch median {frame:?} (all {frames:?})",
        core.workspace.registry_pending_len()
    );
    // Medians of six against generous budgets: a debug build measured
    // ~80 ms / ~210 ms with the queue settled and ~690 ms / ~1,030 ms
    // without (release is several times faster), so these only trip on the
    // regression, not on a loaded machine. The watch coalesces for 75 ms
    // before it recomputes.
    let (round_trip_budget, frame_budget) = if cfg!(debug_assertions) {
        (Duration::from_millis(350), Duration::from_millis(650))
    } else {
        (Duration::from_millis(100), Duration::from_millis(250))
    };
    assert_eq!(
        core.workspace.registry_pending_len(),
        0,
        "the queue settled"
    );
    assert!(
        round_trip < round_trip_budget,
        "mutation round trip {round_trip:?}"
    );
    assert!(frame < frame_budget, "frame latency {frame:?}");
    core.shutdown().await;
}
