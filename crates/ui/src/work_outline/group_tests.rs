// Multi-selection and "Group into…": the selection set, a new or existing
// container, nested-selection dedupe, cycle refusal, and ONE undo / redo step.
// Included from `overview.rs`'s test module so they share its Overview
// fixtures.
use super::*;

use crate::work_outline::optimistic::{self, LocalChange};
use crate::work_outline::outline_view::OutlineView;
use crate::work_outline::outline_view::group::GroupDest;

fn group_overview<'a>(
    cx: &'a mut gpui::TestAppContext,
    dir: &std::path::Path,
) -> (
    Entity<Overview>,
    Entity<OutlineView>,
    OutlineCalls,
    &'a mut gpui::VisualTestContext,
) {
    let (overview, calls, vcx) = instruction_overview(cx, dir, fixture(), reply_ok);
    vcx.update(|_, cx| {
        crate::composer::init(cx, crate::settings::ComposerSendBehavior::default());
        cx.bind_keys(crate::work_outline::outline_view::key_bindings());
    });
    let outline = overview.read_with(vcx, |o, _| o.outline.clone());
    vcx.update(|window, cx| {
        let handle = gpui::Focusable::focus_handle(outline.read(cx), cx);
        window.focus(&handle, cx);
    });
    draw(vcx);
    (overview, outline, calls, vcx)
}

fn fixture() -> WorkOutlineSnapshot {
    crate::work_outline::store::fixture_snapshot("s1")
}

fn key(id: &str) -> String {
    row_key::item("s1", id)
}

fn press(vcx: &mut gpui::VisualTestContext, keys: &str) {
    vcx.simulate_keystrokes(keys);
    vcx.run_until_parked();
}

fn settle(
    overview: &Entity<Overview>,
    vcx: &mut gpui::VisualTestContext,
    snapshot: WorkOutlineSnapshot,
) {
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(snapshot)), cx)
    });
    draw(vcx);
    vcx.executor()
        .advance_clock(std::time::Duration::from_secs(3));
    vcx.run_until_parked();
    draw(vcx);
}

fn call(calls: &OutlineCalls, ix: usize) -> serde_json::Value {
    calls.lock().unwrap()[ix].clone()
}

fn count(calls: &OutlineCalls) -> usize {
    calls.lock().unwrap().len()
}

fn select_items(outline: &Entity<OutlineView>, vcx: &mut gpui::VisualTestContext, ids: &[&str]) {
    outline.update(vcx, |outline, cx| {
        outline.select(&key(ids[0]), cx);
        for id in &ids[1..] {
            outline.toggle_multi(&key(id), cx);
        }
    });
}

fn group_selection(outline: &Entity<OutlineView>, vcx: &mut gpui::VisualTestContext) -> Vec<String> {
    outline.read_with(vcx, |o, _| {
        o.group_selection().into_iter().map(|(_, id)| id).collect()
    })
}

/// The snapshot once `items` sit under the new container `container`.
fn grouped_snapshot(container: &str, title: &str, items: &[&str]) -> WorkOutlineSnapshot {
    let mut snapshot = fixture();
    snapshot = optimistic::apply(
        &snapshot,
        &LocalChange::Create {
            work_item_id: container.into(),
            parent: Some(LAUNCH.into()),
            placement: mast_proto::WorkOutlinePlacement::Before {
                sibling_work_item_id: API.into(),
            },
            title: title.into(),
            created_at: 1,
        },
    )
    .unwrap();
    for id in items {
        snapshot = optimistic::apply(
            &snapshot,
            &LocalChange::Move {
                work_item_id: (*id).into(),
                destination_parent: Some(container.into()),
                placement: mast_proto::WorkOutlinePlacement::Last,
            },
        )
        .unwrap();
    }
    snapshot
}

/// The grouped snapshot after the undo: items back, container archived.
fn undone_snapshot(container: &str, title: &str) -> WorkOutlineSnapshot {
    let mut snapshot = grouped_snapshot(container, title, &[API, UI]);
    for (id, placement) in [
        (API, mast_proto::WorkOutlinePlacement::First),
        (
            UI,
            mast_proto::WorkOutlinePlacement::After {
                sibling_work_item_id: API.into(),
            },
        ),
    ] {
        snapshot = optimistic::apply(
            &snapshot,
            &LocalChange::Move {
                work_item_id: id.into(),
                destination_parent: Some(LAUNCH.into()),
                placement,
            },
        )
        .unwrap();
    }
    let view = snapshot
        .items
        .iter_mut()
        .find(|item| item.id == container)
        .unwrap();
    view.archived_at = Some(crate::work_outline::store::FIXTURE_NOW + 5);
    snapshot
}

/// Create the container through the picker and the inline title row.
fn group_api_and_ui_into_new(
    outline: &Entity<OutlineView>,
    vcx: &mut gpui::VisualTestContext,
    title: &str,
) {
    select_items(outline, vcx, &[API, UI]);
    outline.update_in(vcx, |outline, window, cx| {
        outline.open_group_picker(window, cx).unwrap()
    });
    draw(vcx);
    // Entry 0 is "New container…".
    outline.update_in(vcx, |outline, window, cx| {
        outline.choose_picker_entry(0, window, cx)
    });
    draw(vcx);
    assert!(outline.read_with(vcx, |o, _| o.is_editing()));
    vcx.simulate_input(title);
    draw(vcx);
    press(vcx, "enter");
}

