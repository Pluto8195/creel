//! Execution registry tests: configure/reset writes, compare-before-write,
//! size and shape gates, whole-row resolution through the repaired tree,
//! binding capture (write once, replay, conflict, no revival), snapshot
//! exception markers, two-replica convergence, and Space deletion / sweep.

use std::collections::HashMap;

use super::*;
use crate::registry::tests::{chat, server_round, space};
use zeron_proto::work_outline::{
    ArchiveWorkItemParams, AssignChatToWorkItemParams, CreateWorkItemParams,
    DescendantArchivePolicy, MoveWorkItemParams, WorkOutlineActorKind, WorkOutlineMutateReply,
    WorkOutlineMutation, WorkOutlinePlacement, WorkOutlineSnapshot,
};
use zeron_proto::work_outline_execution::{ExecutionSourceScope, RepositoryBinding};

const SPACE: &str = "sp-1";
const OTHER_SPACE: &str = "sp-2";
const NOW: i64 = 1_700_000_000_000;

fn actor(device: &str) -> WorkOutlineActor {
    WorkOutlineActor {
        kind: WorkOutlineActorKind::User,
        device_id: device.into(),
        chat_id: None,
        message_id: None,
    }
}

fn doc_with_spaces(device: &str) -> RegistryDoc {
    let mut doc = RegistryDoc::new(device);
    doc.upsert_space(&space(SPACE, device, "/ws")).unwrap();
    doc.upsert_space(&space(OTHER_SPACE, device, "/other"))
        .unwrap();
    doc
}

fn apply(doc: &mut RegistryDoc, mutation: WorkOutlineMutation) -> Outcome<WorkOutlineMutateReply> {
    let device = doc.device_id().to_owned();
    doc.apply_work_outline_mutation(&mutation, &actor(&device), NOW)
}

fn item(doc: &mut RegistryDoc, op: &str, parent: Option<&str>) -> String {
    apply(
        doc,
        WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: SPACE.into(),
            parent_work_item_id: parent.map(str::to_owned),
            placement: WorkOutlinePlacement::Last,
            title: op.into(),
            description: None,
            status: None,
            operation_id: op.into(),
        }),
    )
    .unwrap()
    .work_item_id
    .unwrap()
}

fn binding(repo: &str, primary: bool) -> RepositoryBinding {
    RepositoryBinding {
        repo_id: format!("repo:{repo}"),
        repo_fingerprint: format!("{repo}-root"),
        repo_path: format!("/ws/{repo}"),
        member: true,
        primary,
        worktree_id: None,
        worktree_path: None,
        expected_ref: None,
    }
}

fn config(root: &str, bindings: Vec<RepositoryBinding>) -> ExecutionConfiguration {
    ExecutionConfiguration {
        execution_root: root.into(),
        bindings,
        schema: 1,
    }
}

fn work_item(id: &str) -> ExecutionScope {
    ExecutionScope::WorkItem {
        work_item_id: id.into(),
    }
}

fn space_scope() -> ExecutionScope {
    ExecutionScope::Space {
        space_id: SPACE.into(),
    }
}

fn configure(
    doc: &mut RegistryDoc,
    op: &str,
    scope: ExecutionScope,
    configuration: Option<ExecutionConfiguration>,
) -> Outcome<WorkOutlineMutateReply> {
    apply(
        doc,
        WorkOutlineMutation::ConfigureExecution(ConfigureExecutionParams {
            scope,
            configuration,
            operation_id: op.into(),
        }),
    )
}

fn space_chat(id: &str, space_id: Option<&str>) -> zeron_proto::Chat {
    let mut chat = chat(id, "dev-a");
    chat.space_id = space_id.map(str::to_owned);
    chat.parent_chat_id = None;
    chat
}

fn assign(doc: &mut RegistryDoc, op: &str, chat_id: &str, work_item: &str) {
    apply(
        doc,
        WorkOutlineMutation::AssignChatToWorkItem(AssignChatToWorkItemParams {
            chat_id: chat_id.into(),
            work_item_id: work_item.into(),
            operation_id: op.into(),
        }),
    )
    .unwrap();
}

