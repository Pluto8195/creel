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

/// Run pending timers and work, then redraw (after a menu closes).
fn settle(vcx: &mut gpui::VisualTestContext) {
    vcx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    vcx.run_until_parked();
    draw(vcx);
}

/// The Space's device advertises the PR review sync (so the section shows
/// without a destination).
fn advertise_pr_sync(overview: &Entity<Overview>, vcx: &mut gpui::VisualTestContext) {
    let state = overview.read_with(vcx, |o, _| o.state.clone());
    state.update(vcx, |state, _| {
        state.devices = vec![
            serde_json::from_value(serde_json::json!({
                "id": "d", "name": "Mac", "platform": "macos", "lastSeenAt": null,
                "capabilities": [zeron_proto::capabilities::PR_REVIEW_SYNC_V1],
            }))
            .unwrap(),
        ];
    });
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
/// marker with Reveal, and from each row's ⋯ menu: Review filed under the
/// synced item (else the destination), Create under the destination (item
/// + PR reference), Link to the selected item; folds that persist.
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
    press(vcx, "github-pr-more-app-3-my-open".into());
    press(vcx, "github-pr-link-app-3-my-open".into());
    assert!(mutations(&calls, "createWorkItemExternalReference").is_empty());
    panel.read_with(vcx, |panel, _| assert!(panel.notice.is_some()));
    settle(vcx);

    // Reveal selects the synced item.
    press(vcx, "github-pr-synced-launch-42-review-requested".into());
    overview.read_with(vcx, |o, cx| {
        assert_eq!(
            o.outline.read(cx).selected_key(),
            Some(row_key::item("s1", API).as_str())
        )
    });

    // Review: filed under the synced item, else the destination.
    press(vcx, "github-pr-more-launch-42-review-requested".into());
    press(vcx, "github-pr-review-launch-42-review-requested".into());
    settle(vcx);
    overview.update(vcx, |o, _| {
        let (pr, item, _) = o.captured_pr_review.take().unwrap();
        assert_eq!(pr.number, 42);
        assert_eq!(item, Some(("s1".to_string(), API.to_string())));
    });
    press(vcx, "github-pr-more-launch-7-review-requested".into());
    press(vcx, "github-pr-review-launch-7-review-requested".into());
    settle(vcx);
    overview.update(vcx, |o, _| {
        let (_, item, _) = o.captured_pr_review.take().unwrap();
        assert_eq!(item, Some(("s1".to_string(), LAUNCH.to_string())));
    });

    // Create: an item under the destination, then its PR reference.
    press(vcx, "github-pr-more-launch-7-review-requested".into());
    press(vcx, "github-pr-create-launch-7-review-requested".into());
    settle(vcx);
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
    press(vcx, "github-pr-more-app-3-my-open".into());
    press(vcx, "github-pr-link-app-3-my-open".into());
    settle(vcx);
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

/// The header's scope chip opens a real menu (All repos + each repo, the
/// current one checked) and picking a repo filters both lists; ↑/↓/↩ pick
/// from the keyboard; Esc closes the menu, not the column. Nothing cycles.
#[gpui::test]
fn scope_dropdown_filters_by_repo(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) =
        pr_overview(cx, dir.path(), sync_snapshot(Some(LAUNCH)), default_reply);
    let panel = open_column(&overview, vcx);
    let space = panel.read_with(vcx, |panel, _| panel.space_id.clone());
    press(vcx, "github-panel-space".into());
    assert!(
        vcx.debug_bounds("github-scope-menu").is_some(),
        "a menu opens"
    );
    for selector in [
        "github-scope-repo-all",
        "github-scope-repo-acme-app",
        "github-scope-repo-acme-launch",
    ] {
        assert!(row_bounds(vcx, selector.into()).is_some(), "{selector}");
    }
    panel.read_with(vcx, |panel, cx| {
        let rows = panel.menu_rows(cx);
        assert!(rows[0].0.checked, "All repos is current");
        assert_eq!(rows[0].0.label.as_ref(), "All repos");
        assert_eq!(panel.space_id, space, "opening never switches anything");
    });
    press(vcx, "github-scope-repo-acme-app".into());
    settle(vcx);
    panel.read_with(vcx, |panel, _| {
        assert!(panel.open_menu_kind().is_none(), "picking closes it");
        assert_eq!(panel.repo_filter.as_deref(), Some("acme/app"));
        assert_eq!(panel.space_id, space);
    });
    assert!(row_bounds(vcx, "github-pr-row-app-3-my-open".into()).is_some());
    assert!(row_bounds(vcx, "github-pr-row-launch-7-review-requested".into()).is_none());
    assert!(
        row_bounds(vcx, "github-panel-space".into()).is_some(),
        "the chip stays"
    );

    // Keyboard: the cursor starts on the current repo (app); ↓ ↩ = launch.
    press(vcx, "github-panel-space".into());
    vcx.simulate_keystrokes("down enter");
    settle(vcx);
    panel.read_with(vcx, |panel, _| {
        assert!(panel.open_menu_kind().is_none());
        assert_eq!(panel.repo_filter.as_deref(), Some("acme/launch"));
    });
    assert!(row_bounds(vcx, "github-pr-row-launch-7-review-requested".into()).is_some());
    assert!(row_bounds(vcx, "github-pr-row-app-3-my-open".into()).is_none());

    // Esc closes the menu and leaves the column open; All repos clears.
    press(vcx, "github-panel-space".into());
    vcx.simulate_keystrokes("escape");
    settle(vcx);
    panel.read_with(vcx, |panel, _| assert!(panel.open_menu_kind().is_none()));
    overview.read_with(vcx, |o, _| {
        assert!(o.column_shown(), "Esc only closed the menu")
    });
    press(vcx, "github-panel-space".into());
    press(vcx, "github-scope-repo-all".into());
    panel.read_with(vcx, |panel, _| assert_eq!(panel.repo_filter, None));
    assert!(row_bounds(vcx, "github-pr-row-app-3-my-open".into()).is_some());
}

