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
    press(vcx, "shift-up");
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
    press(vcx, "shift-up");
    assert_eq!(calls.lock().unwrap().len(), 1);
    refresh(&overview, vcx, fixture("s1"));

    // Move down: API after UI.
    select_item(&outline, vcx, API);
    press(vcx, "shift-down");
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
        (API, "shift-up"),
        (UI, "shift-down"),
        (HOUSEKEEPING, "shift-down"),
        (API, "tab"),
        (API, "shift-right"),
        (LAUNCH, "tab"),
        (LAUNCH, "shift-tab"),
        (LAUNCH, "shift-left"),
    ] {
        select_item(&outline, vcx, item);
        press(vcx, keys);
        assert_eq!(calls.lock().unwrap().len(), 2, "{item} {keys} is a no-op");
    }

    // Indent: UI becomes the last child of API (Shift+Right; Tab is an alias,
    // covered below).
    select_item(&outline, vcx, UI);
    press(vcx, "shift-right");
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
    press(vcx, "shift-left");
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
    press(vcx, "shift-down");
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
    press(vcx, "shift-up");
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
    press(vcx, "shift-up");
    assert!(outline.read_with(vcx, |outline, _| outline.notice().is_some()));
    press(vcx, "shift-up");
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
    press(vcx, "shift-down");
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
    for keys in [
        "shift-up",
        "shift-down",
        "shift-left",
        "shift-right",
        "tab",
        "shift-tab",
    ] {
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
    for keys in ["shift-up", "shift-left", "shift-right", "tab", "shift-tab"] {
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
    press(vcx, "shift-up");
    press(vcx, "shift-right");
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

/// Where on a target row a drag is released.
#[derive(Clone, Copy)]
enum Spot {
    Upper,
    Middle,
    Lower,
}

fn row_selector(key: &str) -> String {
    format!("outline-row-{key}")
}

/// Press on row `from`, drag onto row `to` at `spot`, and stop there (the
/// button is still down). Returns the release point.
fn drag_over(
    vcx: &mut gpui::VisualTestContext,
    from: &str,
    to: &str,
    spot: Spot,
) -> gpui::Point<Pixels> {
    let start = row_bounds(vcx, row_selector(from))
        .unwrap_or_else(|| panic!("{from} is shown"))
        .center();
    let target = row_bounds(vcx, row_selector(to)).unwrap_or_else(|| panic!("{to} is shown"));
    let end = match spot {
        Spot::Upper => gpui::point(target.center().x, target.top() + px(2.0)),
        Spot::Middle => target.center(),
        Spot::Lower => gpui::point(target.center().x, target.bottom() - px(2.0)),
    };
    let left = Some(gpui::MouseButton::Left);
    vcx.simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::default());
    vcx.simulate_mouse_move(
        start + gpui::point(px(0.0), px(6.0)),
        left,
        gpui::Modifiers::default(),
    );
    vcx.simulate_mouse_move(end, left, gpui::Modifiers::default());
    vcx.simulate_mouse_move(end, left, gpui::Modifiers::default());
    draw(vcx);
    end
}

fn release(vcx: &mut gpui::VisualTestContext, at: gpui::Point<Pixels>) {
    vcx.simulate_mouse_up(at, gpui::MouseButton::Left, gpui::Modifiers::default());
    vcx.run_until_parked();
    draw(vcx);
}

fn hint(
    outline: &Entity<OutlineView>,
    vcx: &mut gpui::VisualTestContext,
) -> Option<(String, crate::work_outline::restructure::DropZone)> {
    outline.read_with(vcx, |outline, _| {
        outline
            .drop_hint()
            .map(|hint| (hint.key.clone(), hint.zone))
    })
}

/// Dropping a work item on another item's middle makes it the target's last
/// child; the upper / lower edge band puts it before / after the target
/// under the target's parent. Each drop is one moveWorkItem, the indicator
/// shows first, and the selection follows the moved item.
#[gpui::test]
fn drag_onto_items_and_edges_sends_moves(cx: &mut gpui::TestAppContext) {
    use crate::work_outline::restructure::DropZone;
    let dir = tempfile::tempdir().unwrap();
    let fixture = crate::work_outline::store::fixture_snapshot;
    let (overview, outline, calls, vcx) =
        restructure_overview(cx, dir.path(), fixture("s1"), reply_ok);
    let item = |id: &str| row_key::item("s1", id);

    // Middle of API: last child of API.
    let at = drag_over(vcx, &item(HOUSEKEEPING), &item(API), Spot::Middle);
    assert_eq!(hint(&outline, vcx), Some((item(API), DropZone::Into)));
    release(vcx, at);
    assert_eq!(
        move_at(&calls, 0),
        (
            serde_json::json!(API),
            placement("last", None),
            HOUSEKEEPING.to_owned()
        )
    );
    assert_eq!(hint(&outline, vcx), None, "the indicator goes on release");
    assert_eq!(selected(&outline, vcx), Some(item(HOUSEKEEPING)));
    refresh(&overview, vcx, fixture("s1"));

    // Upper band of UI: before UI, under Launch v2.
    let at = drag_over(vcx, &item(HOUSEKEEPING), &item(UI), Spot::Upper);
    assert_eq!(hint(&outline, vcx), Some((item(UI), DropZone::Before)));
    assert!(row_bounds(vcx, format!("outline-drop-before-{}", item(UI))).is_some());
    release(vcx, at);
    assert_eq!(
        move_at(&calls, 1),
        (
            serde_json::json!(LAUNCH),
            placement("before", Some(UI)),
            HOUSEKEEPING.to_owned()
        )
    );
    refresh(&overview, vcx, fixture("s1"));

    // Lower band of API: after API, under Launch v2.
    let at = drag_over(vcx, &item(HOUSEKEEPING), &item(API), Spot::Lower);
    assert!(row_bounds(vcx, format!("outline-drop-after-{}", item(API))).is_some());
    release(vcx, at);
    assert_eq!(
        move_at(&calls, 2),
        (
            serde_json::json!(LAUNCH),
            placement("after", Some(API)),
            HOUSEKEEPING.to_owned()
        )
    );
    // Only moves: nothing else (Canvas positions included) is written.
    assert_eq!(ops(&calls), ["moveWorkItem"; 3]);
}

/// A work item cannot be dropped on itself or its descendants: no
/// indicator, and releasing there sends nothing.
#[gpui::test]
fn drag_onto_self_or_descendant_is_refused(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, vcx) = restructure_overview(
        cx,
        dir.path(),
        crate::work_outline::store::fixture_snapshot("s1"),
        reply_ok,
    );
    let item = |id: &str| row_key::item("s1", id);
    for (target, spot) in [
        (API_AUTH, Spot::Middle),
        (API, Spot::Upper),
        (UI, Spot::Lower),
        (LAUNCH, Spot::Middle),
    ] {
        let at = drag_over(vcx, &item(LAUNCH), &item(target), spot);
        assert_eq!(hint(&outline, vcx), None, "{target} is refused");
        assert!(row_bounds(vcx, format!("outline-drop-before-{}", item(target))).is_none());
        assert!(row_bounds(vcx, format!("outline-drop-after-{}", item(target))).is_none());
        release(vcx, at);
    }
    assert!(calls.lock().unwrap().is_empty());
}

