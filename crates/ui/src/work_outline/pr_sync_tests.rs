// PR review sync UI tests (destination, chips, the GitHub inbox section,
// ⌘↩ reviews, system Required Actions, hosts without the RPCs). Included
// from `overview.rs`'s test module so they share its Overview fixtures.
// Every GitHub reply is scripted; nothing reaches GitHub.
use super::*;

use crate::github::fixture as gf;
use crate::github::panel::GithubPanel;

type PrCalls = std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>;

/// The store fixture (it links `acme/launch#42` on `w-api`) with the sync
/// destination `destination`.
fn sync_snapshot(destination: Option<&str>) -> WorkOutlineSnapshot {
    let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
    snapshot.space_settings.pr_review_sync_work_item_id = destination.map(Into::into);
    snapshot
}

/// Default scripted host: live statuses, two review requests (launch#42,
/// synced on w-api, and launch#7), one open PR of mine, and creates.
fn default_reply(
    method: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value, zeron_rpc::RpcError> {
    Ok(match method {
        "PrStatus" => {
            let repo = params["repo"].as_str().unwrap_or("launch");
            let number = params["number"].as_u64().unwrap_or(42);
            serde_json::to_value(gf::summary(repo, number, "Add auth endpoints")).unwrap()
        }
        "PrReviewRequested" => serde_json::to_value(vec![
            gf::summary("launch", 42, "Add auth endpoints"),
            gf::summary("launch", 7, "Fix login"),
        ])
        .unwrap(),
        "PrMyOpen" => {
            let mut mine = gf::summary("app", 3, "My change");
            mine.my_review_state = zeron_proto::PullRequestMyReviewState::None;
            serde_json::to_value(vec![mine]).unwrap()
        }
        "PrReviewSyncNow" => serde_json::json!({"created": 1, "updated": 2, "resolved": 0}),
        _ => match params["op"].as_str() {
            Some("createWorkItem") => serde_json::json!({"workItemId": "w-new-pr"}),
            Some("createWorkItemExternalReference") => serde_json::json!({"referenceId": "r-new"}),
            _ => serde_json::json!({}),
        },
    })
}

fn pr_overview<'a>(
    cx: &'a mut gpui::TestAppContext,
    dir: &std::path::Path,
    snapshot: WorkOutlineSnapshot,
    reply: impl Fn(&str, &serde_json::Value) -> Result<serde_json::Value, zeron_rpc::RpcError>
    + Send
    + Sync
    + 'static,
) -> (Entity<Overview>, PrCalls, &'a mut gpui::VisualTestContext) {
    let (overview, state, vcx) = outline_overview(cx, dir);
    vcx.update(|_, cx| crate::motion::set_reduced_motion(cx, true));
    let calls: PrCalls = Default::default();
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
    draw(vcx);
    (overview, calls, vcx)
}

fn press(vcx: &mut gpui::VisualTestContext, selector: String) {
    let at = row_bounds(vcx, selector.clone())
        .unwrap_or_else(|| panic!("{selector} renders"))
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    draw(vcx);
}

fn mutations(calls: &PrCalls, op: &str) -> Vec<serde_json::Value> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, params)| method == "WorkOutlineMutate" && params["op"] == op)
        .map(|(_, params)| params.clone())
        .collect()
}

fn calls_to(calls: &PrCalls, method: &str) -> Vec<serde_json::Value> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, _)| m == method)
        .map(|(_, params)| params.clone())
        .collect()
}

fn open_column(
    overview: &Entity<Overview>,
    vcx: &mut gpui::VisualTestContext,
) -> Entity<GithubPanel> {
    vcx.update(|window, cx| {
        overview.update(cx, |o, cx| {
            o.run_work_outline_command(WorkOutlineCommand::OpenLinear, window, cx)
        })
    });
    vcx.run_until_parked();
    draw(vcx);
    overview.read_with(vcx, |o, _| {
        o.github_panel().expect("the GitHub section opens")
    })
}

