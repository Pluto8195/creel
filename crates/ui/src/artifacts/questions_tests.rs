//! Plan question UI tests: answered cards, the Answer flow, the drift badge
//! and the "updated since you last viewed" marker (headless gpui).
use std::sync::Arc;

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use serde_json::json;
use mast_proto::{
    ArtifactContentEncoding, ArtifactDriftReply, ArtifactKind, ArtifactQuestionAnswer,
    ArtifactView, WorkOutlineActor,
};
use mast_rpc::methods;

use super::tab::ArtifactTab;
use super::tests::{Calls, Reply, artifact, content_reply, id, init_ui, scripted, snapshot};
use crate::markdown::render;

const PLAN: &str = "# Plan\n\n\
> [!question] Q-1 · Merge by MPN?\n\
> status: open\n\n\
> [!question] Q-2 · Which timezone applies?\n\
> blocking: true\n\
> status: open\n\n\
> [!question] Q-3 · Where is the report?\n\
> status: resolved\n\
> decision: retro\n";

fn plan(answers: Vec<ArtifactQuestionAnswer>) -> ArtifactView {
    let mut view = artifact(&id(1), "Plan", ArtifactKind::Markdown);
    view.answers = answers;
    view
}

fn answer(id: &str, text: &str, at: i64) -> ArtifactQuestionAnswer {
    ArtifactQuestionAnswer {
        id: id.into(),
        answer: text.into(),
        at,
        ..Default::default()
    }
}

fn chat(id: &str, archived: bool) -> mast_proto::Chat {
    serde_json::from_value(json!({
        "id": id, "deviceId": "dev", "archived": archived,
        "createdAt": "2026-07-19T12:00:00Z",
    }))
    .expect("chat")
}

fn replies(drift: Option<ArtifactDriftReply>) -> Reply {
    Arc::new(move |method, params| match method {
        methods::ARTIFACT_CONTENT => Ok(content_reply(
            ArtifactKind::Markdown,
            2,
            PLAN,
            ArtifactContentEncoding::Utf8,
        )),
        methods::ARTIFACT_DRIFT => match &drift {
            Some(drift) => Ok(serde_json::to_value(drift).unwrap()),
            None => Ok(json!({ "artifactId": params["artifactId"] })),
        },
        methods::ARTIFACT_ANSWER_QUESTION => Ok(json!({
            "artifactId": params["artifactId"], "questionId": params["questionId"],
        })),
        methods::CONTINUE_CHAT_FOR_WORK_ITEM => Ok(json!({
            "chatId": params["chatId"], "messageId": "m", "commandId": "k", "state": "started",
        })),
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

fn open(
    cx: &mut TestAppContext,
    view: ArtifactView,
    chats: Vec<mast_proto::Chat>,
    drift: Option<ArtifactDriftReply>,
) -> (Entity<ArtifactTab>, Calls, &mut VisualTestContext) {
    let (actions, calls) = scripted(replies(drift));
    let factory = super::tab::FakeWebHost::factory(Default::default());
    let state = cx.new(|_| crate::state::AppState::new());
    state.update(cx, |state, _| {
        state.local_device_id = Some("dev".into());
        state.work_outline_actions_override = Some(actions);
        state.chats = chats;
        state.set_test_artifacts(snapshot(vec![view.clone()]));
    });
    let artifact_id = view.id.clone();
    let (tab, vcx) = cx.add_window_view(move |window, cx| {
        ArtifactTab::new(state, None, artifact_id, factory, window, cx)
    });
    settle(vcx);
    (tab, calls, vcx)
}

fn has(vcx: &mut VisualTestContext, selector: &'static str) -> bool {
    vcx.debug_bounds(selector).is_some()
}

/// The last card state drawn per question id.
fn cards(vcx: &mut VisualTestContext) -> Vec<(String, &'static str, Option<String>, bool)> {
    vcx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
    render::take_answer_log()
}

fn method_calls(calls: &Calls, method: &str) -> Vec<serde_json::Value> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, _)| m == method)
        .map(|(_, params)| params.clone())
        .collect()
}

#[gpui::test]
fn cards_show_answered_resolved_and_open_states(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    render::take_answer_log();
    let view = plan(vec![
        answer("Q-1", "By MPN", 3_000),
        answer("Q-3", "ignored", 3_000),
    ]);
    let (_tab, _calls, vcx) = open(cx, view, Vec::new(), None);
    let drawn = cards(vcx);
    let find = |key: &str| drawn.iter().rfind(|card| card.0 == key).cloned().unwrap();
    // Open in the document + an answer on the row: Answered, with the answer.
    assert_eq!(
        find("Q-1"),
        ("Q-1".into(), "Answered", Some("By MPN".into()), false)
    );
    // Nothing answered: still Open, with the Answer affordance.
    assert_eq!(find("Q-2"), ("Q-2".into(), "Open", None, true));
    // Resolved in the document wins over an answer on the row.
    assert_eq!(find("Q-3"), ("Q-3".into(), "Resolved", None, false));
    assert!(has(vcx, "question-answer-row-Q-1"));
    assert!(!has(vcx, "question-answer-row-Q-3"));
    assert!(has(vcx, "question-answer-Q-2"));
    assert!(!has(vcx, "question-answer-Q-1"));
}