/// Dragging a Chat row onto a work item associates it (the same
/// assignChatToWorkItem the picker sends); its current item is refused.
#[gpui::test]
fn dragging_a_chat_onto_an_item_associates_it(cx: &mut gpui::TestAppContext) {
    use crate::work_outline::restructure::DropZone;
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, vcx) = restructure_overview(
        cx,
        dir.path(),
        crate::work_outline::store::fixture_snapshot("s1"),
        reply_ok,
    );
    let item = |id: &str| row_key::item("s1", id);

    // Its own item: nothing to do.
    let at = drag_over(vcx, &row_key::chat(CHAT_UI), &item(UI), Spot::Middle);
    assert_eq!(hint(&outline, vcx), None);
    release(vcx, at);
    assert!(calls.lock().unwrap().is_empty());

    // Any spot on another item's row means "into".
    let at = drag_over(
        vcx,
        &row_key::chat(CHAT_UI),
        &item(HOUSEKEEPING),
        Spot::Upper,
    );
    assert_eq!(
        hint(&outline, vcx),
        Some((item(HOUSEKEEPING), DropZone::Into))
    );
    release(vcx, at);
    let call = calls.lock().unwrap()[0].clone();
    assert_eq!(call["op"], "assignChatToWorkItem", "{call}");
    assert_eq!(call["chatId"], CHAT_UI);
    assert_eq!(call["workItemId"], HOUSEKEEPING);
    assert_eq!(selected(&outline, vcx), Some(row_key::chat(CHAT_UI)));
}