#[gpui::test]
fn cmd_click_set_api_toggles_rows_and_plain_actions_clear_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, vcx) = group_overview(cx, dir.path());
    // One selected item groups on its own.
    outline.update(vcx, |o, cx| o.select(&key(UI), cx));
    assert_eq!(group_selection(&outline, vcx), [UI]);

    // Additive, in visible order whatever the click order.
    select_items(&outline, vcx, &[UI, API]);
    assert_eq!(group_selection(&outline, vcx), [API, UI]);
    outline.update(vcx, |o, cx| o.toggle_multi(&key(API), cx));
    assert_eq!(group_selection(&outline, vcx), [UI]);

    // Shift-click extends over the visible item rows from the primary row.
    outline.update(vcx, |o, cx| {
        o.clear_multi(cx);
        o.select(&key(LAUNCH), cx);
        o.extend_multi_to(&key(UI), cx);
    });
    let selected = group_selection(&outline, vcx);
    assert!(selected.contains(&LAUNCH.to_owned()) && selected.contains(&UI.to_owned()));

    // Chat rows are not groupable.
    outline.update(vcx, |o, cx| {
        o.clear_multi(cx);
        o.select(&key(UI), cx);
        o.toggle_multi(&row_key::chat(CHAT_LOOSE_A), cx);
    });
    assert_eq!(group_selection(&outline, vcx), [UI]);

    // Esc clears the set; the primary row stays.
    select_items(&outline, vcx, &[UI, API]);
    press(vcx, "escape");
    assert_eq!(group_selection(&outline, vcx), [API], "the last clicked row stays");
    assert_eq!(count(&calls), 0);
}

#[gpui::test]
fn group_into_a_new_container_is_one_create_and_ordered_moves(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, vcx) = group_overview(cx, dir.path());
    group_api_and_ui_into_new(&outline, vcx, "Bucket");

    assert_eq!(count(&calls), 3);
    let create = call(&calls, 0);
    assert_eq!(create["op"], "createWorkItem");
    assert_eq!(create["title"], "Bucket");
    assert_eq!(create["parentWorkItemId"], LAUNCH, "same parent as API");
    assert_eq!(create["placement"]["type"], "before");
    assert_eq!(create["placement"]["siblingWorkItemId"], API);
    let container = mast_proto::derive_id(
        create["operationId"].as_str().unwrap(),
        mast_proto::roles::ITEM,
    );
    for (ix, id) in [API, UI].into_iter().enumerate() {
        let moved = call(&calls, ix + 1);
        assert_eq!(moved["op"], "moveWorkItem");
        assert_eq!(moved["workItemId"], id, "visible order");
        assert_eq!(moved["destinationParentWorkItemId"], container);
        assert_eq!(moved["placement"]["type"], "last");
    }
    for ix in 0..3 {
        let id = call(&calls, ix)["operationId"].as_str().unwrap().to_owned();
        assert!(!id.is_empty() && id.len() <= 64, "{id}");
    }
    outline.read_with(vcx, |o, _| {
        assert_eq!(o.selected_key(), Some(key(&container).as_str()));
    });
    assert_eq!(group_selection(&outline, vcx), [container]);
}

#[gpui::test]
fn group_into_an_existing_container_appends_in_order(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, vcx) = group_overview(cx, dir.path());
    select_items(&outline, vcx, &[UI, HOUSEKEEPING]);
    outline.update_in(vcx, |outline, window, cx| {
        outline.open_group_picker(window, cx).unwrap()
    });
    draw(vcx);
    // New container…, Launch, API, Auth endpoints: the picked items are not
    // offered. Choose "API".
    let api_entry = 2;
    outline.update_in(vcx, |outline, window, cx| {
        outline.choose_picker_entry(api_entry, window, cx)
    });
    vcx.run_until_parked();
    assert_eq!(count(&calls), 2);
    for (ix, id) in [UI, HOUSEKEEPING].into_iter().enumerate() {
        let moved = call(&calls, ix);
        assert_eq!(moved["op"], "moveWorkItem");
        assert_eq!(moved["workItemId"], id);
        assert_eq!(moved["destinationParentWorkItemId"], API);
        assert_eq!(moved["placement"]["type"], "last");
    }
}

