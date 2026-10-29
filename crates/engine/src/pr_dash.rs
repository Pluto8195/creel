//! Engine side of the PR page: a port of the Cofactr workspace's `pr-dash`
//! (spec: `docs/plans/work-outline/pr-dash-spec.md`).
//!
//! * [`GhRunner`] is the only thing that talks to GitHub (`gh api …`); tests
//!   plug in a canned implementation, so nothing here needs the network.
//! * [`fetch_rows`] runs the two-stage fetch (search, then 5-PR batched reads)
//!   for one tab, retries `mergeable: UNKNOWN`, normalizes, stacks and
//!   classifies.
//! * [`normalize`] is the pure port of `pr-dash`'s `normalize()` (spec 3.1-3.7).
//! * [`PrDash`] is the service held by the RPC layer: a per-tab cache (60 s),
//!   one shared in-flight fetch per tab, and last-good rows on failure.
//!
//! Deliberate deviations from pr-dash: no "inert line" discount of churn (it
//! needs one REST diff request per PR), no file paging beyond the first 100
//! files of a PR, and `mergeable: UNKNOWN` is retried twice rather than three
//! times.

use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use crate::swr::{Entry, SwrStore};
use futures::future::FutureExt;
use mast_proto::{
    PrDashBucket, PrDashCheck, PrDashDiff, PrDashReply, PrDashReviewer, PrDashRow, PrDashTab,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `classify` as a function pointer so tests can inject a trivial one.
pub type ClassifyFn = fn(PrDashTab, &PrDashRow) -> (PrDashBucket, Vec<String>);

/// Per-query result cap (and the cap after the review-requested union).
pub const SEARCH_LIMIT: usize = 200;
/// PRs per stage-two request.
pub const BATCH_SIZE: usize = 5;
/// Concurrent stage-two requests.
pub const BATCH_CONCURRENCY: usize = 4;
/// At most this many `mergeable: UNKNOWN` PRs are re-queried per attempt.
const MERGEABLE_RETRY_MAX_PRS: usize = 60;
const SEARCH_PAGE_SIZE_GUARD: usize = 20;

const BOT_LOGINS: &[&str] = &[
    "greptile-apps",
    "macroscope-app",
    "macroscope-dev",
    "coderabbitai",
    "github-actions",
    "dependabot",
    "sentry-io",
    "codecov",
    "vercel",
    "renovate",
    "claude",
    "cursor",
    "devin-ai-integration",
];

const DEFAULT_BASES: &[&str] = &["main", "master", "develop", "dev"];

const SEARCH_QUERY: &str = "query($q: String!, $after: String) {
  search(query: $q, type: ISSUE, first: 100, after: $after) {
    issueCount
    pageInfo { hasNextPage endCursor }
    nodes { ... on PullRequest { number url updatedAt repository { nameWithOwner } } }
  }
}";

/// PR_FIELDS from spec section 2, verbatim (`contexts` must be selected or the
/// rollup state is wrong).
const PR_FIELDS: &str = "number title url state isDraft
additions deletions changedFiles
createdAt updatedAt mergedAt closedAt
mergeable mergeStateStatus reviewDecision
repository { nameWithOwner }
headRefName baseRefName headRefOid
author { login }
labels(first: 15) { nodes { name color } }
assignees(first: 10) { nodes { login } }
reviewRequests(first: 20) { nodes { requestedReviewer { __typename ... on User { login } ... on Team { name } } } }
reviews(first: 100) { nodes { author { __typename login } state submittedAt bodyText } }
comments(last: 40) { totalCount nodes { author { __typename login } createdAt } }
files(first: 100) { totalCount pageInfo { hasNextPage endCursor } nodes { path additions deletions changeType } }
reviewThreads(first: 100) { nodes { isResolved isOutdated comments(last: 20) { nodes { author { __typename login } } } } }
commits(last: 1) { nodes { commit { statusCheckRollup { state
   contexts(first: 100) { nodes { __typename
      ... on CheckRun { name conclusion status detailsUrl }
      ... on StatusContext { context state targetUrl } } } } } } }";

// ---------------------------------------------------------------------------
// gh plumbing
// ---------------------------------------------------------------------------

/// The GitHub CLI boundary. Errors are human-readable strings (they surface
/// inline on the PR page).
#[async_trait]
pub trait GhRunner: Send + Sync + 'static {
    /// `gh api user --jq .login`.
    async fn current_user(&self) -> Result<String, String>;
    /// `gh api graphql -f query=… -f k=v …`; returns the parsed response body
    /// (`{"data": …}`). A response with usable `data` is returned even when
    /// GitHub also reported per-field `errors`.
    async fn graphql(&self, query: &str, vars: &[(&str, &str)]) -> Result<Value, String>;
}

/// Shells out to `gh` (resolved through the login shell PATH, like the rest of
/// the engine's GitHub calls).
pub struct RealGh;