/// A drop that changes an assigned Chat's effective instructions goes
/// through the drift confirmation; a same-parent reorder does not.
#[gpui::test]
fn drops_confirm_drift_when_instructions_change(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = restructure_overview(
        cx,
        dir.path(),
        instruction_fixture::snapshot("s1"),
        reply_ok,
    );
    let item = |id: &str| row_key::item("s1", id);

    // API before Launch v2 (top level) loses Launch's instruction.
    let at = drag_over(vcx, &item(API), &item(LAUNCH), Spot::Upper);
    release(vcx, at);
    let request = outline
        .update(vcx, |outline, _| outline.captured_drift.take())
        .expect("asks");
    assert!(calls.lock().unwrap().is_empty());
    vcx.update(|window, cx| (request.on_decision)(true, window, cx));
    vcx.run_until_parked();
    assert_eq!(
        move_at(&calls, 0),
        (
            serde_json::Value::Null,
            placement("before", Some(LAUNCH)),
            API.to_owned()
        )
    );
    refresh(&overview, vcx, instruction_fixture::snapshot("s1"));

    // UI before API: same parent, no question.
    let at = drag_over(vcx, &item(UI), &item(API), Spot::Upper);
    release(vcx, at);
    assert!(outline.read_with(vcx, |outline, _| outline.captured_drift.is_none()));
    assert_eq!(
        move_at(&calls, 1),
        (
            serde_json::json!(LAUNCH),
            placement("before", Some(API)),
            UI.to_owned()
        )
    );
}

/// Tab / Shift+Tab stay aliases for indent / outdent.
#[gpui::test]
fn tab_and_shift_tab_alias_indent_and_outdent(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let fixture = crate::work_outline::store::fixture_snapshot;
    let (overview, outline, calls, vcx) =
        restructure_overview(cx, dir.path(), fixture("s1"), reply_ok);
    select_item(&outline, vcx, UI);
    press(vcx, "tab");
    assert_eq!(
        move_at(&calls, 0),
        (
            serde_json::json!(API),
            placement("last", None),
            UI.to_owned()
        )
    );
    refresh(&overview, vcx, fixture("s1"));
    select_item(&outline, vcx, API_AUTH);
    press(vcx, "shift-tab");
    assert_eq!(
        move_at(&calls, 1),
        (
            serde_json::json!(LAUNCH),
            placement("after", Some(API)),
            API_AUTH.to_owned()
        )
    );
}

/// Up / Down walk the rows as displayed; Alt+Up / Alt+Down jump over
/// subtrees; Home / End go to the ends. Nothing here sends a request.
#[gpui::test]
fn arrows_follow_visual_order_and_alt_arrows_skip_subtrees(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, vcx) = restructure_overview(
        cx,
        dir.path(),
        crate::work_outline::store::fixture_snapshot("s1"),
        reply_ok,
    );
    let item = |id: &str| Some(row_key::item("s1", id));

    // Down from an expanded parent enters its first child.
    select_item(&outline, vcx, LAUNCH);
    press(vcx, "down");
    assert_eq!(selected(&outline, vcx), item(API));
    // Up from a first child selects the parent.
    press(vcx, "up");
    assert_eq!(selected(&outline, vcx), item(LAUNCH));
    // Down keeps descending: API's own subtree is walked row by row.
    press(vcx, "down");
    press(vcx, "down");
    let below_api = selected(&outline, vcx);
    assert_ne!(below_api, item(UI), "Down did not skip API's subtree");
    assert!(below_api.is_some());
    // Alt+Down from API jumps over its subtree to the next sibling.
    select_item(&outline, vcx, API);
    press(vcx, "alt-down");
    assert_eq!(selected(&outline, vcx), item(UI));
    press(vcx, "alt-up");
    assert_eq!(selected(&outline, vcx), item(API));
    // Alt+Down from the first top-level item skips the whole subtree.
    select_item(&outline, vcx, LAUNCH);
    press(vcx, "alt-down");
    assert_eq!(selected(&outline, vcx), item(HOUSEKEEPING));
    // Down from the last child of an expanded branch (past its own Chat
    // row) reaches the parent's next sibling.
    select_item(&outline, vcx, UI);
    press(vcx, "down");
    assert_eq!(selected(&outline, vcx), Some(row_key::chat(CHAT_UI)));
    press(vcx, "down");
    assert_eq!(selected(&outline, vcx), item(HOUSEKEEPING));
    // Up crosses back into the Chat row.
    press(vcx, "up");
    assert_eq!(selected(&outline, vcx), Some(row_key::chat(CHAT_UI)));
    // Home / End.
    press(vcx, "home");
    assert_eq!(selected(&outline, vcx), item(LAUNCH));
    press(vcx, "end");
    let last = outline.read_with(vcx, |outline, _| {
        outline.model().rows.last().unwrap().key.clone()
    });
    assert_eq!(selected(&outline, vcx), Some(last));
    assert!(calls.lock().unwrap().is_empty());
}

