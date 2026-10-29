//! Instruction registry tests: mutators and budgets, effective resolution
//! through the materializer (acceptance 5-6), revision immutability
//! (acceptance 7-8), application selection and convergence, drift, replay,
//! integrity, orphans, Space deletion, restart, and execution-session reports.

use std::collections::HashMap;

use super::*;
use crate::registry::tests::{chat, server_round, space};
use mast_proto::HarnessId;
use mast_proto::work_outline::{AssignChatToWorkItemParams, CreateWorkItemParams};
use mast_proto::work_outline_instructions::{
    InstructionApplicationReason, InstructionSourceKind, InstructionTurnBoundary,
    SelectedRevisionIntegrity,
};

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
    doc.upsert_space(&space(SPACE, device, "/tmp/one")).unwrap();
    doc.upsert_space(&space(OTHER_SPACE, device, "/tmp/two"))
        .unwrap();
    doc
}

fn apply(doc: &mut RegistryDoc, mutation: WorkOutlineMutation) -> Outcome<WorkOutlineMutateReply> {
    let device = doc.device_id().to_owned();
    doc.apply_work_outline_mutation(&mutation, &actor(&device), NOW)
}

fn item_in(
    doc: &mut RegistryDoc,
    space_id: &str,
    op: &str,
    parent: Option<&str>,
    title: &str,
) -> String {
    apply(
        doc,
        WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: space_id.into(),
            parent_work_item_id: parent.map(str::to_owned),
            placement: WorkOutlinePlacement::Last,
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

fn item(doc: &mut RegistryDoc, op: &str, parent: Option<&str>, title: &str) -> String {
    item_in(doc, SPACE, op, parent, title)
}

fn scope_of(work_item: Option<&str>) -> InstructionScope {
    match work_item {
        None => InstructionScope::SpaceRoot,
        Some(id) => InstructionScope::WorkItem {
            work_item_id: id.into(),
        },
    }
}

fn create_params(op: &str, work_item: Option<&str>, text: &str) -> CreateWorkItemInstructionParams {
    CreateWorkItemInstructionParams {
        space_id: SPACE.into(),
        scope: scope_of(work_item),
        text: text.into(),
        placement: None,
        source: None,
        operation_id: op.into(),
    }
}

fn try_instruct(
    doc: &mut RegistryDoc,
    op: &str,
    work_item: Option<&str>,
    text: &str,
) -> Outcome<String> {
    apply(
        doc,
        WorkOutlineMutation::CreateWorkItemInstruction(create_params(op, work_item, text)),
    )
    .map(|reply| reply.instruction_id.unwrap())
}

fn instruct(doc: &mut RegistryDoc, op: &str, work_item: Option<&str>, text: &str) -> String {
    try_instruct(doc, op, work_item, text).unwrap()
}

fn edit(doc: &mut RegistryDoc, op: &str, id: &str, text: &str) -> Outcome<WorkOutlineMutateReply> {
    apply(
        doc,
        WorkOutlineMutation::UpdateWorkItemInstruction(UpdateWorkItemInstructionParams {
            instruction_id: id.into(),
            text: text.into(),
            operation_id: op.into(),
        }),
    )
}

fn archive(doc: &mut RegistryDoc, op: &str, id: &str) {
    apply(
        doc,
        WorkOutlineMutation::ArchiveWorkItemInstruction(ArchiveWorkItemInstructionParams {
            instruction_id: id.into(),
            operation_id: op.into(),
        }),
    )
    .unwrap();
}

fn move_instruction(
    doc: &mut RegistryDoc,
    op: &str,
    id: &str,
    placement: InstructionPlacement,
) -> Outcome<WorkOutlineMutateReply> {
    apply(
        doc,
        WorkOutlineMutation::MoveWorkItemInstruction(MoveWorkItemInstructionParams {
            instruction_id: id.into(),
            placement,
            operation_id: op.into(),
        }),
    )
}

fn revise(doc: &mut RegistryDoc, op: &str, work_item: &str) -> Outcome<String> {
    apply(
        doc,
        WorkOutlineMutation::CreateInstructionRevision(CreateInstructionRevisionParams {
            work_item_id: work_item.into(),
            operation_id: op.into(),
        }),
    )
    .map(|reply| reply.revision_id.unwrap())
}

fn apply_revision(
    doc: &mut RegistryDoc,
    op: &str,
    chat_id: &str,
    revision: &str,
) -> Outcome<String> {
    apply(
        doc,
        WorkOutlineMutation::ApplyInstructionRevision(ApplyInstructionRevisionParams {
            chat_id: chat_id.into(),
            revision_id: revision.into(),
            reason: InstructionApplicationReason::Explicit,
            turn_boundary: None,
            operation_id: op.into(),
        }),
    )
    .map(|reply| reply.application_id.unwrap())
}

fn space_chat(id: &str, space_id: Option<&str>) -> mast_proto::Chat {
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

fn snap(doc: &RegistryDoc) -> WorkOutlineSnapshot {
    doc.work_outline_snapshot(SPACE, true)
}

fn texts(doc: &RegistryDoc, work_item: &str) -> Vec<String> {
    snap(doc)
        .effective_instructions(work_item)
        .entries
        .into_iter()
        .map(|e| e.text)
        .collect()
}

fn scope_texts(doc: &RegistryDoc, work_item: Option<&str>) -> Vec<String> {
    snap(doc)
        .instructions
        .into_iter()
        .filter(|i| i.work_item_id.as_deref() == work_item && !i.archived())
        .map(|i| i.text)
        .collect()
}

fn set_inherit(doc: &mut RegistryDoc, op: &str, work_item: &str, inherit: bool) {
    apply(
        doc,
        WorkOutlineMutation::UpdateWorkItem(mast_proto::UpdateWorkItemParams {
            work_item_id: work_item.into(),
            changes: mast_proto::WorkItemChanges {
                inherit_parent_instructions: Some(inherit),
                ..Default::default()
            },
            operation_id: op.into(),
        }),
    )
    .unwrap();
}

fn state<'a>(snapshot: &'a WorkOutlineSnapshot, chat_id: &str) -> &'a ChatInstructionState {
    snapshot.chat_instruction_state(chat_id).unwrap()
}

/// A doc with three levels A > B > C and a Chat assigned to C.
fn three_levels() -> (RegistryDoc, String, String, String) {
    let mut doc = doc_with_spaces("dev-a");
    let a = item(&mut doc, "op-a", None, "A");
    let b = item(&mut doc, "op-b", Some(&a), "B");
    let c = item(&mut doc, "op-c", Some(&b), "C");
    doc.upsert_chat(&space_chat("chat-1", Some(SPACE))).unwrap();
    assign(&mut doc, "op-assign", "chat-1", &c);
    (doc, a, b, c)
}

use mast_proto::work_outline_instructions::ChatInstructionState;

// ── mutators ────────────────────────────────────────────────────────────────

#[test]
fn create_place_move_update_archive_within_a_scope() {
    let (mut doc, a, _b, _c) = three_levels();
    let one = instruct(&mut doc, "i1", Some(&a), "one");
    let three = instruct(&mut doc, "i3", Some(&a), "three");
    apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItemInstruction(CreateWorkItemInstructionParams {
            placement: Some(InstructionPlacement::Before {
                sibling_instruction_id: three.clone(),
            }),
            ..create_params("i2", Some(&a), "two")
        }),
    )
    .unwrap();
    apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItemInstruction(CreateWorkItemInstructionParams {
            placement: Some(InstructionPlacement::First),
            ..create_params("i0", Some(&a), "zero")
        }),
    )
    .unwrap();
    assert_eq!(scope_texts(&doc, Some(&a)), ["zero", "one", "two", "three"]);
    assert_eq!(one, derive_id("i1", roles::INSTRUCTION));

    move_instruction(&mut doc, "m1", &one, InstructionPlacement::Last).unwrap();
    assert_eq!(scope_texts(&doc, Some(&a)), ["zero", "two", "three", "one"]);
    move_instruction(
        &mut doc,
        "m2",
        &one,
        InstructionPlacement::After {
            sibling_instruction_id: derive_id("i0", roles::INSTRUCTION),
        },
    )
    .unwrap();
    assert_eq!(scope_texts(&doc, Some(&a)), ["zero", "one", "two", "three"]);
    // Already there: nothing written.
    let pending = doc.pending_len();
    move_instruction(
        &mut doc,
        "m3",
        &one,
        InstructionPlacement::Before {
            sibling_instruction_id: derive_id("i2", roles::INSTRUCTION),
        },
    )
    .unwrap();
    assert_eq!(doc.pending_len(), pending);
    assert_eq!(
        move_instruction(
            &mut doc,
            "m4",
            &one,
            InstructionPlacement::Before {
                sibling_instruction_id: one.clone(),
            },
        )
        .unwrap_err()
        .code,
        WorkOutlineErrorCode::Invalid
    );
    // A sibling from another scope is not a placement target.
    let root = instruct(&mut doc, "r1", None, "root");
    assert_eq!(
        move_instruction(
            &mut doc,
            "m5",
            &one,
            InstructionPlacement::Before {
                sibling_instruction_id: root,
            },
        )
        .unwrap_err()
        .code,
        WorkOutlineErrorCode::NotFound
    );

    edit(&mut doc, "e1", &one, "ONE").unwrap();
    let pending = doc.pending_len();
    edit(&mut doc, "e2", &one, "ONE").unwrap();
    assert_eq!(doc.pending_len(), pending, "unchanged text writes nothing");
    archive(&mut doc, "x1", &three);
    archive(&mut doc, "x2", &three);
    assert_eq!(doc.pending_len(), pending + 1, "second archive is a no-op");
    assert_eq!(scope_texts(&doc, Some(&a)), ["zero", "ONE", "two"]);
    let archived = snap(&doc)
        .instructions
        .into_iter()
        .find(|i| i.id == three)
        .unwrap();
    assert!(archived.archived() && archived.archived_by.is_some());
    assert_eq!(
        edit(&mut doc, "e3", &three, "x").unwrap_err().code,
        WorkOutlineErrorCode::Archived
    );
    // The archive pair is one op.
    let row = doc.row(KIND_WORK_ITEM_INSTRUCTIONS, &three).unwrap();
    assert_eq!(row.clocks["archivedAt"], row.clocks["archivedBy"]);
}

