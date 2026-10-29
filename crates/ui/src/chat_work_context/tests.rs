//! Work-context resolver tests. Every store is filled by hand; nothing reaches
//! GitHub or Linear.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use chrono::{TimeZone, Utc};
use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext,
    VisualTestContext, Window, div, px,
};
use mast_engine::pr_ticket_cache::{ChatLinkStatus, PrLinkStatus};
use mast_proto::{
    ArtifactKind, ChatLinkSource, PullRequestCiStatus, PullRequestMyReviewState, PullRequestRef,
    PullRequestReviewDecision, PullRequestState, WorkOutlineSnapshot,
};

use super::*;
use crate::artifacts::tests::{artifact, id as artifact_id, snapshot as artifacts_snapshot};
use crate::github::fixture as gf;
use crate::github::row_icons::Tone;
use crate::github::store::PrEntry;
use crate::linear::fixture as lf;
use crate::linear::store::IssueEntry;
use crate::theme::Theme;
use crate::work_outline::store::{fixture_ids::*, fixture_snapshot};

// ----------------------------------------------------------------------
// Resolver
// ----------------------------------------------------------------------

fn pr_ref(repo: &str, number: u64) -> PullRequestRef {
    PullRequestRef::new("acme", repo, number)
}

fn link(repo: &str, number: u64, source: ChatLinkSource) -> ChatPr {
    ChatPr {
        pr: pr_ref(repo, number),
        source: Some(source),
    }
}

/// The store fixture: `w-launch` > `w-api` (CHAT_API) > `w-api-auth`
/// (CHAT_AUTH), and `w-launch` > `w-ui` (CHAT_UI); Linear on.
fn snapshot() -> WorkOutlineSnapshot {
    let mut snapshot = fixture_snapshot("s1");
    snapshot.space_settings.linear_enabled = true;
    snapshot
}

/// `w-api` is a PR review item: a child of the sync destination.
fn with_review_item(snapshot: &mut WorkOutlineSnapshot, prs: &[(&str, u64)]) {
    snapshot.space_settings.pr_review_sync_work_item_id = Some(LAUNCH.into());
    snapshot.external_references = prs
        .iter()
        .enumerate()
        .map(|(i, (repo, number))| {
            let mut reference = gf::reference(&format!("r{i}"), "s1", API, repo, *number);
            reference.created_at = i as i64 + 1;
            reference
        })
        .collect();
}

fn linear_issue_on(snapshot: &mut WorkOutlineSnapshot, item: &str, identifier: &str) {
    snapshot.external_references.push(lf::reference(
        &format!("li-{item}"),
        "s1",
        item,
        "issue",
        identifier,
        &format!("https://linear.app/acme/issue/{identifier}/x"),
    ));
}

fn resolve_for(
    snapshot: &WorkOutlineSnapshot,
    chat: &str,
    chat_prs: &[ChatPr],
    artifacts: Option<&ArtifactsSnapshot>,
) -> Option<WorkContext> {
    resolve(&ResolveInput {
        chat_id: chat,
        snapshot: Some(snapshot),
        chat_prs,
        artifacts,
    })
}

fn as_review(context: &WorkContext) -> &PrReviewContext {
    match &context.variant {
        Variant::PrReview(review) => review,
        other => panic!("expected a PR review strip, got {other:?}"),
    }
}

fn as_implementation(context: &WorkContext) -> &ImplementationContext {
    match &context.variant {
        Variant::Implementation(implementation) => implementation,
        other => panic!("expected an implementation strip, got {other:?}"),
    }
}

fn as_planning(context: &WorkContext) -> &PlanningContext {
    match &context.variant {
        Variant::Planning(planning) => planning,
        other => panic!("expected a planning strip, got {other:?}"),
    }
}

#[test]
fn a_chat_on_a_review_item_shows_that_items_pr() {
    let mut snapshot = snapshot();
    with_review_item(&mut snapshot, &[("app", 7)]);
    let context = resolve_for(&snapshot, CHAT_API, &[], None).expect("strip");
    assert_eq!(context.item_id.as_deref(), Some(API));
    let review = as_review(&context);
    assert_eq!(review.prs, vec![pr_ref("app", 7)]);
    assert_eq!(review.pr_item_id.as_deref(), Some(API));
    assert_eq!(review.ticket, None);
}