/// No destination: the section leads with "Auto-sync review requests
/// into… Choose item ▾"; picking an item sends setPrReviewSyncDestination;
/// once set the bar reads "Syncing into <item>" with Sync now, Change ▾
/// (searchable, keyboard) and Stop.
#[gpui::test]
fn sync_cta_sets_the_destination(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = pr_overview(cx, dir.path(), sync_snapshot(None), default_reply);
    advertise_pr_sync(&overview, vcx);
    let panel = open_column(&overview, vcx);
    assert!(vcx.debug_bounds("github-sync-cta").is_some());
    assert!(vcx.debug_bounds("github-sync-status").is_none());
    assert!(
        row_bounds(vcx, "github-pr-pending-launch-7-review-requested".into()).is_none(),
        "nothing is pending without a destination"
    );
    press(vcx, "github-sync-choose".into());
    assert!(vcx.debug_bounds("github-sync-menu").is_some());
    assert!(vcx.debug_bounds("github-sync-create").is_some());
    assert!(
        row_bounds(vcx, format!("github-sync-pick-{RETIRED}")).is_none(),
        "archived items are not offered"
    );
    press(vcx, format!("github-sync-pick-{UI}"));
    settle(vcx);
    let set = mutations(&calls, "setPrReviewSyncDestination");
    assert_eq!(set.len(), 1);
    assert_eq!(set[0]["spaceId"], "s1");
    assert_eq!(set[0]["workItemId"], UI);
    panel.read_with(vcx, |panel, _| assert!(panel.open_menu_kind().is_none()));

    // Set: "Syncing into UI".
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(sync_snapshot(Some(UI)))), cx)
    });
    draw(vcx);
    assert!(vcx.debug_bounds("github-sync-cta").is_none());
    assert!(vcx.debug_bounds("github-sync-status").is_some());
    assert!(vcx.debug_bounds("github-sync-destination").is_some());
    assert!(vcx.debug_bounds("github-sync-last").is_some());
    assert!(
        row_bounds(vcx, "github-pr-pending-launch-7-review-requested".into()).is_some(),
        "launch#7 waits for its first sync"
    );
    press(vcx, "github-sync-now".into());
    assert_eq!(
        calls_to(&calls, "PrReviewSyncNow"),
        [serde_json::json!({"spaceId": "s1"})]
    );

    // Change ▾: type to search, ↩ picks the first match.
    press(vcx, "github-sync-change".into());
    assert!(vcx.debug_bounds("github-sync-search").is_some());
    vcx.simulate_input("house");
    draw(vcx);
    assert!(row_bounds(vcx, format!("github-sync-pick-{HOUSEKEEPING}")).is_some());
    assert!(row_bounds(vcx, format!("github-sync-pick-{API}")).is_none());
    vcx.simulate_keystrokes("enter");
    settle(vcx);
    let set = mutations(&calls, "setPrReviewSyncDestination");
    assert_eq!(set.len(), 2);
    assert_eq!(set[1]["workItemId"], HOUSEKEEPING);

    // Stop clears it.
    press(vcx, "github-sync-stop".into());
    let set = mutations(&calls, "setPrReviewSyncDestination");
    assert_eq!(set.len(), 3);
    assert!(set[2]["workItemId"].is_null());
}