#[test]
fn create_validates_scope_and_keeps_provenance_immutable() {
    let (mut doc, a, _b, _c) = three_levels();
    let other = item_in(&mut doc, OTHER_SPACE, "op-o", None, "Other");
    assert_eq!(
        try_instruct(&mut doc, "x", Some(&other), "t")
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::CrossSpace
    );
    assert_eq!(
        try_instruct(&mut doc, "y", Some("nope"), "t")
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::NotFound
    );
    assert_eq!(
        try_instruct(&mut doc, "z", None, "   ").unwrap_err().code,
        WorkOutlineErrorCode::Invalid
    );
    let excerpt = "copied from the chat";
    let id = apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItemInstruction(CreateWorkItemInstructionParams {
            source: Some(InstructionSource {
                chat_id: Some("chat-1".into()),
                message_id: Some("m-1".into()),
                excerpt: Some(excerpt.into()),
                content_hash: Some("client-supplied".into()),
            }),
            ..create_params("src", Some(&a), "Use the copied rule")
        }),
    )
    .unwrap()
    .instruction_id
    .unwrap();
    let view = snap(&doc)
        .instructions
        .into_iter()
        .find(|i| i.id == id)
        .unwrap();
    let source = view.source.clone().unwrap();
    assert_eq!(source.excerpt.as_deref(), Some(excerpt));
    assert_eq!(
        source.content_hash.unwrap(),
        format!("{:x}", Sha256::digest(excerpt.as_bytes())),
        "contentHash is sha256(excerpt)"
    );
    // Effective entries drop the excerpt but keep the hash.
    let entry = snap(&doc)
        .effective_instructions(&a)
        .entries
        .into_iter()
        .find(|e| e.instruction_id == id)
        .unwrap();
    assert!(entry.provenance.source.as_ref().unwrap().excerpt.is_none());
    assert!(entry.provenance.source.unwrap().content_hash.is_some());

    // Later writes never carry immutable fields.
    edit(&mut doc, "e", &id, "Edited").unwrap();
    archive(&mut doc, "x", &id);
    let ops: Vec<&RowOp> = doc
        .pending
        .iter()
        .flat_map(|b| b.ops.iter())
        .filter(|op| op.kind == KIND_WORK_ITEM_INSTRUCTIONS && op.id == id)
        .collect();
    assert_eq!(ops[0].op, OpKind::Upsert);
    for op in &ops[1..] {
        assert_eq!(op.op, OpKind::Update);
        let set = op.set.as_ref().unwrap();
        for immutable in [
            "spaceId",
            "workItemId",
            "source",
            "createdBy",
            "createdAt",
            "inputsHash",
        ] {
            assert!(!set.contains_key(immutable), "{immutable} rewritten");
        }
    }
    // Archived work items take no new instructions.
    apply(
        &mut doc,
        WorkOutlineMutation::ArchiveWorkItem(mast_proto::ArchiveWorkItemParams {
            work_item_id: a.clone(),
            descendant_archive_policy: mast_proto::DescendantArchivePolicy::ArchiveDescendants,
            operation_id: "arch".into(),
        }),
    )
    .unwrap();
    assert_eq!(
        try_instruct(&mut doc, "late", Some(&a), "t")
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::Archived
    );
}

