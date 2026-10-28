// Undo / redo / Recent changes tests: ⌘Z and ⇧⌘Z in the Outline versus a text
// field, the notice, the Recent changes list, conflicts, and failures.
// Included from `overview.rs`'s test module so they share its Overview
// fixtures.
use super::*;

use crate::work_outline::outline_view::OutlineView;

fn undo_overview<'a>(
    cx: &'a mut gpui::TestAppContext,
    dir: &std::path::Path,
    snapshot: WorkOutlineSnapshot,
    reply: impl Fn(&serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError>
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
    vcx.update(|_, cx| {
        // The app's text-input bindings too: the rename box's own ⌘Z lives there.
        crate::composer::init(cx, crate::settings::ComposerSendBehavior::default());
        cx.bind_keys(crate::work_outline::outline_view::key_bindings());
    });
    let outline = overview.read_with(vcx, |o, _| o.outline.clone());
    focus(&outline, vcx);
    (overview, outline, calls, vcx)
}

fn focus(outline: &Entity<OutlineView>, vcx: &mut gpui::VisualTestContext) {
    vcx.update(|window, cx| {
        let handle = gpui::Focusable::focus_handle(outline.read(cx), cx);
        window.focus(&handle, cx);
    });
    draw(vcx);
}

fn select(outline: &Entity<OutlineView>, vcx: &mut gpui::VisualTestContext, item: &str) {
    outline.update(vcx, |outline, cx| {
        outline.select(&row_key::item("s1", item), cx)
    });
    draw(vcx);
}

fn press(vcx: &mut gpui::VisualTestContext, keys: &str) {
    vcx.simulate_keystrokes(keys);
    vcx.run_until_parked();
}

/// A frame of `snapshot`, with the wait for optimistic changes over.
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

fn fixture() -> WorkOutlineSnapshot {
    crate::work_outline::store::fixture_snapshot("s1")
}

fn notice(outline: &Entity<OutlineView>, vcx: &mut gpui::VisualTestContext) -> Option<String> {
    outline.read_with(vcx, |outline, _| outline.notice().map(str::to_owned))
}

fn call(calls: &OutlineCalls, ix: usize) -> serde_json::Value {
    calls.lock().unwrap()[ix].clone()
}

fn count(calls: &OutlineCalls) -> usize {
    calls.lock().unwrap().len()
}

/// Every call carries its own operation id, within the registry's 64-char cap.
fn assert_distinct_short_operation_ids(calls: &OutlineCalls) {
    let ids: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|c| c["operationId"].as_str().unwrap_or_default().to_owned())
        .collect();
    for id in &ids {
        assert!(!id.is_empty() && id.len() <= 64, "{id}");
    }
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "{ids:?}");
}

fn undo_label(outline: &Entity<OutlineView>, vcx: &mut gpui::VisualTestContext) -> Option<String> {
    outline.read_with(vcx, |outline, cx| outline.next_undo_label(cx))
}

/// ⌘Z sends the inverse move through `moveWorkItem` under a fresh operation
/// id and says what it undid; ⇧⌘Z sends the move again.
#[gpui::test]
fn cmd_z_and_shift_cmd_z_undo_and_redo_a_move(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = undo_overview(cx, dir.path(), fixture(), reply_ok);
    select(&outline, vcx, UI);
    press(vcx, "shift-up");
    assert_eq!(count(&calls), 1);
    assert_eq!(call(&calls, 0)["placement"]["type"], "before");
    settle(&overview, vcx, fixture());

    press(vcx, "cmd-z");
    assert_eq!(count(&calls), 2);
    let undo = call(&calls, 1);
    assert_eq!(undo["op"], "moveWorkItem");
    assert_eq!(undo["workItemId"], UI);
    assert_eq!(undo["destinationParentWorkItemId"], LAUNCH);
    assert_eq!(undo["placement"]["type"], "after");
    assert_eq!(undo["placement"]["siblingWorkItemId"], API);
    assert_eq!(
        notice(&outline, vcx).as_deref(),
        Some("Undid: reordered \u{2018}UI\u{2019}")
    );
    // Undoing again has nothing left.
    press(vcx, "cmd-z");
    assert_eq!(count(&calls), 2);
    assert_eq!(notice(&outline, vcx).as_deref(), Some("Nothing to undo"));

    press(vcx, "cmd-shift-z");
    assert_eq!(count(&calls), 3);
    let redo = call(&calls, 2);
    assert_eq!(redo["op"], "moveWorkItem");
    assert_eq!(redo["placement"], call(&calls, 0)["placement"]);
    assert_eq!(
        notice(&outline, vcx).as_deref(),
        Some("Redid: reordered \u{2018}UI\u{2019}")
    );
    press(vcx, "cmd-shift-z");
    assert_eq!(count(&calls), 3);
    assert_distinct_short_operation_ids(&calls);
}