/// "Create 'PR reviews' item" creates a top-level item and makes it the
/// destination.
#[gpui::test]
fn create_pr_reviews_item_creates_and_sets(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = pr_overview(cx, dir.path(), sync_snapshot(None), default_reply);
    advertise_pr_sync(&overview, vcx);
    open_column(&overview, vcx);
    press(vcx, "github-sync-choose".into());
    press(vcx, "github-sync-create".into());
    let created = mutations(&calls, "createWorkItem");
    assert_eq!(created.len(), 1);
    assert_eq!(created[0]["title"], "PR reviews");
    assert!(created[0]["parentWorkItemId"].is_null(), "top level");
    let set = mutations(&calls, "setPrReviewSyncDestination");
    assert_eq!(set.len(), 1);
    assert_eq!(set[0]["workItemId"], "w-new-pr");
    assert_eq!(set[0]["spaceId"], "s1");
}

/// With a destination: a synced PR reads "Synced ✓" and its row's click
/// reveals the item; the manual actions live in the row's ⋯ menu only.
#[gpui::test]
fn synced_rows_reveal_and_manual_actions_are_secondary(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) =
        pr_overview(cx, dir.path(), sync_snapshot(Some(LAUNCH)), default_reply);
    let panel = open_column(&overview, vcx);
    assert!(vcx.debug_bounds("github-sync-status").is_some());
    let key = "launch-42-review-requested";
    assert!(row_bounds(vcx, format!("github-pr-synced-{key}")).is_some());
    for action in ["review", "link", "create"] {
        assert!(
            row_bounds(vcx, format!("github-pr-{action}-{key}")).is_none(),
            "{action} is not a row button"
        );
    }
    // The row's primary click reveals the synced item.
    let row = row_bounds(vcx, format!("github-pr-row-{key}")).unwrap();
    vcx.simulate_click(
        gpui::point(row.left() + px(30.0), row.center().y),
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    draw(vcx);
    overview.read_with(vcx, |o, cx| {
        assert_eq!(
            o.outline.read(cx).selected_key(),
            Some(row_key::item("s1", API).as_str())
        )
    });
    // ⋯ opens the secondary actions.
    press(vcx, format!("github-pr-more-{key}"));
    assert!(vcx.debug_bounds("github-pr-menu").is_some());
    for selector in ["reveal", "review", "link", "create"] {
        assert!(
            row_bounds(vcx, format!("github-pr-{selector}-{key}")).is_some(),
            "{selector} in the menu"
        );
    }
    vcx.simulate_keystrokes("escape");
    settle(vcx);
    panel.read_with(vcx, |panel, _| assert!(panel.open_menu_kind().is_none()));
    assert!(mutations(&calls, "createWorkItem").is_empty());
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

/// A ticket item (Linear issue) that links two PRs (from Linear's GitHub
/// attachments or its Chats, system-created): one chip per PR in the
/// Outline and in Details, and ⌘↩ still opens the generic Start Chat (it is
/// not a PR review item: no sync record, not under the destination, not
/// titled `<repo>#<number>`).
#[gpui::test]
fn a_ticket_item_with_linked_prs_shows_each_and_starts_a_chat(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mut snapshot = sync_snapshot(Some(UI));
    let system = zeron_proto::WorkOutlineActor {
        kind: zeron_proto::WorkOutlineActorKind::System,
        device_id: "d".into(),
        chat_id: None,
        message_id: None,
    };
    let mut add = |provider: &str, kind: &str, external_id: &str, url: &str, at: i64| {
        let id = zeron_proto::external_reference_id(API, provider, kind, external_id);
        snapshot
            .external_references
            .push(zeron_proto::WorkItemExternalReferenceView {
                id: id.clone(),
                space_id: "s1".into(),
                work_item_id: API.into(),
                provider: provider.into(),
                reference_type: kind.into(),
                external_id: external_id.into(),
                url: url.into(),
                cached_title: None,
                created_by: Some(system.clone()),
                created_at: at,
            });
        let item = snapshot.items.iter_mut().find(|i| i.id == API).unwrap();
        item.external_reference_ids.push(id);
    };
    add(
        "linear",
        "issue",
        "ENG-2690",
        "https://linear.app/acme/issue/ENG-2690",
        1,
    );
    add(
        "github",
        "pullRequest",
        "acme/flagship#12",
        "https://github.com/acme/flagship/pull/12",
        4_000_000_000_000,
    );
    snapshot
        .items
        .iter_mut()
        .find(|i| i.id == API)
        .unwrap()
        .title = "ENG-2690 \u{00b7} API".into();
    let (overview, calls, vcx) = pr_overview(cx, dir.path(), snapshot, default_reply);
    let key = row_key::item("s1", API);
    assert!(row_bounds(vcx, format!("outline-pr-{key}")).is_some());
    assert!(
        row_bounds(vcx, format!("outline-pr-{key}-1")).is_some(),
        "the second PR's chip"
    );
    let asked: Vec<_> = calls_to(&calls, "PrStatus")
        .iter()
        .map(|p| format!("{}#{}", p["repo"].as_str().unwrap(), p["number"]))
        .collect();
    assert!(asked.contains(&"launch#42".to_string()) && asked.contains(&"flagship#12".to_string()));

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
        assert!(o.captured_pr_review.take().is_none(), "not a review");
        assert_eq!(
            o.captured_start_chat.take(),
            Some(("s1".to_string(), API.to_string()))
        );
    });

    open_details(&overview, vcx, API);
    draw(vcx);
    assert!(vcx.debug_bounds("work-item-pr-chip").is_some());
    assert!(vcx.debug_bounds("work-item-pr-chip-1").is_some());
}

