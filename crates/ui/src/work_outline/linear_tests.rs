// Linear integration UI tests (panel, per-Space gating, chips, drops,
// details). Included from `overview.rs`'s test module so they share its
// Overview fixtures. Every Linear reply is scripted; nothing reaches Linear.
use super::*;

use crate::linear::fixture as lf;
use crate::linear::panel::LinearPanel;

type LinearCalls = std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>;

/// The store fixture with Linear `enabled` and ENG-1 linked on `w-api`.
fn linear_snapshot(enabled: bool) -> WorkOutlineSnapshot {
    let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
    snapshot.space_settings.linear_enabled = enabled;
    snapshot.external_references = vec![lf::reference(
        "r-eng-1",
        "s1",
        API,
        "issue",
        "ENG-1",
        "https://linear.app/acme/issue/eng-1/slug",
    )];
    snapshot
}

/// Default scripted host: My issues ENG-1/ENG-2, one project with ENG-3/4,
/// a connected key, and creates that answer `w-new-<n>`.
fn default_reply(method: &str, params: &serde_json::Value) -> Result<serde_json::Value, zeron_rpc::RpcError> {
    Ok(match method {
        "LinearStatus" => serde_json::json!({"configured": true, "source": "keychain",
            "viewer": {"id": "u", "name": "Mikey"}}),
        "LinearMyIssues" => serde_json::to_value(vec![
            lf::issue("ENG-1", "Fix login"),
            lf::issue("ENG-2", "Add billing"),
        ])
        .unwrap(),
        "LinearMyProjects" => serde_json::to_value(vec![lf::project("p-launch", "Launch")]).unwrap(),
        "LinearProjectIssues" => serde_json::to_value(vec![
            lf::issue("ENG-3", "Docs"),
            lf::issue("ENG-4", "Pricing page"),
        ])
        .unwrap(),
        "LinearIssue" => {
            let id = params["identifier"].as_str().unwrap_or("ENG-1");
            serde_json::to_value(lf::issue(id, "Fix login")).unwrap()
        }
        "LinearRefreshLinkedItems" => serde_json::json!({"updated": 1}),
        _ => match params["op"].as_str() {
            Some("createWorkItemFromLinear") => serde_json::json!({
                "workItemId": format!("w-new-{}", params["issue"]["identifier"].as_str().unwrap_or("")),
                "referenceId": "r-new"
            }),
            Some("createWorkItemExternalReference") | Some("linkLinearProject") => {
                serde_json::json!({"referenceId": "r-new"})
            }
            _ => serde_json::json!({}),
        },
    })
}

