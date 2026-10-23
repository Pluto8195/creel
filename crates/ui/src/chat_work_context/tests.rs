//! Work-context strip tests: the pure resolver, the field wording, and the
//! headless render (collapse / expand, click targets, empty states). Every
//! store is filled by hand; nothing reaches GitHub or Linear.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use chrono::{TimeZone, Utc};
use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext, VisualTestContext,
    Window, div, px,
};
use zeron_engine::pr_ticket_cache::{ChatLinkStatus, PrLinkStatus};
use zeron_proto::{
    ArtifactKind, ChatLinkSource, PullRequestCiStatus, PullRequestMyReviewState, PullRequestRef,
    PullRequestReviewDecision, PullRequestState, WorkOutlineSnapshot,
};

use super::strip::{self, Actions, PrFields, StripData};
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
    assert_eq!(as_review(&context).prs, vec![pr_ref("app", 8), pr_ref("ui", 5)]);
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
    snapshot.external_references.retain(|r| r.provider == "linear");
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
    assert_eq!(as_review(&context).ticket.as_ref().unwrap().identifier, "ENG-1");

    // Issue + project (no PR): implementation wins.
    snapshot.space_settings.pr_review_sync_work_item_id = None;
    snapshot.external_references.retain(|r| r.provider == "linear");
    let context = resolve_for(&snapshot, CHAT_API, &[], None).expect("strip");
    assert!(matches!(context.variant, Variant::Implementation(_)));

    // Project only: planning.
    snapshot.external_references.retain(|r| r.reference_type == "project");
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
    let titles: Vec<&str> = planning.artifacts.iter().map(|a| a.title.as_str()).collect();
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

// ----------------------------------------------------------------------
// Field wording
// ----------------------------------------------------------------------

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 17, 0, 0, 0).unwrap()
}

fn ready(status: zeron_proto::PullRequestStatusSummary) -> PrEntry {
    PrEntry::Ready {
        status: Box::new(status),
        at: Instant::now(),
        partial: false,
    }
}

fn fields(status: zeron_proto::PullRequestStatusSummary) -> PrFields {
    let pr = status.pr_ref.clone();
    strip::pr_fields(&pr, Some(&ready(status)), None, now())
}

#[test]
fn the_author_reads_you_for_the_viewers_own_pr() {
    let status = gf::summary("app", 42, "Add auth");
    let pr = status.pr_ref.clone();
    let author = |viewer: Option<&str>| {
        strip::pr_fields(&pr, Some(&ready(status.clone())), viewer, now()).author
    };
    assert_eq!(author(Some("octo")).as_deref(), Some("you"));
    assert_eq!(author(Some("OCTO")).as_deref(), Some("you"));
    assert_eq!(author(Some("sam")).as_deref(), Some("octo"));
    assert_eq!(author(None).as_deref(), Some("octo"));
}

#[test]
fn fields_name_state_author_branches_and_age() {
    let fields = fields(gf::summary("app", 42, "Add auth"));
    assert_eq!(fields.number, "app #42");
    assert_eq!(fields.title.as_deref(), Some("Add auth"));
    assert_eq!(fields.author.as_deref(), Some("octo"));
    assert_eq!(fields.state, Some("Open"));
    assert_eq!(fields.branches.as_deref(), Some("feature-42 \u{2192} main"));
    assert_eq!(fields.updated.as_deref(), Some("3d"));
    assert_eq!(fields.decision, Some("Review required"));
    assert_eq!(fields.reviewers, ["mikey"]);
    assert_eq!(fields.note, None);
}

#[test]
fn fields_follow_each_pr_state() {
    for (state, label, tone) in [
        (PullRequestState::Open, "Open", Tone::Success),
        (PullRequestState::Draft, "Draft", Tone::Muted),
        (PullRequestState::Merged, "Merged", Tone::Merged),
        (PullRequestState::Closed, "Closed", Tone::Danger),
    ] {
        let mut status = gf::summary("app", 1, "T");
        status.state = state;
        let fields = fields(status);
        assert_eq!(fields.state, Some(label));
        assert_eq!(fields.icon, (crate::icons::PULL_REQUEST, tone));
        // A finished PR no longer asks for a review decision.
        assert_eq!(fields.decision.is_some(), !state.is_finished(), "{label}");
    }
}

