use super::*;
use crate::work_outline::store::{fixture_ids::*, fixture_snapshot};
use zeron_proto::WorkItemInstructionView;

fn snap() -> WorkOutlineSnapshot {
    fixture_snapshot("s1")
}

fn instruction(id: &str, item: Option<&str>, text: &str) -> WorkItemInstructionView {
    WorkItemInstructionView {
        id: id.into(),
        space_id: "s1".into(),
        work_item_id: item.map(str::to_owned),
        text: text.into(),
        order_key: id.into(),
        created_by: None,
        created_at: 0,
        updated_at: 0,
        source: None,
        source_proposal_id: None,
        archived_at: None,
        archived_by: None,
    }
}

/// Apply `step` to a snapshot the way the engine would, for the steps these
/// tests round-trip.
fn apply(snapshot: &mut WorkOutlineSnapshot, step: &Step) {
    match step {
        Step::Update {
            work_item_id,
            changes,
        } => {
            let item = snapshot
                .items
                .iter_mut()
                .find(|item| &item.id == work_item_id)
                .unwrap();
            if let Some(title) = &changes.title {
                item.title = title.clone();
            }
            if let Some(status) = changes.status {
                item.status = status;
            }
            match &changes.description {
                FieldPatch::Unchanged => {}
                FieldPatch::Clear => item.description = None,
                FieldPatch::Set(text) => item.description = Some(text.clone()),
            }
        }
        Step::Archive { work_item_id } | Step::Restore { work_item_id } => {
            let archive = matches!(step, Step::Archive { .. });
            let item = snapshot
                .items
                .iter_mut()
                .find(|item| &item.id == work_item_id)
                .unwrap();
            item.archived_at = archive.then_some(1);
        }
        Step::UpdateInstruction {
            instruction_id,
            text,
        } => {
            snapshot
                .instructions
                .iter_mut()
                .find(|i| &i.id == instruction_id)
                .unwrap()
                .text = text.clone();
        }
        other => panic!("not modelled: {other:?}"),
    }
}

#[test]
fn move_entry_inverse_puts_the_item_back_between_its_old_neighbours() {
    let snapshot = snap();
    // UI is after API under LAUNCH; move it under API as the last child.
    let entry = move_entry(&snapshot, UI, Some(API), &WorkOutlinePlacement::Last, None).unwrap();
    assert_eq!(entry.label, "moved \u{2018}UI\u{2019} under \u{2018}API\u{2019}");
    assert_eq!(
        entry.inverse,
        Step::Move {
            work_item_id: UI.into(),
            parent: Some(LAUNCH.into()),
            placement: WorkOutlinePlacement::After {
                sibling_work_item_id: API.into()
            },
        }
    );
    // The first child goes back to First.
    let entry = move_entry(&snapshot, API, None, &WorkOutlinePlacement::Last, None).unwrap();
    assert_eq!(
        entry.inverse,
        Step::Move {
            work_item_id: API.into(),
            parent: Some(LAUNCH.into()),
            placement: WorkOutlinePlacement::First,
        }
    );
    assert!(entry.label.contains("to the top level"));
    // Same parent is a reorder.
    let entry = move_entry(
        &snapshot,
        UI,
        Some(LAUNCH),
        &WorkOutlinePlacement::First,
        None,
    )
    .unwrap();
    assert!(entry.label.starts_with("reordered"));
    // Archived items cannot be moved, so nothing is logged.
    assert!(move_entry(&snapshot, RETIRED, None, &WorkOutlinePlacement::First, None).is_none());
}

#[test]
fn move_probes_require_the_parent_each_side_left() {
    let snapshot = snap();
    let entry = move_entry(&snapshot, UI, Some(API), &WorkOutlinePlacement::Last, None).unwrap();
    // Before the move UI sits under LAUNCH: redo's probe holds, undo's does not.
    assert!(entry.before.check(&snapshot).is_ok());
    assert!(entry.after.check(&snapshot).is_err());
    let mut moved = snapshot.clone();
    moved
        .items
        .iter_mut()
        .find(|item| item.id == UI)
        .unwrap()
        .parent_work_item_id = Some(API.into());
    assert!(entry.after.check(&moved).is_ok());
    assert!(entry.before.check(&moved).is_err());
}

