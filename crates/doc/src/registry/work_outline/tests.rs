//! Work Outline registry tests: write rules R1-R8, placement, materialization
//! properties, convergence through a simulated room, Space suppression, and
//! the large-outline budget.

use std::collections::HashMap;

use super::*;
use crate::registry::tests::{chat, server_round, space};
use zeron_proto::work_outline::WorkItemChatAssignmentState;

const SPACE: &str = "sp-1";
const OTHER_SPACE: &str = "sp-2";

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
    doc.upsert_space(&space(SPACE, device, "/tmp/one")).unwrap();
    doc.upsert_space(&space(OTHER_SPACE, device, "/tmp/two"))
        .unwrap();
    doc
}

fn apply(doc: &mut RegistryDoc, mutation: WorkOutlineMutation) -> Outcome<WorkOutlineMutateReply> {
    let device = doc.device_id().to_owned();
    doc.apply_work_outline_mutation(&mutation, &actor(&device), 1_700_000_000_000)
}

fn create_in(
    doc: &mut RegistryDoc,
    space_id: &str,
    op: &str,
    parent: Option<&str>,
    placement: WorkOutlinePlacement,
    title: &str,
) -> String {
    apply(
        doc,
        WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: space_id.into(),
            parent_work_item_id: parent.map(str::to_owned),
            placement,
            title: title.into(),
            description: None,
            status: None,
            operation_id: op.into(),
        }),
    )
    .unwrap()
    .work_item_id
    .unwrap()
}

fn create(doc: &mut RegistryDoc, op: &str, parent: Option<&str>, title: &str) -> String {
    create_in(doc, SPACE, op, parent, WorkOutlinePlacement::Last, title)
}

fn move_to(
    doc: &mut RegistryDoc,
    op: &str,
    id: &str,
    parent: Option<&str>,
    placement: WorkOutlinePlacement,
) -> Outcome<WorkOutlineMutateReply> {
    apply(
        doc,
        WorkOutlineMutation::MoveWorkItem(MoveWorkItemParams {
            work_item_id: id.into(),
            destination_parent_work_item_id: parent.map(str::to_owned),
            placement,
            operation_id: op.into(),
        }),
    )
}

fn before(id: &str) -> WorkOutlinePlacement {
    WorkOutlinePlacement::Before {
        sibling_work_item_id: id.into(),
    }
}

fn after(id: &str) -> WorkOutlinePlacement {
    WorkOutlinePlacement::After {
        sibling_work_item_id: id.into(),
    }
}

fn snap(doc: &RegistryDoc) -> WorkOutlineSnapshot {
    doc.work_outline_snapshot(SPACE, true)
}

/// Titles of the effective children of `parent` (`None` = roots), in order.
fn titles(snapshot: &WorkOutlineSnapshot, parent: Option<&str>) -> Vec<String> {
    let ids = match parent {
        None => snapshot.root_work_item_ids.clone(),
        Some(id) => snapshot.item(id).unwrap().child_ids.clone(),
    };
    ids.iter()
        .map(|id| snapshot.item(id).unwrap().title.clone())
        .collect()
}

fn pending_kinds(doc: &RegistryDoc) -> BTreeSet<String> {
    doc.pending
        .iter()
        .flat_map(|b| b.ops.iter().map(|op| op.kind.clone()))
        .collect()
}

fn item_row(
    id: &str,
    space_id: &str,
    parent: Option<&str>,
    parent_clock: &str,
    key: Option<&str>,
) -> RegistryRow {
    let mut fields = BTreeMap::from([
        ("spaceId".to_owned(), json!(space_id)),
        ("title".to_owned(), json!(format!("T {id}"))),
    ]);
    let mut clocks = BTreeMap::from([
        ("spaceId".to_owned(), "0000000000001-000000-seed".to_owned()),
        ("title".to_owned(), "0000000000001-000000-seed".to_owned()),
        ("parentWorkItemId".to_owned(), parent_clock.to_owned()),
    ]);
    if let Some(parent) = parent {
        fields.insert("parentWorkItemId".into(), json!(parent));
    }
    if let Some(key) = key {
        fields.insert("orderKey".into(), json!(key));
        clocks.insert("orderKey".into(), parent_clock.to_owned());
    }
    RegistryRow {
        kind: KIND_WORK_ITEMS.into(),
        id: id.into(),
        seq: 0,
        deleted: false,
        del_hlc: None,
        fields,
        clocks,
    }
}

// ── mechanism: R1, R4, chunking ─────────────────────────────────────────────

#[test]
fn row_state_distinguishes_absent_live_and_tombstoned() {
    let mut doc = doc_with_spaces("dev-a");
    assert_eq!(doc.row_state(KIND_WORK_ITEMS, "nope"), RowState::Absent);
    let id = create(&mut doc, "op-1", None, "A");
    assert_eq!(doc.row_state(KIND_WORK_ITEMS, &id), RowState::Live);
    doc.delete_row_ops(&[(KIND_WORK_ITEMS, id.as_str())]);
    assert_eq!(doc.row_state(KIND_WORK_ITEMS, &id), RowState::Tombstoned);
    // A retried create of the deleted id never revives it.
    let err = apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: SPACE.into(),
            parent_work_item_id: None,
            placement: WorkOutlinePlacement::Last,
            title: "A".into(),
            description: None,
            status: None,
            operation_id: "op-1".into(),
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::NotFound);
    assert_eq!(doc.row_state(KIND_WORK_ITEMS, &id), RowState::Tombstoned);
}

fn small_op(i: usize) -> RowOp {
    RowOp {
        kind: "chats".into(),
        id: format!("c-{i}"),
        op: OpKind::Upsert,
        set: Some(BTreeMap::from([("title".to_owned(), json!("x"))])),
        hlc: encode_hlc(1, 0, "dev-a"),
        clocks: None,
    }
}

#[test]
fn chunking_keeps_400_op_batches_for_small_ops() {
    let mut doc = RegistryDoc::new("dev-a");
    doc.enqueue_ops((0..400).map(small_op).collect());
    assert_eq!(doc.pending_len(), 1);
    let mut doc = RegistryDoc::new("dev-a");
    doc.enqueue_ops((0..801).map(small_op).collect());
    let sizes: Vec<usize> = doc.pending.iter().map(|b| b.ops.len()).collect();
    assert_eq!(sizes, vec![400, 400, 1]);
}

#[test]
fn chunking_is_byte_aware_for_large_ops() {
    let mut doc = RegistryDoc::new("dev-a");
    let big = "x".repeat(12_000);
    let ops: Vec<RowOp> = (0..400)
        .map(|i| {
            let mut op = small_op(i);
            op.set = Some(BTreeMap::from([("title".to_owned(), json!(big))]));
            op
        })
        .collect();
    let total = ops.len();
    doc.enqueue_ops(ops);
    assert!(doc.pending_len() > 1, "400 x 12 KB must not be one frame");
    for batch in &doc.pending {
        let units: usize = batch.ops.iter().map(op_utf16_units).sum();
        assert!(units <= MAX_BATCH_UTF16_UNITS, "batch of {units} units");
    }
    // Order is preserved across chunks.
    let ids: Vec<String> = doc
        .pending
        .iter()
        .flat_map(|b| b.ops.iter().map(|op| op.id.clone()))
        .collect();
    assert_eq!(ids.len(), total);
    assert_eq!(ids[0], "c-0");
    assert_eq!(ids[total - 1], "c-399");
}

