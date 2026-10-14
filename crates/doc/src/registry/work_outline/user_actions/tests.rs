//! Milestone 4 registry tests: user action requests (create, dedupe,
//! resolve, history, retries, effective status, completion gate, rollups,
//! missing origin Chats, convergence), instruction proposals (approve,
//! reject, concurrent review, provenance, budgets), Space deletion and the
//! sweep, and the agent authorization matrix.

use std::collections::HashMap;

use sha2::{Digest, Sha256};
use zeron_proto::work_outline::{
    ArchiveWorkItemInstructionParams, ArchiveWorkItemParams, AssignChatToWorkItemParams,
    CreateInstructionRevisionParams, CreateWorkItemExternalReferenceParams,
    CreateWorkItemInstructionParams, CreateWorkItemParams, InstructionPlacement,
    MoveWorkItemParams, RemoveChatFromWorkItemParams, RepairWorkOutlineParams,
    RestoreWorkItemParams, UpdateWorkItemInstructionParams, UpdateWorkItemParams, WorkItemChanges,
    WorkItemEffectiveStatus,
};
use zeron_proto::work_outline_execution::{ConfigureExecutionParams, ExecutionScope};
use zeron_proto::work_outline_user_actions::{
    InstructionProposalOrigin, UserActionRequestOrigin, user_action_limits,
};

use super::*;
use crate::registry::tests::{chat, server_round, space};

const SPACE: &str = "sp-1";
const OTHER_SPACE: &str = "sp-2";
const NOW: i64 = 1_700_000_000_000;

fn user(device: &str) -> WorkOutlineActor {
    WorkOutlineActor {
        kind: WorkOutlineActorKind::User,
        device_id: device.into(),
        chat_id: None,
        message_id: None,
    }
}

fn agent(chat_id: &str) -> WorkOutlineActor {
    WorkOutlineActor {
        kind: WorkOutlineActorKind::Agent,
        device_id: "dev-a".into(),
        chat_id: Some(chat_id.into()),
        message_id: None,
    }
}

fn space_chat(id: &str, space_id: Option<&str>) -> zeron_proto::Chat {
    let mut chat = chat(id, "dev-a");
    chat.space_id = space_id.map(str::to_owned);
    chat.parent_chat_id = None;
    chat
}

/// Spaces, Chats `c1` / `c2` in `SPACE`, `other` in `OTHER_SPACE`.
fn doc_with_spaces(device: &str) -> RegistryDoc {
    let mut doc = RegistryDoc::new(device);
    doc.upsert_space(&space(SPACE, device, "/tmp/one")).unwrap();
    doc.upsert_space(&space(OTHER_SPACE, device, "/tmp/two"))
        .unwrap();
    for (id, space_id) in [("c1", SPACE), ("c2", SPACE), ("other", OTHER_SPACE)] {
        doc.upsert_chat(&space_chat(id, Some(space_id))).unwrap();
    }
    doc
}

fn apply_as(
    doc: &mut RegistryDoc,
    actor: &WorkOutlineActor,
    mutation: WorkOutlineMutation,
) -> Outcome<WorkOutlineMutateReply> {
    doc.apply_work_outline_mutation(&mutation, actor, NOW)
}

fn apply(doc: &mut RegistryDoc, mutation: WorkOutlineMutation) -> Outcome<WorkOutlineMutateReply> {
    let device = doc.device_id().to_owned();
    apply_as(doc, &user(&device), mutation)
}

fn create_item_params(op: &str, space_id: &str, parent: Option<&str>) -> WorkOutlineMutation {
    WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
        space_id: space_id.into(),
        parent_work_item_id: parent.map(str::to_owned),
        placement: WorkOutlinePlacement::Last,
        title: format!("Item {op}"),
        description: None,
        status: None,
        operation_id: op.into(),
    })
}

fn item_in(doc: &mut RegistryDoc, space_id: &str, op: &str, parent: Option<&str>) -> String {
    apply(doc, create_item_params(op, space_id, parent))
        .unwrap()
        .work_item_id
        .unwrap()
}

fn item(doc: &mut RegistryDoc, op: &str, parent: Option<&str>) -> String {
    item_in(doc, SPACE, op, parent)
}

fn assign(doc: &mut RegistryDoc, op: &str, chat_id: &str, work_item: &str) {
    apply(doc, assign_params(op, chat_id, work_item)).unwrap();
}

fn assign_params(op: &str, chat_id: &str, work_item: &str) -> WorkOutlineMutation {
    WorkOutlineMutation::AssignChatToWorkItem(AssignChatToWorkItemParams {
        chat_id: chat_id.into(),
        work_item_id: work_item.into(),
        operation_id: op.into(),
    })
}

fn request_params(
    op: &str,
    work_item: &str,
    chat_id: &str,
    dedupe: Option<&str>,
) -> WorkOutlineMutation {
    WorkOutlineMutation::CreateUserActionRequest(CreateUserActionRequestParams {
        work_item_id: work_item.into(),
        origin: UserActionRequestOrigin {
            kind: Default::default(),
            chat_id: chat_id.into(),
            message_id: Some("m-1".into()),
            tool_call_id: Some("t-1".into()),
        },
        text: "Approve the migration?".into(),
        info: Some("It drops a column".into()),
        dedupe_key: dedupe.map(str::to_owned),
        operation_id: op.into(),
    })
}

fn request(doc: &mut RegistryDoc, op: &str, work_item: &str, dedupe: Option<&str>) -> String {
    apply(doc, request_params(op, work_item, "c1", dedupe))
        .unwrap()
        .user_action_request_id
        .unwrap()
}

fn resolve_params(op: &str, id: &str, note: Option<&str>) -> WorkOutlineMutation {
    WorkOutlineMutation::ResolveUserActionRequest(ResolveUserActionRequestParams {
        user_action_request_id: id.into(),
        note: note.map(str::to_owned),
        operation_id: op.into(),
    })
}

fn status_params(
    op: &str,
    work_item: &str,
    status: WorkItemStatus,
    resolve: bool,
) -> WorkOutlineMutation {
    WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
        work_item_id: work_item.into(),
        changes: WorkItemChanges {
            status: Some(status),
            resolve_open_requests: resolve,
            ..Default::default()
        },
        operation_id: op.into(),
    })
}

fn propose_params(
    op: &str,
    target: InstructionProposalTarget,
    chat_id: Option<&str>,
) -> WorkOutlineMutation {
    WorkOutlineMutation::ProposeWorkItemInstruction(ProposeWorkItemInstructionParams {
        space_id: None,
        target,
        origin: InstructionProposalOrigin {
            chat_id: chat_id.map(str::to_owned),
            message_id: Some("m-9".into()),
            tool_call_id: None,
        },
        text: "Always run the linter before committing".into(),
        operation_id: op.into(),
    })
}

