//! Linear rows and operations: per-Space setting, createWorkItemFromLinear
//! (one batch, idempotent, title format), project links, modified-field
//! marking and clearing, agent refusal, and Space deletion.

use super::*;
use crate::registry::tests::space;
use mast_proto::linear::LinearIssueLink;
use mast_proto::work_outline::{
    CreateWorkItemParams, FieldPatch, RemoveWorkItemExternalReferenceParams, UpdateWorkItemParams,
    WorkItemChanges, WorkOutlineActorKind, WorkOutlineErrorCode, WorkOutlineMutateReply,
    WorkOutlineMutation, WorkOutlinePlacement, WorkOutlineSnapshot,
};
use mast_proto::{LinearProjectRef, WorkOutlineError};

const SPACE: &str = "sp-1";
const OFF_SPACE: &str = "sp-2";
const NOW: i64 = 1_700_000_000_000;

fn actor(kind: WorkOutlineActorKind) -> WorkOutlineActor {
    WorkOutlineActor {
        kind,
        device_id: "dev-a".into(),
        chat_id: None,
        message_id: None,
    }
}

fn doc() -> RegistryDoc {
    let mut doc = RegistryDoc::new("dev-a");
    doc.upsert_space(&space(SPACE, "dev-a", "/tmp/one")).unwrap();
    doc.upsert_space(&space(OFF_SPACE, "dev-a", "/tmp/two"))
        .unwrap();
    doc
}

fn apply_as(
    doc: &mut RegistryDoc,
    kind: WorkOutlineActorKind,
    mutation: WorkOutlineMutation,
) -> Result<WorkOutlineMutateReply, WorkOutlineError> {
    doc.apply_work_outline_mutation(&mutation, &actor(kind), NOW)
}

fn apply(
    doc: &mut RegistryDoc,
    mutation: WorkOutlineMutation,
) -> Result<WorkOutlineMutateReply, WorkOutlineError> {
    apply_as(doc, WorkOutlineActorKind::User, mutation)
}

fn enable(doc: &mut RegistryDoc, space_id: &str, op: &str, enabled: bool) {
    apply(
        doc,
        WorkOutlineMutation::SetSpaceLinearEnabled(SetSpaceLinearEnabledParams {
            space_id: space_id.into(),
            enabled,
            operation_id: op.into(),
        }),
    )
    .unwrap();
}

fn issue(identifier: &str, title: &str, description: Option<&str>) -> LinearIssueLink {
    LinearIssueLink {
        id: format!("uuid-{identifier}"),
        identifier: identifier.into(),
        title: title.into(),
        description: description.map(str::to_owned),
        url: format!("https://linear.app/acme/issue/{identifier}"),
    }
}

fn from_linear(
    doc: &mut RegistryDoc,
    space_id: &str,
    op: &str,
    parent: Option<&str>,
    issue: LinearIssueLink,
) -> Result<WorkOutlineMutateReply, WorkOutlineError> {
    apply(
        doc,
        WorkOutlineMutation::CreateWorkItemFromLinear(CreateWorkItemFromLinearParams {
            space_id: space_id.into(),
            parent_work_item_id: parent.map(str::to_owned),
            placement: WorkOutlinePlacement::Last,
            issue,
            operation_id: op.into(),
        }),
    )
}

fn plain_item(doc: &mut RegistryDoc, op: &str, title: &str, description: Option<&str>) -> String {
    apply(
        doc,
        WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: SPACE.into(),
            parent_work_item_id: None,
            placement: WorkOutlinePlacement::Last,
            title: title.into(),
            description: description.map(str::to_owned),
            status: None,
            operation_id: op.into(),
        }),
    )
    .unwrap()
    .work_item_id
    .unwrap()
}

fn update(
    doc: &mut RegistryDoc,
    kind: WorkOutlineActorKind,
    op: &str,
    id: &str,
    changes: WorkItemChanges,
) {
    apply_as(
        doc,
        kind,
        WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: id.into(),
            changes,
            operation_id: op.into(),
        }),
    )
    .unwrap();
}