/// The PR chip text of `pr` as the Outline would render it right now
/// (`#n` alone while there is nothing cached).
fn shown_pr_text(state: &Entity<AppState>, vcx: &mut gpui::VisualTestContext, number: u64) -> String {
    let pr = zeron_proto::PullRequestRef::new("acme", "launch", number);
    state.read_with(vcx, |state, _| match state.github.status(&pr) {
        Some(crate::github::store::PrEntry::Ready { status, .. }) => {
            crate::github::chip::chip_text(status)
        }
        _ => format!("#{number}"),
    })
}

/// Scripted `PrStatus` whose answer a test swaps between pulls; `None` = the
/// pull fails.
type PrAnswer = std::sync::Arc<std::sync::Mutex<Option<zeron_proto::PullRequestStatusSummary>>>;

fn pr_status_reply(answer: &PrAnswer) -> impl Fn(&str, &serde_json::Value) -> Result<serde_json::Value, zeron_rpc::RpcError> + Send + Sync + 'static {
    let answer = answer.clone();
    move |method, params| match method {
        "PrStatus" => match answer.lock().unwrap().clone() {
            Some(status) => Ok(serde_json::to_value(status).unwrap()),
            None => Err(zeron_rpc::RpcError::Failed("gh unavailable".into())),
        },
        _ => default_reply(method, params),
    }
}