#[gpui::test]
fn nested_selected_items_move_with_their_ancestor(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, vcx) = group_overview(cx, dir.path());
    let result = outline.update(vcx, |o, cx| {
        o.group_items(
            "s1",
            &[UI.into(), API_AUTH.into(), API.into()],
            GroupDest::Existing(HOUSEKEEPING.into()),
            cx,
        )
    });
    assert_eq!(result, Ok(()));
    vcx.run_until_parked();
    let moved: Vec<String> = (0..count(&calls))
        .map(|ix| call(&calls, ix)["workItemId"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(moved, [API, UI], "Auth endpoints rides along with API");
}

#[gpui::test]
fn grouping_into_a_selected_item_or_its_descendant_is_refused(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, vcx) = group_overview(cx, dir.path());
    for (items, target) in [
        (vec![LAUNCH], API_AUTH),
        (vec![API], API),
        (vec![API, UI], API_AUTH),
    ] {
        let items: Vec<String> = items.into_iter().map(String::from).collect();
        let result = outline.update(vcx, |o, cx| {
            o.group_items("s1", &items, GroupDest::Existing(target.into()), cx)
        });
        assert!(result.is_err(), "{items:?} into {target}");
    }
    vcx.run_until_parked();
    assert_eq!(count(&calls), 0, "nothing sent");
    assert!(
        outline
            .read_with(vcx, |o, cx| o.next_undo_label(cx))
            .is_none(),
        "nothing logged"
    );
}

/// Create + both moves are ONE log entry: one ⌘Z moves both back and archives
/// the container; ⇧⌘Z restores it and groups again.
#[gpui::test]
fn one_undo_reverts_the_whole_grouping_and_redo_repeats_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = group_overview(cx, dir.path());
    group_api_and_ui_into_new(&outline, vcx, "Bucket");
    assert_eq!(count(&calls), 3);
    let container = mast_proto::derive_id(
        call(&calls, 0)["operationId"].as_str().unwrap(),
        mast_proto::roles::ITEM,
    );
    settle(&overview, vcx, grouped_snapshot(&container, "Bucket", &[API, UI]));
    assert_eq!(
        outline
            .read_with(vcx, |o, cx| o.next_undo_label(cx))
            .as_deref(),
        Some("grouped 2 items into \u{2018}Bucket\u{2019}")
    );

    press(vcx, "cmd-z");
    assert_eq!(count(&calls), 6, "moves back x2 and the archive, one ⌘Z");
    assert_eq!(call(&calls, 3)["op"], "moveWorkItem");
    assert_eq!(call(&calls, 3)["workItemId"], API);
    assert_eq!(call(&calls, 3)["destinationParentWorkItemId"], LAUNCH);
    assert_eq!(call(&calls, 3)["placement"]["type"], "first");
    assert_eq!(call(&calls, 4)["workItemId"], UI);
    assert_eq!(call(&calls, 4)["placement"]["type"], "after");
    assert_eq!(call(&calls, 4)["placement"]["siblingWorkItemId"], API);
    assert_eq!(call(&calls, 5)["op"], "archiveWorkItem");
    assert_eq!(call(&calls, 5)["workItemId"], container);
    assert!(
        outline
            .read_with(vcx, |o, cx| o.next_undo_label(cx))
            .is_none(),
        "the grouping was a single step"
    );
    let notice = outline.read_with(vcx, |o, _| o.notice().map(str::to_owned));
    assert_eq!(
        notice.as_deref(),
        Some("Undid: grouped 2 items into \u{2018}Bucket\u{2019}")
    );

    settle(&overview, vcx, undone_snapshot(&container, "Bucket"));
    press(vcx, "cmd-shift-z");
    assert_eq!(count(&calls), 9);
    assert_eq!(call(&calls, 6)["op"], "restoreWorkItem");
    assert_eq!(call(&calls, 6)["workItemId"], container);
    for (ix, id) in [(7, API), (8, UI)] {
        assert_eq!(call(&calls, ix)["op"], "moveWorkItem");
        assert_eq!(call(&calls, ix)["workItemId"], id);
        assert_eq!(call(&calls, ix)["destinationParentWorkItemId"], container);
    }
    for ix in 0..9 {
        let id = call(&calls, ix)["operationId"].as_str().unwrap().to_owned();
        assert!(!id.is_empty() && id.len() <= 64, "{id}");
    }
}

/// Undoing an existing-container grouping only moves the items back.
#[gpui::test]
fn undo_of_grouping_into_an_existing_container_moves_items_back(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = group_overview(cx, dir.path());
    let result = outline.update(vcx, |o, cx| {
        o.group_items(
            "s1",
            &[UI.into(), HOUSEKEEPING.into()],
            GroupDest::Existing(API.into()),
            cx,
        )
    });
    assert_eq!(result, Ok(()));
    vcx.run_until_parked();
    let mut after = fixture();
    for id in [UI, HOUSEKEEPING] {
        after = optimistic::apply(
            &after,
            &LocalChange::Move {
                work_item_id: id.into(),
                destination_parent: Some(API.into()),
                placement: mast_proto::WorkOutlinePlacement::Last,
            },
        )
        .unwrap();
    }
    settle(&overview, vcx, after);
    outline.update(vcx, |o, cx| o.select(&key(API), cx));
    press(vcx, "cmd-z");
    assert_eq!(count(&calls), 4);
    assert_eq!(call(&calls, 2)["workItemId"], UI);
    assert_eq!(call(&calls, 2)["destinationParentWorkItemId"], LAUNCH);
    assert_eq!(call(&calls, 3)["workItemId"], HOUSEKEEPING);
    assert!(call(&calls, 3)["destinationParentWorkItemId"].is_null());
}