#[test]
fn update_entry_round_trips_rename_status_and_description() {
    let original = snap();
    let cases = [
        WorkItemChanges {
            title: Some("API v2".into()),
            ..Default::default()
        },
        WorkItemChanges {
            status: Some(WorkItemStatus::Completed),
            ..Default::default()
        },
        WorkItemChanges {
            description: FieldPatch::Clear,
            ..Default::default()
        },
        WorkItemChanges {
            title: Some("Both".into()),
            description: FieldPatch::Set("new".into()),
            ..Default::default()
        },
    ];
    for changes in cases {
        let entry = update_entry(&original, API, &changes, None).unwrap();
        let mut state = original.clone();
        apply(&mut state, &entry.forward);
        assert_ne!(state.item(API), original.item(API), "{changes:?}");
        assert!(entry.after.check(&state).is_ok());
        assert!(entry.before.check(&state).is_err());
        apply(&mut state, &entry.inverse);
        assert_eq!(state.item(API), original.item(API), "{changes:?}");
        assert!(entry.before.check(&state).is_ok());
    }
}

#[test]
fn update_entry_labels_and_skips_what_cannot_be_inverted() {
    let snapshot = snap();
    let rename = WorkItemChanges {
        title: Some("Interface".into()),
        ..Default::default()
    };
    assert_eq!(
        update_entry(&snapshot, UI, &rename, None).unwrap().label,
        "renamed \u{2018}UI\u{2019} to \u{2018}Interface\u{2019}"
    );
    let done = WorkItemChanges {
        status: Some(WorkItemStatus::Completed),
        ..Default::default()
    };
    assert_eq!(
        update_entry(&snapshot, UI, &done, None).unwrap().label,
        "marked \u{2018}UI\u{2019} completed"
    );
    // No-ops, request resolutions, inheritance and sync state are not logged.
    let same = WorkItemChanges {
        title: Some("UI".into()),
        ..Default::default()
    };
    assert!(update_entry(&snapshot, UI, &same, None).is_none());
    let resolving = WorkItemChanges {
        status: Some(WorkItemStatus::Completed),
        resolve_open_requests: true,
        ..Default::default()
    };
    assert!(update_entry(&snapshot, UI, &resolving, None).is_none());
    let inherit = WorkItemChanges {
        inherit_parent_instructions: Some(false),
        ..Default::default()
    };
    assert!(update_entry(&snapshot, UI, &inherit, None).is_none());
}

#[test]
fn archive_and_restore_invert_each_other() {
    let snapshot = snap();
    let entry = archive_entry(
        &snapshot,
        UI,
        &DescendantArchivePolicy::ArchiveDescendants,
        None,
    )
    .unwrap();
    assert_eq!(entry.inverse, Step::Restore { work_item_id: UI.into() });
    let mut state = snapshot.clone();
    apply(&mut state, &entry.forward);
    assert!(entry.after.check(&state).is_ok());
    apply(&mut state, &entry.inverse);
    assert!(entry.before.check(&state).is_ok());

    let restore = restore_entry(&snapshot, RETIRED, None).unwrap();
    assert_eq!(restore.inverse, Step::Archive { work_item_id: RETIRED.into() });
    assert!(restore_entry(&snapshot, UI, None).is_none());
}

#[test]
fn archive_that_moves_children_out_is_not_undoable() {
    let snapshot = snap();
    let moving = DescendantArchivePolicy::MoveDescendants {
        destination_parent_work_item_id: None,
    };
    // LAUNCH has children: restore would not bring them back.
    assert!(archive_entry(&snapshot, LAUNCH, &moving, None).is_none());
    // A leaf has none to strand.
    assert!(archive_entry(&snapshot, UI, &moving, None).is_some());
}

#[test]
fn create_inverse_archives_and_refuses_once_the_item_gains_content() {
    let mut snapshot = snap();
    let entry = create_entry(UI, "UI", None);
    assert_eq!(entry.inverse, Step::Archive { work_item_id: UI.into() });
    assert_eq!(entry.forward, Step::Restore { work_item_id: UI.into() });
    // UI has a chat: undoing its creation would orphan it.
    assert!(entry.after.check(&snapshot).is_err());
    // A chat-less leaf is fine, a leaf with a child is not.
    let fresh = create_entry(HOUSEKEEPING, "Housekeeping", None);
    assert!(fresh.after.check(&snapshot).is_ok());
    snapshot
        .items
        .iter_mut()
        .find(|item| item.id == RETIRED)
        .unwrap()
        .parent_work_item_id = Some(HOUSEKEEPING.into());
    snapshot
        .items
        .iter_mut()
        .find(|item| item.id == RETIRED)
        .unwrap()
        .archived_at = None;
    snapshot
        .items
        .iter_mut()
        .find(|item| item.id == HOUSEKEEPING)
        .unwrap()
        .child_ids = vec![RETIRED.into()];
    assert!(fresh.after.check(&snapshot).is_err());
}