/// A refresh that returns the same data leaves the chip text identical at
/// every step (no loading text while the pull is in flight), a failed pull
/// keeps the last good text, and a real change still shows.
#[gpui::test]
fn pr_chip_text_is_stable_across_refreshes_and_changes_with_the_data(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let answer: PrAnswer = std::sync::Arc::new(std::sync::Mutex::new(Some(gf::summary(
        "launch",
        42,
        "Add auth endpoints",
    ))));
    let (overview, _calls, vcx) = pr_overview(
        cx,
        dir.path(),
        sync_snapshot(Some(LAUNCH)),
        pr_status_reply(&answer),
    );
    let state = overview.read_with(vcx, |o, _| o.state.clone());
    vcx.run_until_parked();
    let pr = zeron_proto::PullRequestRef::new("acme", "launch", 42);
    let before = shown_pr_text(&state, vcx, 42);
    assert_eq!(
        before,
        "#42 \u{00b7} Open \u{00b7} \u{2713} \u{00b7} Review requested"
    );

    // The same data pulled twice: identical while in flight and after.
    for _ in 0..2 {
        vcx.update(|_, cx| crate::github::store::request_status(&state, &pr, true, cx));
        assert!(state.read_with(vcx, |s, _| s.github.is_loading(&pr)));
        assert_eq!(shown_pr_text(&state, vcx, 42), before, "in flight");
        vcx.run_until_parked();
        assert!(!state.read_with(vcx, |s, _| s.github.is_loading(&pr)));
        assert_eq!(shown_pr_text(&state, vcx, 42), before, "after the pull");
    }

    // A failed pull keeps the last good text.
    *answer.lock().unwrap() = None;
    vcx.update(|_, cx| crate::github::store::request_status(&state, &pr, true, cx));
    assert_eq!(shown_pr_text(&state, vcx, 42), before, "failing, in flight");
    vcx.run_until_parked();
    assert_eq!(shown_pr_text(&state, vcx, 42), before, "after a failed pull");

    // Real changes show.
    let mut changed = gf::summary("launch", 42, "Add auth endpoints");
    changed.ci_status = zeron_proto::PullRequestCiStatus::Failing;
    changed.state = zeron_proto::PullRequestState::Draft;
    *answer.lock().unwrap() = Some(changed);
    vcx.update(|_, cx| crate::github::store::request_status(&state, &pr, true, cx));
    vcx.run_until_parked();
    let after = shown_pr_text(&state, vcx, 42);
    assert_ne!(after, before);
    assert!(after.contains("Draft") && after.contains('\u{2715}'), "{after}");
}

/// A list row (search hit: no CI or review detail) never replaces a fully
/// fetched status, but does fill a gap, and the chip then fetches the full
/// status once.
#[gpui::test]
fn partial_list_rows_do_not_overwrite_a_full_pr_status(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let answer: PrAnswer = std::sync::Arc::new(std::sync::Mutex::new(Some(gf::summary(
        "launch",
        42,
        "Add auth endpoints",
    ))));
    let (overview, calls, vcx) = pr_overview(
        cx,
        dir.path(),
        sync_snapshot(Some(LAUNCH)),
        pr_status_reply(&answer),
    );
    let state = overview.read_with(vcx, |o, _| o.state.clone());
    vcx.run_until_parked();
    let full = shown_pr_text(&state, vcx, 42);

    let mut hit = gf::summary("launch", 42, "Add auth endpoints");
    hit.ci_status = zeron_proto::PullRequestCiStatus::None;
    hit.my_review_state = zeron_proto::PullRequestMyReviewState::None;
    state.update(vcx, |state, _| state.github.remember([&hit]));
    assert_eq!(shown_pr_text(&state, vcx, 42), full, "full status wins");

    // A PR with nothing cached takes the row, then the chip fetches it.
    state.update(vcx, |state, _| {
        state.github.remember([&gf::summary("launch", 7, "Fix login")])
    });
    let fetches = |calls: &PrCalls| calls_to(calls, "PrStatus").len();
    let before = fetches(&calls);
    let pr7 = zeron_proto::PullRequestRef::new("acme", "launch", 7);
    vcx.update(|_, cx| crate::github::store::request_status(&state, &pr7, false, cx));
    vcx.run_until_parked();
    assert_eq!(fetches(&calls), before + 1, "a partial row is fetched in full");
    vcx.update(|_, cx| crate::github::store::request_status(&state, &pr7, false, cx));
    vcx.run_until_parked();
    assert_eq!(fetches(&calls), before + 1, "and then cached");
}