#[test]
fn a_chat_below_a_review_item_shows_the_nearest_ancestors_pr() {
    let mut snapshot = snapshot();
    with_review_item(&mut snapshot, &[("app", 7)]);
    let context = resolve_for(&snapshot, CHAT_AUTH, &[], None).expect("strip");
    assert_eq!(context.item_id.as_deref(), Some(API_AUTH));
    assert_eq!(as_review(&context).prs, vec![pr_ref("app", 7)]);
    assert_eq!(as_review(&context).pr_item_id.as_deref(), Some(API));
    // A sibling branch without the PR gets nothing.
    assert!(resolve_for(&snapshot, CHAT_UI, &[], None).is_none());
}

#[test]
fn an_archived_review_item_still_gives_its_chat_the_strip() {
    // Merged and closed PRs auto-archive their item; the review Chat stays.
    let mut snapshot = snapshot();
    with_review_item(&mut snapshot, &[("app", 7)]);
    snapshot
        .items
        .iter_mut()
        .find(|item| item.id == API)
        .unwrap()
        .archived_at = Some(5);
    assert!(resolve_for(&snapshot, CHAT_API, &[], None).is_some());
}

#[test]
fn a_synced_or_titled_item_is_a_review_item_without_a_destination() {
    let mut snapshot = snapshot();
    snapshot.external_references = vec![gf::reference("r1", "s1", UI, "app", 3)];
    // A plain feature item linking a PR is not a review (implementation).
    let context = resolve_for(&snapshot, CHAT_UI, &[], None).expect("strip");
    assert!(matches!(context.variant, Variant::Implementation(_)));
    // Titled `<repo>#<number>` (the panel's manual Create) is a review.
    snapshot
        .items
        .iter_mut()
        .find(|item| item.id == UI)
        .unwrap()
        .title = "app#3 Fix login".into();
    let context = resolve_for(&snapshot, CHAT_UI, &[], None).expect("strip");
    assert_eq!(as_review(&context).prs, vec![pr_ref("app", 3)]);
}

#[test]
fn a_launcher_review_without_an_item_shows_the_pr_it_was_created_for() {
    let snapshot = snapshot();
    let launched = [link("app", 9, ChatLinkSource::ReviewLauncher)];
    let context = resolve_for(&snapshot, CHAT_LOOSE_A, &launched, None).expect("strip");
    assert_eq!(context.item_id, None);
    assert_eq!(as_review(&context).prs, vec![pr_ref("app", 9)]);
    assert_eq!(as_review(&context).pr_item_id, None);
    // Without a snapshot (a Chat in no Space) it still resolves.
    let context = resolve(&ResolveInput {
        chat_id: "x",
        snapshot: None,
        chat_prs: &launched,
        artifacts: None,
    })
    .expect("strip");
    assert_eq!(as_review(&context).prs, vec![pr_ref("app", 9)]);
}

#[test]
fn a_hand_linked_pr_is_not_a_launched_review() {
    let snapshot = snapshot();
    let manual = [link("app", 9, ChatLinkSource::Manual)];
    assert!(resolve_for(&snapshot, CHAT_LOOSE_A, &manual, None).is_none());
}

#[test]
fn plain_chats_and_transcript_mentions_get_no_strip() {
    let snapshot = snapshot();
    let mentioned = [link("app", 9, ChatLinkSource::Mentioned)];
    assert!(resolve_for(&snapshot, CHAT_LOOSE_A, &[], None).is_none());
    assert!(resolve_for(&snapshot, CHAT_LOOSE_A, &mentioned, None).is_none());
    // A PR the Chat opened itself is not a launched review.
    let created = [link("app", 9, ChatLinkSource::CreatedInChat)];
    assert!(resolve_for(&snapshot, CHAT_LOOSE_A, &created, None).is_none());
    // Dangling, disassociated and orphaned Chats have no item.
    for chat in [CHAT_GHOST, CHAT_LOOSE_B, CHAT_ORPHAN] {
        assert!(resolve_for(&snapshot, chat, &[], None).is_none(), "{chat}");
    }
}

