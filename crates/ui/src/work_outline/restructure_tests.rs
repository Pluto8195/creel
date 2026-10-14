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

/// A fresh frame of `snapshot` (a new allocation, as every engine frame is),
/// with the wait for earlier optimistic changes over: the frame wins, so the
/// next restructuring plans from exactly `snapshot`.
fn refresh(
    overview: &Entity<Overview>,
    vcx: &mut gpui::VisualTestContext,
    snapshot: WorkOutlineSnapshot,
) {
    deliver_frame(overview, vcx, snapshot);
    vcx.executor()
        .advance_clock(std::time::Duration::from_secs(3));
    vcx.run_until_parked();
    draw(vcx);
}

/// Just a frame of `snapshot`: pending optimistic changes it does not show
/// stay on top of it.
fn deliver_frame(
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
    // The move already shows (before any frame), so another press plans
    // from the new order: UI is first now, a boundary no-op.
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

/// A move that changes an assigned Chat's effective instructions applies at
/// once (no confirmation) and raises an Undo toast; a reorder under the same
/// parent raises none.
#[gpui::test]
fn keyboard_moves_toast_drift_only_when_instructions_change(cx: &mut gpui::TestAppContext) {
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
    assert_eq!(
        move_at(&calls, 0),
        (
            serde_json::Value::Null,
            placement("after", Some(LAUNCH)),
            API.to_owned()
        ),
        "sent without a confirmation"
    );
    let title = api_title();
    assert_eq!(
        toast(&outline, vcx),
        Some(format!(
            "Moved {title} \u{b7} instructions changed for 2 chats (they keep their current instructions)"
        ))
    );
    refresh(&overview, vcx, instruction_fixture::snapshot("s1"));

    // Reordering siblings keeps every effective instruction: no toast.
    outline.update(vcx, |outline, cx| outline.dismiss_move_toast(cx));
    select_item(&outline, vcx, UI);
    press(vcx, "shift-up");
    assert_eq!(toast(&outline, vcx), None);
    assert_eq!(ops(&calls), ["moveWorkItem", "moveWorkItem"]);
}

fn toast(outline: &Entity<OutlineView>, vcx: &mut gpui::VisualTestContext) -> Option<String> {
    outline.read_with(vcx, |outline, _| outline.move_toast_text())
}

fn api_title() -> String {
    instruction_fixture::snapshot("s1")
        .item(API)
        .unwrap()
        .title
        .clone()
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

    // A Chat row selected: Indent and its Tab alias are no-ops, no notice
    // (the Chat moves are covered in `chat_rows_move_between_items_*`).
    outline.update(vcx, |outline, cx| {
        outline.select(&row_key::chat(CHAT_UI), cx)
    });
    press(vcx, "tab");
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

    // A non-item row selected: a notice, no request.
    outline.update(vcx, |outline, cx| {
        outline.select(&row_key::unassociated(Some("s1")), cx)
    });
    run(WorkOutlineCommand::IndentWorkItem, vcx);
    assert_eq!(calls.lock().unwrap().len(), 4);
    assert_eq!(
        outline.read_with(vcx, |outline, _| outline.notice().map(str::to_owned)),
        Some("Select a work item first".to_owned())
    );
}

/// With a Chat row selected the palette commands (and so their rebindable
/// shortcuts) do what the keys do: Move Up / Down re-associate the Chat with
/// the previous / next item, Outdent with its parent, Indent is a no-op.
#[gpui::test]
fn palette_commands_move_the_selected_chat(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = restructure_overview(
        cx,
        dir.path(),
        crate::work_outline::store::fixture_snapshot("s1"),
        reply_ok,
    );
    let run = |command: WorkOutlineCommand, vcx: &mut gpui::VisualTestContext| {
        vcx.update(|window, cx| {
            overview.update(cx, |o, cx| o.run_work_outline_command(command, window, cx))
        });
        vcx.run_until_parked();
    };
    let count = |calls: &OutlineCalls| calls.lock().unwrap().len();
    select_chat(&outline, vcx, CHAT_UI);
    run(WorkOutlineCommand::MoveWorkItemUp, vcx);
    assert_eq!(chat_call_at(&calls, 0), assigned(CHAT_UI, API_AUTH));
    run(WorkOutlineCommand::MoveWorkItemUp, vcx);
    assert_eq!(chat_call_at(&calls, 1), assigned(CHAT_UI, API));
    run(WorkOutlineCommand::MoveWorkItemDown, vcx);
    assert_eq!(chat_call_at(&calls, 2), assigned(CHAT_UI, API_AUTH));
    run(WorkOutlineCommand::OutdentWorkItem, vcx);
    assert_eq!(chat_call_at(&calls, 3), assigned(CHAT_UI, API));
    assert_eq!(selected(&outline, vcx), Some(row_key::chat(CHAT_UI)));
    // Indent does nothing on a Chat, as with Shift+Right: no request, and
    // no "select a work item" refusal.
    run(WorkOutlineCommand::IndentWorkItem, vcx);
    assert_eq!(count(&calls), 4);
    assert!(outline.read_with(vcx, |o, _| o.notice().is_none()));
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
fn drops_toast_drift_when_instructions_change(cx: &mut gpui::TestAppContext) {
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
    assert!(toast(&outline, vcx).is_some(), "toast, not a confirmation");
    assert_eq!(
        move_at(&calls, 0),
        (
            serde_json::Value::Null,
            placement("before", Some(LAUNCH)),
            API.to_owned()
        )
    );
    refresh(&overview, vcx, instruction_fixture::snapshot("s1"));

    // UI before API: same parent, no toast.
    outline.update(vcx, |outline, cx| outline.dismiss_move_toast(cx));
    let at = drag_over(vcx, &item(UI), &item(API), Spot::Upper);
    release(vcx, at);
    assert_eq!(toast(&outline, vcx), None);
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

/// Shift+Arrows through the app's REAL keymap: the bindings come from
/// `shell::apply_keymap` (what the app installs at startup and on every
/// settings change), not the Outline's own test table, and the row is
/// focused and selected by a real click.
#[gpui::test]
fn shift_arrows_restructure_through_the_app_keymap(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let fixture = crate::work_outline::store::fixture_snapshot;
    let (overview, calls, vcx) = instruction_overview(cx, dir.path(), fixture("s1"), reply_ok);
    vcx.update(|_, cx| {
        crate::shell::apply_keymap(
            cx,
            &crate::settings::KeymapConfig::default(),
            crate::settings::ComposerSendBehavior::default(),
        )
    });
    let outline = overview.read_with(vcx, |o, _| o.outline.clone());
    draw(vcx);
    let click = |vcx: &mut gpui::VisualTestContext, item: &str| {
        let at = row_bounds(vcx, format!("outline-row-{}", row_key::item("s1", item)))
            .expect("the row renders")
            .center();
        vcx.simulate_click(at, gpui::Modifiers::default());
        draw(vcx);
    };
    let null = serde_json::Value::Null;

    click(vcx, UI);
    assert_eq!(selected(&outline, vcx), Some(row_key::item("s1", UI)));
    press(vcx, "shift-up");
    assert_eq!(
        move_at(&calls, 0),
        (
            serde_json::json!(LAUNCH),
            placement("before", Some(API)),
            UI.to_owned()
        )
    );
    refresh(&overview, vcx, fixture("s1"));

    click(vcx, API);
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

    click(vcx, UI);
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

    click(vcx, UI);
    press(vcx, "shift-left");
    assert_eq!(
        move_at(&calls, 3),
        (null, placement("after", Some(LAUNCH)), UI.to_owned())
    );
    refresh(&overview, vcx, fixture("s1"));

    // Plain arrows still navigate, F2 / R still rename.
    click(vcx, API);
    press(vcx, "down");
    assert_eq!(selected(&outline, vcx), Some(row_key::item("s1", API_AUTH)));
    press(vcx, "up");
    assert_eq!(selected(&outline, vcx), Some(row_key::item("s1", API)));
    assert_eq!(calls.lock().unwrap().len(), 4);
}

/// [`restructure_overview`] whose engine replies wait for the returned
/// gate: a mutation stays in flight until the test releases it.
fn gated_restructure_overview<'a>(
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
    crate::work_outline::actions::ReplyGate,
    &'a mut gpui::VisualTestContext,
) {
    let (overview, state, vcx) = outline_overview(cx, dir);
    let calls: OutlineCalls = Default::default();
    let sink = calls.clone();
    let gate = crate::work_outline::actions::ReplyGate::default();
    let executor = vcx.update(|_, cx| cx.background_executor().clone());
    state.update(vcx, |state, _| {
        state.work_outline_actions_override =
            Some(crate::work_outline::WorkOutlineActions::scripted_gated(
                std::sync::Arc::new(move |_, params| {
                    sink.lock().unwrap().push(params.clone());
                    reply(&params)
                }),
                gate.clone(),
                executor,
            ));
    });
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(snapshot)), cx);
        o.set_mode(OverviewMode::Outline, cx);
    });
    draw(vcx);
    vcx.update(|_, cx| cx.bind_keys(crate::work_outline::outline_view::key_bindings()));
    let outline = overview.read_with(vcx, |o, _| o.outline.clone());
    focus_outline(&outline, vcx);
    (overview, outline, calls, gate, vcx)
}