#[test]
fn replays_return_the_same_id_and_conflicts_are_refused() {
    let (mut doc, a, _b, c) = three_levels();
    let id = instruct(&mut doc, "i1", Some(&a), "one");
    let pending = doc.pending_len();
    assert_eq!(instruct(&mut doc, "i1", Some(&a), "one"), id);
    assert_eq!(
        try_instruct(&mut doc, "i1", Some(&a), "other")
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::OperationConflict
    );
    let revision = revise(&mut doc, "r1", &c).unwrap();
    assert_eq!(revision, derive_id("r1", roles::REVISION));
    let application = apply_revision(&mut doc, "p1", "chat-1", &revision).unwrap();
    assert_eq!(application, derive_id("p1", roles::APPLICATION));
    let pending = (pending, doc.pending_len());
    let rows = |doc: &RegistryDoc| {
        (
            doc.row(KIND_INSTRUCTION_REVISIONS, &revision).unwrap(),
            doc.row(KIND_INSTRUCTION_REVISION_APPLICATIONS, &application)
                .unwrap(),
        )
    };
    let before = rows(&doc);
    assert_eq!(revise(&mut doc, "r1", &c).unwrap(), revision);
    assert_eq!(
        apply_revision(&mut doc, "p1", "chat-1", &revision).unwrap(),
        application
    );
    assert_eq!(doc.pending_len(), pending.1, "replays write nothing");
    assert_eq!(rows(&doc), before);
    assert_eq!(
        revise(&mut doc, "r1", &derive_id("op-b", roles::ITEM))
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::OperationConflict
    );
    // Same content, new operation id: a second row with the same hash.
    let again = revise(&mut doc, "r2", &c).unwrap();
    assert_ne!(again, revision);
    assert_eq!(
        doc.instruction_revision(&again).unwrap().body.hash,
        doc.instruction_revision(&revision).unwrap().body.hash
    );
    // A deleted id is never revived.
    doc.delete_row_ops(&[(KIND_WORK_ITEM_INSTRUCTIONS, id.as_str())]);
    assert_eq!(
        try_instruct(&mut doc, "i1", Some(&a), "one")
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::NotFound
    );
}

// ── budgets ─────────────────────────────────────────────────────────────────

#[test]
fn size_count_and_effective_budgets_are_enforced_per_scope() {
    let (mut doc, a, b, c) = three_levels();
    let too_long = "x".repeat(limits::MAX_INSTRUCTION_TEXT_CHARS + 1);
    assert_eq!(
        try_instruct(&mut doc, "big", Some(&a), &too_long)
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::LimitExceeded
    );
    let excerpt = apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItemInstruction(CreateWorkItemInstructionParams {
            source: Some(InstructionSource {
                excerpt: Some("e".repeat(limits::MAX_INSTRUCTION_EXCERPT_CHARS + 1)),
                ..Default::default()
            }),
            ..create_params("ex", Some(&a), "t")
        }),
    )
    .unwrap_err();
    assert_eq!(excerpt.code, WorkOutlineErrorCode::LimitExceeded);

    // Effective characters: root 5,000 + A 3,000 leaves 1,000 for C.
    instruct(&mut doc, "r", None, &"r".repeat(5_000));
    instruct(&mut doc, "a", Some(&a), &"a".repeat(3_000));
    let fits = instruct(&mut doc, "c", Some(&c), &"c".repeat(1_000));
    let over = try_instruct(&mut doc, "c2", Some(&c), "y").unwrap_err();
    assert_eq!(over.code, WorkOutlineErrorCode::LimitExceeded);
    assert!(
        over.message.contains("\"C\""),
        "names the scope: {}",
        over.message
    );
    // A Space-root instruction that would push C over names C.
    let over = try_instruct(&mut doc, "r2", None, "y").unwrap_err();
    assert_eq!(over.code, WorkOutlineErrorCode::LimitExceeded);
    assert!(over.message.contains("\"C\""), "{}", over.message);
    // Growing an edit is checked; shrinking always passes.
    assert_eq!(
        edit(&mut doc, "e1", &fits, &"c".repeat(1_001))
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::LimitExceeded
    );
    edit(&mut doc, "e2", &fits, "short").unwrap();
    // A boundary takes A out of C's budget, so C fits again.
    archive(&mut doc, "x", &fits);
    set_inherit(&mut doc, "inh", &b, false);
    instruct(&mut doc, "c3", Some(&c), &"c".repeat(3_500));
    let effective = snap(&doc).effective_instructions(&c);
    assert!(effective.within_budget);
    assert_eq!(effective.rendered_chars, 8_500);

    // Per-scope count. The 32-entry revision cap binds first through the
    // API, so the 50-per-scope cap guards rows other writers produced.
    let rows: Vec<RegistryRow> = (0..limits::MAX_INSTRUCTIONS_PER_SCOPE)
        .map(|i| RegistryRow {
            kind: KIND_WORK_ITEM_INSTRUCTIONS.into(),
            id: format!("peer-{i}"),
            seq: 50,
            deleted: false,
            del_hlc: None,
            fields: BTreeMap::from([
                ("spaceId".to_owned(), json!(SPACE)),
                ("workItemId".to_owned(), json!(b)),
                ("text".to_owned(), json!("n")),
                ("orderKey".to_owned(), json!(format!("a{i:02}"))),
            ]),
            clocks: BTreeMap::new(),
        })
        .collect();
    let _ = doc.apply_rows(50, rows);
    let full = try_instruct(&mut doc, "n-last", Some(&b), "n").unwrap_err();
    assert_eq!(full.code, WorkOutlineErrorCode::LimitExceeded);
    assert!(
        full.message.contains("\"B\" already has"),
        "{}",
        full.message
    );
}