fn approve_params(op: &str, proposal: &str, text: &str) -> WorkOutlineMutation {
    WorkOutlineMutation::ApproveWorkItemInstructionProposal(
        ApproveWorkItemInstructionProposalParams {
            proposal_id: proposal.into(),
            approved_text: text.into(),
            placement: None,
            operation_id: op.into(),
        },
    )
}

fn reject_params(op: &str, proposal: &str) -> WorkOutlineMutation {
    WorkOutlineMutation::RejectWorkItemInstructionProposal(
        RejectWorkItemInstructionProposalParams {
            proposal_id: proposal.into(),
            reason: Some("too broad".into()),
            operation_id: op.into(),
        },
    )
}

fn snap(doc: &RegistryDoc) -> WorkOutlineSnapshot {
    doc.work_outline_snapshot(SPACE, true)
}

fn pending_ops(doc: &RegistryDoc) -> usize {
    doc.pending.iter().map(|b| b.ops.len()).sum()
}

// ── requests ────────────────────────────────────────────────────────────────

#[test]
fn request_create_dedupe_resolve_and_history() {
    let mut doc = doc_with_spaces("dev-a");
    let w = item(&mut doc, "op-w", None);
    let first = request(&mut doc, "op-r1", &w, Some("migration"));
    assert_eq!(first, derive_id("op-r1", roles::REQUEST));
    let duplicate = request(&mut doc, "op-r2", &w, Some("migration"));
    let unrelated = request(&mut doc, "op-r3", &w, None);

    let s = snap(&doc);
    let view = s.user_action_request(&first).unwrap();
    assert_eq!(view.space_id, SPACE);
    assert_eq!(view.work_item_id, w);
    assert_eq!(view.chat_id, "c1");
    assert_eq!(view.message_id.as_deref(), Some("m-1"));
    assert_eq!(view.tool_call_id.as_deref(), Some("t-1"));
    assert_eq!(view.info.as_deref(), Some("It drops a column"));
    assert_eq!(view.created_at, NOW);
    assert_eq!(
        view.created_by.as_ref().unwrap().kind,
        WorkOutlineActorKind::User
    );
    assert!(view.is_unresolved() && view.origin_chat_available);
    assert!(s.user_action_request(&duplicate).unwrap().deduped);
    assert!(!s.user_action_request(&unrelated).unwrap().deduped);
    let item_view = s.item(&w).unwrap();
    assert_eq!(
        item_view.rollup.unresolved_user_action_count, 2,
        "deduped not counted"
    );
    assert_eq!(
        item_view.effective_status,
        WorkItemEffectiveStatus::UserActionRequired
    );

    // Resolving the duplicate resolves the whole dedupe group in one batch.
    let before = doc.pending_len();
    apply(
        &mut doc,
        resolve_params("op-x1", &duplicate, Some("Approved")),
    )
    .unwrap();
    assert_eq!(doc.pending_len(), before + 1);
    let s = snap(&doc);
    for id in [&first, &duplicate] {
        let view = s.user_action_request(id).unwrap();
        assert_eq!(view.state, UserActionRequestState::Resolved);
        let resolution = view.resolution.as_ref().unwrap();
        assert_eq!(resolution.note.as_deref(), Some("Approved"));
        assert_eq!(resolution.at, NOW);
        assert_eq!(resolution.by.kind, WorkOutlineActorKind::User);
        assert!(!view.deduped);
    }
    assert_eq!(s.item(&w).unwrap().rollup.unresolved_user_action_count, 1);

    // Monotone: resolving again writes nothing; history keeps the first note.
    let before = pending_ops(&doc);
    apply(&mut doc, resolve_params("op-x2", &first, Some("again"))).unwrap();
    assert_eq!(pending_ops(&doc), before);
    assert_eq!(
        snap(&doc)
            .user_action_request(&first)
            .unwrap()
            .resolution
            .as_ref()
            .unwrap()
            .note
            .as_deref(),
        Some("Approved")
    );

    // A new request with the same key after resolution is a fresh request.
    let again = request(&mut doc, "op-r4", &w, Some("migration"));
    assert!(
        snap(&doc)
            .user_action_request(&again)
            .unwrap()
            .is_unresolved()
    );
}

#[test]
fn request_retries_never_duplicate() {
    let mut doc = doc_with_spaces("dev-a");
    let w = item(&mut doc, "op-w", None);
    let id = request(&mut doc, "op-r", &w, None);
    let before = pending_ops(&doc);
    assert_eq!(request(&mut doc, "op-r", &w, None), id);
    assert_eq!(pending_ops(&doc), before, "replay writes nothing");
    let conflict = apply(&mut doc, request_params("op-r", &w, "c2", None)).unwrap_err();
    assert_eq!(conflict.code, WorkOutlineErrorCode::OperationConflict);
    assert_eq!(snap(&doc).user_action_requests.len(), 1);
    // A tombstoned request is never revived.
    doc.delete_row_ops(&[(KIND_USER_ACTION_REQUESTS, id.as_str())]);
    let gone = apply(&mut doc, request_params("op-r", &w, "c1", None)).unwrap_err();
    assert_eq!(gone.code, WorkOutlineErrorCode::NotFound);
}

#[test]
fn request_validation() {
    let mut doc = doc_with_spaces("dev-a");
    let w = item(&mut doc, "op-w", None);
    let code = |doc: &mut RegistryDoc, m| apply(doc, m).unwrap_err().code;
    assert_eq!(
        code(&mut doc, request_params("a", "missing", "c1", None)),
        WorkOutlineErrorCode::NotFound
    );
    assert_eq!(
        code(&mut doc, request_params("b", &w, "nope", None)),
        WorkOutlineErrorCode::NotFound
    );
    assert_eq!(
        code(&mut doc, request_params("c", &w, "other", None)),
        WorkOutlineErrorCode::CrossSpace
    );
    doc.upsert_chat(&space_chat("np", None)).unwrap();
    assert_eq!(
        code(&mut doc, request_params("d", &w, "np", None)),
        WorkOutlineErrorCode::Projectless
    );
    let WorkOutlineMutation::CreateUserActionRequest(mut long) =
        request_params("e", &w, "c1", None)
    else {
        unreachable!()
    };
    long.text = "x".repeat(user_action_limits::MAX_REQUEST_TEXT_CHARS + 1);
    assert_eq!(
        code(&mut doc, WorkOutlineMutation::CreateUserActionRequest(long)),
        WorkOutlineErrorCode::LimitExceeded
    );
    apply(
        &mut doc,
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: w.clone(),
            descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
            operation_id: "arch".into(),
        }),
    )
    .unwrap();
    assert_eq!(
        code(&mut doc, request_params("f", &w, "c1", None)),
        WorkOutlineErrorCode::Archived
    );
}

