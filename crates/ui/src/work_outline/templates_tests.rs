// Project templates UI: Save as template, the Space's Templates list, "New
// from template…" (one ordered mutation sequence, ONE undo step), the
// disabled states, error surfacing, and the Linear entry. Included from
// `overview.rs`'s test module so they share its Overview fixtures. Every
// reply is scripted; nothing reaches Linear or the engine.
use super::*;

use crate::linear::fixture as lf;
use crate::work_outline::optimistic::{self, LocalChange};
use crate::work_outline::outline_view::OutlineView;
use crate::work_outline::outline_view::templates::TemplateFlow;

type TemplateCalls = std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>;

fn template_node(title: &str, children: Vec<mast_proto::TemplateNode>) -> mast_proto::TemplateNode {
    mast_proto::TemplateNode {
        title: title.into(),
        description: None,
        is_project: false,
        is_worker_destination: false,
        instructions: Vec::new(),
        children,
    }
}

/// "Launch" (project) > "Tickets" (worker destination) + "Docs" (one
/// instruction): 3 items, 1 instruction, then mark / destination / link.
fn launch_template(revision: u32) -> mast_proto::WorkItemTemplateView {
    let mut tickets = template_node("Tickets", vec![]);
    tickets.is_worker_destination = true;
    let mut docs = template_node("Docs", vec![]);
    docs.description = Some("Write the docs".into());
    docs.instructions = vec!["Keep it short.".into()];
    let mut root = template_node("Launch", vec![tickets, docs]);
    root.is_project = true;
    mast_proto::WorkItemTemplateView {
        id: "t-1".into(),
        space_id: "s1".into(),
        name: "Launch kit".into(),
        revision,
        body: root,
        created_at: 1,
        updated_at: 2,
    }
}

fn template_fixture() -> WorkOutlineSnapshot {
    let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
    snapshot.templates = vec![launch_template(2)];
    let housekeeping = snapshot
        .items
        .iter_mut()
        .find(|item| item.id == HOUSEKEEPING)
        .unwrap();
    housekeeping.is_project = true;
    snapshot
}

fn template_reply(
    method: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, mast_rpc::RpcError> {
    Ok(match method {
        "LinearStatus" => serde_json::json!({"configured": true, "source": "keychain",
            "viewer": {"id": "u", "name": "Mikey"}}),
        "LinearMyIssues" | "LinearProjectIssues" => serde_json::json!([]),
        "LinearMyProjects" => {
            serde_json::to_value(vec![lf::project("p-launch", "Launch")]).unwrap()
        }
        _ => match params["op"].as_str() {
            Some("saveWorkItemTemplate") => serde_json::json!({"templateId": "t-new"}),
            Some("linkLinearProject") => serde_json::json!({"referenceId": "r-new"}),
            _ => serde_json::json!({}),
        },
    })
}

fn template_overview<'a>(
    cx: &'a mut gpui::TestAppContext,
    dir: &std::path::Path,
    snapshot: WorkOutlineSnapshot,
    reply: impl Fn(&str, &serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError>
    + Send
    + Sync
    + 'static,
) -> (
    Entity<Overview>,
    Entity<OutlineView>,
    TemplateCalls,
    &'a mut gpui::VisualTestContext,
) {
    let (overview, state, vcx) = outline_overview(cx, dir);
    vcx.update(|_, cx| {
        crate::motion::set_reduced_motion(cx, true);
        crate::composer::init(cx, crate::settings::ComposerSendBehavior::default());
        cx.bind_keys(crate::work_outline::outline_view::key_bindings());
    });
    let calls: TemplateCalls = Default::default();
    let sink = calls.clone();
    state.update(vcx, |state, _| {
        state.work_outline_actions_override =
            Some(crate::work_outline::WorkOutlineActions::scripted(
                std::sync::Arc::new(move |method, params| {
                    sink.lock()
                        .unwrap()
                        .push((method.to_string(), params.clone()));
                    reply(method, &params)
                }),
            ));
    });
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(snapshot)), cx);
        o.set_mode(OverviewMode::Outline, cx);
    });
    let outline = overview.read_with(vcx, |o, _| o.outline.clone());
    vcx.update(|window, cx| {
        let handle = gpui::Focusable::focus_handle(outline.read(cx), cx);
        window.focus(&handle, cx);
    });
    draw(vcx);
    (overview, outline, calls, vcx)
}