#[test]
fn entry_budget_counts_inherited_entries() {
    let (mut doc, a, b, c) = three_levels();
    for i in 0..16 {
        instruct(&mut doc, &format!("a{i}"), Some(&a), "a");
        instruct(&mut doc, &format!("b{i}"), Some(&b), "b");
    }
    let over = try_instruct(&mut doc, "c", Some(&c), "c").unwrap_err();
    assert_eq!(over.code, WorkOutlineErrorCode::LimitExceeded);
    // Many entries reach the body-size cap below 9,000 characters, and the
    // error says why.
    let mut many = doc_with_spaces("dev-a");
    let text = "s".repeat(290);
    let mut created = 0;
    let error = loop {
        match try_instruct(&mut many, &format!("s{created}"), None, &text) {
            Ok(_) => created += 1,
            Err(error) => break error,
        }
    };
    assert!(created * 290 < limits::MAX_EFFECTIVE_INSTRUCTION_CHARS);
    assert_eq!(error.code, WorkOutlineErrorCode::LimitExceeded);
    assert!(
        error.message.contains("revision size limit")
            && error.message.contains("each instruction adds"),
        "{}",
        error.message
    );
    // The Space root alone is budgeted too (no item needed).
    let mut empty = doc_with_spaces("dev-a");
    instruct(&mut empty, "r1", None, &"r".repeat(6_000));
    assert_eq!(
        try_instruct(&mut empty, "r2", None, &"r".repeat(3_001))
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::LimitExceeded
    );
}

// ── acceptance 5-6: effective resolution through the materializer ───────────

#[test]
fn effective_instructions_compose_root_to_child_with_boundaries() {
    let (mut doc, a, b, c) = three_levels();
    instruct(&mut doc, "r", None, "root");
    instruct(&mut doc, "a", Some(&a), "from A");
    instruct(&mut doc, "b", Some(&b), "from B");
    instruct(&mut doc, "c", Some(&c), "from C");
    let archived = instruct(&mut doc, "c-old", Some(&c), "archived C");
    archive(&mut doc, "x", &archived);

    let effective = snap(&doc).effective_instructions(&c);
    let entries: Vec<_> = effective
        .entries
        .iter()
        .map(|e| {
            (
                e.text.as_str(),
                e.scope_title.as_str(),
                e.inherited,
                e.source_kind,
            )
        })
        .collect();
    assert_eq!(
        entries,
        [
            (
                "root",
                SPACE_ROOT_SCOPE_TITLE,
                true,
                InstructionSourceKind::SpaceRoot
            ),
            ("from A", "A", true, InstructionSourceKind::WorkItem),
            ("from B", "B", true, InstructionSourceKind::WorkItem),
            ("from C", "C", false, InstructionSourceKind::WorkItem),
        ]
    );
    assert_eq!(effective.entries[0].scope_id, SPACE);
    assert!(
        effective
            .entries
            .iter()
            .all(|e| e.provenance.created_by.is_some())
    );
    assert_eq!(effective.boundary_work_item_id, None);

    // B opts out: A is dropped for B and C; the Space root always stays.
    set_inherit(&mut doc, "inh-b", &b, false);
    assert_eq!(texts(&doc, &c), ["root", "from B", "from C"]);
    assert_eq!(texts(&doc, &b), ["root", "from B"]);
    assert_eq!(texts(&doc, &a), ["root", "from A"]);
    assert_eq!(
        snap(&doc).effective_instructions(&c).boundary_work_item_id,
        Some(b.clone())
    );
    // C opts out too: a new boundary at C.
    set_inherit(&mut doc, "inh-c", &c, false);
    assert_eq!(texts(&doc, &c), ["root", "from C"]);
    set_inherit(&mut doc, "inh-b2", &b, true);
    set_inherit(&mut doc, "inh-c2", &c, true);
    assert_eq!(texts(&doc, &c), ["root", "from A", "from B", "from C"]);
    // The snapshot lists archived instructions, flagged, Space root first.
    let s = snap(&doc);
    assert_eq!(s.instructions.len(), 5);
    assert_eq!(s.instructions[0].work_item_id, None);
    assert!(s.instructions.iter().any(|i| i.archived()));
}

// ── acceptance 7-8: revisions are immutable ─────────────────────────────────

#[test]
fn edits_and_archives_never_change_an_existing_revision() {
    let (mut doc, a, b, c) = three_levels();
    let ia = instruct(&mut doc, "a", Some(&a), "from A");
    let ib = instruct(&mut doc, "b", Some(&b), "from B");
    instruct(&mut doc, "r", None, "root");
    let revision = revise(&mut doc, "rev", &c).unwrap();
    apply_revision(&mut doc, "app", "chat-1", &revision).unwrap();
    let view = doc.instruction_revision(&revision).unwrap();
    assert_eq!(view.integrity, InstructionRevisionIntegrity::Ok);
    assert_eq!(view.work_item_id, c);
    let ids: Vec<_> = view
        .body
        .entries
        .iter()
        .map(|e| e.instruction_id.clone())
        .collect();
    assert_eq!(ids[1..], [ia.clone(), ib.clone()]);
    let s = snap(&doc);
    assert!(!state(&s, "chat-1").drift);
    let row_before = doc.row(KIND_INSTRUCTION_REVISIONS, &revision).unwrap();

    edit(&mut doc, "e", &ia, "changed A").unwrap();
    archive(&mut doc, "x", &ib);
    let row_after = doc.row(KIND_INSTRUCTION_REVISIONS, &revision).unwrap();
    assert_eq!(row_after, row_before, "the revision row is untouched");
    let view_after = doc.instruction_revision(&revision).unwrap();
    assert_eq!(view_after, view);
    assert_eq!(view_after.body.entries[1].text, "from A");

    let s = snap(&doc);
    let chat = state(&s, "chat-1");
    assert!(chat.drift, "the Chat sees changed instructions");
    assert_eq!(
        chat.selected_revision_id.as_deref(),
        Some(revision.as_str())
    );
    assert_eq!(
        s.revision(&revision).unwrap().integrity,
        InstructionRevisionIntegrity::Ok
    );

    // The newer revision differs and can be applied explicitly.
    let newer = revise(&mut doc, "rev2", &c).unwrap();
    let comparison = mast_proto::compare_instruction_revisions(
        &doc.instruction_revision(&revision).unwrap().body,
        &doc.instruction_revision(&newer).unwrap().body,
    )
    .unwrap();
    assert_eq!(comparison.changed.len(), 1);
    assert_eq!(comparison.removed.len(), 1);
    apply_revision(&mut doc, "app2", "chat-1", &newer).unwrap();
    let s = snap(&doc);
    assert!(!state(&s, "chat-1").drift);
    assert_eq!(
        state(&s, "chat-1").selected_revision_id.as_deref(),
        Some(newer.as_str())
    );
}

// ── selection and drift ─────────────────────────────────────────────────────

