// Compact single-chat Outline rows: one row for an item with exactly one live
// Chat. Included from `overview.rs`'s test module so it shares its Overview
// fixtures.
use super::*;

use crate::work_outline::outline_view::context_menu::MenuAction;
use crate::work_outline::outline_view::OutlineView;

type Events = std::sync::Arc<std::sync::Mutex<Vec<OutlineEvent>>>;

/// A focused Outline over `snapshot` (compact rows on, as in the app) that
/// records the events it emits.
fn compact_overview<'a>(
    cx: &'a mut gpui::TestAppContext,
    dir: &std::path::Path,
    snapshot: WorkOutlineSnapshot,
) -> (
    Entity<Overview>,
    Entity<OutlineView>,
    OutlineCalls,
    Events,
    &'a mut gpui::VisualTestContext,
) {
    let (overview, calls, vcx) = instruction_overview(cx, dir, snapshot, reply_ok);
    vcx.update(|_, cx| cx.bind_keys(crate::work_outline::outline_view::key_bindings()));
    let outline = overview.read_with(vcx, |o, _| o.outline.clone());
    let events: Events = Default::default();
    let sink = events.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&outline, move |_, event: &OutlineEvent, _| {
            sink.lock().unwrap().push(event.clone())
        })
        .detach()
    });
    vcx.update(|window, cx| {
        let handle = gpui::Focusable::focus_handle(outline.read(cx), cx);
        window.focus(&handle, cx);
    });
    draw(vcx);
    (overview, outline, calls, events, vcx)
}

fn item_key(id: &str) -> String {
    row_key::item("s1", id)
}

fn selected_key(
    outline: &Entity<OutlineView>,
    vcx: &mut gpui::VisualTestContext,
) -> Option<String> {
    outline.read_with(vcx, |outline, _| outline.selected_key().map(str::to_owned))
}

fn select_key(outline: &Entity<OutlineView>, vcx: &mut gpui::VisualTestContext, key: &str) {
    outline.update(vcx, |outline, cx| outline.select(key, cx));
    draw(vcx);
}

fn press_keys(vcx: &mut gpui::VisualTestContext, keys: &str) {
    vcx.simulate_keystrokes(keys);
    vcx.run_until_parked();
}

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

fn shown(vcx: &mut gpui::VisualTestContext, key: &str) -> bool {
    row_bounds(vcx, format!("outline-row-{key}")).is_some()
}

#[gpui::test]
fn a_single_chat_item_renders_one_row_with_the_chat_state(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, _outline, _calls, _events, vcx) =
        compact_overview(cx, dir.path(), crate::work_outline::store::fixture_snapshot("s1"));
    // w-ui and w-api-auth each fold their only Chat.
    for (item, chat) in [(UI, CHAT_UI), (API_AUTH, CHAT_AUTH)] {
        assert!(shown(vcx, &item_key(item)), "{item} row");
        assert!(!shown(vcx, &row_key::chat(chat)), "{chat} has no row");
        assert!(
            row_bounds(vcx, format!("outline-chat-dot-{}", item_key(item))).is_some(),
            "{item} shows the Chat's state dot"
        );
        // The row keeps the shared height.
        let bounds = row_bounds(vcx, format!("outline-row-{}", item_key(item))).unwrap();
        assert!(f32::from(bounds.size.height) <= 28.0);
        // No separate chats-count chip: the one Chat is the row.
        assert!(row_bounds(vcx, format!("outline-chats-{}", item_key(item))).is_none());
    }
    // An item with two chats keeps item + Chat rows and no dot of its own.
    assert!(shown(vcx, &row_key::chat(CHAT_API)));
    assert!(row_bounds(vcx, format!("outline-chat-dot-{}", item_key(API))).is_none());
}

