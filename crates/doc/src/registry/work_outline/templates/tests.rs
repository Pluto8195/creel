use mast_proto::CreateWorkItemInstructionParams;
use mast_proto::work_outline::{
    ArchiveWorkItemParams, CreateWorkItemExternalReferenceParams, CreateWorkItemParams,
    DescendantArchivePolicy, FieldPatch, UpdateWorkItemParams, WorkItemChanges, WorkOutlineActor,
    WorkOutlineActorKind, WorkOutlineErrorCode, WorkOutlineMutateReply, WorkOutlineMutation,
    WorkOutlinePlacement, WorkOutlineSnapshot,
};
use mast_proto::work_outline_instructions::InstructionScope;
use mast_proto::work_outline_templates::{
    DeleteWorkItemTemplateParams, SaveWorkItemTemplateParams, WorkItemTemplateLink,
};

use super::*;
use crate::registry::tests::{chat, space};

const SPACE: &str = "sp-1";
const OTHER: &str = "sp-2";
const NOW: i64 = 1_700_000_000_000;

fn actor(kind: WorkOutlineActorKind) -> WorkOutlineActor {
    WorkOutlineActor {
        kind,
        device_id: "dev-a".into(),
        chat_id: (kind == WorkOutlineActorKind::Agent).then(|| "c1".into()),
        message_id: None,
    }
}

fn doc() -> RegistryDoc {
    let mut doc = RegistryDoc::new("dev-a");
    doc.upsert_space(&space(SPACE, "dev-a", "/tmp/one"))
        .unwrap();
    doc.upsert_space(&space(OTHER, "dev-a", "/tmp/two"))
        .unwrap();
    let mut c = chat("c1", "dev-a");
    c.space_id = Some(SPACE.into());
    c.parent_chat_id = None;
    doc.upsert_chat(&c).unwrap();
    doc
}

fn apply_as(
    doc: &mut RegistryDoc,
    kind: WorkOutlineActorKind,
    mutation: WorkOutlineMutation,
) -> Result<WorkOutlineMutateReply, mast_proto::WorkOutlineError> {
    doc.apply_work_outline_mutation(&mutation, &actor(kind), NOW)
}

fn apply(doc: &mut RegistryDoc, mutation: WorkOutlineMutation) -> Outcome<WorkOutlineMutateReply> {
    apply_as(doc, WorkOutlineActorKind::User, mutation)
}

fn item(
    doc: &mut RegistryDoc,
    space_id: &str,
    op: &str,
    parent: Option<&str>,
    title: &str,
) -> String {
    apply(
        doc,
        WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: space_id.into(),
            parent_work_item_id: parent.map(str::to_owned),
            placement: WorkOutlinePlacement::Last,
            title: title.into(),
            description: Some(format!("About {title}")),
            status: None,
            operation_id: op.into(),
        }),
    )
    .unwrap()
    .work_item_id
    .unwrap()
}

fn instruct(doc: &mut RegistryDoc, op: &str, item: &str, text: &str) {
    apply(
        doc,
        WorkOutlineMutation::CreateWorkItemInstruction(CreateWorkItemInstructionParams {
            space_id: SPACE.into(),
            scope: InstructionScope::WorkItem {
                work_item_id: item.into(),
            },
            text: text.into(),
            placement: None,
            source: None,
            operation_id: op.into(),
        }),
    )
    .unwrap();
}

fn update(
    doc: &mut RegistryDoc,
    kind: WorkOutlineActorKind,
    id: &str,
    changes: WorkItemChanges,
) -> Outcome<WorkOutlineMutateReply> {
    apply_as(
        doc,
        kind,
        WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: id.into(),
            changes,
            operation_id: format!("up-{id}-{}", doc.pending.len()),
        }),
    )
}

struct Project {
    doc: RegistryDoc,
    project: String,
    planning: String,
    tickets: String,
    ticket: String,
}