fn mutations(calls: &TemplateCalls) -> Vec<serde_json::Value> {
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

fn item_key(id: &str) -> String {
    row_key::item("s1", id)
}

/// Create the template flow under `parent` and type `title`.
fn new_from_template(
    outline: &Entity<OutlineView>,
    vcx: &mut gpui::VisualTestContext,
    parent: Option<&str>,
    title: &str,
) {
    outline.update_in(vcx, |outline, window, cx| {
        outline
            .begin_new_from_template(TemplateFlow::new("s1", parent.map(str::to_owned)), window, cx)
            .unwrap()
    });
    draw(vcx);
    // The picker lists the Space's one template; choose it.
    outline.update_in(vcx, |outline, window, cx| {
        outline.choose_picker_entry(0, window, cx)
    });
    draw(vcx);
    assert!(outline.read_with(vcx, |o, _| o.is_editing()), "the title row");
    vcx.simulate_input(title);
    draw(vcx);
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    draw(vcx);
}

/// The snapshot with the plan's items created (and optionally the root
/// archived), as the engine would show it after the calls in `calls`.
fn created_snapshot(
    calls: &[serde_json::Value],
    title: &str,
    archived: bool,
) -> WorkOutlineSnapshot {
    let mut snapshot = template_fixture();
    for create in calls.iter().filter(|call| call["op"] == "createWorkItem") {
        snapshot = optimistic::apply(
            &snapshot,
            &LocalChange::Create {
                work_item_id: mast_proto::derive_id(
                    create["operationId"].as_str().unwrap(),
                    mast_proto::roles::ITEM,
                ),
                parent: create["parentWorkItemId"].as_str().map(str::to_owned),
                placement: mast_proto::WorkOutlinePlacement::Last,
                title: create["title"].as_str().unwrap_or(title).to_owned(),
                created_at: 1,
            },
        )
        .unwrap();
    }
    if archived {
        let root = mast_proto::derive_id(
            calls[0]["operationId"].as_str().unwrap(),
            mast_proto::roles::ITEM,
        );
        snapshot
            .items
            .iter_mut()
            .find(|item| item.id == root)
            .unwrap()
            .archived_at = Some(crate::work_outline::store::FIXTURE_NOW + 5);
    }
    snapshot
}

fn settle_snapshot(
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
// Save as template
// --------------------------------------------------------------------------

#[gpui::test]
fn save_as_template_sends_the_mutation_and_is_not_undoable(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) =
        template_overview(cx, dir.path(), template_fixture(), template_reply);
    open_details(&overview, vcx, HOUSEKEEPING);
    click(vcx, "work-item-save-template");
    assert!(row_bounds(vcx, "work-item-template-save".into()).is_some());
    assert!(
        mutations(&calls).is_empty(),
        "opening the prompt writes nothing"
    );
    click(vcx, "work-item-template-save");

    let sent = mutations(&calls);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["op"], "saveWorkItemTemplate");
    assert_eq!(sent[0]["spaceId"], "s1");
    assert_eq!(sent[0]["projectWorkItemId"], HOUSEKEEPING);
    assert_eq!(sent[0]["name"], "Housekeeping", "prefilled with the title");
    assert!(sent[0].get("templateId").is_none(), "a new template");
    let id = sent[0]["operationId"].as_str().unwrap();
    assert!(!id.is_empty() && id.len() <= 64);
    assert_eq!(
        outline.read_with(vcx, |o, _| o.notice().map(str::to_owned)).as_deref(),
        Some("Saved template \u{2018}Housekeeping\u{2019} \u{b7} rev 1")
    );
    assert!(
        outline.read_with(vcx, |o, cx| o.next_undo_label(cx)).is_none(),
        "saving is not an undo step"
    );
}

#[gpui::test]
fn a_project_linked_to_a_template_offers_update_or_save_as_new(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mut snapshot = template_fixture();
    {
        let project = snapshot
            .items
            .iter_mut()
            .find(|item| item.id == HOUSEKEEPING)
            .unwrap();
        project.template_id = Some("t-1".into());
        project.template_revision = Some(2);
    }
    let (overview, outline, calls, vcx) =
        template_overview(cx, dir.path(), snapshot, template_reply);
    open_details(&overview, vcx, HOUSEKEEPING);
    click(vcx, "work-item-save-template");
    assert!(row_bounds(vcx, "work-item-template-update".into()).is_some());
    assert!(row_bounds(vcx, "work-item-template-new-copy".into()).is_some());
    click(vcx, "work-item-template-update");
    let sent = mutations(&calls);
    assert_eq!(sent[0]["templateId"], "t-1", "replaces the linked template");
    assert_eq!(sent[0]["name"], "Launch kit", "prefilled with its name");
    assert_eq!(
        outline.read_with(vcx, |o, _| o.notice().map(str::to_owned)).as_deref(),
        Some("Saved template \u{2018}Launch kit\u{2019} \u{b7} rev 3")
    );
}

