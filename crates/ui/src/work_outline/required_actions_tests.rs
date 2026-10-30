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
    reply: impl Fn(&serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError>
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
            Err(mast_rpc::RpcError::Failed(
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

fn completion_reply(params: &serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError> {
    // The host refuses to complete Housekeeping: a request arrived after the
    // click (the local snapshot still shows none).
    if params["op"] == "updateWorkItem"
        && params["workItemId"] == HOUSEKEEPING
        && params["changes"].get("resolveOpenRequests").is_none()
    {
        return Err(mast_rpc::RpcError::Failed(
            mast_proto::WorkOutlineError::open_user_action_requests(2).to_wire_message(),
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

fn proposal_reply(params: &serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError> {
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

// --------------------------------------------------------------------------
// Reply & resolve
// --------------------------------------------------------------------------

fn is_reply_send(params: &serde_json::Value) -> bool {
    params.get("prompt").is_some() || params.get("holdForTurnEnd").is_some()
}

fn is_resolve(params: &serde_json::Value) -> bool {
    params["op"] == "resolveUserActionRequest"
}

fn sends(calls: &OutlineCalls) -> Vec<serde_json::Value> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|p| is_reply_send(p))
        .cloned()
        .collect()
}

fn resolves(calls: &OutlineCalls) -> Vec<serde_json::Value> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|p| is_resolve(p))
        .cloned()
        .collect()
}

fn turn_reply(state: &str, detail: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "chatId": CHAT_UI,
        "messageId": "m-reply",
        "commandId": "c-reply",
        "state": state,
        "detail": detail,
    })
}

/// The host accepts the reply Turn and the resolve.
fn reply_started(params: &serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError> {
    if params.get("prompt").is_some() {
        Ok(turn_reply("started", None))
    } else if params.get("holdForTurnEnd").is_some() {
        Ok(serde_json::json!({"id": "q-reply"}))
    } else {
        reply_ok(params)
    }
}

const PALETTE_REPLY: &str = "Re: \"Approve the color palette\": Picked palette B";

/// Reply & resolve: the row's primary action opens the note; Reply stays
/// disabled until a note is typed; Enter sends exactly one message with the
/// formatted text to the origin Chat, then resolves with the same note under
/// its own derived operation id; the answered row offers Open chat.
#[gpui::test]
fn reply_and_resolve_sends_one_message_then_resolves(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = m4_overview(cx, dir.path(), m4::snapshot("s1"), reply_started);
    let list = open_global(&overview, vcx);
    click(vcx, format!("required-actions-reply-{}", m4::R_UI));
    assert!(vcx.debug_bounds("required-actions-note").is_some());

    // Empty note: Reply & resolve is disabled (click and direct call alike).
    click(vcx, "required-actions-reply-submit".into());
    list.update(vcx, |list, cx| list.submit_reply(cx));
    vcx.run_until_parked();
    assert!(calls.lock().unwrap().is_empty());

    // Type the note and press Enter.
    let input = list.read_with(vcx, |list, _| list.note_input());
    let focus = input.read_with(vcx, |input, cx| gpui::Focusable::focus_handle(input, cx));
    vcx.update(|window, cx| {
        crate::composer::init(cx, crate::settings::ComposerSendBehavior::default());
        window.activate_window();
        window.focus(&focus, cx);
    });
    draw(vcx);
    vcx.simulate_input("Picked palette B");
    draw(vcx);
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    draw(vcx);

    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.len(), 2, "one send, one resolve: {recorded:?}");
    let send = &recorded[0];
    assert_eq!(send["chatId"], CHAT_UI);
    assert_eq!(send["prompt"]["text"], PALETTE_REPLY);
    let reply_op = send["operationId"].as_str().unwrap().to_owned();
    let resolve = &recorded[1];
    assert!(is_resolve(resolve));
    assert_eq!(resolve["userActionRequestId"], m4::R_UI);
    assert_eq!(resolve["note"], "Picked palette B");
    assert_eq!(
        resolve["operationId"],
        mast_proto::work_outline::derive_id(
            &reply_op,
            crate::work_outline::required_actions::REPLY_RESOLVE_ROLE
        )
        .as_str()
    );
    list.read_with(vcx, |list, _| {
        assert!(list.resolving.is_none());
        assert!(list.replies.is_empty());
        assert!(list.error.is_none());
    });
    assert!(
        vcx.debug_bounds("required-actions-replied-open-chat")
            .is_some()
    );

    // The host confirms the resolve: the history row links the Chat.
    let mut resolved = m4::snapshot("s1");
    for request in &mut resolved.user_action_requests {
        if request.work_item_id == UI {
            m4::resolve(request, m4::NOW + 20, Some("Picked palette B"));
        }
    }
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(resolved)), cx)
    });
    draw(vcx);
    click(vcx, "required-actions-history-toggle".into());
    assert!(
        row_bounds(
            vcx,
            format!("required-actions-history-open-chat-{}", m4::R_UI)
        )
        .is_some()
    );
    select_chat(&overview, vcx, CHAT_UI);
    click(
        vcx,
        format!("required-actions-history-open-chat-{}", m4::R_UI),
    );
    overview.read_with(vcx, |o, _| assert!(o.chat_panel_open));
}

