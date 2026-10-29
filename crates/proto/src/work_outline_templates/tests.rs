use serde_json::json;

use super::*;
use crate::work_outline::WorkOutlineSnapshot;

fn node(title: &str, instructions: &[&str], children: Vec<TemplateNode>) -> TemplateNode {
    TemplateNode {
        title: title.into(),
        instructions: instructions.iter().map(|s| (*s).to_owned()).collect(),
        children,
        ..Default::default()
    }
}

/// Project (project) / Planning (instruction) / Tickets (destination).
fn template() -> WorkItemTemplateView {
    let mut root = node(
        "Project",
        &["Be careful"],
        vec![
            node("Planning", &["Write a plan"], vec![]),
            node("Tickets", &[], vec![]),
        ],
    );
    root.is_project = true;
    root.description = Some("Root".into());
    root.children[1].is_worker_destination = true;
    WorkItemTemplateView {
        id: "t1".into(),
        space_id: "s".into(),
        name: "Standard".into(),
        revision: 3,
        body: root,
        created_at: 1,
        updated_at: 2,
    }
}

fn view(id: &str, title: &str, children: &[&str]) -> serde_json::Value {
    json!({"id": id, "spaceId": "s", "title": title, "childIds": children})
}

fn snapshot(
    items: Vec<serde_json::Value>,
    instructions: Vec<serde_json::Value>,
) -> WorkOutlineSnapshot {
    serde_json::from_value(json!({
        "spaceId": "s", "available": true, "synced": true,
        "items": items, "instructions": instructions,
    }))
    .unwrap()
}

fn instruction(id: &str, item: &str, key: &str, text: &str) -> serde_json::Value {
    json!({"id": id, "spaceId": "s", "workItemId": item, "orderKey": key, "text": text})
}

/// A live project that matches [`template`], plus a ticket and a chat-less
/// linked item under the destination and an archived/linked sibling.
fn live_project() -> WorkOutlineSnapshot {
    let mut project = view("p", "My project", &["plan", "tix", "archived", "linked"]);
    project["isProject"] = json!(true);
    project["workerDestinationId"] = json!("tix");
    let mut archived = view("archived", "Old", &[]);
    archived["archivedAt"] = json!(5);
    let mut linked = view("linked", "Linked", &[]);
    linked["externalReferenceIds"] = json!(["r"]);
    snapshot(
        vec![
            project,
            view("plan", "Planning", &[]),
            view("tix", "Tickets", &["ticket"]),
            view("ticket", "Ticket 1", &[]),
            archived,
            linked,
        ],
        vec![
            instruction("i2", "p", "b", "second"),
            instruction("i1", "p", "a", "Be careful"),
            instruction("i3", "plan", "a", "Write a plan"),
            instruction("i4", "ticket", "a", "ticket only"),
        ],
    )
}