#[test]
fn fields_follow_each_ci_state() {
    for (ci, expected) in [
        (PullRequestCiStatus::Passing, Some((Tone::Success, "CI passing"))),
        (PullRequestCiStatus::Failing, Some((Tone::Danger, "CI failing"))),
        (PullRequestCiStatus::Pending, Some((Tone::Warning, "CI pending"))),
        (PullRequestCiStatus::None, None),
    ] {
        let mut status = gf::summary("app", 1, "T");
        status.ci_status = ci;
        assert_eq!(fields(status).ci, expected);
    }
}

#[test]
fn fields_follow_each_of_my_review_states() {
    use crate::icons::{CHECK, CLOSE_CIRCLE, EYE, REFRESH};
    for (mine, expected) in [
        (
            PullRequestMyReviewState::Requested,
            Some((EYE, Tone::Attention, "Review requested")),
        ),
        (
            PullRequestMyReviewState::ReRequested,
            Some((REFRESH, Tone::Attention, "Re-review requested")),
        ),
        (
            PullRequestMyReviewState::Approved,
            Some((CHECK, Tone::Success, "Approved by you")),
        ),
        (
            PullRequestMyReviewState::ChangesRequested,
            Some((CLOSE_CIRCLE, Tone::Orange, "Changes requested by you")),
        ),
        (PullRequestMyReviewState::Commented, None),
        (PullRequestMyReviewState::None, None),
    ] {
        let mut status = gf::summary("app", 1, "T");
        status.my_review_state = mine;
        assert_eq!(fields(status).my_review, expected, "{mine:?}");
    }
    let mut status = gf::summary("app", 1, "T");
    status.review_decision = Some(PullRequestReviewDecision::ChangesRequested);
    assert_eq!(fields(status).decision, Some("Changes requested"));
}

#[test]
fn loading_and_failed_entries_keep_the_number_and_a_note() {
    let pr = pr_ref("app", 7);
    let loading = strip::pr_fields(&pr, Some(&PrEntry::Loading), None, now());
    assert_eq!(loading.number, "app #7");
    assert_eq!(loading.title, None);
    assert!(loading.note.as_deref().unwrap().contains("Loading"));
    assert_eq!(strip::pr_fields(&pr, None, None, now()), loading);
    let failed = strip::pr_fields(
        &pr,
        Some(&PrEntry::Failed {
            error: crate::github::GithubUiError::new(
                crate::github::client::GithubFailure::Other,
                "gh failed",
            ),
            at: Instant::now(),
        }),
        None,
        now(),
    );
    assert_eq!(failed.note.as_deref(), Some("gh failed"));
}

#[test]
fn reviewers_and_excerpts_are_cut_for_the_line() {
    let names: Vec<String> = ["a", "b", "c", "d"].map(String::from).to_vec();
    assert_eq!(strip::reviewers_text(&names, 2).as_deref(), Some("a, b +2"));
    assert_eq!(strip::reviewers_text(&names[..2], 2).as_deref(), Some("a, b"));
    assert_eq!(strip::reviewers_text(&[], 2), None);
    assert_eq!(strip::description_excerpt("\n\none\ntwo\nthree\nfour", 3), "one\ntwo\nthree\u{2026}");
    assert_eq!(strip::description_excerpt("one\n\n", 3), "one");
}

#[test]
fn revision_text_names_the_revision_and_age() {
    let brief = ArtifactBrief {
        id: "a".into(),
        title: "Plan".into(),
        kind: "markdown",
        revision: 3,
        at: now().timestamp_millis() - 2 * 86_400_000,
        pinned: true,
    };
    assert_eq!(strip::revision_text(&brief, now()), "r3 \u{00b7} 2d ago");
    let unrevised = ArtifactBrief { revision: 0, ..brief };
    assert_eq!(strip::revision_text(&unrevised, now()), "2d ago");
}