#[test]
fn effective_status_reveals_the_unchanged_base_status() {
    let mut doc = doc_with_spaces("dev-a");
    let w = item(&mut doc, "op-w", None);
    apply(
        &mut doc,
        status_params("s1", &w, WorkItemStatus::Active, false),
    )
    .unwrap();
    let r1 = request(&mut doc, "op-r1", &w, None);
    let r2 = request(&mut doc, "op-r2", &w, None);
    let s = snap(&doc);
    let view = s.item(&w).unwrap();
    assert_eq!(view.status, WorkItemStatus::Active, "base status unchanged");
    assert_eq!(
        view.effective_status,
        WorkItemEffectiveStatus::UserActionRequired
    );
    assert_eq!(view.rollup.active_count, 1, "stored status still counted");
    apply(&mut doc, resolve_params("x1", &r1, None)).unwrap();
    assert_eq!(
        snap(&doc).item(&w).unwrap().effective_status,
        WorkItemEffectiveStatus::UserActionRequired
    );
    apply(&mut doc, resolve_params("x2", &r2, None)).unwrap();
    let s = snap(&doc);
    let view = s.item(&w).unwrap();
    assert_eq!(view.effective_status, WorkItemEffectiveStatus::Active);
    assert_eq!(view.rollup.unresolved_user_action_count, 0);
    assert_eq!(view.rollup.user_action_required_count, 0);
}

#[test]
fn completing_with_open_requests_errors_or_resolves_every_one_explicitly() {
    let mut doc = doc_with_spaces("dev-a");
    let w = item(&mut doc, "op-w", None);
    let r1 = request(&mut doc, "op-r1", &w, Some("k"));
    let r2 = request(&mut doc, "op-r2", &w, Some("k"));
    let r3 = request(&mut doc, "op-r3", &w, None);
    let before = pending_ops(&doc);
    let error = apply(
        &mut doc,
        status_params("s1", &w, WorkItemStatus::Completed, false),
    )
    .unwrap_err();
    assert_eq!(error.code, WorkOutlineErrorCode::OpenUserActionRequests);
    assert_eq!(
        error.open_request_count(),
        Some(2),
        "deduped duplicate not counted"
    );
    assert_eq!(pending_ops(&doc), before, "nothing written");
    assert_eq!(snap(&doc).item(&w).unwrap().status, WorkItemStatus::Pending);

    // Other statuses are not gated.
    apply(
        &mut doc,
        status_params("s2", &w, WorkItemStatus::Active, false),
    )
    .unwrap();

    let batches = doc.pending_len();
    apply(
        &mut doc,
        status_params("s3", &w, WorkItemStatus::Completed, true),
    )
    .unwrap();
    assert_eq!(
        doc.pending_len(),
        batches + 1,
        "status + resolutions in one batch"
    );
    let s = snap(&doc);
    let view = s.item(&w).unwrap();
    assert_eq!(view.status, WorkItemStatus::Completed);
    assert_eq!(view.effective_status, WorkItemEffectiveStatus::Completed);
    for id in [&r1, &r2, &r3] {
        let request = s.user_action_request(id).unwrap();
        assert_eq!(request.state, UserActionRequestState::Resolved);
        assert_eq!(
            request.resolution.as_ref().unwrap().note.as_deref(),
            Some(COMPLETION_RESOLUTION_NOTE)
        );
    }
    // A completed item with a new request shows it; re-completing resolves it.
    let r4 = request(&mut doc, "op-r4", &w, None);
    assert_eq!(
        snap(&doc).item(&w).unwrap().effective_status,
        WorkItemEffectiveStatus::UserActionRequired
    );
    assert_eq!(
        apply(
            &mut doc,
            status_params("s4", &w, WorkItemStatus::Completed, false)
        )
        .unwrap_err()
        .code,
        WorkOutlineErrorCode::OpenUserActionRequests
    );
    apply(
        &mut doc,
        status_params("s5", &w, WorkItemStatus::Completed, true),
    )
    .unwrap();
    assert!(!snap(&doc).user_action_request(&r4).unwrap().is_open());
}

#[test]
fn rollups_reach_every_ancestor() {
    let mut doc = doc_with_spaces("dev-a");
    let root = item(&mut doc, "op-root", None);
    let mid = item(&mut doc, "op-mid", Some(&root));
    let leaf = item(&mut doc, "op-leaf", Some(&mid));
    let sibling = item(&mut doc, "op-sib", Some(&root));
    for (op, id) in [("a1", &mid), ("a2", &leaf), ("a3", &sibling)] {
        apply(
            &mut doc,
            status_params(op, id, WorkItemStatus::Active, false),
        )
        .unwrap();
    }
    request(&mut doc, "r1", &leaf, None);
    request(&mut doc, "r2", &leaf, None);
    request(&mut doc, "r3", &sibling, None);
    let s = snap(&doc);
    let rollup = |id: &str| s.item(id).unwrap().rollup.clone();
    assert_eq!(rollup(&leaf).unresolved_user_action_count, 2);
    assert_eq!(rollup(&leaf).user_action_required_count, 1);
    assert_eq!(rollup(&mid).unresolved_user_action_count, 0);
    assert_eq!(rollup(&mid).user_action_required_count, 1);
    assert_eq!(rollup(&mid).subtree_unresolved_user_action_count, 2);
    // "Flagship 3 active · 2 user action required".
    assert_eq!(rollup(&root).active_count, 3);
    assert_eq!(rollup(&root).user_action_required_count, 2);
    assert_eq!(rollup(&root).subtree_unresolved_user_action_count, 3);
    assert_eq!(
        s.item(&root).unwrap().effective_status,
        WorkItemEffectiveStatus::Pending,
        "ancestors keep their own status"
    );
}