fn release_reply(
    gate: &crate::work_outline::actions::ReplyGate,
    vcx: &mut gpui::VisualTestContext,
) {
    assert!(gate.release_one(), "a reply is held");
    vcx.run_until_parked();
    draw(vcx);
}

/// The fixture with three more top-level items after Housekeeping:
/// Launch v2, Housekeeping, Extra 1, Extra 2, Extra 3 (+ archived Retired).
fn long_fixture() -> WorkOutlineSnapshot {
    let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
    let template = snapshot.item(HOUSEKEEPING).unwrap().clone();
    let at = snapshot
        .root_work_item_ids
        .iter()
        .position(|id| id == HOUSEKEEPING)
        .unwrap();
    for n in 1..=3 {
        let mut item = template.clone();
        item.id = format!("w-extra-{n}");
        item.title = format!("Extra {n}");
        snapshot.root_work_item_ids.insert(at + n, item.id.clone());
        snapshot.items.push(item);
    }
    snapshot
}

/// The fixture's top level in `order` (ids of `long_fixture`).
fn with_roots(mut snapshot: WorkOutlineSnapshot, order: &[&str]) -> WorkOutlineSnapshot {
    let rest: Vec<String> = snapshot
        .root_work_item_ids
        .iter()
        .filter(|id| !order.contains(&id.as_str()))
        .cloned()
        .collect();
    snapshot.root_work_item_ids = order.iter().map(|id| (*id).to_owned()).collect();
    snapshot.root_work_item_ids.extend(rest);
    snapshot
}