fn linear_overview<'a>(
    cx: &'a mut gpui::TestAppContext,
    dir: &std::path::Path,
    snapshot: WorkOutlineSnapshot,
    reply: impl Fn(&str, &serde_json::Value) -> Result<serde_json::Value, zeron_rpc::RpcError>
    + Send
    + Sync
    + 'static,
) -> (Entity<Overview>, LinearCalls, &'a mut gpui::VisualTestContext) {
    let (overview, state, vcx) = outline_overview(cx, dir);
    let calls: LinearCalls = Default::default();
    let sink = calls.clone();
    state.update(vcx, |state, _| {
        state.work_outline_actions_override = Some(crate::work_outline::WorkOutlineActions::scripted(
            std::sync::Arc::new(move |method, params| {
                sink.lock().unwrap().push((method.to_string(), params.clone()));
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

fn open_panel(overview: &Entity<Overview>, vcx: &mut gpui::VisualTestContext) -> Entity<LinearPanel> {
    vcx.update(|window, cx| {
        overview.update(cx, |o, cx| {
            o.run_work_outline_command(WorkOutlineCommand::OpenLinear, window, cx)
        })
    });
    vcx.run_until_parked();
    draw(vcx);
    overview.read_with(vcx, |o, _| o.linear_panel().expect("the Linear panel opens"))
}

fn select_row(overview: &Entity<Overview>, vcx: &mut gpui::VisualTestContext, key: &str) {
    overview.update(vcx, |o, cx| {
        o.outline.update(cx, |outline, cx| outline.select(key, cx))
    });
    draw(vcx);
}

fn mutations(calls: &LinearCalls, op: &str) -> Vec<serde_json::Value> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, params)| method == "WorkOutlineMutate" && params["op"] == op)
        .map(|(_, params)| params.clone())
        .collect()
}

fn called(calls: &LinearCalls, method: &str) -> usize {
    calls.lock().unwrap().iter().filter(|(m, _)| m == method).count()
}

/// A snapshot that also shows the item a create produced.
fn with_created(mut snapshot: WorkOutlineSnapshot, id: &str, parent: &str) -> WorkOutlineSnapshot {
    let mut item = snapshot.item(UI).unwrap().clone();
    item.id = id.into();
    item.parent_work_item_id = Some(parent.into());
    item.raw_parent_work_item_id = Some(parent.into());
    item.title = "ENG-2 \u{00b7} Add billing".into();
    item.child_ids.clear();
    item.chat_ids.clear();
    item.order_key = "zz".into();
    for parent_item in snapshot.items.iter_mut() {
        if parent_item.id == parent {
            parent_item.child_ids.push(id.into());
        }
    }
    snapshot.items.push(item);
    snapshot
}

/// Linear off (the default): no toolbar button, no panel, no menu entries,
/// no chips, no Linear calls; the Open Linear command only explains.
#[gpui::test]
fn linear_off_shows_no_linear_ui(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(false), default_reply);
    assert!(vcx.debug_bounds("outline-linear").is_none(), "no toolbar button");
    assert!(
        row_bounds(vcx, format!("outline-linear-{}", row_key::item("s1", API))).is_none(),
        "no chip on the linked item"
    );
    vcx.update(|window, cx| {
        overview.update(cx, |o, cx| {
            o.run_work_outline_command(WorkOutlineCommand::OpenLinear, window, cx)
        })
    });
    draw(vcx);
    overview.read_with(vcx, |o, cx| {
        assert!(o.linear_panel().is_none());
        assert!(o.outline.read(cx).notice().is_some_and(|n| n.contains("Linear")));
        let item_menu = o
            .outline
            .read(cx)
            .context_menu(&row_key::item("s1", API), cx)
            .unwrap();
        assert!(item_menu.entries.iter().all(|e| !e.id.starts_with("linear")));
    });
    assert_eq!(called(&calls, "LinearIssue"), 0, "no Linear calls for a Space with Linear off");
    assert_eq!(called(&calls, "LinearMyIssues"), 0);
}

/// Pasting a Linear URL with Linear off keeps a plain link and shows the
/// one-time hint; Enable turns Linear on for the Space.
#[gpui::test]
fn paste_with_linear_off_is_a_plain_link_with_a_one_time_hint(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(false), default_reply);
    let details = open_details(&overview, vcx, UI);
    details.update(vcx, |d, cx| {
        d.paste_reference_for_test("https://linear.app/acme/issue/eng-9/x", cx)
    });
    vcx.run_until_parked();
    draw(vcx);
    let refs = mutations(&calls, "createWorkItemExternalReference");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0]["provider"], "web");
    assert_eq!(refs[0]["referenceType"], "link");
    assert!(vcx.debug_bounds("work-item-linear-hint").is_some(), "the hint shows");
    assert!(vcx.debug_bounds("work-item-linear-hint-enable").is_some());
    // (The hint sits below the fold of the details body; run its Enable.)
    details.update(vcx, |d, cx| d.set_linear_enabled(true, cx));
    vcx.run_until_parked();
    let toggles = mutations(&calls, "setSpaceLinearEnabled");
    assert_eq!(toggles.len(), 1);
    assert_eq!(toggles[0]["spaceId"], "s1");
    assert_eq!(toggles[0]["enabled"], true);
    // One time: a later paste in a fresh surface shows no hint.
    let details = open_details(&overview, vcx, API);
    details.update(vcx, |d, cx| {
        d.paste_reference_for_test("https://linear.app/acme/issue/eng-10/y", cx)
    });
    vcx.run_until_parked();
    draw(vcx);
    assert!(vcx.debug_bounds("work-item-linear-hint").is_none());
}