#[test]
fn requests_stay_inspectable_when_the_origin_chat_is_archived_or_deleted() {
    let mut doc = doc_with_spaces("dev-a");
    let w = item(&mut doc, "op-w", None);
    let r1 = request(&mut doc, "r1", &w, None);
    let r2 = apply(&mut doc, request_params("r2", &w, "c2", None))
        .unwrap()
        .user_action_request_id
        .unwrap();
    doc.set_chat_archived("c1", true).unwrap();
    doc.delete_chat("c2").unwrap();
    let s = snap(&doc);
    for id in [&r1, &r2] {
        let view = s.user_action_request(id).unwrap();
        assert!(!view.origin_chat_available);
        assert!(view.is_unresolved(), "still counts");
    }
    assert_eq!(s.user_action_request(&r2).unwrap().chat_id, "c2");
    // Resolvable without its Chat.
    apply(&mut doc, resolve_params("x", &r2, None)).unwrap();
    assert!(!snap(&doc).user_action_request(&r2).unwrap().is_open());
}

#[test]
fn snapshot_bounds_resolved_history_but_lists_every_open_request() {
    let mut doc = doc_with_spaces("dev-a");
    let w = item(&mut doc, "op-w", None);
    let total = user_action_limits::MAX_RECENT_RESOLVED_REQUESTS + 5;
    for i in 0..total {
        let id = request(&mut doc, &format!("r{i}"), &w, None);
        doc.apply_work_outline_mutation(
            &resolve_params(&format!("x{i}"), &id, None),
            &user("dev-a"),
            NOW + i as i64,
        )
        .unwrap();
    }
    for i in 0..3 {
        request(&mut doc, &format!("open{i}"), &w, None);
    }
    let s = snap(&doc);
    let open = s
        .user_action_requests
        .iter()
        .filter(|r| r.is_open())
        .count();
    let resolved = s.user_action_requests.len() - open;
    assert_eq!(open, 3);
    assert_eq!(resolved, user_action_limits::MAX_RECENT_RESOLVED_REQUESTS);
    // The oldest resolutions are the ones dropped.
    assert!(
        s.user_action_request(&derive_id("r0", roles::REQUEST))
            .is_none()
    );
    assert!(
        s.user_action_request(&derive_id(&format!("r{}", total - 1), roles::REQUEST))
            .is_some()
    );
}

// ── two replicas ────────────────────────────────────────────────────────────

fn synced_pair() -> (
    RegistryDoc,
    RegistryDoc,
    HashMap<(String, String), RegistryRow>,
    u64,
    String,
) {
    let mut a = doc_with_spaces("dev-a");
    let mut b = RegistryDoc::new("dev-b");
    let mut server = HashMap::new();
    let mut seq = 0;
    let w = item(&mut a, "op-w", None);
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    (a, b, server, seq, w)
}

#[test]
fn two_replicas_converge_on_requests() {
    let (mut a, mut b, mut server, mut seq, w) = synced_pair();
    // Concurrent duplicates on both devices collapse to one canonical.
    let ra = request(&mut a, "ra", &w, Some("deploy"));
    let rb = request(&mut b, "rb", &w, Some("deploy"));
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    let (sa, sb) = (snap(&a), snap(&b));
    assert_eq!(sa, sb);
    assert_eq!(sa.item(&w).unwrap().rollup.unresolved_user_action_count, 1);
    let deduped: Vec<bool> = [&ra, &rb]
        .iter()
        .map(|id| sa.user_action_request(id).unwrap().deduped)
        .collect();
    assert_eq!(deduped.iter().filter(|d| **d).count(), 1);

    // Concurrent resolutions with different notes converge (resolved on
    // both; one note wins everywhere).
    let rc = request(&mut a, "rc", &w, None);
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    apply(&mut a, resolve_params("xa", &rc, Some("from a"))).unwrap();
    apply(&mut b, resolve_params("xb", &rc, Some("from b"))).unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    let (sa, sb) = (snap(&a), snap(&b));
    assert_eq!(sa, sb);
    assert!(!sa.user_action_request(&rc).unwrap().is_open());

    // Resolve on one device, re-create on the other: never reopened.
    apply(&mut a, resolve_params("xd", &ra, None)).unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    assert_eq!(snap(&a), snap(&b));
    assert!(snap(&b).user_action_requests.iter().all(|r| !r.is_open()));
}

#[test]
fn concurrent_approve_and_reject_converge_to_approved() {
    let (mut a, mut b, mut server, mut seq, w) = synced_pair();
    let p = apply(
        &mut a,
        propose_params("op-p", InstructionProposalTarget::work_item(&w), Some("c1")),
    )
    .unwrap()
    .proposal_id
    .unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    apply(&mut a, approve_params("ap", &p, "Run the linter")).unwrap();
    // B rejects later in HLC order, so its review wins the register.
    b.clock.last_ms = Utc::now().timestamp_millis() + 60_000;
    apply(&mut b, reject_params("rj", &p)).unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    let (sa, sb) = (snap(&a), snap(&b));
    assert_eq!(sa, sb);
    let proposal = sa.instruction_proposal(&p).unwrap();
    assert_eq!(proposal.status, InstructionProposalStatus::Approved);
    let review = proposal.review.as_ref().unwrap();
    assert_eq!(review.state, ProposalReviewState::Approved);
    assert_eq!(
        review.instruction_id.as_deref(),
        Some(derive_id(&p, roles::INSTRUCTION).as_str())
    );
    // The stored register says rejected; the reader still says approved.
    let stored = a
        .overlay_row(KIND_WORK_ITEM_INSTRUCTION_PROPOSALS, &p)
        .unwrap();
    assert_eq!(stored.fields["review"]["state"], json!("rejected"));
}

// ── proposals ───────────────────────────────────────────────────────────────