/// Visible order of `ids` in the Outline (each must render exactly once).
fn shown_order(
    outline: &Entity<OutlineView>,
    vcx: &mut gpui::VisualTestContext,
    ids: &[&str],
) -> Vec<String> {
    outline.read_with(vcx, |outline, _| {
        let model = outline.model();
        let mut placed: Vec<(usize, String)> = ids
            .iter()
            .map(|id| {
                let key = row_key::item("s1", id);
                let count = model.rows.iter().filter(|row| row.key == key).count();
                assert_eq!(count, 1, "{id} renders once");
                (model.position(&key).unwrap(), (*id).to_owned())
            })
            .collect();
        placed.sort();
        placed.into_iter().map(|(_, id)| id).collect()
    })
}

const ROOTS: [&str; 5] = [LAUNCH, HOUSEKEEPING, "w-extra-1", "w-extra-2", "w-extra-3"];

/// A key press moves the row in the same frame (before the engine replies)
/// and the selection stays on it; a failed mutation rolls it back with a
/// notice.
#[gpui::test]
fn a_move_shows_at_once_and_rolls_back_when_it_fails(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, gate, vcx) = gated_restructure_overview(
        cx,
        dir.path(),
        crate::work_outline::store::fixture_snapshot("s1"),
        |_| {
            Err(zeron_rpc::RpcError::Failed(
                "work-outline/conflict: stale".into(),
            ))
        },
    );
    select_item(&outline, vcx, UI);
    press(vcx, "shift-up");
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(gate.held(), 1, "still in flight");
    assert_eq!(
        shown_order(&outline, vcx, &[API, UI]),
        [UI, API],
        "moved now"
    );
    assert_eq!(selected(&outline, vcx), Some(row_key::item("s1", UI)));
    assert!(outline.read_with(vcx, |o, _| o.notice().is_none()));

    release_reply(&gate, vcx);
    assert_eq!(
        shown_order(&outline, vcx, &[API, UI]),
        [API, UI],
        "rolled back"
    );
    assert_eq!(selected(&outline, vcx), Some(row_key::item("s1", UI)));
    assert!(outline.read_with(vcx, |o, _| o.notice().is_some()));
    assert_eq!(outline.read_with(vcx, |o, _| o.optimistic_len()), 0);
    assert!(
        !crate::overview_positions::path(dir.path()).exists(),
        "canvas positions untouched"
    );
}

/// Rapid Shift+Down presses while the first move is in flight all apply,
/// in order: each queued press plans from the order the previous one
/// already shows, and the selection rides along.
#[gpui::test]
fn rapid_shift_down_presses_all_apply_in_order(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, gate, vcx) =
        gated_restructure_overview(cx, dir.path(), long_fixture(), reply_ok);
    select_item(&outline, vcx, LAUNCH);
    press(vcx, "shift-down");
    press(vcx, "shift-down");
    press(vcx, "shift-down");
    assert_eq!(calls.lock().unwrap().len(), 1, "one in flight, two queued");
    assert_eq!(
        shown_order(&outline, vcx, &ROOTS),
        [HOUSEKEEPING, LAUNCH, "w-extra-1", "w-extra-2", "w-extra-3"]
    );

    release_reply(&gate, vcx);
    assert_eq!(calls.lock().unwrap().len(), 2);
    release_reply(&gate, vcx);
    assert_eq!(calls.lock().unwrap().len(), 3);
    release_reply(&gate, vcx);
    assert_eq!(gate.held(), 0);
    let null = serde_json::Value::Null;
    for (ix, after) in [HOUSEKEEPING, "w-extra-1", "w-extra-2"]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            move_at(&calls, ix),
            (
                null.clone(),
                placement("after", Some(after)),
                LAUNCH.to_owned()
            )
        );
    }
    assert_eq!(
        shown_order(&outline, vcx, &ROOTS),
        [HOUSEKEEPING, "w-extra-1", "w-extra-2", LAUNCH, "w-extra-3"]
    );
    assert_eq!(selected(&outline, vcx), Some(row_key::item("s1", LAUNCH)));
    // Its subtree travelled with it.
    let (launch, api) = outline.read_with(vcx, |o, _| {
        let model = o.model();
        (
            model.position(&row_key::item("s1", LAUNCH)).unwrap(),
            model.position(&row_key::item("s1", API)).unwrap(),
        )
    });
    assert_eq!(api, launch + 1);
}