#[gpui::test]
fn a_failing_save_shows_an_error_and_keeps_the_prompt(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, _calls, vcx) =
        template_overview(cx, dir.path(), template_fixture(), |method, params| {
            if params["op"] == "saveWorkItemTemplate" {
                return Err(mast_rpc::RpcError::Failed(
                    "work-outline/invalid: a template named 'Housekeeping' already exists".into(),
                ));
            }
            template_reply(method, params)
        });
    open_details(&overview, vcx, HOUSEKEEPING);
    click(vcx, "work-item-save-template");
    click(vcx, "work-item-template-save");
    let error = outline.read_with(vcx, |o, _| o.error_notice().map(|(t, d)| format!("{t} {d}")));
    assert!(error.unwrap().contains("already exists"));
    assert!(
        row_bounds(vcx, "work-item-template-save".into()).is_some(),
        "the prompt stays open to rename"
    );
    assert!(row_bounds(vcx, "work-item-template-error".into()).is_some());
}

#[gpui::test]
fn save_as_template_is_disabled_with_a_reason_until_the_item_is_a_project(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, _calls, vcx) =
        template_overview(cx, dir.path(), template_fixture(), template_reply);
    let entry = |id: &str, vcx: &mut gpui::VisualTestContext| {
        outline.read_with(vcx, |o, cx| {
            let menu = o.context_menu(&item_key(id), cx).expect("item menu");
            let ids: Vec<_> = menu.entries.iter().map(|e| e.id).collect();
            assert!(ids.contains(&"new-from-template"));
            menu.entries
                .into_iter()
                .find(|e| e.id == "save-as-template")
                .expect("save entry")
        })
    };
    assert_eq!(
        entry(UI, vcx).disabled.as_deref(),
        Some("Mark it as a project first")
    );
    assert_eq!(entry(HOUSEKEEPING, vcx).disabled, None);
}

// --------------------------------------------------------------------------
// The Templates list
// --------------------------------------------------------------------------

#[gpui::test]
fn the_space_root_lists_templates_and_deletes_after_confirming(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _outline, calls, vcx) =
        template_overview(cx, dir.path(), template_fixture(), template_reply);
    let details = open_details(&overview, vcx, "");
    assert!(row_bounds(vcx, "space-templates".into()).is_some());
    assert!(row_bounds(vcx, "space-template-t-1".into()).is_some());
    assert!(row_bounds(vcx, "space-template-new-t-1".into()).is_some());

    click(vcx, "space-template-delete-t-1");
    assert!(mutations(&calls).is_empty(), "Delete asks first");
    let request = details
        .update(vcx, |d, _| d.captured_confirm.take())
        .expect("a confirmation");
    assert!(request.destructive);
    assert!(request.title.contains("Launch kit"));
    vcx.update(|window, cx| (request.on_decision)(true, window, cx));
    vcx.run_until_parked();
    let sent = mutations(&calls);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["op"], "deleteWorkItemTemplate");
    assert_eq!(sent[0]["templateId"], "t-1");
}

#[gpui::test]
fn the_empty_templates_list_explains_how_to_make_one(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mut snapshot = template_fixture();
    snapshot.templates.clear();
    let (overview, outline, calls, vcx) =
        template_overview(cx, dir.path(), snapshot, template_reply);
    open_details(&overview, vcx, "");
    assert!(row_bounds(vcx, "space-templates".into()).is_some());
    assert!(row_bounds(vcx, "space-template-new-t-1".into()).is_none());
    // New from template… with none says so and sends nothing.
    let result = outline.update_in(vcx, |outline, window, cx| {
        outline.begin_new_from_template(TemplateFlow::new("s1", None), window, cx)
    });
    assert!(result.unwrap_err().contains("No templates"));
    assert!(mutations(&calls).is_empty());
}

// --------------------------------------------------------------------------
// New from template
// --------------------------------------------------------------------------

