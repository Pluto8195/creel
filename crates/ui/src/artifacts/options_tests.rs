//! Side-by-side options, picking and the canonical plan's Linear panel
//! (Artifacts phase 5; headless gpui, scripted engine, fake web host).
//! Every Linear reply is fake data: nothing reaches Linear.
use std::sync::Arc;

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use serde_json::json;
use zeron_proto::{ArtifactContentEncoding, ArtifactKind, ArtifactOptionPick, ArtifactView};
use zeron_rpc::methods;

use super::tab::{ArtifactTab, FakeWebHost};
use super::tests::{
    Calls, Reply, SPACE, Shown, artifact, content_reply, id, init_ui, scripted, snapshot,
};

const OPTIONS: &str = r#"{"title":"Layouts","question":"Which layout?","options":[
  {"id":"A","label":"Grid","path":"a.fragment.html"},
  {"id":"B","label":"List","path":"b.fragment.html"},
  {"id":"C","label":"Cards","path":"c.fragment.html"}]}"#;

const FIVE: &str = r#"{"title":"Layouts","options":[
  {"id":"A","label":"One","path":"a.html"},{"id":"B","label":"Two","path":"b.html"},
  {"id":"C","label":"Three","path":"c.html"},{"id":"D","label":"Four","path":"d.html"},
  {"id":"E","label":"Five","path":"e.html"}]}"#;

fn chat(id: &str) -> zeron_proto::Chat {
    serde_json::from_value(json!({
        "id": id, "deviceId": "dev", "archived": false, "createdAt": "2026-07-19T12:00:00Z",
    }))
    .unwrap()
}

fn replies(manifest: &'static str) -> Reply {
    Arc::new(move |method, params| match method {
        methods::ARTIFACT_CONTENT => Ok(match params.get("companion").and_then(|c| c.as_str()) {
            Some(path) => content_reply(
                ArtifactKind::HtmlFragment,
                0,
                &format!("<p>{path}</p>"),
                ArtifactContentEncoding::Utf8,
            ),
            None => content_reply(
                ArtifactKind::Options,
                2,
                manifest,
                ArtifactContentEncoding::Utf8,
            ),
        }),
        methods::ARTIFACT_PICK_OPTION => {
            let option = params["optionId"].as_str().map(|option| {
                json!({ "optionId": option, "label": params["label"], "note": params["note"],
                        "at": 5, "revision": 2 })
            });
            Ok(json!({ "artifactId": params["artifactId"], "picked": option }))
        }
        methods::CONTINUE_CHAT_FOR_WORK_ITEM => Ok(json!({
            "chatId": params["chatId"], "messageId": "m", "commandId": "k", "state": "started",
        })),
        methods::ARTIFACT_DRIFT => Ok(json!({ "artifactId": params["artifactId"] })),
        other => panic!("unexpected {other}"),
    })
}

fn settle(vcx: &mut VisualTestContext) {
    for _ in 0..3 {
        vcx.run_until_parked();
        vcx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
    }
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    manifest: &'static str,
    picked: Option<ArtifactOptionPick>,
) -> (Entity<ArtifactTab>, Calls, Shown, &'a mut VisualTestContext) {
    let (actions, calls) = scripted(replies(manifest));
    let shown: Shown = Default::default();
    let factory = FakeWebHost::factory(shown.clone());
    let mut view: ArtifactView = artifact(&id(1), "Layouts", ArtifactKind::Options);
    view.picked = picked;
    view.revisions[0].by.chat_id = Some("chat-latest".into());
    let state = cx.new(|_| crate::state::AppState::new());
    state.update(cx, |state, _| {
        state.local_device_id = Some("dev".into());
        state.work_outline_actions_override = Some(actions);
        state.chats = vec![chat("chat-latest")];
        state.set_test_artifacts(snapshot(vec![view.clone()]));
    });
    let artifact_id = view.id.clone();
    let (tab, vcx) = cx.add_window_view(move |window, cx| {
        ArtifactTab::new(state, None, artifact_id, factory, window, cx)
    });
    settle(vcx);
    (tab, calls, shown, vcx)
}

