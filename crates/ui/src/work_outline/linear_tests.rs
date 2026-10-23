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
    // The column's open/close tween snaps, so rows are hit-testable at once
    // (`linear_column_tween_respects_reduced_motion` covers the tween).
    vcx.update(|_, cx| crate::motion::set_reduced_motion(cx, true));
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

/// A Linear row is an icon cluster like a PR row's (assignee, needs, status):
/// the state dot and status icon share one fixed-width slot, and the totals
/// text, chat count and references icon are folded into the tooltip. The
/// full chip stays in Details; plain items keep their text.
#[gpui::test]
fn linear_rows_show_a_compact_icon_cluster(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) =
        linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    vcx.run_until_parked();
    draw(vcx);
    let key = row_key::item("s1", API);
    let slot = row_bounds(vcx, format!("outline-linear-{key}")).expect("the Linear slot renders");
    assert_eq!(f32::from(slot.size.width), 28.0, "fixed width");
    for selector in [
        format!("outline-linear-slots-{key}"),
        format!("outline-linear-assignee-{key}"),
        format!("outline-linear-slot-needs-{key}"),
        format!("outline-linear-slot-status-{key}"),
    ] {
        assert!(row_bounds(vcx, selector.clone()).is_some(), "{selector} renders");
    }
    // No PR cluster on a Linear-only row, and none of the text it replaces.
    assert!(row_bounds(vcx, format!("outline-pr-slots-{key}")).is_none());
    for gone in ["outline-totals", "outline-chats", "outline-refs"] {
        assert!(
            row_bounds(vcx, format!("{gone}-{key}")).is_none(),
            "{gone} is folded into the tooltip"
        );
    }
    // The row has them to show: chats, references, and the parent's totals.
    overview.read_with(vcx, |o, cx| {
        let outline = o.outline.read(cx);
        let item = outline.row_for_test(&key).unwrap().item().unwrap();
        assert!(item.chat_count > 0 && item.has_references);
    });
    // The row's state: loaded issue is In Progress (started).
    let state = overview.read_with(vcx, |o, _| o.state.clone());
    state.read_with(vcx, |state, _| {
        let issue = state.linear.ready_issue("ENG-1").expect("cached");
        assert_eq!(
            crate::work_outline::outline_view::linear_outline::linear_status_icon(
                issue.state.state_type
            ),
            crate::icons::CLOCK_CIRCLE
        );
    });
    // Plain items (no issue) are unchanged: totals and chat count stay, and
    // there is no cluster.
    let plain = row_key::item("s1", LAUNCH);
    assert!(row_bounds(vcx, format!("outline-totals-{plain}")).is_some());
    assert!(row_bounds(vcx, format!("outline-linear-slots-{plain}")).is_none());
    open_details(&overview, vcx, API);
    assert!(vcx.debug_bounds("work-item-linear-chip").is_some());
}

/// Narrow Outline: the state dot drops first, then the assignee; the needs
/// slot and the status icon never drop.
#[gpui::test]
fn narrow_widths_drop_the_dot_then_the_assignee(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) =
        linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    vcx.run_until_parked();
    draw(vcx);
    let key = row_key::item("s1", API);
    let seen = |vcx: &mut gpui::VisualTestContext| {
        (
            row_bounds(vcx, format!("outline-linear-slot-assignee-{key}")).is_some(),
            row_bounds(vcx, format!("outline-linear-slot-needs-{key}")).is_some(),
            row_bounds(vcx, format!("outline-linear-{key}"))
                .map(|bounds| f32::from(bounds.size.width)),
        )
    };
    let mut states = Vec::new();
    let mut width = 700.0;
    while width >= 120.0 {
        overview.update(vcx, |o, cx| {
            o.outline
                .update(cx, |outline, cx| outline.set_row_width_for_test(Some(width), cx))
        });
        draw(vcx);
        let state = seen(vcx);
        assert!(state.1 && state.2.is_some(), "needs and status stay at {width}");
        if states.last() != Some(&state) {
            states.push(state);
        }
        width -= 4.0;
    }
    // (assignee, needs, status width): the dot goes with 28 → 17, then the
    // assignee.
    assert_eq!(
        states,
        [
            (true, true, Some(28.0)),
            (true, true, Some(17.0)),
            (false, true, Some(17.0)),
        ]
    );
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

fn resize(vcx: &mut gpui::VisualTestContext, width: f32) {
    vcx.simulate_resize(gpui::size(px(width), px(900.0)));
    draw(vcx);
    draw(vcx);
}

fn open_chat_panel(overview: &Entity<Overview>, vcx: &mut gpui::VisualTestContext) {
    overview.update(vcx, |o, cx| {
        o.state.update(cx, |state, _| state.selected_chat = Some(CHAT_API.into()));
        o.chat_panel_open = true;
    });
    draw(vcx);
}