#[test]
fn drift_follows_selection_and_current_effective_hash() {
    let (mut doc, a, _b, c) = three_levels();
    // Assigned, nothing selected, nothing effective: no drift.
    let s = snap(&doc);
    let chat = state(&s, "chat-1");
    assert!(!chat.drift && chat.selected_revision_id.is_none());
    assert!(chat.current_effective_hash.is_some());
    // Something effective but nothing selected: drift.
    let from_a = instruct(&mut doc, "a", Some(&a), "from A");
    let s = snap(&doc);
    assert!(state(&s, "chat-1").drift);
    let revision = revise(&mut doc, "rev", &c).unwrap();
    apply_revision(&mut doc, "app", "chat-1", &revision).unwrap();
    let s = snap(&doc);
    let chat = state(&s, "chat-1");
    assert!(!chat.drift);
    assert_eq!(chat.selected_revision_hash, chat.current_effective_hash);
    assert_eq!(
        doc.selected_instruction_revision("chat-1"),
        Some(InstructionRevisionPin {
            revision_id: revision.clone(),
            revision_hash: chat.selected_revision_hash.clone().unwrap(),
        })
    );
    // Renaming an inherited item changes the snapshotted scope title (so
    // the hash a new revision would carry), but it is not drift: drift
    // compares instruction content only.
    apply(
        &mut doc,
        WorkOutlineMutation::UpdateWorkItem(mast_proto::UpdateWorkItemParams {
            work_item_id: a.clone(),
            changes: mast_proto::WorkItemChanges {
                title: Some("A renamed".into()),
                ..Default::default()
            },
            operation_id: "rename".into(),
        }),
    )
    .unwrap();
    let s = snap(&doc);
    let renamed = state(&s, "chat-1");
    assert!(!renamed.drift);
    assert_ne!(
        renamed.selected_revision_hash,
        renamed.current_effective_hash
    );
    // Editing the inherited instruction's text is drift.
    edit(&mut doc, "edit-a", &from_a, "from A, edited").unwrap();
    assert!(state(&snap(&doc), "chat-1").drift);
    // Re-selecting the current instructions clears it again.
    let edited = revise(&mut doc, "rev-edited", &c).unwrap();
    apply_revision(&mut doc, "app-edited", "chat-1", &edited).unwrap();
    assert!(!state(&snap(&doc), "chat-1").drift);
    // Unassigned Chats keep their selection but never drift.
    apply(
        &mut doc,
        WorkOutlineMutation::RemoveChatFromWorkItem(mast_proto::RemoveChatFromWorkItemParams {
            chat_id: "chat-1".into(),
            operation_id: "unassign".into(),
        }),
    )
    .unwrap();
    let s = snap(&doc);
    let chat = state(&s, "chat-1");
    assert!(!chat.drift && chat.current_effective_hash.is_none());
    assert_eq!(chat.selected_revision_id.as_deref(), Some(edited.as_str()));
    // Applying needs an assignment.
    assert_eq!(
        apply_revision(&mut doc, "app3", "chat-1", &revision)
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::Invalid
    );
}

#[test]
fn apply_validates_chat_space_and_revision() {
    let (mut doc, _a, _b, c) = three_levels();
    let revision = revise(&mut doc, "rev", &c).unwrap();
    doc.upsert_chat(&space_chat("np", None)).unwrap();
    doc.upsert_chat(&space_chat("other", Some(OTHER_SPACE)))
        .unwrap();
    let o = item_in(&mut doc, OTHER_SPACE, "op-o", None, "O");
    assign(&mut doc, "as-o", "other", &o);
    assert_eq!(
        apply_revision(&mut doc, "x1", "np", &revision)
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::Projectless
    );
    assert_eq!(
        apply_revision(&mut doc, "x2", "other", &revision)
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::CrossSpace
    );
    assert_eq!(
        apply_revision(&mut doc, "x3", "chat-1", "missing")
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::NotFound
    );
    assert_eq!(
        apply_revision(&mut doc, "x4", "ghost", &revision)
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::NotFound
    );
    // The application row: reason, boundary, own HLC.
    let id = apply(
        &mut doc,
        WorkOutlineMutation::ApplyInstructionRevision(ApplyInstructionRevisionParams {
            chat_id: "chat-1".into(),
            revision_id: revision.clone(),
            reason: InstructionApplicationReason::ForkInherit,
            turn_boundary: Some(InstructionTurnBoundary {
                message_id: Some("msg-1".into()),
            }),
            operation_id: "fork".into(),
        }),
    )
    .unwrap()
    .application_id
    .unwrap();
    let row = doc
        .row(KIND_INSTRUCTION_REVISION_APPLICATIONS, &id)
        .unwrap();
    assert_eq!(row.fields["appliedHlc"], json!(row.clocks["appliedHlc"]));
    assert_eq!(row.fields["reason"], json!("forkInherit"));
    assert_eq!(row.fields["turnBoundary"], json!({ "messageId": "msg-1" }));
    assert_eq!(row.fields["result"], json!("accepted"));
    assert_eq!(row.fields["workItemId"], json!(c));
    assert_eq!(row.fields["idempotencyKey"], json!("fork"));
}