#[test]
fn approval_creates_a_provenance_linked_instruction_in_one_batch() {
    let mut doc = doc_with_spaces("dev-a");
    let w = item(&mut doc, "op-w", None);
    let p = apply(
        &mut doc,
        propose_params("op-p", InstructionProposalTarget::work_item(&w), Some("c1")),
    )
    .unwrap()
    .proposal_id
    .unwrap();
    assert_eq!(p, derive_id("op-p", roles::PROPOSAL));
    let s = snap(&doc);
    let proposal = s.instruction_proposal(&p).unwrap();
    assert_eq!(proposal.status, InstructionProposalStatus::Pending);
    assert_eq!(proposal.target, InstructionProposalTarget::work_item(&w));
    assert_eq!(proposal.source_ref.chat_id.as_deref(), Some("c1"));
    assert!(
        s.instructions.is_empty(),
        "a proposal is not an instruction"
    );

    let batches = doc.pending_len();
    let instruction_id = apply(&mut doc, approve_params("op-a", &p, "Run the linter first"))
        .unwrap()
        .instruction_id
        .unwrap();
    assert_eq!(
        doc.pending_len(),
        batches + 1,
        "instruction + review in one batch"
    );
    assert_eq!(instruction_id, derive_id(&p, roles::INSTRUCTION));
    let s = snap(&doc);
    let instruction = s
        .instructions
        .iter()
        .find(|i| i.id == instruction_id)
        .unwrap();
    assert_eq!(
        instruction.text, "Run the linter first",
        "the reviewed text"
    );
    assert_eq!(instruction.work_item_id.as_deref(), Some(w.as_str()));
    assert_eq!(instruction.source_proposal_id.as_deref(), Some(p.as_str()));
    let source = instruction.source.as_ref().unwrap();
    assert_eq!(source.chat_id.as_deref(), Some("c1"));
    assert_eq!(source.message_id.as_deref(), Some("m-9"));
    let excerpt = "Always run the linter before committing";
    assert_eq!(source.excerpt.as_deref(), Some(excerpt));
    assert_eq!(
        source.content_hash.as_deref(),
        Some(format!("{:x}", Sha256::digest(excerpt.as_bytes())).as_str())
    );
    assert_eq!(s.effective_instructions(&w).entries.len(), 1);
    let proposal = s.instruction_proposal(&p).unwrap();
    assert_eq!(proposal.status, InstructionProposalStatus::Approved);
    assert_eq!(proposal.text, excerpt, "the proposal keeps its own text");

    // Approving again (any operation id) is a no-op returning the same id.
    let before = pending_ops(&doc);
    assert_eq!(
        apply(&mut doc, approve_params("op-a2", &p, "different"))
            .unwrap()
            .instruction_id
            .unwrap(),
        instruction_id
    );
    assert_eq!(pending_ops(&doc), before);
    // Rejecting an approved proposal is refused.
    assert_eq!(
        apply(&mut doc, reject_params("op-r", &p)).unwrap_err().code,
        WorkOutlineErrorCode::Invalid
    );
}

#[test]
fn rejection_is_durable_and_idempotent() {
    let mut doc = doc_with_spaces("dev-a");
    let p = apply(
        &mut doc,
        propose_params("op-p", InstructionProposalTarget::space_root(), Some("c1")),
    )
    .unwrap()
    .proposal_id
    .unwrap();
    assert_eq!(snap(&doc).instruction_proposal(&p).unwrap().space_id, SPACE);
    apply(&mut doc, reject_params("op-r", &p)).unwrap();
    let s = snap(&doc);
    let proposal = s.instruction_proposal(&p).unwrap();
    assert_eq!(proposal.status, InstructionProposalStatus::Rejected);
    assert_eq!(
        proposal.review.as_ref().unwrap().reason.as_deref(),
        Some("too broad")
    );
    let before = pending_ops(&doc);
    apply(&mut doc, reject_params("op-r2", &p)).unwrap();
    assert_eq!(pending_ops(&doc), before);
    // A later approval still wins (the user changed their mind).
    let id = apply(&mut doc, approve_params("op-a", &p, "Prefer small PRs"))
        .unwrap()
        .instruction_id
        .unwrap();
    let s = snap(&doc);
    assert_eq!(
        s.instruction_proposal(&p).unwrap().status,
        InstructionProposalStatus::Approved
    );
    let instruction = s.instructions.iter().find(|i| i.id == id).unwrap();
    assert!(instruction.work_item_id.is_none(), "Space-root instruction");
}

#[test]
fn proposal_validation_and_space_resolution() {
    let mut doc = doc_with_spaces("dev-a");
    let w = item(&mut doc, "op-w", None);
    let code = |doc: &mut RegistryDoc, m| apply(doc, m).unwrap_err().code;
    // Space root needs a Space: none from params or origin.
    assert_eq!(
        code(
            &mut doc,
            propose_params("a", InstructionProposalTarget::space_root(), None)
        ),
        WorkOutlineErrorCode::Invalid
    );
    // Origin in another Space than the target.
    assert_eq!(
        code(
            &mut doc,
            propose_params("b", InstructionProposalTarget::work_item(&w), Some("other"))
        ),
        WorkOutlineErrorCode::CrossSpace
    );
    assert_eq!(
        code(
            &mut doc,
            propose_params("c", InstructionProposalTarget::work_item("nope"), None)
        ),
        WorkOutlineErrorCode::NotFound
    );
    let WorkOutlineMutation::ProposeWorkItemInstruction(mut with_space) =
        propose_params("d", InstructionProposalTarget::space_root(), None)
    else {
        unreachable!()
    };
    with_space.space_id = Some(SPACE.into());
    let p = apply(
        &mut doc,
        WorkOutlineMutation::ProposeWorkItemInstruction(with_space.clone()),
    )
    .unwrap()
    .proposal_id
    .unwrap();
    // Replay, conflict.
    assert_eq!(
        apply(
            &mut doc,
            WorkOutlineMutation::ProposeWorkItemInstruction(with_space.clone())
        )
        .unwrap()
        .proposal_id
        .unwrap(),
        p
    );
    with_space.text = "other".into();
    assert_eq!(
        code(
            &mut doc,
            WorkOutlineMutation::ProposeWorkItemInstruction(with_space)
        ),
        WorkOutlineErrorCode::OperationConflict
    );
}

#[test]
fn approval_enforces_instruction_budgets() {
    let mut doc = doc_with_spaces("dev-a");
    let w = item(&mut doc, "op-w", None);
    // Fill the effective budget close to the limit with existing instructions.
    for i in 0..2 {
        apply(
            &mut doc,
            WorkOutlineMutation::CreateWorkItemInstruction(CreateWorkItemInstructionParams {
                space_id: SPACE.into(),
                scope: InstructionScope::WorkItem {
                    work_item_id: w.clone(),
                },
                text: "y".repeat(4_000),
                placement: None,
                source: None,
                operation_id: format!("i{i}"),
            }),
        )
        .unwrap();
    }
    let p = apply(
        &mut doc,
        propose_params("op-p", InstructionProposalTarget::work_item(&w), Some("c1")),
    )
    .unwrap()
    .proposal_id
    .unwrap();
    let before = pending_ops(&doc);
    let error = apply(&mut doc, approve_params("op-a", &p, &"z".repeat(1_500))).unwrap_err();
    assert_eq!(error.code, WorkOutlineErrorCode::LimitExceeded);
    assert!(
        error.message.starts_with("Approving this proposal"),
        "{}",
        error.message
    );
    assert_eq!(pending_ops(&doc), before, "nothing written");
    assert_eq!(
        snap(&doc).instruction_proposal(&p).unwrap().status,
        InstructionProposalStatus::Pending
    );
    // A shorter reviewed text fits.
    apply(&mut doc, approve_params("op-a2", &p, "short")).unwrap();
}

