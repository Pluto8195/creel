// Space-root management tests (reorder, hide / unhide, single-Space
// behavior). Included from `overview.rs`'s test module so they share its
// Overview fixtures.
use super::*;

use crate::work_outline::model::OutlineRowKind;
use crate::work_outline::outline_view::OutlineView;

fn space_json(id: &str, path: &str) -> zeron_proto::Space {
    serde_json::from_value(serde_json::json!({
        "id": id, "deviceId": "d", "path": path,
        "createdAt": "2026-01-01T00:00:00Z"
    }))
    .unwrap()
}

fn space_chat(id: &str, space: &str, path: &str) -> zeron_proto::Chat {
    let mut chat = row(id, path, None).chat;
    chat.space_id = Some(space.into());
    chat.title = Some(format!("Chat {id}"));
    chat.last_message_at = Some(Utc::now());
    chat
}

/// Loads for `ids`: `s1` gets the store fixture with its Chats, every other
/// Space the same tree without Chats.
fn loads_for(ids: &[&str]) -> HashMap<String, WorkOutlineLoad> {
    ids.iter()
        .map(|id| {
            let mut snapshot = crate::work_outline::store::fixture_snapshot(id);
            if *id != "s1" {
                for item in &mut snapshot.items {
                    item.chat_ids.clear();
                }
                snapshot.assignments.clear();
                snapshot.unassociated_chat_ids = vec![format!("chat-{id}")];
            }
            (
                id.to_string(),
                WorkOutlineLoad::Ready(std::sync::Arc::new(snapshot)),
            )
        })
        .collect()
}

/// Spaces alpha (s1), beta (s2), gamma (s3) in Outline mode, the app keymap
/// installed (`shell::apply_keymap`). `s1` holds the store fixture's Chats;
/// `s2` / `s3` one unassociated Chat each (`chat-s2`, `chat-s3`).
fn spaces_overview<'a>(
    cx: &'a mut gpui::TestAppContext,
    dir: &std::path::Path,
) -> (
    Entity<Overview>,
    Entity<OutlineView>,
    Entity<AppState>,
    &'a mut gpui::VisualTestContext,
) {
    let (overview, state, vcx) = outline_overview(cx, dir);
    state.update(vcx, |state, cx| {
        state.spaces.push(space_json("s2", "/work/beta"));
        state.spaces.push(space_json("s3", "/work/gamma"));
        state.chats.push(space_chat("chat-s2", "s2", "/work/beta"));
        state.chats.push(space_chat("chat-s3", "s3", "/work/gamma"));
        cx.notify();
    });
    vcx.update(|_, cx| {
        crate::shell::apply_keymap(
            cx,
            &crate::settings::KeymapConfig::default(),
            crate::settings::ComposerSendBehavior::default(),
        )
    });
    let outline = open_spaces_outline(&overview, &["s1", "s2", "s3"], vcx);
    (overview, outline, state, vcx)
}

fn open_spaces_outline(
    overview: &Entity<Overview>,
    ids: &[&str],
    vcx: &mut gpui::VisualTestContext,
) -> Entity<OutlineView> {
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(loads_for(ids), cx);
        o.set_mode(OverviewMode::Outline, cx);
    });
    draw(vcx);
    overview.read_with(vcx, |o, _| o.outline.clone())
}

/// The Space root rows in display order.
fn roots(outline: &Entity<OutlineView>, vcx: &mut gpui::VisualTestContext) -> Vec<String> {
    outline.read_with(vcx, |outline, _| {
        outline
            .model()
            .rows
            .iter()
            .filter_map(|row| match &row.kind {
                OutlineRowKind::Space { space_id, .. } => Some(space_id.clone()),
                _ => None,
            })
            .collect()
    })
}

fn shows(outline: &Entity<OutlineView>, vcx: &mut gpui::VisualTestContext, key: &str) -> bool {
    outline.read_with(vcx, |outline, _| outline.model().row(key).is_some())
}

fn click_at(vcx: &mut gpui::VisualTestContext, selector: String) {
    let at = row_bounds(vcx, selector.clone())
        .unwrap_or_else(|| panic!("{selector} renders"))
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    draw(vcx);
}

fn press(vcx: &mut gpui::VisualTestContext, keys: &str) {
    vcx.simulate_keystrokes(keys);
    vcx.run_until_parked();
    draw(vcx);
}

fn saved_order(dir: &std::path::Path) -> Vec<String> {
    overview_positions::load_string_list(dir, overview_positions::OUTLINE_SPACE_ORDER_FILE)
}