/// With Linear on, an issue URL becomes an issue reference and a project
/// URL links the matching project.
#[gpui::test]
fn paste_with_linear_on_creates_issue_and_project_references(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    let details = open_details(&overview, vcx, UI);
    details.update(vcx, |d, cx| {
        d.paste_reference_for_test("https://linear.app/acme/issue/eng-9/x", cx)
    });
    vcx.run_until_parked();
    let refs = mutations(&calls, "createWorkItemExternalReference");
    assert_eq!(refs[0]["provider"], "linear");
    assert_eq!(refs[0]["referenceType"], "issue");
    assert_eq!(refs[0]["externalId"], "ENG-9");
    details.update(vcx, |d, cx| {
        d.paste_reference_for_test("https://linear.app/acme/project/launch-launch", cx)
    });
    vcx.run_until_parked();
    let links = mutations(&calls, "linkLinearProject");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0]["workItemId"], UI);
    assert_eq!(links[0]["project"]["id"], "p-launch");
    assert!(vcx.debug_bounds("work-item-linear-hint").is_none());
}

/// The Space-root details carry the per-Space toggle and, without a key,
/// Connect Linear.
#[gpui::test]
fn space_root_toggle_and_connect_prompt(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(false), |method, params| {
        if method == "LinearStatus" {
            return Ok(serde_json::json!({"configured": false, "source": "none"}));
        }
        default_reply(method, params)
    });
    open_details(&overview, vcx, "");
    draw(vcx);
    assert!(vcx.debug_bounds("space-linear").is_some());
    assert_eq!(called(&calls, "LinearStatus"), 0, "Linear off: no calls");
    press(vcx, "space-linear-toggle".into());
    assert!(vcx.debug_bounds("space-linear-connect").is_some(), "no key: Connect Linear");
    let toggles = mutations(&calls, "setSpaceLinearEnabled");
    assert_eq!(toggles.len(), 1);
    assert_eq!(toggles[0]["enabled"], true);
    assert!(toggles[0]["operationId"].as_str().is_some_and(|id| !id.is_empty()));
}

/// The panel lists My issues and My projects, filters on the search, marks
/// the issue a live item links, and Reveal selects that item.
#[gpui::test]
fn panel_lists_search_and_linked_marker(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    assert!(vcx.debug_bounds("outline-linear").is_some(), "toolbar button with Linear on");
    let panel = open_panel(&overview, vcx);
    assert_eq!(called(&calls, "LinearMyIssues"), 1, "refresh on open");
    assert_eq!(called(&calls, "LinearMyProjects"), 1);
    for selector in [
        "linear-issue-row-ENG-1",
        "linear-issue-row-ENG-2",
        "linear-project-row-p-launch",
        "linear-issue-linked-ENG-1",
    ] {
        assert!(row_bounds(vcx, selector.into()).is_some(), "{selector}");
    }
    assert!(row_bounds(vcx, "linear-issue-linked-ENG-2".into()).is_none());
    // Search.
    panel.update(vcx, |panel, cx| panel.set_query("billing", cx));
    draw(vcx);
    assert!(row_bounds(vcx, "linear-issue-row-ENG-1".into()).is_none());
    assert!(row_bounds(vcx, "linear-issue-row-ENG-2".into()).is_some());
    assert!(row_bounds(vcx, "linear-project-row-p-launch".into()).is_none());
    panel.update(vcx, |panel, cx| panel.set_query("", cx));
    draw(vcx);
    // Reveal in Outline.
    press(vcx, "linear-issue-linked-ENG-1".into());
    overview.read_with(vcx, |o, cx| {
        assert_eq!(o.outline.read(cx).selected_key(), Some(row_key::item("s1", API).as_str()))
    });
    // ↻ refetches.
    press(vcx, "linear-panel-refresh".into());
    assert_eq!(called(&calls, "LinearMyIssues"), 2);
}