/// Project (isProject, destination Tickets) / Planning (+instruction) /
/// Tickets / Ticket 1 (+instruction), with an archived and a linked sibling.
fn project() -> Project {
    let mut doc = doc();
    let p = item(&mut doc, SPACE, "p", None, "Project");
    let planning = item(&mut doc, SPACE, "pl", Some(&p), "Planning");
    let tickets = item(&mut doc, SPACE, "tk", Some(&p), "Tickets");
    let ticket = item(&mut doc, SPACE, "t1", Some(&tickets), "Ticket 1");
    let archived = item(&mut doc, SPACE, "ar", Some(&p), "Old");
    let linked = item(&mut doc, SPACE, "li", Some(&p), "Linked");
    instruct(&mut doc, "i1", &p, "Be careful");
    instruct(&mut doc, "i2", &planning, "Write a plan");
    instruct(&mut doc, "i3", &ticket, "ticket only");
    apply(
        &mut doc,
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: archived,
            descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
            operation_id: "arch".into(),
        }),
    )
    .unwrap();
    apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItemExternalReference(
            CreateWorkItemExternalReferenceParams {
                work_item_id: linked,
                provider: "github".into(),
                reference_type: "pull_request".into(),
                external_id: "o/r#1".into(),
                url: "https://example.com/1".into(),
                cached_title: None,
                operation_id: "ref".into(),
            },
        ),
    )
    .unwrap();
    update(
        &mut doc,
        WorkOutlineActorKind::User,
        &p,
        WorkItemChanges {
            is_project: Some(true),
            worker_destination: FieldPatch::Set(tickets.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    Project {
        doc,
        project: p,
        planning,
        tickets,
        ticket,
    }
}

fn save(
    doc: &mut RegistryDoc,
    op: &str,
    project: &str,
    name: &str,
    id: Option<&str>,
) -> Outcome<String> {
    apply(
        doc,
        WorkOutlineMutation::SaveWorkItemTemplate(SaveWorkItemTemplateParams {
            space_id: SPACE.into(),
            project_work_item_id: project.into(),
            name: name.into(),
            template_id: id.map(str::to_owned),
            operation_id: op.into(),
        }),
    )
    .map(|reply| reply.template_id.unwrap())
}

fn snap(doc: &RegistryDoc) -> WorkOutlineSnapshot {
    doc.work_outline_snapshot(SPACE, true)
}

fn code<T: std::fmt::Debug>(result: Outcome<T>) -> WorkOutlineErrorCode {
    result.unwrap_err().code
}

#[test]
fn save_freezes_the_subtree_and_links_the_project() {
    let mut f = project();
    let id = save(&mut f.doc, "s1", &f.project, "  Standard  ", None).unwrap();
    assert_eq!(id, derive_id("s1", roles::TEMPLATE));
    let snapshot = snap(&f.doc);
    assert_eq!(snapshot.templates.len(), 1);
    let template = &snapshot.templates[0];
    assert_eq!((template.name.as_str(), template.revision), ("Standard", 1));
    assert_eq!(template.space_id, SPACE);
    let body = &template.body;
    assert_eq!(body.title, "Project");
    assert_eq!(body.description.as_deref(), Some("About Project"));
    assert!(body.is_project);
    assert_eq!(body.instructions, vec!["Be careful"]);
    let titles: Vec<&str> = body.children.iter().map(|c| c.title.as_str()).collect();
    // Archived items and the tickets are gone; an externally linked item
    // stays as a plain node (its links are never part of a template).
    let mut sorted = titles.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, ["Linked", "Planning", "Tickets"]);
    assert!(titles.iter().position(|t| *t == "Planning") < titles.iter().position(|t| *t == "Tickets"));
    assert_eq!(body.children[0].instructions, vec!["Write a plan"]);
    assert!(body.children[1].is_worker_destination);
    assert!(body.children[1].children.is_empty());
    let project = snapshot.item(&f.project).unwrap();
    assert_eq!(project.template_id.as_deref(), Some(id.as_str()));
    assert_eq!(project.template_revision, Some(1));
    let _ = (&f.planning, &f.tickets, &f.ticket);
}

#[test]
fn resave_bumps_the_revision_and_replays_do_not() {
    let mut f = project();
    let id = save(&mut f.doc, "s1", &f.project, "Standard", None).unwrap();
    // Change the live project, then re-save under a new name.
    let extra = item(&mut f.doc, SPACE, "x", Some(&f.project), "Review");
    let again = save(&mut f.doc, "s2", &f.project, "Standard v2", Some(&id)).unwrap();
    assert_eq!(again, id);
    let snapshot = snap(&f.doc);
    let template = &snapshot.templates[0];
    assert_eq!(
        (template.name.as_str(), template.revision),
        ("Standard v2", 2)
    );
    assert!(template.body.children.iter().any(|c| c.title == "Review"));
    assert_eq!(
        snapshot.item(&f.project).unwrap().template_revision,
        Some(2)
    );
    // Replaying the same operation does not bump again.
    save(&mut f.doc, "s2", &f.project, "Standard v2", Some(&id)).unwrap();
    assert_eq!(snap(&f.doc).templates[0].revision, 2);
    // Re-saving from another project of the Space replaces the body too.
    let other = item(&mut f.doc, SPACE, "p2", None, "Other");
    update(
        &mut f.doc,
        WorkOutlineActorKind::User,
        &other,
        WorkItemChanges {
            is_project: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    save(&mut f.doc, "s3", &other, "Standard v2", Some(&id)).unwrap();
    let snapshot = snap(&f.doc);
    assert_eq!(snapshot.templates[0].revision, 3);
    assert_eq!(snapshot.templates[0].body.title, "Other");
    let _ = extra;
}

#[test]
fn save_validates_project_space_and_names() {
    let mut f = project();
    // Any item can be saved (not only a project); it is a plain node then.
    assert!(save(&mut f.doc, "n1", &f.planning, "Planning only", None).is_ok());
    assert!(!snap(&f.doc).templates[0].body.is_project);
    // Missing item.
    assert_eq!(
        code(save(&mut f.doc, "n2", "nope", "X", None)),
        WorkOutlineErrorCode::NotFound
    );
    // Another Space's project.
    let foreign = item(&mut f.doc, OTHER, "fp", None, "Foreign");
    assert_eq!(
        code(save(&mut f.doc, "n3", &foreign, "X", None)),
        WorkOutlineErrorCode::CrossSpace
    );
    // Names: empty, too long, duplicate (case-insensitive, trimmed).
    assert_eq!(
        code(save(&mut f.doc, "n4", &f.project, "   ", None)),
        WorkOutlineErrorCode::Invalid
    );
    let long = "x".repeat(81);
    assert_eq!(
        code(save(&mut f.doc, "n5", &f.project, &long, None)),
        WorkOutlineErrorCode::LimitExceeded
    );
    assert!(save(&mut f.doc, "n6", &f.project, &"x".repeat(80), None).is_ok());
    let id = save(&mut f.doc, "n7", &f.project, "Standard", None).unwrap();
    assert_eq!(
        code(save(&mut f.doc, "n8", &f.project, " standard ", None)),
        WorkOutlineErrorCode::Invalid
    );
    // Re-saving under its own name is fine; taking another's name is not.
    assert!(save(&mut f.doc, "n9", &f.project, "STANDARD", Some(&id)).is_ok());
    let first = snap(&f.doc)
        .templates
        .iter()
        .find(|t| t.name.len() == 80)
        .unwrap()
        .id
        .clone();
    assert_eq!(
        code(save(
            &mut f.doc,
            "n10",
            &f.project,
            "standard",
            Some(&first)
        )),
        WorkOutlineErrorCode::Invalid
    );
    // Unknown template id on replace.
    assert_eq!(
        code(save(&mut f.doc, "n11", &f.project, "Z", Some("missing"))),
        WorkOutlineErrorCode::NotFound
    );
    // The same name is fine in another Space.
    let mut other_doc_item = |doc: &mut RegistryDoc| {
        let p = item(doc, OTHER, "op2", None, "P");
        update(
            doc,
            WorkOutlineActorKind::User,
            &p,
            WorkItemChanges {
                is_project: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        p
    };
    let p = other_doc_item(&mut f.doc);
    let reply = apply(
        &mut f.doc,
        WorkOutlineMutation::SaveWorkItemTemplate(SaveWorkItemTemplateParams {
            space_id: OTHER.into(),
            project_work_item_id: p,
            name: "Standard".into(),
            template_id: None,
            operation_id: "n12".into(),
        }),
    );
    assert!(reply.is_ok());
    assert_eq!(f.doc.work_outline_snapshot(OTHER, true).templates.len(), 1);
}

#[test]
fn save_enforces_template_limits() {
    let mut f = project();
    // More than 64 nodes.
    for i in 0..64 {
        item(
            &mut f.doc,
            SPACE,
            &format!("w{i}"),
            Some(&f.project),
            &format!("W{i}"),
        );
    }
    assert_eq!(
        code(save(&mut f.doc, "l1", &f.project, "Big", None)),
        WorkOutlineErrorCode::LimitExceeded
    );
    assert!(snap(&f.doc).templates.is_empty());

    // Too deep: seven levels.
    let mut f = project();
    let mut parent = f.planning.clone();
    for i in 0..5 {
        parent = item(
            &mut f.doc,
            SPACE,
            &format!("d{i}"),
            Some(&parent),
            &format!("D{i}"),
        );
    }
    assert_eq!(
        code(save(&mut f.doc, "l2", &f.project, "Deep", None)),
        WorkOutlineErrorCode::LimitExceeded
    );

    // Too many instructions on one node.
    let mut f = project();
    for i in 0..16 {
        instruct(
            &mut f.doc,
            &format!("m{i}"),
            &f.project,
            &format!("rule {i}"),
        );
    }
    assert_eq!(
        code(save(&mut f.doc, "l3", &f.project, "Wordy", None)),
        WorkOutlineErrorCode::LimitExceeded
    );

    // A body over the op-size budget (long descriptions).
    let mut f = project();
    for i in 0..20 {
        let child = item(
            &mut f.doc,
            SPACE,
            &format!("b{i}"),
            Some(&f.planning),
            &format!("B{i}"),
        );
        update(
            &mut f.doc,
            WorkOutlineActorKind::User,
            &child,
            WorkItemChanges {
                description: FieldPatch::Set("y".repeat(800)),
                ..Default::default()
            },
        )
        .unwrap();
    }
    assert_eq!(
        code(save(&mut f.doc, "l4", &f.project, "Heavy", None)),
        WorkOutlineErrorCode::LimitExceeded
    );
}

#[test]
fn delete_tombstones_the_row_and_keeps_project_links() {
    let mut f = project();
    let id = save(&mut f.doc, "s1", &f.project, "Standard", None).unwrap();
    let delete = |doc: &mut RegistryDoc, op: &str| {
        apply(
            doc,
            WorkOutlineMutation::DeleteWorkItemTemplate(DeleteWorkItemTemplateParams {
                template_id: id.clone(),
                operation_id: op.into(),
            }),
        )
    };
    delete(&mut f.doc, "d1").unwrap();
    delete(&mut f.doc, "d2").unwrap();
    assert_eq!(
        f.doc.row_state(KIND_WORK_ITEM_TEMPLATES, &id),
        RowState::Tombstoned
    );
    let snapshot = snap(&f.doc);
    assert!(snapshot.templates.is_empty());
    assert_eq!(
        snapshot.item(&f.project).unwrap().template_id.as_deref(),
        Some(id.as_str())
    );
    // The name is free again; a deleted id is never revived.
    assert!(save(&mut f.doc, "s2", &f.project, "Standard", None).is_ok());
    assert_eq!(
        code(save(&mut f.doc, "s3", &f.project, "Again", Some(&id))),
        WorkOutlineErrorCode::NotFound
    );
}

#[test]
fn template_link_validation_and_clear() {
    let mut f = project();
    let id = save(&mut f.doc, "s1", &f.project, "Standard", None).unwrap();
    let link = |rev: u32| WorkItemChanges {
        template: FieldPatch::Set(WorkItemTemplateLink {
            template_id: id.clone(),
            revision: rev,
        }),
        ..Default::default()
    };
    // Other item in the Space, user and system allowed.
    let other = item(&mut f.doc, SPACE, "p2", None, "Other");
    update(&mut f.doc, WorkOutlineActorKind::User, &other, link(1)).unwrap();
    update(&mut f.doc, WorkOutlineActorKind::System, &other, link(1)).unwrap();
    let snapshot = snap(&f.doc);
    assert_eq!(
        snapshot.item(&other).unwrap().template_id.as_deref(),
        Some(id.as_str())
    );
    assert_eq!(snapshot.item(&other).unwrap().template_revision, Some(1));
    // A newer revision than the template's, a missing template, a foreign Space.
    assert_eq!(
        code(update(
            &mut f.doc,
            WorkOutlineActorKind::User,
            &other,
            link(9)
        )),
        WorkOutlineErrorCode::Invalid
    );
    let missing = WorkItemChanges {
        template: FieldPatch::Set(WorkItemTemplateLink {
            template_id: "nope".into(),
            revision: 1,
        }),
        ..Default::default()
    };
    assert_eq!(
        code(update(
            &mut f.doc,
            WorkOutlineActorKind::User,
            &other,
            missing
        )),
        WorkOutlineErrorCode::NotFound
    );
    let foreign = item(&mut f.doc, OTHER, "fx", None, "Foreign");
    assert_eq!(
        code(update(
            &mut f.doc,
            WorkOutlineActorKind::User,
            &foreign,
            link(1)
        )),
        WorkOutlineErrorCode::CrossSpace
    );
    // Agents may not touch the link, alone or with a status.
    let agent_link = WorkItemChanges {
        status: Some(mast_proto::work_outline::WorkItemStatus::Active),
        ..link(1)
    };
    assert_eq!(
        code(update(
            &mut f.doc,
            WorkOutlineActorKind::Agent,
            &other,
            agent_link
        )),
        WorkOutlineErrorCode::Unauthorized
    );
    // A deleted template cannot be linked.
    apply(
        &mut f.doc,
        WorkOutlineMutation::DeleteWorkItemTemplate(DeleteWorkItemTemplateParams {
            template_id: id.clone(),
            operation_id: "del".into(),
        }),
    )
    .unwrap();
    assert_eq!(
        code(update(
            &mut f.doc,
            WorkOutlineActorKind::User,
            &other,
            link(1)
        )),
        WorkOutlineErrorCode::NotFound
    );
    // Clearing still works.
    let clear = WorkItemChanges {
        template: FieldPatch::Clear,
        ..Default::default()
    };
    update(&mut f.doc, WorkOutlineActorKind::User, &other, clear).unwrap();
    let snapshot = snap(&f.doc);
    assert_eq!(snapshot.item(&other).unwrap().template_id, None);
    assert_eq!(snapshot.item(&other).unwrap().template_revision, None);
}

#[test]
fn agents_may_never_save_or_delete_templates() {
    let mut f = project();
    let id = save(&mut f.doc, "s1", &f.project, "Standard", None).unwrap();
    let assigned = apply(
        &mut f.doc,
        WorkOutlineMutation::AssignChatToWorkItem(
            mast_proto::work_outline::AssignChatToWorkItemParams {
                chat_id: "c1".into(),
                work_item_id: f.project.clone(),
                operation_id: "as".into(),
            },
        ),
    );
    assigned.unwrap();
    let save_op = WorkOutlineMutation::SaveWorkItemTemplate(SaveWorkItemTemplateParams {
        space_id: SPACE.into(),
        project_work_item_id: f.project.clone(),
        name: "Agent".into(),
        template_id: None,
        operation_id: "ag1".into(),
    });
    let delete_op = WorkOutlineMutation::DeleteWorkItemTemplate(DeleteWorkItemTemplateParams {
        template_id: id.clone(),
        operation_id: "ag2".into(),
    });
    for mutation in [save_op, delete_op] {
        let name = mutation.op_name();
        let error = apply_as(&mut f.doc, WorkOutlineActorKind::Agent, mutation).unwrap_err();
        assert_eq!(error.code, WorkOutlineErrorCode::Unauthorized, "{name}");
    }
    assert_eq!(snap(&f.doc).templates.len(), 1);
}

#[test]
fn templates_follow_the_space_cascade() {
    let mut f = project();
    let id = save(&mut f.doc, "s1", &f.project, "Standard", None).unwrap();
    assert!(
        f.doc
            .work_outline_rows_of_space(SPACE)
            .contains(&(KIND_WORK_ITEM_TEMPLATES, id))
    );
}