/// A reply that never lands does not wedge restructuring: after
/// `DIRECT_REPLY_TIMEOUT` the attempt counts as failed (rolled back, with a
/// notice) and the next Shift+Arrow goes out, replaying the operation id
/// when it is the same move. A late reply to the expired attempt does not
/// drain presses queued behind the new one.
#[gpui::test]
fn a_reply_that_never_lands_does_not_wedge_restructuring(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, gate, vcx) =
        gated_restructure_overview(cx, dir.path(), long_fixture(), reply_ok);
    select_item(&outline, vcx, HOUSEKEEPING);
    press(vcx, "shift-up");
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(
        shown_order(&outline, vcx, &[LAUNCH, HOUSEKEEPING]),
        [HOUSEKEEPING, LAUNCH],
        "moved now"
    );
    // Presses while in flight queue.
    press(vcx, "shift-down");
    assert_eq!(calls.lock().unwrap().len(), 1, "queued behind the first");

    // The engine never answers.
    vcx.executor()
        .advance_clock(crate::work_outline::outline_view::DIRECT_REPLY_TIMEOUT);
    vcx.run_until_parked();
    draw(vcx);
    assert!(!outline.read_with(vcx, |o, _| o.restructuring_busy()));
    assert_eq!(
        shown_order(&outline, vcx, &[LAUNCH, HOUSEKEEPING]),
        [LAUNCH, HOUSEKEEPING],
        "rolled back"
    );
    assert!(outline.read_with(vcx, |o, _| o.notice().is_some()));
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "the queued press was dropped"
    );

    // The next press goes out: the same move replays its operation id.
    press(vcx, "shift-up");
    assert_eq!(calls.lock().unwrap().len(), 2);
    let op = |ix: usize| calls.lock().unwrap()[ix]["operationId"].clone();
    assert_eq!(op(0), op(1), "a retry replays the operation id");
    assert_eq!(
        move_at(&calls, 1),
        (
            serde_json::Value::Null,
            placement("before", Some(LAUNCH)),
            HOUSEKEEPING.to_owned()
        )
    );
    release_reply(&gate, vcx);
    release_reply(&gate, vcx);
    assert!(!outline.read_with(vcx, |o, _| o.restructuring_busy()));

    // A different move expires, then another goes out with a press queued
    // behind it: the expired move's late reply leaves that press queued.
    press(vcx, "shift-down");
    assert_eq!(calls.lock().unwrap().len(), 3);
    vcx.executor()
        .advance_clock(crate::work_outline::outline_view::DIRECT_REPLY_TIMEOUT);
    vcx.run_until_parked();
    draw(vcx);
    assert!(!outline.read_with(vcx, |o, _| o.restructuring_busy()));
    press(vcx, "shift-down");
    assert_eq!(calls.lock().unwrap().len(), 4);
    assert_ne!(op(2), op(3), "a different move mints a new id");
    press(vcx, "shift-up");
    release_reply(&gate, vcx);
    assert!(outline.read_with(vcx, |o, _| o.restructuring_busy()));
    assert_eq!(calls.lock().unwrap().len(), 4, "still queued");
    release_reply(&gate, vcx);
    assert_eq!(calls.lock().unwrap().len(), 5, "the queued press went out");
}

/// Engine frames reconcile the optimistic order: a stale frame (showing only
/// the first of three moves) does not flicker the row back, an intermediate
/// one neither, and the frame showing the last move retires every pending
/// change without duplicating a row.
#[gpui::test]
fn frames_reconcile_optimistic_moves_without_flicker(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) =
        restructure_overview(cx, dir.path(), long_fixture(), reply_ok);
    select_item(&outline, vcx, LAUNCH);
    for _ in 0..3 {
        press(vcx, "shift-down");
    }
    assert_eq!(calls.lock().unwrap().len(), 3);
    let last = [HOUSEKEEPING, "w-extra-1", "w-extra-2", LAUNCH, "w-extra-3"];
    assert_eq!(shown_order(&outline, vcx, &ROOTS), last);
    assert_eq!(outline.read_with(vcx, |o, _| o.optimistic_len()), 3);

    // The engine's frames trail behind: after the first move, then the second.
    deliver_frame(
        &overview,
        vcx,
        with_roots(long_fixture(), &[HOUSEKEEPING, LAUNCH]),
    );
    assert_eq!(shown_order(&outline, vcx, &ROOTS), last, "no flicker");
    deliver_frame(
        &overview,
        vcx,
        with_roots(long_fixture(), &[HOUSEKEEPING, "w-extra-1", LAUNCH]),
    );
    assert_eq!(shown_order(&outline, vcx, &ROOTS), last, "no flicker");
    assert_eq!(outline.read_with(vcx, |o, _| o.optimistic_len()), 1);

    deliver_frame(&overview, vcx, with_roots(long_fixture(), &last));
    assert_eq!(shown_order(&outline, vcx, &ROOTS), last);
    assert_eq!(outline.read_with(vcx, |o, _| o.optimistic_len()), 0);
    assert_eq!(selected(&outline, vcx), Some(row_key::item("s1", LAUNCH)));
}

/// The move picker (the context menu's "Move To…") takes the optimistic
/// path too: the item shows under its new parent and the picker closes
/// before the engine replies; a failure moves it back with a notice.
#[gpui::test]
fn a_picker_move_shows_at_once_and_rolls_back_when_it_fails(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, gate, vcx) = gated_restructure_overview(
        cx,
        dir.path(),
        crate::work_outline::store::fixture_snapshot("s1"),
        |_| {
            Err(zeron_rpc::RpcError::Failed(
                "work-outline/conflict: stale".into(),
            ))
        },
    );
    select_item(&outline, vcx, HOUSEKEEPING);
    outline.update_in(vcx, |outline, window, cx| {
        outline.open_move_picker(window, cx).unwrap()
    });
    draw(vcx);
    // Entry 1 is Launch v2 (entry 0 is the top level).
    outline.update_in(vcx, |outline, window, cx| {
        outline.choose_picker_entry(1, window, cx)
    });
    vcx.run_until_parked();
    draw(vcx);
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(gate.held(), 1, "still in flight");
    let parent = |vcx: &mut gpui::VisualTestContext| {
        outline.read_with(vcx, |o, _| {
            let model = o.model();
            let row = model.row(&row_key::item("s1", HOUSEKEEPING)).unwrap();
            row.depth
        })
    };
    assert_eq!(parent(vcx), 1, "under Launch v2 now");
    assert!(!editing(&outline, vcx), "the picker closed");
    assert_eq!(
        selected(&outline, vcx),
        Some(row_key::item("s1", HOUSEKEEPING))
    );

    release_reply(&gate, vcx);
    assert_eq!(parent(vcx), 0, "rolled back to the top level");
    assert!(outline.read_with(vcx, |o, _| o.notice().is_some()));
    assert_eq!(outline.read_with(vcx, |o, _| o.optimistic_len()), 0);
}