#[test]
fn oversize_ops_are_refused_before_the_queue() {
    let mut doc = doc_with_spaces("dev-a");
    let before = doc.pending_len();
    let mut op = small_op(0);
    op.set = Some(BTreeMap::from([(
        "title".to_owned(),
        json!("y".repeat(16_000)),
    )]));
    assert!(doc.write_ops(vec![small_op(1), op]).is_err());
    assert_eq!(
        doc.pending_len(),
        before,
        "nothing enqueued, not even the small op"
    );

    // 8,000 quote characters are within the visible limit but serialize
    // to 16,000+ units: the op gate catches what the field check cannot.
    let err = apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: SPACE.into(),
            parent_work_item_id: None,
            placement: WorkOutlinePlacement::Last,
            title: "Big".into(),
            description: Some("\"".repeat(8_000)),
            status: None,
            operation_id: "op-big".into(),
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::LimitExceeded);
    assert!(
        err.message.contains("Description is too long to save")
            && err.message.contains("units after encoding"),
        "field-level message, got: {}",
        err.message
    );
    assert_eq!(doc.pending_len(), before);
    assert!(snap(&doc).items.is_empty());
}

// ── acceptance 1: three-level outline ───────────────────────────────────────

#[test]
fn three_level_create_rename_archive_move_reorder() {
    let mut doc = doc_with_spaces("dev-a");
    let a = create(&mut doc, "op-a", None, "A");
    let b = create(&mut doc, "op-b", None, "B");
    let a1 = create(&mut doc, "op-a1", Some(&a), "A1");
    let a2 = create(&mut doc, "op-a2", Some(&a), "A2");
    let a1x = create(&mut doc, "op-a1x", Some(&a1), "A1x");
    let first = create_in(
        &mut doc,
        SPACE,
        "op-a0",
        Some(&a),
        WorkOutlinePlacement::First,
        "A0",
    );
    let mid = create_in(&mut doc, SPACE, "op-mid", Some(&a), after(&a1), "A1.5");
    let s = snap(&doc);
    assert_eq!(titles(&s, None), ["A", "B"]);
    assert_eq!(titles(&s, Some(&a)), ["A0", "A1", "A1.5", "A2"]);
    assert_eq!(titles(&s, Some(&a1)), ["A1x"]);
    assert_eq!(s.item(&a1x).unwrap().depth, 2);
    assert!(s.repairs.is_empty(), "{:?}", s.repairs);
    // Pre-order with depth.
    let order: Vec<&str> = s.items.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(order, ["A", "A0", "A1", "A1x", "A1.5", "A2", "B"]);

    // Rename.
    apply(
        &mut doc,
        WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: a1.clone(),
            changes: zeron_proto::WorkItemChanges {
                title: Some("A-one".into()),
                status: Some(WorkItemStatus::Active),
                ..Default::default()
            },
            operation_id: "op-rename".into(),
        }),
    )
    .unwrap();
    // Reorder within A, then move a level-3 item to level 1.
    move_to(&mut doc, "op-r", &a2, Some(&a), before(&first)).unwrap();
    move_to(&mut doc, "op-m", &a1x, None, before(&a)).unwrap();
    let s = snap(&doc);
    assert_eq!(titles(&s, None), ["A1x", "A", "B"]);
    assert_eq!(titles(&s, Some(&a)), ["A2", "A0", "A-one", "A1.5"]);
    assert_eq!(s.item(&a1).unwrap().status, WorkItemStatus::Active);
    assert_eq!(s.item(&a).unwrap().rollup.active_count, 1);
    assert_eq!(s.item(&a).unwrap().rollup.pending_count, 4);
    // Group P: every placement op wrote both fields with one clock.
    let row = doc.overlay_row(KIND_WORK_ITEMS, &a1x).unwrap();
    assert_eq!(row.clocks["parentWorkItemId"], row.clocks["orderKey"]);
    // Immutable fields were written once, by the create.
    let created = doc.overlay_row(KIND_WORK_ITEMS, &a1).unwrap();
    assert!(created.clocks["spaceId"] < created.clocks["title"]);

    // Archive with cascade, then restore.
    apply(
        &mut doc,
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: a.clone(),
            descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
            operation_id: "op-arch".into(),
        }),
    )
    .unwrap();
    let s = snap(&doc);
    assert_eq!(
        s.item(&a).unwrap().archive_cause,
        Some(WorkItemArchiveCause::SelfArchived)
    );
    for id in [&first, &a1, &a2, &mid] {
        assert_eq!(
            s.item(id).unwrap().archive_cause,
            Some(WorkItemArchiveCause::Cascade)
        );
    }
    assert!(s.item(&b).unwrap().archived_at.is_none());
    assert_eq!(
        s.item(&a).unwrap().rollup.pending_count,
        0,
        "archived not counted"
    );
    apply(
        &mut doc,
        WorkOutlineMutation::RestoreWorkItem(RestoreWorkItemParams {
            work_item_id: a.clone(),
            operation_id: "op-restore".into(),
        }),
    )
    .unwrap();
    let s = snap(&doc);
    assert!(s.items.iter().all(|i| i.archived_at.is_none()));
}

#[test]
fn archive_moving_descendants_and_restore_respects_self_archived() {
    let mut doc = doc_with_spaces("dev-a");
    let a = create(&mut doc, "op-a", None, "A");
    let dest = create(&mut doc, "op-d", None, "D");
    let d0 = create(&mut doc, "op-d0", Some(&dest), "D0");
    let c1 = create(&mut doc, "op-c1", Some(&a), "C1");
    let c2 = create(&mut doc, "op-c2", Some(&a), "C2");
    let g = create(&mut doc, "op-g", Some(&c1), "G");
    // Moving under the archived item's own descendant is a cycle.
    let err = apply(
        &mut doc,
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: a.clone(),
            descendant_archive_policy: DescendantArchivePolicy::MoveDescendants {
                destination_parent_work_item_id: Some(g.clone()),
            },
            operation_id: "op-bad".into(),
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::Cycle);
    let pending = doc.pending_len();
    apply(
        &mut doc,
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: a.clone(),
            descendant_archive_policy: DescendantArchivePolicy::MoveDescendants {
                destination_parent_work_item_id: Some(dest.clone()),
            },
            operation_id: "op-arch".into(),
        }),
    )
    .unwrap();
    assert_eq!(
        doc.pending_len(),
        pending + 1,
        "one logical mutation, one batch"
    );
    let s = snap(&doc);
    assert_eq!(titles(&s, Some(&dest)), ["D0", "C1", "C2"]);
    assert!(s.item(&a).unwrap().child_ids.is_empty());
    assert!(s.item(&c1).unwrap().archived_at.is_none());
    assert_eq!(
        s.item(&g).unwrap().parent_work_item_id.as_deref(),
        Some(c1.as_str())
    );
    let _ = d0;

    // Self-archived descendants stay archived when an ancestor is restored.
    apply(
        &mut doc,
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: c1.clone(),
            descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
            operation_id: "op-c1".into(),
        }),
    )
    .unwrap();
    apply(
        &mut doc,
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: dest.clone(),
            descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
            operation_id: "op-dest".into(),
        }),
    )
    .unwrap();
    apply(
        &mut doc,
        WorkOutlineMutation::RestoreWorkItem(RestoreWorkItemParams {
            work_item_id: dest.clone(),
            operation_id: "op-rd".into(),
        }),
    )
    .unwrap();
    let s = snap(&doc);
    assert!(s.item(&dest).unwrap().archived_at.is_none());
    assert!(s.item(&c2).unwrap().archived_at.is_none());
    assert_eq!(
        s.item(&c1).unwrap().archive_cause,
        Some(WorkItemArchiveCause::SelfArchived)
    );
    assert!(
        s.item(&g).unwrap().archived_at.is_some(),
        "under a self-archived item"
    );
    // Creating under an archived parent is refused.
    let err = apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: SPACE.into(),
            parent_work_item_id: Some(c1.clone()),
            placement: WorkOutlinePlacement::Last,
            title: "x".into(),
            description: None,
            status: None,
            operation_id: "op-x".into(),
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::Archived);
}