/// A refused send never resolves: a capability error (the host can't
/// deliver the Chat's instructions) and a rejected Turn (a context reset
/// waiting on confirmation) show their message; a retry of the same note
/// reuses the reply's operation id.
#[gpui::test]
fn reply_send_rejections_keep_the_request_open(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let attempt = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = attempt.clone();
    let (overview, calls, vcx) = m4_overview(cx, dir.path(), m4::snapshot("s1"), move |params| {
        if params.get("prompt").is_none() {
            return reply_ok(params);
        }
        match counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
            0 => Err(mast_rpc::RpcError::Failed(
                mast_proto::WorkOutlineError::new(
                    mast_proto::WorkOutlineErrorCode::Capability,
                    "Update Mast on the Chat's device to deliver Work Outline instructions",
                )
                .to_wire_message(),
            )),
            _ => Ok(turn_reply(
                "rejected",
                Some("Codex needs a fresh session for the new instructions"),
            )),
        }
    });
    let list = open_global(&overview, vcx);
    let reply = |list: &Entity<crate::work_outline::required_actions_view::RequiredActionsList>,
                 vcx: &mut gpui::VisualTestContext| {
        list.update(vcx, |list, cx| {
            list.reply_and_resolve(m4::R_UI.into(), "Yes".into(), cx)
        });
        vcx.run_until_parked();
        draw(vcx);
    };

    reply(&list, vcx);
    let error = list.read_with(vcx, |list, _| list.error.clone()).unwrap();
    assert!(
        error.contains("deliver Work Outline instructions"),
        "{error}"
    );
    assert!(vcx.debug_bounds("required-actions-error").is_some());
    assert!(resolves(&calls).is_empty(), "capability error: no resolve");

    reply(&list, vcx);
    let error = list.read_with(vcx, |list, _| list.error.clone()).unwrap();
    assert!(error.contains("fresh session"), "{error}");
    assert!(
        vcx.debug_bounds("required-actions-error-open-chat")
            .is_some(),
        "the Chat holds the reset confirmation"
    );
    assert!(resolves(&calls).is_empty(), "rejected Turn: no resolve");
    let sent = sends(&calls);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["operationId"], sent[1]["operationId"]);
    list.read_with(vcx, |list, _| {
        assert!(
            list.replies
                .get(m4::R_UI)
                .is_some_and(|intent| !intent.sent)
        )
    });
}

/// Sent but not resolved: the retry only resolves (no second message), with
/// the same derived resolve operation id.
#[gpui::test]
fn reply_retry_after_a_failed_resolve_only_resolves(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let flag = fail.clone();
    let (overview, calls, vcx) = m4_overview(cx, dir.path(), m4::snapshot("s1"), move |params| {
        if is_resolve(params) && flag.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err(mast_rpc::RpcError::Failed(
                "work-outline/invalid: try again".into(),
            ));
        }
        reply_started(params)
    });
    let list = open_global(&overview, vcx);
    for _ in 0..2 {
        list.update(vcx, |list, cx| {
            list.reply_and_resolve(m4::R_UI.into(), "Picked palette B".into(), cx)
        });
        vcx.run_until_parked();
    }
    let sent = sends(&calls);
    assert_eq!(sent.len(), 1, "the retry never sends twice");
    assert_eq!(sent[0]["prompt"]["text"], PALETTE_REPLY);
    let resolved = resolves(&calls);
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0]["operationId"], resolved[1]["operationId"]);
    assert_eq!(resolved[1]["note"], "Picked palette B");
    list.read_with(vcx, |list, _| {
        assert!(list.error.is_none());
        assert!(list.replies.is_empty());
    });
}

