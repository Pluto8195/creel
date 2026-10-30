// "Convert to project…": the pure plan (matching by title path, default moves,
// ids, order), the preview that sends nothing, Apply, and ONE undo step.
// Included from `overview.rs`'s test module for its fixtures. Fixture
// snapshots and scripted replies only.
use super::*;

use crate::work_outline::convert;
use crate::work_outline::outline_view::OutlineView;

type ConvertCalls = std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>;

fn cnode(title: &str, children: Vec<mast_proto::TemplateNode>) -> mast_proto::TemplateNode {
    mast_proto::TemplateNode {
        title: title.into(),
        description: None,
        is_project: false,
        is_worker_destination: false,
        instructions: Vec::new(),
        children,
    }
}

/// "Launch kit": Planning ("Plan it.") and Tickets (the worker destination).
fn convert_template() -> mast_proto::WorkItemTemplateView {
    let mut planning = cnode("Planning", vec![]);
    planning.instructions = vec!["Plan it.".into()];
    let mut tickets = cnode("Tickets", vec![]);
    tickets.is_worker_destination = true;
    mast_proto::WorkItemTemplateView {
        id: "t-kit".into(),
        space_id: "s1".into(),
        name: "Launch kit".into(),
        revision: 3,
        body: cnode("Launch kit", vec![planning, tickets]),
        created_at: 1,
        updated_at: 2,
    }
}

/// Launch v2 (children API, UI): API carries an external link, i.e. it is
/// ticket-like.
fn convert_fixture() -> WorkOutlineSnapshot {
    let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
    snapshot.templates = vec![convert_template()];
    let api = snapshot.items.iter_mut().find(|i| i.id == API).unwrap();
    api.external_reference_ids = vec!["ref-api".into()];
    snapshot
}

fn plan_for(snapshot: &WorkOutlineSnapshot) -> convert::ConvertPlan {
    let template = &snapshot.templates[0];
    let choices = convert::default_moves(snapshot, LAUNCH, template);
    convert::plan_conversion(snapshot, LAUNCH, template, &choices, "key").unwrap()
}

fn ops_of(plan: &convert::ConvertPlan) -> Vec<&'static str> {
    plan.mutations.iter().map(|m| m.op_name()).collect()
}

#[test]
fn the_plan_creates_the_template_nodes_moves_tickets_and_links() {
    let snapshot = convert_fixture();
    let plan = plan_for(&snapshot);
    assert_eq!(
        ops_of(&plan),
        [
            "createWorkItem",
            "createWorkItemInstruction",
            "createWorkItem",
            "moveWorkItem",
            "updateWorkItem",
            "updateWorkItem",
            "updateWorkItem",
        ]
    );
    assert_eq!(plan.created, ["Planning", "Tickets"]);
    assert_eq!(plan.created_roots, plan.created_ids);
    // Only the ticket-like child moves, and it goes to the destination.
    assert_eq!(plan.moves.len(), 1);
    assert_eq!(plan.moves[0].child_id, API);
    assert_eq!(plan.moves[0].to_label, "Tickets");
    assert_eq!(plan.destination.as_ref().unwrap().1, "Tickets");
    assert!(plan.marks_project);
    assert_eq!(plan.link_revision, 3, "no instruction differs on existing nodes");
    for mutation in &plan.mutations {
        assert!(mutation.operation_id().len() <= 64);
    }
    for id in &plan.created_ids {
        assert!(id.len() <= 64);
    }
    // Deterministic: the same key plans the same ids.
    assert_eq!(plan, plan_for(&snapshot));
    let lines = convert::preview_lines(&plan);
    assert!(lines.contains(&"Create \u{201c}Planning\u{201d}".to_owned()));
    assert!(lines.contains(&"Move \u{201c}API\u{201d} under \u{201c}Tickets\u{201d}".to_owned()));
    assert!(lines.contains(&"Chats stay where they are".to_owned()));
}

#[test]
fn existing_nodes_are_reused_unticked_children_stay_and_projects_are_refused() {
    let mut snapshot = convert_fixture();
    // An existing child named like a template node is reused, never moved.
    snapshot = crate::work_outline::optimistic::apply(
        &snapshot,
        &crate::work_outline::optimistic::LocalChange::Create {
            work_item_id: "w-existing-planning".into(),
            parent: Some(LAUNCH.into()),
            placement: mast_proto::WorkOutlinePlacement::Last,
            title: "Planning".into(),
            created_at: 1,
        },
    )
    .unwrap();
    let template = snapshot.templates[0].clone();
    let mut choices = convert::default_moves(&snapshot, LAUNCH, &template);
    // The user unticks API.
    for choice in &mut choices {
        if choice.0 == API {
            choice.1 = None;
        }
    }
    let plan = convert::plan_conversion(&snapshot, LAUNCH, &template, &choices, "key").unwrap();
    assert_eq!(plan.created, ["Tickets"], "Planning already exists");
    assert!(plan.moves.is_empty(), "the unticked child stays");
    // Planning exists but lacks the template's instruction: reported, and the
    // link says "not reviewed yet" so the chip invites the review.
    assert_eq!(plan.unapplied, ["Planning: 1"]);
    assert_eq!(plan.link_revision, 0);
    assert!(!ops_of(&plan).contains(&"createWorkItemInstruction"));

    let mut project = snapshot.clone();
    project.items.iter_mut().find(|i| i.id == LAUNCH).unwrap().is_project = true;
    assert!(convert::plan_conversion(&project, LAUNCH, &template, &choices, "key").is_err());
}

