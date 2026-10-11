// Milestone 4 user interface tests (Required Actions, completion with open
// requests, rollups, proposals). Included from `overview.rs`'s test module
// so they share its Overview fixtures.
use super::*;

use crate::work_outline::required_actions::fixture as m4;
use crate::work_outline::store::{WorkItemEffectiveStatus, WorkItemStatus};

fn m4_overview<'a>(
    cx: &'a mut gpui::TestAppContext,
    dir: &std::path::Path,
    snapshot: WorkOutlineSnapshot,
    reply: impl Fn(&serde_json::Value) -> Result<serde_json::Value, zeron_rpc::RpcError>
    + Send
    + Sync
    + 'static,
) -> (
    Entity<Overview>,
    OutlineCalls,
    &'a mut gpui::VisualTestContext,
) {
    instruction_overview(cx, dir, snapshot, reply)
}

/// The Overview's chat panel closes itself when no Chat is selected (no
/// Shell in these tests), so select the Chat an Open is expected to show.
fn select_chat(overview: &Entity<Overview>, vcx: &mut gpui::VisualTestContext, chat_id: &str) {
    let state = overview.read_with(vcx, |o, _| o.state.clone());
    state.update(vcx, |state, _| state.selected_chat = Some(chat_id.into()));
}

fn click(vcx: &mut gpui::VisualTestContext, selector: String) {
    let at = row_bounds(vcx, selector.clone())
        .unwrap_or_else(|| panic!("{selector} renders"))
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    draw(vcx);
}

fn open_global(
    overview: &Entity<Overview>,
    vcx: &mut gpui::VisualTestContext,
) -> Entity<crate::work_outline::required_actions_view::RequiredActionsList> {
    vcx.update(|window, cx| {
        overview.update(cx, |o, cx| {
            o.run_work_outline_command(WorkOutlineCommand::OpenRequiredActions, window, cx)
        })
    });
    draw(vcx);
    overview.read_with(vcx, |o, _| o.required_actions_panel().unwrap())
}