/// Space-root details: Choose… lists every live item; picking one sends
/// setPrReviewSyncDestination; Clear sends `workItemId: null`.
#[gpui::test]
fn destination_picker_sets_and_clears_the_setting(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = pr_overview(cx, dir.path(), sync_snapshot(None), default_reply);
    open_details(&overview, vcx, "");
    draw(vcx);
    assert!(vcx.debug_bounds("space-pr-sync").is_some());
    assert!(
        vcx.debug_bounds("space-pr-sync-now").is_none(),
        "no Sync now while off"
    );
    press(vcx, "space-pr-sync-choose".into());
    assert!(vcx.debug_bounds("space-pr-sync-picker").is_some());
    assert!(
        row_bounds(vcx, format!("space-pr-sync-pick-{RETIRED}")).is_none(),
        "archived items are not offered"
    );
    press(vcx, format!("space-pr-sync-pick-{UI}"));
    let set = mutations(&calls, "setPrReviewSyncDestination");
    assert_eq!(set.len(), 1);
    assert_eq!(set[0]["spaceId"], "s1");
    assert_eq!(set[0]["workItemId"], UI);
    assert!(
        set[0]["operationId"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert!(
        vcx.debug_bounds("space-pr-sync-picker").is_none(),
        "closes once set"
    );

    // Set: the destination shows, Clear turns the sync off.
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(sync_snapshot(Some(UI)))), cx)
    });
    draw(vcx);
    press(vcx, "space-pr-sync-clear".into());
    let set = mutations(&calls, "setPrReviewSyncDestination");
    assert_eq!(set.len(), 2);
    assert!(set[1]["workItemId"].is_null());
}

/// Sync now runs PrReviewSyncNow for the Space and shows the result.
#[gpui::test]
fn sync_now_runs_and_reports_the_result(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) =
        pr_overview(cx, dir.path(), sync_snapshot(Some(LAUNCH)), default_reply);
    open_details(&overview, vcx, "");
    draw(vcx);
    press(vcx, "space-pr-sync-now".into());
    assert_eq!(
        calls_to(&calls, "PrReviewSyncNow"),
        [serde_json::json!({"spaceId": "s1"})]
    );
    let state = overview.read_with(vcx, |o, _| o.state.clone());
    state.read_with(vcx, |state, _| {
        assert_eq!(
            state.github.sync_results.get("s1").map(|r| r.line()),
            Some("Synced: 1 created \u{00b7} 2 updated \u{00b7} 0 resolved".to_string())
        );
    });
    assert!(vcx.debug_bounds("space-pr-sync-result").is_some());
}

/// Linked items show a live PR chip from PrStatus (fetched once, then
/// cached); the destination row shows its marker; Details refresh with
/// forceRefresh on open and by ↻. Without the sync no chip and no calls.
#[gpui::test]
fn pr_chips_from_pr_status_and_the_destination_marker(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, calls, vcx) = pr_overview(cx, dir.path(), sync_snapshot(None), default_reply);
    let chip = format!("outline-pr-{}", row_key::item("s1", API));
    assert!(
        row_bounds(vcx, chip.clone()).is_none(),
        "the sync does not apply"
    );
    assert!(calls_to(&calls, "PrStatus").is_empty());

    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) =
        pr_overview(cx, dir.path(), sync_snapshot(Some(LAUNCH)), default_reply);
    draw(vcx);
    assert!(row_bounds(vcx, chip).is_some(), "the chip renders");
    assert!(
        row_bounds(
            vcx,
            format!("outline-pr-destination-{}", row_key::item("s1", LAUNCH))
        )
        .is_some(),
        "the destination marker"
    );
    let status = calls_to(&calls, "PrStatus");
    assert_eq!(status.len(), 1, "fetched once, then cached");
    assert_eq!(
        status[0],
        serde_json::json!({"owner": "acme", "repo": "launch", "number": 42})
    );
    let state = overview.read_with(vcx, |o, _| o.state.clone());
    state.read_with(vcx, |state, _| {
        let pr = zeron_proto::PullRequestRef::new("acme", "launch", 42);
        let ready = state.github.ready(&pr).expect("cached");
        assert_eq!(
            crate::github::chip::chip_text(ready),
            "#42 \u{00b7} Open \u{00b7} \u{2713} \u{00b7} Review requested"
        );
    });
    // Details: the Pull request section refreshes on open, and by ↻.
    open_details(&overview, vcx, API);
    draw(vcx);
    assert!(vcx.debug_bounds("work-item-pr").is_some());
    assert!(vcx.debug_bounds("work-item-pr-chip").is_some());
    let forced = |calls: &PrCalls| {
        calls_to(calls, "PrStatus")
            .iter()
            .filter(|p| p["forceRefresh"] == true)
            .count()
    };
    assert_eq!(forced(&calls), 1, "refreshed on open");
    press(vcx, "work-item-pr-refresh".into());
    assert_eq!(forced(&calls), 2);
}