/// The Linear panel is its own left column: it renders left of the
/// Outline body while the chat panel stays up on the right, and Work Item
/// Details / Required Actions take the right slot without closing it.
#[gpui::test]
fn linear_column_sits_left_of_the_body_beside_the_chat_panel(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    resize(vcx, 1800.0);
    open_chat_panel(&overview, vcx);
    open_panel(&overview, vcx);
    let column = vcx.debug_bounds("linear-column").expect("the Linear column renders");
    let chat = vcx.debug_bounds("overview-chat-panel").expect("the chat panel stays open");
    let row = row_bounds(vcx, format!("outline-row-{}", row_key::item("s1", UI))).unwrap();
    assert!(column.right() <= row.left(), "{column:?} left of {row:?}");
    assert!(row.right() <= chat.left(), "{row:?} left of {chat:?}");
    assert_eq!(column.size.width, px(crate::linear::prefs::PANEL_DEFAULT_W));
    overview.read_with(vcx, |o, cx| {
        assert!(o.chat_panel_open);
        assert_eq!(o.shown_panel_chat(cx).as_deref(), Some(CHAT_API));
    });
    // Details take the right slot; the Linear column stays.
    overview.update(vcx, |o, cx| o.open_work_item_details("s1".into(), API.into(), cx));
    draw(vcx);
    assert!(vcx.debug_bounds("work-item-details").is_some());
    assert!(vcx.debug_bounds("linear-panel").is_some());
    overview.update(vcx, |o, cx| {
        let details = o.work_item_details.clone().unwrap();
        details.update(cx, |d, cx| d.request_close(cx));
    });
    draw(vcx);
    assert!(vcx.debug_bounds("overview-chat-panel").is_some());
    assert!(vcx.debug_bounds("linear-panel").is_some());
    // Escape with focus outside the panel (the tree) leaves it open...
    vcx.update(|window, cx| {
        let handle = gpui::Focusable::focus_handle(overview.read(cx).outline.read(cx), cx);
        window.focus(&handle, cx)
    });
    vcx.simulate_keystrokes("escape");
    draw(vcx);
    assert!(vcx.debug_bounds("linear-panel").is_some());
    // ...and inside it (the search the toggle focused) closes it.
    let panel = overview.read_with(vcx, |o, _| o.linear_panel().unwrap());
    vcx.update(|window, cx| panel.update(cx, |p, cx| p.focus_search(window, cx)));
    vcx.simulate_keystrokes("escape");
    draw(vcx);
    assert!(vcx.debug_bounds("linear-panel").is_none());
    assert!(vcx.debug_bounds("overview-chat-panel").is_some(), "the chat panel stays");
}

/// Too narrow for body + Linear + chat panel: the column shrinks (the body
/// keeps its minimum), then at its minimum it overlays the body instead.
#[gpui::test]
fn narrow_window_shrinks_then_overlays_the_linear_column(cx: &mut gpui::TestAppContext) {
    use crate::linear::prefs::{PANEL_DEFAULT_W, PANEL_MIN_W};
    use crate::overview::linear_host::OVERVIEW_BODY_MIN_W;
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    open_chat_panel(&overview, vcx);
    open_panel(&overview, vcx);
    let chat_w = f32::from(vcx.debug_bounds("overview-chat-panel").unwrap().size.width);
    // Room for 300px of column.
    resize(vcx, chat_w + OVERVIEW_BODY_MIN_W + 300.0);
    let column = vcx.debug_bounds("linear-column").unwrap();
    assert_eq!(column.size.width, px(300.0));
    let row = row_bounds(vcx, format!("outline-row-{}", row_key::item("s1", UI))).unwrap();
    assert!(column.right() <= row.left(), "still inline");
    // Narrower than body minimum + column minimum: overlay at the minimum.
    resize(vcx, chat_w + OVERVIEW_BODY_MIN_W + PANEL_MIN_W - 80.0);
    let column = vcx.debug_bounds("linear-column").unwrap();
    assert_eq!(column.size.width, px(PANEL_MIN_W));
    let row = row_bounds(vcx, format!("outline-row-{}", row_key::item("s1", UI))).unwrap();
    assert!(row.left() < column.right(), "the column floats over the body");
    assert!(
        f32::from(vcx.debug_bounds("overview-chat-panel").unwrap().left()) > PANEL_MIN_W,
        "the body keeps its width instead of being squeezed"
    );
    assert!(vcx.debug_bounds("linear-column-resize").is_none(), "no grabber while overlaid");
    // Wide again: back inline at the preferred width.
    resize(vcx, 1800.0);
    assert_eq!(vcx.debug_bounds("linear-column").unwrap().size.width, px(PANEL_DEFAULT_W));
}