#[gpui::test]
fn enter_and_double_click_open_the_chat_and_the_menu_reaches_the_item(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, _calls, events, vcx) =
        compact_overview(cx, dir.path(), crate::work_outline::store::fixture_snapshot("s1"));
    select_key(&outline, vcx, &item_key(UI));
    press_keys(vcx, "enter");
    assert!(
        events
            .lock()
            .unwrap()
            .contains(&OutlineEvent::OpenChat(CHAT_UI.into()))
    );
    // An item with no single chat still opens its details.
    events.lock().unwrap().clear();
    select_key(&outline, vcx, &item_key(LAUNCH));
    press_keys(vcx, "enter");
    assert!(events.lock().unwrap().iter().any(|e| matches!(
        e,
        OutlineEvent::OpenDetails { item_id, .. } if item_id == LAUNCH
    )));

    // A single click selects (and previews the Chat); a double click opens it.
    events.lock().unwrap().clear();
    let at = row_bounds(vcx, format!("outline-row-{}", item_key(API_AUTH)))
        .unwrap()
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(selected_key(&outline, vcx), Some(item_key(API_AUTH)));
    assert!(
        outline
            .read_with(vcx, |o, _| o.selected_open_chat())
            .as_deref()
            == Some(CHAT_AUTH)
    );

    // The item's actions live in the context menu.
    let menu = outline.read_with(vcx, |o, cx| o.context_menu(&item_key(UI), cx)).unwrap();
    let ids: Vec<_> = menu.entries.iter().map(|entry| entry.id).collect();
    for id in [
        "open-chat",
        "open-details",
        "start-chat",
        "rename",
        "set-status",
        "add-instruction",
        "archive",
        "archive-chat",
    ] {
        assert!(ids.contains(&id), "{id} in {ids:?}");
    }
    let action = |id: &str| menu.entries.iter().find(|e| e.id == id).unwrap().action.clone();
    assert_eq!(action("open-details"), MenuAction::OpenDetails);
    assert_eq!(action("archive-chat"), MenuAction::ArchiveChat(CHAT_UI.into()));
    assert_eq!(action("archive"), MenuAction::Archive);
    // Expanded items keep the plain item menu.
    let plain = outline.read_with(vcx, |o, cx| o.context_menu(&item_key(API), cx)).unwrap();
    assert!(plain.entries.iter().all(|e| e.id != "open-chat"));
    assert_eq!(
        plain.entries.iter().find(|e| e.id == "open-details").unwrap().action,
        MenuAction::Outline(crate::shell::work_outline_ui::OutlineNavKey::OpenSelected)
    );
}

#[gpui::test]
fn item_actions_still_act_on_the_item(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, _calls, events, vcx) =
        compact_overview(cx, dir.path(), crate::work_outline::store::fixture_snapshot("s1"));
    select_key(&outline, vcx, &item_key(UI));
    vcx.dispatch_action(crate::work_outline::outline_view::StartChatSelected);
    vcx.run_until_parked();
    assert!(events.lock().unwrap().iter().any(|e| matches!(
        e,
        OutlineEvent::StartChat { item_id, .. } if item_id == UI
    )));
    // Rename edits the item's title (the row's text), not the Chat's.
    events.lock().unwrap().clear();
    press_keys(vcx, "f2");
    assert!(
        !events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, OutlineEvent::RenameChat(_)))
    );
    assert!(outline.read_with(vcx, |o, _| o.is_editing()));
}

#[gpui::test]
fn keyboard_navigation_treats_the_compact_row_as_one_row(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, _calls, _events, vcx) =
        compact_overview(cx, dir.path(), crate::work_outline::store::fixture_snapshot("s1"));
    select_key(&outline, vcx, &item_key(API_AUTH));
    press_keys(vcx, "down");
    // The next row is w-api's Chat, not a Chat row under w-api-auth.
    let after = selected_key(&outline, vcx).unwrap();
    assert!(after.starts_with("chat:"), "{after}");
    assert_ne!(after, row_key::chat(CHAT_AUTH));
    press_keys(vcx, "up");
    assert_eq!(selected_key(&outline, vcx), Some(item_key(API_AUTH)));
    // Right has no child to enter.
    press_keys(vcx, "right");
    assert_eq!(selected_key(&outline, vcx), Some(item_key(API_AUTH)));
}

