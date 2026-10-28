//! PR review sync (contract `docs/plans/work-outline/pr-sync-contract.md`).
//!
//! For every Space with a destination (`setPrReviewSyncDestination`) that
//! this device owns, every open PR requesting the user's review in one of the
//! Space's GitHub repositories (its checkout, submodules, nested
//! repositories) gets a child work item under the destination, a
//! `github`/`pullRequest` reference, a sync record (`externalSyncState`),
//! and one system-origin Required Action per review round. All writes are
//! the system actor through the ordinary mutation path; ids derive from the
//! Space and the PR, so passes and devices converge.
//!
//! [`PrReviewSyncService`] also serves the `PR_*` RPCs: `PrStatus` (a ~2
//! minute per-PR cache), `PrMyOpen` and `PrReviewRequested` (a ~1 minute
//! search cache), and `PrReviewSyncNow`. Everything goes through the `gh`
//! CLI (login-shell PATH, `GH_PROMPT_DISABLED=1`); nothing is written to
//! GitHub.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde_json::Value;
use zeron_proto::pr_review_sync::{
    GITHUB_PROVIDER, GITHUB_PULL_REQUEST_REFERENCE, GithubError, GithubErrorCode,
    PR_RESOLUTION_CLOSED, PR_RESOLUTION_MERGED, PR_RESOLUTION_REVIEW_SUBMITTED,
    PR_RESOLUTION_WITHDRAWN, PrReviewRequestState, PrReviewSyncNowReply, PrStatusParams,
    PullRequestCiStatus, PullRequestMyReviewState, PullRequestRef, PullRequestReviewDecision,
    PullRequestState, PullRequestStatusSummary, WorkItemExternalSyncState, pr_item_title,
    pr_review_dedupe_key,
};
use zeron_proto::work_outline::{
    ArchiveWorkItemParams, CreateWorkItemExternalReferenceParams, CreateWorkItemParams,
    DescendantArchivePolicy, RestoreWorkItemParams, UpdateWorkItemParams, WorkItemArchiveCause,
    WorkItemChanges, WorkItemStatus, WorkOutlineActorKind, WorkOutlineErrorCode,
    WorkOutlinePlacement, derive_id, limits, roles,
};
use zeron_proto::work_outline_user_actions::{
    CreateUserActionRequestParams, ResolveUserActionRequestParams, UserActionOriginKind,
    UserActionRequestOrigin, user_action_limits,
};
use zeron_proto::{
    Space, WorkOutlineError, WorkOutlineMutateReply, WorkOutlineMutation, WorkOutlineSnapshot,
};

use crate::pr_ticket_cache::{ChecksStatus, MyOpenPr};
use crate::repos::Repos;
use crate::work_outline::WorkOutlineService;
use crate::workspace_host::WorkspaceHost;

/// Background cadence.
pub const SYNC_INTERVAL: Duration = Duration::from_secs(3 * 60);
/// First background pass after boot (lets the registry sync first).
const FIRST_SYNC_DELAY: Duration = Duration::from_secs(60);
/// How long `PrStatus` serves a fetched PR.
pub const STATUS_CACHE_TTL: Duration = Duration::from_secs(2 * 60);
/// How long a `gh search prs` result is reused.
pub const SEARCH_CACHE_TTL: Duration = Duration::from_secs(60);
/// How long a Space's repository set is reused.
const REPO_SET_TTL: Duration = Duration::from_secs(10 * 60);
/// Upper bound on one `gh` call.
const GH_TIMEOUT: Duration = Duration::from_secs(60);
/// Results per `gh search prs`.
const SEARCH_LIMIT: &str = "100";
/// Clock skew allowed when deciding whether a review belongs to this round.
const ROUND_SKEW_MS: i64 = 10 * 60 * 1000;

const PR_VIEW_FIELDS: &str = "number,title,url,state,isDraft,author,reviewDecision,reviews,\
reviewRequests,statusCheckRollup,headRefName,baseRefName,updatedAt";
const SEARCH_FIELDS: &str = "number,title,url,repository,isDraft,updatedAt,author";
const REVIEW_REQUESTED: &str = "--review-requested=@me";
const AUTHORED: &str = "--author=@me";

/// Everything the service talks to (swappable for tests).
#[derive(Debug, Clone)]
pub struct PrReviewSyncConfig {
    /// `gh` executable; `None` = resolve from the login-shell PATH.
    pub gh: Option<PathBuf>,
    pub status_ttl: Duration,
    pub search_ttl: Duration,
    /// Whether the background loop syncs (tests turn it off).
    pub background: bool,
}

impl Default for PrReviewSyncConfig {
    fn default() -> Self {
        Self {
            gh: None,
            status_ttl: STATUS_CACHE_TTL,
            search_ttl: SEARCH_CACHE_TTL,
            background: true,
        }
    }
}

impl PrReviewSyncConfig {
    /// A fixed `gh` (a fake) and no background passes.
    pub fn for_tests(gh: impl Into<PathBuf>) -> Self {
        Self {
            gh: Some(gh.into()),
            background: false,
            ..Self::default()
        }
    }
}

/// One fetched PR with what the sync needs beyond the summary.
#[derive(Debug, Clone)]
struct PrDetail {
    summary: PullRequestStatusSummary,
    /// The user has submitted at least one (non-pending) review.
    reviewed_by_me: bool,
    /// Epoch ms of the user's latest review.
    my_latest_review_at: Option<i64>,
    /// The user (not a team) is a requested reviewer.
    requested_me: bool,
}

struct Inner {
    workspace: WorkspaceHost,
    repos: Repos,
    work_outline: Arc<WorkOutlineService>,
    config: RwLock<PrReviewSyncConfig>,
    viewer: Mutex<Option<String>>,
    statuses: Mutex<HashMap<String, (Instant, PrDetail)>>,
    searches: Mutex<HashMap<&'static str, (Instant, Vec<MyOpenPr>)>>,
    repo_sets: Mutex<HashMap<String, (Instant, BTreeSet<String>)>>,
    /// Stored Chat transcripts, for the work item PR link backfill
    /// (`work_outline_pr_links`); `None` = no backfill.
    transcripts: RwLock<Option<crate::work_outline_pr_links::TranscriptReader>>,
    /// One sync pass at a time (background, `PrReviewSyncNow`, destination
    /// set), so two passes never race on the same derived ids.
    passes: tokio::sync::Mutex<()>,
}

