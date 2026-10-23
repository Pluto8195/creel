//! Offline tests: every GitHub call goes through [`FakeGh`].

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use super::*;

const PR_FULL: &str = include_str!("../../tests/fixtures/pr_dash/pr_full.json");

fn base_pr() -> Value {
    serde_json::from_str(PR_FULL).expect("fixture parses")
}

fn pr_node(repo: &str, number: u64, head: &str, base: &str) -> Value {
    let mut pr = base_pr();
    pr["number"] = json!(number);
    pr["url"] = json!(format!("https://github.com/{repo}/pull/{number}"));
    pr["repository"] = json!({ "nameWithOwner": repo });
    pr["headRefName"] = json!(head);
    pr["baseRefName"] = json!(base);
    pr
}

fn set_contexts(pr: &mut Value, state: &str, contexts: Value) {
    pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"] =
        json!({ "state": state, "contexts": { "nodes": contexts } });
}

fn authored(pr: &Value) -> PrDashRow {
    normalize(pr, "alice", PrDashTab::Authored)
}

fn state_of<'a>(row: &'a PrDashRow, login: &str) -> Option<&'a str> {
    row.reviewers
        .iter()
        .find(|r| r.login == login)
        .map(|r| r.state.as_str())
}

// ----- normalize ------------------------------------------------------------

#[test]
fn normalize_author_role_full_fixture() {
    let row = authored(&base_pr());
    assert_eq!(row.number, 12);
    assert_eq!(row.repo_full, "Acme/widgets");
    assert_eq!(row.repo, "widgets");
    assert_eq!(row.author, "alice");
    assert_eq!(row.state, "OPEN");
    assert!(!row.draft);
    assert_eq!(row.ticket.as_deref(), Some("ENG-45"));
    assert_eq!(row.labels, vec!["risk:low", "backend"]);
    assert_eq!(row.review_decision.as_deref(), Some("REVIEW_REQUIRED"));
    assert_eq!(row.mergeable.as_deref(), Some("MERGEABLE"));
    assert_eq!(row.merge_state.as_deref(), Some("CLEAN"));
    assert_eq!(row.comments_total, 4);
    assert_eq!(row.my_review.as_deref(), Some("COMMENTED"));
    assert_eq!(
        row.my_last_comment_at.as_deref(),
        Some("2026-10-10T10:00:00Z")
    );
    assert!(!row.requested_from_me);
    assert_eq!(row.bucket, PrDashBucket::InReview);
    assert!(row.reasons.is_empty());
}

#[test]
fn reviewers_states_requests_and_bots() {
    let row = authored(&base_pr());
    // dave approved; bob CHANGES_REQUESTED then COMMENTED (no downgrade) but
    // re-requested; carol only commented; erin and the team are pending asks;
    // greptile (bot), the author and a PENDING review never appear.
    let got: Vec<(&str, &str)> = row
        .reviewers
        .iter()
        .map(|r| (r.login.as_str(), r.state.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("bob", "RE_REQUESTED"),
            ("carol", "COMMENTED"),
            ("dave", "APPROVED"),
            ("erin", "REQUESTED"),
            ("platform", "REQUESTED"),
        ]
    );
    assert!(!row.blocking_changes);
}

#[test]
fn blocking_comes_from_reviewer_verdicts_not_review_decision() {
    // Re-requested reviewer: GitHub still says CHANGES_REQUESTED, pr-dash does not block.
    let mut pr = base_pr();
    pr["reviewDecision"] = json!("CHANGES_REQUESTED");
    assert!(!authored(&pr).blocking_changes);

    // Not re-requested: blocks and sorts first.
    pr["reviewRequests"]["nodes"] = json!([]);
    pr["reviewDecision"] = json!("REVIEW_REQUIRED");
    let row = authored(&pr);
    assert!(row.blocking_changes);
    assert_eq!(row.reviewers[0].login, "bob");
    assert_eq!(row.reviewers[0].state, "CHANGES_REQUESTED");
}

#[test]
fn later_approval_replaces_changes_requested_and_comment_does_not_downgrade() {
    let mut pr = base_pr();
    pr["reviewRequests"]["nodes"] = json!([]);
    pr["reviews"]["nodes"] = json!([
        { "author": { "__typename": "User", "login": "bob" }, "state": "CHANGES_REQUESTED", "submittedAt": "2026-10-03T10:00:00Z", "bodyText": "x" },
        { "author": { "__typename": "User", "login": "bob" }, "state": "APPROVED", "submittedAt": "2026-10-04T10:00:00Z", "bodyText": "" },
        { "author": { "__typename": "User", "login": "bob" }, "state": "COMMENTED", "submittedAt": "2026-10-05T10:00:00Z", "bodyText": "later" },
        { "author": { "__typename": "User", "login": "carol" }, "state": "DISMISSED", "submittedAt": "2026-10-05T10:00:00Z", "bodyText": "" },
    ]);
    let row = authored(&pr);
    assert_eq!(state_of(&row, "bob"), Some("APPROVED"));
    assert_eq!(state_of(&row, "carol"), Some("DISMISSED"));
    assert!(!row.blocking_changes);
}