#[test]
fn move_validation_cross_space_self_and_descendant() {
    let mut doc = doc_with_spaces("dev-a");
    let a = create(&mut doc, "op-a", None, "A");
    let b = create(&mut doc, "op-b", Some(&a), "B");
    let other = create_in(
        &mut doc,
        OTHER_SPACE,
        "op-o",
        None,
        WorkOutlinePlacement::Last,
        "O",
    );
    let code = |r: Outcome<WorkOutlineMutateReply>| r.unwrap_err().code;
    assert_eq!(
        code(move_to(
            &mut doc,
            "m1",
            &a,
            Some(&b),
            WorkOutlinePlacement::Last
        )),
        WorkOutlineErrorCode::Cycle
    );
    assert_eq!(
        code(move_to(
            &mut doc,
            "m2",
            &a,
            Some(&a),
            WorkOutlinePlacement::Last
        )),
        WorkOutlineErrorCode::Cycle
    );
    assert_eq!(
        code(move_to(
            &mut doc,
            "m3",
            &a,
            Some(&other),
            WorkOutlinePlacement::Last
        )),
        WorkOutlineErrorCode::CrossSpace
    );
    assert_eq!(
        code(move_to(&mut doc, "m4", &a, None, before("nope"))),
        WorkOutlineErrorCode::NotFound
    );
    assert_eq!(
        code(move_to(&mut doc, "m5", &a, None, before(&a))),
        WorkOutlineErrorCode::Invalid
    );
    // A no-op reorder writes nothing.
    let pending = doc.pending_len();
    move_to(&mut doc, "m6", &a, None, WorkOutlinePlacement::Last).unwrap();
    assert_eq!(doc.pending_len(), pending);
}

// ── placement: tie groups, cap, length overflow ─────────────────────────────

#[test]
fn tie_groups_are_rekeyed_in_the_same_batch() {
    let mut doc = doc_with_spaces("dev-a");
    // Three siblings with equal keys (two devices minted the same key).
    let rows = vec![
        item_row("w1", SPACE, None, "0000000000010-000000-x", Some("a0")),
        item_row("w2", SPACE, None, "0000000000010-000000-x", Some("a0")),
        item_row("w3", SPACE, None, "0000000000010-000000-x", Some("a0")),
        item_row("w4", SPACE, None, "0000000000010-000000-x", Some("a2")),
    ];
    let _ = doc.apply_rows(1, rows);
    let pending = doc.pending_len();
    let new = create_in(&mut doc, SPACE, "op-n", None, after("w1"), "N");
    assert_eq!(doc.pending_len(), pending + 1);
    let s = snap(&doc);
    assert_eq!(s.root_work_item_ids, ["w1", new.as_str(), "w2", "w3", "w4"]);
    let keys: Vec<&str> = s.items.iter().map(|i| i.order_key.as_str()).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        5,
        "all keys distinct after re-keying: {keys:?}"
    );
    assert_eq!(
        doc.overlay_row(KIND_WORK_ITEMS, "w4").unwrap().fields["orderKey"],
        json!("a2")
    );
}

#[test]
fn invalid_keys_sort_last_and_length_overflow_rebalances() {
    // `place` alone: a gap between two keys at the 64-char cap.
    let lo = format!("a0{}", "0".repeat(61)) + "1";
    let hi = format!("a0{}", "0".repeat(61)) + "2";
    assert_eq!(lo.len(), 64);
    let siblings = vec![
        ("x".to_owned(), Some(lo.clone())),
        ("y".to_owned(), Some(hi.clone())),
    ];
    let placed = place(&siblings, 1, 1).unwrap();
    assert_eq!(placed.rekeys.len(), 2, "whole group rebalanced");
    let x = &placed.rekeys[0].1;
    let y = &placed.rekeys[1].1;
    assert!(x < &placed.keys[0] && placed.keys[0] < *y);
    assert!(x.len() <= 3 && y.len() <= 3);

    let mut doc = doc_with_spaces("dev-a");
    let rows = vec![
        item_row("w1", SPACE, None, "0000000000010-000000-x", Some("a0")),
        item_row(
            "w2",
            SPACE,
            None,
            "0000000000010-000000-x",
            Some("not valid!"),
        ),
        item_row("w0", SPACE, None, "0000000000010-000000-x", None),
    ];
    let _ = doc.apply_rows(1, rows);
    let s = snap(&doc);
    assert_eq!(s.root_work_item_ids, ["w1", "w0", "w2"]);
    assert!(s.repairs.contains(&WorkOutlineRepair::BadOrderKey {
        work_item_id: "w2".into()
    }));
    apply(
        &mut doc,
        WorkOutlineMutation::RepairWorkOutline(RepairWorkOutlineParams {
            space_id: SPACE.into(),
            operation_id: "op-rep".into(),
        }),
    )
    .unwrap();
    let repaired = snap(&doc);
    assert_eq!(repaired.root_work_item_ids, s.root_work_item_ids);
    assert!(repaired.repairs.is_empty(), "{:?}", repaired.repairs);
}

#[test]
fn children_cap_is_enforced() {
    let mut doc = doc_with_spaces("dev-a");
    let keys = order_keys_between(None, None, limits::MAX_CHILDREN_PER_PARENT).unwrap();
    let rows: Vec<RegistryRow> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| {
            item_row(
                &format!("w{i:05}"),
                SPACE,
                None,
                "0000000000010-000000-x",
                Some(k),
            )
        })
        .collect();
    let _ = doc.apply_rows(1, rows);
    let err = apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: SPACE.into(),
            parent_work_item_id: None,
            placement: WorkOutlinePlacement::First,
            title: "one too many".into(),
            description: None,
            status: None,
            operation_id: "op-cap".into(),
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::LimitExceeded);
}

// ── idempotency ─────────────────────────────────────────────────────────────

#[test]
fn retries_with_the_same_operation_id_never_duplicate() {
    let mut doc = doc_with_spaces("dev-a");
    let params = CreateWorkItemParams {
        space_id: SPACE.into(),
        parent_work_item_id: None,
        placement: WorkOutlinePlacement::Last,
        title: "Once".into(),
        description: Some("d".into()),
        status: None,
        operation_id: "op-retry".into(),
    };
    let first = apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItem(params.clone()),
    )
    .unwrap();
    let pending = doc.pending_len();
    let second = apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItem(params.clone()),
    )
    .unwrap();
    assert_eq!(first, second);
    assert_eq!(
        first.work_item_id.as_deref(),
        Some(derive_id("op-retry", roles::ITEM).as_str())
    );
    assert_eq!(doc.pending_len(), pending, "replay writes nothing");
    assert_eq!(snap(&doc).items.len(), 1);
    // Same operation id, different inputs.
    let mut other = params;
    other.title = "Twice".into();
    let err = apply(&mut doc, WorkOutlineMutation::CreateWorkItem(other)).unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::OperationConflict);

    // References and assignments are keyed so a replay converges too.
    let item = first.work_item_id.unwrap();
    let reference = CreateWorkItemExternalReferenceParams {
        work_item_id: item.clone(),
        provider: "github".into(),
        reference_type: "pullRequest".into(),
        external_id: "acme/repo#1".into(),
        url: "https://github.com/acme/repo/pull/1".into(),
        cached_title: Some("Fix".into()),
        operation_id: "op-ref".into(),
    };
    for _ in 0..2 {
        apply(
            &mut doc,
            WorkOutlineMutation::CreateWorkItemExternalReference(reference.clone()),
        )
        .unwrap();
    }
    let s = snap(&doc);
    assert_eq!(s.external_references.len(), 1);
    assert_eq!(s.item(&item).unwrap().external_reference_ids.len(), 1);
    // Remove, then an explicit re-add revives the content-derived row.
    let id = s.external_references[0].id.clone();
    apply(
        &mut doc,
        WorkOutlineMutation::RemoveWorkItemExternalReference(
            RemoveWorkItemExternalReferenceParams {
                reference_id: id.clone(),
                operation_id: "op-rm".into(),
            },
        ),
    )
    .unwrap();
    assert!(snap(&doc).external_references.is_empty());
    let mut readd = reference;
    readd.operation_id = "op-readd".into();
    apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItemExternalReference(readd),
    )
    .unwrap();
    assert_eq!(snap(&doc).external_references.len(), 1);
}