#[gpui::test]
fn selection_survives_expanding_and_compacting(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let fixture = crate::work_outline::store::fixture_snapshot("s1");
    let (overview, outline, _calls, _events, vcx) = compact_overview(cx, dir.path(), fixture.clone());
    select_key(&outline, vcx, &item_key(UI));

    // The item gets a second chat: it expands, the item stays selected.
    let mut two = fixture.clone();
    two.items
        .iter_mut()
        .find(|item| item.id == UI)
        .unwrap()
        .chat_ids
        .push(CHAT_LOOSE_A.into());
    deliver_frame(&overview, vcx, two);
    assert!(shown(vcx, &row_key::chat(CHAT_UI)));
    assert!(shown(vcx, &row_key::chat(CHAT_LOOSE_A)));
    assert_eq!(selected_key(&outline, vcx), Some(item_key(UI)));

    // Select its Chat row, then the other chat goes away: the row compacts
    // and the selection moves to the item rather than vanishing.
    select_key(&outline, vcx, &row_key::chat(CHAT_UI));
    deliver_frame(&overview, vcx, fixture);
    assert!(!shown(vcx, &row_key::chat(CHAT_UI)));
    assert_eq!(selected_key(&outline, vcx), Some(item_key(UI)));
}

#[gpui::test]
fn dragging_a_compact_row_moves_the_item(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, _events, vcx) =
        compact_overview(cx, dir.path(), crate::work_outline::store::fixture_snapshot("s1"));
    let start = row_bounds(vcx, format!("outline-row-{}", item_key(UI)))
        .unwrap()
        .center();
    let target = row_bounds(vcx, format!("outline-row-{}", item_key(HOUSEKEEPING))).unwrap();
    let end = target.center();
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
    assert_eq!(
        outline.read_with(vcx, |o, _| o.drop_hint().map(|h| h.key.clone())),
        Some(item_key(HOUSEKEEPING))
    );
    vcx.simulate_mouse_up(end, gpui::MouseButton::Left, gpui::Modifiers::default());
    vcx.run_until_parked();
    draw(vcx);
    let call = calls.lock().unwrap()[0].clone();
    assert_eq!(call["op"], "moveWorkItem", "{call}");
    assert_eq!(call["workItemId"], UI);
    assert_eq!(call["destinationParentWorkItemId"], HOUSEKEEPING);
}

/// Compact rows add no rebuild storm: streaming frames (no row-relevant
/// field changes) neither rebuild nor repaint the Overview, with the Outline
/// showing compact rows; a row-relevant change (the Chat's title, shown on
/// the folded row's tooltip, or its archived flag) still gets through.
#[gpui::test]
fn streaming_with_compact_rows_does_no_extra_outline_work(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, state, vcx) = outline_overview(cx, dir.path());
    overview.update(vcx, |o, cx| o.set_mode(OverviewMode::Outline, cx));
    draw(vcx);
    assert!(!shown(vcx, &row_key::chat(CHAT_UI)));
    let before = overview.read_with(vcx, |o, _| o.perf);
    for _ in 0..50 {
        state.update(vcx, |state, cx| {
            state.transcript.clear();
            state.queue.clear();
            cx.notify();
        });
        vcx.run_until_parked();
        draw(vcx);
    }
    let after = overview.read_with(vcx, |o, _| o.perf);
    assert_eq!(after.observer_calls - before.observer_calls, 50);
    assert_eq!(after.observer_notifies, before.observer_notifies, "{after:?}");
    assert_eq!(after.rows_rebuilds, before.rows_rebuilds, "{after:?}");
    assert_eq!(after.fetch_checks, before.fetch_checks, "{after:?}");

    // Archiving the folded Chat changes the fingerprint: the row expands out
    // of compact mode only when archived chats are shown, else it goes.
    state.update(vcx, |state, cx| {
        state
            .chats
            .iter_mut()
            .find(|chat| chat.id == CHAT_UI)
            .unwrap()
            .archived = true;
        cx.notify();
    });
    vcx.run_until_parked();
    draw(vcx);
    let end = overview.read_with(vcx, |o, _| o.perf);
    assert!(end.observer_notifies > after.observer_notifies, "{end:?}");
    assert!(!shown(vcx, &row_key::chat(CHAT_UI)));
    assert!(shown(vcx, &item_key(UI)));
}