#[test]
fn approval_of_an_archived_target_fails_and_placement_is_honored() {
    let mut doc = doc_with_spaces("dev-a");
    let w = item(&mut doc, "op-w", None);
    let existing = apply(
        &mut doc,
        WorkOutlineMutation::CreateWorkItemInstruction(CreateWorkItemInstructionParams {
            space_id: SPACE.into(),
            scope: InstructionScope::WorkItem {
                work_item_id: w.clone(),
            },
            text: "existing".into(),
            placement: None,
            source: None,
            operation_id: "i1".into(),
        }),
    )
    .unwrap()
    .instruction_id
    .unwrap();
    let p = apply(
        &mut doc,
        propose_params("op-p", InstructionProposalTarget::work_item(&w), Some("c1")),
    )
    .unwrap()
    .proposal_id
    .unwrap();
    let id = apply(
        &mut doc,
        WorkOutlineMutation::ApproveWorkItemInstructionProposal(
            ApproveWorkItemInstructionProposalParams {
                proposal_id: p,
                approved_text: "first".into(),
                placement: Some(InstructionPlacement::First),
                operation_id: "op-a".into(),
            },
        ),
    )
    .unwrap()
    .instruction_id
    .unwrap();
    let entries: Vec<String> = snap(&doc)
        .effective_instructions(&w)
        .entries
        .iter()
        .map(|e| e.instruction_id.clone())
        .collect();
    assert_eq!(entries, vec![id, existing]);

    let p2 = apply(
        &mut doc,
        propose_params(
            "op-p2",
            InstructionProposalTarget::work_item(&w),
            Some("c1"),
        ),
    )
    .unwrap()
    .proposal_id
    .unwrap();
    apply(
        &mut doc,
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: w.clone(),
            descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
            operation_id: "arch".into(),
        }),
    )
    .unwrap();
    assert_eq!(
        apply(&mut doc, approve_params("op-a2", &p2, "x"))
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::Archived
    );
}

// ── Space deletion and the sweep ────────────────────────────────────────────

#[test]
fn space_deletion_and_sweep_cover_requests_and_proposals() {
    let (mut a, mut b, mut server, mut seq, w) = synced_pair();
    let r = request(&mut a, "r", &w, None);
    let p = apply(
        &mut a,
        propose_params("p", InstructionProposalTarget::work_item(&w), Some("c1")),
    )
    .unwrap()
    .proposal_id
    .unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    a.delete_space(SPACE).unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    for doc in [&a, &b] {
        assert_eq!(
            doc.row_state(KIND_USER_ACTION_REQUESTS, &r),
            RowState::Tombstoned
        );
        assert_eq!(
            doc.row_state(KIND_WORK_ITEM_INSTRUCTION_PROPOSALS, &p),
            RowState::Tombstoned
        );
        assert!(!snap(doc).available);
    }

    // A request created concurrently on a device that has not seen the
    // deletion is swept.
    let (mut a, mut b, mut server, mut seq, w) = synced_pair();
    b.upsert_chat(&space_chat("c1", Some(SPACE))).unwrap();
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    a.delete_space(SPACE).unwrap();
    let late = request(&mut b, "late", &w, None);
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    assert_eq!(
        a.row_state(KIND_USER_ACTION_REQUESTS, &late),
        RowState::Live
    );
    assert!(a.sweep_work_outline_orphans() >= 1);
    server_round(&mut server, &mut seq, &mut [&mut a, &mut b]);
    for doc in [&a, &b] {
        assert_eq!(
            doc.row_state(KIND_USER_ACTION_REQUESTS, &late),
            RowState::Tombstoned
        );
    }
}

// ── agent authorization matrix ──────────────────────────────────────────────

struct Fixture {
    doc: RegistryDoc,
    /// Parent of the agent's item.
    parent: String,
    /// The item Chat `c1` is assigned to.
    own: String,
    child: String,
    sibling: String,
    other_space_item: String,
    /// A request raised by `c2` on `sibling`.
    foreign_request: String,
}

fn fixture() -> Fixture {
    let mut doc = doc_with_spaces("dev-a");
    let parent = item(&mut doc, "op-parent", None);
    let own = item(&mut doc, "op-own", Some(&parent));
    let child = item(&mut doc, "op-child", Some(&own));
    let sibling = item(&mut doc, "op-sib", Some(&parent));
    let other_space_item = item_in(&mut doc, OTHER_SPACE, "op-other", None);
    assign(&mut doc, "as1", "c1", &own);
    let foreign_request = apply(&mut doc, request_params("fr", &sibling, "c2", None))
        .unwrap()
        .user_action_request_id
        .unwrap();
    Fixture {
        doc,
        parent,
        own,
        child,
        sibling,
        other_space_item,
        foreign_request,
    }
}

fn as_agent(f: &mut Fixture, mutation: WorkOutlineMutation) -> Outcome<WorkOutlineMutateReply> {
    apply_as(&mut f.doc, &agent("c1"), mutation)
}

fn denied(f: &mut Fixture, mutation: WorkOutlineMutation) {
    let name = mutation.op_name();
    let error = as_agent(f, mutation).unwrap_err();
    assert_eq!(
        error.code,
        WorkOutlineErrorCode::Unauthorized,
        "{name}: {error:?}"
    );
}

#[test]
fn agent_may_create_children_only_under_its_item() {
    let mut f = fixture();
    let (own, child, sibling, parent) = (
        f.own.clone(),
        f.child.clone(),
        f.sibling.clone(),
        f.parent.clone(),
    );
    let id = as_agent(&mut f, create_item_params("ac1", SPACE, Some(&own)))
        .unwrap()
        .work_item_id
        .unwrap();
    let created = snap(&f.doc).item(&id).unwrap().created_by.clone().unwrap();
    assert_eq!(created.kind, WorkOutlineActorKind::Agent);
    assert_eq!(
        created.chat_id.as_deref(),
        Some("c1"),
        "indicates the creating Chat"
    );
    as_agent(&mut f, create_item_params("ac2", SPACE, Some(&child))).unwrap();
    denied(&mut f, create_item_params("ac3", SPACE, Some(&sibling)));
    denied(&mut f, create_item_params("ac4", SPACE, Some(&parent)));
    denied(&mut f, create_item_params("ac5", SPACE, None));
    let other = f.other_space_item.clone();
    denied(&mut f, create_item_params("ac6", OTHER_SPACE, Some(&other)));
}