// --------------------------------------------------------------------------
// Chat rows: Shift+Arrows re-associate the selected Chat
// --------------------------------------------------------------------------

/// `(op, chatId, workItemId)` of the recorded call `ix`.
fn chat_call_at(calls: &OutlineCalls, ix: usize) -> (String, String, serde_json::Value) {
    let call = calls.lock().unwrap()[ix].clone();
    (
        call["op"].as_str().unwrap_or_default().to_owned(),
        call["chatId"].as_str().unwrap_or_default().to_owned(),
        call["workItemId"].clone(),
    )
}

fn assigned(chat: &str, item: &str) -> (String, String, serde_json::Value) {
    (
        "assignChatToWorkItem".into(),
        chat.into(),
        serde_json::json!(item),
    )
}

/// The row key the Chat's row sits under.
fn chat_parent(
    outline: &Entity<OutlineView>,
    vcx: &mut gpui::VisualTestContext,
    chat: &str,
) -> Option<String> {
    outline.read_with(vcx, |outline, _| {
        let model = outline.model();
        let ix = model.position(&row_key::chat(chat))?;
        model.rows[ix]
            .parent
            .map(|parent| model.rows[parent].key.clone())
    })
}

fn select_chat(outline: &Entity<OutlineView>, vcx: &mut gpui::VisualTestContext, chat: &str) {
    outline.update(vcx, |outline, cx| outline.select(&row_key::chat(chat), cx));
    draw(vcx);
}

/// Shift+Up / Shift+Down move a Chat to the previous / next work item in
/// visual row order (any depth, stepping over Chat rows and its own item),
/// Shift+Left to its item's parent and, from a top-level item, out to
/// Unassociated Chats; the ends, Shift+Right and Tab are no-ops. The
/// selection rides along with the Chat.
#[gpui::test]
fn chat_rows_move_between_items_in_visual_order(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, vcx) = restructure_overview(
        cx,
        dir.path(),
        crate::work_outline::store::fixture_snapshot("s1"),
        reply_ok,
    );
    let item = |id: &str| Some(row_key::item("s1", id));
    let count = |calls: &OutlineCalls| calls.lock().unwrap().len();

    // UI's Chat: up to Auth endpoints (deeper, above UI, past chat rows).
    select_chat(&outline, vcx, CHAT_UI);
    press(vcx, "shift-up");
    assert_eq!(chat_call_at(&calls, 0), assigned(CHAT_UI, API_AUTH));
    assert_eq!(
        chat_parent(&outline, vcx, CHAT_UI),
        item(API_AUTH),
        "shown at once"
    );
    assert_eq!(selected(&outline, vcx), Some(row_key::chat(CHAT_UI)));
    assert_eq!(toast(&outline, vcx), None);
    // Then API, then Launch v2; nothing above the first item.
    press(vcx, "shift-up");
    assert_eq!(chat_call_at(&calls, 1), assigned(CHAT_UI, API));
    press(vcx, "shift-up");
    assert_eq!(chat_call_at(&calls, 2), assigned(CHAT_UI, LAUNCH));
    press(vcx, "shift-up");
    assert_eq!(count(&calls), 3, "the first item is the top end");

    // Down to its child API, then Shift+Left back to the parent.
    press(vcx, "shift-down");
    assert_eq!(chat_call_at(&calls, 3), assigned(CHAT_UI, API));
    press(vcx, "shift-left");
    assert_eq!(chat_call_at(&calls, 4), assigned(CHAT_UI, LAUNCH));
    // From a top-level item: out of its item.
    press(vcx, "shift-left");
    let (op, chat, _) = chat_call_at(&calls, 5);
    assert_eq!(
        (op.as_str(), chat.as_str()),
        ("removeChatFromWorkItem", CHAT_UI)
    );
    assert_eq!(
        chat_parent(&outline, vcx, CHAT_UI),
        Some(row_key::unassociated(Some("s1")))
    );
    assert_eq!(selected(&outline, vcx), Some(row_key::chat(CHAT_UI)));
    // Unassociated: Shift+Left / Shift+Tab / Shift+Down do nothing.
    for keys in [
        "shift-left",
        "shift-tab",
        "shift-down",
        "shift-right",
        "tab",
    ] {
        press(vcx, keys);
        assert_eq!(count(&calls), 6, "{keys} is a no-op");
    }
    // Shift+Up: the Space's last shown work item.
    press(vcx, "shift-up");
    assert_eq!(chat_call_at(&calls, 6), assigned(CHAT_UI, HOUSEKEEPING));
    // The last item is the bottom end (archived Retired is never a target).
    press(vcx, "shift-down");
    press(vcx, "shift-right");
    press(vcx, "tab");
    assert_eq!(count(&calls), 7);
    // Shift+Tab is the Shift+Left alias.
    press(vcx, "shift-tab");
    let (op, _, _) = chat_call_at(&calls, 7);
    assert_eq!(op, "removeChatFromWorkItem");
    assert!(outline.read_with(vcx, |o, _| o.notice().is_none()));
    assert!(
        !crate::overview_positions::path(dir.path()).exists(),
        "canvas positions untouched"
    );
}