/// Ctrl-Z / Ctrl-Shift-Z work too.
#[gpui::test]
fn ctrl_z_and_ctrl_shift_z_also_undo_and_redo(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = undo_overview(cx, dir.path(), fixture(), reply_ok);
    select(&outline, vcx, UI);
    press(vcx, "shift-up");
    settle(&overview, vcx, fixture());
    press(vcx, "ctrl-z");
    assert_eq!(count(&calls), 2);
    press(vcx, "ctrl-shift-z");
    assert_eq!(count(&calls), 3);
}

/// With a text field focused (the inline rename box) ⌘Z is the field's own;
/// the Outline's undo only applies once focus is back on the tree.
#[gpui::test]
fn cmd_z_in_a_text_field_does_not_undo_the_outline(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = undo_overview(cx, dir.path(), fixture(), reply_ok);
    select(&outline, vcx, UI);
    press(vcx, "shift-up");
    settle(&overview, vcx, fixture());
    assert_eq!(count(&calls), 1);

    press(vcx, "f2");
    assert!(outline.read_with(vcx, |o, _| o.is_editing()));
    vcx.simulate_input("!");
    press(vcx, "cmd-z");
    press(vcx, "cmd-shift-z");
    press(vcx, "ctrl-z");
    assert_eq!(count(&calls), 1, "text-field undo must not reach the outline");
    assert!(outline.read_with(vcx, |o, _| o.is_editing()));
    assert_eq!(notice(&outline, vcx), None);

    outline.update_in(vcx, |outline, _, cx| outline.cancel_edit(cx));
    focus(&outline, vcx);
    press(vcx, "cmd-z");
    assert_eq!(count(&calls), 2, "back on the tree, ⌘Z undoes the move");
}

/// A rename is one undoable action: undo restores the old title through
/// `updateWorkItem`.
#[gpui::test]
fn rename_is_undoable_through_update_work_item(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = undo_overview(cx, dir.path(), fixture(), reply_ok);
    select(&outline, vcx, UI);
    press(vcx, "f2");
    draw(vcx);
    vcx.simulate_input("!");
    draw(vcx);
    press(vcx, "enter");
    assert_eq!(call(&calls, 0)["op"], "updateWorkItem");
    assert_eq!(call(&calls, 0)["changes"]["title"], "UI!");
    // The engine's frame shows the new title.
    let mut renamed = fixture();
    renamed
        .items
        .iter_mut()
        .find(|item| item.id == UI)
        .unwrap()
        .title = "UI!".into();
    settle(&overview, vcx, renamed.clone());

    press(vcx, "cmd-z");
    let undo = call(&calls, 1);
    assert_eq!(undo["op"], "updateWorkItem");
    assert_eq!(undo["workItemId"], UI);
    assert_eq!(undo["changes"]["title"], "UI");
    assert_eq!(
        notice(&outline, vcx).as_deref(),
        Some("Undid: renamed \u{2018}UI\u{2019} to \u{2018}UI!\u{2019}")
    );
    assert_distinct_short_operation_ids(&calls);
}