// ── acceptance 3 and 4: Chats ───────────────────────────────────────────────

fn space_chat(id: &str, space_id: Option<&str>) -> zeron_proto::Chat {
    let mut chat = chat(id, "dev-a");
    chat.space_id = space_id.map(str::to_owned);
    chat.parent_chat_id = None;
    chat
}

fn assign(
    doc: &mut RegistryDoc,
    op: &str,
    chat_id: &str,
    item: &str,
) -> Outcome<WorkOutlineMutateReply> {
    apply(
        doc,
        WorkOutlineMutation::AssignChatToWorkItem(AssignChatToWorkItemParams {
            chat_id: chat_id.into(),
            work_item_id: item.into(),
            operation_id: op.into(),
        }),
    )
}

fn unassign(doc: &mut RegistryDoc, op: &str, chat_id: &str) {
    apply(
        doc,
        WorkOutlineMutation::RemoveChatFromWorkItem(RemoveChatFromWorkItemParams {
            chat_id: chat_id.into(),
            operation_id: op.into(),
        }),
    )
    .unwrap();
}

#[test]
fn association_never_touches_chat_space_or_session_rows() {
    let mut doc = doc_with_spaces("dev-a");
    doc.upsert_chat(&space_chat("c1", Some(SPACE))).unwrap();
    doc.upsert_chat(&space_chat("c2", Some(SPACE))).unwrap();
    let a = create(&mut doc, "op-a", None, "A");
    let b = create(&mut doc, "op-b", None, "B");
    let mut server = HashMap::new();
    let mut seq = 0;
    server_round(&mut server, &mut seq, &mut [&mut doc]);
    let chat_rows = |doc: &RegistryDoc| {
        ["c1", "c2"]
            .map(|id| serde_json::to_vec(&doc.overlay_row(KIND_CHATS, id).unwrap()).unwrap())
    };
    let space_row = |doc: &RegistryDoc| {
        serde_json::to_vec(&doc.overlay_row(KIND_SPACES, SPACE).unwrap()).unwrap()
    };
    let chats_before = chat_rows(&doc);
    let space_before = space_row(&doc);

    assign(&mut doc, "o1", "c1", &a).unwrap();
    assign(&mut doc, "o2", "c2", &a).unwrap();
    let s = snap(&doc);
    assert_eq!(s.item(&a).unwrap().chat_ids, ["c1", "c2"]);
    assert_eq!(s.item(&a).unwrap().rollup.associated_chat_count, 2);
    assert!(s.unassociated_chat_ids.is_empty());
    assign(&mut doc, "o3", "c1", &b).unwrap(); // move
    unassign(&mut doc, "o4", "c2");
    let s = snap(&doc);
    assert_eq!(s.item(&b).unwrap().chat_ids, ["c1"]);
    assert!(s.item(&a).unwrap().chat_ids.is_empty());
    assert_eq!(s.unassociated_chat_ids, ["c2"]);
    let c2 = s.assignments.iter().find(|a| a.chat_id == "c2").unwrap();
    assert_eq!(c2.state, WorkItemChatAssignmentState::Unassociated);
    assert_eq!(c2.work_item_id, None);

    assert_eq!(
        pending_kinds(&doc),
        BTreeSet::from([KIND_WORK_ITEM_CHAT_ASSIGNMENTS.to_owned()])
    );
    server_round(&mut server, &mut seq, &mut [&mut doc]);
    assert_eq!(chat_rows(&doc), chats_before, "chat rows byte-identical");
    assert_eq!(space_row(&doc), space_before);
    // Group S was written as one op each time.
    let row = doc
        .overlay_row(KIND_WORK_ITEM_CHAT_ASSIGNMENTS, "c1")
        .unwrap();
    assert_eq!(row.clocks["workItemId"], row.clocks["assignedAt"]);
    assert_eq!(row.clocks["workItemId"], row.clocks["assignedBy"]);
    assert!(
        row.clocks["spaceId"] < row.clocks["workItemId"],
        "spaceId written once"
    );
}

#[test]
fn projectless_archived_child_dangling_and_cross_space_chats() {
    let mut doc = doc_with_spaces("dev-a");
    let a = create(&mut doc, "op-a", None, "A");
    let other = create_in(
        &mut doc,
        OTHER_SPACE,
        "op-o",
        None,
        WorkOutlinePlacement::Last,
        "O",
    );
    doc.upsert_chat(&space_chat("projectless", None)).unwrap();
    let mut archived = space_chat("archived", Some(SPACE));
    archived.archived = true;
    doc.upsert_chat(&archived).unwrap();
    let mut child = space_chat("child", Some(SPACE));
    child.parent_chat_id = Some("archived".into());
    doc.upsert_chat(&child).unwrap();
    doc.upsert_chat(&space_chat("gone", Some(SPACE))).unwrap();
    doc.upsert_chat(&space_chat("elsewhere", Some(OTHER_SPACE)))
        .unwrap();

    assert_eq!(
        assign(&mut doc, "p", "projectless", &a).unwrap_err().code,
        WorkOutlineErrorCode::Projectless
    );
    assert_eq!(
        assign(&mut doc, "x", "elsewhere", &a).unwrap_err().code,
        WorkOutlineErrorCode::CrossSpace
    );
    assert_eq!(
        assign(&mut doc, "n", "missing-chat", &a).unwrap_err().code,
        WorkOutlineErrorCode::NotFound
    );
    // Archived and child Chats are listed, never hidden.
    let s = snap(&doc);
    assert_eq!(s.unassociated_chat_ids, ["archived", "child", "gone"]);
    assign(&mut doc, "a1", "archived", &a).unwrap();
    assign(&mut doc, "a2", "gone", &a).unwrap();
    doc.delete_chat("gone").unwrap();
    let s = snap(&doc);
    // The deleted Chat stays inspectable as a dangling assignment.
    assert_eq!(s.item(&a).unwrap().chat_ids, ["archived", "gone"]);
    assert_eq!(s.item(&a).unwrap().rollup.associated_chat_count, 1);
    let gone = s.assignments.iter().find(|x| x.chat_id == "gone").unwrap();
    assert_eq!(gone.state, WorkItemChatAssignmentState::DanglingChat);
    assert!(s.repairs.contains(&WorkOutlineRepair::DanglingChat {
        chat_id: "gone".into()
    }));
    assert_eq!(s.unassociated_chat_ids, ["child"]);
    // Explicit removal of the dangling assignment.
    unassign(&mut doc, "rm", "gone");
    assert!(snap(&doc).item(&a).unwrap().chat_ids == ["archived"]);

    // An assignment written by a misbehaving peer is quarantined, not lost.
    let _ = doc.apply_rows(
        99,
        vec![
            RegistryRow {
                kind: KIND_WORK_ITEM_CHAT_ASSIGNMENTS.into(),
                id: "child".into(),
                seq: 99,
                deleted: false,
                del_hlc: None,
                fields: BTreeMap::from([
                    ("spaceId".to_owned(), json!(SPACE)),
                    ("workItemId".to_owned(), json!(other)),
                ]),
                clocks: BTreeMap::new(),
            },
            RegistryRow {
                kind: KIND_WORK_ITEM_CHAT_ASSIGNMENTS.into(),
                id: "elsewhere".into(),
                seq: 99,
                deleted: false,
                del_hlc: None,
                fields: BTreeMap::from([
                    ("spaceId".to_owned(), json!(SPACE)),
                    ("workItemId".to_owned(), json!(a)),
                ]),
                clocks: BTreeMap::new(),
            },
        ],
    );
    let s = snap(&doc);
    let state = |id: &str| {
        s.assignments
            .iter()
            .find(|x| x.chat_id == id)
            .unwrap()
            .state
    };
    assert_eq!(
        state("child"),
        WorkItemChatAssignmentState::DanglingWorkItem
    );
    assert_eq!(
        state("elsewhere"),
        WorkItemChatAssignmentState::SpaceMismatch
    );
    assert_eq!(s.unassociated_chat_ids, ["child"]);
    assert!(
        !s.item(&a)
            .unwrap()
            .chat_ids
            .contains(&"elsewhere".to_owned())
    );
}