/// ⇧↑ / ⇧↓ on a clicked Space root (the app keymap) swap it with its
/// neighbour, stop at either end, send nothing to the engine, and the order
/// survives a fresh Overview.
#[gpui::test]
fn shift_arrows_reorder_space_roots_and_persist(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, state, vcx) = spaces_overview(cx, dir.path());
    assert_eq!(
        roots(&outline, vcx),
        ["s1", "s2", "s3"],
        "alphabetical first"
    );

    click_at(vcx, format!("outline-row-{}", row_key::space("s3")));
    press(vcx, "shift-up");
    assert_eq!(roots(&outline, vcx), ["s1", "s3", "s2"]);
    press(vcx, "shift-up");
    assert_eq!(roots(&outline, vcx), ["s3", "s1", "s2"]);
    press(vcx, "shift-up");
    assert_eq!(roots(&outline, vcx), ["s3", "s1", "s2"], "already first");
    assert_eq!(
        outline.read_with(vcx, |o, _| o.selected_key().map(str::to_owned)),
        Some(row_key::space("s3")),
        "the selection stays on the moved root"
    );
    press(vcx, "shift-down");
    assert_eq!(roots(&outline, vcx), ["s1", "s3", "s2"]);
    assert_eq!(saved_order(dir.path()), ["s1", "s3", "s2"]);
    // Indent / Outdent mean nothing on a Space root.
    press(vcx, "shift-right");
    assert_eq!(roots(&outline, vcx), ["s1", "s3", "s2"]);

    // A fresh Overview reads the saved order.
    let (second, vcx) = overview_over(cx, &state);
    let outline = open_spaces_outline(&second, &["s1", "s2", "s3"], vcx);
    assert_eq!(roots(&outline, vcx), ["s1", "s3", "s2"]);
}