#[gpui::test]
fn answering_records_the_answer_and_tells_the_publishing_chat(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let mut view = plan(Vec::new());
    view.revisions[0].by = WorkOutlineActor {
        chat_id: Some("chat-latest".into()),
        ..Default::default()
    };
    let (tab, calls, vcx) = open(
        cx,
        view,
        vec![chat("chat-latest", false), chat("chat-api", false)],
        None,
    );
    tab.update_in(vcx, |tab, window, cx| {
        tab.begin_answer("q-2".into(), window, cx)
    });
    settle(vcx);
    assert!(has(vcx, "question-answer-input-Q-2"));
    assert!(has(vcx, "question-answer-send-Q-2"));
    tab.update(vcx, |tab, cx| {
        tab.set_answer_text_for_tests("UTC", cx);
        tab.send_answer("Q-2".into(), cx);
    });
    settle(vcx);
    let recorded = method_calls(&calls, methods::ARTIFACT_ANSWER_QUESTION);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["questionId"], "Q-2");
    assert_eq!(recorded[0]["answer"], "UTC");
    let sent = method_calls(&calls, methods::CONTINUE_CHAT_FOR_WORK_ITEM);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["chatId"], "chat-latest");
    assert_eq!(
        sent[0]["prompt"]["text"],
        "Re: \"Q-2: Which timezone applies?\": UTC"
    );
    // Optimistically Answered before the snapshot catches up; input closed.
    let drawn = cards(vcx);
    assert!(
        drawn
            .iter()
            .any(|c| c.0 == "Q-2" && c.1 == "Answered" && c.2.as_deref() == Some("UTC"))
    );
    assert!(!has(vcx, "question-answer-input-Q-2"));
}

#[gpui::test]
fn answering_still_records_when_the_chat_is_archived(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, calls, vcx) = open(cx, plan(Vec::new()), vec![chat("chat-api", true)], None);
    tab.update_in(vcx, |tab, window, cx| {
        tab.begin_answer("Q-1".into(), window, cx)
    });
    tab.update(vcx, |tab, cx| {
        tab.set_answer_text_for_tests("By MPN", cx);
        tab.send_answer("Q-1".into(), cx);
    });
    settle(vcx);
    assert_eq!(
        method_calls(&calls, methods::ARTIFACT_ANSWER_QUESTION).len(),
        1
    );
    assert!(method_calls(&calls, methods::CONTINUE_CHAT_FOR_WORK_ITEM).is_empty());
    assert!(tab.read_with(vcx, |tab, _| {
        tab.notice().unwrap_or("").contains("unavailable")
    }));
}

fn drift_reply(commits: u32) -> ArtifactDriftReply {
    ArtifactDriftReply {
        artifact_id: id(1),
        commits_since: commits,
        branch: Some("feat/x".into()),
        latest_commit_at: Some(5_000),
        since: 2_000,
    }
}

#[gpui::test]
fn drift_badge_shows_for_unreflected_answers_and_commits_only(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    // Nothing answered, no commits: no badge, but the open questions count.
    let (_tab, calls, vcx) = open(cx, plan(Vec::new()), Vec::new(), None);
    assert!(!has(vcx, "artifact-drift-badge"));
    assert!(has(vcx, "artifact-open-questions"));
    // Fetched once on open (not on every draw), forwarded nowhere (local).
    let drift_calls = method_calls(&calls, methods::ARTIFACT_DRIFT);
    assert_eq!(drift_calls.len(), 1);
    assert_eq!(drift_calls[0]["artifactId"], id(1));
    assert!(drift_calls[0].get("targetDeviceId").is_none());

    // An answer newer than the revision that the plan does not reflect.
    let (_tab, _calls, vcx) = open(
        cx,
        plan(vec![answer("Q-1", "By MPN", 3_000)]),
        Vec::new(),
        None,
    );
    assert!(has(vcx, "artifact-drift-badge"));

    // Commits since the revision.
    let (_tab, _calls, vcx) = open(cx, plan(Vec::new()), Vec::new(), Some(drift_reply(3)));
    assert!(has(vcx, "artifact-drift-badge"));

    // An answer older than the latest revision is reflected already.
    let (_tab, _calls, vcx) = open(
        cx,
        plan(vec![answer("Q-1", "By MPN", 1_500)]),
        Vec::new(),
        Some(drift_reply(0)),
    );
    assert!(!has(vcx, "artifact-drift-badge"));
}

#[gpui::test]
fn changed_since_marker_survives_the_visit_record(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let key = id(1);
    // First visit: nothing recorded, no marker, current revision recorded.
    let (_tab, _calls, vcx) = open(cx, plan(Vec::new()), Vec::new(), None);
    assert!(!has(vcx, "artifact-changed-since"));
    let recorded = vcx.update(|_, cx| {
        crate::settings::current(cx)
            .artifact_last_viewed
            .get(&key)
            .copied()
    });
    assert_eq!(recorded, Some(2));
    // Equal to current: no marker.
    let (_tab, _calls, vcx) = open(cx, plan(Vec::new()), Vec::new(), None);
    assert!(!has(vcx, "artifact-changed-since"));

    // Last viewed r1, current r2: the marker shows, and still shows after
    // the visit was recorded (the record happens after computing it).
    cx.update(|cx| {
        crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
            settings.artifact_last_viewed.insert(key.clone(), 1);
        });
    });
    let (tab, _calls, vcx) = open(cx, plan(Vec::new()), Vec::new(), None);
    assert!(has(vcx, "artifact-changed-since"));
    settle(vcx);
    assert!(has(vcx, "artifact-changed-since"));
    let recorded = vcx.update(|_, cx| {
        crate::settings::current(cx)
            .artifact_last_viewed
            .get(&key)
            .copied()
    });
    assert_eq!(recorded, Some(2));

    // Clicking opens Compare from the last viewed revision to the current.
    tab.update_in(vcx, |tab, window, cx| tab.open_changes(window, cx));
    settle(vcx);
    let pair = tab.read_with(vcx, |tab, _| {
        let view = tab.compare_view().expect("compare view");
        (view.left, view.right)
    });
    assert_eq!(pair, (1, 2));
    assert!(!has(vcx, "artifact-changed-since"));
}