fn convert_overview<'a>(
    cx: &'a mut gpui::TestAppContext,
    dir: &std::path::Path,
) -> (
    Entity<Overview>,
    Entity<OutlineView>,
    ConvertCalls,
    &'a mut gpui::VisualTestContext,
) {
    let (overview, state, vcx) = outline_overview(cx, dir);
    vcx.update(|_, cx| {
        crate::motion::set_reduced_motion(cx, true);
        crate::composer::init(cx, crate::settings::ComposerSendBehavior::default());
        cx.bind_keys(crate::work_outline::outline_view::key_bindings());
    });
    let calls: ConvertCalls = Default::default();
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
        o.set_outline_loads(fixture_loads(Some(convert_fixture())), cx);
        o.set_mode(OverviewMode::Outline, cx);
    });
    let outline = overview.read_with(vcx, |o, _| o.outline.clone());
    draw(vcx);
    (overview, outline, calls, vcx)
}

fn mutations_sent(calls: &ConvertCalls) -> Vec<serde_json::Value> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, _)| method == "WorkOutlineMutate")
        .map(|(_, params)| params.clone())
        .collect()
}

fn click(vcx: &mut gpui::VisualTestContext, selector: &str) {
    let at = row_bounds(vcx, selector.to_string())
        .unwrap_or_else(|| panic!("{selector} renders"))
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    draw(vcx);
}

#[gpui::test]
fn the_wizard_previews_without_writing_then_applies_as_one_undo_step(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) = convert_overview(cx, dir.path());
    open_details(&overview, vcx, LAUNCH);
    click(vcx, "work-item-convert-project");
    click(vcx, "convert-template-t-kit");
    assert!(row_bounds(vcx, "convert-preview".to_string()).is_some(), "the preview shows");
    assert!(
        mutations_sent(&calls).is_empty(),
        "opening the wizard and previewing send nothing"
    );

    // Cancel writes nothing either.
    click(vcx, "work-item-convert-cancel");
    assert!(mutations_sent(&calls).is_empty());
    assert!(row_bounds(vcx, "convert-preview".to_string()).is_none());

    // Apply: the plan goes out in order.
    click(vcx, "work-item-convert-project");
    click(vcx, "convert-template-t-kit");
    click(vcx, "work-item-convert-apply");
    vcx.run_until_parked();
    let ops: Vec<String> = mutations_sent(&calls)
        .iter()
        .map(|m| m["op"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        ops,
        [
            "createWorkItem",
            "createWorkItemInstruction",
            "createWorkItem",
            "moveWorkItem",
            "updateWorkItem",
            "updateWorkItem",
            "updateWorkItem",
        ]
    );
    let sent = mutations_sent(&calls);
    assert_eq!(sent[4]["changes"]["isProject"], true);
    assert_eq!(sent[6]["changes"]["template"]["templateId"], "t-kit");

    // ONE undo step for the whole conversion.
    let label = outline
        .read_with(vcx, |o, cx| o.next_undo_label(cx))
        .expect("an undo step");
    assert!(label.contains("converted"), "{label}");
    let entry = {
        let snapshot = convert_fixture();
        let plan = plan_for(&snapshot);
        crate::work_outline::action_log::convert_entry(&snapshot, &plan, None).unwrap()
    };
    use crate::work_outline::action_log::Step;
    let kinds = |step: &Step| -> Vec<&'static str> {
        step.leaves()
            .iter()
            .map(|leaf| match leaf {
                Step::Move { .. } => "move",
                Step::Update { .. } => "update",
                Step::Archive { .. } => "archive",
                Step::Restore { .. } => "restore",
                _ => "other",
            })
            .collect()
    };
    assert_eq!(kinds(&entry.inverse), ["move", "update", "archive", "archive"]);
    assert_eq!(kinds(&entry.forward), ["restore", "restore", "move", "update"]);
}

#[gpui::test]
fn the_convert_entry_is_disabled_with_a_reason_for_projects_and_without_templates(
    cx: &mut gpui::TestAppContext,
) {
    let mut snapshot = convert_fixture();
    assert_eq!(
        crate::work_outline::templates::convert_blocked(Some(&snapshot), LAUNCH),
        None
    );
    snapshot.items.iter_mut().find(|i| i.id == LAUNCH).unwrap().is_project = true;
    assert_eq!(
        crate::work_outline::templates::convert_blocked(Some(&snapshot), LAUNCH),
        Some("This item is already a project")
    );
    snapshot.templates.clear();
    snapshot.items.iter_mut().find(|i| i.id == LAUNCH).unwrap().is_project = false;
    assert!(
        crate::work_outline::templates::convert_blocked(Some(&snapshot), LAUNCH)
            .unwrap()
            .starts_with("Save a template first")
    );
    let _ = cx;
}