fn capture(
    doc: &mut RegistryDoc,
    chat_id: &str,
    scope: &ExecutionScope,
    op: &str,
) -> Outcome<bool> {
    let resolved = doc.resolve_execution(scope).unwrap();
    let device = doc.device_id().to_owned();
    doc.capture_chat_execution_binding(chat_id, &resolved, op, &actor(&device), NOW)
}

fn snap(doc: &RegistryDoc) -> WorkOutlineSnapshot {
    doc.work_outline_snapshot(SPACE, true)
}

// ── configure / reset ───────────────────────────────────────────────────────

#[test]
fn configure_writes_one_whole_row_then_updates_and_resets() {
    let mut doc = doc_with_spaces("dev-a");
    let w = item(&mut doc, "w", None);
    let first = config("/ws", vec![binding("ui", true), binding("flagship", false)]);
    configure(&mut doc, "c1", work_item(&w), Some(first.clone())).unwrap();
    let id = format!("w:{w}");
    let row = doc.row(KIND_EXECUTION_CONFIGURATIONS, &id).unwrap();
    assert_eq!(row.fields["spaceId"], json!(SPACE));
    assert_eq!(row.fields["workItemId"], json!(w));
    assert_eq!(
        row.fields["configuration"],
        serde_json::to_value(&first).unwrap()
    );
    assert_eq!(row.fields["v"], json!(1));
    assert_eq!(row.fields["updatedBy"]["k"], json!("user"));

    // Equal configuration: compare-before-write, no new batch.
    let pending = doc.pending_len();
    configure(&mut doc, "c2", work_item(&w), Some(first.clone())).unwrap();
    assert_eq!(doc.pending_len(), pending);

    // Update: an `update` op that never carries the immutable fields.
    let second = config("/ws", vec![binding("ui", true)]);
    configure(&mut doc, "c3", work_item(&w), Some(second.clone())).unwrap();
    let last = doc.pending.last().unwrap().ops.last().unwrap().clone();
    assert_eq!(last.op, OpKind::Update);
    let set = last.set.unwrap();
    assert!(!set.contains_key("spaceId") && !set.contains_key("workItemId"));
    assert_eq!(
        snap(&doc).execution_configurations[0].configuration,
        Some(second.clone())
    );

    // Reset tombstones; a second reset is a no-op.
    configure(&mut doc, "c4", work_item(&w), None).unwrap();
    assert_eq!(
        doc.row_state(KIND_EXECUTION_CONFIGURATIONS, &id),
        RowState::Tombstoned
    );
    let pending = doc.pending_len();
    configure(&mut doc, "c5", work_item(&w), None).unwrap();
    assert_eq!(doc.pending_len(), pending);
    assert!(snap(&doc).execution_configurations.is_empty());

    // Explicit re-configure revives the scope row.
    configure(&mut doc, "c6", work_item(&w), Some(first.clone())).unwrap();
    assert_eq!(
        doc.row_state(KIND_EXECUTION_CONFIGURATIONS, &id),
        RowState::Live
    );
}

#[test]
fn space_default_row_id_and_fields() {
    let mut doc = doc_with_spaces("dev-a");
    configure(&mut doc, "c", space_scope(), Some(config("/ws", vec![]))).unwrap();
    let row = doc
        .row(KIND_EXECUTION_CONFIGURATIONS, &format!("s:{SPACE}"))
        .unwrap();
    assert!(!row.fields.contains_key("workItemId"));
    let view = &snap(&doc).execution_configurations[0];
    assert_eq!(view.work_item_id, None);
    assert_eq!(view.scope(), Some(space_scope()));
}