/// Undo is refused, with nothing sent, when the target changed since: here
/// someone archived the item after it was moved.
#[gpui::test]
fn undo_refuses_when_the_target_changed_since(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = undo_overview(cx, dir.path(), fixture(), reply_ok);
    select(&outline, vcx, UI);
    press(vcx, "shift-up");
    let mut changed = fixture();
    changed
        .items
        .iter_mut()
        .find(|item| item.id == UI)
        .unwrap()
        .archived_at = Some(5);
    settle(&overview, vcx, changed);

    press(vcx, "cmd-z");
    assert_eq!(count(&calls), 1, "nothing is sent on a conflict");
    let text = notice(&outline, vcx).expect("a message");
    assert!(text.starts_with("Can\u{2019}t undo"), "{text}");
    assert!(text.contains("archived"), "{text}");
    assert!(text.contains("Nothing was changed"), "{text}");
}

/// Changes nobody made from this UI (an agent's, another device's) are not on
/// the stack: a frame with different content leaves ⌘Z with nothing to do.
#[gpui::test]
fn changes_that_arrive_from_elsewhere_are_not_undoable(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = undo_overview(cx, dir.path(), fixture(), reply_ok);
    let mut remote = fixture();
    remote
        .items
        .iter_mut()
        .find(|item| item.id == UI)
        .unwrap()
        .title = "Renamed by an agent".into();
    settle(&overview, vcx, remote);
    select(&outline, vcx, UI);
    press(vcx, "cmd-z");
    assert_eq!(count(&calls), 0);
    assert_eq!(notice(&outline, vcx).as_deref(), Some("Nothing to undo"));
    assert_eq!(undo_label(&outline, vcx), None);
}

/// A failing undo reports the error, leaves the change on the stack, and a
/// retry sends it again.
#[gpui::test]
fn a_failed_undo_keeps_the_entry(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let moves = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = moves.clone();
    let (overview, outline, calls, vcx) = undo_overview(cx, dir.path(), fixture(), move |params| {
        if params["op"] == "moveWorkItem" && seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1
        {
            return Err(mast_rpc::RpcError::Failed("engine said no".into()));
        }
        reply_ok(params)
    });
    select(&outline, vcx, UI);
    press(vcx, "shift-up");
    settle(&overview, vcx, fixture());
    press(vcx, "cmd-z");
    assert_eq!(count(&calls), 2);
    assert!(
        outline
            .read_with(vcx, |o, _| o.error_notice().map(|(title, _)| title.to_owned()))
            .is_some()
    );
    assert!(undo_label(&outline, vcx).is_some(), "still undoable");
    press(vcx, "cmd-z");
    assert_eq!(count(&calls), 3);
    assert_eq!(undo_label(&outline, vcx), None);
}

/// "Recent changes" lists the last actions newest first; choosing an older
/// one undoes everything back to it.
#[gpui::test]
fn recent_changes_lists_actions_and_undoes_back_to_one(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = undo_overview(cx, dir.path(), fixture(), reply_ok);
    select(&outline, vcx, UI);
    press(vcx, "shift-up");
    settle(&overview, vcx, fixture());
    select(&outline, vcx, HOUSEKEEPING);
    press(vcx, "shift-up");
    settle(&overview, vcx, fixture());
    assert_eq!(count(&calls), 2);

    let recent = outline.read_with(vcx, |o, cx| o.recent_changes(cx));
    let labels: Vec<_> = recent.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "reordered \u{2018}Housekeeping\u{2019}",
            "reordered \u{2018}UI\u{2019}"
        ]
    );
    assert!(recent.iter().all(|r| !r.undone));

    vcx.update(|window, cx| {
        outline.update(cx, |outline, cx| {
            outline.open_recent_changes(window, cx).unwrap();
            outline.choose_picker_entry(1, window, cx);
        })
    });
    vcx.run_until_parked();
    assert_eq!(count(&calls), 4, "both moves are undone");
    assert_eq!(call(&calls, 2)["workItemId"], HOUSEKEEPING);
    assert_eq!(call(&calls, 3)["workItemId"], UI);
    assert_eq!(
        notice(&outline, vcx).as_deref(),
        Some("Undid 2 changes, back to: reordered \u{2018}UI\u{2019}")
    );
    // Both now show as undone and can be redone.
    let recent = outline.read_with(vcx, |o, cx| o.recent_changes(cx));
    assert!(recent.iter().all(|r| r.undone));
    assert_distinct_short_operation_ids(&calls);
}