fn gh_command() -> Option<std::process::Command> {
    let executable = mast_harness::resolve_login_shell_executable("gh")?;
    let mut command = std::process::Command::new(executable);
    mast_harness::compose_login_shell_path_std(&mut command);
    command.env("GH_PROMPT_DISABLED", "1").stdin(Stdio::null());
    Some(command)
}

const GH_TIMEOUT: Duration = Duration::from_secs(90);

async fn run_gh(args: Vec<String>) -> Result<std::process::Output, String> {
    let job = tokio::task::spawn_blocking(move || {
        let mut command =
            gh_command().ok_or_else(|| "the GitHub CLI (gh) was not found".to_string())?;
        command
            .args(&args)
            .output()
            .map_err(|e| format!("could not run gh: {e}"))
    });
    match tokio::time::timeout(GH_TIMEOUT, job).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => Err(format!("gh task failed: {e}")),
        Err(_) => Err("gh timed out".to_string()),
    }
}

fn truncate_message(text: &str) -> String {
    let text = text.trim();
    let mut out: String = text.chars().take(300).collect();
    if text.chars().count() > 300 {
        out.push('…');
    }
    out
}

#[async_trait]
impl GhRunner for RealGh {
    async fn current_user(&self) -> Result<String, String> {
        let output = run_gh(vec![
            "api".into(),
            "user".into(),
            "--jq".into(),
            ".login".into(),
        ])
        .await?;
        let login = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if output.status.success() && !login.is_empty() {
            Ok(login)
        } else {
            Err(truncate_message(&String::from_utf8_lossy(&output.stderr)))
        }
    }