#[test]
fn several_prs_keep_the_earliest_first_and_the_rest_follow() {
    let mut snapshot = snapshot();
    with_review_item(&mut snapshot, &[("app", 8), ("ui", 5), ("app", 8)]);
    let context = resolve_for(&snapshot, CHAT_API, &[], None).expect("strip");
    // The duplicate reference collapses; `+1` is what the strip shows.
    assert_eq!(
        as_review(&context).prs,
        vec![pr_ref("app", 8), pr_ref("ui", 5)]
    );
}

#[test]
fn an_item_or_ancestor_with_a_linear_issue_is_an_implementation_chat() {
    let mut snapshot = snapshot();
    linear_issue_on(&mut snapshot, LAUNCH, "ENG-7");
    let context = resolve_for(&snapshot, CHAT_UI, &[], None).expect("strip");
    let implementation = as_implementation(&context);
    assert_eq!(implementation.ticket.as_ref().unwrap().identifier, "ENG-7");
    assert!(implementation.prs.is_empty(), "No PR yet");
    // Linear off in the Space: no ticket, no strip.
    snapshot.space_settings.linear_enabled = false;
    assert!(resolve_for(&snapshot, CHAT_UI, &[], None).is_none());
}

#[test]
fn implementation_prs_come_from_the_item_and_deliberate_chat_links() {
    let mut snapshot = snapshot();
    linear_issue_on(&mut snapshot, UI, "ENG-7");
    snapshot
        .external_references
        .push(gf::reference("r1", "s1", UI, "ui", 3));
    let chat_links = [
        link("ui", 3, ChatLinkSource::CreatedInChat),
        link("app", 4, ChatLinkSource::CreatedInChat),
        link("app", 5, ChatLinkSource::Mentioned),
    ];
    let context = resolve_for(&snapshot, CHAT_UI, &chat_links, None).expect("strip");
    let implementation = as_implementation(&context);
    assert_eq!(implementation.prs, vec![pr_ref("ui", 3), pr_ref("app", 4)]);
    assert_eq!(implementation.pr_item_id.as_deref(), Some(UI));
    // The ticket and PRs come from a chat link alone, too.
    snapshot
        .external_references
        .retain(|r| r.provider == "linear");
    let context = resolve_for(&snapshot, CHAT_UI, &chat_links, None).expect("strip");
    assert_eq!(
        implementation_prs(&context),
        vec![pr_ref("ui", 3), pr_ref("app", 4)]
    );
    assert_eq!(as_implementation(&context).pr_item_id, None);
}

fn implementation_prs(context: &WorkContext) -> Vec<PullRequestRef> {
    as_implementation(context).prs.clone()
}

#[test]
fn precedence_is_review_then_issue_then_project_or_plan() {
    let mut snapshot = snapshot();
    // Review item + issue + project: review wins, and carries the ticket.
    with_review_item(&mut snapshot, &[("app", 7)]);
    linear_issue_on(&mut snapshot, API, "ENG-1");
    snapshot.external_references.push(lf::reference(
        "lp",
        "s1",
        API,
        "project",
        "p-launch",
        "https://linear.app/acme/project/launch-abc",
    ));
    let context = resolve_for(&snapshot, CHAT_API, &[], None).expect("strip");
    assert_eq!(
        as_review(&context).ticket.as_ref().unwrap().identifier,
        "ENG-1"
    );

    // Issue + project (no PR): implementation wins.
    snapshot.space_settings.pr_review_sync_work_item_id = None;
    snapshot
        .external_references
        .retain(|r| r.provider == "linear");
    let context = resolve_for(&snapshot, CHAT_API, &[], None).expect("strip");
    assert!(matches!(context.variant, Variant::Implementation(_)));

    // Project only: planning.
    snapshot
        .external_references
        .retain(|r| r.reference_type == "project");
    let context = resolve_for(&snapshot, CHAT_API, &[], None).expect("strip");
    assert!(matches!(context.variant, Variant::Planning(_)));
}

