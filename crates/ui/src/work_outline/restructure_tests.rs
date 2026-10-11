// Direct restructuring tests (keyboard, palette, move picker placement, drag
// and drop). Included from `overview.rs`'s test module so they share its
// Overview fixtures.
use super::*;

use crate::work_outline::outline_view::OutlineView;

/// An Outline over `snapshot` with a recording engine, the outline keys
/// bound, and the Outline focused.
fn restructure_overview<'a>(
    cx: &'a mut gpui::TestAppContext,
    dir: &std::path::Path,
    snapshot: WorkOutlineSnapshot,
    reply: impl Fn(&serde_json::Value) -> Result<serde_json::Value, zeron_rpc::RpcError>
    + Send
    + Sync
    + 'static,
) -> (
    Entity<Overview>,
    Entity<OutlineView>,
    OutlineCalls,
    &'a mut gpui::VisualTestContext,
) {
    let (overview, calls, vcx) = instruction_overview(cx, dir, snapshot, reply);
    vcx.update(|_, cx| cx.bind_keys(crate::work_outline::outline_view::key_bindings()));
    let outline = overview.read_with(vcx, |o, _| o.outline.clone());
    focus_outline(&outline, vcx);
    (overview, outline, calls, vcx)
}

fn focus_outline(outline: &Entity<OutlineView>, vcx: &mut gpui::VisualTestContext) {
    vcx.update(|window, cx| {
        let handle = gpui::Focusable::focus_handle(outline.read(cx), cx);
        window.focus(&handle, cx);
    });
    draw(vcx);
}

fn select_item(outline: &Entity<OutlineView>, vcx: &mut gpui::VisualTestContext, item: &str) {
    outline.update(vcx, |outline, cx| {
        outline.select(&row_key::item("s1", item), cx)
    });
    draw(vcx);
}

fn press(vcx: &mut gpui::VisualTestContext, keys: &str) {
    vcx.simulate_keystrokes(keys);
    vcx.run_until_parked();
}

/// A fresh frame of `snapshot` (a new allocation, as every engine frame is):
/// lets the next restructuring plan from it.
fn refresh(
    overview: &Entity<Overview>,
    vcx: &mut gpui::VisualTestContext,
    snapshot: WorkOutlineSnapshot,
) {
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(snapshot)), cx)
    });
    draw(vcx);
}

/// `(destination parent, placement)` of the recorded call `ix`.
fn move_at(calls: &OutlineCalls, ix: usize) -> (serde_json::Value, serde_json::Value, String) {
    let call = calls.lock().unwrap()[ix].clone();
    assert_eq!(call["op"], "moveWorkItem", "{call}");
    (
        call["destinationParentWorkItemId"].clone(),
        call["placement"].clone(),
        call["workItemId"].as_str().unwrap_or_default().to_owned(),
    )
}

fn placement(kind: &str, sibling: Option<&str>) -> serde_json::Value {
    match sibling {
        Some(sibling) => serde_json::json!({"type": kind, "siblingWorkItemId": sibling}),
        None => serde_json::json!({"type": kind}),
    }
}

fn selected(outline: &Entity<OutlineView>, vcx: &mut gpui::VisualTestContext) -> Option<String> {
    outline.read_with(vcx, |outline, _| outline.selected_key().map(str::to_owned))
}