// ----------------------------------------------------------------------
// Headless render
// ----------------------------------------------------------------------

#[derive(Default)]
struct Log {
    urls: Vec<String>,
    items: Vec<String>,
    artifacts: Vec<String>,
    chats: Vec<String>,
}

struct Host {
    context: WorkContext,
    data: StripData,
    expanded: bool,
    log: Rc<RefCell<Log>>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::default();
        let host = cx.entity();
        let (urls, items, artifacts, chats) = (
            self.log.clone(),
            self.log.clone(),
            self.log.clone(),
            self.log.clone(),
        );
        let actions = Actions {
            toggle: Rc::new(move |_, cx| {
                host.update(cx, |host, cx| {
                    host.expanded = !host.expanded;
                    cx.notify();
                })
            }),
            open_url: Rc::new(move |url, _, _| urls.borrow_mut().urls.push(url)),
            open_item: Rc::new(move |item, _, _| items.borrow_mut().items.push(item)),
            open_artifact: Rc::new(move |id, _, _| artifacts.borrow_mut().artifacts.push(id)),
            open_chat: Rc::new(move |id, _, _| chats.borrow_mut().chats.push(id)),
            shell: None,
        };
        div()
            .w(px(900.0))
            .child(strip::render(&self.context, &self.data, self.expanded, &actions, &theme))
    }
}

fn mount(
    cx: &mut TestAppContext,
    context: WorkContext,
    data: StripData,
) -> (gpui::Entity<Host>, Rc<RefCell<Log>>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
    });
    let log = Rc::new(RefCell::new(Log::default()));
    let sink = log.clone();
    let (host, vcx) = cx.add_window_view(move |_, _| Host {
        context,
        data,
        expanded: false,
        log: sink,
    });
    draw(vcx);
    (host, log, vcx)
}

fn draw(vcx: &mut VisualTestContext) {
    vcx.update(|window, cx| window.draw(cx).clear());
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear());
}

fn present(vcx: &mut VisualTestContext, selector: &'static str) -> bool {
    vcx.debug_bounds(selector).is_some()
}

fn click(vcx: &mut VisualTestContext, selector: &'static str) {
    let at = vcx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} renders"))
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    draw(vcx);
}

fn data_for(statuses: &[zeron_proto::PullRequestStatusSummary]) -> StripData {
    StripData {
        prs: statuses
            .iter()
            .map(|status| (status.pr_ref.key(), ready(status.clone())))
            .collect::<HashMap<_, _>>(),
        issue: None,
        project: None,
        viewer: None,
        review_chats: Vec::new(),
        now: now(),
    }
}

fn review_context(prs: &[PullRequestRef]) -> WorkContext {
    WorkContext {
        item_id: Some(API.into()),
        variant: Variant::PrReview(PrReviewContext {
            pr_item_id: Some(API.into()),
            prs: prs.to_vec(),
            ticket: None,
        }),
    }
}

#[gpui::test]
fn the_review_strip_shows_the_pr_fields_on_one_line(cx: &mut TestAppContext) {
    let status = gf::summary("app", 42, "Add auth");
    let (_, _, vcx) = mount(cx, review_context(&[pr_ref("app", 42)]), data_for(&[status]));
    for selector in [
        "work-context-strip",
        "work-context-line",
        "work-context-chevron",
        "work-context-pr-icon",
        "work-context-pr-number",
        "work-context-pr-title",
        "work-context-pr-author",
        "work-context-pr-my-review",
        "work-context-pr-ci",
        "work-context-pr-decision",
        "work-context-pr-reviewers",
        "work-context-pr-branches",
        "work-context-pr-updated",
    ] {
        assert!(present(vcx, selector), "{selector} renders");
    }
    assert!(!present(vcx, "work-context-panel"), "collapsed by default");
    assert!(!present(vcx, "work-context-more"), "one PR has no +N");
    // One line: the strip is a single row tall.
    let strip = vcx.debug_bounds("work-context-strip").unwrap();
    assert!(strip.size.height <= px(24.0), "{:?}", strip.size);
}