/// A busy origin Chat gets the reply as a queued row held for the turn end
/// (the composer's queue send), then the request resolves.
#[gpui::test]
fn reply_to_a_busy_chat_queues_then_resolves(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = m4_overview(cx, dir.path(), m4::snapshot("s1"), reply_started);
    let state = overview.read_with(vcx, |o, _| o.state.clone());
    state.update(vcx, |state, _| {
        state.begin_pending_send(CHAT_UI, "m-busy", chrono::Utc::now())
    });
    let list = open_global(&overview, vcx);
    list.update(vcx, |list, cx| {
        list.reply_and_resolve(m4::R_UI.into(), "Picked palette B".into(), cx)
    });
    vcx.run_until_parked();
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    assert_eq!(recorded[0]["chatId"], CHAT_UI);
    assert_eq!(recorded[0]["text"], PALETTE_REPLY);
    assert_eq!(recorded[0]["holdForTurnEnd"], true);
    assert!(is_resolve(&recorded[1]));
}

/// An unavailable origin Chat offers only Resolve: Reply & resolve is
/// disabled (its row button opens nothing) and Enter resolves with the note.
#[gpui::test]
fn unavailable_chat_disables_reply_and_resolve_still_works(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = m4_overview(cx, dir.path(), m4::snapshot("s1"), reply_started);
    let list = open_global(&overview, vcx);
    click(vcx, format!("required-actions-reply-{}", m4::R_API));
    list.read_with(vcx, |list, _| assert!(list.resolving.is_none()));

    click(vcx, format!("required-actions-resolve-{}", m4::R_API));
    list.update(vcx, |list, cx| list.set_note("Went with OAuth", cx));
    draw(vcx);
    click(vcx, "required-actions-reply-submit".into());
    list.update(vcx, |list, cx| {
        list.reply_and_resolve(m4::R_API.into(), "Went with OAuth".into(), cx)
    });
    vcx.run_until_parked();
    assert!(calls.lock().unwrap().is_empty(), "no reply can be sent");

    // Enter falls back to Resolve.
    list.update(vcx, |list, cx| list.submit_note(cx));
    vcx.run_until_parked();
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    assert!(is_resolve(&recorded[0]));
    assert_eq!(recorded[0]["userActionRequestId"], m4::R_API);
    assert_eq!(recorded[0]["note"], "Went with OAuth");
}

/// Work Item Details embeds the same list: Reply & resolve works there and
/// the section stays open (with Open chat) after its last request resolved.
#[gpui::test]
fn details_required_actions_offer_reply_and_resolve(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = m4_overview(cx, dir.path(), m4::snapshot("s1"), reply_started);
    let details = open_details(&overview, vcx, UI);
    click(vcx, format!("required-actions-reply-{}", m4::R_UI));
    let list = details.read_with(vcx, |d, _| d.required_actions_list());
    list.update(vcx, |list, cx| list.set_note("Picked palette B", cx));
    draw(vcx);
    click(vcx, "required-actions-reply-submit".into());
    assert_eq!(sends(&calls).len(), 1);
    assert_eq!(resolves(&calls).len(), 1);
    let mut resolved = m4::snapshot("s1");
    for request in &mut resolved.user_action_requests {
        if request.work_item_id == UI {
            m4::resolve(request, m4::NOW + 20, Some("Picked palette B"));
        }
    }
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(resolved)), cx)
    });
    draw(vcx);
    assert!(
        vcx.debug_bounds("required-actions-replied-open-chat")
            .is_some()
    );
}

const INSTRUCTION_UI: &str = "ins-ui";

/// A snapshot where `UI` carries one instruction and the given proposals
/// target it (`(id, action, proposed text, base text)`).
fn modification_snapshot(
    instruction_text: &str,
    proposals: &[(&str, &str, &str, &str)],
) -> WorkOutlineSnapshot {
    let mut snapshot = m4::snapshot("s1");
    snapshot.instructions.push(
        serde_json::from_value(serde_json::json!({
            "id": INSTRUCTION_UI,
            "spaceId": "s1",
            "workItemId": UI,
            "text": instruction_text,
            "orderKey": "a0",
        }))
        .unwrap(),
    );
    for (id, action, text, base) in proposals {
        let mut proposal = m4::proposal(id, "s1", Some(UI), CHAT_API, text);
        proposal.action = serde_json::from_value(serde_json::json!(action)).unwrap();
        proposal.instruction_id = Some(INSTRUCTION_UI.into());
        proposal.base_text = Some((*base).into());
        proposal.rationale = Some("CI runs both".into());
        snapshot.instruction_proposals.push(proposal);
    }
    snapshot
}

fn open_ui_proposals(
    overview: &Entity<Overview>,
    vcx: &mut gpui::VisualTestContext,
) -> Entity<crate::work_outline::details_panel::WorkItemDetails> {
    overview.update(vcx, |o, cx| o.open_proposals("s1".into(), UI.into(), cx));
    vcx.run_until_parked();
    draw(vcx);
    overview.read_with(vcx, |o, _| o.work_item_details.clone().unwrap())
}