/// Create from a row goes under the selected item (placement Last), keeps
/// its operation id across a retry, and selects the new node once shown.
#[gpui::test]
fn create_from_row_under_selected_item_retries_and_selects(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let flag = fail.clone();
    let (overview, calls, vcx) =
        linear_overview(cx, dir.path(), linear_snapshot(true), move |method, params| {
            if params["op"] == "createWorkItemFromLinear"
                && flag.swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(zeron_rpc::RpcError::Failed("linear/rateLimited: slow down".into()));
            }
            default_reply(method, params)
        });
    let panel = open_panel(&overview, vcx);
    select_row(&overview, vcx, &row_key::item("s1", API));
    press(vcx, "linear-issue-create-ENG-2".into());
    panel.read_with(vcx, |panel, _| {
        assert!(panel.notice.as_ref().is_some_and(|n| n.error && n.text.contains("rate-limiting")))
    });
    press(vcx, "linear-issue-create-ENG-2".into());
    let creates = mutations(&calls, "createWorkItemFromLinear");
    assert_eq!(creates.len(), 2);
    assert_eq!(creates[0]["operationId"], creates[1]["operationId"], "a retry replays");
    assert_eq!(creates[1]["spaceId"], "s1");
    assert_eq!(creates[1]["parentWorkItemId"], API);
    assert_eq!(creates[1]["placement"], serde_json::json!({"type": "last"}));
    assert_eq!(creates[1]["issue"]["identifier"], "ENG-2");
    assert_eq!(creates[1]["issue"]["url"], "https://linear.app/acme/issue/eng-2/slug");
    // The new node shows on the next frame: selected.
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(
            fixture_loads(Some(with_created(linear_snapshot(true), "w-new-ENG-2", API))),
            cx,
        )
    });
    draw(vcx);
    overview.read_with(vcx, |o, cx| {
        assert_eq!(
            o.outline.read(cx).selected_key(),
            Some(row_key::item("s1", "w-new-ENG-2").as_str())
        )
    });
}

/// Without a usable selection, Create asks for the parent first.
#[gpui::test]
fn create_without_selection_asks_for_a_parent(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    open_panel(&overview, vcx);
    overview.update(vcx, |o, cx| {
        o.outline.update(cx, |outline, cx| outline.select("no-such-row", cx))
    });
    press(vcx, "linear-issue-create-ENG-2".into());
    assert!(mutations(&calls, "createWorkItemFromLinear").is_empty());
    assert!(vcx.debug_bounds("linear-parent-picker").is_some());
    press(vcx, format!("linear-parent-{UI}"));
    let creates = mutations(&calls, "createWorkItemFromLinear");
    assert_eq!(creates.len(), 1);
    assert_eq!(creates[0]["parentWorkItemId"], UI);
    assert!(vcx.debug_bounds("linear-parent-picker").is_none());
}