#[gpui::test]
fn the_strip_author_reads_you_for_the_viewers_own_pr(cx: &mut TestAppContext) {
    let mut status = gf::summary("app", 42, "Add auth");
    status.author = "alexander-the-great".into();
    let mut data = data_for(&[status]);
    let (host, _, vcx) = mount(cx, review_context(&[pr_ref("app", 42)]), data.clone());
    let width = |vcx: &mut VisualTestContext| {
        f32::from(vcx.debug_bounds("work-context-pr-author").expect("author").size.width)
    };
    let login = width(vcx);
    // Someone else is the viewer, or nobody known: still the login.
    data.viewer = Some("sam".into());
    host.update(vcx, |host, cx| {
        host.data = data.clone();
        cx.notify();
    });
    draw(vcx);
    assert_eq!(width(vcx), login);
    data.viewer = Some("Alexander-The-Great".into());
    host.update(vcx, |host, cx| {
        host.data = data.clone();
        cx.notify();
    });
    draw(vcx);
    assert!(width(vcx) < login, "\"you\" is shorter than the login");
}

#[gpui::test]
fn the_strip_omits_fields_a_pr_does_not_have(cx: &mut TestAppContext) {
    let mut status = gf::summary("app", 42, "Add auth");
    status.state = PullRequestState::Merged;
    status.ci_status = PullRequestCiStatus::None;
    status.my_review_state = PullRequestMyReviewState::None;
    status.requested_reviewers.clear();
    status.head_ref_name = None;
    let (_, _, vcx) = mount(cx, review_context(&[pr_ref("app", 42)]), data_for(&[status]));
    for selector in [
        "work-context-pr-ci",
        "work-context-pr-my-review",
        "work-context-pr-decision",
        "work-context-pr-reviewers",
        "work-context-pr-branches",
    ] {
        assert!(!present(vcx, selector), "{selector} is hidden");
    }
    assert!(present(vcx, "work-context-pr-number"));
}

#[gpui::test]
fn loading_and_failed_entries_render_a_note_not_blank_fields(cx: &mut TestAppContext) {
    let mut data = data_for(&[]);
    data.prs.insert(pr_ref("app", 42).key(), PrEntry::Loading);
    let (host, _, vcx) = mount(cx, review_context(&[pr_ref("app", 42)]), data);
    assert!(present(vcx, "work-context-pr-note"));
    assert!(present(vcx, "work-context-pr-number"));
    assert!(!present(vcx, "work-context-pr-title"));
    // Data arriving replaces the note in place.
    host.update(vcx, |host, cx| {
        host.data = data_for(&[gf::summary("app", 42, "Add auth")]);
        cx.notify();
    });
    draw(vcx);
    assert!(!present(vcx, "work-context-pr-note"));
    assert!(present(vcx, "work-context-pr-title"));
}

#[gpui::test]
fn a_failed_refresh_keeps_the_last_status_in_the_store(cx: &mut TestAppContext) {
    // The strip renders what the store holds, and the store keeps a Ready
    // entry through a failed or repeated pull (`github::store`).
    use std::sync::atomic::{AtomicBool, Ordering};
    let flag = std::sync::Arc::new(AtomicBool::new(false));
    let seen = flag.clone();
    let state = cx.new(|_| crate::state::AppState::new());
    state.update(cx, |state, _| {
        state.work_outline_actions_override =
            Some(crate::work_outline::WorkOutlineActions::scripted(
                std::sync::Arc::new(move |method, _params| {
                    assert_eq!(method, "PrStatus");
                    if seen.load(Ordering::SeqCst) {
                        Err(zeron_rpc::RpcError::Failed("gh exploded".into()))
                    } else {
                        Ok(serde_json::to_value(gf::summary("app", 7, "Add auth")).unwrap())
                    }
                }),
            ));
    });
    let pr = pr_ref("app", 7);
    cx.update(|cx| crate::github::store::request_status(&state, &pr, true, cx));
    cx.run_until_parked();
    let first = state.read_with(cx, |state, _| state.github.status(&pr).cloned());
    let before = strip::pr_fields(&pr, first.as_ref(), None, now());
    assert_eq!(before.title.as_deref(), Some("Add auth"));

    flag.store(true, Ordering::SeqCst);
    cx.update(|cx| crate::github::store::request_status(&state, &pr, true, cx));
    cx.run_until_parked();
    let after = state.read_with(cx, |state, _| state.github.status(&pr).cloned());
    assert_eq!(strip::pr_fields(&pr, after.as_ref(), None, now()), before);
}