/// An edit shows as a diff with its rationale; Accept goes through the drift
/// confirmation and sends the proposed text without `allowStale`.
#[gpui::test]
fn edit_proposal_shows_a_diff_and_accepts(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = modification_snapshot(
        "Run the linter.",
        &[("p-edit", "edit", "Run the linter and tests.", "Run the linter.")],
    );
    let (overview, calls, vcx) = m4_overview(cx, dir.path(), snapshot, proposal_reply);
    let details = open_ui_proposals(&overview, vcx);
    assert!(row_bounds(vcx, "work-item-proposal-diff-p-edit".into()).is_some());
    assert!(row_bounds(vcx, "work-item-proposal-rationale-p-edit".into()).is_some());
    assert!(row_bounds(vcx, "work-item-proposal-stale-p-edit".into()).is_none());
    assert!(row_bounds(vcx, "work-item-proposal-review-p-edit".into()).is_some());
    click(vcx, "work-item-proposal-accept-p-edit".into());
    let request = details
        .update(vcx, |d, _| d.captured_drift.take())
        .expect("chat-ui is assigned to UI: drift confirmation");
    vcx.update(|window, cx| (request.on_decision)(true, window, cx));
    vcx.run_until_parked();
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["op"], "approveWorkItemInstructionProposal");
    assert_eq!(recorded[0]["proposalId"], "p-edit");
    assert_eq!(recorded[0]["approvedText"], "Run the linter and tests.");
    assert!(recorded[0].get("allowStale").is_none());
}

/// An edit whose instruction changed since the base is stale: the three-way
/// view shows, and Apply anyway sends `allowStale`.
#[gpui::test]
fn stale_edit_shows_three_way_and_applies_anyway(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = modification_snapshot(
        "Run clippy.",
        &[("p-edit", "edit", "Run the linter and tests.", "Run the linter.")],
    );
    let (overview, calls, vcx) = m4_overview(cx, dir.path(), snapshot, proposal_reply);
    let details = open_ui_proposals(&overview, vcx);
    assert!(row_bounds(vcx, "work-item-proposal-stale-p-edit".into()).is_some());
    assert!(row_bounds(vcx, "work-item-proposal-now-p-edit".into()).is_some());
    assert!(row_bounds(vcx, "work-item-proposal-diff-p-edit".into()).is_some());
    click(vcx, "work-item-proposal-accept-p-edit".into());
    if let Some(request) = details.update(vcx, |d, _| d.captured_drift.take()) {
        vcx.update(|window, cx| (request.on_decision)(true, window, cx));
        vcx.run_until_parked();
    }
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["allowStale"], true);
}

/// A remove shows the struck-through instruction and the rationale; Dismiss
/// rejects without a reason.
#[gpui::test]
fn remove_proposal_shows_strikethrough_and_dismisses(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = modification_snapshot(
        "Run the linter.",
        &[("p-rm", "remove", "", "Run the linter.")],
    );
    let (overview, calls, vcx) = m4_overview(cx, dir.path(), snapshot, proposal_reply);
    open_ui_proposals(&overview, vcx);
    assert!(row_bounds(vcx, "work-item-proposal-diff-p-rm".into()).is_some());
    assert!(row_bounds(vcx, "work-item-proposal-rationale-p-rm".into()).is_some());
    assert!(
        row_bounds(vcx, "work-item-proposal-review-p-rm".into()).is_none(),
        "a remove has no editor"
    );
    click(vcx, "work-item-proposal-dismiss-p-rm".into());
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["op"], "rejectWorkItemInstructionProposal");
    assert_eq!(recorded[0]["proposalId"], "p-rm");
    assert!(recorded[0].get("reason").is_none());
}

/// Proposals that touch one instruction share a group; an add stands alone.
#[gpui::test]
fn proposals_for_one_instruction_group_together(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = modification_snapshot(
        "Run the linter.",
        &[
            ("p-a", "edit", "Run the linter and tests.", "Run the linter."),
            ("p-b", "remove", "", "Run the linter."),
        ],
    );
    let (overview, _calls, vcx) = m4_overview(cx, dir.path(), snapshot, proposal_reply);
    open_ui_proposals(&overview, vcx);
    let group = row_bounds(vcx, format!("work-item-proposal-group-{INSTRUCTION_UI}"))
        .expect("one group for both");
    for id in ["p-a", "p-b"] {
        let card = row_bounds(vcx, format!("work-item-proposal-{id}")).unwrap();
        assert!(group.contains(&card.center()), "{id} sits in the group");
    }
    // The pre-existing add proposal on UI is outside the group.
    let add = row_bounds(vcx, format!("work-item-proposal-{}", m4::P_UI)).unwrap();
    assert!(!group.contains(&add.center()));
}