/// Global Required Actions: rows (duplicates folded), a missing origin Chat
/// stays inspectable but cannot be opened, Resolve sends the note, history
/// is collapsed, Open lands in the origin Chat.
#[gpui::test]
fn global_required_actions_list_resolve_open_and_missing_chat(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = m4_overview(cx, dir.path(), m4::snapshot("s1"), reply_ok);
    let list = open_global(&overview, vcx);
    assert!(vcx.debug_bounds("required-actions-panel").is_some());
    assert!(row_bounds(vcx, format!("required-actions-row-{}", m4::R_UI)).is_some());
    assert!(row_bounds(vcx, format!("required-actions-row-{}", m4::R_API)).is_some());
    assert!(
        row_bounds(vcx, format!("required-actions-row-{}", m4::R_UI_DUP)).is_none(),
        "the duplicate folds into its canonical request"
    );
    let rows = list.read_with(vcx, |list, _| list.rows());
    assert_eq!(rows.open.len(), 2);
    let missing = rows
        .open
        .iter()
        .find(|r| r.request_id == m4::R_API)
        .unwrap();
    assert_eq!(missing.chat_label, "Chat unavailable");
    assert_eq!(missing.path, "Launch v2 \u{203a} API");

    // Open on a missing Chat does nothing; the row stays.
    click(vcx, format!("required-actions-open-{}", m4::R_API));
    overview.read_with(vcx, |o, _| {
        assert!(o.required_actions_panel.is_some());
        assert!(!o.chat_panel_open);
    });

    // History is collapsed until opened.
    assert!(row_bounds(vcx, format!("required-actions-history-row-{}", m4::R_AUTH)).is_none());
    click(vcx, "required-actions-history-toggle".into());
    assert!(row_bounds(vcx, format!("required-actions-history-row-{}", m4::R_AUTH)).is_some());

    // Resolve with a note.
    click(vcx, format!("required-actions-resolve-{}", m4::R_UI));
    assert!(vcx.debug_bounds("required-actions-note").is_some());
    list.update(vcx, |list, cx| list.set_note("Picked palette B", cx));
    click(vcx, "required-actions-resolve-submit".into());
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["op"], "resolveUserActionRequest");
    assert_eq!(recorded[0]["userActionRequestId"], m4::R_UI);
    assert_eq!(recorded[0]["note"], "Picked palette B");
    assert!(
        recorded[0]["operationId"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    list.read_with(vcx, |list, _| assert!(list.resolving.is_none()));

    // Resolve without a note omits it.
    list.update(vcx, |list, cx| list.resolve(m4::R_API.into(), None, cx));
    vcx.run_until_parked();
    assert!(calls.lock().unwrap()[1].get("note").is_none());

    // Open on an available Chat closes the surface and opens the Chat.
    select_chat(&overview, vcx, CHAT_UI);
    click(vcx, format!("required-actions-open-{}", m4::R_UI));
    overview.read_with(vcx, |o, _| {
        assert!(o.required_actions_panel.is_none());
        assert!(o.chat_panel_open);
    });
}

/// A failed resolve keeps the row and reuses the operation id on retry.
#[gpui::test]
fn failed_resolve_retries_with_the_same_operation_id(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let flag = fail.clone();
    let (overview, calls, vcx) = m4_overview(cx, dir.path(), m4::snapshot("s1"), move |_| {
        if flag.swap(false, std::sync::atomic::Ordering::SeqCst) {
            Err(zeron_rpc::RpcError::Failed(
                "work-outline/invalid: try again".into(),
            ))
        } else {
            Ok(serde_json::json!({}))
        }
    });
    let list = open_global(&overview, vcx);
    list.update(vcx, |list, cx| {
        list.resolve(m4::R_UI.into(), Some("ok".into()), cx)
    });
    vcx.run_until_parked();
    list.read_with(vcx, |list, _| {
        assert_eq!(list.error.as_deref(), Some("try again"))
    });
    list.update(vcx, |list, cx| {
        list.resolve(m4::R_UI.into(), Some("ok".into()), cx)
    });
    vcx.run_until_parked();
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0]["operationId"], recorded[1]["operationId"]);
}

/// Space-level views: item details list the item's subtree, the Space-root
/// details list the whole Space; Open from details goes to the Chat.
#[gpui::test]
fn space_level_required_actions_in_details(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) = m4_overview(cx, dir.path(), m4::snapshot("s1"), reply_ok);
    let details = open_details(&overview, vcx, API);
    assert!(vcx.debug_bounds("work-item-required-actions").is_some());
    let rows = details.read_with(vcx, |d, cx| d.required_actions_list().read(cx).rows());
    let open: Vec<_> = rows.open.iter().map(|r| r.request_id.as_str()).collect();
    assert_eq!(open, [m4::R_API]);
    assert_eq!(rows.resolved.len(), 1, "Auth endpoints sits under API");
    // Open by default while something is open.
    assert!(row_bounds(vcx, format!("required-actions-row-{}", m4::R_API)).is_some());

    // A leaf without requests: the section stays collapsed with a zero count.
    let details = open_details(&overview, vcx, HOUSEKEEPING);
    assert!(details.read_with(vcx, |d, cx| {
        d.required_actions_list().read(cx).rows().open.is_empty()
    }));
    assert!(vcx.debug_bounds("required-actions-list").is_none());

    // Space root: every request of the Space.
    let details = open_details(
        &overview,
        vcx,
        crate::work_outline::details_panel::SPACE_ROOT_ITEM_ID,
    );
    let rows = details.read_with(vcx, |d, cx| d.required_actions_list().read(cx).rows());
    assert_eq!(rows.open.len(), 2);
    select_chat(&overview, vcx, CHAT_UI);
    click(vcx, format!("required-actions-open-{}", m4::R_UI));
    overview.read_with(vcx, |o, _| {
        assert!(o.work_item_details.is_none());
        assert!(o.chat_panel_open);
    });
}