#[gpui::test]
fn expanding_and_collapsing_toggles_the_details_panel(cx: &mut TestAppContext) {
    let status = gf::summary("app", 42, "Add auth");
    let (host, _, vcx) = mount(cx, review_context(&[pr_ref("app", 42)]), data_for(&[status]));
    click(vcx, "work-context-chevron");
    assert!(host.read_with(vcx, |host, _| host.expanded));
    assert!(present(vcx, "work-context-panel"));
    for selector in [
        "work-context-panel-pr-0-state",
        "work-context-panel-pr-0-author",
        "work-context-panel-pr-0-ci",
        "work-context-panel-pr-0-decision",
        "work-context-panel-pr-0-my-review",
        "work-context-panel-pr-0-reviewers",
        "work-context-panel-pr-0-branches",
        "work-context-panel-pr-0-updated",
        "work-context-panel-pr-0-item-link",
    ] {
        assert!(present(vcx, selector), "{selector} renders");
    }
    // Clicking the line itself (not a link) collapses it again.
    click(vcx, "work-context-pr-title");
    assert!(!present(vcx, "work-context-panel"));
    click(vcx, "work-context-chevron");
    assert!(present(vcx, "work-context-panel"));
    click(vcx, "work-context-chevron");
    assert!(!present(vcx, "work-context-panel"));
}

#[gpui::test]
fn several_prs_show_the_first_with_a_badge_and_all_in_the_panel(cx: &mut TestAppContext) {
    let statuses = [
        gf::summary("app", 8, "First"),
        gf::summary("ui", 5, "Second"),
        gf::summary("api", 2, "Third"),
    ];
    let prs: Vec<_> = statuses.iter().map(|s| s.pr_ref.clone()).collect();
    let (_, _, vcx) = mount(cx, review_context(&prs), data_for(&statuses));
    assert!(present(vcx, "work-context-more"));
    assert!(present(vcx, "work-context-pr-number"));
    assert!(!present(vcx, "work-context-panel-pr-0-number"));
    click(vcx, "work-context-chevron");
    for selector in [
        "work-context-panel-pr-0-number",
        "work-context-panel-pr-1-number",
        "work-context-panel-pr-2-number",
    ] {
        assert!(present(vcx, selector), "{selector} renders");
    }
}

#[gpui::test]
fn clicking_the_number_opens_the_pr_without_toggling(cx: &mut TestAppContext) {
    let status = gf::summary("app", 42, "Add auth");
    let (host, log, vcx) = mount(cx, review_context(&[pr_ref("app", 42)]), data_for(&[status]));
    click(vcx, "work-context-pr-number");
    assert_eq!(log.borrow().urls, ["https://github.com/acme/app/pull/42"]);
    assert!(!host.read_with(vcx, |host, _| host.expanded), "link click is not a toggle");
    // The panel's number and the outline link route too.
    click(vcx, "work-context-chevron");
    click(vcx, "work-context-panel-pr-0-number");
    assert_eq!(log.borrow().urls.len(), 2);
    click(vcx, "work-context-panel-pr-0-item-link");
    assert_eq!(log.borrow().items, [API]);
}

