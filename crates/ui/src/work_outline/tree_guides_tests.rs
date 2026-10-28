// Outline tree guides, active path, drop indicator offsets and labels, and
// the keyboard-move flash, asserted on rendered geometry. Included from
// `overview.rs`'s test module so it shares its Overview fixtures.
use super::*;

use crate::work_outline::outline_view::OutlineView;

fn guides_overview<'a>(
    cx: &'a mut gpui::TestAppContext,
    dir: &std::path::Path,
) -> (Entity<OutlineView>, &'a mut gpui::VisualTestContext) {
    let (overview, _calls, vcx) = instruction_overview(
        cx,
        dir,
        crate::work_outline::store::fixture_snapshot("s1"),
        reply_ok,
    );
    vcx.update(|_, cx| cx.bind_keys(crate::work_outline::outline_view::key_bindings()));
    let outline = overview.read_with(vcx, |o, _| o.outline.clone());
    outline.update(vcx, |outline, cx| outline.set_compact_single_chat(false, cx));
    vcx.update(|window, cx| {
        let handle = gpui::Focusable::focus_handle(outline.read(cx), cx);
        window.focus(&handle, cx);
    });
    draw(vcx);
    (outline, vcx)
}

fn item_key(id: &str) -> String {
    row_key::item("s1", id)
}

fn shown(vcx: &mut gpui::VisualTestContext, selector: String) -> bool {
    row_bounds(vcx, selector).is_some()
}

fn select(outline: &Entity<OutlineView>, vcx: &mut gpui::VisualTestContext, key: &str) {
    outline.update(vcx, |outline, cx| outline.select(key, cx));
    draw(vcx);
}

/// Guides sit under the parent's disclosure triangle, elbows lead into
/// every child (items and Chats), and the 1px line adds no indent.
#[gpui::test]
fn guides_align_under_parent_disclosure(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_outline, vcx) = guides_overview(cx, dir.path());
    let launch = item_key(LAUNCH);
    let api = item_key(API);
    // Roots have no guides.
    assert!(!shown(vcx, format!("outline-guide-arm-{launch}")));
    // API (depth 1) has an elbow in Launch's column (0).
    let hit = row_bounds(vcx, format!("outline-guide-{api}-0")).expect("API's guide");
    let disclosure =
        row_bounds(vcx, format!("outline-disclosure-{launch}")).expect("Launch disclosure");
    let line_x = f32::from(hit.left()) + 3.0;
    let center = f32::from(disclosure.center().x);
    assert!(
        (line_x - center).abs() <= 1.0,
        "guide x {line_x} under disclosure center {center}"
    );
    assert!(shown(vcx, format!("outline-guide-arm-{api}")));
    // Auth endpoints (depth 2): Launch's line continues (API has a later
    // sibling, UI) and API's elbow leads in.
    let auth = item_key(API_AUTH);
    assert!(shown(vcx, format!("outline-guide-{auth}-0")));
    assert!(shown(vcx, format!("outline-guide-{auth}-1")));
    // Chats get guides too.
    assert!(shown(
        vcx,
        format!("outline-guide-arm-{}", row_key::chat(CHAT_AUTH))
    ));
    assert!(shown(
        vcx,
        format!("outline-guide-arm-{}", row_key::chat(CHAT_UI))
    ));
    // The row's own disclosure did not move: still one indent step in.
    let api_disclosure = row_bounds(vcx, format!("outline-disclosure-{api}")).unwrap();
    assert_eq!(
        f32::from(api_disclosure.left() - disclosure.left()),
        crate::work_outline::tree_guides::INDENT_STEP
    );
    // UI is Launch's last child: Launch's column ends there, so UI's Chat
    // row has no line in column 0.
    let ui_chat = row_key::chat(CHAT_UI);
    assert!(shown(vcx, format!("outline-guide-{ui_chat}-1")));
    assert!(!shown(vcx, format!("outline-guide-{ui_chat}-0")));
}

/// The selected row's ancestor chain and its own subtree are lit; other
/// guides are not.
#[gpui::test]
fn selection_lights_the_active_path(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (outline, vcx) = guides_overview(cx, dir.path());
    let api = item_key(API);
    let auth = item_key(API_AUTH);
    let ui = item_key(UI);
    select(&outline, vcx, &auth);
    // Launch -> API, API -> Auth endpoints.
    assert!(shown(vcx, format!("outline-guide-lit-{api}-0")));
    assert!(shown(vcx, format!("outline-guide-lit-{auth}-1")));
    // Auth endpoints' own subtree (its Chat).
    let auth_chat = row_key::chat(CHAT_AUTH);
    assert!(shown(vcx, format!("outline-guide-lit-{auth_chat}-2")));
    // Not on the path: Launch's line past API, API's Chats, UI.
    assert!(!shown(vcx, format!("outline-guide-lit-{auth}-0")));
    assert!(!shown(
        vcx,
        format!("outline-guide-lit-{}-1", row_key::chat(CHAT_API))
    ));
    assert!(!shown(vcx, format!("outline-guide-lit-{ui}-0")));

    // Selecting UI lights Launch's column from API down to UI.
    select(&outline, vcx, &ui);
    assert!(shown(vcx, format!("outline-guide-lit-{api}-0")));
    assert!(shown(vcx, format!("outline-guide-lit-{auth}-0")));
    assert!(shown(vcx, format!("outline-guide-lit-{ui}-0")));
    assert!(!shown(vcx, format!("outline-guide-lit-{auth}-1")));
}