fn completion_reply(params: &serde_json::Value) -> Result<serde_json::Value, zeron_rpc::RpcError> {
    // The host refuses to complete Housekeeping: a request arrived after the
    // click (the local snapshot still shows none).
    if params["op"] == "updateWorkItem"
        && params["workItemId"] == HOUSEKEEPING
        && params["changes"].get("resolveOpenRequests").is_none()
    {
        return Err(zeron_rpc::RpcError::Failed(
            zeron_proto::WorkOutlineError::open_user_action_requests(2).to_wire_message(),
        ));
    }
    Ok(serde_json::json!({}))
}

/// Completing an item with open requests asks first, then sends
/// `resolveOpenRequests`; without requests it completes directly; a host
/// refusal (`openUserActionRequests`) asks with the host's count.
#[gpui::test]
fn completing_with_open_requests_confirms_and_resolves(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = m4_overview(cx, dir.path(), m4::snapshot("s1"), completion_reply);
    let details = open_details(&overview, vcx, UI);
    details.update(vcx, |d, cx| d.set_status(WorkItemStatus::Completed, cx));
    let request = details
        .update(vcx, |d, _| d.captured_confirm.take())
        .expect("completion asks");
    assert!(request.body.contains("1 open user action request"));
    assert_eq!(request.confirm_label.as_ref(), "Complete and resolve");
    vcx.update(|window, cx| (request.on_decision)(false, window, cx));
    vcx.run_until_parked();
    assert!(calls.lock().unwrap().is_empty(), "Cancel writes nothing");

    details.update(vcx, |d, cx| d.set_status(WorkItemStatus::Completed, cx));
    let request = details
        .update(vcx, |d, _| d.captured_confirm.take())
        .unwrap();
    vcx.update(|window, cx| (request.on_decision)(true, window, cx));
    vcx.run_until_parked();
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["op"], "updateWorkItem");
    assert_eq!(recorded[0]["changes"]["status"], "completed");
    assert_eq!(recorded[0]["changes"]["resolveOpenRequests"], true);

    // Other statuses are never gated.
    details.update(vcx, |d, cx| d.set_status(WorkItemStatus::Pending, cx));
    vcx.run_until_parked();
    assert!(details.read_with(vcx, |d, _| d.captured_confirm.is_none()));
    assert!(
        calls.lock().unwrap()[1]["changes"]
            .get("resolveOpenRequests")
            .is_none()
    );

    // No local requests: completes directly; the host refusal then asks.
    calls.lock().unwrap().clear();
    let details = open_details(&overview, vcx, HOUSEKEEPING);
    details.update(vcx, |d, cx| d.set_status(WorkItemStatus::Completed, cx));
    vcx.run_until_parked();
    assert_eq!(calls.lock().unwrap().len(), 1);
    let request = details
        .update(vcx, |d, _| d.captured_confirm.take())
        .expect("host refusal asks");
    assert!(request.body.contains("2 open user action requests"));
    vcx.update(|window, cx| (request.on_decision)(true, window, cx));
    vcx.run_until_parked();
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[1]["changes"]["resolveOpenRequests"], true);
}