#[test]
fn concurrent_applications_on_two_replicas_converge_on_the_latest() {
    let (mut a, _ia, _ib, c) = three_levels();
    instruct(&mut a, "i", Some(&c), "first");
    let r1 = revise(&mut a, "rev1", &c).unwrap();
    let mut b = RegistryDoc::new("dev-b");
    let mut server = HashMap::new();
    let mut seq = 0;
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    instruct(&mut b, "i2", Some(&c), "second");
    let r2 = revise(&mut b, "rev2", &c).unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    // Both apply offline, B's clock a little ahead.
    b.clock.last_ms = Utc::now().timestamp_millis() + 60_000;
    let app_a = apply_revision(&mut a, "app-a", "chat-1", &r1).unwrap();
    let app_b = apply_revision(&mut b, "app-b", "chat-1", &r2).unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    let (sa, sb) = (snap(&a), snap(&b));
    assert_eq!(sa, sb, "both replicas materialize the same selection");
    let hlc = |doc: &RegistryDoc, id: &str| {
        doc.row(KIND_INSTRUCTION_REVISION_APPLICATIONS, id)
            .unwrap()
            .fields["appliedHlc"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert!(hlc(&a, &app_b) > hlc(&a, &app_a));
    assert_eq!(
        state(&sa, "chat-1").selected_revision_id.as_deref(),
        Some(r2.as_str())
    );
    assert_eq!(
        a.selected_instruction_revision("chat-1"),
        b.selected_instruction_revision("chat-1")
    );
    // A applies again after seeing B's (faster-clock) application: its
    // application observes B's clock and wins.
    let app_c = apply_revision(&mut a, "app-c", "chat-1", &r1).unwrap();
    assert!(hlc(&a, &app_c) > hlc(&a, &app_b));
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    assert_eq!(snap(&a), snap(&b));
    assert_eq!(
        state(&snap(&b), "chat-1").selected_revision_id.as_deref(),
        Some(r1.as_str())
    );
}

// ── integrity, orphans ──────────────────────────────────────────────────────

fn tamper(doc: &mut RegistryDoc, revision: &str, edit: impl FnOnce(&mut Value)) {
    let mut row = doc.row(KIND_INSTRUCTION_REVISIONS, revision).unwrap();
    edit(row.fields.get_mut("body").unwrap());
    row.seq = 1_000;
    let _ = doc.apply_rows(1_000, vec![row]);
}

#[test]
fn corrupt_selected_revision_is_reported_never_substituted() {
    let (mut doc, a, _b, c) = three_levels();
    instruct(&mut doc, "i", Some(&a), "keep secrets out of logs");
    let good = revise(&mut doc, "good", &c).unwrap();
    apply_revision(&mut doc, "app1", "chat-1", &good).unwrap();
    instruct(&mut doc, "i2", Some(&c), "more");
    let bad = revise(&mut doc, "bad", &c).unwrap();
    let bad_hash = doc.instruction_revision(&bad).unwrap().body.hash;
    apply_revision(&mut doc, "app2", "chat-1", &bad).unwrap();
    let mut server = HashMap::new();
    let mut seq = 0;
    server_round(&mut server, &mut seq, &mut [&mut doc]);
    assert_eq!(
        state(&snap(&doc), "chat-1").selected_revision_id.as_deref(),
        Some(bad.as_str())
    );
    tamper(&mut doc, &bad, |body| {
        body["entries"][0]["text"] = json!("log every secret");
    });
    assert_eq!(
        doc.instruction_revision(&bad).unwrap().integrity,
        InstructionRevisionIntegrity::Corrupt
    );
    let s = snap(&doc);
    assert!(s.repairs.contains(&WorkOutlineRepair::CorruptRevision {
        revision_id: bad.clone()
    }));
    let chat = state(&s, "chat-1");
    assert_eq!(
        chat.selected_revision_id.as_deref(),
        Some(bad.as_str()),
        "never falls back to an older revision (contract 5.3)"
    );
    assert_eq!(
        chat.selected_revision_integrity,
        Some(SelectedRevisionIntegrity::Corrupt)
    );
    // The pin carries the hash verified when it was applied, so delivery
    // re-verifies the tampered row against it and fails closed.
    let pin = doc.selected_instruction_revision("chat-1").unwrap();
    assert_eq!(pin.revision_id, bad);
    assert_eq!(
        Some(&pin.revision_hash),
        chat.selected_revision_hash.as_ref()
    );
    assert_eq!(pin.revision_hash, bad_hash);
    let listed = s.revision(&bad).unwrap();
    assert_eq!(listed.integrity, InstructionRevisionIntegrity::Corrupt);
    assert!(
        s.revision(&good).is_none(),
        "an older application is history"
    );
    assert_eq!(
        apply_revision(&mut doc, "app3", "chat-1", &bad)
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::RevisionIntegrity
    );
    // Re-applying the intact revision selects it again.
    apply_revision(&mut doc, "app4", "chat-1", &good).unwrap();
    let s = snap(&doc);
    assert_eq!(
        state(&s, "chat-1").selected_revision_integrity,
        Some(SelectedRevisionIntegrity::Ok)
    );
    // An undecodable body is corrupt too, and still selected.
    tamper(&mut doc, &good, |body| *body = json!({ "entries": "nope" }));
    assert_eq!(
        doc.instruction_revision(&good).unwrap().integrity,
        InstructionRevisionIntegrity::Corrupt
    );
    let s = snap(&doc);
    let chat = state(&s, "chat-1");
    assert_eq!(chat.selected_revision_id.as_deref(), Some(good.as_str()));
    assert_eq!(
        chat.selected_revision_integrity,
        Some(SelectedRevisionIntegrity::Corrupt)
    );
}

#[test]
fn snapshot_revisions_stay_bounded_to_current_selection_and_delivery() {
    let (mut doc, _a, _b, c) = three_levels();
    doc.upsert_chat(&space_chat("chat-2", Some(SPACE))).unwrap();
    assign(&mut doc, "op-assign-2", "chat-2", &c);
    let mut revisions = Vec::new();
    for i in 0..5 {
        instruct(&mut doc, &format!("i{i}"), Some(&c), &format!("rule {i}"));
        let revision = revise(&mut doc, &format!("rev{i}"), &c).unwrap();
        apply_revision(&mut doc, &format!("app{i}"), "chat-1", &revision).unwrap();
        revisions.push(revision);
    }
    let inputs = doc.work_outline_inputs(SPACE, true);
    assert_eq!(inputs.applications.len(), 1, "only the newest application");
    assert_eq!(inputs.revisions.len(), 1);
    let s = snap(&doc);
    let listed: Vec<&str> = s.revisions.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(listed, [revisions[4].as_str()]);
    // A delivered revision and another Chat's selection are listed too.
    doc.report_execution_session("chat-1", &report(Some(&revisions[0]), "h"), NOW)
        .unwrap();
    apply_revision(&mut doc, "app-2", "chat-2", &revisions[2]).unwrap();
    let s = snap(&doc);
    let mut expected = vec![
        revisions[0].as_str(),
        revisions[2].as_str(),
        revisions[4].as_str(),
    ];
    expected.sort();
    let listed: Vec<&str> = s.revisions.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(listed, expected);
    assert_eq!(doc.work_outline_inputs(SPACE, true).revisions.len(), 3);
}

#[test]
fn selection_waits_for_an_unsynced_revision_with_its_recorded_hash() {
    let (mut a, _x, _y, c) = three_levels();
    instruct(&mut a, "i", Some(&c), "rule");
    let revision = revise(&mut a, "rev", &c).unwrap();
    let hash = a.instruction_revision(&revision).unwrap().body.hash;
    apply_revision(&mut a, "app", "chat-1", &revision).unwrap();
    let mut server = HashMap::new();
    let mut seq = 0;
    server_round(&mut server, &mut seq, &mut [&mut a]);
    // Another replica that has the application but not (yet) the revision.
    let mut b = RegistryDoc::new("dev-b");
    let rows: Vec<RegistryRow> = server
        .values()
        .filter(|row| row.kind != KIND_INSTRUCTION_REVISIONS)
        .cloned()
        .collect();
    let _ = b.apply_rows(seq, rows);
    let s = snap(&b);
    let chat = state(&s, "chat-1");
    assert_eq!(
        chat.selected_revision_id.as_deref(),
        Some(revision.as_str())
    );
    assert_eq!(chat.selected_revision_hash.as_deref(), Some(hash.as_str()));
    assert_eq!(
        chat.selected_revision_integrity,
        Some(SelectedRevisionIntegrity::Missing)
    );
    assert_eq!(
        b.selected_instruction_revision("chat-1")
            .unwrap()
            .revision_hash,
        hash
    );
    assert!(s.revisions.is_empty(), "not synced, so not listed");
}

#[test]
fn orphan_instructions_are_listed_flagged_and_never_effective() {
    let (mut doc, a, _b, c) = three_levels();
    let other = item_in(&mut doc, OTHER_SPACE, "op-o", None, "O");
    instruct(&mut doc, "keep", Some(&a), "kept");
    let orphan = |id: &str, work_item: &str| RegistryRow {
        kind: KIND_WORK_ITEM_INSTRUCTIONS.into(),
        id: id.into(),
        seq: 99,
        deleted: false,
        del_hlc: None,
        fields: BTreeMap::from([
            ("spaceId".to_owned(), json!(SPACE)),
            ("workItemId".to_owned(), json!(work_item)),
            ("text".to_owned(), json!("orphan")),
            ("orderKey".to_owned(), json!("a0")),
        ]),
        clocks: BTreeMap::new(),
    };
    let _ = doc.apply_rows(
        99,
        vec![orphan("gone-item", "w-missing"), orphan("cross", &other)],
    );
    let s = snap(&doc);
    for (instruction_id, work_item_id) in [("gone-item", "w-missing"), ("cross", other.as_str())] {
        assert!(s.repairs.contains(&WorkOutlineRepair::OrphanInstruction {
            instruction_id: instruction_id.into(),
            work_item_id: work_item_id.into(),
        }));
        assert!(s.instructions.iter().any(|i| i.id == instruction_id));
    }
    assert_eq!(texts(&doc, &c), ["kept"]);
    assert_eq!(
        doc.work_outline_snapshot(OTHER_SPACE, true)
            .effective_instructions(&other)
            .entries
            .len(),
        0,
        "a row of another Space naming the item is never effective"
    );
}

// ── acceptance 29: Space deletion, restart ──────────────────────────────────

#[test]
fn space_deletion_cascades_and_sweeps_instruction_kinds() {
    let (mut doc, a, _b, c) = three_levels();
    let instruction = instruct(&mut doc, "i", Some(&a), "rule");
    let root = instruct(&mut doc, "r", None, "root");
    let revision = revise(&mut doc, "rev", &c).unwrap();
    let application = apply_revision(&mut doc, "app", "chat-1", &revision).unwrap();
    let mut late = RegistryDoc::new("dev-b");
    let mut server = HashMap::new();
    let mut seq = 0;
    server_round(&mut server, &mut seq, &mut [&mut doc, &mut late]);
    // dev-b, offline, adds a root instruction while dev-a deletes the Space.
    let late_id = instruct(&mut late, "late", None, "late");
    doc.delete_space(SPACE).unwrap();
    for (kind, id) in [
        (KIND_WORK_ITEM_INSTRUCTIONS, &instruction),
        (KIND_WORK_ITEM_INSTRUCTIONS, &root),
        (KIND_INSTRUCTION_REVISIONS, &revision),
        (KIND_INSTRUCTION_REVISION_APPLICATIONS, &application),
    ] {
        assert_eq!(doc.row_state(kind, id), RowState::Tombstoned, "{kind}");
    }
    server_round(&mut server, &mut seq, &mut [&mut doc, &mut late]);
    assert!(!snap(&doc).available && snap(&doc).instructions.is_empty());
    assert_eq!(
        doc.row_state(KIND_WORK_ITEM_INSTRUCTIONS, &late_id),
        RowState::Live
    );
    assert!(
        doc.work_outline_orphans()
            .iter()
            .any(|(_, id)| *id == late_id)
    );
    assert_eq!(doc.sweep_work_outline_orphans(), 1);
    assert_eq!(
        doc.row_state(KIND_WORK_ITEM_INSTRUCTIONS, &late_id),
        RowState::Tombstoned
    );
    // Writes into the deleted Space are refused.
    assert_eq!(
        try_instruct(&mut late, "after", None, "x")
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::NotFound
    );
}

#[test]
fn restart_round_trip_preserves_instruction_state() {
    let (mut doc, a, _b, c) = three_levels();
    instruct(&mut doc, "i", Some(&a), "rule");
    let revision = revise(&mut doc, "rev", &c).unwrap();
    apply_revision(&mut doc, "app", "chat-1", &revision).unwrap();
    doc.report_execution_session("chat-1", &report(Some(&revision), "h"), NOW)
        .unwrap();
    let before = snap(&doc);
    assert_eq!(before.revisions.len(), 1);
    let restored = RegistryDoc::from_bytes(&doc.to_bytes().unwrap(), "dev-a").unwrap();
    assert_eq!(snap(&restored), before);
    let mut restored = restored;
    let mut server = HashMap::new();
    let mut seq = 0;
    server_round(&mut server, &mut seq, &mut [&mut restored]);
    let again = RegistryDoc::from_bytes(&restored.to_bytes().unwrap(), "dev-a").unwrap();
    assert_eq!(again.pending_len(), 0);
    assert_eq!(snap(&again), before);
}

// ── execution-session reports ───────────────────────────────────────────────

fn report(revision: Option<&str>, hash: &str) -> ExecutionSessionReport {
    ExecutionSessionReport {
        id: "es-1".into(),
        native_resume_id: Some("native-1".into()),
        harness: HarnessId::Mock,
        execution_root: "/tmp/one".into(),
        revision_id: revision.map(str::to_owned),
        revision_hash: revision.map(|_| hash.to_owned()),
    }
}

#[test]
fn execution_session_reports_are_idempotent_and_surface_as_delivery() {
    let (mut doc, a, _b, c) = three_levels();
    instruct(&mut doc, "i", Some(&a), "rule");
    let revision = revise(&mut doc, "rev", &c).unwrap();
    let hash = doc.instruction_revision(&revision).unwrap().body.hash;
    // No status row yet: one is created as idle and still reads as a session.
    assert!(
        doc.report_execution_session("chat-1", &report(Some(&revision), &hash), NOW)
            .unwrap()
    );
    let sessions = doc.read_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].status, mast_proto::SessionStatus::Idle);
    let pending = doc.pending_len();
    assert!(
        !doc.report_execution_session("chat-1", &report(Some(&revision), &hash), NOW)
            .unwrap()
    );
    assert_eq!(doc.pending_len(), pending, "an equal report writes nothing");

    let s = snap(&doc);
    let chat = state(&s, "chat-1");
    assert_eq!(
        chat.delivered_revision_id.as_deref(),
        Some(revision.as_str())
    );
    assert_eq!(chat.delivered_revision_hash.as_deref(), Some(hash.as_str()));
    assert!(
        chat.selected_revision_id.is_none(),
        "delivery is not selection"
    );
    assert_eq!(s.revisions.len(), 1, "the delivered revision is listed");

    // A status write keeps the report (different fields).
    let mut session = sessions[0].clone();
    session.status = mast_proto::SessionStatus::Working;
    doc.upsert_session(&session).unwrap();
    let row = doc.row(KIND_SESSIONS, "chat-1").unwrap();
    assert_eq!(row.fields["status"], json!("working"));
    assert!(row.fields.contains_key("executionSession"));
    // A changed report is an update that observes the row's clocks.
    doc.clock.last_ms = 0;
    assert!(
        doc.report_execution_session("chat-1", &report(None, ""), NOW)
            .unwrap()
    );
    let after = doc.row(KIND_SESSIONS, "chat-1").unwrap();
    assert!(after.clocks["executionSession"] > row.clocks["status"]);
    assert!(state(&snap(&doc), "chat-1").delivered_revision_id.is_none());
    // A deleted Chat's row is never revived.
    doc.delete_chat("chat-1").unwrap();
    assert!(
        !doc.report_execution_session("chat-1", &report(Some(&revision), &hash), NOW)
            .unwrap()
    );
    assert_eq!(doc.row_state(KIND_SESSIONS, "chat-1"), RowState::Tombstoned);
}