#[test]
fn configure_rejects_bad_shapes_and_targets() {
    let mut doc = doc_with_spaces("dev-a");
    let w = item(&mut doc, "w", None);
    let code = |r: Outcome<WorkOutlineMutateReply>| r.unwrap_err().code;
    assert_eq!(
        code(configure(
            &mut doc,
            "a",
            work_item(&w),
            Some(config("/ws", vec![binding("ui", true), binding("x", true)]))
        )),
        WorkOutlineErrorCode::Invalid
    );
    assert_eq!(
        code(configure(
            &mut doc,
            "b",
            work_item(&w),
            Some(config("rel", vec![]))
        )),
        WorkOutlineErrorCode::Invalid
    );
    assert_eq!(
        code(configure(
            &mut doc,
            "c",
            work_item("nope"),
            Some(config("/ws", vec![]))
        )),
        WorkOutlineErrorCode::NotFound
    );
    assert_eq!(
        code(configure(
            &mut doc,
            "d",
            ExecutionScope::Space {
                space_id: "gone".into()
            },
            Some(config("/ws", vec![]))
        )),
        WorkOutlineErrorCode::NotFound
    );
    // Oversize: many long bindings exceed the 8,000-unit budget.
    let long: Vec<RepositoryBinding> = (0..16)
        .map(|i| {
            let mut b = binding(&format!("r{i}"), false);
            b.repo_path = format!("/{}{i}", "p".repeat(600));
            b
        })
        .collect();
    assert_eq!(
        code(configure(
            &mut doc,
            "e",
            work_item(&w),
            Some(config("/ws", long))
        )),
        WorkOutlineErrorCode::LimitExceeded
    );
    // Archived item: configuring fails, resetting is allowed.
    apply(
        &mut doc,
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: w.clone(),
            descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
            operation_id: "arch".into(),
        }),
    )
    .unwrap();
    assert_eq!(
        code(configure(
            &mut doc,
            "f",
            work_item(&w),
            Some(config("/ws", vec![]))
        )),
        WorkOutlineErrorCode::Archived
    );
    configure(&mut doc, "g", work_item(&w), None).unwrap();
    // Nothing was written by any failure.
    assert!(snap(&doc).execution_configurations.is_empty());
}

// ── resolution ──────────────────────────────────────────────────────────────

#[test]
fn resolution_is_whole_row_over_the_repaired_tree() {
    let mut doc = doc_with_spaces("dev-a");
    let root = item(&mut doc, "root", None);
    let mid = item(&mut doc, "mid", Some(&root));
    let leaf = item(&mut doc, "leaf", Some(&mid));

    let r = doc.resolve_execution(&work_item(&leaf)).unwrap();
    assert_eq!(r.configuration, ExecutionConfiguration::implicit("/ws"));
    assert_eq!(r.device_id, "dev-a");
    assert!(matches!(
        r.source_scope,
        ExecutionSourceScope::ImplicitSpaceRoot { .. }
    ));

    configure(&mut doc, "s", space_scope(), Some(config("/ws/s", vec![]))).unwrap();
    assert!(matches!(
        doc.resolve_execution(&work_item(&leaf))
            .unwrap()
            .source_scope,
        ExecutionSourceScope::SpaceDefault { .. }
    ));
    let mid_cfg = config("/ws", vec![binding("ui", true)]);
    configure(&mut doc, "m", work_item(&mid), Some(mid_cfg.clone())).unwrap();
    let r = doc.resolve_execution(&work_item(&leaf)).unwrap();
    assert_eq!(r.configuration, mid_cfg);
    assert_eq!(
        r.source_scope,
        ExecutionSourceScope::WorkItem {
            work_item_id: mid.clone()
        }
    );
    // Same answer from the snapshot (client-side resolution).
    assert_eq!(snap(&doc).resolve_execution(&work_item(&leaf)), Some(r));

    // Moving the leaf to the root level leaves mid's configuration behind.
    apply(
        &mut doc,
        WorkOutlineMutation::MoveWorkItem(MoveWorkItemParams {
            work_item_id: leaf.clone(),
            destination_parent_work_item_id: None,
            placement: WorkOutlinePlacement::Last,
            operation_id: "mv".into(),
        }),
    )
    .unwrap();
    assert!(matches!(
        doc.resolve_execution(&work_item(&leaf))
            .unwrap()
            .source_scope,
        ExecutionSourceScope::SpaceDefault { .. }
    ));
    assert_eq!(
        doc.resolve_execution(&work_item("missing"))
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::NotFound
    );
}