/// With no history there is nothing to list.
#[gpui::test]
fn recent_changes_is_empty_without_history(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, _calls, vcx) = undo_overview(cx, dir.path(), fixture(), reply_ok);
    let opened = vcx.update(|window, cx| {
        outline.update(cx, |outline, cx| outline.open_recent_changes(window, cx))
    });
    assert!(opened.is_err());
}

/// The existing move toast's Undo still reverts the move and keeps the log in
/// step: the move is not undoable a second time, and ⇧⌘Z redoes it.
#[gpui::test]
fn the_move_toast_undo_and_the_log_agree(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = undo_overview(
        cx,
        dir.path(),
        instruction_fixture::snapshot("s1"),
        reply_ok,
    );
    // Moving a chat under Housekeeping changes its instructions: a toast.
    select(&outline, vcx, HOUSEKEEPING);
    vcx.update(|window, cx| {
        outline.update(cx, |outline, cx| {
            outline.open_associate_picker(None, window, cx).unwrap();
            outline.choose_picker_entry(0, window, cx);
        })
    });
    vcx.run_until_parked();
    assert!(outline.read_with(vcx, |o, _| o.move_toast_text()).is_some());
    assert!(undo_label(&outline, vcx).is_some());
    settle(&overview, vcx, instruction_fixture::snapshot("s1"));

    outline.update(vcx, |outline, cx| outline.undo_move(cx));
    vcx.run_until_parked();
    assert_eq!(count(&calls), 2);
    assert_eq!(undo_label(&outline, vcx), None, "the log knows it was undone");
    let redo = outline.read_with(vcx, |o, cx| o.next_redo_label(cx));
    assert!(redo.is_some_and(|label| label.starts_with("moved chat")));
}

/// Work Item Details edits share the log: a status change made there is
/// undone with ⌘Z from the Outline, through `updateWorkItem`.
#[gpui::test]
fn a_status_change_in_details_is_undone_from_the_outline(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = undo_overview(cx, dir.path(), fixture(), reply_ok);
    let details = open_details(&overview, vcx, API);
    details.update(vcx, |d, cx| {
        d.set_status(mast_proto::WorkItemStatus::Pending, cx)
    });
    vcx.run_until_parked();
    assert_eq!(call(&calls, 0)["changes"]["status"], "pending");
    let mut changed = fixture();
    changed
        .items
        .iter_mut()
        .find(|item| item.id == API)
        .unwrap()
        .status = mast_proto::WorkItemStatus::Pending;
    settle(&overview, vcx, changed);
    focus(&outline, vcx);

    assert_eq!(
        undo_label(&outline, vcx).as_deref(),
        Some("marked \u{2018}API\u{2019} pending")
    );
    press(vcx, "cmd-z");
    let undo = call(&calls, 1);
    assert_eq!(undo["op"], "updateWorkItem");
    assert_eq!(undo["workItemId"], API);
    assert_eq!(undo["changes"]["status"], "active");
    assert_distinct_short_operation_ids(&calls);
}