/// Acceptance 1: each key sends one `moveWorkItem` naming the parent and
/// the placement; boundaries send nothing.
#[gpui::test]
fn keyboard_restructuring_sends_moves_and_noops_at_boundaries(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let fixture = crate::work_outline::store::fixture_snapshot;
    let (overview, outline, calls, vcx) =
        restructure_overview(cx, dir.path(), fixture("s1"), reply_ok);
    let null = serde_json::Value::Null;

    // Move up: UI before API, same parent.
    select_item(&outline, vcx, UI);
    press(vcx, "alt-shift-up");
    assert_eq!(
        move_at(&calls, 0),
        (
            serde_json::json!(LAUNCH),
            placement("before", Some(API)),
            UI.to_owned()
        )
    );
    assert_eq!(selected(&outline, vcx), Some(row_key::item("s1", UI)));
    // Until the frame showing it arrives, another press waits (it would
    // plan from the stale order).
    press(vcx, "alt-shift-up");
    assert_eq!(calls.lock().unwrap().len(), 1);
    refresh(&overview, vcx, fixture("s1"));

    // Move down: API after UI.
    select_item(&outline, vcx, API);
    press(vcx, "alt-shift-down");
    assert_eq!(
        move_at(&calls, 1),
        (
            serde_json::json!(LAUNCH),
            placement("after", Some(UI)),
            API.to_owned()
        )
    );
    refresh(&overview, vcx, fixture("s1"));

    // Boundaries: first / last sibling, nothing to indent under, top level.
    for (item, keys) in [
        (API, "alt-shift-up"),
        (UI, "alt-shift-down"),
        (HOUSEKEEPING, "alt-shift-down"),
        (API, "tab"),
        (LAUNCH, "tab"),
        (LAUNCH, "shift-tab"),
    ] {
        select_item(&outline, vcx, item);
        press(vcx, keys);
        assert_eq!(calls.lock().unwrap().len(), 2, "{item} {keys} is a no-op");
    }

    // Indent: UI becomes the last child of API.
    select_item(&outline, vcx, UI);
    press(vcx, "tab");
    assert_eq!(
        move_at(&calls, 2),
        (
            serde_json::json!(API),
            placement("last", None),
            UI.to_owned()
        )
    );
    refresh(&overview, vcx, fixture("s1"));

    // Outdent: Auth endpoints goes to Launch v2, right after API.
    select_item(&outline, vcx, API_AUTH);
    press(vcx, "shift-tab");
    assert_eq!(
        move_at(&calls, 3),
        (
            serde_json::json!(LAUNCH),
            placement("after", Some(API)),
            API_AUTH.to_owned()
        )
    );
    refresh(&overview, vcx, fixture("s1"));

    // Outdent from depth 1 lands at the Space root after the old parent.
    select_item(&outline, vcx, UI);
    press(vcx, "shift-tab");
    assert_eq!(
        move_at(&calls, 4),
        (null, placement("after", Some(LAUNCH)), UI.to_owned())
    );
    assert_eq!(ops(&calls).len(), 5);
}

/// The whole subtree travels with one request, and the selection follows
/// the moved item to its new row once the frame arrives.
#[gpui::test]
fn descendants_move_with_the_item_and_selection_follows(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let fixture = crate::work_outline::store::fixture_snapshot;
    let (overview, outline, calls, vcx) =
        restructure_overview(cx, dir.path(), fixture("s1"), reply_ok);
    select_item(&outline, vcx, API);
    press(vcx, "alt-shift-down");
    assert_eq!(ops(&calls), ["moveWorkItem"], "one request for the subtree");

    // The engine's frame: Launch v2 > [UI, API > Auth endpoints].
    let mut moved = fixture("s1");
    moved
        .items
        .iter_mut()
        .find(|item| item.id == LAUNCH)
        .unwrap()
        .child_ids = vec![UI.into(), API.into()];
    refresh(&overview, vcx, moved);
    let (ui, api, auth) = outline.read_with(vcx, |outline, _| {
        let model = outline.model();
        (
            model.position(&row_key::item("s1", UI)).unwrap(),
            model.position(&row_key::item("s1", API)).unwrap(),
            model.position(&row_key::item("s1", API_AUTH)).unwrap(),
        )
    });
    assert!(ui < api && api < auth, "{ui} {api} {auth}");
    assert_eq!(selected(&outline, vcx), Some(row_key::item("s1", API)));
    outline.read_with(vcx, |outline, _| assert!(!outline.restructuring_busy()));
}

/// A move that changes an assigned Chat's effective instructions asks
/// first; a reorder under the same parent never does.
#[gpui::test]
fn keyboard_moves_confirm_drift_only_when_instructions_change(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = restructure_overview(
        cx,
        dir.path(),
        instruction_fixture::snapshot("s1"),
        reply_ok,
    );
    // Outdenting API to the top level loses Launch's instruction.
    select_item(&outline, vcx, API);
    press(vcx, "shift-tab");
    let request = outline
        .update(vcx, |outline, _| outline.captured_drift.take())
        .expect("asks");
    assert_eq!(request.impact.summary(), "2 Chats and 2 work items");
    assert!(calls.lock().unwrap().is_empty());
    vcx.update(|window, cx| (request.on_decision)(false, window, cx));
    vcx.run_until_parked();
    assert!(calls.lock().unwrap().is_empty(), "cancel sends nothing");

    press(vcx, "shift-tab");
    let request = outline
        .update(vcx, |outline, _| outline.captured_drift.take())
        .expect("asks again");
    vcx.update(|window, cx| (request.on_decision)(true, window, cx));
    vcx.run_until_parked();
    assert_eq!(
        move_at(&calls, 0),
        (
            serde_json::Value::Null,
            placement("after", Some(LAUNCH)),
            API.to_owned()
        )
    );
    refresh(&overview, vcx, instruction_fixture::snapshot("s1"));

    // Reordering siblings keeps every effective instruction: no question.
    select_item(&outline, vcx, UI);
    press(vcx, "alt-shift-up");
    assert!(outline.read_with(vcx, |outline, _| outline.captured_drift.is_none()));
    assert_eq!(ops(&calls), ["moveWorkItem", "moveWorkItem"]);
}