// ── keepInstructionSession ──────────────────────────────────────────────────

fn keep(doc: &mut RegistryDoc, op: &str, chat_id: &str) -> Outcome<String> {
    apply(
        doc,
        WorkOutlineMutation::KeepInstructionSession(KeepInstructionSessionParams {
            chat_id: chat_id.into(),
            operation_id: op.into(),
        }),
    )
    .map(|reply| reply.application_id.unwrap())
}

#[test]
fn keep_session_selects_the_reported_revision_and_a_later_apply_wins() {
    let (mut doc, a, _b, c) = three_levels();
    instruct(&mut doc, "i", Some(&a), "rule");
    let old = revise(&mut doc, "rev-old", &c).unwrap();
    let old_hash = doc.instruction_revision(&old).unwrap().body.hash;
    apply_revision(&mut doc, "app-old", "chat-1", &old).unwrap();
    doc.report_execution_session("chat-1", &report(Some(&old), &old_hash), NOW)
        .unwrap();
    instruct(&mut doc, "i2", Some(&a), "second rule");
    let new = revise(&mut doc, "rev-new", &c).unwrap();
    apply_revision(&mut doc, "app-new", "chat-1", &new).unwrap();
    assert_eq!(
        doc.selected_instruction_revision("chat-1")
            .unwrap()
            .revision_id,
        new
    );

    // Keep: the selection reverts to what the session received.
    let id = keep(&mut doc, "keep-1", "chat-1").unwrap();
    assert_eq!(id, derive_id("keep-1", roles::APPLICATION));
    let pin = doc.selected_instruction_revision("chat-1").unwrap();
    assert_eq!(pin.revision_id, old);
    assert_eq!(pin.revision_hash, old_hash);
    let row = doc
        .row(KIND_INSTRUCTION_REVISION_APPLICATIONS, &id)
        .unwrap();
    assert_eq!(row.fields["reason"], json!("keepSession"));
    let s = snap(&doc);
    let chat = state(&s, "chat-1");
    assert_eq!(chat.selected_revision_id.as_deref(), Some(old.as_str()));
    assert!(chat.drift, "the newer instructions still show as drift");

    // Idempotent replay: same id, nothing written, even after the report changed.
    doc.report_execution_session("chat-1", &report(None, ""), NOW)
        .unwrap();
    let pending = doc.pending_len();
    assert_eq!(keep(&mut doc, "keep-1", "chat-1").unwrap(), id);
    assert_eq!(doc.pending_len(), pending);
    assert_eq!(
        doc.selected_instruction_revision("chat-1")
            .unwrap()
            .revision_id,
        old
    );

    // A later Apply sorts after the keep and wins.
    apply_revision(&mut doc, "app-again", "chat-1", &new).unwrap();
    assert_eq!(
        doc.selected_instruction_revision("chat-1")
            .unwrap()
            .revision_id,
        new
    );
}