/// Restoring an archived item is undone by archiving it again.
#[gpui::test]
fn restoring_an_item_is_undone_by_archiving_it_again(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = undo_overview(cx, dir.path(), fixture(), reply_ok);
    let details = open_details(&overview, vcx, RETIRED);
    details.update(vcx, |d, cx| d.restore(cx));
    vcx.run_until_parked();
    assert_eq!(call(&calls, 0)["op"], "restoreWorkItem");
    let mut restored = fixture();
    restored
        .items
        .iter_mut()
        .find(|item| item.id == RETIRED)
        .unwrap()
        .archived_at = None;
    settle(&overview, vcx, restored);
    focus(&outline, vcx);

    press(vcx, "cmd-z");
    let undo = call(&calls, 1);
    assert_eq!(undo["op"], "archiveWorkItem");
    assert_eq!(undo["workItemId"], RETIRED);
    assert_eq!(undo["descendantArchivePolicy"]["type"], "archiveDescendants");
}

/// An instruction edit is undone by writing the old text back; the engine
/// call is the ordinary `updateWorkItemInstruction`.
#[gpui::test]
fn an_instruction_edit_is_undone_with_the_old_text(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mut bare = instruction_fixture::snapshot("s1");
    bare.instructions
        .retain(|i| i.work_item_id.as_deref() == Some(HOUSEKEEPING));
    bare.instructions.push(instruction_fixture::instruction(
        "i-house",
        "s1",
        Some(HOUSEKEEPING),
        "a0",
        "Tidy up.",
    ));
    let (overview, outline, calls, vcx) = undo_overview(cx, dir.path(), bare.clone(), reply_ok);
    let details = open_details(&overview, vcx, HOUSEKEEPING);
    vcx.update(|window, cx| {
        details.update(cx, |d, cx| {
            d.begin_instruction_draft(Some("i-house".into()), window, cx);
            d.set_instruction_text("Tidy more.", cx);
            d.save_instruction(cx);
        })
    });
    vcx.run_until_parked();
    assert_eq!(call(&calls, 0)["op"], "updateWorkItemInstruction");
    let mut edited = bare;
    edited
        .instructions
        .iter_mut()
        .find(|i| i.id == "i-house")
        .unwrap()
        .text = "Tidy more.".into();
    settle(&overview, vcx, edited);
    focus(&outline, vcx);

    press(vcx, "cmd-z");
    let undo = call(&calls, 1);
    assert_eq!(undo["op"], "updateWorkItemInstruction");
    assert_eq!(undo["instructionId"], "i-house");
    assert_eq!(undo["text"], "Tidy up.");
    assert_distinct_short_operation_ids(&calls);
}

/// A compact row (an item with exactly one chat) is the item: moving it
/// records and undoes like any item, and its right-click menu has the item's
/// actions, the chat's, and the history entries.
#[gpui::test]
fn compact_rows_record_and_undo_like_items(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = undo_overview(cx, dir.path(), fixture(), reply_ok);
    // UI holds only chat-ui: it renders as one compact row.
    select(&outline, vcx, UI);
    assert_eq!(
        outline.read_with(vcx, |o, _| o.selected_key().map(str::to_owned)),
        Some(row_key::item("s1", UI))
    );
    press(vcx, "shift-up");
    assert_eq!(call(&calls, 0)["workItemId"], UI);
    settle(&overview, vcx, fixture());
    press(vcx, "cmd-z");
    let undo = call(&calls, 1);
    assert_eq!(undo["op"], "moveWorkItem");
    assert_eq!(undo["workItemId"], UI);
    assert_eq!(
        notice(&outline, vcx).as_deref(),
        Some("Undid: reordered \u{2018}UI\u{2019}")
    );
    let menu = outline
        .read_with(vcx, |o, cx| o.context_menu(&row_key::item("s1", UI), cx))
        .expect("menu");
    let ids: Vec<_> = menu.entries.iter().map(|e| e.id).collect();
    for id in ["open-chat", "start-chat", "archive", "undo", "redo", "recent-changes"] {
        assert!(ids.contains(&id), "{id} in {ids:?}");
    }
}