fn pinned_plan(id: &str, item: &str, title: &str) -> ArtifactView {
    let mut plan = artifact(id, title, ArtifactKind::Markdown);
    plan.work_item_id = Some(item.into());
    plan.pinned_as_plan = true;
    plan
}

fn other_artifact(id: &str, item: &str, title: &str, updated_at: i64) -> ArtifactView {
    let mut other = artifact(id, title, ArtifactKind::HtmlPage);
    other.work_item_id = Some(item.into());
    other.updated_at = updated_at;
    other
}

#[test]
fn a_project_or_pinned_plan_makes_a_planning_chat() {
    let mut snapshot = snapshot();
    // The store fixture links a PR on `w-api`; this test is about planning.
    snapshot.external_references.clear();
    let artifacts = artifacts_snapshot(vec![
        pinned_plan(&artifact_id(1), LAUNCH, "Launch plan"),
        other_artifact(&artifact_id(2), API_AUTH, "Auth options", 9_000),
        other_artifact(&artifact_id(3), UI, "UI mocks", 3_000),
    ]);
    // The plan of an ancestor counts; the list covers the item's subtree.
    let context = resolve_for(&snapshot, CHAT_API, &[], Some(&artifacts)).expect("strip");
    let planning = as_planning(&context);
    assert_eq!(planning.project, None);
    assert_eq!(planning.plan.as_ref().unwrap().title, "Launch plan");
    assert_eq!(planning.plan.as_ref().unwrap().revision, 2);
    let titles: Vec<&str> = planning
        .artifacts
        .iter()
        .map(|a| a.title.as_str())
        .collect();
    // Plan first (from the ancestor), then the subtree of `w-api`: UI mocks
    // belongs to a sibling and is left out.
    assert_eq!(titles, ["Launch plan", "Auth options"]);

    // A project alone, no artifacts yet: "No plan yet".
    snapshot.external_references.push({
        let mut project = lf::reference(
            "lp",
            "s1",
            LAUNCH,
            "project",
            "p-launch",
            "https://linear.app/acme/project/launch-abc",
        );
        project.cached_title = Some("Launch".into());
        project
    });
    let context = resolve_for(&snapshot, CHAT_UI, &[], None).expect("strip");
    let planning = as_planning(&context);
    assert_eq!(planning.project.as_ref().unwrap().name, "Launch");
    assert_eq!(planning.plan, None);
    assert!(planning.artifacts.is_empty());

    // Neither a project nor a plan: no strip. Other artifacts alone don't count.
    snapshot.external_references.clear();
    let unpinned = artifacts_snapshot(vec![other_artifact(&artifact_id(2), API, "Notes", 1)]);
    assert!(resolve_for(&snapshot, CHAT_API, &[], Some(&unpinned)).is_none());
}

#[test]
fn pr_urls_parse_and_chat_links_prefer_the_durable_list() {
    assert_eq!(
        parse_pr_url("https://github.com/acme/app/pull/12/files?x=1#y"),
        Some(pr_ref("app", 12))
    );
    assert_eq!(parse_pr_url("https://github.com/acme/app/issues/12"), None);
    assert_eq!(parse_pr_url("https://example.com/acme/app/pull/12"), None);
    assert_eq!(parse_pr_url("https://github.com/acme/app/pull/0"), None);

    let status = ChatLinkStatus {
        pr_links: vec![
            PrLinkStatus {
                url: "https://github.com/acme/app/pull/3".into(),
                source: Some(ChatLinkSource::Manual),
                detail: None,
            },
            PrLinkStatus {
                url: "not a url".into(),
                source: None,
                detail: None,
            },
        ],
        ..ChatLinkStatus::default()
    };
    assert_eq!(
        chat_prs(&status),
        vec![ChatPr {
            pr: pr_ref("app", 3),
            source: Some(ChatLinkSource::Manual)
        }]
    );
}