#[test]
fn my_review_keeps_verdict_over_later_comment() {
    let mut pr = base_pr();
    pr["reviews"]["nodes"] = json!([
        { "author": { "__typename": "User", "login": "mikey" }, "state": "COMMENTED", "submittedAt": "2026-10-03T10:00:00Z", "bodyText": "a" },
        { "author": { "__typename": "User", "login": "mikey" }, "state": "APPROVED", "submittedAt": "2026-10-04T10:00:00Z", "bodyText": "" },
        { "author": { "__typename": "User", "login": "mikey" }, "state": "COMMENTED", "submittedAt": "2026-10-05T10:00:00Z", "bodyText": "b" },
        { "author": { "__typename": "User", "login": "mikey" }, "state": "PENDING", "submittedAt": null, "bodyText": "" },
    ]);
    let row = normalize(&pr, "mikey", PrDashTab::ReviewRequested);
    assert_eq!(row.my_review.as_deref(), Some("APPROVED"));
    assert!(row.reviewers.iter().all(|r| r.login != "mikey"));
}

#[test]
fn requested_from_me_is_not_a_reviewer_and_team_requests_never_set_it() {
    let row = normalize(&base_pr(), "erin", PrDashTab::ReviewRequested);
    assert!(row.requested_from_me);
    assert!(row.reviewers.iter().all(|r| r.login != "erin"));
    // Teams are listed by name; they only match `me` if a login equals the team name.
    let row = normalize(&base_pr(), "zed", PrDashTab::ReviewRequested);
    assert!(!row.requested_from_me);
    assert_eq!(state_of(&row, "platform"), Some("REQUESTED"));
}

#[test]
fn ci_rollup_state_is_kept_without_cancelled_runs() {
    let mut pr = base_pr();
    set_contexts(
        &mut pr,
        "FAILURE",
        json!([{ "__typename": "CheckRun", "name": "build", "conclusion": "SUCCESS", "status": "COMPLETED" }]),
    );
    let row = authored(&pr);
    assert_eq!(row.ci.as_deref(), Some("FAILURE"));
    assert!(row.failing_checks.is_empty());

    pr["commits"]["nodes"] = json!([{ "commit": { "statusCheckRollup": null } }]);
    assert_eq!(authored(&pr).ci, None);
    pr["commits"]["nodes"] = json!([]);
    assert_eq!(authored(&pr).ci, None);
}

#[test]
fn ci_failing_and_cancelled_lists_from_fixture() {
    let row = authored(&base_pr());
    assert_eq!(row.ci.as_deref(), Some("FAILURE"));
    assert_eq!(
        row.failing_checks,
        vec![PrDashCheck {
            name: "lint".into(),
            url: Some("https://ci.example.test/lint".into())
        }]
    );
    assert_eq!(row.cancelled_checks.len(), 1);
    assert_eq!(row.cancelled_checks[0].name, "e2e");
}

#[test]
fn ci_cancel_override() {
    let cancelled = json!({ "__typename": "CheckRun", "name": "e2e", "conclusion": "CANCELLED", "status": "COMPLETED", "detailsUrl": "u" });
    let mut pr = base_pr();

    // Only a cancelled run: GitHub says FAILURE, pr-dash says CANCELLED.
    set_contexts(&mut pr, "FAILURE", json!([cancelled]));
    let row = authored(&pr);
    assert_eq!(row.ci.as_deref(), Some("CANCELLED"));
    assert!(row.failing_checks.is_empty());
    assert_eq!(row.cancelled_checks.len(), 1);

    // Something still running wins over CANCELLED.
    set_contexts(
        &mut pr,
        "FAILURE",
        json!([cancelled, { "__typename": "CheckRun", "name": "build", "conclusion": null, "status": "IN_PROGRESS" }]),
    );
    assert_eq!(authored(&pr).ci.as_deref(), Some("PENDING"));

    // A same-named run that was not cancelled supersedes it.
    set_contexts(
        &mut pr,
        "FAILURE",
        json!([cancelled, { "__typename": "CheckRun", "name": "e2e", "conclusion": "SUCCESS", "status": "COMPLETED" }]),
    );
    let row = authored(&pr);
    assert_eq!(row.ci.as_deref(), Some("SUCCESS"));
    assert!(row.cancelled_checks.is_empty());

    // A real failure wins over everything.
    set_contexts(
        &mut pr,
        "FAILURE",
        json!([cancelled, { "__typename": "StatusContext", "context": "ci/x", "state": "ERROR", "targetUrl": "t" }]),
    );
    let row = authored(&pr);
    assert_eq!(row.ci.as_deref(), Some("FAILURE"));
    assert_eq!(row.failing_checks[0].name, "ci/x");
    assert_eq!(row.failing_checks[0].url.as_deref(), Some("t"));
}