#[gpui::test]
fn new_from_template_sends_the_ordered_sequence_with_derived_ids(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, vcx) =
        template_overview(cx, dir.path(), template_fixture(), template_reply);
    new_from_template(&outline, vcx, Some(API), "Q3 launch");

    let sent = mutations(&calls);
    let ops: Vec<&str> = sent.iter().map(|m| m["op"].as_str().unwrap()).collect();
    assert_eq!(
        ops,
        [
            "createWorkItem",
            "createWorkItem",
            "createWorkItem",
            "createWorkItemInstruction",
            "updateWorkItem",
            "updateWorkItem",
            "updateWorkItem",
        ]
    );
    let item = |ix: usize| {
        mast_proto::derive_id(sent[ix]["operationId"].as_str().unwrap(), mast_proto::roles::ITEM)
    };
    let (root, tickets, docs) = (item(0), item(1), item(2));
    assert_eq!(sent[0]["title"], "Q3 launch");
    assert_eq!(sent[0]["parentWorkItemId"], API, "under the picked parent");
    assert_eq!(sent[0]["placement"]["type"], "last");
    assert_eq!(sent[1]["title"], "Tickets");
    assert_eq!(sent[1]["parentWorkItemId"], root);
    assert_eq!(sent[2]["title"], "Docs");
    assert_eq!(sent[2]["description"], "Write the docs");
    assert_eq!(sent[2]["parentWorkItemId"], root);
    assert_eq!(sent[3]["scope"]["workItemId"], docs);
    assert_eq!(sent[3]["text"], "Keep it short.");
    assert_eq!(sent[4]["workItemId"], root);
    assert_eq!(sent[4]["changes"]["isProject"], true);
    assert_eq!(sent[5]["workItemId"], root);
    assert_eq!(sent[5]["changes"]["workerDestination"], tickets);
    assert_eq!(sent[6]["workItemId"], root);
    assert_eq!(sent[6]["changes"]["template"]["templateId"], "t-1");
    assert_eq!(sent[6]["changes"]["template"]["revision"], 2);
    for m in &sent {
        let id = m["operationId"].as_str().unwrap();
        assert!(!id.is_empty() && id.len() <= 64, "{id}");
    }
    outline.read_with(vcx, |o, _| {
        assert_eq!(o.template_toast_text().map(|t| t.starts_with("Created project")), Some(true));
    });
    assert!(row_bounds(vcx, "outline-template-toast".into()).is_some());
}

#[gpui::test]
fn new_from_template_at_the_space_root_has_no_parent(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, vcx) =
        template_overview(cx, dir.path(), template_fixture(), template_reply);
    new_from_template(&outline, vcx, None, "Top project");
    let sent = mutations(&calls);
    assert_eq!(sent[0]["title"], "Top project");
    assert!(sent[0].get("parentWorkItemId").is_none_or(|p| p.is_null()));
}

#[gpui::test]
fn one_undo_archives_the_new_project_and_redo_restores_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) =
        template_overview(cx, dir.path(), template_fixture(), template_reply);
    new_from_template(&outline, vcx, Some(API), "Q3 launch");
    let sent = mutations(&calls);
    assert_eq!(sent.len(), 7);
    let root = mast_proto::derive_id(sent[0]["operationId"].as_str().unwrap(), mast_proto::roles::ITEM);
    settle_snapshot(&overview, vcx, created_snapshot(&sent, "Q3 launch", false));
    outline.read_with(vcx, |o, _| {
        assert_eq!(o.selected_key(), Some(item_key(&root).as_str()), "the new root is selected");
    });
    assert_eq!(
        outline.read_with(vcx, |o, cx| o.next_undo_label(cx)).as_deref(),
        Some("created project \u{2018}Q3 launch\u{2019} from template \u{2018}Launch kit\u{2019}")
    );

    vcx.simulate_keystrokes("cmd-z");
    vcx.run_until_parked();
    let sent_after = mutations(&calls);
    assert_eq!(sent_after.len(), 8, "a single ⌘Z is one request");
    assert_eq!(sent_after[7]["op"], "archiveWorkItem");
    assert_eq!(sent_after[7]["workItemId"], root);
    assert_eq!(sent_after[7]["descendantArchivePolicy"], "archiveDescendants");
    assert!(
        outline.read_with(vcx, |o, cx| o.next_undo_label(cx)).is_none(),
        "the whole instantiation was one step"
    );

    settle_snapshot(&overview, vcx, created_snapshot(&sent, "Q3 launch", true));
    vcx.simulate_keystrokes("cmd-shift-z");
    vcx.run_until_parked();
    let sent_redo = mutations(&calls);
    assert_eq!(sent_redo.len(), 9);
    assert_eq!(sent_redo[8]["op"], "restoreWorkItem");
    assert_eq!(sent_redo[8]["workItemId"], root);
}