#[test]
fn keep_session_without_instructions_selects_none() {
    let (mut doc, a, _b, c) = three_levels();
    instruct(&mut doc, "i", Some(&a), "rule");
    let revision = revise(&mut doc, "rev", &c).unwrap();
    apply_revision(&mut doc, "app", "chat-1", &revision).unwrap();
    // No Execution Session report (the session started before any selection).
    let id = keep(&mut doc, "keep-none", "chat-1").unwrap();
    assert!(doc.selected_instruction_revision("chat-1").is_none());
    let row = doc
        .row(KIND_INSTRUCTION_REVISION_APPLICATIONS, &id)
        .unwrap();
    assert_eq!(row.fields["revisionId"], json!(""));
    assert!(!row.fields.contains_key("revisionHash"));
    let s = snap(&doc);
    let chat = state(&s, "chat-1");
    assert!(chat.selected_revision_id.is_none());
    assert!(chat.selected_revision_integrity.is_none());
    assert!(chat.drift, "effective instructions with no selection drift");
    // The engine's own delivery record wins over the registry report.
    let kept = doc
        .keep_instruction_session(
            &KeepInstructionSessionParams {
                chat_id: "chat-1".into(),
                operation_id: "keep-host".into(),
            },
            Some(Some(InstructionRevisionPin {
                revision_id: revision.clone(),
                revision_hash: String::new(),
            })),
            &actor("dev-a"),
            NOW,
        )
        .unwrap();
    assert_eq!(kept, derive_id("keep-host", roles::APPLICATION));
    assert_eq!(
        doc.selected_instruction_revision("chat-1")
            .unwrap()
            .revision_id,
        revision
    );
    // An empty revision id is still dropped for any other reason.
    let mut fields = doc
        .row(KIND_INSTRUCTION_REVISION_APPLICATIONS, &id)
        .unwrap()
        .fields;
    fields.insert("reason".into(), json!("explicit"));
    let row = RegistryRow {
        id: "x".into(),
        fields,
        ..doc
            .row(KIND_INSTRUCTION_REVISION_APPLICATIONS, &id)
            .unwrap()
    };
    assert!(super::super::materialize::decode_application(&row).is_none());
    // Projectless and unknown Chats are refused.
    assert_eq!(
        keep(&mut doc, "keep-ghost", "ghost").unwrap_err().code,
        WorkOutlineErrorCode::NotFound
    );
}