#[test]
fn ci_failing_conclusions_and_status_contexts() {
    let mut pr = base_pr();
    set_contexts(
        &mut pr,
        "FAILURE",
        json!([
            { "__typename": "CheckRun", "name": "a", "conclusion": "TIMED_OUT", "status": "COMPLETED" },
            { "__typename": "CheckRun", "name": "b", "conclusion": "ACTION_REQUIRED", "status": "COMPLETED" },
            { "__typename": "CheckRun", "name": "c", "conclusion": "STARTUP_FAILURE", "status": "COMPLETED" },
            { "__typename": "CheckRun", "name": "d", "conclusion": "NEUTRAL", "status": "COMPLETED" },
            { "__typename": "CheckRun", "name": "e", "conclusion": "SKIPPED", "status": "COMPLETED" },
            { "__typename": "StatusContext", "context": "s", "state": "FAILURE" },
        ]),
    );
    let names: Vec<_> = authored(&pr)
        .failing_checks
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, vec!["a", "b", "c", "s"]);
}

#[test]
fn inline_threads_author_role() {
    let row = authored(&base_pr());
    // never-answered (dave) + reopened (alice, dave) count; dave/alice and
    // alice/bot are answered; one outdated; the resolved one is ignored.
    assert_eq!(row.unresolved, 2);
    assert_eq!(row.answered_threads, 2);
    assert_eq!(row.outdated_threads, 1);
}

#[test]
fn inline_threads_reviewer_role() {
    let row = normalize(&base_pr(), "dave", PrDashTab::ReviewRequested);
    // Only the thread dave spoke on and alice then answered is reopened; the
    // thread dave never touched does not count for a reviewer.
    assert_eq!(row.unresolved, 1);
    assert_eq!(row.answered_threads, 2);
    assert_eq!(row.outdated_threads, 1);
    assert_eq!(row.my_review.as_deref(), Some("APPROVED"));
}

#[test]
fn unanswered_comments_timeline_author_role() {
    // alice last spoke 10-10; only erin's later issue comment counts. bob's
    // "nit" review comment is older, github-actions is a bot.
    let row = authored(&base_pr());
    assert_eq!(row.unanswered_comments, 1);

    // Counts entries, not people.
    let mut pr = base_pr();
    pr["comments"]["nodes"].as_array_mut().unwrap().extend([
        json!({ "author": { "__typename": "User", "login": "erin" }, "createdAt": "2026-10-12T10:00:00Z" }),
        json!({ "author": { "__typename": "User", "login": "frank" }, "createdAt": "2026-10-13T10:00:00Z" }),
    ]);
    assert_eq!(authored(&pr).unanswered_comments, 3);

    // Any review of mine resets the clock, even without a body.
    pr["reviews"]["nodes"].as_array_mut().unwrap().push(json!(
        { "author": { "__typename": "User", "login": "alice" }, "state": "APPROVED", "submittedAt": "2026-10-14T10:00:00Z", "bodyText": "" }
    ));
    assert_eq!(authored(&pr).unanswered_comments, 0);
}

#[test]
fn unanswered_comments_timeline_reviewer_role_only_counts_author() {
    // dave last spoke 10-08; alice (the author) said something later; erin
    // (another reviewer) did too, which is not addressed to dave.
    let row = normalize(&base_pr(), "dave", PrDashTab::ReviewRequested);
    assert_eq!(row.unanswered_comments, 1);
    assert_eq!(
        row.my_last_comment_at.as_deref(),
        Some("2026-10-08T10:00:00Z")
    );
}

#[test]
fn review_comment_bodies_count_only_when_commented_with_text() {
    let mut pr = base_pr();
    pr["comments"]["nodes"] = json!([]);
    pr["reviews"]["nodes"] = json!([
        { "author": { "__typename": "User", "login": "dave" }, "state": "APPROVED", "submittedAt": "2026-10-02T10:00:00Z", "bodyText": "lgtm" },
        { "author": { "__typename": "User", "login": "bob" }, "state": "CHANGES_REQUESTED", "submittedAt": "2026-10-03T10:00:00Z", "bodyText": "fix" },
        { "author": { "__typename": "User", "login": "carol" }, "state": "COMMENTED", "submittedAt": "2026-10-06T10:00:00Z", "bodyText": "  \n" },
        { "author": { "__typename": "User", "login": "erin" }, "state": "COMMENTED", "submittedAt": "2026-10-06T11:00:00Z", "bodyText": "question?" },
    ]);
    let row = authored(&pr);
    assert_eq!(row.unanswered_comments, 1);
    assert_eq!(row.my_last_comment_at, None);
}

#[test]
fn diff_split_by_kind() {
    let row = authored(&base_pr());
    assert_eq!(row.diff.additions, 400);
    assert_eq!(row.diff.deletions, 40);
    assert_eq!(row.diff.changed_files, 6);
    assert_eq!(row.diff.code_churn, 120); // cache.rs: 100 + 20
    assert_eq!(row.diff.code_new_lines, 200); // new_widget.rs (ADDED)
    assert_eq!(row.diff.test_lines, 70);
    assert_eq!(row.diff.generated_lines, 20); // Cargo.lock; logo.png has 0
    assert_eq!(row.diff.docs_lines, 30);
}

