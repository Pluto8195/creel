// "Template changed": the chip (Outline row and Details), the per-change
// review list, Accept / Skip / Accept all / Mark reviewed, and that removals
// never apply. Included from `overview.rs`'s test module for its fixtures.
// Fixture snapshots and scripted replies only; no engine, no live data.
use super::*;

use crate::work_outline::optimistic::{self, LocalChange};
use crate::work_outline::outline_view::OutlineView;
use crate::work_outline::template_review as review;

type ReviewCalls = std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>;

const DOCS: &str = "w-docs";
const TICKETS: &str = "w-tickets";
const LEGACY: &str = "w-legacy";
const DOCS_OLD_RULE: &str = "i-docs-old";

fn node(title: &str, children: Vec<mast_proto::TemplateNode>) -> mast_proto::TemplateNode {
    mast_proto::TemplateNode {
        title: title.into(),
        description: None,
        is_project: false,
        is_worker_destination: false,
        instructions: Vec::new(),
        children,
    }
}

/// Housekeeping > Tickets (destination), Docs ("Keep it short."), Extra
/// ("Be brief."). Revision `revision`.
fn review_template(revision: u32) -> mast_proto::WorkItemTemplateView {
    let mut tickets = node("Tickets", vec![]);
    tickets.is_worker_destination = true;
    let mut docs = node("Docs", vec![]);
    docs.instructions = vec!["Keep it short.".into()];
    let mut extra = node("Extra", vec![]);
    extra.instructions = vec!["Be brief.".into()];
    let mut root = node("Housekeeping", vec![tickets, docs, extra]);
    root.is_project = true;
    mast_proto::WorkItemTemplateView {
        id: "t-1".into(),
        space_id: "s1".into(),
        name: "Housekeeping kit".into(),
        revision,
        body: root,
        created_at: 1,
        updated_at: 2,
    }
}

/// Housekeeping (project, linked to t-1 at `linked`) with Tickets, Docs
/// (instruction "Old rule") and Legacy (not in the template). The template
/// is at revision 2. Diff, in order: 0 + "Keep it short." on Docs, 1 - "Old
/// rule" on Docs, 2 + item Extra, 3 ! Legacy, 4 ~ worker destination.
fn review_fixture(linked: Option<u32>) -> WorkOutlineSnapshot {
    let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
    snapshot.templates = vec![review_template(2)];
    for (id, title) in [(TICKETS, "Tickets"), (DOCS, "Docs"), (LEGACY, "Legacy")] {
        snapshot = optimistic::apply(
            &snapshot,
            &LocalChange::Create {
                work_item_id: id.into(),
                parent: Some(HOUSEKEEPING.into()),
                placement: mast_proto::WorkOutlinePlacement::Last,
                title: title.into(),
                created_at: 1,
            },
        )
        .unwrap();
    }
    snapshot.instructions.push(mast_proto::WorkItemInstructionView {
        id: DOCS_OLD_RULE.into(),
        space_id: "s1".into(),
        work_item_id: Some(DOCS.into()),
        text: "Old rule".into(),
        order_key: "a0".into(),
        created_by: None,
        created_at: 1,
        updated_at: 1,
        source: None,
        source_proposal_id: None,
        archived_at: None,
        archived_by: None,
    });
    let project = snapshot
        .items
        .iter_mut()
        .find(|item| item.id == HOUSEKEEPING)
        .unwrap();
    project.is_project = true;
    if let Some(revision) = linked {
        project.template_id = Some("t-1".into());
        project.template_revision = Some(revision);
    }
    snapshot
}

fn review_overview<'a>(
    cx: &'a mut gpui::TestAppContext,
    dir: &std::path::Path,
    snapshot: WorkOutlineSnapshot,
) -> (
    Entity<Overview>,
    Entity<OutlineView>,
    ReviewCalls,
    &'a mut gpui::VisualTestContext,
) {
    let (overview, state, vcx) = outline_overview(cx, dir);
    vcx.update(|_, cx| {
        crate::motion::set_reduced_motion(cx, true);
        crate::composer::init(cx, crate::settings::ComposerSendBehavior::default());
        cx.bind_keys(crate::work_outline::outline_view::key_bindings());
    });
    let calls: ReviewCalls = Default::default();
    let sink = calls.clone();
    state.update(vcx, |state, _| {
        state.work_outline_actions_override =
            Some(crate::work_outline::WorkOutlineActions::scripted(
                std::sync::Arc::new(move |method, params| {
                    sink.lock()
                        .unwrap()
                        .push((method.to_string(), params.clone()));
                    Ok(serde_json::json!({}))
                }),
            ));
    });
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(snapshot)), cx);
        o.set_mode(OverviewMode::Outline, cx);
    });
    let outline = overview.read_with(vcx, |o, _| o.outline.clone());
    draw(vcx);
    (overview, outline, calls, vcx)
}