/// Dragging the column's edge resizes it within its bounds; width and open
/// state survive a fresh Overview on the same device (data dir).
#[gpui::test]
fn linear_column_width_and_open_state_persist(cx: &mut gpui::TestAppContext) {
    use crate::linear::prefs::{PANEL_DEFAULT_W, PANEL_MAX_W};
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    resize(vcx, 1800.0);
    open_panel(&overview, vcx);
    let handle = vcx.debug_bounds("linear-column-resize").unwrap().center();
    let left = Some(gpui::MouseButton::Left);
    vcx.simulate_mouse_down(handle, gpui::MouseButton::Left, gpui::Modifiers::default());
    vcx.simulate_mouse_move(handle + gpui::point(px(10.0), px(0.0)), left, gpui::Modifiers::default());
    vcx.simulate_mouse_move(handle + gpui::point(px(60.0), px(0.0)), left, gpui::Modifiers::default());
    vcx.simulate_mouse_up(handle + gpui::point(px(60.0), px(0.0)), gpui::MouseButton::Left, gpui::Modifiers::default());
    draw(vcx);
    let width = f32::from(vcx.debug_bounds("linear-column").unwrap().size.width);
    assert_eq!(width, PANEL_DEFAULT_W + 60.0);
    // Past the maximum it clamps.
    let handle = vcx.debug_bounds("linear-column-resize").unwrap().center();
    vcx.simulate_mouse_down(handle, gpui::MouseButton::Left, gpui::Modifiers::default());
    vcx.simulate_mouse_move(handle + gpui::point(px(10.0), px(0.0)), left, gpui::Modifiers::default());
    vcx.simulate_mouse_move(handle + gpui::point(px(900.0), px(0.0)), left, gpui::Modifiers::default());
    vcx.simulate_mouse_up(handle + gpui::point(px(900.0), px(0.0)), gpui::MouseButton::Left, gpui::Modifiers::default());
    draw(vcx);
    assert_eq!(vcx.debug_bounds("linear-column").unwrap().size.width, px(PANEL_MAX_W));
    // Back to a known width.
    overview.update(vcx, |o, cx| {
        o.linear_column.width = PANEL_DEFAULT_W + 60.0;
        o.persist_linear_width(cx);
    });

    // A fresh Overview on the same data dir: open, at that width.
    let (fresh, _calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    resize(vcx, 1800.0);
    fresh.read_with(vcx, |o, _| {
        assert!(o.linear_panel().is_some(), "restored open");
        assert_eq!(o.linear_column_state().width, PANEL_DEFAULT_W + 60.0);
    });
    assert_eq!(
        vcx.debug_bounds("linear-column").unwrap().size.width,
        px(PANEL_DEFAULT_W + 60.0)
    );
    // Closing persists too.
    fresh.update(vcx, |o, cx| o.close_linear_panel(false, cx));
    draw(vcx);
    let (again, _calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    again.read_with(vcx, |o, _| assert!(o.linear_panel().is_none(), "restored closed"));
    assert!(vcx.debug_bounds("linear-column").is_none());
}

/// Persisted open but no Space with Linear on: nothing renders and the
/// toggle only explains; Linear coming back on shows the column again.
#[gpui::test]
fn linear_column_hides_while_no_space_has_linear_on(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    open_panel(&overview, vcx);
    overview.update(vcx, |o, cx| o.set_outline_loads(fixture_loads(Some(linear_snapshot(false))), cx));
    draw(vcx);
    assert!(vcx.debug_bounds("linear-column").is_none());
    let before = called(&calls, "LinearMyIssues");
    vcx.update(|window, cx| {
        overview.update(cx, |o, cx| {
            o.run_work_outline_command(WorkOutlineCommand::OpenLinear, window, cx)
        })
    });
    draw(vcx);
    overview.read_with(vcx, |o, cx| {
        assert!(o.linear_panel().is_none());
        assert_eq!(o.outline.read(cx).notice(), Some(crate::linear::LINEAR_OFF_NOTICE));
    });
    assert_eq!(called(&calls, "LinearMyIssues"), before, "no Linear calls");
    overview.update(vcx, |o, cx| o.set_outline_loads(fixture_loads(Some(linear_snapshot(true))), cx));
    draw(vcx);
    assert!(vcx.debug_bounds("linear-column").is_some(), "the saved open state returns");
}

/// Open and close tween the column's width unless reduced motion is on,
/// when both snap.
#[gpui::test]
fn linear_column_tween_respects_reduced_motion(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    vcx.update(|_, cx| crate::motion::set_reduced_motion(cx, false));
    overview.update(vcx, |o, cx| o.open_linear_panel(None, cx).unwrap());
    overview.read_with(vcx, |o, _| assert!(o.linear_column_state().tween.is_some()));
    overview.update(vcx, |o, cx| o.close_linear_panel(false, cx));
    overview.read_with(vcx, |o, _| assert!(o.linear_column_state().tween.is_some()));
    vcx.update(|_, cx| crate::motion::set_reduced_motion(cx, true));
    overview.update(vcx, |o, cx| o.open_linear_panel(None, cx).unwrap());
    overview.read_with(vcx, |o, _| assert!(o.linear_column_state().tween.is_none()));
    draw(vcx);
    assert!(vcx.debug_bounds("linear-column").is_some());
    overview.update(vcx, |o, cx| o.close_linear_panel(false, cx));
    overview.read_with(vcx, |o, _| assert!(o.linear_column_state().tween.is_none()));
    draw(vcx);
    assert!(vcx.debug_bounds("linear-column").is_none(), "gone at once");
}

/// "My issues" / "My projects" headers show their counts and fold by click
/// and by ←/→ while focused (keys stay in the panel); folds and expanded
/// projects survive a fresh Overview.
#[gpui::test]
fn sections_fold_by_click_and_arrows_and_persist_with_expanded_projects(
    cx: &mut gpui::TestAppContext,
) {
    use crate::linear::prefs::PanelSection;
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    resize(vcx, 1800.0);
    let panel = open_panel(&overview, vcx);
    panel.read_with(vcx, |p, cx| {
        assert_eq!(p.section_count(PanelSection::MyIssues, cx), Some(2));
        assert_eq!(p.section_count(PanelSection::MyProjects, cx), Some(1));
    });
    panel.update(vcx, |p, cx| p.set_query("billing", cx));
    panel.read_with(vcx, |p, cx| {
        assert_eq!(p.section_count(PanelSection::MyIssues, cx), Some(1), "counts follow search");
        assert_eq!(p.section_count(PanelSection::MyProjects, cx), Some(0));
    });
    panel.update(vcx, |p, cx| p.set_query("", cx));
    draw(vcx);
    select_row(&overview, vcx, &row_key::item("s1", UI));

    // Click folds (and focuses the header).
    press(vcx, "linear-section-my-issues".into());
    assert!(row_bounds(vcx, "linear-issue-row-ENG-1".into()).is_none(), "folded");
    assert!(vcx.debug_bounds("linear-section-my-issues").is_some(), "the header stays");
    assert!(vcx.debug_bounds("linear-project-row-p-launch").is_some(), "projects unaffected");
    let focused = vcx.update(|window, cx| {
        panel.read(cx).section_focus_handle(PanelSection::MyIssues).is_focused(window)
    });
    assert!(focused, "the header has focus");
    // → unfolds, ← folds; the Outline selection never moves.
    vcx.simulate_keystrokes("right");
    draw(vcx);
    assert!(row_bounds(vcx, "linear-issue-row-ENG-1".into()).is_some());
    vcx.simulate_keystrokes("left");
    draw(vcx);
    assert!(row_bounds(vcx, "linear-issue-row-ENG-1".into()).is_none());
    overview.read_with(vcx, |o, cx| {
        assert_eq!(o.outline.read(cx).selected_key(), Some(row_key::item("s1", UI).as_str()))
    });
    // Fold projects by keyboard too, then unfold and expand Launch.
    vcx.update(|window, cx| {
        let handle = panel.read(cx).section_focus_handle(PanelSection::MyProjects);
        window.focus(&handle, cx)
    });
    vcx.simulate_keystrokes("left");
    draw(vcx);
    assert!(vcx.debug_bounds("linear-project-row-p-launch").is_none());
    vcx.simulate_keystrokes("right");
    draw(vcx);
    press(vcx, "linear-project-row-p-launch".into());
    assert!(row_bounds(vcx, "linear-issue-row-ENG-3".into()).is_some(), "expanded");
    assert_eq!(called(&calls, "LinearProjectIssues"), 1);

    // A fresh Overview: My issues still folded, Launch still expanded (and
    // its issues fetched on open).
    let (fresh, calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    resize(vcx, 1800.0);
    let panel = fresh.read_with(vcx, |o, _| o.linear_panel().expect("restored open"));
    panel.read_with(vcx, |p, _| {
        assert!(p.collapsed.contains(&PanelSection::MyIssues));
        assert!(!p.collapsed.contains(&PanelSection::MyProjects));
        assert!(p.expanded.contains("p-launch"));
    });
    assert_eq!(called(&calls, "LinearProjectIssues"), 1);
    assert!(row_bounds(vcx, "linear-issue-row-ENG-1".into()).is_none());
    assert!(row_bounds(vcx, "linear-issue-row-ENG-3".into()).is_some());
    // Collapsing the project is remembered as well.
    press(vcx, "linear-project-row-p-launch".into());
    let state = crate::linear::prefs::load_panel_state(dir.path());
    assert!(state.expanded_projects.is_empty());
    assert!(state.collapsed.contains(&PanelSection::MyIssues));
}

/// Dragging from the left column onto an Outline row still creates the
/// work item with the chat panel open on the right.
#[gpui::test]
fn drag_from_the_left_column_creates_beside_the_chat_panel(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    resize(vcx, 1800.0);
    open_chat_panel(&overview, vcx);
    open_panel(&overview, vcx);
    let start = row_bounds(vcx, "linear-issue-row-ENG-2".into()).unwrap();
    let target = row_bounds(vcx, format!("outline-row-{}", row_key::item("s1", API))).unwrap();
    assert!(start.right() <= target.left(), "dragging rightward from the left column");
    let start = gpui::point(start.left() + px(30.0), start.center().y);
    let end = target.center();
    let left = Some(gpui::MouseButton::Left);
    vcx.simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::default());
    vcx.simulate_mouse_move(start + gpui::point(px(6.0), px(0.0)), left, gpui::Modifiers::default());
    vcx.simulate_mouse_move(end, left, gpui::Modifiers::default());
    vcx.simulate_mouse_move(end, left, gpui::Modifiers::default());
    vcx.simulate_mouse_up(end, gpui::MouseButton::Left, gpui::Modifiers::default());
    vcx.run_until_parked();
    draw(vcx);
    let creates = mutations(&calls, "createWorkItemFromLinear");
    assert_eq!(creates.len(), 1);
    assert_eq!(creates[0]["parentWorkItemId"], API);
    assert!(vcx.debug_bounds("overview-chat-panel").is_some());
}

/// `linear_snapshot(true)` with `UI` renamed and linked to the Launch
/// project (the fixture issues' project) by `external_id` / `url`.
fn project_linked_snapshot(external_id: &str, url: &str) -> WorkOutlineSnapshot {
    let mut snapshot = linear_snapshot(true);
    for item in snapshot.items.iter_mut().filter(|item| item.id == UI) {
        item.title = "ReceiveAI UI upgrades".into();
    }
    let mut reference = lf::reference("r-ui-project", "s1", UI, "project", external_id, url);
    reference.cached_title = Some("Launch".into());
    reference.created_at = 5;
    snapshot.external_references.push(reference);
    snapshot
}

fn creates_parent(calls: &LinearCalls) -> Vec<(String, String)> {
    mutations(calls, "createWorkItemFromLinear")
        .iter()
        .map(|c| {
            (
                c["issue"]["identifier"].as_str().unwrap_or("").to_string(),
                c["parentWorkItemId"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

/// An issue whose project a live item links goes under that item whatever
/// the Outline selection, the action label names it, and double-click / ↩
/// on the row do the same as Create.
#[gpui::test]
fn destination_rule_creates_under_the_project_linked_item(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = project_linked_snapshot("p-launch", "https://linear.app/acme/project/launch-abc123");
    let (overview, calls, vcx) = linear_overview(cx, dir.path(), snapshot, default_reply);
    resize(vcx, 1800.0);
    let panel = open_panel(&overview, vcx);
    select_row(&overview, vcx, &row_key::item("s1", API));
    let issue = lf::issue("ENG-2", "Add billing");
    panel.read_with(vcx, |p, _| {
        assert_eq!(p.create_action_label(&issue).as_ref(), "Create under ReceiveAI UI upgrades");
    });
    press(vcx, "linear-issue-create-ENG-2".into());
    assert_eq!(creates_parent(&calls), [("ENG-2".to_string(), UI.to_string())]);
    assert!(vcx.debug_bounds("linear-parent-picker").is_none());
    // Double-click on the row.
    let check = row_bounds(vcx, "linear-issue-check-ENG-1".into()).unwrap();
    let at = gpui::point(check.right() + px(20.0), check.center().y);
    for click_count in [1, 2] {
        vcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: at,
            click_count,
            ..Default::default()
        });
        vcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: at,
            click_count,
            ..Default::default()
        });
    }
    vcx.run_until_parked();
    draw(vcx);
    assert_eq!(creates_parent(&calls).last(), Some(&("ENG-1".to_string(), UI.to_string())));
    // ↩ on a focused project row.
    press(vcx, "linear-project-row-p-launch".into());
    let focus = panel.read_with(vcx, |p, _| p.row_focus_handle("p-launch", "ENG-3").unwrap());
    vcx.update(|window, cx| window.focus(&focus, cx));
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(creates_parent(&calls).last(), Some(&("ENG-3".to_string(), UI.to_string())));
}

/// Several items link the project: the most recently linked one wins; an
/// issue of an unlinked project keeps the selection rule.
#[gpui::test]
fn destination_rule_prefers_the_newest_link_and_falls_back_to_selection(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let mut snapshot = project_linked_snapshot("p-launch", "https://linear.app/acme/project/launch-abc123");
    let mut newer = lf::reference("r-house-project", "s1", HOUSEKEEPING, "project", "p-launch", "");
    newer.created_at = 9;
    snapshot.external_references.push(newer);
    let (overview, calls, vcx) =
        linear_overview(cx, dir.path(), snapshot, |method, params| {
            if method == "LinearMyIssues" {
                let mut loose = lf::issue("ENG-8", "No project");
                loose.project = None;
                return Ok(serde_json::to_value(vec![lf::issue("ENG-2", "Add billing"), loose]).unwrap());
            }
            default_reply(method, params)
        });
    let panel = open_panel(&overview, vcx);
    select_row(&overview, vcx, &row_key::item("s1", API));
    panel.read_with(vcx, |p, _| {
        assert_eq!(
            p.create_action_label(&lf::issue("ENG-2", "x")).as_ref(),
            "Create under Housekeeping"
        );
        let mut loose = lf::issue("ENG-8", "No project");
        loose.project = None;
        assert_eq!(p.create_action_label(&loose).as_ref(), "Create");
    });
    press(vcx, "linear-issue-create-ENG-2".into());
    // (A create selects its parent until the new node shows.)
    select_row(&overview, vcx, &row_key::item("s1", API));
    press(vcx, "linear-issue-create-ENG-8".into());
    assert_eq!(
        creates_parent(&calls),
        [
            ("ENG-2".to_string(), HOUSEKEEPING.to_string()),
            ("ENG-8".to_string(), API.to_string()),
        ]
    );
}

/// My issues has checkboxes too; Create nodes for selected routes each
/// checked issue by the destination rule and names a shared destination.
#[gpui::test]
fn my_issues_create_selected_uses_the_destination_rule(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = project_linked_snapshot("p-launch", "");
    let (overview, calls, vcx) = linear_overview(cx, dir.path(), snapshot, default_reply);
    let panel = open_panel(&overview, vcx);
    select_row(&overview, vcx, &row_key::item("s1", API));
    press(vcx, "linear-issue-check-ENG-1".into());
    press(vcx, "linear-issue-check-ENG-2".into());
    panel.read_with(vcx, |p, _| {
        assert_eq!(
            p.create_selected_label(crate::linear::panel::MY_ISSUES_LIST).as_ref(),
            "Create under ReceiveAI UI upgrades"
        );
    });
    press(vcx, "linear-my-issues-create-selected".into());
    assert_eq!(
        creates_parent(&calls),
        [("ENG-1".to_string(), UI.to_string()), ("ENG-2".to_string(), UI.to_string())]
    );
    assert!(row_bounds(vcx, "linear-my-issues-create-selected".into()).is_none());
}

/// A project link made by pasting a URL keeps the slug: it still matches
/// the issue's project by the slug id, and once My projects resolves it the
/// chip offers Fix link (link by the real id, then remove the slug link).
#[gpui::test]
fn slug_project_link_matches_and_fix_link_repairs_it(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let url = "https://linear.app/acme/project/launch-abc123";
    let snapshot = project_linked_snapshot("launch-abc123", url);
    let (overview, calls, vcx) =
        linear_overview(cx, dir.path(), snapshot, move |method, params| {
            if method == "LinearMyProjects" {
                let mut project = lf::project("p-launch", "Launch");
                project.url = url.into();
                return Ok(serde_json::to_value(vec![project]).unwrap());
            }
            default_reply(method, params)
        });
    let fix = format!("outline-linear-fix-{}", row_key::item("s1", UI));
    assert!(row_bounds(vcx, fix.clone()).is_none(), "nothing to fix before My projects loads");
    open_panel(&overview, vcx);
    select_row(&overview, vcx, &row_key::item("s1", API));
    press(vcx, "linear-issue-create-ENG-2".into());
    assert_eq!(creates_parent(&calls), [("ENG-2".to_string(), UI.to_string())]);
    press(vcx, fix);
    let links = mutations(&calls, "linkLinearProject");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0]["workItemId"], UI);
    assert_eq!(links[0]["project"]["id"], "p-launch");
    let removed = mutations(&calls, "removeWorkItemExternalReference");
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0]["referenceId"], "r-ui-project");
    let order: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(_, p)| p["op"].as_str().map(str::to_string))
        .filter(|op| op == "linkLinearProject" || op == "removeWorkItemExternalReference")
        .collect();
    assert_eq!(order, ["linkLinearProject", "removeWorkItemExternalReference"]);
    // Details shows it too.
    open_details(&overview, vcx, UI);
    assert!(vcx.debug_bounds("work-item-linear-fix-link").is_some());
}