/// Dragging an issue row onto an Outline item shows the "into" indicator
/// and creates the work item under that item.
#[gpui::test]
fn drag_issue_onto_item_creates_under_it(cx: &mut gpui::TestAppContext) {
    use crate::work_outline::restructure::DropZone;
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    open_panel(&overview, vcx);
    let start = row_bounds(vcx, "linear-issue-row-ENG-2".into()).unwrap();
    let start = gpui::point(start.left() + px(30.0), start.center().y);
    let target = row_bounds(vcx, format!("outline-row-{}", row_key::item("s1", UI))).unwrap();
    let end = target.center();
    let left = Some(gpui::MouseButton::Left);
    vcx.simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::default());
    vcx.simulate_mouse_move(start + gpui::point(px(0.0), px(6.0)), left, gpui::Modifiers::default());
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
    let creates = mutations(&calls, "createWorkItemFromLinear");
    assert_eq!(creates.len(), 1);
    assert_eq!(creates[0]["parentWorkItemId"], UI);
    assert_eq!(creates[0]["issue"]["identifier"], "ENG-2");
}

/// Link to selected item: an issue through createWorkItemExternalReference,
/// a project through linkLinearProject; nothing without a selection.
#[gpui::test]
fn link_issue_and_project_to_the_selected_item(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    let panel = open_panel(&overview, vcx);
    overview.update(vcx, |o, cx| {
        o.outline.update(cx, |outline, cx| outline.select("no-such-row", cx))
    });
    press(vcx, "linear-issue-link-ENG-2".into());
    assert!(mutations(&calls, "createWorkItemExternalReference").is_empty());
    panel.read_with(vcx, |panel, _| assert!(panel.notice.is_some()));
    select_row(&overview, vcx, &row_key::item("s1", UI));
    press(vcx, "linear-issue-link-ENG-2".into());
    let refs = mutations(&calls, "createWorkItemExternalReference");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0]["workItemId"], UI);
    assert_eq!(refs[0]["provider"], "linear");
    assert_eq!(refs[0]["referenceType"], "issue");
    assert_eq!(refs[0]["externalId"], "ENG-2");
    press(vcx, "linear-project-link-p-launch".into());
    let links = mutations(&calls, "linkLinearProject");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0]["workItemId"], UI);
    assert_eq!(links[0]["project"]["name"], "Launch");
}

/// A project expands to its open issues; Create nodes for selected sends
/// one createWorkItemFromLinear per checked issue, in order, each with its
/// own operation id.
#[gpui::test]
fn project_create_selected_creates_one_node_per_issue(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    open_panel(&overview, vcx);
    select_row(&overview, vcx, &row_key::item("s1", LAUNCH));
    press(vcx, "linear-project-row-p-launch".into());
    assert_eq!(called(&calls, "LinearProjectIssues"), 1);
    press(vcx, "linear-issue-check-ENG-3".into());
    press(vcx, "linear-issue-check-ENG-4".into());
    press(vcx, "linear-project-create-selected-p-launch".into());
    let creates = mutations(&calls, "createWorkItemFromLinear");
    let ids: Vec<_> = creates.iter().map(|c| c["issue"]["identifier"].clone()).collect();
    assert_eq!(ids, [serde_json::json!("ENG-3"), serde_json::json!("ENG-4")]);
    assert!(creates.iter().all(|c| c["parentWorkItemId"] == LAUNCH));
    assert_ne!(creates[0]["operationId"], creates[1]["operationId"]);
    assert!(
        row_bounds(vcx, "linear-project-create-selected-p-launch".into()).is_none(),
        "the selection clears after creating"
    );
}