/// A Chat move that changes its effective instructions applies at once with
/// an Undo toast, like a drop or the picker.
#[gpui::test]
fn chat_moves_toast_drift_when_instructions_change(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, vcx) = restructure_overview(
        cx,
        dir.path(),
        instruction_fixture::snapshot("s1"),
        reply_ok,
    );
    // UI (root + Launch) to Auth endpoints (+ API + Auth).
    select_chat(&outline, vcx, CHAT_UI);
    press(vcx, "shift-up");
    assert_eq!(chat_call_at(&calls, 0), assigned(CHAT_UI, API_AUTH));
    let text = toast(&outline, vcx).expect("toast");
    assert!(
        text.ends_with("instructions changed for 1 chat (it keeps its current instructions)"),
        "{text}"
    );
}

/// Rapid presses while the first re-association is in flight all apply, in
/// order, each planned from where the previous one already shows the Chat.
#[gpui::test]
fn rapid_chat_moves_apply_in_order(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, gate, vcx) = gated_restructure_overview(
        cx,
        dir.path(),
        crate::work_outline::store::fixture_snapshot("s1"),
        reply_ok,
    );
    select_chat(&outline, vcx, CHAT_UI);
    press(vcx, "shift-up");
    press(vcx, "shift-up");
    press(vcx, "shift-left");
    assert_eq!(calls.lock().unwrap().len(), 1, "one in flight, two queued");
    release_reply(&gate, vcx);
    release_reply(&gate, vcx);
    release_reply(&gate, vcx);
    assert_eq!(gate.held(), 0);
    assert_eq!(chat_call_at(&calls, 0), assigned(CHAT_UI, API_AUTH));
    assert_eq!(chat_call_at(&calls, 1), assigned(CHAT_UI, API));
    assert_eq!(chat_call_at(&calls, 2), assigned(CHAT_UI, LAUNCH));
    assert_eq!(
        chat_parent(&outline, vcx, CHAT_UI),
        Some(row_key::item("s1", LAUNCH))
    );
    assert_eq!(selected(&outline, vcx), Some(row_key::chat(CHAT_UI)));
}

/// A failed re-association rolls the Chat back with a notice; pressing the
/// same key again replays its operation id.
#[gpui::test]
fn a_failed_chat_move_rolls_back_and_retries_with_its_operation_id(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, gate, vcx) = gated_restructure_overview(
        cx,
        dir.path(),
        crate::work_outline::store::fixture_snapshot("s1"),
        |_| {
            Err(zeron_rpc::RpcError::Failed(
                "work-outline/conflict: stale".into(),
            ))
        },
    );
    select_chat(&outline, vcx, CHAT_UI);
    press(vcx, "shift-down");
    assert_eq!(
        chat_parent(&outline, vcx, CHAT_UI),
        Some(row_key::item("s1", HOUSEKEEPING))
    );
    release_reply(&gate, vcx);
    assert_eq!(
        chat_parent(&outline, vcx, CHAT_UI),
        Some(row_key::item("s1", UI)),
        "rolled back"
    );
    assert!(outline.read_with(vcx, |o, _| o.notice().is_some()));
    press(vcx, "shift-down");
    release_reply(&gate, vcx);
    let ids: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| call["operationId"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], ids[1], "the retry replays the operation id");
}

/// The Chat moves are inert while a text field or the picker owns focus, and
/// a dangling Chat row leaves a notice instead of moving.
#[gpui::test]
fn chat_moves_are_inert_in_text_inputs_and_refused_for_dangling_chats(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, vcx) = restructure_overview(
        cx,
        dir.path(),
        crate::work_outline::store::fixture_snapshot("s1"),
        reply_ok,
    );
    select_chat(&outline, vcx, CHAT_UI);
    outline.update_in(vcx, |outline, window, cx| {
        outline.open_associate_picker(None, window, cx).unwrap()
    });
    draw(vcx);
    for keys in ["shift-up", "shift-down", "shift-left", "shift-tab"] {
        press(vcx, keys);
    }
    assert!(calls.lock().unwrap().is_empty(), "the picker owns the keys");
    outline.update_in(vcx, |outline, window, cx| outline.close_picker(window, cx));
    draw(vcx);

    // Inline rename of a work item: its editor owns the keys.
    select_item(&outline, vcx, UI);
    outline.update_in(vcx, |outline, window, cx| {
        outline.begin_rename(window, cx).unwrap()
    });
    draw(vcx);
    for keys in ["shift-up", "shift-down", "shift-left", "shift-tab"] {
        press(vcx, keys);
    }
    assert!(calls.lock().unwrap().is_empty(), "the editor owns the keys");
    outline.update(vcx, |outline, cx| outline.cancel_edit(cx));
    focus_outline(&outline, vcx);

    // chat-ghost: its assignment survives but the Chat is gone.
    select_chat(&outline, vcx, CHAT_GHOST);
    press(vcx, "shift-down");
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(
        outline.read_with(vcx, |o, _| o.notice().map(str::to_owned)),
        Some("This chat no longer exists on any device".to_owned())
    );
}