fn link_issue(doc: &mut RegistryDoc, op: &str, id: &str, identifier: &str) -> String {
    apply(
        doc,
        WorkOutlineMutation::CreateWorkItemExternalReference(
            CreateWorkItemExternalReferenceParams {
                work_item_id: id.into(),
                provider: LINEAR_PROVIDER.into(),
                reference_type: LINEAR_ISSUE_REFERENCE.into(),
                external_id: identifier.into(),
                url: format!("https://linear.app/acme/issue/{identifier}"),
                cached_title: None,
                operation_id: op.into(),
            },
        ),
    )
    .unwrap()
    .reference_id
    .unwrap()
}

fn snap(doc: &RegistryDoc) -> WorkOutlineSnapshot {
    doc.work_outline_snapshot(SPACE, true)
}

fn modified(doc: &RegistryDoc, id: &str) -> Vec<String> {
    snap(doc)
        .item(id)
        .unwrap()
        .external_sync_modified_fields
        .clone()
}

#[test]
fn spaces_default_to_linear_off_and_the_setting_is_lww() {
    let mut doc = doc();
    assert!(!snap(&doc).space_settings.linear_enabled);
    assert!(!doc.space_linear_enabled(SPACE));
    // Turning off what is already off writes nothing.
    let before = doc.batches_written();
    enable(&mut doc, SPACE, "off-0", false);
    assert_eq!(doc.batches_written(), before);
    enable(&mut doc, SPACE, "on-1", true);
    assert!(snap(&doc).space_settings.linear_enabled);
    assert_eq!(doc.linear_enabled_space_ids(), vec![SPACE.to_owned()]);
    assert!(
        !doc.work_outline_snapshot(OFF_SPACE, true)
            .space_settings
            .linear_enabled
    );
    let row = doc
        .overlay_row(KIND_WORK_OUTLINE_SPACE_SETTINGS, SPACE)
        .unwrap();
    assert_eq!(row.fields["spaceId"], json!(SPACE));
    enable(&mut doc, SPACE, "off-2", false);
    assert!(!snap(&doc).space_settings.linear_enabled);
    assert!(doc.linear_enabled_space_ids().is_empty());
    // Unknown Space.
    let err = apply(
        &mut doc,
        WorkOutlineMutation::SetSpaceLinearEnabled(SetSpaceLinearEnabledParams {
            space_id: "nope".into(),
            enabled: true,
            operation_id: "x".into(),
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::NotFound);
}

#[test]
fn create_from_linear_writes_item_and_reference_in_one_batch_and_replays() {
    let mut doc = doc();
    enable(&mut doc, SPACE, "on", true);
    let before = doc.batches_written();
    let reply = from_linear(
        &mut doc,
        SPACE,
        "op-1",
        None,
        issue("ENG-123", "Fix login", Some("Steps\n")),
    )
    .unwrap();
    assert_eq!(doc.batches_written(), before + 1, "one batch");
    let id = reply.work_item_id.clone().unwrap();
    assert_eq!(id, derive_id("op-1", roles::ITEM));
    let s = snap(&doc);
    let item = s.item(&id).unwrap();
    assert_eq!(item.title, "ENG-123 · Fix login");
    assert_eq!(item.description.as_deref(), Some("Steps"));
    assert_eq!(item.status, WorkItemStatus::Pending);
    assert!(item.external_sync_modified_fields.is_empty());
    let reference = s.linear_issue_reference(&id).unwrap();
    assert_eq!(Some(&reference.id), reply.reference_id.as_ref());
    assert_eq!(reference.external_id, "ENG-123");
    assert_eq!(reference.url, "https://linear.app/acme/issue/ENG-123");
    assert_eq!(reference.cached_title.as_deref(), Some("Fix login"));
    // Replay: same ids, no write.
    let written = doc.batches_written();
    let again = from_linear(
        &mut doc,
        SPACE,
        "op-1",
        None,
        issue("ENG-123", "Fix login", Some("Steps\n")),
    )
    .unwrap();
    assert_eq!(again, reply);
    assert_eq!(doc.batches_written(), written);
    // Same operation id, other inputs: conflict.
    let err = from_linear(&mut doc, SPACE, "op-1", None, issue("ENG-9", "x", None)).unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::OperationConflict);
    // Under a parent.
    let child = from_linear(&mut doc, SPACE, "op-2", Some(&id), issue("ENG-7", "Sub", None))
        .unwrap()
        .work_item_id
        .unwrap();
    let s = snap(&doc);
    assert_eq!(s.item(&child).unwrap().parent_work_item_id.as_deref(), Some(id.as_str()));
    assert_eq!(s.item(&child).unwrap().description, None);
}

#[test]
fn linear_operations_are_refused_in_a_space_with_linear_off() {
    let mut doc = doc();
    let err = from_linear(&mut doc, OFF_SPACE, "op-1", None, issue("ENG-1", "x", None))
        .unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::Invalid);
    assert_eq!(err.message, LINEAR_DISABLED_MESSAGE);
    let item = plain_item(&mut doc, "plain", "Plain", None);
    let err = apply(
        &mut doc,
        WorkOutlineMutation::LinkLinearProject(LinkLinearProjectParams {
            work_item_id: item.clone(),
            project: LinearProjectRef {
                id: "p-1".into(),
                name: "Auth".into(),
                url: "https://linear.app/acme/project/auth".into(),
            },
            operation_id: "link".into(),
        }),
    )
    .unwrap_err();
    assert_eq!(err.message, LINEAR_DISABLED_MESSAGE);
    assert!(snap(&doc).external_references.is_empty());
}

