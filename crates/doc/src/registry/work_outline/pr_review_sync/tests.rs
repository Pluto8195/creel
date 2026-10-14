//! PR review sync rows: the destination setting, the system-only sync
//! record, and system-origin user action requests.

use super::*;
use crate::registry::tests::space;
use zeron_proto::pr_review_sync::{PrReviewRequestState, WorkItemExternalSyncState};
use zeron_proto::work_outline::{
    ArchiveWorkItemParams, CreateWorkItemParams, DescendantArchivePolicy, UpdateWorkItemParams,
    WorkItemChanges, WorkItemStatus, WorkOutlineActor, WorkOutlineActorKind,
    WorkOutlineMutateReply, WorkOutlineMutation, WorkOutlinePlacement, WorkOutlineSnapshot,
};
use zeron_proto::work_outline_user_actions::{
    CreateUserActionRequestParams, ResolveUserActionRequestParams, UserActionOriginKind,
    UserActionRequestOrigin,
};
use zeron_proto::WorkOutlineError;

const SPACE: &str = "sp-1";
const OTHER: &str = "sp-2";
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
    doc.upsert_space(&space(OTHER, "dev-a", "/tmp/two")).unwrap();
    doc
}

fn apply_as(
    doc: &mut RegistryDoc,
    kind: WorkOutlineActorKind,
    mutation: WorkOutlineMutation,
) -> Result<WorkOutlineMutateReply, WorkOutlineError> {
    doc.apply_work_outline_mutation(&mutation, &actor(kind), NOW)
}

fn create(doc: &mut RegistryDoc, space_id: &str, op: &str) -> String {
    apply_as(
        doc,
        WorkOutlineActorKind::User,
        WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: space_id.into(),
            parent_work_item_id: None,
            placement: WorkOutlinePlacement::Last,
            title: "Reviews".into(),
            description: None,
            status: None,
            operation_id: op.into(),
        }),
    )
    .unwrap()
    .work_item_id
    .unwrap()
}

fn set_destination(
    doc: &mut RegistryDoc,
    space_id: &str,
    item: Option<&str>,
    op: &str,
) -> Result<WorkOutlineMutateReply, WorkOutlineError> {
    apply_as(
        doc,
        WorkOutlineActorKind::User,
        WorkOutlineMutation::SetPrReviewSyncDestination(SetPrReviewSyncDestinationParams {
            space_id: space_id.into(),
            work_item_id: item.map(str::to_owned),
            operation_id: op.into(),
        }),
    )
}

fn snapshot(doc: &RegistryDoc, space_id: &str) -> WorkOutlineSnapshot {
    doc.work_outline_snapshot(space_id, true)
}

#[test]
fn destination_is_validated_set_and_cleared() {
    let mut doc = doc();
    let item = create(&mut doc, SPACE, "item");
    let other = create(&mut doc, OTHER, "other");
    assert_eq!(snapshot(&doc, SPACE).pr_review_sync_destination(), None);
    // Clearing an absent setting writes nothing.
    set_destination(&mut doc, SPACE, None, "noop").unwrap();
    assert!(doc.pr_review_sync_space_ids().is_empty());

    let error = set_destination(&mut doc, SPACE, Some(&other), "cross").unwrap_err();
    assert_eq!(error.code, WorkOutlineErrorCode::CrossSpace);
    let error = set_destination(&mut doc, SPACE, Some("missing"), "missing").unwrap_err();
    assert_eq!(error.code, WorkOutlineErrorCode::NotFound);

    set_destination(&mut doc, SPACE, Some(&item), "set").unwrap();
    let snap = snapshot(&doc, SPACE);
    assert_eq!(snap.pr_review_sync_destination(), Some(item.as_str()));
    assert!(!snap.space_settings.linear_enabled);
    assert_eq!(doc.pr_review_sync_destination(SPACE), Some(item.clone()));
    assert_eq!(doc.pr_review_sync_space_ids(), vec![SPACE.to_owned()]);

    set_destination(&mut doc, SPACE, None, "clear").unwrap();
    assert_eq!(snapshot(&doc, SPACE).pr_review_sync_destination(), None);
    assert!(doc.pr_review_sync_space_ids().is_empty());

    // An archived item cannot become the destination.
    apply_as(
        &mut doc,
        WorkOutlineActorKind::User,
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: item.clone(),
            descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
            operation_id: "archive".into(),
        }),
    )
    .unwrap();
    let error = set_destination(&mut doc, SPACE, Some(&item), "archived").unwrap_err();
    assert_eq!(error.code, WorkOutlineErrorCode::Archived);
}

#[test]
fn agents_cannot_set_the_destination() {
    let mut doc = doc();
    let item = create(&mut doc, SPACE, "item");
    let mutation =
        WorkOutlineMutation::SetPrReviewSyncDestination(SetPrReviewSyncDestinationParams {
            space_id: SPACE.into(),
            work_item_id: Some(item),
            operation_id: "op".into(),
        });
    let mut agent = actor(WorkOutlineActorKind::Agent);
    agent.chat_id = Some("chat-x".into());
    let error = doc
        .apply_work_outline_mutation(&mutation, &agent, NOW)
        .unwrap_err();
    assert_eq!(error.code, WorkOutlineErrorCode::Unauthorized);
}