fn calls_of(calls: &Calls, method: &str) -> Vec<serde_json::Value> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, _)| m == method)
        .map(|(_, params)| params.clone())
        .collect()
}

fn click(vcx: &mut VisualTestContext, selector: &'static str) {
    let bounds = vcx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is on screen"));
    vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
    settle(vcx);
}

fn has(vcx: &mut VisualTestContext, selector: &'static str) -> bool {
    vcx.debug_bounds(selector).is_some()
}

#[gpui::test]
fn options_open_in_synced_columns_with_a_pick_button_each(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, calls, shown, vcx) = open(cx, OPTIONS, None);
    click(vcx, "artifact-side-by-side");
    tab.read_with(vcx, |tab, _| {
        assert!(tab.side_by_side_shown());
        assert!(tab.shows_web());
    });
    let companions: Vec<String> = calls_of(&calls, methods::ARTIFACT_CONTENT)
        .iter()
        .filter_map(|c| c["companion"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        companions,
        ["a.fragment.html", "b.fragment.html", "c.fragment.html"]
    );
    let doc = shown.borrow().last().unwrap().1.clone();
    assert_eq!(doc.matches("<iframe").count(), 3);
    assert_eq!(doc.matches("sandbox=\"allow-scripts\"").count(), 3);
    // Scroll and zoom are mirrored to every other frame.
    assert!(doc.contains("others.forEach"));
    assert!(doc.contains("flex:1 1 0"));
    for option in ["A", "B", "C"] {
        assert!(has(
            vcx,
            Box::leak(format!("artifact-pick-{option}").into_boxed_str())
        ));
    }
    // The question is the title; the zoom keys apply to the columns.
    assert!(tab.read_with(vcx, |tab, _| tab.zoom_applies()));
    // Back to the list.
    click(vcx, "artifact-side-close");
    tab.read_with(vcx, |tab, _| assert!(!tab.side_by_side_shown()));
    assert!(has(vcx, "artifact-options"));
}

#[gpui::test]
fn more_than_four_options_show_four_and_can_be_chosen(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _calls, shown, vcx) = open(cx, FIVE, None);
    click(vcx, "artifact-side-by-side");
    assert_eq!(
        shown.borrow().last().unwrap().1.matches("<iframe").count(),
        4
    );
    assert!(has(vcx, "artifact-side-choose-E"));
    // Swap D for E: toggle E on is refused at four, then D off, E on.
    tab.update_in(vcx, |tab, window, cx| tab.toggle_side_option(4, window, cx));
    settle(vcx);
    assert_eq!(tab.read_with(vcx, |tab, _| tab.side_chosen()), [0, 1, 2, 3]);
    tab.update_in(vcx, |tab, window, cx| tab.toggle_side_option(3, window, cx));
    tab.update_in(vcx, |tab, window, cx| tab.toggle_side_option(4, window, cx));
    settle(vcx);
    assert_eq!(tab.read_with(vcx, |tab, _| tab.side_chosen()), [0, 1, 2, 4]);
    let doc = shown.borrow().last().unwrap().1.clone();
    assert!(doc.contains("e.html") && !doc.contains("d.html"));
    // Never fewer than two columns.
    for ix in [0, 1] {
        tab.update_in(vcx, |tab, window, cx| {
            tab.toggle_side_option(ix, window, cx)
        });
    }
    settle(vcx);
    assert_eq!(tab.read_with(vcx, |tab, _| tab.side_chosen().len()), 2);
}