fn sent(calls: &ReviewCalls) -> Vec<serde_json::Value> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, _)| method == "WorkOutlineMutate")
        .map(|(_, params)| params.clone())
        .collect()
}

fn press(vcx: &mut gpui::VisualTestContext, selector: &str) {
    let at = row_bounds(vcx, selector.to_string())
        .unwrap_or_else(|| panic!("{selector} renders"))
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    draw(vcx);
}

fn shown(vcx: &mut gpui::VisualTestContext, selector: &str) -> bool {
    row_bounds(vcx, selector.to_string()).is_some()
}

fn row_chip(id: &str) -> String {
    format!("outline-template-changed-{}", row_key::item("s1", id))
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

// --------------------------------------------------------------------------
// The chip
// --------------------------------------------------------------------------

#[gpui::test]
fn the_chip_shows_only_when_the_linked_revision_is_behind(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) =
        review_overview(cx, dir.path(), review_fixture(Some(1)));
    assert!(shown(vcx, &row_chip(HOUSEKEEPING)), "behind: the row chip");
    outline.read_with(vcx, |o, _| {
        let row = o.row_for_test(&row_key::item("s1", HOUSEKEEPING)).unwrap();
        let crate::work_outline::model::OutlineRowKind::Item(item) = &row.kind else {
            panic!("an item row");
        };
        assert_eq!(item.template_changed, Some((1, 2)));
    });
    assert_eq!(
        review::chip_text(1, 2),
        "Template changed (rev 1 \u{2192} 2)",
        "the text names both revisions"
    );
    open_details(&overview, vcx, HOUSEKEEPING);
    assert!(shown(vcx, "work-item-template-changed"));
    assert!(shown(vcx, "work-item-review-template"));
    assert!(sent(&calls).is_empty(), "a chip writes nothing");

    // In sync, unlinked, and a link to a deleted template: no chip.
    for snapshot in [
        review_fixture(Some(2)),
        review_fixture(None),
        {
            let mut snapshot = review_fixture(Some(1));
            snapshot.templates.clear();
            snapshot
        },
    ] {
        settle(&overview, vcx, snapshot);
        assert!(!shown(vcx, &row_chip(HOUSEKEEPING)));
        assert!(!shown(vcx, "work-item-template-changed"));
        assert!(!shown(vcx, "work-item-review-template"));
    }
}

#[gpui::test]
fn a_missing_revision_counts_as_zero_and_archived_projects_show_no_chip(
    cx: &mut gpui::TestAppContext,
) {
    let snapshot = {
        let mut snapshot = review_fixture(Some(1));
        let project = snapshot
            .items
            .iter_mut()
            .find(|item| item.id == HOUSEKEEPING)
            .unwrap();
        project.template_revision = None;
        snapshot
    };
    let project = snapshot.item(HOUSEKEEPING).unwrap();
    assert_eq!(review::chip_revisions(&snapshot, project), Some((0, 2)));
    let mut archived = snapshot.clone();
    archived
        .items
        .iter_mut()
        .find(|item| item.id == HOUSEKEEPING)
        .unwrap()
        .archived_at = Some(5);
    let project = archived.item(HOUSEKEEPING).unwrap();
    assert_eq!(review::chip_revisions(&archived, project), None);
    let _ = cx;
}

// --------------------------------------------------------------------------
// The review list
// --------------------------------------------------------------------------

#[gpui::test]
fn the_review_lists_every_change_kind_and_writes_nothing(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _outline, calls, vcx) =
        review_overview(cx, dir.path(), review_fixture(Some(1)));
    let details = open_details(&overview, vcx, HOUSEKEEPING);
    assert!(!shown(vcx, "work-item-review-row-0"), "closed until asked");
    press(vcx, "work-item-review-template");
    for ix in 0..5 {
        assert!(shown(vcx, &format!("work-item-review-row-{ix}")), "row {ix}");
    }
    assert!(!shown(vcx, "work-item-review-row-5"));
    let kinds = details.read_with(vcx, |d, _| {
        d.templates
            .review
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .map(|row| review::describe(&row.change).0)
            .collect::<Vec<_>>()
    });
    assert_eq!(kinds, ["+", "-", "+", "!", "~"]);
    for ix in [0, 1, 2, 4] {
        assert!(shown(vcx, &format!("work-item-review-accept-{ix}")), "accept {ix}");
        assert!(shown(vcx, &format!("work-item-review-skip-{ix}")));
    }
    // The removal is information only.
    assert!(!shown(vcx, "work-item-review-accept-3"));
    assert!(!shown(vcx, "work-item-review-skip-3"));
    assert!(shown(vcx, "work-item-review-accept-all"));
    assert!(shown(vcx, "work-item-review-mark-reviewed"));
    assert!(sent(&calls).is_empty(), "listing writes nothing");
}