// ── acceptance 2: offline concurrency through the room ──────────────────────

fn synced_pair() -> (
    RegistryDoc,
    RegistryDoc,
    HashMap<(String, String), RegistryRow>,
    u64,
) {
    let mut a = doc_with_spaces("dev-a");
    let mut b = RegistryDoc::new("dev-b");
    let mut server = HashMap::new();
    let mut seq = 0;
    for (op, title) in [("op-p", "P"), ("op-q", "Q"), ("op-r", "R")] {
        create(&mut a, op, None, title);
    }
    let p = derive_id("op-p", roles::ITEM);
    for (op, title) in [("op-x", "X"), ("op-y", "Y"), ("op-z", "Z")] {
        create(&mut a, op, Some(&p), title);
    }
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    assert_eq!(snap(&a), snap(&b));
    (a, b, server, seq)
}

fn assert_converged(a: &RegistryDoc, b: &RegistryDoc) -> WorkOutlineSnapshot {
    let (sa, sb) = (snap(a), snap(b));
    assert_eq!(sa, sb);
    assert_eq!(sa.items.len(), 6, "no item lost");
    for item in &sa.items {
        let row = a.overlay_row(KIND_WORK_ITEMS, &item.id).unwrap();
        assert_eq!(
            row.clocks.get("parentWorkItemId"),
            row.clocks.get("orderKey"),
            "whole placement, never torn"
        );
    }
    sa
}

#[test]
fn concurrent_move_vs_reorder_vs_move_converge() {
    let id = |op: &str| derive_id(op, roles::ITEM);
    // Move (A) vs reorder (B) of the same item.
    let (mut a, mut b, mut server, mut seq) = synced_pair();
    move_to(
        &mut a,
        "m1",
        &id("op-x"),
        Some(&id("op-q")),
        WorkOutlinePlacement::Last,
    )
    .unwrap();
    move_to(
        &mut b,
        "m2",
        &id("op-x"),
        Some(&id("op-p")),
        after(&id("op-z")),
    )
    .unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    let s = assert_converged(&a, &b);
    let x = s.item(&id("op-x")).unwrap();
    assert!(
        (x.parent_work_item_id == Some(id("op-q")))
            || (x.parent_work_item_id == Some(id("op-p"))
                && s.item(&id("op-p")).unwrap().child_ids.last() == Some(&id("op-x")))
    );

    // Move vs move to different parents.
    let (mut a, mut b, mut server, mut seq) = synced_pair();
    move_to(
        &mut a,
        "m1",
        &id("op-y"),
        Some(&id("op-q")),
        WorkOutlinePlacement::First,
    )
    .unwrap();
    move_to(
        &mut b,
        "m2",
        &id("op-y"),
        Some(&id("op-r")),
        WorkOutlinePlacement::First,
    )
    .unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    assert_converged(&a, &b);

    // Crossing moves create a raw cycle: repaired deterministically.
    let (mut a, mut b, mut server, mut seq) = synced_pair();
    move_to(
        &mut a,
        "m1",
        &id("op-q"),
        Some(&id("op-r")),
        WorkOutlinePlacement::Last,
    )
    .unwrap();
    move_to(
        &mut b,
        "m2",
        &id("op-r"),
        Some(&id("op-q")),
        WorkOutlinePlacement::Last,
    )
    .unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    let s = assert_converged(&a, &b);
    assert!(s.repairs.iter().any(
        |r| matches!(r, WorkOutlineRepair::CycleBroken { members, .. } if members.len() == 2)
    ));
    // The next move under the cycle persists the repair in its own batch,
    // so the user's move is never undone by the derived repair.
    let pending = a.pending_len();
    move_to(
        &mut a,
        "m3",
        &id("op-x"),
        Some(&id("op-q")),
        WorkOutlinePlacement::Last,
    )
    .unwrap();
    assert_eq!(a.pending_len(), pending + 1);
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    let s = assert_converged(&a, &b);
    assert!(
        !s.repairs
            .iter()
            .any(|r| matches!(r, WorkOutlineRepair::CycleBroken { .. }))
    );
    let x = s.item(&id("op-x")).unwrap();
    assert_eq!(x.parent_work_item_id, Some(id("op-q")));
}

fn tie_rekey_vs_offline_move(a_pushes_first: bool) {
    let mut a = doc_with_spaces("dev-a");
    let mut b = RegistryDoc::new("dev-b");
    let mut server = HashMap::new();
    let mut seq = 0;
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    // Three tied roots and a destination, all with old clocks.
    let old = "0000000000010-000000-x";
    let rows = vec![
        item_row("s1", SPACE, None, old, Some("a0")),
        item_row("s2", SPACE, None, old, Some("a0")),
        item_row("s3", SPACE, None, old, Some("a0")),
        item_row("q", SPACE, None, old, Some("a5")),
    ];
    for row in &rows {
        server.insert((row.kind.clone(), row.id.clone()), row.clone());
    }
    let _ = a.apply_rows(seq, rows.clone());
    let _ = b.apply_rows(seq, rows);
    assert_eq!(snap(&a), snap(&b));

    // B moves s2 under q while offline; A inserts after s1, which re-keys the
    // tie group (including s2) from A's stale view of s2's parent.
    move_to(&mut b, "m-b", "s2", Some("q"), WorkOutlinePlacement::Last).unwrap();
    let new = create_in(&mut a, SPACE, "op-n", None, after("s1"), "N");
    let rekey = a
        .pending
        .iter()
        .flat_map(|batch| batch.ops.iter())
        .find(|op| op.id == "s2")
        .expect("s2 is re-keyed by A");
    assert_eq!(
        rekey
            .clocks
            .as_ref()
            .and_then(|c| c.get("parentWorkItemId")),
        Some(&old.to_owned()),
        "re-key keeps the sibling's parent at its existing clock"
    );

    // Pushes reach the server in either order; the move wins both ways.
    // (the server applies batches in slice order).
    if a_pushes_first {
        server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    } else {
        server_round(&mut server, &mut seq, &mut [&mut b, &mut a]);
    }
    let (sa, sb) = (snap(&a), snap(&b));
    assert_eq!(sa, sb, "identical materialization");
    assert_eq!(
        sa.item("s2").unwrap().parent_work_item_id.as_deref(),
        Some("q")
    );
    assert_eq!(titles(&sa, Some("q")), ["T s2"]);
    assert!(sa.root_work_item_ids.contains(&new));
    assert!(!sa.root_work_item_ids.iter().any(|id| id == "s2"));
}