#[gpui::test]
fn picking_records_the_choice_tells_the_chat_and_shows_it(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, calls, _shown, vcx) = open(cx, OPTIONS, None);
    click(vcx, "artifact-side-by-side");
    assert!(!has(vcx, "artifact-header-picked"));
    tab.update(vcx, |tab, cx| {
        tab.set_pick_note_for_tests("keep the PO header", cx)
    });
    click(vcx, "artifact-pick-B");
    let recorded = calls_of(&calls, methods::ARTIFACT_PICK_OPTION);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["optionId"], "B");
    assert_eq!(recorded[0]["label"], "List");
    assert_eq!(recorded[0]["note"], "keep the PO header");
    let sent = calls_of(&calls, methods::CONTINUE_CHAT_FOR_WORK_ITEM);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["chatId"], "chat-latest");
    assert_eq!(
        sent[0]["prompt"]["text"],
        "Picked option B for \"Layouts\": List\n\nNote: keep the PO header"
    );
    // Shown at once, in the header and the columns' bar.
    assert!(has(vcx, "artifact-header-picked"));
    assert!(has(vcx, "artifact-picked-chip"));
    let picked = tab.read_with(vcx, |tab, cx| tab.picked_now(&tab.artifact(cx).unwrap()));
    assert_eq!(picked.unwrap().option_id, "B");
}

#[gpui::test]
fn the_pick_can_be_changed_and_withdrawn(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let existing = ArtifactOptionPick {
        option_id: "A".into(),
        label: "Grid".into(),
        at: 1,
        revision: 2,
        ..Default::default()
    };
    let (tab, calls, _shown, vcx) = open(cx, OPTIONS, Some(existing));
    // A pick from the snapshot shows without any local action.
    assert!(has(vcx, "artifact-header-picked"));
    click(vcx, "artifact-side-by-side");
    // Change to C.
    click(vcx, "artifact-pick-C");
    let sent = calls_of(&calls, methods::CONTINUE_CHAT_FOR_WORK_ITEM);
    assert_eq!(
        sent[0]["prompt"]["text"],
        "Changed my pick to option C for \"Layouts\": Cards"
    );
    assert_eq!(
        tab.read_with(vcx, |tab, cx| tab
            .picked_now(&tab.artifact(cx).unwrap())
            .unwrap()
            .option_id),
        "C"
    );
    // Clicking the picked column again withdraws it.
    click(vcx, "artifact-pick-C");
    let recorded = calls_of(&calls, methods::ARTIFACT_PICK_OPTION);
    assert_eq!(recorded.len(), 2);
    assert!(recorded[1]["optionId"].is_null());
    let sent = calls_of(&calls, methods::CONTINUE_CHAT_FOR_WORK_ITEM);
    assert!(
        sent[1]["prompt"]["text"]
            .as_str()
            .unwrap()
            .starts_with("Withdrew my pick of option C")
    );
    assert!(tab.read_with(vcx, |tab, cx| {
        tab.picked_now(&tab.artifact(cx).unwrap()).is_none()
    }));
    assert!(!has(vcx, "artifact-header-picked"));
}

// --------------------------------------------------------------------------
// Canonical plan + Linear project panel
// --------------------------------------------------------------------------

fn project_detail() -> serde_json::Value {
    json!({
        "id": "p-1", "name": "Receiving", "url": "https://linear.app/acme/project/p-1",
        "state": "started", "progress": 0.4, "targetDate": "2026-12-01",
        "lead": { "id": "u", "name": "Ada Lovelace", "displayName": "ada" },
        "updates": [
            { "id": "up", "body": "Login shipped", "health": "onTrack",
              "author": { "id": "u", "name": "Ada" }, "createdAt": "2026-10-25T12:00:00Z" }
        ],
    })
}