#[gpui::test]
fn the_review_strip_shows_the_linked_ticket_chip(cx: &mut TestAppContext) {
    let mut context = review_context(&[pr_ref("app", 42)]);
    if let Variant::PrReview(review) = &mut context.variant {
        review.ticket = Some(IssueRef {
            identifier: "ENG-7".into(),
            url: "https://linear.app/acme/issue/eng-7/x".into(),
        });
    }
    let mut data = data_for(&[gf::summary("app", 42, "Add auth")]);
    data.issue = Some(IssueEntry::Ready {
        issue: lf::issue("ENG-7", "Add auth"),
        at: Instant::now(),
    });
    let (_, log, vcx) = mount(cx, context, data);
    assert!(present(vcx, "work-context-ticket"));
    click(vcx, "work-context-chevron");
    assert!(present(vcx, "work-context-panel-ticket"));
    assert!(present(vcx, "work-context-panel-ticket-link"));
    click(vcx, "work-context-panel-ticket-link");
    assert_eq!(log.borrow().urls, ["https://linear.app/acme/issue/eng-7/x"]);
}

fn review_chat(id: &str, title: &str, state: zeron_proto::ChatIndicator) -> crate::review_chats::ReviewChat {
    crate::review_chats::ReviewChat {
        chat_id: id.into(),
        title: title.into(),
        state,
        updated: None,
        reason: crate::review_chats::ReviewReason::Assigned,
    }
}

#[gpui::test]
fn a_pr_with_a_review_chat_shows_the_being_reviewed_marker(cx: &mut TestAppContext) {
    let mut data = data_for(&[gf::summary("app", 42, "Add auth")]);
    let (_, _, vcx) = mount(cx, review_context(&[pr_ref("app", 42)]), data.clone());
    assert!(!present(vcx, "work-context-reviewing"), "no review chat, no marker");

    data.review_chats = vec![
        review_chat("chat-new", "Review app#42", zeron_proto::ChatIndicator::Working),
        review_chat("chat-old", "Earlier", zeron_proto::ChatIndicator::Idle),
    ];
    let (_, log, vcx) = mount(cx, review_context(&[pr_ref("app", 42)]), data);
    assert!(present(vcx, "work-context-reviewing"));
    // A click opens the most recent review chat without toggling the strip.
    click(vcx, "work-context-reviewing");
    assert_eq!(log.borrow().chats, ["chat-new"]);
    assert!(!present(vcx, "work-context-panel"));
}

#[gpui::test]
fn the_implementation_strip_marks_a_pr_under_review_too(cx: &mut TestAppContext) {
    let mut data = data_for(&[gf::summary("app", 42, "Add auth")]);
    data.review_chats = vec![review_chat(
        "chat-r",
        "Review app#42",
        zeron_proto::ChatIndicator::AwaitingInput,
    )];
    let (_, log, vcx) = mount(cx, implementation_context(&[pr_ref("app", 42)]), data);
    assert!(present(vcx, "work-context-reviewing"));
    click(vcx, "work-context-reviewing");
    assert_eq!(log.borrow().chats, ["chat-r"]);
}

fn implementation_context(prs: &[PullRequestRef]) -> WorkContext {
    WorkContext {
        item_id: Some(UI.into()),
        variant: Variant::Implementation(ImplementationContext {
            pr_item_id: None,
            ticket: Some(IssueRef {
                identifier: "ENG-7".into(),
                url: "https://linear.app/acme/issue/eng-7/x".into(),
            }),
            prs: prs.to_vec(),
        }),
    }
}