/// Outline rows show the descendant totals and drop the attention total
/// (revealing the base status) once the last request resolves.
#[gpui::test]
fn rollup_totals_render_and_clear_when_the_last_request_resolves(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) = m4_overview(cx, dir.path(), m4::snapshot("s1"), reply_ok);
    let totals = |key: String| format!("outline-totals-{key}");
    assert!(row_bounds(vcx, totals(row_key::item("s1", LAUNCH))).is_some());
    assert!(
        row_bounds(vcx, totals(row_key::item("s1", UI))).is_some(),
        "blocked leaf"
    );
    assert!(row_bounds(vcx, totals(row_key::item("s1", HOUSEKEEPING))).is_none());
    let model_totals = overview.read_with(vcx, |o, cx| {
        let row = o
            .outline
            .read(cx)
            .model()
            .row(&row_key::item("s1", LAUNCH))
            .unwrap()
            .item()
            .cloned()
            .unwrap();
        crate::work_outline::required_actions::status_totals(&row.rollup).map(|t| t.label())
    });
    assert_eq!(
        model_totals.as_deref(),
        Some("3 active \u{b7} 1 user action required")
    );

    // The last request on UI resolves: effective status falls back to Active.
    let mut resolved = m4::snapshot("s1");
    for request in &mut resolved.user_action_requests {
        if request.work_item_id == UI {
            m4::resolve(request, m4::NOW + 20, None);
        }
    }
    for item in &mut resolved.items {
        if item.id == UI {
            item.effective_status = WorkItemEffectiveStatus::Active;
            item.rollup.unresolved_user_action_count = 0;
            item.rollup.user_action_required_count = 0;
        }
        if item.id == LAUNCH {
            item.rollup.user_action_required_count = 0;
        }
    }
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(resolved)), cx)
    });
    draw(vcx);
    assert!(row_bounds(vcx, totals(row_key::item("s1", UI))).is_none());
    let ui = overview.read_with(vcx, |o, cx| {
        o.outline
            .read(cx)
            .model()
            .row(&row_key::item("s1", UI))
            .unwrap()
            .item()
            .cloned()
            .unwrap()
    });
    assert_eq!(ui.effective_status, WorkItemEffectiveStatus::Active);
    assert!(!ui.needs_user_action());
}

fn proposals_snapshot() -> WorkOutlineSnapshot {
    let mut snapshot = m4::snapshot("s1");
    snapshot.instruction_proposals.push(m4::proposal(
        "prop-house",
        "s1",
        Some(HOUSEKEEPING),
        CHAT_API,
        "Sweep stale branches weekly.",
    ));
    snapshot
}

fn proposal_reply(params: &serde_json::Value) -> Result<serde_json::Value, zeron_rpc::RpcError> {
    Ok(match params["op"].as_str() {
        Some("approveWorkItemInstructionProposal") => serde_json::json!({"instructionId": "i-p"}),
        _ => serde_json::json!({}),
    })
}

/// The nonmodal indicator opens the review; Approve sends the edited text,
/// through the drift confirmation when assigned Chats are affected.
#[gpui::test]
fn proposals_approve_with_edits_and_drift_confirmation(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = m4_overview(cx, dir.path(), proposals_snapshot(), proposal_reply);
    // Compact indicators on the item and Space rows; nothing opens by itself.
    assert!(
        row_bounds(
            vcx,
            format!("outline-proposals-{}", row_key::item("s1", UI))
        )
        .is_some()
    );
    assert!(row_bounds(vcx, format!("outline-proposals-{}", row_key::space("s1"))).is_some());
    assert!(overview.read_with(vcx, |o, _| o.work_item_details.is_none()));
    click(
        vcx,
        format!("outline-proposals-{}", row_key::item("s1", UI)),
    );
    let details = overview.read_with(vcx, |o, _| o.work_item_details.clone().unwrap());
    details.read_with(vcx, |d, _| {
        assert_eq!(d.item_id, UI);
        assert!(d.proposals_open);
    });
    assert!(row_bounds(vcx, format!("work-item-proposal-{}", m4::P_UI)).is_some());

    // Review: editable text, target, reach.
    click(vcx, format!("work-item-proposal-review-{}", m4::P_UI));
    assert!(vcx.debug_bounds("work-item-proposal-target").is_some());
    details.update(vcx, |d, cx| {
        d.set_proposal_text("Use the design tokens.", cx)
    });
    // An edited review counts as an unsaved draft.
    assert!(details.read_with(vcx, |d, cx| d.has_unsaved_draft(cx)));
    click(vcx, "work-item-proposal-approve".into());
    let request = details
        .update(vcx, |d, _| d.captured_drift.take())
        .expect("chat-ui is assigned to UI: drift confirmation");
    assert_eq!(request.impact.chat_ids, [CHAT_UI.to_string()]);
    vcx.update(|window, cx| (request.on_decision)(false, window, cx));
    vcx.run_until_parked();
    assert!(calls.lock().unwrap().is_empty());
    details.update(vcx, |d, cx| d.approve_proposal(cx));
    let request = details.update(vcx, |d, _| d.captured_drift.take()).unwrap();
    vcx.update(|window, cx| (request.on_decision)(true, window, cx));
    vcx.run_until_parked();
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["op"], "approveWorkItemInstructionProposal");
    assert_eq!(recorded[0]["proposalId"], m4::P_UI);
    assert_eq!(recorded[0]["approvedText"], "Use the design tokens.");
    details.read_with(vcx, |d, _| assert!(d.proposal_review.is_none()));

    // No assigned Chat below Housekeeping: approves without asking.
    calls.lock().unwrap().clear();
    overview.update(vcx, |o, cx| {
        o.open_proposals("s1".into(), HOUSEKEEPING.into(), cx)
    });
    vcx.run_until_parked();
    draw(vcx);
    let details = overview.read_with(vcx, |o, _| o.work_item_details.clone().unwrap());
    vcx.update(|window, cx| {
        details.update(cx, |d, cx| {
            d.begin_proposal_review("prop-house".into(), window, cx)
        })
    });
    details.update(vcx, |d, cx| d.approve_proposal(cx));
    vcx.run_until_parked();
    assert!(details.read_with(vcx, |d, _| d.captured_drift.is_none()));
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["approvedText"], "Sweep stale branches weekly.");
}