/// ⌘↩ on a PR review item (a child of the destination) starts a code review
/// filed under that item instead of the Start Chat dialog; a feature item
/// that merely links its PR keeps the generic Start Chat.
#[gpui::test]
fn cmd_enter_on_a_pr_item_starts_a_review_filed_under_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) =
        pr_overview(cx, dir.path(), sync_snapshot(Some(LAUNCH)), default_reply);
    let key = row_key::item("s1", API);
    overview.update(vcx, |o, cx| {
        o.outline.update(cx, |outline, cx| outline.select(&key, cx))
    });
    let handle = overview.read_with(vcx, |o, cx| {
        gpui::Focusable::focus_handle(o.outline.read(cx), cx)
    });
    vcx.update(|window, cx| window.focus(&handle, cx));
    draw(vcx);
    vcx.dispatch_action(crate::work_outline::outline_view::StartChatSelected);
    vcx.run_until_parked();
    overview.update(vcx, |o, _| {
        assert!(
            o.captured_start_chat.take().is_none(),
            "no Start Chat dialog"
        );
        let (pr, item, action) = o.captured_pr_review.take().expect("StartPrReview");
        assert_eq!(pr.external_id(), "acme/launch#42");
        assert_eq!(item, Some(("s1".to_string(), API.to_string())));
        assert_eq!(action, zeron_proto::PrReviewAction::CodeReview);
    });

    // Without a destination API is a feature item linking its PR.
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(sync_snapshot(None))), cx)
    });
    draw(vcx);
    vcx.update(|window, cx| window.focus(&handle, cx));
    vcx.dispatch_action(crate::work_outline::outline_view::StartChatSelected);
    vcx.run_until_parked();
    overview.update(vcx, |o, _| {
        assert!(o.captured_pr_review.take().is_none());
        assert_eq!(
            o.captured_start_chat.take(),
            Some(("s1".to_string(), API.to_string()))
        );
    });
}

/// The item menu offers Review / Fix CI / Open on GitHub / Refresh and the
/// destination toggle where the sync applies.
#[gpui::test]
fn item_menu_offers_github_entries(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) =
        pr_overview(cx, dir.path(), sync_snapshot(Some(LAUNCH)), default_reply);
    overview.read_with(vcx, |o, cx| {
        let outline = o.outline.read(cx);
        let ids = |key: &str| -> Vec<&'static str> {
            outline
                .context_menu(key, cx)
                .unwrap()
                .entries
                .iter()
                .map(|e| e.id)
                .collect()
        };
        let api = ids(&row_key::item("s1", API));
        for id in [
            "github-review",
            "github-fix-ci",
            "github-open",
            "github-refresh",
        ] {
            assert!(api.contains(&id), "{id}");
        }
        assert!(ids(&row_key::item("s1", LAUNCH)).contains(&"github-clear-destination"));
        assert!(ids(&row_key::item("s1", UI)).contains(&"github-use-destination"));
    });
}