/// Cheap to clone; one per engine.
#[derive(Clone)]
pub struct PrReviewSyncService {
    inner: Arc<Inner>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn github(code: GithubErrorCode, message: impl Into<String>) -> GithubError {
    GithubError::new(code, message)
}

fn invalid(message: impl Into<String>) -> GithubError {
    github(GithubErrorCode::Invalid, message)
}

/// Failures that stop a whole pass (every later call would fail the same way).
fn is_fatal(error: &GithubError) -> bool {
    matches!(
        error.code,
        GithubErrorCode::GhMissing | GithubErrorCode::Unauthorized | GithubErrorCode::RateLimited
    )
}

/// Map a failed `gh` call's stderr to a code.
pub fn classify_gh_failure(stderr: &str) -> GithubErrorCode {
    let text = stderr.to_ascii_lowercase();
    if text.contains("rate limit") || text.contains("http 429") || text.contains("abuse detection")
    {
        GithubErrorCode::RateLimited
    } else if text.contains("gh auth login")
        || text.contains("http 401")
        || text.contains("bad credentials")
        || text.contains("authentication")
        || text.contains("not logged")
    {
        GithubErrorCode::Unauthorized
    } else if text.contains("could not resolve to")
        || text.contains("http 404")
        || text.contains("not found")
    {
        GithubErrorCode::NotFound
    } else {
        GithubErrorCode::Network
    }
}

fn str_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(key)?;
    }
    cursor.as_str()
}

fn parse_time_ms(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|t| t.timestamp_millis())
}

/// Parse `gh pr view --json <PR_VIEW_FIELDS>` for `viewer`.
fn parse_detail(data: &Value, fallback: &PullRequestRef, viewer: &str) -> PrDetail {
    let viewer_lc = viewer.to_ascii_lowercase();
    let is_me = |login: Option<&str>| login.is_some_and(|l| l.to_ascii_lowercase() == viewer_lc);
    let number = data
        .get("number")
        .and_then(Value::as_u64)
        .unwrap_or(fallback.number);
    let mut pr_ref = PullRequestRef::new(fallback.owner.clone(), fallback.repo.clone(), number);
    if let Some(url) = data.get("url").and_then(Value::as_str) {
        pr_ref.url = url.to_owned();
    }
    let is_draft = data
        .get("isDraft")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let state = match data.get("state").and_then(Value::as_str).unwrap_or("") {
        s if s.eq_ignore_ascii_case("merged") => PullRequestState::Merged,
        s if s.eq_ignore_ascii_case("closed") => PullRequestState::Closed,
        _ if is_draft => PullRequestState::Draft,
        _ => PullRequestState::Open,
    };
    let ci_status = match crate::pr_ticket_cache::summarize_checks(data.get("statusCheckRollup")) {
        Some(ChecksStatus::Passing) => PullRequestCiStatus::Passing,
        Some(ChecksStatus::Failing) => PullRequestCiStatus::Failing,
        Some(ChecksStatus::Pending) => PullRequestCiStatus::Pending,
        None => PullRequestCiStatus::None,
    };
    let review_decision = match data.get("reviewDecision").and_then(Value::as_str) {
        Some("APPROVED") => Some(PullRequestReviewDecision::Approved),
        Some("CHANGES_REQUESTED") => Some(PullRequestReviewDecision::ChangesRequested),
        Some("REVIEW_REQUIRED") => Some(PullRequestReviewDecision::ReviewRequired),
        _ => None,
    };
    // The user's submitted reviews, oldest first.
    let mut mine: Vec<(Option<i64>, String)> = data
        .get("reviews")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|review| is_me(str_at(review, &["author", "login"])))
        .filter_map(|review| {
            let state = review
                .get("state")
                .and_then(Value::as_str)?
                .to_ascii_uppercase();
            (state != "PENDING").then(|| {
                (
                    review
                        .get("submittedAt")
                        .and_then(Value::as_str)
                        .and_then(parse_time_ms),
                    state,
                )
            })
        })
        .collect();
    mine.sort_by_key(|(at, _)| *at);
    let mut requested_reviewers = Vec::new();
    let mut requested_me = false;
    for request in data
        .get("reviewRequests")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(login) = request.get("login").and_then(Value::as_str) {
            requested_me |= is_me(Some(login));
            requested_reviewers.push(login.to_owned());
        } else if let Some(team) = request
            .get("slug")
            .or_else(|| request.get("name"))
            .and_then(Value::as_str)
        {
            requested_reviewers.push(team.to_owned());
        }
    }
    let reviewed_by_me = !mine.is_empty();
    let my_review_state = if requested_me {
        if reviewed_by_me {
            PullRequestMyReviewState::ReRequested
        } else {
            PullRequestMyReviewState::Requested
        }
    } else {
        match mine.last().map(|(_, state)| state.as_str()) {
            Some("APPROVED") => PullRequestMyReviewState::Approved,
            Some("CHANGES_REQUESTED") => PullRequestMyReviewState::ChangesRequested,
            Some(_) => PullRequestMyReviewState::Commented,
            None => PullRequestMyReviewState::None,
        }
    };
    PrDetail {
        summary: PullRequestStatusSummary {
            pr_ref,
            title: data
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            author: str_at(data, &["author", "login"])
                .unwrap_or_default()
                .to_owned(),
            state,
            ci_status,
            review_decision,
            my_review_state,
            requested_reviewers,
            updated_at: data
                .get("updatedAt")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            head_ref_name: data
                .get("headRefName")
                .and_then(Value::as_str)
                .map(str::to_owned),
            base_ref_name: data
                .get("baseRefName")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        my_latest_review_at: mine.iter().filter_map(|(at, _)| *at).max(),
        reviewed_by_me,
        requested_me,
    }
}

/// The ref of a search hit (`repository.nameWithOwner` + number).
fn hit_ref(hit: &MyOpenPr) -> Option<PullRequestRef> {
    let repo = hit.repo.as_deref()?;
    let (owner, name) = repo.split_once('/')?;
    if hit.number == 0 || owner.is_empty() || name.is_empty() {
        return None;
    }
    let mut pr = PullRequestRef::new(owner, name, hit.number);
    if let Some(url) = hit.url.as_deref().filter(|u| !u.is_empty()) {
        pr.url = url.to_owned();
    }
    Some(pr)
}

fn slug_of(pr: &PullRequestRef) -> String {
    format!("{}/{}", pr.owner, pr.repo).to_ascii_lowercase()
}

/// A summary from search fields only.
fn summary_from_hit(pr: PullRequestRef, hit: &MyOpenPr) -> PullRequestStatusSummary {
    PullRequestStatusSummary {
        pr_ref: pr,
        title: hit.title.clone().unwrap_or_default(),
        author: hit.author.clone().unwrap_or_default(),
        state: if hit.is_draft {
            PullRequestState::Draft
        } else {
            PullRequestState::Open
        },
        updated_at: hit.updated_at.clone().unwrap_or_default(),
        ..Default::default()
    }
}

/// Operation ids are capped at `limits::MAX_OPERATION_ID_CHARS`, and a Space
/// id plus an `owner/repo#number` key exceeds it, so the inputs are hashed.
fn item_operation(space_id: &str, key: &str) -> String {
    format!(
        "pr-review-sync-{}",
        derive_id(&format!("{space_id}\0{key}"), "pr-review-sync-item")
    )
}

fn request_operation(space_id: &str, key: &str, round: u32) -> String {
    format!(
        "pr-review-sync-{}",
        derive_id(
            &format!("{space_id}\0{key}\0{round}"),
            "pr-review-sync-request"
        )
    )
}