#[test]
fn new_files_only_have_no_churn_and_deletions_of_new_files_are_ignored() {
    let mut pr = base_pr();
    pr["files"]["nodes"] = json!([
        { "path": "src/a.rs", "additions": 50, "deletions": 3, "changeType": "ADDED" },
        { "path": "src/b.rs", "additions": 7, "deletions": 0 },
    ]);
    let row = authored(&pr);
    assert_eq!(row.diff.code_new_lines, 50);
    // Absent changeType reads as churn (the conservative direction).
    assert_eq!(row.diff.code_churn, 7);
    for change in ["DELETED", "RENAMED", "COPIED", "CHANGED"] {
        pr["files"]["nodes"] =
            json!([{ "path": "src/a.rs", "additions": 1, "deletions": 2, "changeType": change }]);
        assert_eq!(authored(&pr).diff.code_churn, 3, "{change}");
    }
}

#[test]
fn path_classification() {
    use PathKind::*;
    let cases = [
        ("src/lib.rs", Code),
        ("app/main.py", Code),
        ("tests/foo.py", Test),
        ("pkg/test/foo.py", Test),
        ("src/__tests__/a.ts", Test),
        ("pkg/test_models.py", Test),
        ("pkg/test_.py", Code),
        ("pkg/tests.py", Test),
        ("pkg/conftest.py", Test),
        ("web/Button.test.tsx", Test),
        ("web/Button.spec.js", Test),
        ("web/Button.test.css", Code),
        ("e2e/login.ts", Test),
        ("pkg/factories.py", Test),
        ("pkg/fixtures/a.json", Test),
        ("pkg/testfixtures/a.json", Test),
        ("web/testUtils.ts", Test),
        ("web/testhelpers2.tsx", Test),
        ("web/testing.ts", Code),
        ("web/setupTests.ts", Test),
        ("web/vitest.config.ts", Test),
        ("web/jest-setup.js", Test),
        ("web/vitesting.ts", Code),
        ("pytest.ini", Test),
        ("api/migrations/0001_init.py", Generated),
        ("alembic/versions/0123abcd_add.py", Generated),
        ("alembic/versions/0123abc_add.py", Code),
        ("web/__snapshots__/a.snap", Generated),
        ("web/a.snap", Generated),
        ("web/schema.generated.ts", Generated),
        ("web/schema.generated.", Code),
        ("web/generated/api.ts", Generated),
        (".basedpyright/baseline.json", Generated),
        ("api/.quality/baseline.json", Generated),
        ("api/x.quality/baseline.json", Code),
        ("assets/logo.PNG", Generated),
        ("assets/font.woff2", Generated),
        ("Cargo.lock", Generated),
        ("web/pnpm-lock.yaml", Generated),
        ("README.md", Docs),
        ("notes.txt", Docs),
        ("docs/design.html", Docs),
        (".claude/skills/a.py", Docs),
        ("docs/tests/a.md", Test),
    ];
    for (path, kind) in cases {
        assert_eq!(classify_path(path), kind, "{path}");
    }
}

#[test]
fn ticket_extraction() {
    assert_eq!(find_ticket("ENG-45 do it").as_deref(), Some("ENG-45"));
    assert_eq!(find_ticket("fix (cof-7): thing").as_deref(), Some("COF-7"));
    assert_eq!(find_ticket("a ops-12").as_deref(), Some("OPS-12"));
    assert_eq!(find_ticket("ENG-12abc"), None);
    assert_eq!(find_ticket("xENG-1"), None);
    assert_eq!(find_ticket("ENG-"), None);
    assert_eq!(find_ticket("FOO-12"), None);
    assert_eq!(find_ticket("see biz-3 and eng-4").as_deref(), Some("BIZ-3"));

    // Title first, then the branch.
    let mut pr = base_pr();
    pr["title"] = json!("no ticket here");
    pr["headRefName"] = json!("alice/fnd-88-cache");
    assert_eq!(authored(&pr).ticket.as_deref(), Some("FND-88"));
    pr["headRefName"] = json!("alice/cache");
    assert_eq!(authored(&pr).ticket, None);
}

#[test]
fn bot_detection() {
    assert!(is_bot("anything", "Bot"));
    assert!(is_bot("renovate[bot]", "User"));
    assert!(is_bot("Dependabot", "User"));
    assert!(is_bot("release-bot", "User"));
    assert!(is_bot("Greptile-Apps", "User"));
    assert!(!is_bot("alice", "User"));
    assert!(!is_bot("robot-lover", ""));
}

#[test]
fn bot_requested_reviewers_are_hidden() {
    let mut pr = base_pr();
    pr["reviewRequests"]["nodes"] = json!([
        { "requestedReviewer": { "__typename": "Bot", "login": "copilot-pull-request-reviewer" } },
        { "requestedReviewer": { "__typename": "User", "login": "coderabbitai" } },
        { "requestedReviewer": null },
    ]);
    let row = authored(&pr);
    assert!(state_of(&row, "copilot-pull-request-reviewer").is_none());
    assert!(state_of(&row, "coderabbitai").is_none());
}