#[gpui::test]
fn accept_sends_the_exact_mutations_and_the_last_one_advances_the_link(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) =
        review_overview(cx, dir.path(), review_fixture(Some(1)));
    open_details(&overview, vcx, HOUSEKEEPING);
    press(vcx, "work-item-review-template");

    press(vcx, "work-item-review-accept-0");
    let all = sent(&calls);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0]["op"], "createWorkItemInstruction");
    assert_eq!(all[0]["scope"]["workItemId"], DOCS);
    assert_eq!(all[0]["text"], "Keep it short.");

    press(vcx, "work-item-review-accept-1");
    let all = sent(&calls);
    assert_eq!(all.len(), 2);
    assert_eq!(all[1]["op"], "archiveWorkItemInstruction");
    assert_eq!(all[1]["instructionId"], DOCS_OLD_RULE);

    press(vcx, "work-item-review-accept-2");
    let all = sent(&calls);
    assert_eq!(all.len(), 4, "the added item and its instruction");
    assert_eq!(all[2]["op"], "createWorkItem");
    assert_eq!(all[2]["title"], "Extra");
    assert_eq!(all[2]["parentWorkItemId"], HOUSEKEEPING);
    let extra = mast_proto::derive_id(all[2]["operationId"].as_str().unwrap(), mast_proto::roles::ITEM);
    assert_eq!(all[3]["op"], "createWorkItemInstruction");
    assert_eq!(all[3]["scope"]["workItemId"], extra);
    assert_eq!(all[3]["text"], "Be brief.");
    assert!(
        all.iter().all(|m| m["op"] != "updateWorkItem"),
        "the link waits for the last change"
    );

    press(vcx, "work-item-review-accept-4");
    let all = sent(&calls);
    assert_eq!(all.len(), 6);
    assert_eq!(all[4]["op"], "updateWorkItem");
    assert_eq!(all[4]["workItemId"], HOUSEKEEPING);
    assert_eq!(all[4]["changes"]["workerDestination"], TICKETS, "matched by title path");
    assert_eq!(all[5]["op"], "updateWorkItem");
    assert_eq!(all[5]["changes"]["template"]["templateId"], "t-1");
    assert_eq!(all[5]["changes"]["template"]["revision"], 2);
    for m in &all {
        let id = m["operationId"].as_str().unwrap();
        assert!(!id.is_empty() && id.len() <= 64, "{id}");
    }
    assert!(
        outline.read_with(vcx, |o, cx| o.next_undo_label(cx)).is_none(),
        "applying a review is not an undo step"
    );
}

#[gpui::test]
fn skip_sends_nothing_and_a_skipped_change_keeps_the_link_back(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _outline, calls, vcx) =
        review_overview(cx, dir.path(), review_fixture(Some(1)));
    let details = open_details(&overview, vcx, HOUSEKEEPING);
    press(vcx, "work-item-review-template");
    press(vcx, "work-item-review-skip-0");
    assert!(sent(&calls).is_empty(), "Skip writes nothing");
    assert!(!shown(vcx, "work-item-review-accept-0"), "a skipped row has no buttons");
    details.read_with(vcx, |d, _| {
        let rows = &d.templates.review.as_ref().unwrap().rows;
        assert_eq!(rows[0].state, crate::work_outline::details_panel::RowStateForTests::skipped());
    });

    press(vcx, "work-item-review-accept-all");
    let all = sent(&calls);
    let ops: Vec<&str> = all.iter().map(|m| m["op"].as_str().unwrap()).collect();
    assert_eq!(
        ops,
        [
            "archiveWorkItemInstruction",
            "createWorkItem",
            "createWorkItemInstruction",
            "updateWorkItem",
        ],
        "everything pending, not the skipped row, and no link: {ops:?}"
    );
    assert!(all[3]["changes"].get("template").is_none());
    assert!(shown(vcx, "work-item-template-changed"), "the chip stays");
}