/// Chat moves stay inside the Chat's Space: the next Space's items are
/// never targets, and a projectless Chat leaves a notice.
#[gpui::test]
fn chat_moves_never_cross_spaces(cx: &mut gpui::TestAppContext) {
    use crate::work_outline::model::tests::{chat, fixture_chats, ready};
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
    });
    let calls: OutlineCalls = Default::default();
    let sink = calls.clone();
    let state = cx.new(|_| AppState::new());
    state.update(cx, |state, _| {
        state.work_outline_actions_override =
            Some(crate::work_outline::WorkOutlineActions::scripted(
                std::sync::Arc::new(move |_, params| {
                    sink.lock().unwrap().push(params.clone());
                    reply_ok(&params)
                }),
            ));
    });
    let (outline, vcx) = cx.add_window_view(|_, cx| OutlineView::new(state.clone(), cx));
    vcx.update(|_, cx| cx.bind_keys(crate::work_outline::outline_view::key_bindings()));
    // Space s2: the same tree, no Chats of its own but one unassociated.
    let mut s2 = crate::work_outline::store::fixture_snapshot("s2");
    for item in &mut s2.items {
        item.chat_ids.clear();
    }
    s2.assignments.clear();
    s2.unassociated_chat_ids = vec!["chat-b".into()];
    let mut chats = fixture_chats("s1");
    chats.push(chat("chat-b", Some("s2")));
    chats.push(chat("chat-nowhere", None));
    outline.update(vcx, |outline, cx| {
        outline.set_inputs(
            crate::work_outline::outline_view::OutlineInputs {
                spaces: vec![
                    ready(
                        "s1",
                        "Alpha",
                        crate::work_outline::store::fixture_snapshot("s1"),
                    ),
                    ready("s2", "Beta", s2),
                ],
                chats,
                options: crate::work_outline::model::OutlineOptions {
                    all_spaces: true,
                    ..Default::default()
                },
            },
            cx,
        )
    });
    focus_outline(&outline, vcx);

    // s1's last item is followed by s2's rows: still the bottom end.
    select_chat(&outline, vcx, CHAT_UI);
    press(vcx, "shift-down");
    assert_eq!(chat_call_at(&calls, 0), assigned(CHAT_UI, HOUSEKEEPING));
    assert_eq!(
        chat_parent(&outline, vcx, CHAT_UI),
        Some(row_key::item("s1", HOUSEKEEPING))
    );
    press(vcx, "shift-down");
    assert_eq!(calls.lock().unwrap().len(), 1, "never into s2");

    // s2's unassociated Chat goes up to s2's own last item.
    select_chat(&outline, vcx, "chat-b");
    press(vcx, "shift-up");
    assert_eq!(chat_call_at(&calls, 1), assigned("chat-b", HOUSEKEEPING));
    assert_eq!(
        chat_parent(&outline, vcx, "chat-b"),
        Some(row_key::item("s2", HOUSEKEEPING))
    );

    // Projectless: nothing to move into.
    select_chat(&outline, vcx, "chat-nowhere");
    press(vcx, "shift-up");
    assert_eq!(calls.lock().unwrap().len(), 2);
    assert_eq!(
        outline.read_with(vcx, |o, _| o.notice().map(str::to_owned)),
        Some("Chats without a project cannot be associated".to_owned())
    );
}

// --------------------------------------------------------------------------
// The Undo toast for moves that change instructions
// --------------------------------------------------------------------------

fn ui_title() -> String {
    instruction_fixture::snapshot("s1")
        .item(UI)
        .unwrap()
        .title
        .clone()
}

/// Undo (the toast's button) sends the inverse move with the previous parent
/// and placement under a new operation id, and raises no toast of its own.
#[gpui::test]
fn undo_restores_a_work_items_parent_and_placement(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = restructure_overview(
        cx,
        dir.path(),
        instruction_fixture::snapshot("s1"),
        reply_ok,
    );
    // API is Launch v2's first child: undo puts it back First.
    select_item(&outline, vcx, API);
    press(vcx, "shift-tab");
    assert!(toast(&outline, vcx).is_some());
    draw(vcx);
    let undo = vcx
        .debug_bounds("outline-move-toast-undo")
        .expect("Undo shown");
    vcx.simulate_click(undo.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        move_at(&calls, 1),
        (
            serde_json::json!(LAUNCH),
            placement("first", None),
            API.to_owned()
        )
    );
    let ids: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| call["operationId"].as_str().unwrap().to_owned())
        .collect();
    assert_ne!(ids[0], ids[1], "Undo is a new operation");
    assert_eq!(toast(&outline, vcx), None);
    refresh(&overview, vcx, instruction_fixture::snapshot("s1"));

    // UI follows API: undo puts it back after API.
    select_item(&outline, vcx, UI);
    press(vcx, "shift-tab");
    assert!(toast(&outline, vcx).is_some());
    outline.update(vcx, |outline, cx| outline.undo_move(cx));
    vcx.run_until_parked();
    assert_eq!(
        move_at(&calls, 3),
        (
            serde_json::json!(LAUNCH),
            placement("after", Some(API)),
            UI.to_owned()
        )
    );
    assert_eq!(ops(&calls).len(), 4);
}

/// Undo of a Chat move restores its previous assignment, or removes the
/// association when it had none.
#[gpui::test]
fn undo_restores_a_chats_assignment(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = restructure_overview(
        cx,
        dir.path(),
        instruction_fixture::snapshot("s1"),
        reply_ok,
    );
    select_chat(&outline, vcx, CHAT_UI);
    press(vcx, "shift-up");
    assert_eq!(chat_call_at(&calls, 0), assigned(CHAT_UI, API_AUTH));
    assert!(toast(&outline, vcx).is_some());
    outline.update(vcx, |outline, cx| outline.undo_move(cx));
    vcx.run_until_parked();
    assert_eq!(chat_call_at(&calls, 1), assigned(CHAT_UI, UI));
    assert_eq!(toast(&outline, vcx), None);
    refresh(&overview, vcx, instruction_fixture::snapshot("s1"));

    // A loose Chat gains instructions from an item; undo unassociates it.
    select_chat(&outline, vcx, CHAT_LOOSE_A);
    press(vcx, "shift-up");
    let (op, chat, _) = chat_call_at(&calls, 2);
    assert_eq!(
        (op.as_str(), chat.as_str()),
        ("assignChatToWorkItem", CHAT_LOOSE_A)
    );
    assert!(toast(&outline, vcx).is_some());
    outline.update(vcx, |outline, cx| outline.undo_move(cx));
    vcx.run_until_parked();
    let (op, chat, _) = chat_call_at(&calls, 3);
    assert_eq!(
        (op.as_str(), chat.as_str()),
        ("removeChatFromWorkItem", CHAT_LOOSE_A)
    );
}