/// Project issues for the "Add issues from…" tests: ENG-3 (linked on
/// `API` by `picker_snapshot`), ENG-4, ENG-5.
fn picker_reply(method: &str, params: &serde_json::Value) -> Result<serde_json::Value, zeron_rpc::RpcError> {
    if method == "LinearProjectIssues" {
        return Ok(serde_json::to_value(vec![
            lf::issue("ENG-3", "Docs"),
            lf::issue("ENG-4", "Pricing page"),
            lf::issue("ENG-5", "Checkout"),
        ])
        .unwrap());
    }
    default_reply(method, params)
}

fn picker_snapshot() -> WorkOutlineSnapshot {
    let mut snapshot = project_linked_snapshot("p-launch", "https://linear.app/acme/project/launch-abc123");
    snapshot.external_references.push(lf::reference(
        "r-eng-3",
        "s1",
        API,
        "issue",
        "ENG-3",
        "https://linear.app/acme/issue/eng-3/slug",
    ));
    snapshot
}

/// "Add issues from <project>…" (menu entry and Details button on a
/// project-linked item) opens a picker without the Linear panel: it lists
/// the project's open issues, shows already-added ones checked + disabled
/// with Reveal, filters on search, Select all checks the rest, and Create
/// adds one node per checked issue under the item, then selects the first.
#[gpui::test]
fn add_issues_from_project_lists_marks_added_and_creates_under_the_item(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = linear_overview(cx, dir.path(), picker_snapshot(), picker_reply);
    resize(vcx, 1800.0);
    overview.read_with(vcx, |o, cx| {
        let menu = o.outline.read(cx).context_menu(&row_key::item("s1", UI), cx).unwrap();
        let entry = menu.entries.iter().find(|e| e.id == "linear-add-issues").expect("menu entry");
        assert_eq!(entry.label, "Add Issues from Launch\u{2026}");
    });
    open_details(&overview, vcx, UI);
    press(vcx, "work-item-linear-add-issues".into());
    let picker = overview.read_with(vcx, |o, _| {
        assert!(o.linear_panel().is_none(), "the Linear panel stays closed");
        o.project_issue_picker().expect("the picker opens")
    });
    let asked: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, _)| m == "LinearProjectIssues")
        .map(|(_, p)| p["projectId"].clone())
        .collect();
    assert_eq!(asked, [serde_json::json!("p-launch")]);
    for selector in [
        "linear-add-issues",
        "linear-add-issues-row-ENG-3",
        "linear-add-issues-reveal-ENG-3",
        "linear-add-issues-row-ENG-4",
        "linear-add-issues-row-ENG-5",
    ] {
        assert!(row_bounds(vcx, selector.into()).is_some(), "{selector}");
    }
    assert!(row_bounds(vcx, "linear-add-issues-reveal-ENG-4".into()).is_none());
    // An added issue can't be toggled.
    press(vcx, "linear-add-issues-check-ENG-3".into());
    picker.read_with(vcx, |p, _| assert!(p.checked.is_empty()));
    // Search narrows; Select all checks only shown, unadded issues.
    picker.update(vcx, |p, cx| p.set_query("pricing", cx));
    draw(vcx);
    assert!(row_bounds(vcx, "linear-add-issues-row-ENG-5".into()).is_none());
    press(vcx, "linear-add-issues-select-all".into());
    picker.read_with(vcx, |p, _| {
        assert_eq!(p.checked.iter().cloned().collect::<Vec<_>>(), ["ENG-4"])
    });
    picker.update(vcx, |p, cx| p.set_query("", cx));
    draw(vcx);
    press(vcx, "linear-add-issues-select-all".into());
    picker.read_with(vcx, |p, _| {
        assert_eq!(p.checked.iter().cloned().collect::<Vec<_>>(), ["ENG-4", "ENG-5"])
    });
    // Reveal an added issue.
    press(vcx, "linear-add-issues-reveal-ENG-3".into());
    overview.read_with(vcx, |o, cx| {
        assert_eq!(o.outline.read(cx).selected_key(), Some(row_key::item("s1", API).as_str()))
    });
    // Create.
    press(vcx, "linear-add-issues-create".into());
    let creates = mutations(&calls, "createWorkItemFromLinear");
    assert_eq!(
        creates_parent(&calls),
        [("ENG-4".to_string(), UI.to_string()), ("ENG-5".to_string(), UI.to_string())]
    );
    assert!(creates.iter().all(|c| c["placement"] == serde_json::json!({"type": "last"})));
    assert!(creates.iter().all(|c| c["spaceId"] == "s1"));
    assert_ne!(creates[0]["operationId"], creates[1]["operationId"]);
    overview.read_with(vcx, |o, _| assert!(o.project_issue_picker().is_none(), "closes after"));
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(with_created(picker_snapshot(), "w-new-ENG-4", UI))), cx)
    });
    draw(vcx);
    overview.read_with(vcx, |o, cx| {
        assert_eq!(
            o.outline.read(cx).selected_key(),
            Some(row_key::item("s1", "w-new-ENG-4").as_str()),
            "the first new node is selected"
        )
    });
}