#[test]
fn agent_may_associate_only_its_own_chat_within_its_space() {
    let mut f = fixture();
    let (own, child, sibling, parent, other) = (
        f.own.clone(),
        f.child.clone(),
        f.sibling.clone(),
        f.parent.clone(),
        f.other_space_item.clone(),
    );
    // Associated: never wider than its current item's subtree.
    denied(&mut f, assign_params("as2", "c1", &sibling));
    denied(&mut f, assign_params("as2p", "c1", &parent));
    as_agent(&mut f, assign_params("as2c", "c1", &child)).unwrap();
    // Now under `child`: `own` is outside the new subtree.
    denied(&mut f, assign_params("as2o", "c1", &own));
    denied(&mut f, assign_params("as3", "c2", &sibling));
    denied(&mut f, assign_params("as4", "c1", &other));
    // Unassociated: any item of its Space.
    apply_as(
        &mut f.doc,
        &agent("c2"),
        assign_params("as5", "c2", &sibling),
    )
    .unwrap();
    denied(
        &mut f,
        WorkOutlineMutation::RemoveChatFromWorkItem(RemoveChatFromWorkItemParams {
            chat_id: "c1".into(),
            operation_id: "rm".into(),
        }),
    );
}

#[test]
fn agent_of_an_execution_bound_chat_cannot_reassociate() {
    let mut f = fixture();
    let (own, child) = (f.own.clone(), f.child.clone());
    let hlc = f.doc.mint_hlc();
    f.doc
        .write_ops(vec![crate::registry::RowOp {
            kind: KIND_CHAT_EXECUTION_BINDINGS.into(),
            id: "c1".into(),
            op: crate::registry::OpKind::Upsert,
            set: Some(BTreeMap::from([("spaceId".to_owned(), json!(SPACE))])),
            hlc,
            clocks: None,
        }])
        .unwrap();
    denied(&mut f, assign_params("ab1", "c1", &child));
    // Re-associating with the item it was started for changes nothing.
    as_agent(&mut f, assign_params("ab2", "c1", &own)).unwrap();
}

#[test]
fn agent_resolve_closes_only_its_own_members_of_a_dedupe_group() {
    let mut f = fixture();
    let own = f.own.clone();
    assign(&mut f.doc, "as-c2", "c2", &own);
    let mine = as_agent(&mut f, request_params("g1", &own, "c1", Some("deploy")))
        .unwrap()
        .user_action_request_id
        .unwrap();
    let theirs = apply_as(
        &mut f.doc,
        &agent("c2"),
        request_params("g2", &own, "c2", Some("deploy")),
    )
    .unwrap()
    .user_action_request_id
    .unwrap();
    let mine_again = as_agent(&mut f, request_params("g3", &own, "c1", Some("deploy")))
        .unwrap()
        .user_action_request_id
        .unwrap();
    as_agent(&mut f, resolve_params("gx", &mine, None)).unwrap();
    let s = snap(&f.doc);
    assert!(!s.user_action_request(&mine).unwrap().is_unresolved());
    assert!(!s.user_action_request(&mine_again).unwrap().is_unresolved());
    let other = s.user_action_request(&theirs).unwrap();
    assert!(other.is_unresolved(), "another Chat's request stays open");
    assert!(!other.deduped, "it becomes the canonical request");
    assert_eq!(s.item(&own).unwrap().rollup.unresolved_user_action_count, 1);
    // The user's resolve still closes the whole (remaining) group.
    apply(&mut f.doc, resolve_params("gu", &theirs, None)).unwrap();
    assert!(
        !snap(&f.doc)
            .user_action_request(&theirs)
            .unwrap()
            .is_unresolved()
    );
}

#[test]
fn agent_may_set_active_or_completed_on_its_subtree_and_never_auto_resolves() {
    let mut f = fixture();
    let (own, child, sibling) = (f.own.clone(), f.child.clone(), f.sibling.clone());
    as_agent(
        &mut f,
        status_params("u1", &own, WorkItemStatus::Active, false),
    )
    .unwrap();
    as_agent(
        &mut f,
        status_params("u2", &child, WorkItemStatus::Completed, false),
    )
    .unwrap();
    denied(
        &mut f,
        status_params("u3", &own, WorkItemStatus::Pending, false),
    );
    denied(
        &mut f,
        status_params("u4", &sibling, WorkItemStatus::Active, false),
    );
    denied(
        &mut f,
        WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: own.clone(),
            changes: WorkItemChanges {
                title: Some("renamed".into()),
                status: Some(WorkItemStatus::Active),
                ..Default::default()
            },
            operation_id: "u5".into(),
        }),
    );
    // Completing with open requests: an error for agents, never resolve-all.
    as_agent(&mut f, request_params("rq", &own, "c1", None)).unwrap();
    assert_eq!(
        as_agent(
            &mut f,
            status_params("u6", &own, WorkItemStatus::Completed, false)
        )
        .unwrap_err()
        .code,
        WorkOutlineErrorCode::OpenUserActionRequests
    );
    denied(
        &mut f,
        status_params("u7", &own, WorkItemStatus::Completed, true),
    );
    assert!(
        snap(&f.doc)
            .item(&own)
            .unwrap()
            .rollup
            .unresolved_user_action_count
            == 1
    );
}

#[test]
fn agent_requests_are_bound_to_its_chat_and_item() {
    let mut f = fixture();
    let (own, sibling, foreign) = (f.own.clone(), f.sibling.clone(), f.foreign_request.clone());
    let id = as_agent(&mut f, request_params("r1", &own, "c1", None))
        .unwrap()
        .user_action_request_id
        .unwrap();
    assert_eq!(
        snap(&f.doc)
            .user_action_request(&id)
            .unwrap()
            .created_by
            .as_ref()
            .unwrap()
            .kind,
        WorkOutlineActorKind::Agent
    );
    denied(&mut f, request_params("r2", &own, "c2", None));
    denied(&mut f, request_params("r3", &sibling, "c1", None));
    as_agent(&mut f, resolve_params("x1", &id, Some("done"))).unwrap();
    denied(&mut f, resolve_params("x2", &foreign, None));
    assert_eq!(
        as_agent(&mut f, resolve_params("x3", "missing", None))
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::NotFound
    );
}