#[test]
fn agents_cannot_run_linear_operations() {
    let mut doc = doc();
    enable(&mut doc, SPACE, "on", true);
    let mut agent = actor(WorkOutlineActorKind::Agent);
    agent.chat_id = Some("chat-1".into());
    let err = doc
        .apply_work_outline_mutation(
            &WorkOutlineMutation::SetSpaceLinearEnabled(SetSpaceLinearEnabledParams {
                space_id: SPACE.into(),
                enabled: false,
                operation_id: "agent-off".into(),
            }),
            &agent,
            NOW,
        )
        .unwrap_err();
    assert_eq!(err.code, WorkOutlineErrorCode::Unauthorized);
    assert!(doc.space_linear_enabled(SPACE));
}

#[test]
fn link_linear_project_adds_a_project_reference() {
    let mut doc = doc();
    enable(&mut doc, SPACE, "on", true);
    let item = plain_item(&mut doc, "plain", "Plain", None);
    let reply = apply(
        &mut doc,
        WorkOutlineMutation::LinkLinearProject(LinkLinearProjectParams {
            work_item_id: item.clone(),
            project: LinearProjectRef {
                id: "p-1".into(),
                name: "Auth".into(),
                url: "https://linear.app/acme/project/auth".into(),
            },
            operation_id: "link".into(),
        }),
    )
    .unwrap();
    let s = snap(&doc);
    let reference = s.linear_project_reference(&item).unwrap();
    assert_eq!(Some(&reference.id), reply.reference_id.as_ref());
    assert_eq!(reference.external_id, "p-1");
    assert_eq!(reference.cached_title.as_deref(), Some("Auth"));
    assert_eq!(
        s.nearest_linear_reference(&item, LINEAR_PROJECT_REFERENCE)
            .map(|r| r.id.clone()),
        reply.reference_id
    );
}

#[test]
fn user_edits_mark_fields_and_system_edits_do_not() {
    let mut doc = doc();
    enable(&mut doc, SPACE, "on", true);
    let id = from_linear(&mut doc, SPACE, "op-1", None, issue("ENG-1", "One", Some("Body")))
        .unwrap()
        .work_item_id
        .unwrap();
    // The sync writer (system actor) changes text without marking.
    update(
        &mut doc,
        WorkOutlineActorKind::System,
        "sync-1",
        &id,
        WorkItemChanges {
            title: Some("ENG-1 · One v2".into()),
            description: FieldPatch::Set("Body v2".into()),
            ..Default::default()
        },
    );
    assert!(modified(&doc, &id).is_empty());
    // A user status change marks nothing.
    update(
        &mut doc,
        WorkOutlineActorKind::User,
        "status",
        &id,
        WorkItemChanges {
            status: Some(WorkItemStatus::Active),
            ..Default::default()
        },
    );
    assert!(modified(&doc, &id).is_empty());
    // Setting the same title is not an edit.
    update(
        &mut doc,
        WorkOutlineActorKind::User,
        "same",
        &id,
        WorkItemChanges {
            title: Some("ENG-1 · One v2".into()),
            ..Default::default()
        },
    );
    assert!(modified(&doc, &id).is_empty());
    // A user title edit marks title, in the same op as the title.
    let before = doc.batches_written();
    update(
        &mut doc,
        WorkOutlineActorKind::User,
        "rename",
        &id,
        WorkItemChanges {
            title: Some("My own title".into()),
            ..Default::default()
        },
    );
    assert_eq!(doc.batches_written(), before + 1);
    assert_eq!(modified(&doc, &id), vec!["title".to_owned()]);
    let row = doc.overlay_row(KIND_WORK_ITEMS, &id).unwrap();
    assert_eq!(row.clocks.get("title"), row.clocks.get(MODIFIED_FIELDS_FIELD));
    // Then the description (cleared counts as an edit).
    update(
        &mut doc,
        WorkOutlineActorKind::User,
        "clear",
        &id,
        WorkItemChanges {
            description: FieldPatch::Clear,
            ..Default::default()
        },
    );
    assert_eq!(
        modified(&doc, &id),
        vec!["description".to_owned(), "title".to_owned()]
    );
    // Unlinking the issue clears the marks.
    let reference = snap(&doc).linear_issue_reference(&id).unwrap().id.clone();
    apply(
        &mut doc,
        WorkOutlineMutation::RemoveWorkItemExternalReference(RemoveWorkItemExternalReferenceParams {
            reference_id: reference,
            operation_id: "unlink".into(),
        }),
    )
    .unwrap();
    assert!(modified(&doc, &id).is_empty());
    assert!(snap(&doc).linear_issue_reference(&id).is_none());
}