#[test]
fn stacks_form_chains_per_repo() {
    let mut rows: Vec<PrDashRow> = [
        ("Acme/widgets", 3, "feat-c", "feat-b"),
        ("Acme/widgets", 1, "feat-a", "main"),
        ("Acme/widgets", 2, "feat-b", "feat-a"),
        ("Acme/other", 9, "feat-b", "feat-a"), // same branch names, other repo
        ("Acme/widgets", 4, "solo", "main"),
        ("Acme/widgets", 5, "orphan", "not-fetched"),
    ]
    .into_iter()
    .map(|(repo, n, head, base)| authored(&pr_node(repo, n, head, base)))
    .collect();
    annotate_stacks(&mut rows);
    let pos: Vec<(u64, u32, u32)> = rows
        .iter()
        .map(|r| (r.number, r.stack_pos, r.stack_size))
        .collect();
    assert_eq!(
        pos,
        vec![
            (3, 3, 3),
            (1, 1, 3),
            (2, 2, 3),
            (9, 0, 0),
            (4, 0, 0),
            (5, 0, 0)
        ]
    );
    assert!(is_stacked(&rows[5]));
    assert!(!is_stacked(&rows[1]));
}

#[test]
fn stack_cycle_terminates() {
    let mut rows = vec![
        authored(&pr_node("A/b", 1, "x", "y")),
        authored(&pr_node("A/b", 2, "y", "x")),
    ];
    annotate_stacks(&mut rows);
    assert!(rows.iter().all(|r| r.stack_pos == 0));
}

// ----- fake gh ----------------------------------------------------------------

#[derive(Debug, Clone)]
struct Call {
    query: String,
    vars: Vec<(String, String)>,
}

impl Call {
    fn var(&self, key: &str) -> Option<&str> {
        self.vars
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
    fn is_search(&self) -> bool {
        self.query.contains("search(")
    }
    fn is_batch(&self) -> bool {
        self.query.contains("reviewThreads")
    }
    fn is_retry(&self) -> bool {
        self.query.contains("repository(owner") && !self.is_batch()
    }
}

#[derive(Default)]
struct FakeGh {
    user: Mutex<Option<Result<String, String>>>,
    /// search qualifier -> hits `(owner/name, number)` in order
    searches: Mutex<HashMap<String, Vec<(String, u64)>>>,
    prs: Mutex<HashMap<String, Value>>,
    calls: Mutex<Vec<Call>>,
    fail: AtomicBool,
    delay: Duration,
    /// answer to the `mergeable` re-query (None: still UNKNOWN)
    retry_mergeable: Option<&'static str>,
}

impl FakeGh {
    fn new() -> Self {
        Self {
            user: Mutex::new(Some(Ok("alice".into()))),
            ..Self::default()
        }
    }

    fn add_pr(&self, node: Value) {
        let key = format!(
            "{}#{}",
            node["repository"]["nameWithOwner"].as_str().unwrap(),
            node["number"]
        );
        self.prs.lock().unwrap().insert(key, node);
    }

    fn add_search(&self, query: &str, hits: Vec<(String, u64)>) {
        self.searches.lock().unwrap().insert(query.into(), hits);
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn search_calls(&self) -> Vec<Call> {
        self.calls().into_iter().filter(Call::is_search).collect()
    }

    fn batch_calls(&self) -> Vec<Call> {
        self.calls().into_iter().filter(Call::is_batch).collect()
    }
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let from = text.find(start).unwrap() + start.len();
    let rest = &text[from..];
    &rest[..rest.find(end).unwrap()]
}

#[async_trait]
impl GhRunner for FakeGh {
    async fn current_user(&self) -> Result<String, String> {
        self.user
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Err("no user".into()))
    }

    async fn graphql(&self, query: &str, vars: &[(&str, &str)]) -> Result<Value, String> {
        let call = Call {
            query: query.to_string(),
            vars: vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        };
        self.calls.lock().unwrap().push(call.clone());
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err("boom".into());
        }
        if call.is_search() {
            let hits = self
                .searches
                .lock()
                .unwrap()
                .get(call.var("q").unwrap())
                .cloned()
                .unwrap_or_default();
            let offset: usize = call
                .var("after")
                .map(|c| c.trim_start_matches('c').parse().unwrap())
                .unwrap_or(0);
            let end = (offset + 100).min(hits.len());
            let nodes: Vec<Value> = hits[offset.min(end)..end]
                .iter()
                .map(|(repo, n)| {
                    json!({ "number": n, "url": format!("https://github.com/{repo}/pull/{n}"),
                            "updatedAt": "2026-10-20T09:00:00Z", "repository": { "nameWithOwner": repo } })
                })
                .collect();
            return Ok(json!({ "data": { "search": {
                "issueCount": hits.len(),
                "pageInfo": { "hasNextPage": end < hits.len(), "endCursor": format!("c{end}") },
                "nodes": nodes,
            } } }));
        }
        let mut data = serde_json::Map::new();
        for (i, part) in query.split("repository(owner: \"").skip(1).enumerate() {
            let owner = between(&format!("\"{part}"), "\"", "\"").to_string();
            let name = between(part, "name: \"", "\"");
            let number: u64 = between(part, "pullRequest(number: ", ")").parse().unwrap();
            let key = format!("{owner}/{name}#{number}");
            let pr = self.prs.lock().unwrap().get(&key).cloned();
            let pr = match (pr, call.is_retry(), self.retry_mergeable) {
                (Some(_), true, Some(m)) => {
                    json!({ "mergeable": m, "mergeStateStatus": "CLEAN" })
                }
                (Some(_), true, None) => {
                    json!({ "mergeable": "UNKNOWN", "mergeStateStatus": "UNKNOWN" })
                }
                (Some(pr), false, _) => pr,
                (None, _, _) => Value::Null,
            };
            data.insert(format!("p{i}"), json!({ "pullRequest": pr }));
        }
        Ok(json!({ "data": data }))
    }
}