#[test]
fn tie_rekey_never_reverts_a_concurrent_offline_move_of_the_sibling() {
    tie_rekey_vs_offline_move(true);
    tie_rekey_vs_offline_move(false);
}

#[test]
fn sweep_and_space_cascade_deletes_beat_a_fast_clock_writers_rows() {
    // B's wall clock runs an hour fast; its row carries a far-future clock.
    let (mut a, mut b, mut server, mut seq) = synced_pair();
    b.clock.last_ms = Utc::now().timestamp_millis() + 3_600_000;
    let late = create(&mut b, "op-late", None, "Late");
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    // A (normal clock) deletes the Space: the cascade must observe the fast
    // clocks, or its tombstones would lose to B's rows.
    a.delete_space(SPACE).unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    for doc in [&a, &b] {
        assert_eq!(doc.row_state(KIND_WORK_ITEMS, &late), RowState::Tombstoned);
    }

    // Same for the orphan sweep: a row from a fast device in a Space that is
    // already gone.
    let (mut a, mut b, mut server, mut seq) = synced_pair();
    b.clock.last_ms = Utc::now().timestamp_millis() + 3_600_000;
    a.delete_space(SPACE).unwrap();
    let late = create(&mut b, "op-late2", None, "Late");
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    assert_eq!(a.row_state(KIND_WORK_ITEMS, &late), RowState::Live);
    assert_eq!(a.sweep_work_outline_orphans(), 1);
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    for doc in [&a, &b] {
        assert_eq!(doc.row_state(KIND_WORK_ITEMS, &late), RowState::Tombstoned);
    }
}

#[test]
fn sweep_with_nothing_to_do_writes_nothing() {
    let (mut a, _b, _server, _seq) = synced_pair();
    let pending = a.pending_len();
    assert_eq!(a.sweep_work_outline_orphans(), 0);
    assert_eq!(a.pending_len(), pending);
}

#[test]
fn slow_clock_device_edit_is_not_lost() {
    let (mut a, mut b, mut server, mut seq) = synced_pair();
    let x = derive_id("op-x", roles::ITEM);
    // A's wall clock runs an hour fast.
    a.clock.last_ms = Utc::now().timestamp_millis() + 3_600_000;
    apply(
        &mut a,
        WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: x.clone(),
            changes: zeron_proto::WorkItemChanges {
                title: Some("from fast A".into()),
                ..Default::default()
            },
            operation_id: "u1".into(),
        }),
    )
    .unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    // B (normal clock) edits after seeing A's write: it must win.
    apply(
        &mut b,
        WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: x.clone(),
            changes: zeron_proto::WorkItemChanges {
                title: Some("later from B".into()),
                ..Default::default()
            },
            operation_id: "u2".into(),
        }),
    )
    .unwrap();
    move_to(&mut b, "m", &x, None, WorkOutlinePlacement::First).unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    let s = assert_converged(&a, &b);
    assert_eq!(s.item(&x).unwrap().title, "later from B");
    assert_eq!(s.root_work_item_ids.first(), Some(&x));
}

// ── acceptance 29: tombstones, restart, Space deletion ──────────────────────

#[test]
fn restart_round_trip_preserves_pending_outline_rows() {
    let mut doc = doc_with_spaces("dev-a");
    let a = create(&mut doc, "op-a", None, "A");
    create(&mut doc, "op-b", Some(&a), "B");
    doc.upsert_chat(&space_chat("c1", Some(SPACE))).unwrap();
    assign(&mut doc, "o", "c1", &a).unwrap();
    let before = snap(&doc);
    let restored = RegistryDoc::from_bytes(&doc.to_bytes().unwrap(), "dev-a").unwrap();
    assert_eq!(snap(&restored), before);
    assert_eq!(restored.pending_len(), doc.pending_len());
    // And after the pending ops are acked, again.
    let mut restored = restored;
    let mut server = HashMap::new();
    let mut seq = 0;
    server_round(&mut server, &mut seq, &mut [&mut restored]);
    let again = RegistryDoc::from_bytes(&restored.to_bytes().unwrap(), "dev-a").unwrap();
    assert_eq!(again.pending_len(), 0);
    assert_eq!(snap(&again), before);
}

#[test]
fn space_deletion_suppresses_cascades_and_sweeps_late_writes() {
    let (mut a, mut b, mut server, mut seq) = synced_pair();
    a.upsert_chat(&space_chat("c1", Some(SPACE))).unwrap();
    let p = derive_id("op-p", roles::ITEM);
    assign(&mut a, "o", "c1", &p).unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    // B, offline, keeps writing into the Space while A deletes it.
    create(&mut b, "op-late", Some(&p), "Late");
    a.delete_space(SPACE).unwrap();
    assert!(!snap(&a).available, "suppressed before the server sees it");
    assert!(snap(&a).items.is_empty());
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    for doc in [&a, &b] {
        let s = snap(doc);
        assert!(!s.available && s.items.is_empty() && s.assignments.is_empty());
    }
    // The cascade tombstoned what A knew; B's late row survives as an
    // orphan until the sweep (synced replicas only) removes it.
    assert_eq!(a.row_state(KIND_WORK_ITEMS, &p), RowState::Tombstoned);
    assert_eq!(
        a.row_state(KIND_WORK_ITEM_CHAT_ASSIGNMENTS, "c1"),
        RowState::Tombstoned
    );
    let late = derive_id("op-late", roles::ITEM);
    assert_eq!(a.row_state(KIND_WORK_ITEMS, &late), RowState::Live);
    assert_eq!(a.sweep_work_outline_orphans(), 1);
    assert_eq!(a.sweep_work_outline_orphans(), 0, "idempotent");
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    assert_eq!(b.row_state(KIND_WORK_ITEMS, &late), RowState::Tombstoned);
    // Writes into the deleted Space are refused.
    let err = apply(
        &mut b,
        WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: SPACE.into(),
            parent_work_item_id: None,
            placement: WorkOutlinePlacement::Last,
            title: "x".into(),
            description: None,
            status: None,
            operation_id: "op-dead".into(),
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::NotFound);
    // Recreating the folder mints a new Space id and adopts nothing.
    a.upsert_space(&space("sp-new", "dev-a", "/tmp/one"))
        .unwrap();
    let fresh = a.work_outline_snapshot("sp-new", true);
    assert!(fresh.available && fresh.items.is_empty());
    // The other Space was untouched.
    assert!(a.work_outline_snapshot(OTHER_SPACE, true).available);
}