/// The archive of a finished PR's item: one id per (round, finished state),
/// so the operation ledger also keeps the sync from archiving it twice.
fn archive_operation(
    space_id: &str,
    key: &str,
    round: u32,
    finished: PrReviewRequestState,
) -> String {
    format!(
        "pr-review-sync-{}",
        derive_id(
            &format!("{space_id}\0{key}\0{round}\0{finished:?}"),
            "pr-review-sync-archive"
        )
    )
}

/// The derived id of the sync's item for `key` in `space_id`.
pub fn synced_item_id(space_id: &str, key: &str) -> String {
    derive_id(&item_operation(space_id, key), roles::ITEM)
}

/// The derived id of the sync's Required Action for `key`'s `round`.
pub fn synced_request_id(space_id: &str, key: &str, round: u32) -> String {
    derive_id(&request_operation(space_id, key, round), roles::REQUEST)
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn ci_label(status: PullRequestCiStatus) -> &'static str {
    match status {
        PullRequestCiStatus::Passing => "passing",
        PullRequestCiStatus::Failing => "failing",
        PullRequestCiStatus::Pending => "pending",
        PullRequestCiStatus::None => "none",
    }
}

/// Whether an item linking `pr` is a PR review item (the UI's
/// `github::pr_review_item` rule): it carries a GitHub sync record, sits
/// directly under the destination, or is titled `<repo>#<number>` (the
/// panel's manual Create). A feature or ticket item that merely links its own
/// PR (`work_outline_pr_links`) is not, so the sync never adopts it.
fn is_review_item(
    snapshot: &WorkOutlineSnapshot,
    item: &zeron_proto::WorkItemView,
    pr: &PullRequestRef,
) -> bool {
    let synced = item
        .external_sync_state
        .as_ref()
        .is_some_and(|state| state.provider == GITHUB_PROVIDER);
    let under_destination = snapshot.pr_review_sync_destination().is_some()
        && item.parent_work_item_id.as_deref() == snapshot.pr_review_sync_destination();
    let titled = item
        .title
        .to_lowercase()
        .starts_with(&format!("{}#{}", pr.repo, pr.number).to_lowercase());
    synced || under_destination || titled
}

/// Items of the Space tracked for each PR key: the sync's derived item
/// first, else the earliest-created PR review item carrying the PR reference
/// ([`is_review_item`]).
fn tracked_items(snapshot: &WorkOutlineSnapshot, space_id: &str) -> BTreeMap<String, String> {
    let mut tracked: BTreeMap<String, (bool, i64, String)> = BTreeMap::new();
    for reference in &snapshot.external_references {
        if reference.provider != GITHUB_PROVIDER
            || reference.reference_type != GITHUB_PULL_REQUEST_REFERENCE
        {
            continue;
        }
        let Some(item) = snapshot.item(&reference.work_item_id) else {
            continue;
        };
        let Some(pr) = PullRequestRef::parse_external_id(&reference.external_id) else {
            continue;
        };
        let key = pr.key();
        let derived = synced_item_id(space_id, &key) == item.id;
        if !derived && !is_review_item(snapshot, item, &pr) {
            continue;
        }
        let rank = (!derived, item.created_at, item.id.clone());
        tracked
            .entry(key)
            .and_modify(|kept| {
                if rank < *kept {
                    *kept = rank.clone();
                }
            })
            .or_insert(rank);
    }
    tracked
        .into_iter()
        .map(|(key, (_, _, id))| (key, id))
        .collect()
}

impl PrReviewSyncService {
    pub fn new(workspace: WorkspaceHost, repos: Repos, config: PrReviewSyncConfig) -> Self {
        let work_outline = Arc::new(WorkOutlineService::new(workspace.clone()));
        Self {
            inner: Arc::new(Inner {
                workspace,
                repos,
                work_outline,
                config: RwLock::new(config),
                viewer: Mutex::new(None),
                statuses: Mutex::new(HashMap::new()),
                searches: Mutex::new(HashMap::new()),
                repo_sets: Mutex::new(HashMap::new()),
                transcripts: RwLock::new(None),
                passes: tokio::sync::Mutex::new(()),
            }),
        }
    }

    /// Swap the tools (tests) and drop every cache.
    pub fn reconfigure(&self, config: PrReviewSyncConfig) {
        *self.inner.config.write().unwrap_or_else(|e| e.into_inner()) = config;
        *lock(&self.inner.viewer) = None;
        lock(&self.inner.statuses).clear();
        lock(&self.inner.searches).clear();
        lock(&self.inner.repo_sets).clear();
    }

    fn config(&self) -> PrReviewSyncConfig {
        self.inner
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    // ── gh ─────────────────────────────────────────────────────────────────

    /// Run `gh <args>` and return stdout, mapping failures to codes.
    async fn gh(&self, args: &[&str]) -> Result<Vec<u8>, GithubError> {
        let missing = || {
            github(
                GithubErrorCode::GhMissing,
                "The GitHub CLI (gh) is not installed on this device; install it and run `gh auth login`",
            )
        };
        let mut command = match self.config().gh {
            Some(path) => std::process::Command::new(path),
            None => {
                let executable =
                    zeron_harness::resolve_login_shell_executable("gh").ok_or_else(missing)?;
                let mut command = std::process::Command::new(executable);
                zeron_harness::compose_login_shell_path_std(&mut command);
                command
            }
        };
        command
            .args(args)
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null());
        let mut command = tokio::process::Command::from(command);
        command.kill_on_drop(true);
        let output = match tokio::time::timeout(GH_TIMEOUT, command.output()).await {
            Err(_) => {
                return Err(github(
                    GithubErrorCode::Network,
                    format!("gh {} timed out", args.first().unwrap_or(&"")),
                ));
            }
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(missing());
            }
            Ok(Err(error)) => {
                return Err(github(
                    GithubErrorCode::Network,
                    format!("Could not run the GitHub CLI (gh): {error}"),
                ));
            }
            Ok(Ok(output)) => output,
        };
        if output.status.success() {
            return Ok(output.stdout);
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let code = classify_gh_failure(&stderr);
        let message = match code {
            GithubErrorCode::Unauthorized => {
                "The GitHub CLI is not signed in on this device; run `gh auth login`".to_owned()
            }
            _ if stderr.is_empty() => format!("gh {} failed", args.join(" ")),
            _ => truncate(&stderr, 500),
        };
        Err(github(code, message))
    }

    async fn gh_json(&self, args: &[&str]) -> Result<Value, GithubError> {
        let stdout = self.gh(args).await?;
        serde_json::from_slice(&stdout).map_err(|e| {
            github(
                GithubErrorCode::Network,
                format!("gh returned unexpected output: {e}"),
            )
        })
    }

    /// The signed-in login (`PrViewer`), cached per engine run.
    pub async fn viewer_login(&self) -> Result<String, GithubError> {
        self.viewer().await
    }

    /// The signed-in login (once per engine run).
    async fn viewer(&self) -> Result<String, GithubError> {
        if let Some(login) = lock(&self.inner.viewer).clone() {
            return Ok(login);
        }
        let user = self.gh_json(&["api", "user"]).await?;
        let login = user
            .get("login")
            .and_then(Value::as_str)
            .filter(|l| !l.is_empty())
            .ok_or_else(|| github(GithubErrorCode::Network, "gh api user returned no login"))?
            .to_owned();
        *lock(&self.inner.viewer) = Some(login.clone());
        Ok(login)
    }