#[gpui::test]
fn the_toast_undo_button_undoes_the_creation(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, outline, calls, vcx) =
        template_overview(cx, dir.path(), template_fixture(), template_reply);
    new_from_template(&outline, vcx, Some(API), "Q3 launch");
    let sent = mutations(&calls);
    settle_snapshot(&overview, vcx, created_snapshot(&sent, "Q3 launch", false));
    click(vcx, "outline-template-toast-undo");
    let after = mutations(&calls);
    assert_eq!(after.len(), 8);
    assert_eq!(after[7]["op"], "archiveWorkItem");
}

#[gpui::test]
fn a_failing_step_shows_an_error_and_a_first_step_failure_is_not_undoable(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, vcx) =
        template_overview(cx, dir.path(), template_fixture(), |method, params| {
            if params["op"] == "createWorkItem" {
                return Err(mast_rpc::RpcError::Failed(
                    "work-outline/limitExceeded: too many items".into(),
                ));
            }
            template_reply(method, params)
        });
    new_from_template(&outline, vcx, Some(API), "Q3 launch");
    assert_eq!(mutations(&calls).len(), 1, "stops at the first failure");
    let error = outline.read_with(vcx, |o, _| o.error_notice().map(|(t, d)| format!("{t} {d}")));
    assert!(error.unwrap().contains("too many items"));
    assert!(outline.read_with(vcx, |o, cx| o.next_undo_label(cx)).is_none());
    assert!(outline.read_with(vcx, |o, _| o.template_toast_text().is_none()));
}

#[gpui::test]
fn an_empty_title_or_archived_parent_is_refused_without_sending(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, outline, calls, vcx) =
        template_overview(cx, dir.path(), template_fixture(), template_reply);
    let mut flow = TemplateFlow::new("s1", Some(API.into()));
    flow.template_id = Some("t-1".into());
    let empty = outline.update(vcx, |o, cx| o.instantiate_template(flow.clone(), "  ".into(), cx));
    assert!(empty.is_err());
    flow.parent = Some(RETIRED.into());
    let archived = outline.update(vcx, |o, cx| o.instantiate_template(flow, "x".into(), cx));
    assert!(archived.is_err());
    vcx.run_until_parked();
    assert!(mutations(&calls).is_empty());
    assert!(outline.read_with(vcx, |o, cx| o.next_undo_label(cx)).is_none());
}

// --------------------------------------------------------------------------
// Linear entry
// --------------------------------------------------------------------------

#[gpui::test]
fn a_linear_project_row_creates_a_project_item_from_a_template_and_links_it(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let mut snapshot = template_fixture();
    snapshot.space_settings.linear_enabled = true;
    let (overview, outline, calls, vcx) =
        template_overview(cx, dir.path(), snapshot, template_reply);
    vcx.update(|window, cx| {
        overview.update(cx, |o, cx| {
            o.run_work_outline_command(WorkOutlineCommand::OpenLinear, window, cx)
        })
    });
    vcx.run_until_parked();
    draw(vcx);
    click(vcx, "linear-project-template-p-launch");

    // Template picker, then the parent picker (entries: Top level, Launch, API, …).
    outline.update_in(vcx, |outline, window, cx| {
        outline.choose_picker_entry(0, window, cx)
    });
    draw(vcx);
    outline.update_in(vcx, |outline, window, cx| {
        outline.choose_picker_entry(2, window, cx)
    });
    vcx.run_until_parked();
    draw(vcx);

    let sent = mutations(&calls);
    let ops: Vec<&str> = sent.iter().map(|m| m["op"].as_str().unwrap()).collect();
    assert_eq!(ops.len(), 8, "the plan, then the link: {ops:?}");
    assert_eq!(ops[7], "linkLinearProject");
    assert_eq!(sent[0]["title"], "Launch", "the Linear project's name");
    assert_eq!(sent[0]["parentWorkItemId"], API);
    let root = mast_proto::derive_id(sent[0]["operationId"].as_str().unwrap(), mast_proto::roles::ITEM);
    assert_eq!(sent[7]["workItemId"], root);
    assert_eq!(sent[7]["project"]["id"], "p-launch");
    assert_eq!(sent[7]["project"]["name"], "Launch");
    assert!(sent[7]["operationId"].as_str().unwrap().len() <= 64);
    let toast = outline
        .read_with(vcx, |o, _| o.template_toast_text().map(str::to_owned))
        .expect("a toast");
    assert!(toast.contains("linked to Linear project Launch"), "{toast}");
    // One undo step for the whole thing.
    assert_eq!(
        outline
            .read_with(vcx, |o, cx| o.recent_changes(cx))
            .len(),
        1
    );
}