fn plan_tab<'a>(
    cx: &'a mut TestAppContext,
    linear_enabled: bool,
    pinned: bool,
    detail: Result<serde_json::Value, zeron_rpc::RpcError>,
) -> (Entity<ArtifactTab>, Calls, &'a mut VisualTestContext) {
    let reply: Reply = Arc::new(move |method, params| match method {
        methods::ARTIFACT_DRIFT => Ok(json!({ "artifactId": params["artifactId"] })),
        methods::ARTIFACT_CONTENT => Ok(content_reply(
            ArtifactKind::Markdown,
            2,
            "# Plan\n",
            ArtifactContentEncoding::Utf8,
        )),
        "LinearProjectDetail" => match &detail {
            Ok(value) => Ok(value.clone()),
            Err(error) => Err(zeron_rpc::RpcError::Failed(error.to_string())),
        },
        other => panic!("unexpected {other}"),
    });
    let (actions, calls) = scripted(reply);
    let factory = FakeWebHost::factory(Default::default());
    let mut plan = artifact(&id(1), "Plan", ArtifactKind::Markdown);
    plan.pinned_as_plan = pinned;
    let mut outline = crate::work_outline::store::fixture_snapshot(SPACE);
    outline.space_settings.linear_enabled = linear_enabled;
    outline
        .external_references
        .push(crate::linear::fixture::reference(
            "ref",
            SPACE,
            "w-api",
            "project",
            "p-1",
            "https://linear.app/acme/project/p-1",
        ));
    let state = cx.new(|_| crate::state::AppState::new());
    state.update(cx, |state, _| {
        state.local_device_id = Some("dev".into());
        state.work_outline_actions_override = Some(actions);
        state.set_test_artifacts(snapshot(vec![plan.clone()]));
        state.set_work_outline_visible(vec![SPACE.into()]);
        assert!(state.apply_work_outline_frame(SPACE, outline));
    });
    let artifact_id = plan.id.clone();
    let (tab, vcx) = cx.add_window_view(move |window, cx| {
        ArtifactTab::new(state, None, artifact_id, factory, window, cx)
    });
    settle(vcx);
    (tab, calls, vcx)
}

#[gpui::test]
fn the_canonical_plan_shows_its_linear_project_details(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (_tab, calls, vcx) = plan_tab(cx, true, true, Ok(project_detail()));
    for selector in [
        "artifact-plan-linear-panel",
        "artifact-plan-linear-name",
        "artifact-plan-linear-state",
        "artifact-plan-linear-progress",
        "artifact-plan-linear-target",
        "artifact-plan-linear-lead",
        "artifact-plan-linear-update-0",
    ] {
        assert!(has(vcx, selector), "{selector}");
    }
    // Stored copy first (never asks Linear), then the cached read: reads only.
    let reads = calls_of(&calls, "LinearProjectDetail");
    assert_eq!(reads.len(), 2);
    assert_eq!(reads[0]["cachedOnly"], true);
    assert_eq!(reads[1]["projectId"], "p-1");
    assert!(reads[1].get("cachedOnly").is_none());
    // Folding it away keeps the toggle.
    click(vcx, "artifact-plan-linear-toggle");
    assert!(!has(vcx, "artifact-plan-linear-panel"));
    assert!(has(vcx, "artifact-plan-linear-toggle"));
}

#[gpui::test]
fn the_panel_needs_the_pinned_plan_and_linear_on(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (_tab, calls, vcx) = plan_tab(cx, true, false, Ok(project_detail()));
    assert!(!has(vcx, "artifact-plan-linear-panel") && !has(vcx, "artifact-plan-linear-toggle"));
    assert!(calls_of(&calls, "LinearProjectDetail").is_empty());
    let (_tab, calls, vcx) = plan_tab(cx, false, true, Ok(project_detail()));
    assert!(!has(vcx, "artifact-plan-linear-panel"));
    assert!(calls_of(&calls, "LinearProjectDetail").is_empty());
}

#[gpui::test]
fn a_failed_linear_read_says_so_instead_of_hiding_the_plan(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let err = zeron_rpc::RpcError::Failed("linear/missingKey: no key".into());
    let (_tab, _calls, vcx) = plan_tab(cx, true, true, Err(err));
    assert!(has(vcx, "artifact-plan-linear-error"));
    assert!(!has(vcx, "artifact-plan-linear-panel"));
    assert!(has(vcx, "artifact-plan-badge"));
}