#[test]
fn chat_entry_restores_the_previous_item_or_unassociation() {
    let snapshot = snap();
    // Assigned chat moves to HOUSEKEEPING.
    let entry = chat_entry(&snapshot, CHAT_UI, "Fix UI", Some(HOUSEKEEPING), None);
    assert_eq!(
        entry.inverse,
        Step::Assign {
            chat_id: CHAT_UI.into(),
            work_item_id: UI.into()
        }
    );
    assert!(entry.label.starts_with("moved chat"));
    // A loose chat gets assigned; undo disassociates it.
    let entry = chat_entry(&snapshot, CHAT_LOOSE_A, "Loose", Some(UI), None);
    assert_eq!(entry.inverse, Step::RemoveChat { chat_id: CHAT_LOOSE_A.into() });
    // Removing an assigned chat: undo re-assigns.
    let entry = chat_entry(&snapshot, CHAT_UI, "Fix UI", None, None);
    assert_eq!(
        entry.inverse,
        Step::Assign {
            chat_id: CHAT_UI.into(),
            work_item_id: UI.into()
        }
    );
    assert!(entry.before.check(&snapshot).is_ok());
    assert!(entry.after.check(&snapshot).is_err());
}

#[test]
fn link_and_unlink_are_inverses_through_the_content_derived_id() {
    let snapshot = snap();
    let reference_id =
        zeron_proto::external_reference_id(API, "github", "pullRequest", "acme/launch#42");
    let unlink = unlink_entry(&snapshot, &reference_id, None).unwrap();
    let Step::LinkReference(data) = &unlink.inverse else {
        panic!("{:?}", unlink.inverse)
    };
    assert_eq!(data.reference_id(), reference_id);
    assert_eq!(data.work_item_id, API);
    assert!(unlink.before.check(&snapshot).is_ok());
    assert!(unlink.after.check(&snapshot).is_err());

    let link = link_entry(&snapshot, data.clone(), None);
    assert_eq!(
        link.inverse,
        Step::UnlinkReference {
            reference_id: reference_id.clone()
        }
    );
    // Linking something that is already there is not "undoable": its
    // before-probe fails, so redo/undo would refuse rather than duplicate.
    assert!(link.before.check(&snapshot).is_err());
    assert!(unlink_entry(&snapshot, "nope", None).is_none());
}

#[test]
fn instruction_edit_round_trips_and_detects_a_later_edit() {
    let mut snapshot = snap();
    snapshot.instructions = vec![
        instruction("i1", Some(API), "Be terse"),
        instruction("i2", Some(API), "Use tabs"),
    ];
    let entry = instruction_edit_entry(&snapshot, "i1", "Be brief", None).unwrap();
    assert!(entry.label.contains("\u{2018}API\u{2019}"));
    let mut state = snapshot.clone();
    apply(&mut state, &entry.forward);
    assert!(entry.after.check(&state).is_ok());
    // Somebody else edits it again: undo must refuse.
    let mut other = state.clone();
    other.instructions[0].text = "Be loud".into();
    assert!(entry.after.check(&other).is_err());
    apply(&mut state, &entry.inverse);
    assert_eq!(state.instructions[0].text, "Be terse");
    assert!(instruction_edit_entry(&snapshot, "i1", "Be terse", None).is_none());
    assert!(instruction_edit_entry(&snapshot, "gone", "x", None).is_none());
}

#[test]
fn instruction_move_inverse_returns_to_the_old_neighbour() {
    let mut snapshot = snap();
    snapshot.instructions = vec![
        instruction("i1", Some(API), "a"),
        instruction("i2", Some(API), "b"),
        instruction("i3", Some(API), "c"),
    ];
    let scope: Vec<String> = ["i1", "i2", "i3"].map(String::from).to_vec();
    // i2 moves up above i1.
    let entry = instruction_move_entry(
        &snapshot,
        &scope,
        "i2",
        InstructionPlacement::Before {
            sibling_instruction_id: "i1".into(),
        },
        None,
    )
    .unwrap();
    assert_eq!(
        entry.inverse,
        Step::MoveInstruction {
            instruction_id: "i2".into(),
            placement: InstructionPlacement::After {
                sibling_instruction_id: "i1".into()
            },
        }
    );
    // The first moves back before its old next sibling.
    let entry = instruction_move_entry(
        &snapshot,
        &scope,
        "i1",
        InstructionPlacement::After {
            sibling_instruction_id: "i2".into(),
        },
        None,
    )
    .unwrap();
    assert_eq!(
        entry.inverse,
        Step::MoveInstruction {
            instruction_id: "i1".into(),
            placement: InstructionPlacement::Before {
                sibling_instruction_id: "i2".into()
            },
        }
    );
    assert!(
        instruction_move_entry(
            &snapshot,
            &["i1".to_owned()],
            "i1",
            InstructionPlacement::Last,
            None
        )
        .is_none()
    );
}