/// A failed move keeps its operation id: pressing again replays it.
#[gpui::test]
fn retrying_a_failed_move_reuses_its_operation_id(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let failures = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(1));
    let remaining = failures.clone();
    let (overview, outline, calls, vcx) = restructure_overview(
        cx,
        dir.path(),
        crate::work_outline::store::fixture_snapshot("s1"),
        move |params| {
            if remaining
                .fetch_update(
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                    |n| n.checked_sub(1),
                )
                .is_ok()
            {
                return Err(zeron_rpc::RpcError::Failed("host unreachable".into()));
            }
            reply_ok(params)
        },
    );
    select_item(&outline, vcx, UI);
    press(vcx, "alt-shift-up");
    assert!(outline.read_with(vcx, |outline, _| outline.notice().is_some()));
    press(vcx, "alt-shift-up");
    let ids: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| call["operationId"].as_str().unwrap().to_owned())
        .collect();
    assert!(ids.len() >= 2, "{ids:?}");
    assert!(ids.iter().all(|id| id == &ids[0]), "{ids:?}");
    let sent = ids.len();
    refresh(
        &overview,
        vcx,
        crate::work_outline::store::fixture_snapshot("s1"),
    );

    // A different move is a new operation.
    select_item(&outline, vcx, API);
    press(vcx, "alt-shift-down");
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), sent + 1);
    assert_ne!(calls[sent]["operationId"].as_str().unwrap(), ids[0]);
}

/// The restructuring keys are inert while a text field owns focus (inline
/// rename, the picker), and Tab without a selected work item propagates.
#[gpui::test]
fn restructuring_keys_are_inert_in_text_inputs(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, vcx) = restructure_overview(
        cx,
        dir.path(),
        crate::work_outline::store::fixture_snapshot("s1"),
        reply_ok,
    );
    select_item(&outline, vcx, UI);
    outline.update_in(vcx, |outline, window, cx| {
        outline.begin_rename(window, cx).unwrap()
    });
    draw(vcx);
    for keys in ["alt-shift-up", "alt-shift-down", "tab", "shift-tab"] {
        press(vcx, keys);
    }
    assert!(
        calls.lock().unwrap().is_empty(),
        "rename editor owns the keys"
    );
    outline.update(vcx, |outline, cx| outline.cancel_edit(cx));
    focus_outline(&outline, vcx);

    outline.update_in(vcx, |outline, window, cx| {
        outline.open_move_picker(window, cx).unwrap()
    });
    draw(vcx);
    for keys in ["alt-shift-up", "tab", "shift-tab"] {
        press(vcx, keys);
    }
    assert!(calls.lock().unwrap().is_empty(), "the picker owns the keys");
    outline.update_in(vcx, |outline, window, cx| outline.close_picker(window, cx));
    draw(vcx);

    // A Chat row selected: nothing to move, no notice.
    outline.update(vcx, |outline, cx| {
        outline.select(&row_key::chat(CHAT_UI), cx)
    });
    press(vcx, "tab");
    press(vcx, "alt-shift-up");
    assert!(calls.lock().unwrap().is_empty());
    assert!(outline.read_with(vcx, |outline, _| outline.notice().is_none()));
}