    /// One `gh search prs <qualifier> --state=open` (cached).
    async fn search(
        &self,
        qualifier: &'static str,
        force: bool,
    ) -> Result<Vec<MyOpenPr>, GithubError> {
        let ttl = self.config().search_ttl;
        if !force
            && let Some((at, hits)) = lock(&self.inner.searches).get(qualifier)
            && at.elapsed() < ttl
        {
            return Ok(hits.clone());
        }
        let stdout = self
            .gh(&[
                "search",
                "prs",
                qualifier,
                "--state=open",
                "--json",
                SEARCH_FIELDS,
                "--limit",
                SEARCH_LIMIT,
            ])
            .await?;
        let hits = crate::pr_ticket_cache::parse_pr_search(&stdout).ok_or_else(|| {
            github(
                GithubErrorCode::Network,
                "gh search prs returned unexpected output",
            )
        })?;
        lock(&self.inner.searches).insert(qualifier, (Instant::now(), hits.clone()));
        Ok(hits)
    }

    /// One PR's detail (`gh pr view`), cached for `status_ttl`.
    async fn detail(&self, pr: &PullRequestRef, force: bool) -> Result<PrDetail, GithubError> {
        let key = pr.key();
        let ttl = self.config().status_ttl;
        if !force
            && let Some((at, detail)) = lock(&self.inner.statuses).get(&key)
            && at.elapsed() < ttl
        {
            return Ok(detail.clone());
        }
        let viewer = self.viewer().await?;
        let repo = format!("{}/{}", pr.owner, pr.repo);
        let number = pr.number.to_string();
        let data = self
            .gh_json(&[
                "pr",
                "view",
                &number,
                "--repo",
                &repo,
                "--json",
                PR_VIEW_FIELDS,
            ])
            .await?;
        let detail = parse_detail(&data, pr, &viewer);
        lock(&self.inner.statuses).insert(key, (Instant::now(), detail.clone()));
        Ok(detail)
    }

    /// A fresh cached detail, if any (never calls `gh`).
    fn cached_detail(&self, pr: &PullRequestRef) -> Option<PrDetail> {
        let ttl = self.config().status_ttl;
        lock(&self.inner.statuses)
            .get(&pr.key())
            .filter(|(at, _)| at.elapsed() < ttl)
            .map(|(_, detail)| detail.clone())
    }

    // ── RPCs ───────────────────────────────────────────────────────────────

    /// `PrStatus`.
    pub async fn status(
        &self,
        params: &PrStatusParams,
    ) -> Result<PullRequestStatusSummary, GithubError> {
        let valid = |part: &str| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        if !valid(params.owner.trim()) || !valid(params.repo.trim()) || params.number == 0 {
            return Err(invalid("Not a pull request (owner, repo, number)"));
        }
        let pr = PullRequestRef::new(params.owner.trim(), params.repo.trim(), params.number);
        Ok(self.detail(&pr, params.force_refresh).await?.summary)
    }

    /// `PrMyOpen`.
    pub async fn my_open(
        &self,
        space_id: &str,
    ) -> Result<Vec<PullRequestStatusSummary>, GithubError> {
        self.space_list(space_id, AUTHORED).await
    }

    /// `PrReviewRequested`.
    pub async fn review_requested(
        &self,
        space_id: &str,
    ) -> Result<Vec<PullRequestStatusSummary>, GithubError> {
        self.space_list(space_id, REVIEW_REQUESTED).await
    }