fn simple(label: &str) -> Draft {
    draft(
        label.into(),
        Step::Restore { work_item_id: "a".into() },
        Step::Archive { work_item_id: "a".into() },
        Probe::Archived { work_item_id: "a".into(), archived: true },
        Probe::Archived { work_item_id: "a".into(), archived: false },
        None,
    )
}

#[test]
fn log_undo_redo_cursor_and_redo_tail_is_dropped_by_a_new_change() {
    let mut log = ActionLog::default();
    let a = log.record("s1", simple("a"));
    let b = log.record("s1", simple("b"));
    assert_eq!(log.next_undo("s1").unwrap().id, b);
    assert!(log.next_redo("s1").is_none());
    log.mark_undone("s1", b);
    assert_eq!(log.next_undo("s1").unwrap().id, a);
    assert_eq!(log.next_redo("s1").unwrap().id, b);
    log.mark_redone("s1", b);
    assert!(log.next_redo("s1").is_none());
    // Undo b, then record c: b can no longer be redone.
    log.mark_undone("s1", b);
    let c = log.record("s1", simple("c"));
    assert!(log.next_redo("s1").is_none());
    let recent = log.recent("s1", 10);
    assert_eq!(
        recent.iter().map(|r| (r.id, r.undone)).collect::<Vec<_>>(),
        [(c, false), (a, false)]
    );
    // A stale id never moves the cursor.
    log.mark_undone("s1", 9999);
    assert_eq!(log.next_undo("s1").unwrap().id, c);
}

#[test]
fn log_is_per_space_and_capped() {
    let mut log = ActionLog::default();
    log.record("s2", simple("other"));
    for n in 0..LOG_CAP + 25 {
        log.record("s1", simple(&format!("n{n}")));
    }
    assert_eq!(log.len("s1"), LOG_CAP);
    assert_eq!(log.len("s2"), 1);
    assert_eq!(log.last_space(), Some("s1"));
    let recent = log.recent("s1", 3);
    assert_eq!(recent[0].label, format!("n{}", LOG_CAP + 24));
    assert!(log.next_undo("s3").is_none());
}

#[test]
fn failed_forward_request_drops_its_entry() {
    let mut log = ActionLog::default();
    let mut pending = simple("pending");
    pending.operation_id = Some("op-1".into());
    log.record("s1", simple("first"));
    log.record("s1", pending);
    log.fail("op-1");
    assert_eq!(log.len("s1"), 1);
    assert_eq!(log.next_undo("s1").unwrap().label, "first");
    let mut kept = simple("kept");
    kept.operation_id = Some("op-2".into());
    log.record("s1", kept);
    log.confirm("op-2");
    log.fail("op-2");
    assert_eq!(log.len("s1"), 2);
}

#[test]
fn undo_chain_lists_newest_first_down_to_the_chosen_entry() {
    let mut log = ActionLog::default();
    let a = log.record("s1", simple("a"));
    let b = log.record("s1", simple("b"));
    let c = log.record("s1", simple("c"));
    assert_eq!(log.undo_chain("s1", b), Some(vec![c, b]));
    assert_eq!(log.undo_chain("s1", a), Some(vec![c, b, a]));
    log.mark_undone("s1", c);
    // c is already undone: not part of a chain.
    assert_eq!(log.undo_chain("s1", c), None);
    assert_eq!(log.undo_chain("s1", b), Some(vec![b]));
}

#[test]
fn long_titles_are_shortened_in_labels() {
    let long = "x".repeat(100);
    let entry = create_entry("w", &long, None);
    assert!(entry.label.chars().count() < 60, "{}", entry.label);
    assert!(entry.label.contains('…'));
    assert_eq!(create_entry("w", "  ", None).label, "created \u{2018}Untitled\u{2019}");
}