/// Left / Right keep collapse / expand, and the plain arrows are inert in a
/// text input (the inline rename editor).
#[gpui::test]
fn navigation_arrows_are_inert_while_renaming(cx: &mut gpui::TestAppContext) {
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
    for keys in ["up", "down", "alt-up", "alt-down", "home", "end"] {
        press(vcx, keys);
        assert_eq!(
            selected(&outline, vcx),
            Some(row_key::item("s1", UI)),
            "{keys}"
        );
    }
    assert!(calls.lock().unwrap().is_empty());
}

/// Click a row (after focus has left the Outline); Outline keys must work
/// straight away.
fn click_row(vcx: &mut gpui::VisualTestContext, key: &str) {
    let at = row_bounds(vcx, format!("outline-row-{key}"))
        .unwrap_or_else(|| panic!("{key} renders"))
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    draw(vcx);
}

fn editing(outline: &Entity<OutlineView>, vcx: &mut gpui::VisualTestContext) -> bool {
    outline.read_with(vcx, |outline, _| outline.is_editing())
}

/// F2 and bare R rename a work item in place.
#[gpui::test]
fn f2_and_r_rename_a_work_item(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, vcx) = restructure_overview(
        cx,
        dir.path(),
        crate::work_outline::store::fixture_snapshot("s1"),
        reply_ok,
    );
    for keys in ["f2", "r"] {
        select_item(&outline, vcx, UI);
        assert!(!editing(&outline, vcx));
        press(vcx, keys);
        assert!(editing(&outline, vcx), "{keys} starts an inline rename");
        outline.update_in(vcx, |outline, _, cx| outline.cancel_edit(cx));
        draw(vcx);
        focus_outline(&outline, vcx);
    }
    assert!(calls.lock().unwrap().is_empty());
}

/// F2 and R on a Chat row ask the Overview to open the Chat rename dialog.
#[gpui::test]
fn f2_and_r_rename_a_chat_row(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, _calls, vcx) = restructure_overview(
        cx,
        dir.path(),
        crate::work_outline::store::fixture_snapshot("s1"),
        reply_ok,
    );
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = events.clone();
    let _subscription = vcx.update(|_, cx| {
        cx.subscribe(&outline, move |_, event: &OutlineEvent, _| {
            sink.lock().unwrap().push(event.clone());
        })
    });
    for keys in ["f2", "r"] {
        outline.update(vcx, |outline, cx| {
            outline.select(&row_key::chat(CHAT_UI), cx)
        });
        draw(vcx);
        press(vcx, keys);
        assert!(!editing(&outline, vcx), "a Chat row has no inline editor");
    }
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            OutlineEvent::RenameChat(CHAT_UI.to_owned()),
            OutlineEvent::RenameChat(CHAT_UI.to_owned())
        ]
    );
}

/// Clicking a row gives the Outline keyboard focus, so F2 / R work right away.
#[gpui::test]
fn clicking_a_row_focuses_the_outline_for_f2_and_r(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, _calls, vcx) = restructure_overview(
        cx,
        dir.path(),
        crate::work_outline::store::fixture_snapshot("s1"),
        reply_ok,
    );
    for keys in ["f2", "r"] {
        vcx.update(|window, _| window.blur());
        draw(vcx);
        let focused = vcx.update(|window, cx| {
            gpui::Focusable::focus_handle(outline.read(cx), cx).is_focused(window)
        });
        assert!(!focused, "focus left the Outline before the click");
        click_row(vcx, &row_key::item("s1", UI));
        assert_eq!(selected(&outline, vcx), Some(row_key::item("s1", UI)));
        press(vcx, keys);
        assert!(editing(&outline, vcx), "{keys} after a click");
        outline.update_in(vcx, |outline, _, cx| outline.cancel_edit(cx));
        draw(vcx);
    }
}

/// F2 / R do not fire while the inline editor owns the keyboard.
#[gpui::test]
fn rename_keys_are_inert_in_text_inputs(cx: &mut gpui::TestAppContext) {
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
    press(vcx, "r");
    press(vcx, "f2");
    assert!(editing(&outline, vcx));
    assert!(calls.lock().unwrap().is_empty());
}