fn test_classify(tab: PrDashTab, row: &PrDashRow) -> (PrDashBucket, Vec<String>) {
    match (tab, row.draft) {
        (PrDashTab::Authored, true) => (PrDashBucket::Draft, vec![]),
        (PrDashTab::Authored, false) => (PrDashBucket::InReview, vec!["test".to_string()]),
        (PrDashTab::ReviewRequested, _) => (PrDashBucket::ToReview, vec![]),
    }
}

fn cfg() -> PrDashConfig {
    PrDashConfig {
        classify: test_classify,
        mergeable_sleep: Duration::ZERO,
        ..PrDashConfig::default()
    }
}

fn hits(repo: &str, range: std::ops::Range<u64>) -> Vec<(String, u64)> {
    range.map(|n| (repo.to_string(), n)).collect()
}

fn add_numbered_prs(gh: &FakeGh, repo: &str, range: std::ops::Range<u64>) {
    for n in range {
        gh.add_pr(pr_node(repo, n, &format!("b{n}"), "main"));
    }
}

// ----- search / fetch -----------------------------------------------------------

const AUTHORED_Q: &str = "author:alice is:pr sort:updated-desc is:open";
const REQUESTED_Q: &str = "review-requested:alice is:pr sort:updated-desc is:open";
const REVIEWED_Q: &str = "reviewed-by:alice -author:alice is:pr sort:updated-desc is:open";

#[test]
fn search_query_strings_match_the_spec() {
    assert_eq!(
        search_queries(PrDashTab::Authored, "alice"),
        vec![AUTHORED_Q]
    );
    assert_eq!(
        search_queries(PrDashTab::ReviewRequested, "alice"),
        vec![REQUESTED_Q, REVIEWED_Q]
    );
}

#[tokio::test]
async fn authored_fetch_passes_exact_query_and_keeps_search_order() {
    let gh = FakeGh::new();
    gh.add_search(
        AUTHORED_Q,
        vec![("Acme/widgets".into(), 7), ("Acme/widgets".into(), 3)],
    );
    add_numbered_prs(&gh, "Acme/widgets", 3..8);
    let rows = fetch_rows(&gh, &cfg(), PrDashTab::Authored, "alice")
        .await
        .unwrap();
    assert_eq!(
        rows.iter().map(|r| r.number).collect::<Vec<_>>(),
        vec![7, 3]
    );
    let searches = gh.search_calls();
    assert_eq!(searches.len(), 1);
    assert_eq!(searches[0].var("q"), Some(AUTHORED_Q));
    assert_eq!(searches[0].var("after"), None);
    assert!(
        searches[0]
            .query
            .contains("search(query: $q, type: ISSUE, first: 100, after: $after)")
    );
    // test classifier ran
    assert_eq!(rows[0].bucket, PrDashBucket::InReview);
    assert_eq!(rows[0].reasons, vec!["test"]);
}

#[tokio::test]
async fn review_requested_unions_dedupes_and_puts_requested_first() {
    let gh = FakeGh::new();
    gh.add_search(
        REQUESTED_Q,
        vec![("Acme/widgets".into(), 1), ("Acme/widgets".into(), 2)],
    );
    gh.add_search(
        REVIEWED_Q,
        vec![("Acme/widgets".into(), 2), ("Acme/widgets".into(), 5)],
    );
    add_numbered_prs(&gh, "Acme/widgets", 1..6);
    let rows = fetch_rows(&gh, &cfg(), PrDashTab::ReviewRequested, "alice")
        .await
        .unwrap();
    assert_eq!(
        rows.iter().map(|r| r.number).collect::<Vec<_>>(),
        vec![1, 2, 5]
    );
    let queries: HashSet<String> = gh
        .search_calls()
        .iter()
        .map(|c| c.var("q").unwrap().to_string())
        .collect();
    assert_eq!(
        queries,
        HashSet::from([REQUESTED_Q.to_string(), REVIEWED_Q.to_string()])
    );
    // The incoming role is used: nothing is classified as authored.
    assert!(rows.iter().all(|r| r.bucket == PrDashBucket::ToReview));
}