/// Where on a target row a drag is released.
fn drag_onto(
    vcx: &mut gpui::VisualTestContext,
    from: &str,
    to: &str,
    lower: bool,
) -> gpui::Point<Pixels> {
    let start = row_bounds(vcx, format!("outline-row-{from}"))
        .unwrap_or_else(|| panic!("{from} is shown"))
        .center();
    let target =
        row_bounds(vcx, format!("outline-row-{to}")).unwrap_or_else(|| panic!("{to} is shown"));
    let end = if lower {
        gpui::point(target.center().x, target.bottom() - px(2.0))
    } else {
        gpui::point(target.center().x, target.top() + px(2.0))
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

fn drop_hint(
    outline: &Entity<OutlineView>,
    vcx: &mut gpui::VisualTestContext,
) -> Option<(String, crate::work_outline::restructure::DropZone)> {
    outline.read_with(vcx, |o, _| o.drop_hint().map(|h| (h.key.clone(), h.zone)))
}

/// Dragging a Space root onto another root's upper / lower half places it
/// before / after that root; Space roots and work items never take each
/// other's drags.
#[gpui::test]
fn dragging_space_roots_reorders_them(cx: &mut gpui::TestAppContext) {
    use crate::work_outline::restructure::DropZone;
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, _state, vcx) = spaces_overview(cx, dir.path());
    let space = row_key::space;
    // Collapse s1 and s2 so all three roots fit on screen.
    for id in ["s1", "s2"] {
        click_at(vcx, format!("outline-disclosure-{}", space(id)));
    }

    let at = drag_onto(vcx, &space("s1"), &space("s3"), true);
    assert_eq!(
        drop_hint(&outline, vcx),
        Some((space("s3"), DropZone::After))
    );
    release(vcx, at);
    assert_eq!(roots(&outline, vcx), ["s2", "s3", "s1"]);
    assert_eq!(saved_order(dir.path()), ["s2", "s3", "s1"]);

    let at = drag_onto(vcx, &space("s1"), &space("s2"), false);
    assert_eq!(
        drop_hint(&outline, vcx),
        Some((space("s2"), DropZone::Before))
    );
    release(vcx, at);
    assert_eq!(roots(&outline, vcx), ["s1", "s2", "s3"]);

    // Onto itself / where it already is: no indicator, no change.
    let at = drag_onto(vcx, &space("s1"), &space("s2"), false);
    assert_eq!(drop_hint(&outline, vcx), None);
    release(vcx, at);
    assert_eq!(roots(&outline, vcx), ["s1", "s2", "s3"]);

    // A Space root is no target for a work item, nor a work item for a root.
    click_at(vcx, format!("outline-disclosure-{}", space("s1")));
    let launch = row_key::item("s1", LAUNCH);
    let at = drag_onto(vcx, &launch, &space("s2"), false);
    assert_eq!(drop_hint(&outline, vcx), None);
    release(vcx, at);
    let at = drag_onto(vcx, &space("s2"), &launch, false);
    assert_eq!(drop_hint(&outline, vcx), None);
    release(vcx, at);
    assert_eq!(roots(&outline, vcx), ["s1", "s2", "s3"]);
}

/// A Space the saved order does not list (a new one) follows the ordered
/// ones, alphabetically among the unlisted; a deleted id is dropped on the
/// next write.
#[gpui::test]
fn new_spaces_append_after_the_saved_order(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    overview_positions::save_string_list(
        dir.path(),
        overview_positions::OUTLINE_SPACE_ORDER_FILE,
        &["s3".into(), "gone".into(), "s1".into()],
    )
    .unwrap();
    let (overview, outline, state, vcx) = spaces_overview(cx, dir.path());
    assert_eq!(roots(&outline, vcx), ["s3", "s1", "s2"]);

    state.update(vcx, |state, cx| {
        state.spaces.push(space_json("s0", "/work/aardvark"));
        cx.notify();
    });
    let outline = open_spaces_outline(&overview, &["s0", "s1", "s2", "s3"], vcx);
    assert_eq!(roots(&outline, vcx), ["s3", "s1", "s0", "s2"]);

    // The next move stores the full current order (no stale ids).
    outline.update(vcx, |o, cx| o.move_space("s2", true, cx));
    assert_eq!(roots(&outline, vcx), ["s3", "s1", "s2", "s0"]);
    assert_eq!(saved_order(dir.path()), ["s3", "s1", "s2", "s0"]);
}

/// Hide from Outline drops the Space and its Chats from the Outline only:
/// List rows and the app's Chats keep them. The toolbar's "Hidden projects
/// (N)" reveals hidden roots dimmed with Unhide; both states persist.
#[gpui::test]
fn hiding_a_space_only_affects_the_outline(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, state, vcx) = spaces_overview(cx, dir.path());
    assert!(row_bounds(vcx, "outline-hidden-spaces".into()).is_none());

    outline.update(vcx, |o, cx| o.set_space_hidden("s2", true, cx));
    draw(vcx);
    assert_eq!(roots(&outline, vcx), ["s1", "s3"]);
    assert!(!shows(&outline, vcx, &row_key::chat("chat-s2")));
    assert!(
        !shows(
            &outline,
            vcx,
            crate::work_outline::model::PROJECTLESS_GROUP_KEY
        ),
        "its Chats do not move to the projectless group"
    );
    assert!(outline.read_with(vcx, |o, _| o.notice().is_some_and(|n| n.contains("beta"))));
    // The toolbar count.
    assert!(row_bounds(vcx, "outline-hidden-spaces".into()).is_some());
    assert_eq!(outline.read_with(vcx, |o, _| o.hidden_space_count()), 1);
    // List and the app state still have the Chat.
    overview.update(vcx, |o, cx| {
        assert!(o.rows(cx).iter().any(|row| row.chat.id == "chat-s2"));
    });
    assert!(state.read_with(vcx, |s, _| s.chats.iter().any(|c| c.id == "chat-s2")));

    // ⇧↑ on the root below skips the hidden one.
    outline.update(vcx, |o, cx| o.move_space("s3", true, cx));
    assert_eq!(roots(&outline, vcx), ["s3", "s1"]);
    outline.update(vcx, |o, cx| o.move_space("s3", false, cx));

    // Reveal: the hidden root shows dimmed, childless, with Unhide.
    click_at(vcx, "outline-hidden-spaces".into());
    assert_eq!(roots(&outline, vcx), ["s1", "s3", "s2"]);
    let hidden_row = outline.read_with(vcx, |o, _| o.model().row("space:s2").cloned());
    let hidden_row = hidden_row.unwrap();
    assert!(!hidden_row.has_children);
    assert!(matches!(
        hidden_row.kind,
        OutlineRowKind::Space { hidden: true, .. }
    ));
    assert!(!shows(&outline, vcx, &row_key::chat("chat-s2")));

    // A fresh Overview still hides it.
    let (second, vcx2) = overview_over(cx, &state);
    let fresh = open_spaces_outline(&second, &["s1", "s2", "s3"], vcx2);
    assert_eq!(roots(&fresh, vcx2), ["s1", "s3"]);
    assert_eq!(fresh.read_with(vcx2, |o, _| o.hidden_space_count()), 1);

    // Unhide brings it back with its rows; the toolbar control goes away.
    click_at(vcx2, "outline-hidden-spaces".into());
    click_at(vcx2, "outline-space-unhide-s2".into());
    assert_eq!(roots(&fresh, vcx2), ["s1", "s3", "s2"]);
    assert!(shows(&fresh, vcx2, &row_key::chat("chat-s2")));
    assert!(row_bounds(vcx2, "outline-hidden-spaces".into()).is_none());
    assert!(
        overview_positions::load_string_set(
            dir.path(),
            overview_positions::OUTLINE_HIDDEN_SPACES_FILE
        )
        .is_empty()
    );
}

/// One Space: no root rows, so nothing to reorder or hide.
#[gpui::test]
fn a_single_space_outline_is_unaffected(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _state, vcx) = outline_overview(cx, dir.path());
    overview.update(vcx, |o, cx| o.set_mode(OverviewMode::Outline, cx));
    draw(vcx);
    let outline = overview.read_with(vcx, |o, _| o.outline.clone());
    outline.update(vcx, |o, cx| o.set_space_hidden("s1", true, cx));
    draw(vcx);
    assert!(roots(&outline, vcx).is_empty());
    assert!(shows(&outline, vcx, &row_key::item("s1", LAUNCH)));
    assert_eq!(outline.read_with(vcx, |o, _| o.hidden_space_count()), 0);
    assert!(row_bounds(vcx, "outline-hidden-spaces".into()).is_none());
}