#[test]
fn wire_shapes_round_trip_and_are_lenient() {
    let t = template();
    let value = serde_json::to_value(&t).unwrap();
    assert_eq!(value["body"]["isProject"], json!(true));
    assert_eq!(
        value["body"]["children"][1]["isWorkerDestination"],
        json!(true)
    );
    assert!(value["body"]["children"][0].get("isProject").is_none());
    assert_eq!(
        serde_json::from_value::<WorkItemTemplateView>(value).unwrap(),
        t
    );
    let lenient: TemplateNode = serde_json::from_value(json!({"title": "x"})).unwrap();
    assert_eq!(lenient, node("x", &[], vec![]));
    let view: WorkItemTemplateView = serde_json::from_value(json!({"id": "t"})).unwrap();
    assert_eq!(view.revision, 0);

    let save = crate::WorkOutlineMutation::SaveWorkItemTemplate(SaveWorkItemTemplateParams {
        space_id: "s".into(),
        project_work_item_id: "p".into(),
        name: "N".into(),
        template_id: None,
        operation_id: "op".into(),
    });
    let value = serde_json::to_value(&save).unwrap();
    assert_eq!(
        value,
        json!({"op":"saveWorkItemTemplate","spaceId":"s","projectWorkItemId":"p","name":"N","templateId":null,"operationId":"op"})
    );
    assert_eq!(
        serde_json::from_value::<crate::WorkOutlineMutation>(value).unwrap(),
        save
    );
    assert_eq!(save.op_name(), "saveWorkItemTemplate");
    let delete = crate::WorkOutlineMutation::DeleteWorkItemTemplate(DeleteWorkItemTemplateParams {
        template_id: "t".into(),
        operation_id: "op2".into(),
    });
    assert_eq!(
        serde_json::to_value(&delete).unwrap(),
        json!({"op":"deleteWorkItemTemplate","templateId":"t","operationId":"op2"})
    );
    assert_eq!(delete.operation_id(), "op2");

    let changes: crate::WorkItemChanges =
        serde_json::from_value(json!({"template": {"templateId": "t", "revision": 2}})).unwrap();
    assert_eq!(
        changes.template,
        crate::FieldPatch::Set(WorkItemTemplateLink {
            template_id: "t".into(),
            revision: 2
        })
    );
    let cleared: crate::WorkItemChanges =
        serde_json::from_value(json!({"template": null})).unwrap();
    assert_eq!(cleared.template, crate::FieldPatch::Clear);
}

#[test]
fn snapshot_templates_are_skipped_when_empty_and_compared() {
    let mut a = WorkOutlineSnapshot::unavailable("s", true, 1);
    assert!(serde_json::to_value(&a).unwrap().get("templates").is_none());
    let mut b = a.clone();
    b.templates.push(template());
    assert!(!a.same_content_as(&b));
    a.templates.push(template());
    assert!(a.same_content_as(&b));
}

#[test]
fn limits_are_checked() {
    assert!(template().body.check_limits().is_ok());
    let wide = node(
        "r",
        &[],
        (0..64)
            .map(|i| node(&format!("c{i}"), &[], vec![]))
            .collect(),
    );
    assert!(wide.check_limits().unwrap_err().contains("64"));
    let mut deep = node("leaf", &[], vec![]);
    for i in 0..5 {
        deep = node(&format!("l{i}"), &[], vec![deep]);
    }
    assert_eq!(deep.depth(), 6);
    assert!(deep.check_limits().is_ok());
    assert!(
        node("top", &[], vec![deep])
            .check_limits()
            .unwrap_err()
            .contains("levels")
    );
    let many = node("n", &["x"; 17], vec![]);
    assert!(many.check_limits().unwrap_err().contains("instructions"));
}

#[test]
fn freeze_drops_tickets_archived_and_linked_items() {
    let body = freeze_project_subtree(&live_project(), "p").unwrap();
    assert_eq!(body.title, "My project");
    assert!(body.is_project);
    assert_eq!(body.instructions, vec!["Be careful", "second"]);
    let titles: Vec<&str> = body.children.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(titles, ["Planning", "Tickets"]);
    assert_eq!(body.children[0].instructions, vec!["Write a plan"]);
    assert!(body.children[1].is_worker_destination);
    assert!(body.children[1].children.is_empty());
    assert!(freeze_project_subtree(&live_project(), "nope").is_none());
}

#[test]
fn instantiation_plan_is_ordered_and_flat() {
    let plan = plan_template_instantiation(&template(), "  New one ");
    use InstantiationStep::*;
    assert_eq!(
        plan,
        vec![
            CreateItem {
                path_id: "n0".into(),
                parent_path_id: None,
                title: "New one".into(),
                description: Some("Root".into())
            },
            AddInstruction {
                path_id: "n0".into(),
                text: "Be careful".into()
            },
            CreateItem {
                path_id: "n1".into(),
                parent_path_id: Some("n0".into()),
                title: "Planning".into(),
                description: None
            },
            AddInstruction {
                path_id: "n1".into(),
                text: "Write a plan".into()
            },
            CreateItem {
                path_id: "n2".into(),
                parent_path_id: Some("n0".into()),
                title: "Tickets".into(),
                description: None
            },
            MarkProject {
                path_id: "n0".into()
            },
            SetWorkerDestination {
                project_path_id: "n0".into(),
                destination_path_id: "n2".into()
            },
            LinkTemplate {
                path_id: "n0".into(),
                template_id: "t1".into(),
                revision: 3
            },
        ]
    );
}