#[test]
fn malformed_rows_fail_closed_and_are_reported() {
    let mut doc = doc_with_spaces("dev-a");
    let parent = item(&mut doc, "p", None);
    let child = item(&mut doc, "c", Some(&parent));
    configure(
        &mut doc,
        "p",
        work_item(&parent),
        Some(config("/ws", vec![])),
    )
    .unwrap();
    configure(
        &mut doc,
        "c",
        work_item(&child),
        Some(config("/ws", vec![])),
    )
    .unwrap();
    // Another writer stores a configuration with two primaries.
    let id = format!("w:{child}");
    let mut row = doc.row(KIND_EXECUTION_CONFIGURATIONS, &id).unwrap();
    row.fields.insert(
        "configuration".into(),
        json!({"executionRoot": "/ws", "bindings": [
            {"repoId": "a", "repoPath": "/ws/a", "primary": true},
            {"repoId": "b", "repoPath": "/ws/b", "primary": true}
        ], "schema": 1}),
    );
    row.seq = 1_000;
    let _ = doc.apply_rows(1_000, vec![row]);
    let r = doc.resolve_execution(&work_item(&child)).unwrap();
    assert!(
        r.invalid_reason.is_some(),
        "never falls through to the parent"
    );
    assert_eq!(
        r.source_scope,
        ExecutionSourceScope::WorkItem {
            work_item_id: child.clone()
        }
    );
    // Undecodable JSON keeps the row, flagged.
    let mut row = doc.row(KIND_EXECUTION_CONFIGURATIONS, &id).unwrap();
    row.fields.insert("configuration".into(), json!("nonsense"));
    row.seq = 1_001;
    let _ = doc.apply_rows(1_001, vec![row]);
    let view = snap(&doc)
        .execution_configurations
        .into_iter()
        .find(|v| v.id == id)
        .unwrap();
    assert!(view.configuration.is_none() && view.invalid_reason.is_some());
    // A row whose Space field is missing is dropped with a marker.
    let mut row = doc.row(KIND_EXECUTION_CONFIGURATIONS, &id).unwrap();
    row.fields.remove("spaceId");
    row.seq = 1_002;
    let _ = doc.apply_rows(1_002, vec![row]);
    assert!(
        snap(&doc)
            .repairs
            .contains(&WorkOutlineRepair::MissingSpace {
                row_kind: KIND_EXECUTION_CONFIGURATIONS.into(),
                id: id.clone(),
            })
    );
}

// ── bindings ────────────────────────────────────────────────────────────────

#[test]
fn binding_is_written_once_and_replays_idempotently() {
    let mut doc = doc_with_spaces("dev-a");
    let w = item(&mut doc, "w", None);
    configure(
        &mut doc,
        "c",
        work_item(&w),
        Some(config("/ws", vec![binding("ui", true)])),
    )
    .unwrap();
    doc.upsert_chat(&space_chat("chat-1", Some(SPACE))).unwrap();
    assert!(capture(&mut doc, "chat-1", &work_item(&w), "launch-1").unwrap());
    let row = doc.row(KIND_CHAT_EXECUTION_BINDINGS, "chat-1").unwrap();
    assert_eq!(row.fields["spaceId"], json!(SPACE));
    assert_eq!(row.fields["operationId"], json!("launch-1"));
    assert_eq!(row.fields["capturedAt"], json!(NOW));
    assert!(row.fields["resolved"]["configuration"].is_object());

    // Same operation, same inputs: no write.
    let pending = doc.pending_len();
    assert!(!capture(&mut doc, "chat-1", &work_item(&w), "launch-1").unwrap());
    assert_eq!(doc.pending_len(), pending);
    // Another operation: the binding is immutable.
    assert_eq!(
        capture(&mut doc, "chat-1", &work_item(&w), "launch-2")
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::OperationConflict
    );
    // Same operation, different inputs.
    let other = doc.resolve_execution(&space_scope()).unwrap();
    assert_eq!(
        doc.capture_chat_execution_binding("chat-1", &other, "launch-1", &actor("dev-a"), NOW)
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::OperationConflict
    );
    let view = doc.chat_execution_binding("chat-1").unwrap();
    assert_eq!(view.resolved.configuration.bindings.len(), 1);
}