fn state(round: u32) -> WorkItemExternalSyncState {
    WorkItemExternalSyncState {
        provider: "github".into(),
        external_id: "acme/ui#9".into(),
        round,
        request_state: PrReviewRequestState::Requested,
        synced_status: WorkItemStatus::Active,
        round_started_at: NOW,
        synced_at: NOW,
        archived_by_sync: None,
    }
}

fn record(state: WorkItemExternalSyncState, item: &str, op: &str) -> WorkOutlineMutation {
    WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
        work_item_id: item.into(),
        changes: WorkItemChanges {
            status: Some(WorkItemStatus::Active),
            external_sync_state: Some(state),
            ..Default::default()
        },
        operation_id: op.into(),
    })
}

#[test]
fn only_the_system_writes_the_sync_record() {
    let mut doc = doc();
    let item = create(&mut doc, SPACE, "item");
    for kind in [WorkOutlineActorKind::User, WorkOutlineActorKind::Agent] {
        let mut who = actor(kind);
        who.chat_id = Some("chat-x".into());
        let error = doc
            .apply_work_outline_mutation(&record(state(1), &item, "u"), &who, NOW)
            .unwrap_err();
        assert_eq!(error.code, WorkOutlineErrorCode::Unauthorized, "{kind:?}");
    }
    assert_eq!(snapshot(&doc, SPACE).item(&item).unwrap().external_sync_state, None);

    apply_as(&mut doc, WorkOutlineActorKind::System, record(state(1), &item, "s1")).unwrap();
    let snap = snapshot(&doc, SPACE);
    let view = snap.item(&item).unwrap();
    assert_eq!(view.external_sync_state, Some(state(1)));
    assert_eq!(view.status, WorkItemStatus::Active);
    let value = serde_json::to_value(view).unwrap();
    assert_eq!(value["externalSyncState"]["round"], serde_json::json!(1));

    apply_as(&mut doc, WorkOutlineActorKind::System, record(state(2), &item, "s2")).unwrap();
    assert_eq!(
        snapshot(&doc, SPACE).item(&item).unwrap().external_sync_state,
        Some(state(2))
    );
}

fn system_request(item: &str, op: &str, dedupe: &str) -> WorkOutlineMutation {
    WorkOutlineMutation::CreateUserActionRequest(CreateUserActionRequestParams {
        work_item_id: item.into(),
        origin: UserActionRequestOrigin {
            kind: UserActionOriginKind::System,
            chat_id: String::new(),
            message_id: None,
            tool_call_id: None,
        },
        text: "Review requested: ui#9 Fix".into(),
        info: Some("Author: ada".into()),
        dedupe_key: Some(dedupe.into()),
        operation_id: op.into(),
    })
}

#[test]
fn system_origin_requests_need_the_system_actor_and_have_no_chat() {
    let mut doc = doc();
    let item = create(&mut doc, SPACE, "item");
    for kind in [WorkOutlineActorKind::User, WorkOutlineActorKind::Agent] {
        let mut who = actor(kind);
        who.chat_id = Some("chat-x".into());
        let error = doc
            .apply_work_outline_mutation(&system_request(&item, "u", "k"), &who, NOW)
            .unwrap_err();
        assert_eq!(error.code, WorkOutlineErrorCode::Unauthorized, "{kind:?}");
    }
    // A system request naming a Chat is malformed.
    let mut bad = system_request(&item, "bad", "k");
    if let WorkOutlineMutation::CreateUserActionRequest(p) = &mut bad {
        p.origin.chat_id = "chat-x".into();
    }
    let error = apply_as(&mut doc, WorkOutlineActorKind::System, bad).unwrap_err();
    assert_eq!(error.code, WorkOutlineErrorCode::Invalid);

    let id = apply_as(
        &mut doc,
        WorkOutlineActorKind::System,
        system_request(&item, "req-1", "pr-review:acme/ui#9:1"),
    )
    .unwrap()
    .user_action_request_id
    .unwrap();
    let snap = snapshot(&doc, SPACE);
    let request = snap.user_action_request(&id).unwrap();
    assert_eq!(request.origin_kind, UserActionOriginKind::System);
    assert_eq!(request.chat_id, "");
    assert!(!request.origin_chat_available);
    assert!(request.is_unresolved());
    assert_eq!(
        snap.item(&item).unwrap().effective_status,
        zeron_proto::WorkItemEffectiveStatus::UserActionRequired
    );
    // Replays are idempotent.
    apply_as(
        &mut doc,
        WorkOutlineActorKind::System,
        system_request(&item, "req-1", "pr-review:acme/ui#9:1"),
    )
    .unwrap();
    assert_eq!(snapshot(&doc, SPACE).user_action_requests.len(), 1);

    // The system resolves it with a note.
    apply_as(
        &mut doc,
        WorkOutlineActorKind::System,
        WorkOutlineMutation::ResolveUserActionRequest(ResolveUserActionRequestParams {
            user_action_request_id: id.clone(),
            note: Some("Review submitted".into()),
            operation_id: "resolve".into(),
        }),
    )
    .unwrap();
    let snap = snapshot(&doc, SPACE);
    let request = snap.user_action_request(&id).unwrap();
    assert!(!request.is_open());
    let resolution = request.resolution.as_ref().unwrap();
    assert_eq!(resolution.note.as_deref(), Some("Review submitted"));
    assert_eq!(resolution.by.kind, WorkOutlineActorKind::System);
}