/// The four palette commands run the same restructuring as the keys; with
/// no work item selected they leave a notice instead of doing nothing.
#[gpui::test]
fn palette_commands_restructure_the_selected_item(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let fixture = crate::work_outline::store::fixture_snapshot;
    let (overview, outline, calls, vcx) =
        restructure_overview(cx, dir.path(), fixture("s1"), reply_ok);
    let run = |command: WorkOutlineCommand, vcx: &mut gpui::VisualTestContext| {
        vcx.update(|window, cx| {
            overview.update(cx, |o, cx| o.run_work_outline_command(command, window, cx))
        });
        vcx.run_until_parked();
    };
    let expected = [
        (
            WorkOutlineCommand::MoveWorkItemUp,
            UI,
            serde_json::json!(LAUNCH),
            placement("before", Some(API)),
        ),
        (
            WorkOutlineCommand::MoveWorkItemDown,
            API,
            serde_json::json!(LAUNCH),
            placement("after", Some(UI)),
        ),
        (
            WorkOutlineCommand::IndentWorkItem,
            HOUSEKEEPING,
            serde_json::json!(LAUNCH),
            placement("last", None),
        ),
        (
            WorkOutlineCommand::OutdentWorkItem,
            API_AUTH,
            serde_json::json!(LAUNCH),
            placement("after", Some(API)),
        ),
    ];
    for (ix, (command, item, parent, place)) in expected.into_iter().enumerate() {
        select_item(&outline, vcx, item);
        run(command, vcx);
        assert_eq!(move_at(&calls, ix), (parent, place, item.to_owned()));
        refresh(&overview, vcx, fixture("s1"));
    }

    // No selection: a notice, no request.
    outline.update(vcx, |outline, cx| {
        outline.select(&row_key::chat(CHAT_UI), cx)
    });
    run(WorkOutlineCommand::IndentWorkItem, vcx);
    assert_eq!(calls.lock().unwrap().len(), 4);
    assert_eq!(
        outline.read_with(vcx, |outline, _| outline.notice().map(str::to_owned)),
        Some("Select a work item first".to_owned())
    );
}

/// The move picker places at the bottom by default; "Top" (or Left) puts
/// the item first under the chosen parent.
#[gpui::test]
fn move_picker_places_at_top_or_bottom(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let fixture = crate::work_outline::store::fixture_snapshot;
    let (overview, outline, calls, vcx) =
        restructure_overview(cx, dir.path(), fixture("s1"), reply_ok);
    let open = |vcx: &mut gpui::VisualTestContext| {
        select_item(&outline, vcx, HOUSEKEEPING);
        outline.update_in(vcx, |outline, window, cx| {
            outline.open_move_picker(window, cx).unwrap()
        });
        draw(vcx);
    };
    // Entry 1 is Launch v2 (entry 0 is the top level).
    let choose_launch = |vcx: &mut gpui::VisualTestContext| {
        outline.update_in(vcx, |outline, window, cx| {
            outline.choose_picker_entry(1, window, cx)
        });
        vcx.run_until_parked();
        draw(vcx);
    };

    open(vcx);
    choose_launch(vcx);
    assert_eq!(
        move_at(&calls, 0),
        (
            serde_json::json!(LAUNCH),
            placement("last", None),
            HOUSEKEEPING.to_owned()
        )
    );
    refresh(&overview, vcx, fixture("s1"));

    open(vcx);
    let top = row_bounds(vcx, "outline-picker-place-top".into())
        .expect("placement chips render for a move")
        .center();
    vcx.simulate_click(top, gpui::Modifiers::default());
    choose_launch(vcx);
    assert_eq!(
        move_at(&calls, 1),
        (
            serde_json::json!(LAUNCH),
            placement("first", None),
            HOUSEKEEPING.to_owned()
        )
    );
    {
        let calls = calls.lock().unwrap();
        assert_ne!(calls[0]["operationId"], calls[1]["operationId"]);
    }
    refresh(&overview, vcx, fixture("s1"));

    // Keyboard: Left = Top, Right = Bottom.
    open(vcx);
    press(vcx, "left");
    press(vcx, "right");
    press(vcx, "left");
    choose_launch(vcx);
    assert_eq!(move_at(&calls, 2).1, placement("first", None));

    // The associate picker has no placement.
    outline.update(vcx, |outline, cx| {
        outline.select(&row_key::chat(CHAT_LOOSE_A), cx)
    });
    outline.update_in(vcx, |outline, window, cx| {
        outline.open_associate_picker(None, window, cx).unwrap()
    });
    draw(vcx);
    assert!(row_bounds(vcx, "outline-picker-place-top".into()).is_none());
}