#[test]
fn edits_of_unlinked_items_mark_nothing() {
    let mut doc = doc();
    let id = plain_item(&mut doc, "plain", "Plain", None);
    update(
        &mut doc,
        WorkOutlineActorKind::User,
        "rename",
        &id,
        WorkItemChanges {
            title: Some("Renamed".into()),
            ..Default::default()
        },
    );
    assert!(modified(&doc, &id).is_empty());
}

#[test]
fn linking_an_issue_keeps_an_existing_description() {
    let mut doc = doc();
    enable(&mut doc, SPACE, "on", true);
    let with_text = plain_item(&mut doc, "a", "Mine", Some("My notes"));
    let without = plain_item(&mut doc, "b", "Bare", None);
    link_issue(&mut doc, "link-a", &with_text, "ENG-1");
    link_issue(&mut doc, "link-b", &without, "ENG-2");
    assert_eq!(modified(&doc, &with_text), vec!["description".to_owned()]);
    assert!(modified(&doc, &without).is_empty());
    // A second issue reference: removing one keeps the marks.
    let second = link_issue(&mut doc, "link-a2", &with_text, "ENG-3");
    apply(
        &mut doc,
        WorkOutlineMutation::RemoveWorkItemExternalReference(RemoveWorkItemExternalReferenceParams {
            reference_id: second,
            operation_id: "unlink-a2".into(),
        }),
    )
    .unwrap();
    assert_eq!(modified(&doc, &with_text), vec!["description".to_owned()]);
}

#[test]
fn space_deletion_removes_the_settings_row() {
    let mut doc = doc();
    enable(&mut doc, SPACE, "on", true);
    doc.delete_space(SPACE).unwrap();
    assert_eq!(
        doc.row_state(KIND_WORK_OUTLINE_SPACE_SETTINGS, SPACE),
        RowState::Tombstoned
    );
    assert!(doc.linear_enabled_space_ids().is_empty());
}

#[test]
fn system_writes_never_overwrite_a_field_the_user_took_over() {
    let mut doc = doc();
    enable(&mut doc, SPACE, "on", true);
    let id = from_linear(&mut doc, SPACE, "op-1", None, issue("ENG-1", "One", Some("Body")))
        .unwrap()
        .work_item_id
        .unwrap();
    update(
        &mut doc,
        WorkOutlineActorKind::User,
        "rename",
        &id,
        WorkItemChanges {
            title: Some("Mine".into()),
            ..Default::default()
        },
    );
    // A sync that read the item before the rename still sends a title.
    update(
        &mut doc,
        WorkOutlineActorKind::System,
        "sync",
        &id,
        WorkItemChanges {
            title: Some("ENG-1 · One v2".into()),
            description: FieldPatch::Set("Body v2".into()),
            ..Default::default()
        },
    );
    let s = snap(&doc);
    let item = s.item(&id).unwrap();
    assert_eq!(item.title, "Mine");
    assert_eq!(item.description.as_deref(), Some("Body v2"));
    assert_eq!(item.status, WorkItemStatus::Pending);
}