#[tokio::test]
async fn search_paginates_to_200_and_union_is_truncated() {
    let gh = FakeGh::new();
    gh.add_search(REQUESTED_Q, hits("Acme/widgets", 0..250));
    gh.add_search(REVIEWED_Q, hits("Acme/widgets", 1000..1010));
    // No PR bodies needed: stage two just yields nothing.
    let found = search_tab(&gh, PrDashTab::ReviewRequested, "alice", 200)
        .await
        .unwrap();
    assert_eq!(found.len(), 200);
    assert_eq!(found[0].number, 0);
    assert_eq!(found[199].number, 199);
    // reviewed-by hits are the dropped tail.
    assert!(found.iter().all(|h| h.number < 1000));
    let requested_pages: Vec<_> = gh
        .search_calls()
        .into_iter()
        .filter(|c| c.var("q") == Some(REQUESTED_Q))
        .collect();
    assert_eq!(requested_pages.len(), 2);
    assert_eq!(requested_pages[1].var("after"), Some("c100"));
}

#[tokio::test]
async fn stage_two_batches_five_prs_per_request_with_verbatim_fields() {
    let gh = FakeGh::new();
    gh.add_search(AUTHORED_Q, hits("Acme/widgets", 1..13));
    add_numbered_prs(&gh, "Acme/widgets", 1..13);
    let rows = fetch_rows(&gh, &cfg(), PrDashTab::Authored, "alice")
        .await
        .unwrap();
    assert_eq!(rows.len(), 12);
    assert_eq!(
        rows.iter().map(|r| r.number).collect::<Vec<_>>(),
        (1..13).collect::<Vec<_>>()
    );
    let mut sizes: Vec<usize> = gh
        .batch_calls()
        .iter()
        .map(|c| c.query.matches("pullRequest(number:").count())
        .collect();
    sizes.sort();
    assert_eq!(sizes, vec![2, 5, 5]);
    let first = &gh.batch_calls()[0].query;
    assert!(first.contains("p4: repository(owner: \"Acme\", name: \"widgets\")"));
    assert!(!first.contains("p5:") || gh.batch_calls().iter().all(|c| !c.query.contains("p5:")));
    for call in gh.batch_calls() {
        assert!(call.query.contains("contexts(first: 100)"));
        assert!(call.query.contains("reviews(first: 100) { nodes { author { __typename login } state submittedAt bodyText } }"));
        assert!(call.query.contains("comments(last: 40)"));
        assert!(call.query.contains(
            "reviewThreads(first: 100) { nodes { isResolved isOutdated comments(last: 20)"
        ));
    }
}

#[tokio::test]
async fn missing_pr_nodes_are_dropped() {
    let gh = FakeGh::new();
    gh.add_search(AUTHORED_Q, hits("Acme/widgets", 1..4));
    gh.add_pr(pr_node("Acme/widgets", 1, "a", "main"));
    gh.add_pr(pr_node("Acme/widgets", 3, "c", "main"));
    let rows = fetch_rows(&gh, &cfg(), PrDashTab::Authored, "alice")
        .await
        .unwrap();
    assert_eq!(
        rows.iter().map(|r| r.number).collect::<Vec<_>>(),
        vec![1, 3]
    );
}

#[tokio::test]
async fn me_falls_back_to_at_me() {
    let gh = FakeGh::new();
    *gh.user.lock().unwrap() = Some(Err("not logged in".into()));
    assert_eq!(resolve_me(&gh).await, "@me");
    let dash = PrDash::new(Arc::new(gh), cfg());
    let reply = dash.dashboard(PrDashTab::Authored, false).await;
    assert_eq!(reply.me, "@me");
    assert!(reply.rows.is_empty());
}

#[tokio::test]
async fn mergeable_unknown_is_retried_and_bounded() {
    let gh = FakeGh {
        retry_mergeable: Some("CONFLICTING"),
        ..FakeGh::new()
    };
    gh.add_search(AUTHORED_Q, hits("Acme/widgets", 1..3));
    for n in 1..3 {
        let mut pr = pr_node("Acme/widgets", n, "a", "main");
        pr["mergeable"] = json!(if n == 1 { "UNKNOWN" } else { "MERGEABLE" });
        gh.add_pr(pr);
    }
    let rows = fetch_rows(&gh, &cfg(), PrDashTab::Authored, "alice")
        .await
        .unwrap();
    assert_eq!(rows[0].mergeable.as_deref(), Some("CONFLICTING"));
    assert_eq!(rows[0].merge_state.as_deref(), Some("CLEAN"));
    assert_eq!(rows[1].mergeable.as_deref(), Some("MERGEABLE"));
    assert_eq!(gh.calls().iter().filter(|c| c.is_retry()).count(), 1);

    // Still UNKNOWN after the retries: stays UNKNOWN, retried at most twice.
    let gh = FakeGh::new();
    gh.add_search(AUTHORED_Q, hits("Acme/widgets", 1..2));
    let mut pr = pr_node("Acme/widgets", 1, "a", "main");
    pr["mergeable"] = json!("UNKNOWN");
    gh.add_pr(pr);
    let rows = fetch_rows(&gh, &cfg(), PrDashTab::Authored, "alice")
        .await
        .unwrap();
    assert_eq!(rows[0].mergeable.as_deref(), Some("UNKNOWN"));
    assert_eq!(gh.calls().iter().filter(|c| c.is_retry()).count(), 2);
}