#[test]
fn binding_capture_rejects_bad_targets_and_never_revives() {
    let mut doc = doc_with_spaces("dev-a");
    let resolved = doc.resolve_execution(&space_scope()).unwrap();
    let a = actor("dev-a");
    let code = |r: Outcome<bool>| r.unwrap_err().code;
    assert_eq!(
        code(doc.capture_chat_execution_binding("missing", &resolved, "op", &a, NOW)),
        WorkOutlineErrorCode::NotFound
    );
    doc.upsert_chat(&space_chat("np", None)).unwrap();
    assert_eq!(
        code(doc.capture_chat_execution_binding("np", &resolved, "op", &a, NOW)),
        WorkOutlineErrorCode::Projectless
    );
    doc.upsert_chat(&space_chat("other", Some(OTHER_SPACE)))
        .unwrap();
    assert_eq!(
        code(doc.capture_chat_execution_binding("other", &resolved, "op", &a, NOW)),
        WorkOutlineErrorCode::CrossSpace
    );
    doc.upsert_chat(&space_chat("chat-1", Some(SPACE))).unwrap();
    doc.capture_chat_execution_binding("chat-1", &resolved, "op", &a, NOW)
        .unwrap();
    doc.delete_row_ops(&[(KIND_CHAT_EXECUTION_BINDINGS, "chat-1")]);
    assert_eq!(
        code(doc.capture_chat_execution_binding("chat-1", &resolved, "op", &a, NOW)),
        WorkOutlineErrorCode::NotFound
    );
}

#[test]
fn editing_the_item_configuration_marks_existing_chats_as_exceptions() {
    let mut doc = doc_with_spaces("dev-a");
    let w = item(&mut doc, "w", None);
    configure(
        &mut doc,
        "c1",
        work_item(&w),
        Some(config("/ws", vec![binding("ui", true)])),
    )
    .unwrap();
    for chat in ["chat-1", "chat-2", "loose"] {
        doc.upsert_chat(&space_chat(chat, Some(SPACE))).unwrap();
    }
    assign(&mut doc, "a1", "chat-1", &w);
    assign(&mut doc, "a2", "chat-2", &w);
    capture(&mut doc, "chat-1", &work_item(&w), "l1").unwrap();
    capture(&mut doc, "chat-2", &work_item(&w), "l2").unwrap();
    // An unassociated Chat with a binding is never an exception.
    capture(&mut doc, "loose", &space_scope(), "l3").unwrap();
    let s = snap(&doc);
    assert!(s.execution_exceptions.is_empty());
    assert_eq!(s.chat_execution_bindings.len(), 3);
    assert_eq!(
        s.chat_execution_binding("chat-1")
            .unwrap()
            .work_item_id
            .as_deref(),
        Some(w.as_str())
    );
    assert!(
        s.chat_execution_binding("loose")
            .unwrap()
            .work_item_id
            .is_none()
    );

    // The item changes: existing Chats keep their binding and show as
    // exceptions.
    configure(
        &mut doc,
        "c2",
        work_item(&w),
        Some(config("/ws", vec![binding("flagship", true)])),
    )
    .unwrap();
    let s = snap(&doc);
    assert_eq!(
        s.execution_exception_chats(&w),
        ["chat-1".to_owned(), "chat-2".to_owned()]
    );
    let captured = s.chat_execution_binding("chat-1").unwrap();
    assert!(captured.exception);
    assert_eq!(
        captured.resolved.configuration.bindings[0].repo_id, "repo:ui",
        "association and edits never retarget a captured binding"
    );
    // A Chat captured elsewhere and then associated is an exception too,
    // while association itself never rewrote its binding.
    assign(&mut doc, "a3", "loose", &w);
    let s = snap(&doc);
    assert!(s.chat_execution_binding("loose").unwrap().exception);
    assert_eq!(
        s.chat_execution_binding("loose")
            .unwrap()
            .resolved
            .source_scope,
        ExecutionSourceScope::ImplicitSpaceRoot {
            space_id: SPACE.into()
        }
    );
    // Restoring the original configuration clears the markers.
    configure(
        &mut doc,
        "c3",
        work_item(&w),
        Some(config("/ws", vec![binding("ui", true)])),
    )
    .unwrap();
    let s = snap(&doc);
    assert_eq!(s.execution_exception_chats(&w), ["loose".to_owned()]);
}