/// A failed create keeps the picker open with that issue still checked,
/// and the retry reuses its operation id.
#[gpui::test]
fn add_issues_retry_keeps_operation_ids(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let flag = fail.clone();
    let (overview, calls, vcx) =
        linear_overview(cx, dir.path(), picker_snapshot(), move |method, params| {
            if params["op"] == "createWorkItemFromLinear"
                && flag.swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(zeron_rpc::RpcError::Failed("linear/rateLimited: slow down".into()));
            }
            picker_reply(method, params)
        });
    overview.update(vcx, |o, cx| o.open_project_issue_picker("s1".into(), UI.into(), cx).unwrap());
    vcx.run_until_parked();
    draw(vcx);
    press(vcx, "linear-add-issues-check-ENG-4".into());
    press(vcx, "linear-add-issues-create".into());
    assert!(vcx.debug_bounds("linear-add-issues-error").is_some());
    let picker = overview.read_with(vcx, |o, _| o.project_issue_picker().expect("stays open"));
    picker.read_with(vcx, |p, _| assert!(p.checked.contains("ENG-4")));
    press(vcx, "linear-add-issues-create".into());
    let creates = mutations(&calls, "createWorkItemFromLinear");
    assert_eq!(creates.len(), 2);
    assert_eq!(creates[0]["operationId"], creates[1]["operationId"], "a retry replays");
    overview.read_with(vcx, |o, _| assert!(o.project_issue_picker().is_none()));
}