/// The GitHub section (Linear off): both lists for the Space, the synced
/// marker with Reveal, Review filed under the synced item (else the
/// destination), Create under the destination (item + PR reference), Link
/// to the selected item, and folds that persist.
#[gpui::test]
fn github_section_lists_marks_and_acts(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) =
        pr_overview(cx, dir.path(), sync_snapshot(Some(LAUNCH)), default_reply);
    assert!(
        vcx.debug_bounds("outline-linear").is_some(),
        "the Inbox toolbar button"
    );
    let panel = open_column(&overview, vcx);
    overview.read_with(vcx, |o, _| {
        assert!(o.linear_panel().is_none(), "Linear is off")
    });
    assert!(vcx.debug_bounds("linear-column").is_some());
    assert_eq!(
        calls_to(&calls, "PrReviewRequested"),
        [serde_json::json!({"spaceId": "s1"})]
    );
    assert_eq!(
        calls_to(&calls, "PrMyOpen"),
        [serde_json::json!({"spaceId": "s1"})]
    );
    for selector in [
        "github-pr-row-launch-42-review-requested",
        "github-pr-row-launch-7-review-requested",
        "github-pr-row-app-3-my-open",
        "github-pr-synced-launch-42-review-requested",
    ] {
        assert!(row_bounds(vcx, selector.into()).is_some(), "{selector}");
    }
    assert!(row_bounds(vcx, "github-pr-synced-launch-7-review-requested".into()).is_none());

    // Link needs a selected item.
    overview.update(vcx, |o, cx| {
        o.outline
            .update(cx, |outline, cx| outline.select("no-such-row", cx))
    });
    draw(vcx);
    press(vcx, "github-pr-link-app-3-my-open".into());
    assert!(mutations(&calls, "createWorkItemExternalReference").is_empty());
    panel.read_with(vcx, |panel, _| assert!(panel.notice.is_some()));

    // Reveal selects the synced item.
    press(vcx, "github-pr-synced-launch-42-review-requested".into());
    overview.read_with(vcx, |o, cx| {
        assert_eq!(
            o.outline.read(cx).selected_key(),
            Some(row_key::item("s1", API).as_str())
        )
    });

    // Review: filed under the synced item, else the destination.
    press(vcx, "github-pr-review-launch-42-review-requested".into());
    overview.update(vcx, |o, _| {
        let (pr, item, _) = o.captured_pr_review.take().unwrap();
        assert_eq!(pr.number, 42);
        assert_eq!(item, Some(("s1".to_string(), API.to_string())));
    });
    press(vcx, "github-pr-review-launch-7-review-requested".into());
    overview.update(vcx, |o, _| {
        let (_, item, _) = o.captured_pr_review.take().unwrap();
        assert_eq!(item, Some(("s1".to_string(), LAUNCH.to_string())));
    });

    // Create: an item under the destination, then its PR reference.
    press(vcx, "github-pr-create-launch-7-review-requested".into());
    let created = mutations(&calls, "createWorkItem");
    assert_eq!(created.len(), 1);
    assert_eq!(created[0]["parentWorkItemId"], LAUNCH);
    assert_eq!(created[0]["title"], "launch#7 \u{00b7} Fix login");
    let refs = mutations(&calls, "createWorkItemExternalReference");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0]["workItemId"], "w-new-pr");
    assert_eq!(refs[0]["provider"], "github");
    assert_eq!(refs[0]["referenceType"], "pullRequest");
    assert_eq!(refs[0]["externalId"], "acme/launch#7");

    // Link: to the selected item.
    let ui_key = row_key::item("s1", UI);
    overview.update(vcx, |o, cx| {
        o.outline
            .update(cx, |outline, cx| outline.select(&ui_key, cx))
    });
    draw(vcx);
    press(vcx, "github-pr-link-app-3-my-open".into());
    let refs = mutations(&calls, "createWorkItemExternalReference");
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[1]["workItemId"], UI);
    assert_eq!(refs[1]["externalId"], "acme/app#3");

    // Folds persist per device.
    press(vcx, "github-section-my-open".into());
    assert!(row_bounds(vcx, "github-pr-row-app-3-my-open".into()).is_none());
    press(vcx, "github-panel-fold".into());
    assert!(row_bounds(vcx, "github-pr-row-launch-7-review-requested".into()).is_none());
    let saved = crate::github::prefs::load_state(dir.path());
    assert!(saved.folded);
    assert!(
        saved
            .collapsed
            .contains(&crate::github::prefs::GithubList::MyOpen)
    );

    // ↻ refetches; the column header closes the whole column.
    press(vcx, "github-panel-fold".into());
    press(vcx, "github-panel-refresh".into());
    assert_eq!(calls_to(&calls, "PrMyOpen").len(), 2);
    press(vcx, "inbox-column-close".into());
    overview.read_with(vcx, |o, _| assert!(!o.column_shown()));
}

/// Dragging a PR row onto an Outline item shows the "into" indicator and
/// creates the work item under that item.
#[gpui::test]
fn drag_pr_onto_item_creates_under_it(cx: &mut gpui::TestAppContext) {
    use crate::work_outline::restructure::DropZone;
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) =
        pr_overview(cx, dir.path(), sync_snapshot(Some(LAUNCH)), default_reply);
    open_column(&overview, vcx);
    let start = row_bounds(vcx, "github-pr-row-launch-7-review-requested".into()).unwrap();
    let start = gpui::point(start.left() + px(30.0), start.center().y);
    let target = row_bounds(vcx, format!("outline-row-{}", row_key::item("s1", UI))).unwrap();
    let end = target.center();
    let left = Some(gpui::MouseButton::Left);
    vcx.simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::default());
    vcx.simulate_mouse_move(
        start + gpui::point(px(0.0), px(6.0)),
        left,
        gpui::Modifiers::default(),
    );
    vcx.simulate_mouse_move(end, left, gpui::Modifiers::default());
    vcx.simulate_mouse_move(end, left, gpui::Modifiers::default());
    draw(vcx);
    overview.read_with(vcx, |o, cx| {
        let hint = o.outline.read(cx).drop_hint().cloned();
        assert_eq!(
            hint.map(|h| (h.key, h.zone)),
            Some((row_key::item("s1", UI), DropZone::Into))
        );
    });
    vcx.simulate_mouse_up(end, gpui::MouseButton::Left, gpui::Modifiers::default());
    vcx.run_until_parked();
    draw(vcx);
    let created = mutations(&calls, "createWorkItem");
    assert_eq!(created.len(), 1);
    assert_eq!(created[0]["parentWorkItemId"], UI);
}