#[gpui::test]
fn the_implementation_strip_shows_the_ticket_and_its_prs(cx: &mut TestAppContext) {
    let mut data = data_for(&[gf::summary("app", 42, "Add auth")]);
    data.issue = Some(IssueEntry::Ready {
        issue: lf::issue("ENG-7", "Add auth"),
        at: Instant::now(),
    });
    let (_, log, vcx) = mount(cx, implementation_context(&[pr_ref("app", 42)]), data);
    for selector in [
        "work-context-ticket",
        "work-context-ticket-title",
        "work-context-pr-number",
        "work-context-pr-state",
        "work-context-pr-ci",
    ] {
        assert!(present(vcx, selector), "{selector} renders");
    }
    assert!(!present(vcx, "work-context-no-pr"));
    // The compact PR leaves the review-only fields to the panel.
    assert!(!present(vcx, "work-context-pr-author"));
    click(vcx, "work-context-pr-number");
    assert_eq!(log.borrow().urls, ["https://github.com/acme/app/pull/42"]);
    click(vcx, "work-context-chevron");
    assert!(present(vcx, "work-context-panel-ticket-description"));
    assert!(present(vcx, "work-context-panel-pr-0-number"));
}

#[gpui::test]
fn the_implementation_strip_says_no_pr_yet(cx: &mut TestAppContext) {
    let (_, _, vcx) = mount(cx, implementation_context(&[]), data_for(&[]));
    assert!(present(vcx, "work-context-no-pr"));
    assert!(present(vcx, "work-context-ticket"));
    assert!(!present(vcx, "work-context-pr-number"));
}

fn brief(id: &str, title: &str, pinned: bool) -> ArtifactBrief {
    ArtifactBrief {
        id: id.into(),
        title: title.into(),
        kind: "markdown",
        revision: 2,
        at: now().timestamp_millis() - 3_600_000,
        pinned,
    }
}

fn planning_context(
    project: bool,
    plan: Option<ArtifactBrief>,
    artifacts: Vec<ArtifactBrief>,
) -> WorkContext {
    WorkContext {
        item_id: Some(UI.into()),
        variant: Variant::Planning(PlanningContext {
            project: project.then(|| ProjectRef {
                id: "p-launch".into(),
                name: "Launch".into(),
                url: "https://linear.app/acme/project/launch-abc".into(),
            }),
            plan,
            artifacts,
        }),
    }
}

#[gpui::test]
fn the_planning_strip_shows_project_plan_and_other_artifacts(cx: &mut TestAppContext) {
    let plan = brief("plan-1", "Launch plan", true);
    let context = planning_context(
        true,
        Some(plan.clone()),
        vec![plan, brief("a-2", "Auth options", false), brief("a-3", "Mocks", false)],
    );
    let mut data = data_for(&[]);
    data.project = Some(lf::project("p-launch", "Launch"));
    let (_, log, vcx) = mount(cx, context, data);
    for selector in [
        "work-context-project",
        "work-context-project-state",
        "work-context-plan",
        "work-context-plan-revision",
        "work-context-artifact-count",
    ] {
        assert!(present(vcx, selector), "{selector} renders");
    }
    assert!(!present(vcx, "work-context-no-plan"));
    click(vcx, "work-context-plan");
    assert_eq!(log.borrow().artifacts, ["plan-1"]);

    click(vcx, "work-context-chevron");
    for selector in [
        "work-context-panel-project-link",
        "work-context-panel-artifact-0",
        "work-context-panel-artifact-1",
        "work-context-panel-artifact-2",
    ] {
        assert!(present(vcx, selector), "{selector} renders");
    }
    click(vcx, "work-context-panel-artifact-2");
    assert_eq!(log.borrow().artifacts, ["plan-1", "a-3"]);
    click(vcx, "work-context-panel-project-link");
    assert_eq!(log.borrow().urls, ["https://linear.app/acme/project/launch-abc"]);
}

#[gpui::test]
fn the_planning_strip_says_no_plan_yet(cx: &mut TestAppContext) {
    let (_, _, vcx) = mount(cx, planning_context(true, None, Vec::new()), data_for(&[]));
    assert!(present(vcx, "work-context-no-plan"));
    assert!(present(vcx, "work-context-project"));
    assert!(!present(vcx, "work-context-plan"));
    assert!(!present(vcx, "work-context-artifact-count"));
    click(vcx, "work-context-chevron");
    assert!(present(vcx, "work-context-panel"));
}