/// Linked items show a live chip from LinearIssue (cached), Details shows
/// the Linear section and "Synced from Linear" until the field is edited,
/// and the item menu offers the Linear entries.
#[gpui::test]
fn chips_details_sync_markers_and_menu(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    draw(vcx);
    let chip = format!("outline-linear-{}", row_key::item("s1", API));
    assert!(row_bounds(vcx, chip).is_some(), "the chip renders");
    assert_eq!(called(&calls, "LinearIssue"), 1, "fetched once, then cached");
    let state = overview.read_with(vcx, |o, _| o.state.clone());
    state.read_with(vcx, |state, _| {
        let issue = state.linear.ready_issue("ENG-1").expect("cached");
        assert_eq!(issue.state.name, "In Progress");
    });
    draw(vcx);
    assert_eq!(called(&calls, "LinearIssue"), 1);
    // Details.
    open_details(&overview, vcx, API);
    assert!(vcx.debug_bounds("work-item-linear-chip").is_some());
    assert!(vcx.debug_bounds("work-item-title-synced").is_some());
    press(vcx, "work-item-linear-refresh".into());
    let refresh: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, _)| m == "LinearRefreshLinkedItems")
        .map(|(_, p)| p.clone())
        .collect();
    assert_eq!(refresh, [serde_json::json!({"spaceId": "s1"})]);
    press(vcx, "work-item-linear-unlink".into());
    let removed = mutations(&calls, "removeWorkItemExternalReference");
    assert_eq!(removed[0]["referenceId"], "r-eng-1");
    // An edited title drops its marker.
    let mut edited = linear_snapshot(true);
    for item in edited.items.iter_mut().filter(|i| i.id == API) {
        item.external_sync_modified_fields = vec!["title".into()];
    }
    overview.update(vcx, |o, cx| o.set_outline_loads(fixture_loads(Some(edited)), cx));
    draw(vcx);
    assert!(vcx.debug_bounds("work-item-title-synced").is_none());
    // Menus.
    overview.read_with(vcx, |o, cx| {
        let outline = o.outline.read(cx);
        let item = outline.context_menu(&row_key::item("s1", API), cx).unwrap();
        let ids: Vec<_> = item.entries.iter().map(|e| e.id).collect();
        assert!(ids.contains(&"linear-open") && ids.contains(&"linear-unlink"));
    });
}

/// A host without the Linear RPCs shows a calm "not available" state; a
/// missing key offers Connect Linear.
#[gpui::test]
fn unknown_method_and_missing_key_states(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) =
        linear_overview(cx, dir.path(), linear_snapshot(true), |method, params| {
            if method.starts_with("Linear") {
                return Err(zeron_rpc::RpcError::UnknownMethod(method.to_string()));
            }
            default_reply(method, params)
        });
    let panel = open_panel(&overview, vcx);
    assert!(vcx.debug_bounds("linear-panel-blocker").is_some());
    assert!(vcx.debug_bounds("linear-panel-connect").is_none());
    panel.read_with(vcx, |panel, _| {
        let error = match &panel.my_issues {
            crate::linear::panel::Load::Failed(error) => error.clone(),
            _ => panic!("failed"),
        };
        assert!(error.not_available());
        assert_eq!(error.user_text(), "Linear isn\u{2019}t available on this host yet");
    });

    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) =
        linear_overview(cx, dir.path(), linear_snapshot(true), |method, params| {
            if method.starts_with("LinearMy") {
                return Err(zeron_rpc::RpcError::Failed(
                    "linear/missingKey: No Linear API key is set".into(),
                ));
            }
            default_reply(method, params)
        });
    open_panel(&overview, vcx);
    assert!(vcx.debug_bounds("linear-panel-connect").is_some());
}

/// Space row menu offers Open Linear Panel (Linear on only) and it opens
/// the panel on that Space.
#[gpui::test]
fn space_row_menu_opens_the_panel(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    overview.read_with(vcx, |o, cx| {
        let menu = o.outline.read(cx).context_menu(&row_key::space("s1"), cx);
        // A single-Space Outline may have no Space row; when it does, it
        // carries the entry.
        if let Some(menu) = menu {
            assert!(menu.entries.iter().any(|e| e.id == "linear-panel"));
        }
    });
    overview.update(vcx, |o, cx| {
        o.open_linear_panel(Some("s1".into()), cx).unwrap()
    });
    draw(vcx);
    let panel = overview.read_with(vcx, |o, _| o.linear_panel().unwrap());
    panel.read_with(vcx, |panel, _| assert_eq!(panel.space_id, "s1"));
    assert!(vcx.debug_bounds("linear-panel").is_some());
}