/// A system-origin Required Action (PR review sync) reads "GitHub" as its
/// origin, offers Resolve but no Reply & resolve, and Open shows its PR
/// item.
#[gpui::test]
fn system_required_actions_resolve_only_and_open_the_pr_item(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mut snapshot = sync_snapshot(Some(LAUNCH));
    let mut request = crate::work_outline::required_actions::fixture::request(
        "req-pr",
        "s1",
        API,
        "",
        "Review requested: launch#42 Add auth endpoints",
    );
    request.origin_kind = zeron_proto::UserActionOriginKind::System;
    request.origin_chat_available = false;
    request.created_by = None;
    snapshot.user_action_requests = vec![request];
    let (overview, _calls, vcx) = pr_overview(cx, dir.path(), snapshot, default_reply);
    vcx.update(|window, cx| {
        overview.update(cx, |o, cx| {
            o.run_work_outline_command(WorkOutlineCommand::OpenRequiredActions, window, cx)
        })
    });
    draw(vcx);
    let list = overview.read_with(vcx, |o, _| o.required_actions_panel().unwrap());
    let rows = list.read_with(vcx, |list, _| list.rows());
    let row = &rows.open[0];
    assert!(row.system && !row.chat_available);
    assert_eq!(row.chat_label, "GitHub");
    assert_eq!(
        row.pr_url.as_deref(),
        Some("https://github.com/acme/launch/pull/42")
    );
    assert!(row_bounds(vcx, "required-actions-row-req-pr".into()).is_some());
    assert!(row_bounds(vcx, "required-actions-resolve-req-pr".into()).is_some());
    assert!(
        row_bounds(vcx, "required-actions-reply-req-pr".into()).is_none(),
        "Resolve only"
    );
    assert!(row_bounds(vcx, "required-actions-view-pr-req-pr".into()).is_some());
    // The resolve form has no Reply & resolve either.
    press(vcx, "required-actions-resolve-req-pr".into());
    assert!(
        vcx.debug_bounds("required-actions-resolve-submit")
            .is_some()
    );
    assert!(vcx.debug_bounds("required-actions-reply-submit").is_none());
    list.update(vcx, |list, cx| list.cancel_resolve(cx));
    draw(vcx);
    // Open lands on the PR item's details.
    press(vcx, "required-actions-open-req-pr".into());
    overview.read_with(vcx, |o, cx| {
        let details = o.work_item_details.clone().expect("details open");
        assert_eq!(details.read(cx).item_id, API);
    });
}

/// A host without the PR review sync RPCs: the section shows a calm "not
/// available" state, the chip stays a plain `#42`, Sync now explains, and
/// the destination mutation reports it inline.
#[gpui::test]
fn unknown_method_reads_as_not_available(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) = pr_overview(
        cx,
        dir.path(),
        sync_snapshot(Some(LAUNCH)),
        |method, params| {
            if method.starts_with("Pr") {
                return Err(zeron_rpc::RpcError::UnknownMethod(method.to_string()));
            }
            if params["op"] == "setPrReviewSyncDestination" {
                return Err(zeron_rpc::RpcError::Failed(
                    "unknown variant `setPrReviewSyncDestination`".into(),
                ));
            }
            default_reply(method, params)
        },
    );
    let panel = open_column(&overview, vcx);
    assert!(vcx.debug_bounds("github-panel-blocker").is_some());
    panel.read_with(vcx, |panel, _| {
        let crate::github::panel::Load::Failed(error) = &panel.my_open else {
            panic!("failed");
        };
        assert!(error.not_available());
        assert_eq!(
            error.user_text(),
            "PR review sync isn\u{2019}t available on this host yet"
        );
    });
    assert!(row_bounds(vcx, format!("outline-pr-{}", row_key::item("s1", API))).is_some());
    open_details(&overview, vcx, "");
    draw(vcx);
    press(vcx, "space-pr-sync-now".into());
    let state = overview.read_with(vcx, |o, _| o.state.clone());
    state.read_with(vcx, |state, _| {
        assert_eq!(
            state.github.sync_results.get("s1").map(|r| r.line()),
            Some("PR review sync isn\u{2019}t available on this host yet".to_string())
        );
    });
    press(vcx, "space-pr-sync-clear".into());
    assert!(vcx.debug_bounds("space-pr-sync-error").is_some());
}