/// Rapid moves share one toast whose counts grow; Undo reverts the most
/// recent move only.
#[gpui::test]
fn rapid_moves_coalesce_into_one_toast_and_undo_reverts_the_last(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = restructure_overview(
        cx,
        dir.path(),
        instruction_fixture::snapshot("s1"),
        reply_ok,
    );
    select_item(&outline, vcx, API);
    press(vcx, "shift-tab");
    assert!(toast(&outline, vcx).unwrap().contains("for 2 chats"));
    refresh(&overview, vcx, instruction_fixture::snapshot("s1"));
    select_item(&outline, vcx, UI);
    press(vcx, "shift-tab");
    assert_eq!(
        toast(&outline, vcx),
        Some(format!(
            "Moved {} and 1 more \u{b7} instructions changed for 3 chats (they keep their current instructions)",
            ui_title()
        ))
    );
    outline.update(vcx, |outline, cx| outline.undo_move(cx));
    vcx.run_until_parked();
    assert_eq!(ops(&calls).len(), 3);
    assert_eq!(
        move_at(&calls, 2),
        (
            serde_json::json!(LAUNCH),
            placement("after", Some(API)),
            UI.to_owned()
        ),
        "only the latest move is undone"
    );
    assert_eq!(toast(&outline, vcx), None);
}

/// A move that changes no assigned Chat's instructions shows no toast.
#[gpui::test]
fn a_move_without_drift_shows_no_toast(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, vcx) = restructure_overview(
        cx,
        dir.path(),
        crate::work_outline::store::fixture_snapshot("s1"),
        reply_ok,
    );
    select_item(&outline, vcx, API);
    press(vcx, "shift-tab");
    assert_eq!(ops(&calls), ["moveWorkItem"]);
    assert_eq!(toast(&outline, vcx), None);
    draw(vcx);
    assert!(vcx.debug_bounds("outline-move-toast").is_none());
}

/// The toast never takes focus: keys keep working in the tree while it is
/// shown. Esc and the close button dismiss it, and it dismisses itself.
#[gpui::test]
fn the_toast_keeps_focus_in_the_tree_and_dismisses(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = restructure_overview(
        cx,
        dir.path(),
        instruction_fixture::snapshot("s1"),
        reply_ok,
    );
    select_item(&outline, vcx, API);
    press(vcx, "shift-tab");
    assert!(toast(&outline, vcx).is_some());
    draw(vcx);
    assert!(vcx.debug_bounds("outline-move-toast").is_some());
    let focused = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|window, cx| {
            gpui::Focusable::focus_handle(outline.read(cx), cx).is_focused(window)
        })
    };
    assert!(focused(vcx));
    let before = selected(&outline, vcx);
    press(vcx, "down");
    assert_ne!(selected(&outline, vcx), before, "arrow keys still navigate");
    assert!(toast(&outline, vcx).is_some(), "navigation keeps the toast");
    refresh(&overview, vcx, instruction_fixture::snapshot("s1"));
    assert!(toast(&outline, vcx).is_some(), "a frame keeps the toast");
    select_item(&outline, vcx, UI);
    press(vcx, "shift-up");
    assert_eq!(ops(&calls).len(), 2, "restructuring keys still work");
    assert!(focused(vcx));

    press(vcx, "escape");
    assert_eq!(toast(&outline, vcx), None, "Esc dismisses");
    assert!(focused(vcx));

    // The close button.
    refresh(&overview, vcx, instruction_fixture::snapshot("s1"));
    select_item(&outline, vcx, UI);
    press(vcx, "shift-tab");
    assert!(toast(&outline, vcx).is_some());
    draw(vcx);
    let close = vcx.debug_bounds("outline-move-toast-close").unwrap();
    vcx.simulate_click(close.center(), gpui::Modifiers::default());
    assert_eq!(toast(&outline, vcx), None);

    // Auto-dismiss after about six seconds.
    refresh(&overview, vcx, instruction_fixture::snapshot("s1"));
    select_item(&outline, vcx, API);
    press(vcx, "shift-tab");
    assert!(toast(&outline, vcx).is_some());
    vcx.executor()
        .advance_clock(std::time::Duration::from_secs(5));
    vcx.run_until_parked();
    assert!(toast(&outline, vcx).is_some(), "still shown at 5 s");
    vcx.executor()
        .advance_clock(std::time::Duration::from_secs(2));
    vcx.run_until_parked();
    assert_eq!(toast(&outline, vcx), None);
}

/// A move the engine rejects rolls back and takes its toast down.
#[gpui::test]
fn a_failed_move_dismisses_its_toast(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, _calls, vcx) =
        restructure_overview(cx, dir.path(), instruction_fixture::snapshot("s1"), |_| {
            Err(zeron_rpc::RpcError::Failed("host unreachable".into()))
        });
    select_item(&outline, vcx, API);
    press(vcx, "shift-tab");
    assert_eq!(toast(&outline, vcx), None);
    assert!(outline.read_with(vcx, |outline, _| outline.notice().is_some()));
}
