// Work Item Details "Project" section: the toggle and the worker destination
// send `updateWorkItem` with the new fields, and neither is an undo step.
// Included from `overview.rs`'s test module so they share its fixtures.
use super::*;

#[gpui::test]
fn the_project_toggle_and_worker_destination_send_updates_and_are_not_undoable(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = instruction_overview(
        cx,
        dir.path(),
        instruction_fixture::snapshot("s1"),
        reply_ok,
    );
    let details = open_details(&overview, vcx, HOUSEKEEPING);
    let at = vcx
        .debug_bounds("work-item-project-toggle")
        .expect("project toggle")
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    let last = calls.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last["op"], "updateWorkItem");
    assert_eq!(last["changes"]["isProject"], true);

    details.update(vcx, |d, cx| {
        d.set_worker_destination(Some("tickets".into()), cx)
    });
    vcx.run_until_parked();
    let last = calls.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last["changes"]["workerDestination"], "tickets");
    details.update(vcx, |d, cx| d.set_worker_destination(None, cx));
    vcx.run_until_parked();
    let last = calls.lock().unwrap().last().cloned().unwrap();
    assert!(last["changes"]["workerDestination"].is_null());
    assert!(last["changes"].get("workerDestination").is_some());

    // Project settings never land on the undo stack.
    let snapshot = instruction_fixture::snapshot("s1");
    let changes = mast_proto::WorkItemChanges {
        is_project: Some(true),
        ..Default::default()
    };
    assert!(
        crate::work_outline::action_log::update_entry(&snapshot, HOUSEKEEPING, &changes, None)
            .is_none()
    );
}