#[gpui::test]
fn mark_reviewed_sends_only_the_link_update(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) =
        review_overview(cx, dir.path(), review_fixture(Some(1)));
    open_details(&overview, vcx, HOUSEKEEPING);
    press(vcx, "work-item-review-template");
    press(vcx, "work-item-review-mark-reviewed");
    let all = sent(&calls);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0]["op"], "updateWorkItem");
    assert_eq!(all[0]["workItemId"], HOUSEKEEPING);
    let changes = all[0]["changes"].as_object().unwrap();
    assert_eq!(changes["template"]["templateId"], "t-1");
    assert_eq!(changes["template"]["revision"], 2);
    for key in ["title", "description", "status", "isProject", "workerDestination"] {
        assert!(!changes.contains_key(key), "{key} untouched: {changes:?}");
    }
    assert!(outline.read_with(vcx, |o, cx| o.next_undo_label(cx)).is_none());

    // The settled snapshot clears the chip.
    let mut settled = review_fixture(Some(2));
    settled.templates = vec![review_template(2)];
    settle(&overview, vcx, settled);
    assert!(!shown(vcx, &row_chip(HOUSEKEEPING)));
    assert!(!shown(vcx, "work-item-template-changed"));
}

#[gpui::test]
fn accept_all_applies_every_change_then_the_settled_snapshot_clears_the_chip(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _outline, calls, vcx) =
        review_overview(cx, dir.path(), review_fixture(Some(1)));
    open_details(&overview, vcx, HOUSEKEEPING);
    press(vcx, "work-item-review-template");
    press(vcx, "work-item-review-accept-all");
    let all = sent(&calls);
    let ops: Vec<&str> = all.iter().map(|m| m["op"].as_str().unwrap()).collect();
    assert_eq!(
        ops,
        [
            "createWorkItemInstruction",
            "archiveWorkItemInstruction",
            "createWorkItem",
            "createWorkItemInstruction",
            "updateWorkItem",
            "updateWorkItem",
        ]
    );
    assert_eq!(all[5]["changes"]["template"]["revision"], 2, "the link advances last");
    assert!(
        all.iter().all(|m| m["op"] != "deleteWorkItem" && m["op"] != "archiveWorkItem"),
        "a removed item is never touched"
    );
    // Until the snapshot settles the chip is still the snapshot's.
    assert!(shown(vcx, &row_chip(HOUSEKEEPING)));
    settle(&overview, vcx, review_fixture(Some(2)));
    assert!(!shown(vcx, &row_chip(HOUSEKEEPING)));
    assert!(!shown(vcx, "work-item-template-changed"));
    assert!(!shown(vcx, "work-item-review-row-0"), "the review closed");
}

#[gpui::test]
fn a_failing_change_stays_pending_and_the_link_does_not_move(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) =
        review_overview(cx, dir.path(), review_fixture(Some(1)));
    outline.update(vcx, |_, _| {});
    // Fail instruction creation.
    let state = overview.read_with(vcx, |o, _| o.state.clone());
    let sink = calls.clone();
    state.update(vcx, |state, _| {
        state.work_outline_actions_override =
            Some(crate::work_outline::WorkOutlineActions::scripted(
                std::sync::Arc::new(move |method, params| {
                    sink.lock()
                        .unwrap()
                        .push((method.to_string(), params.clone()));
                    if params["op"] == "createWorkItemInstruction" {
                        return Err(mast_rpc::RpcError::Failed(
                            "work-outline/limitExceeded: too many instructions".into(),
                        ));
                    }
                    Ok(serde_json::json!({}))
                }),
            ));
    });
    open_details(&overview, vcx, HOUSEKEEPING);
    press(vcx, "work-item-review-template");
    press(vcx, "work-item-review-accept-all");
    let ops: Vec<String> = sent(&calls)
        .iter()
        .map(|m| m["op"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ops, ["createWorkItemInstruction"], "stops at the failure");
    assert!(shown(vcx, "work-item-review-error"));
    assert!(shown(vcx, "work-item-review-accept-0"), "can retry");
}

// --------------------------------------------------------------------------
// Pure planning
// --------------------------------------------------------------------------

#[test]
fn a_removed_item_has_no_mutations_and_the_destination_needs_its_item() {
    let snapshot = review_fixture(Some(1));
    let removed = mast_proto::TemplateChange::RemovedItem {
        path: vec!["Legacy".into()],
    };
    assert!(!review::applicable(&removed));
    let mut overlay = review::Overlay::new();
    assert!(review::plan_change(&snapshot, HOUSEKEEPING, &removed, "k", &mut overlay).is_err());
    let missing = mast_proto::TemplateChange::WorkerDestinationChanged {
        from: None,
        to: Some(vec!["Nope".into()]),
    };
    assert!(review::plan_change(&snapshot, HOUSEKEEPING, &missing, "k", &mut overlay).is_err());
    let cleared = mast_proto::TemplateChange::WorkerDestinationChanged {
        from: Some(vec!["Tickets".into()]),
        to: None,
    };
    let plan = review::plan_change(&snapshot, HOUSEKEEPING, &cleared, "k", &mut overlay).unwrap();
    assert_eq!(plan.len(), 1);
    let _ = LEGACY;
}