/// With Linear off nothing offers "Add issues from…" and the picker
/// refuses to open (no Linear calls).
#[gpui::test]
fn add_issues_is_gated_on_linear(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mut snapshot = picker_snapshot();
    snapshot.space_settings.linear_enabled = false;
    let (overview, calls, vcx) = linear_overview(cx, dir.path(), snapshot, picker_reply);
    overview.read_with(vcx, |o, cx| {
        let menu = o.outline.read(cx).context_menu(&row_key::item("s1", UI), cx).unwrap();
        assert!(menu.entries.iter().all(|e| e.id != "linear-add-issues"));
    });
    open_details(&overview, vcx, UI);
    assert!(vcx.debug_bounds("work-item-linear-add-issues").is_none());
    let result = overview.update(vcx, |o, cx| o.open_project_issue_picker("s1".into(), UI.into(), cx));
    assert!(result.is_err());
    overview.read_with(vcx, |o, _| assert!(o.project_issue_picker().is_none()));
    // Not project-linked: refused too.
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) = linear_overview(cx, dir.path(), picker_snapshot(), picker_reply);
    let result = overview.update(vcx, |o, cx| o.open_project_issue_picker("s1".into(), API.into(), cx));
    assert!(result.is_err());
    assert_eq!(called(&calls, "LinearProjectIssues"), 0);
}