/// Reject takes an optional reason; reviewed outcomes stay in a collapsed
/// history.
#[gpui::test]
fn proposals_reject_with_reason_and_show_history(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = m4_overview(cx, dir.path(), proposals_snapshot(), proposal_reply);
    overview.update(vcx, |o, cx| {
        o.open_proposals(
            "s1".into(),
            crate::work_outline::details_panel::SPACE_ROOT_ITEM_ID.into(),
            cx,
        )
    });
    vcx.run_until_parked();
    draw(vcx);
    let details = overview.read_with(vcx, |o, _| o.work_item_details.clone().unwrap());
    click(vcx, format!("work-item-proposal-review-{}", m4::P_ROOT));
    click(vcx, "work-item-proposal-reject".into());
    assert!(vcx.debug_bounds("work-item-proposal-reason").is_some());
    assert!(
        calls.lock().unwrap().is_empty(),
        "the first click only asks for a reason"
    );
    details.update(vcx, |d, cx| d.set_reject_reason("Linters run in CI", cx));
    click(vcx, "work-item-proposal-reject".into());
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["op"], "rejectWorkItemInstructionProposal");
    assert_eq!(recorded[0]["proposalId"], m4::P_ROOT);
    assert_eq!(recorded[0]["reason"], "Linters run in CI");

    // API carries a rejected proposal: listed in the collapsed history.
    overview.update(vcx, |o, cx| o.open_proposals("s1".into(), API.into(), cx));
    vcx.run_until_parked();
    draw(vcx);
    assert!(row_bounds(vcx, format!("work-item-proposal-history-{}", m4::P_API)).is_none());
    click(vcx, "work-item-proposals-history-toggle".into());
    assert!(row_bounds(vcx, format!("work-item-proposal-history-{}", m4::P_API)).is_some());
}

/// Agent-created items name the Chat that created them.
#[gpui::test]
fn agent_created_items_show_their_chat(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mut snapshot = m4::snapshot("s1");
    for item in &mut snapshot.items {
        if item.id == UI {
            item.created_by = Some(m4::agent(CHAT_UI));
        }
    }
    let (overview, _calls, vcx) = m4_overview(cx, dir.path(), snapshot, reply_ok);
    open_details(&overview, vcx, API);
    assert!(
        vcx.debug_bounds("work-item-created-by").is_none(),
        "user-created"
    );
    open_details(&overview, vcx, UI);
    assert!(vcx.debug_bounds("work-item-created-by").is_some());
    select_chat(&overview, vcx, CHAT_UI);
    click(vcx, "work-item-created-by-chat".into());
    overview.read_with(vcx, |o, _| {
        assert!(o.work_item_details.is_none());
        assert!(o.chat_panel_open);
    });
}