/// Press on row `from`, drag onto `to`'s upper band (`upper`) or middle.
fn drag_onto(
    vcx: &mut gpui::VisualTestContext,
    from: &str,
    to: &str,
    upper: bool,
) -> gpui::Point<Pixels> {
    let start = row_bounds(vcx, format!("outline-row-{from}"))
        .unwrap()
        .center();
    let target = row_bounds(vcx, format!("outline-row-{to}")).unwrap();
    let end = if upper {
        gpui::point(target.center().x, target.top() + px(2.0))
    } else {
        target.center()
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

fn label(outline: &Entity<OutlineView>, vcx: &mut gpui::VisualTestContext) -> Option<String> {
    outline.read_with(vcx, |outline, _| outline.drop_label())
}

/// Before / after lines start at the destination parent's guide, so a
/// deeper destination starts one indent step further right; "into" draws
/// no line. A label names the target and the destination parent.
#[gpui::test]
fn drop_indicator_starts_at_destination_parent_guide(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (outline, vcx) = guides_overview(cx, dir.path());
    let housekeeping = item_key(HOUSEKEEPING);
    let ui = item_key(UI);
    let auth = item_key(API_AUTH);

    // Before UI: under Launch v2 (depth 0).
    let at = drag_onto(vcx, &housekeeping, &ui, true);
    let line = row_bounds(vcx, format!("outline-drop-before-{ui}")).expect("before line");
    let launch_guide = row_bounds(vcx, format!("outline-guide-{ui}-0")).unwrap();
    assert_eq!(line.left(), launch_guide.left() + px(3.0));
    assert_eq!(
        label(&outline, vcx).as_deref(),
        Some("Before UI (under Launch v2)")
    );
    assert!(shown(vcx, "outline-drop-label".into()));
    release(vcx, at);
    assert_eq!(
        label(&outline, vcx),
        None,
        "the label goes with the indicator"
    );
    assert!(!shown(vcx, "outline-drop-label".into()));
    vcx.executor()
        .advance_clock(std::time::Duration::from_secs(3));
    draw(vcx);

    // Before Auth endpoints: under API (depth 1), one step further right.
    let at = drag_onto(vcx, &housekeeping, &auth, true);
    let deeper = row_bounds(vcx, format!("outline-drop-before-{auth}")).expect("before line");
    assert_eq!(
        f32::from(deeper.left() - line.left()),
        crate::work_outline::tree_guides::INDENT_STEP
    );
    assert_eq!(
        label(&outline, vcx).as_deref(),
        Some("Before Auth endpoints (under API)")
    );
    release(vcx, at);
    vcx.executor()
        .advance_clock(std::time::Duration::from_secs(3));
    draw(vcx);

    // Into UI: no insertion line, the row is the target, the label says so.
    let at = drag_onto(vcx, &housekeeping, &ui, false);
    assert!(!shown(vcx, format!("outline-drop-before-{ui}")));
    assert!(!shown(vcx, format!("outline-drop-after-{ui}")));
    assert_eq!(label(&outline, vcx).as_deref(), Some("Into UI"));
    release(vcx, at);
    vcx.executor()
        .advance_clock(std::time::Duration::from_secs(3));
    draw(vcx);

    // A Chat drag goes into the item: "Into API".
    let at = drag_onto(vcx, &row_key::chat(CHAT_UI), &item_key(API), true);
    assert_eq!(label(&outline, vcx).as_deref(), Some("Into API"));
    release(vcx, at);
}

/// A keyboard move flashes the moved row (and its new parent's guide) for
/// at most 400 ms; reduced motion skips the flash.
#[gpui::test]
fn keyboard_move_flashes_unless_reduced_motion(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (outline, vcx) = guides_overview(cx, dir.path());
    let ui = item_key(UI);
    let flash = |outline: &Entity<OutlineView>, vcx: &mut gpui::VisualTestContext| {
        outline.read_with(vcx, |o, _| o.move_flash_key().map(str::to_owned))
    };
    select(&outline, vcx, &ui);
    vcx.simulate_keystrokes("shift-up");
    draw(vcx);
    assert_eq!(flash(&outline, vcx), Some(ui.clone()));
    assert!(shown(vcx, format!("outline-move-flash-{ui}")));
    // UI is Launch v2's first child now; Launch's guide into it is lit.
    assert!(shown(vcx, format!("outline-guide-lit-{ui}-0")));
    vcx.executor()
        .advance_clock(crate::work_outline::tree_guides::MOVE_FLASH);
    vcx.run_until_parked();
    draw(vcx);
    assert_eq!(flash(&outline, vcx), None);
    assert!(!shown(vcx, format!("outline-move-flash-{ui}")));

    // Reduced motion: the move still happens, without the flash.
    vcx.executor()
        .advance_clock(std::time::Duration::from_secs(3));
    vcx.run_until_parked();
    vcx.update(|_, cx| cx.set_reduce_motion(true));
    select(&outline, vcx, &ui);
    vcx.simulate_keystrokes("shift-down");
    draw(vcx);
    assert_eq!(flash(&outline, vcx), None);
    assert!(!shown(vcx, format!("outline-move-flash-{ui}")));
}