/// With two Linear projects the header chip opens a menu (both projects,
/// the current one checked) instead of cycling; ↓ ↩ picks the other one,
/// Esc closes the menu only.
#[gpui::test]
fn project_chip_opens_a_menu_instead_of_cycling(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, _calls, vcx) = linear_overview(cx, dir.path(), linear_snapshot(true), default_reply);
    let state = overview.read_with(vcx, |o, _| o.state.clone());
    state.update(vcx, |state, _| {
        state.spaces.push(
            serde_json::from_value(serde_json::json!({
                "id": "s2", "deviceId": "d", "path": "/work/beta",
                "createdAt": "2026-01-01T00:00:00Z"
            }))
            .unwrap(),
        );
    });
    let mut second = crate::work_outline::store::fixture_snapshot("s2");
    second.space_settings.linear_enabled = true;
    overview.update(vcx, |o, cx| {
        let mut loads = fixture_loads(Some(linear_snapshot(true)));
        loads.insert("s2".into(), WorkOutlineLoad::Ready(std::sync::Arc::new(second)));
        o.set_outline_loads(loads, cx)
    });
    draw(vcx);
    let panel = open_panel(&overview, vcx);
    let first = panel.read_with(vcx, |panel, _| panel.space_id.clone());
    press(vcx, "linear-panel-space".into());
    assert!(vcx.debug_bounds("linear-space-menu").is_some(), "a menu opens");
    assert!(vcx.debug_bounds("linear-space-s1").is_some());
    assert!(vcx.debug_bounds("linear-space-s2").is_some());
    panel.read_with(vcx, |panel, _| {
        assert!(panel.space_menu_open());
        assert_eq!(panel.space_id, first, "opening never switches");
    });
    vcx.simulate_keystrokes("down enter");
    draw(vcx);
    panel.read_with(vcx, |panel, _| {
        assert!(!panel.space_menu_open());
        assert_ne!(panel.space_id, first, "↓ ↩ picked the other project");
    });
    press(vcx, "linear-panel-space".into());
    vcx.simulate_keystrokes("escape");
    draw(vcx);
    panel.read_with(vcx, |panel, _| assert!(!panel.space_menu_open()));
    overview.read_with(vcx, |o, _| assert!(o.column_shown(), "Esc only closed the menu"));
}