#[test]
fn instantiation_of_a_bare_template_still_links() {
    let t = WorkItemTemplateView {
        id: "t".into(),
        revision: 1,
        body: node("Only", &[], vec![]),
        ..Default::default()
    };
    let plan = plan_template_instantiation(&t, "X");
    assert_eq!(plan.len(), 2);
    assert!(matches!(plan[1], InstantiationStep::LinkTemplate { .. }));
}

#[test]
fn diff_is_empty_when_in_sync() {
    // The project has extra tickets, an archived item and a linked item: all
    // dropped by the freeze, so the project is "in sync".
    let mut t = template();
    t.body.instructions.push("second".into());
    assert_eq!(
        diff_template_against_project(&live_project(), &t, "p"),
        vec![]
    );
    assert!(diff_template_against_project(&live_project(), &t, "missing").is_empty());
}

#[test]
fn diff_reports_each_kind() {
    let mut t = template();
    t.body.children.push(node("Review", &["Check"], vec![]));
    t.body.children[0].instructions = vec!["Write a better plan".into()];
    t.body.children[1].is_worker_destination = false;
    t.body.children[0].is_worker_destination = true;
    t.body.instructions.clear();
    t.body.instructions.push("Be careful".into());
    let mut snap = live_project();
    // Project-only item.
    snap.items
        .iter_mut()
        .find(|i| i.id == "p")
        .unwrap()
        .child_ids
        .push("extra".into());
    snap.items
        .push(serde_json::from_value(view("extra", "Extra", &[])).unwrap());
    let changes = diff_template_against_project(&snap, &t, "p");
    assert!(changes.contains(&TemplateChange::AddedItem {
        path: vec!["Review".into()],
        title: "Review".into(),
        node: node("Review", &["Check"], vec![]),
    }));
    assert!(changes.contains(&TemplateChange::RemovedItem {
        path: vec!["Extra".into()]
    }));
    assert!(changes.contains(&TemplateChange::InstructionAdded {
        path: vec!["Planning".into()],
        text: "Write a better plan".into()
    }));
    assert!(changes.contains(&TemplateChange::InstructionRemoved {
        path: vec!["Planning".into()],
        text: "Write a plan".into()
    }));
    assert!(changes.contains(&TemplateChange::InstructionRemoved {
        path: vec![],
        text: "second".into()
    }));
    assert!(changes.contains(&TemplateChange::WorkerDestinationChanged {
        from: Some(vec!["Tickets".into()]),
        to: Some(vec!["Planning".into()]),
    }));
    assert_eq!(changes.len(), 6);
}

#[test]
fn template_changed_compares_revisions() {
    let t = template();
    let project = |id: Option<&str>, rev: Option<u32>| -> crate::WorkItemView {
        serde_json::from_value(
            json!({"id": "p", "spaceId": "s", "templateId": id, "templateRevision": rev}),
        )
        .unwrap()
    };
    assert!(template_changed(&project(Some("t1"), Some(2)), &t));
    assert!(template_changed(&project(Some("t1"), None), &t));
    assert!(!template_changed(&project(Some("t1"), Some(3)), &t));
    assert!(!template_changed(&project(Some("t1"), Some(4)), &t));
    assert!(!template_changed(&project(Some("other"), Some(1)), &t));
    assert!(!template_changed(&project(None, None), &t));
}