#[test]
fn two_replicas_converge_on_configuration_and_bindings() {
    let mut a = doc_with_spaces("dev-a");
    let w = item(&mut a, "w", None);
    a.upsert_chat(&space_chat("chat-1", Some(SPACE))).unwrap();
    let mut b = RegistryDoc::new("dev-b");
    let mut server = HashMap::new();
    let mut seq = 0;
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    configure(&mut a, "ca", work_item(&w), Some(config("/ws/a", vec![]))).unwrap();
    b.clock.last_ms = Utc::now().timestamp_millis() + 60_000;
    configure(&mut b, "cb", work_item(&w), Some(config("/ws/b", vec![]))).unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    let (sa, sb) = (snap(&a), snap(&b));
    assert_eq!(sa, sb);
    // One whole register: the later write wins entirely.
    assert_eq!(
        sa.execution_configurations[0]
            .configuration
            .as_ref()
            .unwrap()
            .execution_root,
        "/ws/b"
    );
    capture(&mut b, "chat-1", &work_item(&w), "launch").unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    assert_eq!(snap(&a), snap(&b));
    // A replay on the other replica is recognized from the row itself.
    assert!(!capture(&mut a, "chat-1", &work_item(&w), "launch").unwrap());
}

// ── Space deletion and sweep ────────────────────────────────────────────────

#[test]
fn space_deletion_and_sweep_cover_execution_rows() {
    let mut doc = doc_with_spaces("dev-a");
    let w = item(&mut doc, "w", None);
    configure(&mut doc, "s", space_scope(), Some(config("/ws", vec![]))).unwrap();
    configure(&mut doc, "w", work_item(&w), Some(config("/ws", vec![]))).unwrap();
    doc.upsert_chat(&space_chat("chat-1", Some(SPACE))).unwrap();
    capture(&mut doc, "chat-1", &work_item(&w), "l").unwrap();
    let keys = doc.work_outline_rows_of_space(SPACE);
    assert!(keys.contains(&(KIND_EXECUTION_CONFIGURATIONS, format!("s:{SPACE}"))));
    assert!(keys.contains(&(KIND_EXECUTION_CONFIGURATIONS, format!("w:{w}"))));
    assert!(keys.contains(&(KIND_CHAT_EXECUTION_BINDINGS, "chat-1".to_owned())));
    doc.delete_space(SPACE).unwrap();
    for (kind, id) in &keys {
        assert_eq!(doc.row_state(kind, id), RowState::Tombstoned, "{kind} {id}");
    }
    let s = snap(&doc);
    assert!(!s.available && s.execution_configurations.is_empty());

    // Sweep: an offline replica configures the Space while another deletes
    // it; the late row survives the cascade until the sweep removes it.
    let mut a = doc_with_spaces("dev-a");
    let mut b = RegistryDoc::new("dev-b");
    let mut server = HashMap::new();
    let mut seq = 0;
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    a.delete_space(OTHER_SPACE).unwrap();
    let other = ExecutionScope::Space {
        space_id: OTHER_SPACE.into(),
    };
    configure(
        &mut b,
        "late",
        other.clone(),
        Some(config("/other", vec![])),
    )
    .unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    let id = other.row_id();
    assert_eq!(
        a.work_outline_orphans(),
        vec![(KIND_EXECUTION_CONFIGURATIONS, id.clone())]
    );
    assert_eq!(a.sweep_work_outline_orphans(), 1);
    assert_eq!(
        a.row_state(KIND_EXECUTION_CONFIGURATIONS, &id),
        RowState::Tombstoned
    );
}