    async fn graphql(&self, query: &str, vars: &[(&str, &str)]) -> Result<Value, String> {
        let mut args: Vec<String> = vec!["api".into(), "graphql".into()];
        args.push("-f".into());
        args.push(format!("query={query}"));
        for (key, value) in vars {
            args.push("-f".into());
            args.push(format!("{key}={value}"));
        }
        let output = run_gh(args).await?;
        // `gh` exits non-zero when the response carries `errors`, even if
        // `data` is mostly there (a deleted repo in one alias, say).
        let parsed: Option<Value> = serde_json::from_slice(&output.stdout).ok();
        if let Some(body) = parsed {
            if body.get("data").is_some_and(|d| !d.is_null()) {
                return Ok(body);
            }
            if let Some(message) = body.pointer("/errors/0/message").and_then(Value::as_str) {
                return Err(truncate_message(message));
            }
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if output.status.success() {
            Err("GitHub returned no data".to_string())
        } else {
            Err(truncate_message(&stderr))
        }
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct PrDashConfig {
    pub classify: ClassifyFn,
    pub cache_ttl: Duration,
    /// Sleep before each `mergeable: UNKNOWN` re-query (1.2 s in pr-dash).
    pub mergeable_sleep: Duration,
    pub mergeable_retries: usize,
    pub search_limit: usize,
    pub batch_size: usize,
    pub concurrency: usize,
}

impl Default for PrDashConfig {
    fn default() -> Self {
        Self {
            classify: mast_proto::classify,
            cache_ttl: Duration::from_secs(60),
            mergeable_sleep: Duration::from_millis(1200),
            mergeable_retries: 2,
            search_limit: SEARCH_LIMIT,
            batch_size: BATCH_SIZE,
            concurrency: BATCH_CONCURRENCY,
        }
    }
}

// ---------------------------------------------------------------------------
// Fetch
// ---------------------------------------------------------------------------

/// `me` for queries and normalization. On failure pr-dash falls back to the
/// literal `@me` (GitHub resolves it in searches; comparisons then never match).
pub async fn resolve_me(gh: &dyn GhRunner) -> String {
    match gh.current_user().await {
        Ok(login) if !login.trim().is_empty() => login.trim().to_string(),
        _ => "@me".to_string(),
    }
}

/// The search qualifier strings of a tab (spec section 1), in union order.
pub fn search_queries(tab: PrDashTab, me: &str) -> Vec<String> {
    match tab {
        PrDashTab::Authored => vec![format!("author:{me} is:pr sort:updated-desc is:open")],
        PrDashTab::ReviewRequested => vec![
            format!("review-requested:{me} is:pr sort:updated-desc is:open"),
            format!("reviewed-by:{me} -author:{me} is:pr sort:updated-desc is:open"),
        ],
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchHit {
    owner: String,
    name: String,
    number: u64,
    url: String,
}

fn parse_search_hit(node: &Value) -> Option<SearchHit> {
    let number = node.get("number")?.as_u64()?;
    let url = node.get("url")?.as_str()?.to_string();
    let full = node.pointer("/repository/nameWithOwner")?.as_str()?;
    let (owner, name) = full.split_once('/')?;
    Some(SearchHit {
        owner: owner.to_string(),
        name: name.to_string(),
        number,
        url,
    })
}

/// One search query, paginated (`first: 100`) until `limit` hits.
async fn search(gh: &dyn GhRunner, query: &str, limit: usize) -> Result<Vec<SearchHit>, String> {
    let mut hits = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..SEARCH_PAGE_SIZE_GUARD {
        let mut vars: Vec<(&str, &str)> = vec![("q", query)];
        if let Some(cursor) = after.as_deref() {
            vars.push(("after", cursor));
        }
        let body = gh.graphql(SEARCH_QUERY, &vars).await?;
        let search = body
            .pointer("/data/search")
            .ok_or_else(|| "GitHub search returned no data".to_string())?;
        if let Some(nodes) = search.get("nodes").and_then(Value::as_array) {
            hits.extend(nodes.iter().filter_map(parse_search_hit));
        }
        let has_next = search
            .pointer("/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let cursor = search
            .pointer("/pageInfo/endCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        if hits.len() >= limit || !has_next || cursor.is_none() || cursor == after {
            break;
        }
        after = cursor;
    }
    hits.truncate(limit);
    Ok(hits)
}

/// Stage one for a tab: the (unioned, deduped, capped) PR list.
async fn search_tab(
    gh: &dyn GhRunner,
    tab: PrDashTab,
    me: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    let queries = search_queries(tab, me);
    let results = futures::future::join_all(queries.iter().map(|q| search(gh, q, limit))).await;
    let mut seen = HashSet::new();
    let mut union = Vec::new();
    for result in results {
        for hit in result? {
            if seen.insert(hit.url.clone()) {
                union.push(hit);
            }
        }
    }
    union.truncate(limit);
    Ok(union)
}

fn graphql_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn batch_query(hits: &[SearchHit], fields: &str) -> String {
    let mut query = String::from("query {\n");
    for (i, hit) in hits.iter().enumerate() {
        query.push_str(&format!(
            "p{i}: repository(owner: {}, name: {}) {{ pullRequest(number: {}) {{ {fields} }} }}\n",
            graphql_string(&hit.owner),
            graphql_string(&hit.name),
            hit.number,
        ));
    }
    query.push('}');
    query
}

/// Run one aliased batch; missing nodes are dropped silently (`None`).
async fn read_batch(
    gh: &dyn GhRunner,
    hits: &[SearchHit],
    fields: &str,
) -> Result<Vec<Option<Value>>, String> {
    let body = gh.graphql(&batch_query(hits, fields), &[]).await?;
    let data = body.get("data").cloned().unwrap_or(Value::Null);
    Ok((0..hits.len())
        .map(|i| {
            data.get(format!("p{i}"))
                .and_then(|alias| alias.get("pullRequest"))
                .filter(|pr| pr.is_object())
                .cloned()
        })
        .collect())
}

/// Stage two: full payloads in search order.
async fn read_prs(
    gh: &dyn GhRunner,
    cfg: &PrDashConfig,
    hits: &[SearchHit],
) -> Result<Vec<Value>, String> {
    let batches: Vec<&[SearchHit]> = hits.chunks(cfg.batch_size.max(1)).collect();
    let mut results: Vec<Result<Vec<Option<Value>>, String>> = Vec::new();
    for group in batches.chunks(cfg.concurrency.max(1)) {
        results.extend(
            futures::future::join_all(group.iter().map(|batch| read_batch(gh, batch, PR_FIELDS)))
                .await,
        );
    }
    let mut prs = Vec::new();
    for result in results {
        prs.extend(result?.into_iter().flatten());
    }
    Ok(prs)
}

/// GitHub computes `mergeable` lazily; re-ask for the PRs that came back
/// UNKNOWN (first 60 per attempt, a pause before each attempt). Best effort:
/// a failed re-query leaves the value as it was.
async fn resolve_mergeable(gh: &dyn GhRunner, cfg: &PrDashConfig, prs: &mut [Value]) {
    for _ in 0..cfg.mergeable_retries {
        let pending: Vec<usize> = prs
            .iter()
            .enumerate()
            .filter(|(_, pr)| pr.get("mergeable").and_then(Value::as_str) == Some("UNKNOWN"))
            .map(|(i, _)| i)
            .take(MERGEABLE_RETRY_MAX_PRS)
            .collect();
        if pending.is_empty() {
            return;
        }
        if !cfg.mergeable_sleep.is_zero() {
            tokio::time::sleep(cfg.mergeable_sleep).await;
        }
        for chunk in pending.chunks(cfg.batch_size.max(1)) {
            let hits: Vec<SearchHit> = chunk
                .iter()
                .filter_map(|&i| {
                    let pr = &prs[i];
                    let full = pr.pointer("/repository/nameWithOwner")?.as_str()?;
                    let (owner, name) = full.split_once('/')?;
                    Some(SearchHit {
                        owner: owner.to_string(),
                        name: name.to_string(),
                        number: pr.get("number")?.as_u64()?,
                        url: String::new(),
                    })
                })
                .collect();
            if hits.len() != chunk.len() {
                continue;
            }
            let Ok(fresh) = read_batch(gh, &hits, "mergeable mergeStateStatus").await else {
                continue;
            };
            for (&i, fresh) in chunk.iter().zip(fresh) {
                let Some(fresh) = fresh else { continue };
                if let Some(mergeable) = fresh.get("mergeable").filter(|m| !m.is_null()) {
                    prs[i]["mergeable"] = mergeable.clone();
                    if let Some(state) = fresh.get("mergeStateStatus") {
                        prs[i]["mergeStateStatus"] = state.clone();
                    }
                }
            }
        }
    }
}

/// Fetch, normalize, stack and classify one tab's rows (no chat links).
pub async fn fetch_rows(
    gh: &dyn GhRunner,
    cfg: &PrDashConfig,
    tab: PrDashTab,
    me: &str,
) -> Result<Vec<PrDashRow>, String> {
    let hits = search_tab(gh, tab, me, cfg.search_limit).await?;
    let mut prs = read_prs(gh, cfg, &hits).await?;
    resolve_mergeable(gh, cfg, &mut prs).await;
    let mut rows: Vec<PrDashRow> = prs.iter().map(|pr| normalize(pr, me, tab)).collect();
    annotate_stacks(&mut rows);
    classify_rows(tab, &mut rows, cfg.classify);
    Ok(rows)
}

/// Set `bucket` / `reasons` on every row.
pub fn classify_rows(tab: PrDashTab, rows: &mut [PrDashRow], classify: ClassifyFn) {
    for row in rows {
        let (bucket, reasons) = classify(tab, row);
        row.bucket = bucket;
        row.reasons = reasons;
    }
}

// ---------------------------------------------------------------------------
// normalize (spec section 3)
// ---------------------------------------------------------------------------

fn str_field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn opt_string(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `v[key].nodes` with nulls dropped.
fn nodes<'a>(v: &'a Value, key: &str) -> Vec<&'a Value> {
    v.get(key)
        .and_then(|c| c.get("nodes"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter(|n| !n.is_null()).collect())
        .unwrap_or_default()
}

/// `(login, __typename)` of an `author { __typename login }` object.
fn actor(v: &Value) -> (&str, &str) {
    match v.get("author") {
        Some(a) if a.is_object() => (str_field(a, "login"), str_field(a, "__typename")),
        _ => ("", ""),
    }
}

fn is_bot(login: &str, typename: &str) -> bool {
    let lo = login.to_lowercase();
    typename == "Bot"
        || lo.ends_with("[bot]")
        || BOT_LOGINS.contains(&lo.as_str())
        || lo.ends_with("-bot")
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `\b((?:ENG|COF|FND|ASK|OPS|BIZ)-\d+)\b`, case-insensitive; first match.
fn find_ticket(text: &str) -> Option<String> {
    const PREFIXES: [&str; 6] = ["ENG", "COF", "FND", "ASK", "OPS", "BIZ"];
    let chars: Vec<char> = text.chars().collect();
    for start in 0..chars.len() {
        if start > 0 && is_word_char(chars[start - 1]) {
            continue;
        }
        if start + 4 > chars.len() {
            break;
        }
        let prefix: String = chars[start..start + 3]
            .iter()
            .collect::<String>()
            .to_uppercase();
        if !PREFIXES.contains(&prefix.as_str()) || chars[start + 3] != '-' {
            continue;
        }
        let digits_start = start + 4;
        let mut end = digits_start;
        while end < chars.len() && chars[end].is_ascii_digit() {
            end += 1;
        }
        if end == digits_start || (end < chars.len() && is_word_char(chars[end])) {
            continue;
        }
        let digits: String = chars[digits_start..end].iter().collect();
        return Some(format!("{prefix}-{digits}"));
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathKind {
    Code,
    Test,
    Generated,
    Docs,
}

fn ends_with_any(s: &str, suffixes: &[&str]) -> bool {
    suffixes.iter().any(|x| s.ends_with(x))
}

/// Path classification (spec 3.6; the regexes of pr-risk's `diff_kinds.json`
/// hand-matched): first of test, generated, docs wins, else code.
fn classify_path(path: &str) -> PathKind {
    let lower = path.to_lowercase();
    let segs: Vec<&str> = lower.split('/').collect();
    let file = *segs.last().unwrap_or(&"");
    let dirs = &segs[..segs.len().saturating_sub(1)];
    let in_dir = |names: &[&str]| dirs.iter().any(|d| names.contains(d));

    // Test.
    let test_helper = file
        .strip_suffix(".tsx")
        .or_else(|| file.strip_suffix(".jsx"))
        .or_else(|| file.strip_suffix(".ts"))
        .or_else(|| file.strip_suffix(".js"))
        .and_then(|stem| stem.strip_prefix("test"))
        .is_some_and(|rest| {
            rest.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                && [
                    "setup", "util", "utils", "helper", "helpers", "harness", "support", "fixture",
                    "fixtures",
                ]
                .iter()
                .any(|alt| rest.starts_with(alt))
        });
    let setup_tests = file
        .strip_suffix(".tsx")
        .or_else(|| file.strip_suffix(".jsx"))
        .or_else(|| file.strip_suffix(".ts"))
        .or_else(|| file.strip_suffix(".js"))
        .is_some_and(|stem| stem == "setuptest" || stem == "setuptests");
    let tool_config = ["vitest", "jest", "playwright", "cypress", "karma"]
        .iter()
        .any(|w| {
            file.strip_prefix(w)
                .is_some_and(|rest| rest.starts_with(['.', '-']))
        });
    if in_dir(&["test", "tests", "__tests__", "e2e", "cypress", "playwright"])
        || in_dir(&["fixture", "fixtures", "testfixture", "testfixtures"])
        || (file.starts_with("test_") && file.ends_with(".py") && file.len() >= "test_x.py".len())
        || file == "test.py"
        || file == "tests.py"
        || file == "conftest.py"
        || ends_with_any(
            file,
            &[
                ".test.js",
                ".test.jsx",
                ".test.ts",
                ".test.tsx",
                ".spec.js",
                ".spec.jsx",
                ".spec.ts",
                ".spec.tsx",
            ],
        )
        || file == "factory.py"
        || file == "factories.py"
        || test_helper
        || setup_tests
        || tool_config
        || file == "pytest.ini"
        || file == "tox.ini"
    {
        return PathKind::Test;
    }

    // Generated, lockfiles, binary assets.
    let versions_migration = segs.windows(2).any(|w| {
        w[0] == "versions" && {
            let hex = w[1].chars().take_while(|c| c.is_ascii_hexdigit()).count();
            hex >= 8 && w[1].chars().nth(hex) == Some('_')
        }
    });
    let generated_infix = file
        .match_indices(".generated.")
        .any(|(i, m)| i + m.len() < file.len());
    let baseline = lower
        .strip_suffix("/baseline.json")
        .map(|head| {
            [".basedpyright", ".quality"].iter().any(|d| {
                head.strip_suffix(d)
                    .is_some_and(|before| before.is_empty() || before.ends_with('/'))
            })
        })
        .unwrap_or(false);
    if in_dir(&["migrations", "__snapshots__", "generated"])
        || versions_migration
        || file.ends_with(".snap")
        || generated_infix
        || baseline
        || ends_with_any(
            file,
            &[
                ".svg", ".png", ".jpg", ".jpeg", ".gif", ".ico", ".woff", ".woff2", ".ttf", ".pdf",
            ],
        )
        || [
            "package-lock.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "uv.lock",
            "poetry.lock",
            "cargo.lock",
            "pipfile.lock",
        ]
        .contains(&file)
    {
        return PathKind::Generated;
    }

    // Docs.
    if ends_with_any(file, &[".md", ".mdx", ".rst", ".txt", ".adoc"])
        || in_dir(&["doc", "docs", ".claude"])
    {
        return PathKind::Docs;
    }
    PathKind::Code
}

/// Per-PR facts: a port of pr-dash's `normalize()` (spec 3.1-3.7).
///
/// `tab` picks the role (`Authored` = author, `ReviewRequested` = reviewer) and
/// `me` is the login every "mine" comparison uses. The returned row carries the
/// default bucket; [`classify_rows`] sets the real one. Stack fields are set by
/// [`annotate_stacks`].
///
/// Diff: no inert-line (comment/blank) churn discount, and only the first 100
/// files of a PR are seen.
pub fn normalize(node: &Value, me: &str, tab: PrDashTab) -> PrDashRow {
    let role_author = tab == PrDashTab::Authored;
    let repo_full = node
        .pointer("/repository/nameWithOwner")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let repo = repo_full.rsplit('/').next().unwrap_or("").to_string();
    let author = node
        .get("author")
        .map(|a| str_field(a, "login"))
        .unwrap_or("")
        .to_string();
    let reviews = nodes(node, "reviews");

    // 3.1 my_review.
    let mut my_review: Option<&str> = None;
    for r in &reviews {
        if actor(r).0 != me {
            continue;
        }
        let state = str_field(r, "state");
        match state {
            "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED" => my_review = Some(state),
            "COMMENTED" if my_review.is_none() => my_review = Some("COMMENTED"),
            _ => {}
        }
    }

    // 3.2 reviewers (insertion-ordered like a Python dict).
    let mut states: Vec<(String, String)> = Vec::new();
    for r in &reviews {
        let (login, typename) = actor(r);
        if login.is_empty() || login == author || login == me || is_bot(login, typename) {
            continue;
        }
        let state = str_field(r, "state");
        let existing = states.iter().position(|(l, _)| l == login);
        match state {
            "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED" => match existing {
                Some(i) => states[i].1 = state.to_string(),
                None => states.push((login.to_string(), state.to_string())),
            },
            "COMMENTED" if existing.is_none() => {
                states.push((login.to_string(), "COMMENTED".to_string()))
            }
            _ => {}
        }
    }
    let mut requested: Vec<String> = Vec::new();
    let mut requested_from_me = false;
    for rr in nodes(node, "reviewRequests") {
        let Some(rev) = rr.get("requestedReviewer").filter(|v| v.is_object()) else {
            continue;
        };
        let login = Some(str_field(rev, "login"))
            .filter(|s| !s.is_empty())
            .or_else(|| Some(str_field(rev, "name")).filter(|s| !s.is_empty()));
        let Some(login) = login else { continue };
        if login == me {
            requested_from_me = true;
            continue;
        }
        if is_bot(login, str_field(rev, "__typename")) {
            continue;
        }
        requested.push(login.to_string());
    }
    let requested_set: HashSet<&str> = requested.iter().map(String::as_str).collect();
    let mut reviewers: Vec<PrDashReviewer> = Vec::new();
    for (login, state) in &states {
        reviewers.push(PrDashReviewer {
            login: login.clone(),
            state: if requested_set.contains(login.as_str()) {
                "RE_REQUESTED".to_string()
            } else {
                state.clone()
            },
        });
    }
    for login in &requested {
        if !states.iter().any(|(l, _)| l == login) {
            reviewers.push(PrDashReviewer {
                login: login.clone(),
                state: "REQUESTED".to_string(),
            });
        }
    }
    reviewers.sort_by_key(|r| (r.state != "CHANGES_REQUESTED", r.login.to_lowercase()));
    let blocking_changes = reviewers.iter().any(|r| r.state == "CHANGES_REQUESTED");

    // 3.3 CI.
    let mut ci: Option<String> = None;
    let mut failing: Vec<PrDashCheck> = Vec::new();
    let mut cancelled: Vec<PrDashCheck> = Vec::new();
    let rollup = nodes(node, "commits")
        .first()
        .and_then(|c| c.pointer("/commit/statusCheckRollup"))
        .filter(|r| r.is_object());
    if let Some(rollup) = rollup {
        ci = opt_string(rollup, "state");
        let contexts = nodes(rollup, "contexts");
        let live: HashSet<&str> = contexts
            .iter()
            .filter(|c| {
                str_field(c, "__typename") == "CheckRun"
                    && str_field(c, "conclusion") != "CANCELLED"
            })
            .map(|c| str_field(c, "name"))
            .collect();
        let mut saw_cancel = false;
        let mut pending = false;
        for c in &contexts {
            match str_field(c, "__typename") {
                "CheckRun" => {
                    let name = str_field(c, "name");
                    let check = PrDashCheck {
                        name: name.to_string(),
                        url: opt_string(c, "detailsUrl"),
                    };
                    let conclusion = str_field(c, "conclusion");
                    if conclusion == "CANCELLED" {
                        saw_cancel = true;
                        if !live.contains(name) {
                            cancelled.push(check);
                        }
                        continue;
                    }
                    if str_field(c, "status") != "COMPLETED" {
                        pending = true;
                    }
                    if matches!(
                        conclusion,
                        "FAILURE" | "TIMED_OUT" | "STARTUP_FAILURE" | "ACTION_REQUIRED"
                    ) {
                        failing.push(check);
                    }
                }
                "StatusContext" => {
                    let state = str_field(c, "state");
                    if matches!(state, "PENDING" | "EXPECTED") {
                        pending = true;
                    }
                    if matches!(state, "FAILURE" | "ERROR") {
                        failing.push(PrDashCheck {
                            name: str_field(c, "context").to_string(),
                            url: opt_string(c, "targetUrl"),
                        });
                    }
                }
                _ => {}
            }
        }
        if saw_cancel {
            ci = Some(
                if !failing.is_empty() {
                    "FAILURE"
                } else if pending {
                    "PENDING"
                } else if !cancelled.is_empty() {
                    "CANCELLED"
                } else {
                    "SUCCESS"
                }
                .to_string(),
            );
        }
    }

    // 3.6 diff (no inert-line discount).
    let mut diff = PrDashDiff {
        additions: node.get("additions").and_then(Value::as_u64).unwrap_or(0),
        deletions: node.get("deletions").and_then(Value::as_u64).unwrap_or(0),
        changed_files: node
            .get("changedFiles")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        ..PrDashDiff::default()
    };
    for f in nodes(node, "files") {
        let adds = f.get("additions").and_then(Value::as_u64).unwrap_or(0);
        let dels = f.get("deletions").and_then(Value::as_u64).unwrap_or(0);
        match classify_path(str_field(f, "path")) {
            PathKind::Test => diff.test_lines += adds + dels,
            PathKind::Generated => diff.generated_lines += adds + dels,
            PathKind::Docs => diff.docs_lines += adds + dels,
            PathKind::Code => {
                if str_field(f, "changeType") == "ADDED" {
                    diff.code_new_lines += adds;
                } else {
                    diff.code_churn += adds + dels;
                }
            }
        }
    }

    // 3.4 inline threads.
    let open_threads: Vec<&Value> = nodes(node, "reviewThreads")
        .into_iter()
        .filter(|t| {
            !t.get("isResolved")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .collect();
    let live_threads: Vec<&&Value> = open_threads
        .iter()
        .filter(|t| {
            !t.get("isOutdated")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .collect();
    let outdated_threads = (open_threads.len() - live_threads.len()) as u64;
    let mut unresolved = 0u64;
    let mut answered_threads = 0u64;
    for thread in live_threads {
        let comments = nodes(thread, "comments");
        let last_mine = comments.iter().rposition(|c| actor(c).0 == me);
        match last_mine {
            None => {
                if role_author {
                    unresolved += 1;
                }
            }
            Some(i) => {
                let reopened = comments[i + 1..].iter().any(|c| {
                    let (login, typename) = actor(c);
                    login != me && !is_bot(login, typename)
                });
                if reopened {
                    unresolved += 1;
                } else {
                    answered_threads += 1;
                }
            }
        }
    }

    // 3.5 top-level unanswered comments.
    let comments_conn = node.get("comments");
    let mut timeline: Vec<(String, String, String)> = Vec::new();
    for c in nodes(node, "comments") {
        let (login, typename) = actor(c);
        timeline.push((
            str_field(c, "createdAt").to_string(),
            login.to_string(),
            typename.to_string(),
        ));
    }
    for r in &reviews {
        let (login, typename) = actor(r);
        let submitted = str_field(r, "submittedAt");
        if submitted.is_empty() {
            continue;
        }
        if login != me
            && (str_field(r, "state") != "COMMENTED" || str_field(r, "bodyText").trim().is_empty())
        {
            continue;
        }
        timeline.push((
            submitted.to_string(),
            login.to_string(),
            typename.to_string(),
        ));
    }
    timeline.sort();
    let last_mine_at = timeline
        .iter()
        .filter(|(_, login, _)| login == me)
        .map(|(ts, _, _)| ts.as_str())
        .max()
        .unwrap_or("")
        .to_string();
    let unanswered_comments = timeline
        .iter()
        .filter(|(ts, login, typename)| {
            ts.as_str() > last_mine_at.as_str()
                && !login.is_empty()
                && !is_bot(login, typename)
                && if role_author {
                    login != me
                } else {
                    *login == author
                }
        })
        .count() as u64;

    // 3.7 other fields.
    let title = str_field(node, "title").to_string();
    let head_ref = str_field(node, "headRefName").to_string();
    let ticket = find_ticket(&title).or_else(|| find_ticket(&head_ref));
    let comments_total = comments_conn
        .and_then(|c| c.get("totalCount"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| nodes(node, "comments").len() as u64);

    PrDashRow {
        number: node.get("number").and_then(Value::as_u64).unwrap_or(0),
        title,
        url: str_field(node, "url").to_string(),
        repo_full,
        repo,
        author,
        state: str_field(node, "state").to_string(),
        draft: node
            .get("isDraft")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        head_ref,
        base_ref: str_field(node, "baseRefName").to_string(),
        ticket,
        labels: nodes(node, "labels")
            .into_iter()
            .map(|l| str_field(l, "name").to_string())
            .collect(),
        created_at: str_field(node, "createdAt").to_string(),
        updated_at: str_field(node, "updatedAt").to_string(),
        diff,
        review_decision: opt_string(node, "reviewDecision"),
        mergeable: opt_string(node, "mergeable"),
        merge_state: opt_string(node, "mergeStateStatus"),
        ci,
        failing_checks: failing,
        cancelled_checks: cancelled,
        reviewers,
        blocking_changes,
        my_review: my_review.map(str::to_string),
        my_last_comment_at: Some(last_mine_at).filter(|s| !s.is_empty()),
        requested_from_me,
        unresolved,
        unanswered_comments,
        answered_threads,
        outdated_threads,
        comments_total,
        stack_pos: 0,
        stack_size: 0,
        bucket: PrDashBucket::default(),
        reasons: Vec::new(),
        chats: Vec::new(),
    }
}

/// `annotate_stacks`: PRs whose base is another fetched PR's head (same repo)
/// form chains; each chain's root is position 1 (nearest `main`).
pub fn annotate_stacks(rows: &mut [PrDashRow]) {
    let mut by_head: HashMap<(&str, &str), usize> = HashMap::new();
    for (i, row) in rows.iter().enumerate() {
        by_head
            .entry((row.repo_full.as_str(), row.head_ref.as_str()))
            .or_insert(i);
    }
    let mut parent: HashMap<usize, usize> = HashMap::new();
    let mut kids: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, row) in rows.iter().enumerate() {
        if let Some(&p) = by_head.get(&(row.repo_full.as_str(), row.base_ref.as_str())) {
            if p != i {
                parent.insert(i, p);
                kids.entry(p).or_default().push(i);
            }
        }
    }
    let mut chains: Vec<Vec<usize>> = Vec::new();
    for i in 0..rows.len() {
        if parent.contains_key(&i) || !kids.contains_key(&i) {
            continue;
        }
        let mut chain = vec![i];
        let mut seen: HashSet<usize> = HashSet::from([i]);
        loop {
            let last = *chain.last().unwrap_or(&i);
            let next = kids
                .get(&last)
                .and_then(|k| k.iter().find(|k| !seen.contains(k)).copied());
            let Some(next) = next else { break };
            seen.insert(next);
            chain.push(next);
        }
        chains.push(chain);
    }
    for row in rows.iter_mut() {
        row.stack_pos = 0;
        row.stack_size = 0;
    }
    for chain in chains {
        let size = chain.len() as u32;
        for (pos, &i) in chain.iter().enumerate() {
            rows[i].stack_pos = pos as u32 + 1;
            rows[i].stack_size = size;
        }
    }
}

/// Rows that pr-dash would call `stacked` (base is not a default branch).
pub fn is_stacked(row: &PrDashRow) -> bool {
    !DEFAULT_BASES.contains(&row.base_ref.as_str())
}

// ---------------------------------------------------------------------------
// Service: cache + shared in-flight fetch
// ---------------------------------------------------------------------------

/// What one tab's fetch produced; stored (and persisted) per tab.
#[derive(Clone, Serialize, Deserialize)]
struct Cached {
    me: String,
    rows: Vec<PrDashRow>,
}

type Store = SwrStore<Cached, String>;

struct Inner {
    gh: Arc<dyn GhRunner>,
    cfg: PrDashConfig,
    me: Mutex<Option<String>>,
    store: Store,
}

/// The PR page's data service. Cheap to clone.
#[derive(Clone)]
pub struct PrDash {
    inner: Arc<Inner>,
}

fn tab_key(tab: PrDashTab) -> &'static str {
    match tab {
        PrDashTab::Authored => "authored",
        PrDashTab::ReviewRequested => "review-requested",
    }
}

fn reply_of(entry: Entry<Cached>, error: Option<String>) -> PrDashReply {
    PrDashReply {
        me: entry.value.me,
        rows: entry.value.rows,
        fetched_at: entry
            .fetched_at
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        error,
    }
}

impl PrDash {
    pub fn new(gh: Arc<dyn GhRunner>, cfg: PrDashConfig) -> Self {
        let store = Store::new(cfg.cache_ttl);
        Self {
            inner: Arc::new(Inner {
                gh,
                cfg,
                me: Mutex::new(None),
                store,
            }),
        }
    }

    /// The real service: `gh`, spec defaults, `mast_proto::classify`.
    pub fn with_real_gh() -> Self {
        Self::new(Arc::new(RealGh), PrDashConfig::default())
    }

    /// Persist the last good rows of each tab to `path` and load what an
    /// earlier run left there, so a cold start has rows to show.
    pub fn with_cache_file(self, path: std::path::PathBuf) -> Self {
        self.inner.store.attach_file(path);
        self
    }

    /// The last good rows of `tab`, however old; `None` before the first
    /// success (this run or a persisted one). Never fetches.
    pub fn peek(&self, tab: PrDashTab) -> Option<PrDashReply> {
        self.inner
            .store
            .peek(tab_key(tab))
            .map(|entry| reply_of(entry, None))
    }

    /// Rows of `tab` (chat links not filled in). Cached per tab for the
    /// config's TTL unless `force`; concurrent callers share one fetch. On
    /// failure the last good rows come back with `error` set, or empty rows
    /// when there is nothing yet.
    pub async fn dashboard(&self, tab: PrDashTab, force: bool) -> PrDashReply {
        let inner = self.inner.clone();
        let result = self
            .inner
            .store
            .get(tab_key(tab), force, move || {
                async move { inner.run_fetch(tab).await }.boxed()
            })
            .await;
        match result {
            Ok(entry) => reply_of(entry, None),
            Err(error) => match self.inner.store.peek(tab_key(tab)) {
                Some(last) => reply_of(last, Some(error)),
                None => PrDashReply {
                    me: self
                        .inner
                        .me
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                        .unwrap_or_default(),
                    rows: Vec::new(),
                    fetched_at: now_rfc3339(),
                    error: Some(error),
                },
            },
        }
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl Inner {
    async fn run_fetch(&self, tab: PrDashTab) -> Result<Cached, String> {
        let cached_me = self.me.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let me = match cached_me {
            Some(me) => me,
            None => {
                let me = resolve_me(self.gh.as_ref()).await;
                if me != "@me" {
                    *self.me.lock().unwrap_or_else(|e| e.into_inner()) = Some(me.clone());
                }
                me
            }
        };
        let rows = fetch_rows(self.gh.as_ref(), &self.cfg, tab, &me).await?;
        Ok(Cached { me, rows })
    }
}

#[cfg(test)]
mod tests;