#[test]
fn lenient_rows_degrade_fields_not_rows() {
    let mut doc = doc_with_spaces("dev-a");
    let mut row = item_row("w1", SPACE, None, "0000000000010-000000-x", Some("a0"));
    row.fields.insert("status".into(), json!("blocked"));
    row.fields.remove("title");
    row.fields.insert("archiveCause".into(), json!("merged"));
    row.fields.insert("archivedAt".into(), json!(5));
    row.fields.insert("futureField".into(), json!({"x": 1}));
    let mut bad = item_row("w2", SPACE, None, "0000000000010-000000-x", Some("a1"));
    bad.fields.insert("spaceId".into(), json!(42));
    let _ = doc.apply_rows(1, vec![row, bad]);
    let s = snap(&doc);
    let item = s.item("w1").unwrap();
    assert_eq!(item.status, WorkItemStatus::Pending);
    assert_eq!(item.title, "Untitled");
    assert_eq!(item.archive_cause, Some(WorkItemArchiveCause::SelfArchived));
    assert!(s.repairs.contains(&WorkOutlineRepair::UnknownStatus {
        work_item_id: "w1".into()
    }));
    assert!(s.repairs.contains(&WorkOutlineRepair::MissingTitle {
        work_item_id: "w1".into()
    }));
    assert!(s.repairs.contains(&WorkOutlineRepair::Malformed {
        row_kind: KIND_WORK_ITEMS.into(),
        id: "w2".into()
    }));
}

// ── materialization properties ──────────────────────────────────────────────

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn random_rows(rng: &mut Rng) -> Vec<RegistryRow> {
    let n = 1 + rng.below(24) as usize;
    let keys = ["a0", "a1", "a1", "a2", "Zz", "bad key", "a0V"];
    (0..n)
        .map(|i| {
            let space_id = if rng.below(6) == 0 {
                OTHER_SPACE
            } else {
                SPACE
            };
            let parent = match rng.below(10) {
                0 | 1 => None,
                2 => Some(format!("w{i}")), // self-parent
                3 => Some("missing".to_owned()),
                _ => Some(format!("w{}", rng.below(n as u64))),
            };
            let clock = encode_hlc(1 + rng.below(50) as i64, 0, "seed");
            let key = (rng.below(8) != 0).then(|| keys[rng.below(keys.len() as u64) as usize]);
            item_row(&format!("w{i}"), space_id, parent.as_deref(), &clock, key)
        })
        .collect()
}

fn inputs(rows: Vec<RegistryRow>) -> WorkOutlineInputs {
    WorkOutlineInputs {
        space_live: true,
        synced: true,
        items: rows,
        // The Space `doc_with_spaces` creates (the implicit execution root).
        execution_space: Some(zeron_proto::work_outline_execution::ExecutionSpace {
            space_id: SPACE.into(),
            path: "/tmp/one".into(),
            device_id: "dev-a".into(),
        }),
        ..Default::default()
    }
}

fn check_tree_invariants(s: &WorkOutlineSnapshot, rows: &[RegistryRow]) {
    let in_space: BTreeSet<&str> = rows
        .iter()
        .filter(|r| r.fields["spaceId"] == json!(SPACE))
        .map(|r| r.id.as_str())
        .collect();
    let listed: Vec<&str> = s.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(listed.len(), in_space.len(), "every item exactly once");
    assert_eq!(listed.iter().copied().collect::<BTreeSet<_>>(), in_space);
    for item in &s.items {
        // Acyclic: reach a root within n steps.
        let mut cur = item.parent_work_item_id.clone();
        let mut steps = 0;
        while let Some(p) = cur {
            assert!(in_space.contains(p.as_str()));
            steps += 1;
            assert!(steps <= s.items.len(), "effective cycle at {}", item.id);
            cur = s.item(&p).unwrap().parent_work_item_id.clone();
        }
        for child in &item.child_ids {
            assert_eq!(
                s.item(child).unwrap().parent_work_item_id.as_deref(),
                Some(item.id.as_str())
            );
        }
    }
    for repair in &s.repairs {
        if let WorkOutlineRepair::CycleBroken {
            victim, members, ..
        } = repair
        {
            let clock = |id: &str| {
                rows.iter()
                    .find(|r| r.id == id)
                    .and_then(|r| r.clocks.get("parentWorkItemId").cloned())
            };
            let best = members
                .iter()
                .max_by(|a, b| (clock(a), *a).cmp(&(clock(b), *b)))
                .unwrap();
            assert_eq!(best, victim);
        }
    }
}

#[test]
fn random_parent_graphs_materialize_to_a_valid_deterministic_tree() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..400 {
        let rows = random_rows(&mut rng);
        let s = materialize_work_outline(SPACE, &inputs(rows.clone()));
        check_tree_invariants(&s, &rows);
        // Determinism under input permutation.
        let mut shuffled = rows.clone();
        for i in (1..shuffled.len()).rev() {
            let j = rng.below(i as u64 + 1) as usize;
            shuffled.swap(i, j);
        }
        assert_eq!(materialize_work_outline(SPACE, &inputs(shuffled)), s);
        // Repair idempotence: persisting the derived repairs keeps the tree
        // and clears the cycle/key markers; repairing again writes nothing.
        let mut doc = doc_with_spaces("dev-a");
        let _ = doc.apply_rows(1, rows.clone());
        assert_eq!(snap(&doc), s);
        doc.repair_work_outline(&RepairWorkOutlineParams {
            space_id: SPACE.into(),
            operation_id: "op".into(),
        })
        .unwrap();
        let repaired = snap(&doc);
        for item in &s.items {
            let after = repaired.item(&item.id).unwrap();
            assert_eq!(after.parent_work_item_id, item.parent_work_item_id);
            assert_eq!(after.child_ids, item.child_ids);
        }
        assert_eq!(repaired.root_work_item_ids, s.root_work_item_ids);
        assert!(!repaired.repairs.iter().any(|r| matches!(
            r,
            WorkOutlineRepair::CycleBroken { .. } | WorkOutlineRepair::BadOrderKey { .. }
        )));
        let pending = doc.pending_len();
        doc.repair_work_outline(&RepairWorkOutlineParams {
            space_id: SPACE.into(),
            operation_id: "op2".into(),
        })
        .unwrap();
        assert_eq!(doc.pending_len(), pending);
    }
}

#[test]
fn cycle_shapes_self_two_nested_and_disjoint() {
    let c = |ms: i64| encode_hlc(ms, 0, "seed");
    let rows = vec![
        // self-parent
        item_row("s", SPACE, Some("s"), &c(5), Some("a0")),
        // 2-cycle: newest parent edge is on "u"
        item_row("t", SPACE, Some("u"), &c(3), Some("a1")),
        item_row("u", SPACE, Some("t"), &c(9), Some("a2")),
        // 3-cycle with a tail hanging into it
        item_row("x", SPACE, Some("y"), &c(4), Some("a3")),
        item_row("y", SPACE, Some("z"), &c(4), Some("a4")),
        item_row("z", SPACE, Some("x"), &c(4), Some("a5")),
        item_row("tail", SPACE, Some("y"), &c(99), Some("a6")),
        // cross-Space parent and a missing one
        item_row("o", OTHER_SPACE, None, &c(1), Some("a0")),
        item_row("k", SPACE, Some("o"), &c(1), Some("a7")),
        item_row("m", SPACE, Some("nope"), &c(1), Some("a8")),
    ];
    let s = materialize_work_outline(SPACE, &inputs(rows.clone()));
    check_tree_invariants(&s, &rows);
    let broken: Vec<(&str, usize)> = s
        .repairs
        .iter()
        .filter_map(|r| match r {
            WorkOutlineRepair::CycleBroken {
                victim, members, ..
            } => Some((victim.as_str(), members.len())),
            _ => None,
        })
        .collect();
    // Equal clocks tie-break on the greatest id.
    assert_eq!(broken, [("s", 1), ("u", 2), ("z", 3)]);
    assert!(s.repairs.contains(&WorkOutlineRepair::CrossSpaceParent {
        work_item_id: "k".into(),
        parent_work_item_id: "o".into()
    }));
    assert!(s.repairs.contains(&WorkOutlineRepair::ParentMissing {
        work_item_id: "m".into(),
        parent_work_item_id: "nope".into()
    }));
    assert_eq!(
        s.item("tail").unwrap().parent_work_item_id.as_deref(),
        Some("y")
    );
    assert_eq!(
        s.item("z").unwrap().raw_parent_work_item_id.as_deref(),
        Some("x")
    );
    assert_eq!(s.item("z").unwrap().parent_work_item_id, None);
}