/// The Linear issue chip text of `identifier` as the Outline would render it
/// right now (the bare identifier while there is nothing cached).
fn shown_issue_text(state: &Entity<AppState>, vcx: &mut gpui::VisualTestContext) -> String {
    state.read_with(vcx, |state, _| match state.linear.issue("ENG-1") {
        Some(crate::linear::store::IssueEntry::Ready { issue, .. }) => {
            crate::work_outline::outline_view::linear_outline::issue_chip_text(issue)
        }
        _ => "ENG-1".to_string(),
    })
}

/// A refresh that returns the same issue leaves the chip text identical at
/// every step (nothing shown while the pull is in flight), a failed pull
/// keeps the last good text, and a real change still shows.
#[gpui::test]
fn issue_chip_text_is_stable_across_refreshes_and_changes_with_the_data(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let answer: std::sync::Arc<std::sync::Mutex<Option<zeron_proto::linear::LinearIssueSummary>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Some(lf::issue("ENG-1", "Fix login"))));
    let script = answer.clone();
    let (overview, _calls, vcx) = linear_overview(
        cx,
        dir.path(),
        linear_snapshot(true),
        move |method, params| match method {
            "LinearIssue" => match script.lock().unwrap().clone() {
                Some(issue) => Ok(serde_json::to_value(issue).unwrap()),
                None => Err(zeron_rpc::RpcError::Failed("linear unavailable".into())),
            },
            _ => default_reply(method, params),
        },
    );
    let state = overview.read_with(vcx, |o, _| o.state.clone());
    vcx.run_until_parked();
    let before = shown_issue_text(&state, vcx);
    assert_ne!(before, "ENG-1", "the issue loaded");

    let pull = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|_, cx| crate::linear::store::request_issue(&state, "ENG-1", true, cx))
    };
    for _ in 0..2 {
        pull(vcx);
        assert_eq!(shown_issue_text(&state, vcx), before, "in flight");
        vcx.run_until_parked();
        assert_eq!(shown_issue_text(&state, vcx), before, "after the pull");
    }

    *answer.lock().unwrap() = None;
    pull(vcx);
    assert_eq!(shown_issue_text(&state, vcx), before, "failing, in flight");
    vcx.run_until_parked();
    assert_eq!(shown_issue_text(&state, vcx), before, "after a failed pull");

    let mut changed = lf::issue("ENG-1", "Fix login");
    changed.state.name = "Done".into();
    *answer.lock().unwrap() = Some(changed);
    pull(vcx);
    vcx.run_until_parked();
    let after = shown_issue_text(&state, vcx);
    assert_ne!(after, before);
    assert!(after.starts_with("Done"), "{after}");
}