#[test]
fn agent_proposals_target_its_item_parent_or_space_root() {
    let mut f = fixture();
    let (own, parent, sibling, child) = (
        f.own.clone(),
        f.parent.clone(),
        f.sibling.clone(),
        f.child.clone(),
    );
    for (op, target) in [
        ("p1", InstructionProposalTarget::work_item(&own)),
        ("p2", InstructionProposalTarget::work_item(&parent)),
        ("p3", InstructionProposalTarget::space_root()),
    ] {
        // No origin Chat: the agent's Chat is recorded.
        let id = as_agent(&mut f, propose_params(op, target, None))
            .unwrap()
            .proposal_id
            .unwrap();
        let proposal = snap(&f.doc).instruction_proposal(&id).unwrap().clone();
        assert_eq!(proposal.source_ref.chat_id.as_deref(), Some("c1"));
        assert_eq!(proposal.space_id, SPACE);
    }
    denied(
        &mut f,
        propose_params("p4", InstructionProposalTarget::work_item(&sibling), None),
    );
    denied(
        &mut f,
        propose_params("p5", InstructionProposalTarget::work_item(&child), None),
    );
    denied(
        &mut f,
        propose_params("p6", InstructionProposalTarget::work_item(&own), Some("c2")),
    );
    let WorkOutlineMutation::ProposeWorkItemInstruction(mut foreign_root) =
        propose_params("p7", InstructionProposalTarget::space_root(), None)
    else {
        unreachable!()
    };
    foreign_root.space_id = Some(OTHER_SPACE.into());
    denied(
        &mut f,
        WorkOutlineMutation::ProposeWorkItemInstruction(foreign_root),
    );
}

#[test]
fn agent_is_denied_every_other_operation() {
    let mut f = fixture();
    let (own, child) = (f.own.clone(), f.child.clone());
    let proposal = apply(
        &mut f.doc,
        propose_params("up", InstructionProposalTarget::work_item(&own), Some("c1")),
    )
    .unwrap()
    .proposal_id
    .unwrap();
    let instruction = apply(
        &mut f.doc,
        WorkOutlineMutation::CreateWorkItemInstruction(CreateWorkItemInstructionParams {
            space_id: SPACE.into(),
            scope: InstructionScope::WorkItem {
                work_item_id: own.clone(),
            },
            text: "user instruction".into(),
            placement: None,
            source: None,
            operation_id: "ui".into(),
        }),
    )
    .unwrap()
    .instruction_id
    .unwrap();
    let denied_ops = vec![
        WorkOutlineMutation::MoveWorkItem(MoveWorkItemParams {
            work_item_id: child.clone(),
            destination_parent_work_item_id: None,
            placement: WorkOutlinePlacement::Last,
            operation_id: "d1".into(),
        }),
        WorkOutlineMutation::ArchiveWorkItem(ArchiveWorkItemParams {
            work_item_id: child.clone(),
            descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
            operation_id: "d2".into(),
        }),
        WorkOutlineMutation::RestoreWorkItem(RestoreWorkItemParams {
            work_item_id: child.clone(),
            operation_id: "d3".into(),
        }),
        WorkOutlineMutation::CreateWorkItemExternalReference(
            CreateWorkItemExternalReferenceParams {
                work_item_id: own.clone(),
                provider: "github".into(),
                reference_type: "issue".into(),
                external_id: "1".into(),
                url: "https://example.com/1".into(),
                cached_title: None,
                operation_id: "d4".into(),
            },
        ),
        WorkOutlineMutation::RepairWorkOutline(RepairWorkOutlineParams {
            space_id: SPACE.into(),
            operation_id: "d5".into(),
        }),
        WorkOutlineMutation::CreateWorkItemInstruction(CreateWorkItemInstructionParams {
            space_id: SPACE.into(),
            scope: InstructionScope::WorkItem {
                work_item_id: own.clone(),
            },
            text: "sneaky".into(),
            placement: None,
            source: None,
            operation_id: "d6".into(),
        }),
        WorkOutlineMutation::UpdateWorkItemInstruction(UpdateWorkItemInstructionParams {
            instruction_id: instruction.clone(),
            text: "changed".into(),
            operation_id: "d7".into(),
        }),
        WorkOutlineMutation::ArchiveWorkItemInstruction(ArchiveWorkItemInstructionParams {
            instruction_id: instruction.clone(),
            operation_id: "d8".into(),
        }),
        WorkOutlineMutation::CreateInstructionRevision(CreateInstructionRevisionParams {
            work_item_id: own.clone(),
            operation_id: "d9".into(),
        }),
        WorkOutlineMutation::ConfigureExecution(ConfigureExecutionParams {
            scope: ExecutionScope::WorkItem {
                work_item_id: own.clone(),
            },
            configuration: None,
            operation_id: "d10".into(),
        }),
        approve_params("d11", &proposal, "self-approve"),
        reject_params("d12", &proposal),
    ];
    let before = pending_ops(&f.doc);
    for mutation in denied_ops {
        denied(&mut f, mutation);
    }
    assert_eq!(pending_ops(&f.doc), before, "denials write nothing");
    // The same operations still work for the user.
    apply(&mut f.doc, approve_params("ok", &proposal, "approved")).unwrap();
}

#[test]
fn agent_without_assignment_or_project_is_narrowed_further() {
    let mut f = fixture();
    let own = f.own.clone();
    // c2 has no assignment: it may only associate itself, or propose for
    // the Space root.
    let as_c2 = |f: &mut Fixture, m| apply_as(&mut f.doc, &agent("c2"), m);
    assert_eq!(
        as_c2(&mut f, create_item_params("n1", SPACE, Some(&own)))
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::Unauthorized
    );
    assert_eq!(
        as_c2(
            &mut f,
            propose_params("n2", InstructionProposalTarget::work_item(&own), None)
        )
        .unwrap_err()
        .code,
        WorkOutlineErrorCode::Unauthorized
    );
    as_c2(
        &mut f,
        propose_params("n3", InstructionProposalTarget::space_root(), None),
    )
    .unwrap();
    as_c2(&mut f, assign_params("n4", "c2", &own)).unwrap();
    // Now assigned, it may create under its item.
    as_c2(&mut f, create_item_params("n5", SPACE, Some(&own))).unwrap();

    f.doc.upsert_chat(&space_chat("np", None)).unwrap();
    let error = apply_as(
        &mut f.doc,
        &agent("np"),
        propose_params("n6", InstructionProposalTarget::space_root(), None),
    )
    .unwrap_err();
    assert_eq!(error.code, WorkOutlineErrorCode::Unauthorized);
    let missing = apply_as(
        &mut f.doc,
        &agent("ghost"),
        assign_params("n7", "ghost", &own),
    )
    .unwrap_err();
    assert_eq!(missing.code, WorkOutlineErrorCode::Unauthorized);
    let no_chat = WorkOutlineActor {
        chat_id: None,
        ..agent("c1")
    };
    assert_eq!(
        apply_as(&mut f.doc, &no_chat, assign_params("n8", "c1", &own))
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::Unauthorized
    );
}