#[test]
fn unavailable_space_suppresses_everything() {
    let rows = vec![item_row(
        "w",
        SPACE,
        None,
        "0000000000001-000000-x",
        Some("a0"),
    )];
    let mut input = inputs(rows);
    input.space_live = false;
    input.synced = false;
    assert_eq!(
        materialize_work_outline(SPACE, &input),
        WorkOutlineSnapshot::unavailable(SPACE, false, 0)
    );
}

// ── acceptance 30: large outline ────────────────────────────────────────────

#[test]
fn ten_thousand_items_materialize_within_budget() {
    let mut rng = Rng(42);
    let n = 10_000usize;
    let keys = order_keys_between(None, None, 64).unwrap();
    let mut ops: Vec<RowOp> = Vec::with_capacity(n + 2_000);
    for i in 0..n {
        let parent = (i > 50).then(|| format!("w{:05}", rng.below(i as u64)));
        let mut set = BTreeMap::from([
            ("spaceId".to_owned(), json!(SPACE)),
            ("title".to_owned(), json!(format!("Item {i}"))),
            ("orderKey".to_owned(), json!(keys[i % keys.len()])),
            ("parentWorkItemId".to_owned(), opt_value(parent.as_deref())),
        ]);
        set.insert("status".into(), json!("active"));
        ops.push(RowOp {
            kind: KIND_WORK_ITEMS.into(),
            id: format!("w{i:05}"),
            op: OpKind::Upsert,
            set: Some(set),
            hlc: encode_hlc(1, 0, "seed"),
            clocks: None,
        });
    }
    let mut doc = doc_with_spaces("dev-a");
    for i in 0..2_000 {
        doc.upsert_chat(&space_chat(&format!("c{i:04}"), Some(SPACE)))
            .unwrap();
        if i % 2 == 0 {
            ops.push(RowOp {
                kind: KIND_WORK_ITEM_CHAT_ASSIGNMENTS.into(),
                id: format!("c{i:04}"),
                op: OpKind::Upsert,
                set: Some(BTreeMap::from([
                    ("spaceId".to_owned(), json!(SPACE)),
                    ("workItemId".to_owned(), json!(format!("w{:05}", i * 3))),
                ])),
                hlc: encode_hlc(1, 0, "seed"),
                clocks: None,
            });
        }
    }
    // All of it still pending (offline): the overlay must stay linear.
    doc.write_ops(ops).unwrap();
    let started = std::time::Instant::now();
    let input = doc.work_outline_inputs(SPACE, true);
    let cloned = started.elapsed();
    let s = materialize_work_outline(SPACE, &input);
    let total = started.elapsed();
    assert_eq!(s.items.len(), n);
    assert_eq!(s.unassociated_chat_ids.len(), 1_000);
    let root_total: u32 = s
        .root_work_item_ids
        .iter()
        .map(|id| s.item(id).unwrap().rollup.active_count)
        .sum();
    assert_eq!(root_total as usize, n, "ancestor totals cover every item");
    // Generous for unoptimized test builds; release is far faster.
    assert!(
        total.as_secs_f64() < 5.0,
        "clone {cloned:?}, total {total:?}"
    );
    // A write against the large outline also stays bounded.
    let started = std::time::Instant::now();
    create(&mut doc, "op-big", Some("w00010"), "one more");
    assert!(
        started.elapsed().as_secs_f64() < 5.0,
        "{:?}",
        started.elapsed()
    );
}

// ── undo: the inverse operations the UI's action log sends ──────────────────

/// Undo is an ordinary mutation: the inverse of a move (`First` or `After`
/// the previous sibling, under the old parent) puts an item back exactly, a
/// field patch restores a title, and restore reverses a cascading archive.
/// See `ui::work_outline::action_log`.
#[test]
fn undo_inverse_operations_restore_order_parent_fields_and_archive() {
    let mut doc = doc_with_spaces("d1");
    let a = create(&mut doc, "op-a", None, "A");
    let b = create(&mut doc, "op-b", None, "B");
    let c = create(&mut doc, "op-c", None, "C");
    let base = titles(&snap(&doc), None);
    assert_eq!(base, ["A", "B", "C"]);

    // Reorder C to the front; the inverse is After(previous sibling = B).
    move_to(&mut doc, "op-1", &c, None, WorkOutlinePlacement::First).unwrap();
    assert_eq!(titles(&snap(&doc), None), ["C", "A", "B"]);
    move_to(&mut doc, "op-1-undo", &c, None, after(&b)).unwrap();
    assert_eq!(titles(&snap(&doc), None), base);

    // Reparent A's next sibling under A (last); the inverse names the old
    // parent and the previous sibling (A).
    move_to(&mut doc, "op-2", &b, Some(&a), WorkOutlinePlacement::Last).unwrap();
    assert_eq!(titles(&snap(&doc), Some(&a)), ["B"]);
    move_to(&mut doc, "op-2-undo", &b, None, after(&a)).unwrap();
    assert_eq!(titles(&snap(&doc), None), base);
    assert!(titles(&snap(&doc), Some(&a)).is_empty());

    // First child of a parent: the inverse placement is First.
    move_to(&mut doc, "op-3", &a, Some(&c), WorkOutlinePlacement::Last).unwrap();
    move_to(&mut doc, "op-3-undo", &a, None, WorkOutlinePlacement::First).unwrap();
    assert_eq!(titles(&snap(&doc), None), base);

    // A patch with the old values restores them, and nothing else changes.
    let before = snap(&doc).item(&b).unwrap().clone();
    for (op, title, status) in [
        ("op-4", "B2", WorkItemStatus::Completed),
        ("op-4-undo", "B", before.status),
    ] {
        apply(
            &mut doc,
            WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
                work_item_id: b.clone(),
                changes: zeron_proto::WorkItemChanges {
                    title: Some(title.into()),
                    status: Some(status),
                    ..Default::default()
                },
                operation_id: op.into(),
            }),
        )
        .unwrap();
    }
    let after_undo = snap(&doc).item(&b).unwrap().clone();
    assert_eq!(after_undo.title, before.title);
    assert_eq!(after_undo.status, before.status);

    // Archive with descendants, then restore: the subtree is live again.
    let child = create(&mut doc, "op-child", Some(&a), "A child");
    apply(
        &mut doc,
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: a.clone(),
            descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
            operation_id: "op-5".into(),
        }),
    )
    .unwrap();
    assert!(snap(&doc).item(&child).unwrap().archived_at.is_some());
    apply(
        &mut doc,
        WorkOutlineMutation::RestoreWorkItem(RestoreWorkItemParams {
            work_item_id: a.clone(),
            operation_id: "op-5-undo".into(),
        }),
    )
    .unwrap();
    let s = snap(&doc);
    assert!(s.items.iter().all(|i| i.archived_at.is_none()));
    assert_eq!(titles(&s, Some(&a)), ["A child"]);
}