#[tokio::test]
async fn fetch_with_the_real_classifier() {
    let gh = FakeGh::new();
    gh.add_search(AUTHORED_Q, hits("Acme/widgets", 12..13));
    gh.add_pr(base_pr());
    let config = PrDashConfig {
        mergeable_sleep: Duration::ZERO,
        ..PrDashConfig::default()
    };
    let rows = fetch_rows(&gh, &config, PrDashTab::Authored, "alice")
        .await
        .unwrap();
    // The fixture's lint check fails.
    assert_eq!(rows[0].bucket, PrDashBucket::NeedsYou);
    assert!(rows[0].reasons.iter().any(|r| r.contains("failing")));
}

// ----- service ----------------------------------------------------------------------

fn dash_with(gh: Arc<FakeGh>, config: PrDashConfig) -> PrDash {
    PrDash::new(gh, config)
}

fn simple_gh() -> Arc<FakeGh> {
    let gh = FakeGh::new();
    gh.add_search(AUTHORED_Q, hits("Acme/widgets", 1..3));
    add_numbered_prs(&gh, "Acme/widgets", 1..3);
    Arc::new(gh)
}

#[tokio::test]
async fn cache_serves_a_second_call_within_the_ttl() {
    let gh = simple_gh();
    let dash = dash_with(gh.clone(), cfg());
    let first = dash.dashboard(PrDashTab::Authored, false).await;
    let calls = gh.calls().len();
    let second = dash.dashboard(PrDashTab::Authored, false).await;
    assert_eq!(gh.calls().len(), calls);
    assert_eq!(first, second);
    assert_eq!(first.me, "alice");
    assert_eq!(first.rows.len(), 2);
    assert_eq!(first.error, None);
    assert!(!first.fetched_at.is_empty());

    // force skips the cache.
    dash.dashboard(PrDashTab::Authored, true).await;
    assert!(gh.calls().len() > calls);

    // The tabs cache independently.
    let before = gh.calls().len();
    dash.dashboard(PrDashTab::ReviewRequested, false).await;
    assert!(gh.calls().len() > before);
}

#[tokio::test]
async fn expired_cache_refetches() {
    let gh = simple_gh();
    let dash = dash_with(
        gh.clone(),
        PrDashConfig {
            cache_ttl: Duration::ZERO,
            ..cfg()
        },
    );
    dash.dashboard(PrDashTab::Authored, false).await;
    let calls = gh.calls().len();
    dash.dashboard(PrDashTab::Authored, false).await;
    assert!(gh.calls().len() > calls);
}

#[tokio::test]
async fn failure_returns_last_good_rows_with_the_error() {
    let gh = simple_gh();
    let dash = dash_with(gh.clone(), cfg());
    let good = dash.dashboard(PrDashTab::Authored, false).await;
    assert_eq!(good.error, None);

    gh.fail.store(true, Ordering::SeqCst);
    let failed = dash.dashboard(PrDashTab::Authored, true).await;
    assert_eq!(failed.error.as_deref(), Some("boom"));
    assert_eq!(failed.rows, good.rows);
    assert_eq!(failed.fetched_at, good.fetched_at);

    // The failure was not cached as good: recovery works.
    gh.fail.store(false, Ordering::SeqCst);
    let recovered = dash.dashboard(PrDashTab::Authored, false).await;
    assert_eq!(recovered.error, None);
    assert_eq!(recovered.rows.len(), 2);
}

#[tokio::test]
async fn failure_without_rows_returns_empty_rows_and_the_error() {
    let gh = simple_gh();
    gh.fail.store(true, Ordering::SeqCst);
    let dash = dash_with(gh.clone(), cfg());
    let reply = dash.dashboard(PrDashTab::Authored, false).await;
    assert!(reply.rows.is_empty());
    assert_eq!(reply.error.as_deref(), Some("boom"));
}

#[tokio::test]
async fn concurrent_calls_share_one_fetch() {
    let gh = FakeGh {
        delay: Duration::from_millis(40),
        ..FakeGh::new()
    };
    gh.add_search(AUTHORED_Q, hits("Acme/widgets", 1..3));
    add_numbered_prs(&gh, "Acme/widgets", 1..3);
    let gh = Arc::new(gh);
    let dash = dash_with(gh.clone(), cfg());
    let (a, b, c) = tokio::join!(
        dash.dashboard(PrDashTab::Authored, true),
        dash.dashboard(PrDashTab::Authored, true),
        dash.dashboard(PrDashTab::Authored, false),
    );
    assert_eq!(gh.search_calls().len(), 1);
    assert_eq!(a, b);
    assert_eq!(b, c);
    assert_eq!(a.rows.len(), 2);
}