    async fn space_list(
        &self,
        space_id: &str,
        qualifier: &'static str,
    ) -> Result<Vec<PullRequestStatusSummary>, GithubError> {
        let space = self.local_space(space_id)?;
        let slugs = self.repo_set(&space, false).await?;
        let hits = self.search(qualifier, false).await?;
        let mut out: Vec<PullRequestStatusSummary> = hits
            .iter()
            .filter_map(|hit| {
                let pr = hit_ref(hit)?;
                slugs.contains(&slug_of(&pr)).then_some((pr, hit))
            })
            .map(|(pr, hit)| {
                let mut summary = match self.cached_detail(&pr) {
                    Some(detail) => detail.summary,
                    None => summary_from_hit(pr, hit),
                };
                if qualifier == REVIEW_REQUESTED && !summary.my_review_state.is_requested() {
                    let reviewed = self
                        .cached_detail(&summary.pr_ref)
                        .is_some_and(|detail| detail.reviewed_by_me);
                    summary.my_review_state = if reviewed {
                        PullRequestMyReviewState::ReRequested
                    } else {
                        PullRequestMyReviewState::Requested
                    };
                }
                summary
            })
            .collect();
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    // ── repository set ─────────────────────────────────────────────────────

    /// The Space, which must be live and owned by this device.
    fn local_space(&self, space_id: &str) -> Result<Space, GithubError> {
        let space = self
            .inner
            .workspace
            .space(space_id)
            .ok()
            .flatten()
            .ok_or_else(|| invalid("Space not found"))?;
        if space.device_id != self.inner.workspace.device_id() {
            return Err(invalid(
                "This Space lives on another device; its PRs sync there",
            ));
        }
        Ok(space)
    }

    /// Lowercased `owner/repo` of every GitHub repository in the Space: its
    /// checkout, submodules, and nested repositories, by `origin` remote.
    async fn repo_set(&self, space: &Space, force: bool) -> Result<BTreeSet<String>, GithubError> {
        if !force
            && let Some((at, slugs)) = lock(&self.inner.repo_sets).get(&space.id)
            && at.elapsed() < REPO_SET_TTL
        {
            return Ok(slugs.clone());
        }
        let root = PathBuf::from(&space.path);
        if crate::repos::is_broad_workspace_root_resolved(&root) {
            return Err(invalid("PR review sync needs a specific project folder"));
        }
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        let paths = crate::repository_discovery::repository_paths(&self.inner.repos, &root)
            .await
            .map_err(|e| invalid(format!("Could not read the Space's repositories: {e}")))?;
        let mut slugs = BTreeSet::new();
        for path in paths {
            if let Some(slug) = self.origin_slug(&path).await {
                slugs.insert(slug);
            }
        }
        lock(&self.inner.repo_sets).insert(space.id.clone(), (Instant::now(), slugs.clone()));
        Ok(slugs)
    }

    async fn origin_slug(&self, path: &Path) -> Option<String> {
        if !path.is_dir() {
            return None;
        }
        let url = self
            .inner
            .repos
            .git(&["config", "--get", "remote.origin.url"], Some(path))
            .await
            .ok()?;
        crate::pr_review_launch::github_slug(&url)
    }

    // ── work item PR links (`work_outline_pr_links`) ───────────────────────

    /// Where the work item PR link backfill reads stored Chat transcripts.
    pub fn set_transcript_reader(&self, reader: crate::work_outline_pr_links::TranscriptReader) {
        *self
            .inner
            .transcripts
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(reader);
    }

    pub(crate) fn transcript_reader(
        &self,
    ) -> Option<crate::work_outline_pr_links::TranscriptReader> {
        self.inner
            .transcripts
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn workspace(&self) -> &WorkspaceHost {
        &self.inner.workspace
    }

    pub(crate) async fn outline_snapshot(
        &self,
        space_id: &str,
    ) -> Result<WorkOutlineSnapshot, GithubError> {
        self.snapshot(space_id).await
    }

    /// The Space's repository set (`owner/repo`, lowercased); the Space
    /// must live on this device.
    pub(crate) async fn space_repo_slugs(
        &self,
        space_id: &str,
    ) -> Result<BTreeSet<String>, GithubError> {
        let space = self.local_space(space_id)?;
        self.repo_set(&space, false).await
    }

    pub(crate) async fn system_mutate(
        &self,
        mutation: WorkOutlineMutation,
    ) -> Result<WorkOutlineMutateReply, WorkOutlineError> {
        self.mutate(mutation).await
    }

    /// Link the PRs the Space's Chats created to their work items
    /// (`work_outline_pr_links::link_chat_pull_requests`). No `gh` call.
    pub async fn link_chat_pull_requests(
        &self,
        space_id: &str,
    ) -> Result<crate::work_outline_pr_links::ChatLinkPass, GithubError> {
        let _pass = self.inner.passes.lock().await;
        crate::work_outline_pr_links::link_chat_pull_requests(self, space_id).await
    }

    /// [`Self::link_chat_pull_requests`] for every Space this device owns.
    pub async fn link_all_chat_pull_requests(&self) {
        let device = self.inner.workspace.device_id().to_owned();
        let Ok(spaces) = self.inner.workspace.read_spaces() else {
            return;
        };
        for space in spaces {
            if space.device_id != device
                || crate::repos::is_automatic_access_blocked(Path::new(&space.path))
            {
                continue;
            }
            if let Err(error) = self.link_chat_pull_requests(&space.id).await {
                tracing::debug!(
                    code = error.code.as_str(),
                    space = %space.id,
                    "work item PR link: Space skipped"
                );
            }
        }
    }

    // ── writes ─────────────────────────────────────────────────────────────

    async fn snapshot(&self, space_id: &str) -> Result<WorkOutlineSnapshot, GithubError> {
        let service = self.inner.work_outline.clone();
        let space = space_id.to_owned();
        tokio::task::spawn_blocking(move || service.snapshot(&space))
            .await
            .map_err(|e| github(GithubErrorCode::Network, e.to_string()))
    }

    async fn mutate(
        &self,
        mutation: WorkOutlineMutation,
    ) -> Result<WorkOutlineMutateReply, WorkOutlineError> {
        let service = self.inner.work_outline.clone();
        tokio::task::spawn_blocking(move || {
            let actor = service.system_actor();
            service.mutate_as(&mutation, &actor)
        })
        .await
        .unwrap_or_else(|e| {
            Err(WorkOutlineError::new(
                WorkOutlineErrorCode::Invalid,
                e.to_string(),
            ))
        })
    }

    fn fresh_operation() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// Record `state` (and `status`, when given) on an item. An item with
    /// other open requests cannot complete: the record is written alone.
    async fn write_record(
        &self,
        work_item_id: &str,
        status: Option<WorkItemStatus>,
        mut state: WorkItemExternalSyncState,
        previous_status: WorkItemStatus,
    ) -> Result<(), WorkOutlineError> {
        if let Some(status) = status {
            state.synced_status = status;
        }
        let update = |status: Option<WorkItemStatus>, state: WorkItemExternalSyncState| {
            WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
                work_item_id: work_item_id.to_owned(),
                changes: WorkItemChanges {
                    status,
                    external_sync_state: Some(state),
                    ..Default::default()
                },
                operation_id: Self::fresh_operation(),
            })
        };
        match self.mutate(update(status, state.clone())).await {
            Err(error) if error.code == WorkOutlineErrorCode::OpenUserActionRequests => {
                state.synced_status = previous_status;
                self.mutate(update(None, state)).await.map(drop)
            }
            other => other.map(drop),
        }
    }

    async fn link(&self, work_item_id: &str, pr: &PullRequestRef, title: &str) {
        let cached: String = title
            .trim()
            .chars()
            .take(limits::MAX_REFERENCE_TITLE_CHARS)
            .collect();
        let result = self
            .mutate(WorkOutlineMutation::CreateWorkItemExternalReference(
                CreateWorkItemExternalReferenceParams {
                    work_item_id: work_item_id.to_owned(),
                    provider: GITHUB_PROVIDER.into(),
                    reference_type: GITHUB_PULL_REQUEST_REFERENCE.into(),
                    external_id: pr.external_id(),
                    url: pr.url.clone(),
                    cached_title: (!cached.is_empty()).then_some(cached),
                    operation_id: Self::fresh_operation(),
                },
            ))
            .await;
        if let Err(error) = result {
            tracing::warn!(code = error.code.as_str(), "pr review sync: link failed");
        }
    }

    /// Archive a finished (merged or closed) PR's item as the system actor
    /// (descendants archive with it; chats are never touched), then record
    /// `archivedBySync` so the same round and state never archive it again.
    /// `false` when the archive failed (the next pass retries).
    async fn archive_finished(
        &self,
        space_id: &str,
        key: &str,
        work_item_id: &str,
        state: &WorkItemExternalSyncState,
    ) -> bool {
        let archived = self
            .mutate(WorkOutlineMutation::ArchiveWorkItem(
                ArchiveWorkItemParams {
                    work_item_id: work_item_id.to_owned(),
                    descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
                    operation_id: archive_operation(
                        space_id,
                        key,
                        state.round,
                        state.request_state,
                    ),
                },
            ))
            .await;
        if let Err(error) = archived {
            tracing::warn!(code = error.code.as_str(), "pr review sync: archive failed");
            return false;
        }
        let marked = WorkItemExternalSyncState {
            archived_by_sync: Some(state.request_state),
            ..state.clone()
        };
        if let Err(error) = self
            .write_record(work_item_id, None, marked, state.synced_status)
            .await
        {
            tracing::warn!(
                code = error.code.as_str(),
                "pr review sync: archive record failed"
            );
        }
        true
    }

    /// The round's Required Action (deterministic id; a request that exists
    /// on any device, open or resolved, is left alone).
    async fn raise(
        &self,
        space_id: &str,
        work_item_id: &str,
        summary: &PullRequestStatusSummary,
        round: u32,
        re_request: bool,
    ) {
        let pr = &summary.pr_ref;
        let head = format!("{}#{}", pr.repo, pr.number);
        let verb = if re_request {
            "Re-review requested"
        } else {
            "Review requested"
        };
        let title = summary
            .title
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let text = truncate(
            format!("{verb}: {head} {title}").trim(),
            user_action_limits::MAX_REQUEST_TEXT_CHARS,
        );
        let mut info = Vec::new();
        if !summary.author.is_empty() {
            info.push(format!("Author: @{}", summary.author));
        }
        if !summary.updated_at.is_empty() {
            info.push(format!("Requested: {}", summary.updated_at));
        }
        info.push(format!("CI: {}", ci_label(summary.ci_status)));
        info.push(pr.url.clone());
        let mutation =
            WorkOutlineMutation::CreateUserActionRequest(CreateUserActionRequestParams {
                work_item_id: work_item_id.to_owned(),
                origin: UserActionRequestOrigin {
                    kind: UserActionOriginKind::System,
                    chat_id: String::new(),
                    message_id: None,
                    tool_call_id: None,
                },
                text,
                info: Some(truncate(
                    &info.join("\n"),
                    user_action_limits::MAX_REQUEST_INFO_CHARS,
                )),
                dedupe_key: Some(pr_review_dedupe_key(&pr.owner, &pr.repo, pr.number, round)),
                operation_id: request_operation(space_id, &pr.key(), round),
            });
        match self.mutate(mutation).await {
            Ok(_) => {}
            // Already raised (here or on another device) with other details.
            Err(error) if error.code == WorkOutlineErrorCode::OperationConflict => {}
            Err(error) => {
                tracing::warn!(code = error.code.as_str(), "pr review sync: request failed")
            }
        }
    }

    // ── the sync ───────────────────────────────────────────────────────────

    /// Sync one Space (`force` bypasses every cache). `github/invalid` when
    /// the Space has no usable destination or lives on another device; a
    /// `ghMissing` / `unauthorized` / `rateLimited` failure stops the pass
    /// before any write.
    pub async fn sync_space(
        &self,
        space_id: &str,
        force: bool,
    ) -> Result<PrReviewSyncNowReply, GithubError> {
        let _pass = self.inner.passes.lock().await;
        let space = self.local_space(space_id)?;
        let snapshot = self.snapshot(space_id).await?;
        let destination = snapshot
            .pr_review_sync_destination()
            .map(str::to_owned)
            .ok_or_else(|| invalid("No PR review sync destination is set for this Space"))?;
        if !snapshot
            .item(&destination)
            .is_some_and(|item| item.archived_at.is_none())
        {
            return Err(invalid(
                "The PR review sync destination is missing or archived",
            ));
        }
        let mut reply = PrReviewSyncNowReply::default();
        let slugs = self.repo_set(&space, force).await?;
        if slugs.is_empty() {
            return Ok(reply);
        }
        // Fail fast (before any write) when gh is missing or signed out.
        if force {
            *lock(&self.inner.viewer) = None;
        }
        self.viewer().await?;
        let requested: BTreeMap<String, (PullRequestRef, MyOpenPr)> = self
            .search(REVIEW_REQUESTED, force)
            .await?
            .into_iter()
            .filter_map(|hit| {
                let pr = hit_ref(&hit)?;
                slugs.contains(&slug_of(&pr)).then(|| (pr.key(), (pr, hit)))
            })
            .collect();
        let tracked = tracked_items(&snapshot, space_id);
        let now = crate::now_ms();

        for (key, (pr, hit)) in &requested {
            let derived = synced_item_id(space_id, key);
            let existing = tracked
                .get(key)
                .cloned()
                .or_else(|| snapshot.item(&derived).map(|_| derived.clone()));
            let Some(item_id) = existing else {
                if self
                    .create_synced_item(space_id, &destination, pr, hit, now)
                    .await?
                {
                    reply.created += 1;
                }
                continue;
            };
            let Some(item) = snapshot.item(&item_id) else {
                continue;
            };
            // The user archived it: leave it alone. One the sync archived
            // (merged or closed) comes back only when GitHub reopened the PR
            // and requests the review again: a new round.
            if item.archived_at.is_some() {
                let by_sync = item
                    .external_sync_state
                    .as_ref()
                    .is_some_and(|s| s.archived_by_sync.is_some())
                    && item.archive_cause == Some(WorkItemArchiveCause::SelfArchived)
                    && item
                        .archived_by
                        .as_ref()
                        .is_some_and(|actor| actor.kind == WorkOutlineActorKind::System);
                if !by_sync {
                    continue;
                }
                match self.detail(pr, true).await {
                    Ok(detail) if !detail.summary.state.is_finished() => {}
                    Err(error) if is_fatal(&error) => return Err(error),
                    _ => continue,
                }
                let restored = self
                    .mutate(WorkOutlineMutation::RestoreWorkItem(
                        RestoreWorkItemParams {
                            work_item_id: item_id.clone(),
                            operation_id: Self::fresh_operation(),
                        },
                    ))
                    .await;
                if let Err(error) = restored {
                    tracing::warn!(code = error.code.as_str(), "pr review sync: restore failed");
                    continue;
                }
            }
            if snapshot.github_pull_request_reference(&item_id).is_none() {
                self.link(&item_id, pr, hit.title.as_deref().unwrap_or_default())
                    .await;
            }
            match &item.external_sync_state {
                Some(state) if state.request_state == PrReviewRequestState::Requested => {
                    // Steady state. Repair a missing request for this round
                    // (never one the user resolved: those stay listed).
                    let request = synced_request_id(space_id, key, state.round);
                    if snapshot.user_action_request(&request).is_none() {
                        let mut summary = summary_from_hit(pr.clone(), hit);
                        if let Some(detail) = self.cached_detail(pr) {
                            summary.ci_status = detail.summary.ci_status;
                        }
                        self.raise(space_id, &item_id, &summary, state.round, state.round > 1)
                            .await;
                    }
                }
                previous => {
                    // A new request: a new round.
                    let round = previous
                        .as_ref()
                        .map_or(1, |s| s.round.saturating_add(1).max(1));
                    let detail = match self.detail(pr, true).await {
                        Ok(detail) => Some(detail),
                        Err(error) if is_fatal(&error) => return Err(error),
                        Err(_) => None,
                    };
                    let re_request = detail.as_ref().map_or(
                        previous
                            .as_ref()
                            .is_some_and(|s| s.request_state == PrReviewRequestState::Submitted),
                        |d| d.reviewed_by_me,
                    );
                    let summary = detail
                        .map(|d| d.summary)
                        .unwrap_or_else(|| summary_from_hit(pr.clone(), hit));
                    let state = WorkItemExternalSyncState {
                        provider: GITHUB_PROVIDER.into(),
                        external_id: pr.external_id(),
                        round,
                        request_state: PrReviewRequestState::Requested,
                        synced_status: WorkItemStatus::Active,
                        round_started_at: now,
                        synced_at: now,
                        archived_by_sync: None,
                    };
                    if let Err(error) = self
                        .write_record(&item_id, Some(WorkItemStatus::Active), state, item.status)
                        .await
                    {
                        tracing::warn!(code = error.code.as_str(), "pr review sync: round failed");
                        continue;
                    }
                    self.raise(space_id, &item_id, &summary, round, re_request)
                        .await;
                    reply.updated += 1;
                }
            }
        }

        // Tracked PRs whose request went away: merged, closed, reviewed, or
        // withdrawn. A merged or closed PR completes its item and then
        // archives it, once per round and state (`archivedBySync`): a user
        // who restores the item keeps it, and only a new round (the PR
        // reopened and requested again) can archive it again.
        for (key, item_id) in &tracked {
            if requested.contains_key(key) {
                continue;
            }
            let Some(item) = snapshot.item(item_id) else {
                continue;
            };
            if item.archived_at.is_some() {
                continue;
            }
            let Some(state) = item.external_sync_state.clone() else {
                continue;
            };
            if state.archived_by_sync.is_some() {
                continue;
            }
            let Some(pr) = PullRequestRef::parse_external_id(&state.external_id)
                .or_else(|| PullRequestRef::parse_external_id(key))
            else {
                continue;
            };
            // A repository no longer in the Space: leave its items.
            if !slugs.contains(&slug_of(&pr)) {
                continue;
            }
            // `completed`: whether this pass writes the new state (else the
            // item already finished and only its archive is missing).
            let (request_state, note, completed) = match state.request_state {
                // Merged or closed by an earlier pass, never archived (an
                // archive that failed, or a record from before archiving).
                PrReviewRequestState::Merged => {
                    (PrReviewRequestState::Merged, PR_RESOLUTION_MERGED, false)
                }
                PrReviewRequestState::Closed => {
                    (PrReviewRequestState::Closed, PR_RESOLUTION_CLOSED, false)
                }
                previous => {
                    // Reviewed or withdrawn items only watch for the PR to
                    // finish: the status cache is good enough.
                    let fresh = force || previous == PrReviewRequestState::Requested;
                    let detail = match self.detail(&pr, fresh).await {
                        Ok(detail) => detail,
                        Err(error) if is_fatal(&error) => return Err(error),
                        Err(error) => {
                            tracing::debug!(
                                code = error.code.as_str(),
                                "pr review sync: PR skipped"
                            );
                            continue;
                        }
                    };
                    let finished = match detail.summary.state {
                        PullRequestState::Merged => {
                            Some((PrReviewRequestState::Merged, PR_RESOLUTION_MERGED))
                        }
                        PullRequestState::Closed => {
                            Some((PrReviewRequestState::Closed, PR_RESOLUTION_CLOSED))
                        }
                        _ => None,
                    };
                    if let Some((request_state, note)) = finished {
                        (request_state, note, true)
                    } else if previous != PrReviewRequestState::Requested {
                        // Reviewed or withdrawn and still open: settled
                        // until it finishes or is requested again.
                        continue;
                    } else if detail.requested_me {
                        // Search lags GitHub: the request is still there.
                        continue;
                    } else if detail.my_latest_review_at.is_some_and(|at| {
                        state.round <= 1 || at + ROUND_SKEW_MS >= state.round_started_at
                    }) {
                        (
                            PrReviewRequestState::Submitted,
                            PR_RESOLUTION_REVIEW_SUBMITTED,
                            true,
                        )
                    } else {
                        (
                            PrReviewRequestState::Withdrawn,
                            PR_RESOLUTION_WITHDRAWN,
                            true,
                        )
                    }
                }
            };
            let next = WorkItemExternalSyncState {
                request_state,
                synced_at: if completed { now } else { state.synced_at },
                ..state
            };
            let mut changed = false;
            if completed {
                let request = synced_request_id(space_id, key, next.round);
                if snapshot
                    .user_action_request(&request)
                    .is_some_and(|r| r.is_open())
                {
                    let resolved = self
                        .mutate(WorkOutlineMutation::ResolveUserActionRequest(
                            ResolveUserActionRequestParams {
                                user_action_request_id: request,
                                note: Some(note.into()),
                                operation_id: Self::fresh_operation(),
                            },
                        ))
                        .await;
                    match resolved {
                        Ok(_) => reply.resolved += 1,
                        Err(error) => {
                            tracing::warn!(
                                code = error.code.as_str(),
                                "pr review sync: resolve failed"
                            )
                        }
                    }
                }
                match self
                    .write_record(
                        item_id,
                        Some(WorkItemStatus::Completed),
                        next.clone(),
                        item.status,
                    )
                    .await
                {
                    Ok(()) => changed = true,
                    Err(error) => {
                        tracing::warn!(code = error.code.as_str(), "pr review sync: update failed");
                        continue;
                    }
                }
            }
            if matches!(
                request_state,
                PrReviewRequestState::Merged | PrReviewRequestState::Closed
            ) && self.archive_finished(space_id, key, item_id, &next).await
            {
                changed = true;
            }
            if changed {
                reply.updated += 1;
            }
        }
        Ok(reply)
    }

    /// A new item under the destination: item, reference, record, request.
    async fn create_synced_item(
        &self,
        space_id: &str,
        destination: &str,
        pr: &PullRequestRef,
        hit: &MyOpenPr,
        now: i64,
    ) -> Result<bool, GithubError> {
        let detail = match self.detail(pr, true).await {
            Ok(detail) => Some(detail),
            Err(error) if is_fatal(&error) => return Err(error),
            Err(_) => None,
        };
        let re_request = detail.as_ref().is_some_and(|d| d.reviewed_by_me);
        let mut summary = detail
            .map(|d| d.summary)
            .unwrap_or_else(|| summary_from_hit(pr.clone(), hit));
        if summary.title.is_empty() {
            summary.title = hit.title.clone().unwrap_or_default();
        }
        let key = pr.key();
        let created = self
            .mutate(WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
                space_id: space_id.to_owned(),
                parent_work_item_id: Some(destination.to_owned()),
                placement: WorkOutlinePlacement::Last,
                title: pr_item_title(&pr.repo, pr.number, &summary.title),
                description: None,
                status: Some(WorkItemStatus::Active),
                operation_id: item_operation(space_id, &key),
            }))
            .await;
        let item_id = match created {
            Ok(reply) => reply
                .work_item_id
                .unwrap_or_else(|| synced_item_id(space_id, &key)),
            // Created elsewhere with other inputs, or deleted: leave it.
            Err(error) => {
                tracing::warn!(
                    code = error.code.as_str(),
                    message = %error.message,
                    pr = %key,
                    "pr review sync: create failed"
                );
                return Ok(false);
            }
        };
        self.link(&item_id, pr, &summary.title).await;
        let state = WorkItemExternalSyncState {
            provider: GITHUB_PROVIDER.into(),
            external_id: pr.external_id(),
            round: 1,
            request_state: PrReviewRequestState::Requested,
            synced_status: WorkItemStatus::Active,
            round_started_at: now,
            synced_at: now,
            archived_by_sync: None,
        };
        if let Err(error) = self
            .write_record(&item_id, None, state, WorkItemStatus::Active)
            .await
        {
            tracing::warn!(code = error.code.as_str(), "pr review sync: record failed");
        }
        self.raise(space_id, &item_id, &summary, 1, re_request)
            .await;
        Ok(true)
    }

    /// One background pass over every Space this device owns that has a
    /// destination (no `gh` call when there is none).
    pub async fn sync_all(&self) {
        let device = self.inner.workspace.device_id().to_owned();
        for space_id in self.inner.workspace.pr_review_sync_space_ids() {
            let Ok(Some(space)) = self.inner.workspace.space(&space_id) else {
                continue;
            };
            if space.device_id != device
                || crate::repos::is_automatic_access_blocked(Path::new(&space.path))
            {
                continue;
            }
            match self.sync_space(&space_id, false).await {
                Ok(reply) if reply == PrReviewSyncNowReply::default() => {}
                Ok(reply) => tracing::info!(
                    space = %space_id,
                    created = reply.created,
                    updated = reply.updated,
                    resolved = reply.resolved,
                    "pr review sync: pass applied"
                ),
                Err(error) if is_fatal(&error) => {
                    tracing::info!(code = error.code.as_str(), "pr review sync: pass stopped");
                    return;
                }
                Err(error) => {
                    tracing::debug!(code = error.code.as_str(), "pr review sync: Space skipped")
                }
            }
        }
    }

    /// Sync one Space in the background (after the destination is set).
    /// Automatic like the loop: off when the config turns background off.
    pub fn spawn_space_sync(&self, space_id: String) {
        if !self.config().background {
            return;
        }
        let service = self.clone();
        tokio::spawn(async move {
            if let Err(error) = service.sync_space(&space_id, false).await {
                tracing::info!(
                    code = error.code.as_str(),
                    "pr review sync: destination sync stopped"
                );
            }
        });
    }

    /// The background loop (held by `EngineCore` for the process lifetime).
    pub fn spawn_sync_loop(&self) -> tokio::task::JoinHandle<()> {
        let service = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(FIRST_SYNC_DELAY).await;
            loop {
                if service.config().background {
                    service.sync_all().await;
                    service.link_all_chat_pull_requests().await;
                }
                tokio::time::sleep(SYNC_INTERVAL).await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn operation_ids_fit_the_limit_for_real_space_ids_and_long_keys() {
        let space = "f9117668-f2f4-4dd2-b16c-ef80bc8e46f3";
        let key = "Cofactr/receive-ai-workflows-and-tooling#123456";
        let item = item_operation(space, key);
        let request = request_operation(space, key, 42);
        assert!(
            item.chars().count() <= limits::MAX_OPERATION_ID_CHARS,
            "{item}"
        );
        assert!(
            request.chars().count() <= limits::MAX_OPERATION_ID_CHARS,
            "{request}"
        );
        let archive = archive_operation(space, key, 42, PrReviewRequestState::Closed);
        assert!(
            archive.chars().count() <= limits::MAX_OPERATION_ID_CHARS,
            "{archive}"
        );
        assert_ne!(item, item_operation(space, "Cofactr/ui#1"));
        assert_ne!(request, request_operation(space, key, 43));
        assert_ne!(archive, item);
        assert_ne!(archive, request_operation(space, key, 42));
        assert_ne!(
            archive,
            archive_operation(space, key, 42, PrReviewRequestState::Merged)
        );
        assert_ne!(
            archive,
            archive_operation(space, key, 43, PrReviewRequestState::Closed)
        );
    }
    use zeron_proto::pr_review_sync::pr_key;

    #[test]
    fn gh_failures_map_to_codes() {
        for (stderr, code) in [
            (
                "To get started with GitHub CLI, please run:  gh auth login",
                GithubErrorCode::Unauthorized,
            ),
            ("HTTP 401: Bad credentials", GithubErrorCode::Unauthorized),
            (
                "API rate limit exceeded for user ID 1",
                GithubErrorCode::RateLimited,
            ),
            (
                "GraphQL: Could not resolve to a PullRequest with the number of 9.",
                GithubErrorCode::NotFound,
            ),
            ("dial tcp: i/o timeout", GithubErrorCode::Network),
        ] {
            assert_eq!(classify_gh_failure(stderr), code, "{stderr}");
        }
    }

    fn pr() -> PullRequestRef {
        PullRequestRef::new("acme", "ui", 9)
    }

    #[test]
    fn detail_parses_state_ci_and_my_review() {
        let data = json!({
            "number": 9, "title": "Fix", "url": "https://github.com/acme/ui/pull/9",
            "state": "OPEN", "isDraft": false, "author": {"login": "ada"},
            "reviewDecision": "CHANGES_REQUESTED",
            "reviews": [
                {"author": {"login": "Me"}, "state": "COMMENTED", "submittedAt": "2026-10-01T00:00:00Z"},
                {"author": {"login": "bob"}, "state": "APPROVED", "submittedAt": "2026-10-02T00:00:00Z"},
                {"author": {"login": "me"}, "state": "PENDING"}
            ],
            "reviewRequests": [{"__typename": "User", "login": "me"}, {"__typename": "Team", "slug": "eng"}],
            "statusCheckRollup": [{"status": "COMPLETED", "conclusion": "FAILURE"}],
            "headRefName": "ada/fix", "baseRefName": "main", "updatedAt": "2026-10-03T00:00:00Z"
        });
        let detail = parse_detail(&data, &pr(), "me");
        assert!(detail.requested_me);
        assert!(detail.reviewed_by_me);
        let s = &detail.summary;
        assert_eq!(s.state, PullRequestState::Open);
        assert_eq!(s.ci_status, PullRequestCiStatus::Failing);
        assert_eq!(
            s.review_decision,
            Some(PullRequestReviewDecision::ChangesRequested)
        );
        assert_eq!(s.my_review_state, PullRequestMyReviewState::ReRequested);
        assert_eq!(
            s.requested_reviewers,
            vec!["me".to_owned(), "eng".to_owned()]
        );
        assert_eq!(s.author, "ada");
        assert_eq!(s.head_ref_name.as_deref(), Some("ada/fix"));

        let merged = parse_detail(
            &json!({"state": "MERGED", "isDraft": true, "reviews": [
                {"author": {"login": "me"}, "state": "APPROVED", "submittedAt": "2026-10-01T00:00:00Z"}
            ]}),
            &pr(),
            "me",
        );
        assert_eq!(merged.summary.state, PullRequestState::Merged);
        assert_eq!(
            merged.summary.my_review_state,
            PullRequestMyReviewState::Approved
        );
        assert_eq!(merged.summary.ci_status, PullRequestCiStatus::None);
        let draft = parse_detail(&json!({"state": "OPEN", "isDraft": true}), &pr(), "me");
        assert_eq!(draft.summary.state, PullRequestState::Draft);
        assert_eq!(
            draft.summary.my_review_state,
            PullRequestMyReviewState::None
        );
        assert_eq!(
            draft.summary.pr_ref.url,
            "https://github.com/acme/ui/pull/9"
        );
    }

    #[test]
    fn derived_ids_are_stable_per_space_and_pr() {
        let a = synced_item_id("s", &pr_key("Acme", "UI", 9));
        assert_eq!(a, synced_item_id("s", "acme/ui#9"));
        assert_ne!(a, synced_item_id("t", "acme/ui#9"));
        assert_ne!(
            synced_request_id("s", "acme/ui#9", 1),
            synced_request_id("s", "acme/ui#9", 2)
        );
    }
}
