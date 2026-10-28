//! "Review PR" launcher (`StartPrReview`; docs/pr-review.md): a port of the
//! Cofactr workspace's `tools/cursor-claude-launcher/launch.sh` onto Mast
//! Chats.
//!
//! Given `github.com/<owner>/<name>#<number>`:
//!
//! 1. find the Space whose folder holds a checkout of that repository (the
//!    Space root itself or one of its submodules), matched by the raw
//!    `remote.origin.url`, never by folder name; the outermost Space wins,
//!    so the Cofactr workspace superproject beats a Space opened on one of
//!    its submodules;
//! 2. `gh pr view` → head branch, `isCrossRepository` (forks refused) and the
//!    author (a review of the viewer's own PR is seeded as a self-review);
//! 3. `git fetch origin +refs/heads/<b>:refs/remotes/origin/<b>`;
//! 4. choose the checkout exactly like launch.sh: the branch's existing
//!    checkout (canonical or any worktree) → the canonical checkout when it
//!    is on `main` with no tracked changes (`gh pr checkout <n>` there) → a
//!    new worktree `<root>/<name>-pr-<n>` (root: first line of
//!    `<space>/.devcontainer/.current-worktrees-root`, else
//!    `$WORKSPACE_WORKTREES_DIR`, else `<space>/.worktrees`);
//! 5. count commits behind `origin/<b>` and describe the location with
//!    launch.sh's wording;
//! 6. create the Chat at the Space root (launch.sh `cd "$ws"`), titled,
//!    linked to the PR, optionally assigned to a work item, and queue the
//!    seeded `/code-review` (or `/fix-ci`) Run last.
//!
//! Idempotent by `operationId`: ids derive from it, the inputs are claimed
//! in the processed ledger, and the finished reply is kept in a device-local
//! sidecar, so a retry returns the same Chat without touching git again.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use mast_doc::{RowState, SessionCommandPayload};
use mast_proto::work_outline::derive_id;
use mast_proto::{
    AssignChatToWorkItemParams, ChatConfig, HarnessId, PrCheckoutKind, PrReviewAction,
    PrReviewError, PrReviewErrorCode, RunRequest, SandboxLevel, Space, StartPrReviewParams,
    StartPrReviewReply, WorkOutlineActor, WorkOutlineActorKind, WorkOutlineMutation,
};

use crate::doc_host::DocHost;
use crate::workspace_host::{ChatLinkKind, WorkspaceHost};

/// Derived-id roles (namespaced so they never collide with Work Outline
/// launch ids derived from the same operation id).
pub mod roles {
    pub const CHAT: &str = "pr-review:chat";
    pub const MESSAGE: &str = "pr-review:message";
    pub const COMMAND: &str = "pr-review:command";
    pub const ASSIGNMENT: &str = "pr-review:assignment";
    pub const REPLY: &str = "pr-review:reply";
}

const LEDGER_PREFIX: &str = "pr-review:";
const REPLY_DIR: &str = "pr_review_launches";

fn err(code: PrReviewErrorCode, message: impl Into<String>) -> PrReviewError {
    PrReviewError::new(code, message)
}

fn invalid(message: impl Into<String>) -> PrReviewError {
    err(PrReviewErrorCode::Invalid, message)
}

/// Where `WORKSPACE_WORKTREES_DIR` comes from.
#[derive(Debug, Clone, Default)]
pub enum WorktreesDirEnv {
    /// This process's environment (production).
    #[default]
    Process,
    /// A fixed value (tests): `None` = unset.
    Fixed(Option<String>),
}

impl WorktreesDirEnv {
    fn value(&self) -> Option<String> {
        match self {
            Self::Process => std::env::var("WORKSPACE_WORKTREES_DIR").ok(),
            Self::Fixed(value) => value.clone(),
        }
        .filter(|value| !value.is_empty())
    }
}

/// External tools the launcher shells out to. Tests point `gh` at a fake.
#[derive(Debug, Clone, Default)]
pub struct PrReviewTools {
    /// `gh` executable; `None` = resolve from the login shell PATH.
    pub gh: Option<PathBuf>,
    pub worktrees_dir_env: WorktreesDirEnv,
}

impl PrReviewTools {
    pub(crate) fn gh(&self) -> Result<Command, PrReviewError> {
        let unavailable = || {
            err(
                PrReviewErrorCode::GhUnavailable,
                "The GitHub CLI (gh) is not installed on this device; install it and run `gh auth login`",
            )
        };
        let mut command = match &self.gh {
            Some(path) => Command::new(path),
            None => {
                let executable =
                    mast_harness::resolve_login_shell_executable("gh").ok_or_else(unavailable)?;
                let mut command = Command::new(executable);
                mast_harness::compose_login_shell_path_std(&mut command);
                command
            }
        };
        command
            .env("GH_PROMPT_DISABLED", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null());
        Ok(command)
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = mast_harness::git_command()
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn git_failed(what: &str, detail: String) -> PrReviewError {
    err(PrReviewErrorCode::GitFailed, format!("{what}: {detail}"))
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

// ── repository resolution ───────────────────────────────────────────────

/// `owner/name` (lowercase) of a GitHub remote URL in any of its spellings
/// (`https://github.com/o/r(.git)`, `git@github.com:o/r.git`,
/// `ssh://git@github.com/o/r`), else `None`.
pub fn github_slug(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    let at = url.to_ascii_lowercase().find("github.com")?;
    let rest = &url[at + "github.com".len()..];
    let rest = rest.strip_prefix([':', '/'])?;
    let mut parts = rest.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{name}").to_ascii_lowercase())
}

/// The checkout of `slug` inside `space_root`: the root itself or a
/// (nested) submodule, matched by `remote.origin.url` as configured.
fn find_repo_in(space_root: &Path, slug: &str, depth: usize) -> Option<PathBuf> {
    let origin = git(space_root, &["config", "--get", "remote.origin.url"]).ok();
    if origin.as_deref().and_then(github_slug).as_deref() == Some(slug) {
        return Some(space_root.to_path_buf());
    }
    if depth == 0 || !space_root.join(".gitmodules").is_file() {
        return None;
    }
    let listing = git(
        space_root,
        &[
            "config",
            "--file",
            ".gitmodules",
            "--get-regexp",
            r"^submodule\..*\.path$",
        ],
    )
    .ok()?;
    listing
        .lines()
        .filter_map(|line| line.split_once(' ').map(|(_, path)| path.trim()))
        .map(|path| space_root.join(path))
        .filter(|path| path.join(".git").exists())
        .find_map(|path| find_repo_in(&path, slug, depth - 1))
}

/// Whether `path` is a linked worktree (its git dir differs from the common
/// dir). Submodules and main checkouts are not.
fn is_linked_worktree(path: &Path) -> bool {
    let dir = git(path, &["rev-parse", "--path-format=absolute", "--git-dir"]);
    let common = git(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    matches!((dir, common), (Ok(a), Ok(b)) if canonical(Path::new(&a)) != canonical(Path::new(&b)))
}

#[derive(Debug, Clone)]
struct RepoMatch {
    space: Space,
    /// Canonical Space root.
    ws: PathBuf,
    /// Canonical checkout of the repository.
    repo_dir: PathBuf,
}

/// The Space (among `spaces`, all on this device) holding `slug`: canonical
/// (non-worktree) Spaces first, then the outermost folder, then the oldest.
fn resolve_repo(spaces: &[Space], slug: &str) -> Option<RepoMatch> {
    let mut found: Vec<(bool, usize, RepoMatch)> = spaces
        .iter()
        .filter_map(|space| {
            let ws = canonical(Path::new(&space.path));
            let repo_dir = canonical(&find_repo_in(&ws, slug, 3)?);
            Some((
                is_linked_worktree(&ws),
                ws.components().count(),
                RepoMatch {
                    space: space.clone(),
                    ws,
                    repo_dir,
                },
            ))
        })
        .collect();
    found.sort_by(|a, b| (a.0, a.1, a.2.space.created_at).cmp(&(b.0, b.1, b.2.space.created_at)));
    found.into_iter().next().map(|(_, _, m)| m)
}

// ── gh ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrHead {
    head_ref_name: String,
    #[serde(default)]
    is_cross_repository: bool,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    author: Option<PrAuthor>,
}

#[derive(Debug, Clone, Deserialize)]
struct PrAuthor {
    #[serde(default)]
    login: String,
}

fn gh_pr_view(
    tools: &PrReviewTools,
    owner: &str,
    name: &str,
    number: u64,
) -> Result<PrHead, PrReviewError> {
    let output = tools
        .gh()?
        .args([
            "pr",
            "view",
            &number.to_string(),
            "--repo",
            &format!("{owner}/{name}"),
            "--json",
            "headRefName,isCrossRepository,title,author",
        ])
        .output()
        .map_err(|e| {
            err(
                PrReviewErrorCode::GhUnavailable,
                format!("Could not run the GitHub CLI (gh): {e}"),
            )
        })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(err(
            PrReviewErrorCode::PrNotFound,
            format!("gh pr view {number} --repo {owner}/{name} failed: {detail}"),
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| {
        err(
            PrReviewErrorCode::PrNotFound,
            format!("gh pr view returned unexpected output: {e}"),
        )
    })
}

// ── location choice (launch.sh, line for line) ──────────────────────────

/// launch.sh `worktrees_root`.
pub fn worktrees_root(ws: &Path, env: &WorktreesDirEnv) -> PathBuf {
    let from_file = std::fs::read_to_string(ws.join(".devcontainer/.current-worktrees-root"))
        .ok()
        .and_then(|text| text.lines().next().map(|line| line.trim().to_owned()))
        .filter(|line| !line.is_empty());
    let root = from_file
        .or_else(|| env.value())
        .map(PathBuf::from)
        .unwrap_or_else(|| ws.join(".worktrees"));
    if root.is_absolute() {
        root
    } else {
        ws.join(root)
    }
}

/// launch.sh `checked_out_at`: the path the branch is checked out at. A
/// submodule lists its main worktree as its gitdir, so the first entry maps
/// to `repo_dir`.
fn checked_out_at(repo_dir: &Path, branch: &str) -> Result<Option<PathBuf>, PrReviewError> {
    let listing = git(repo_dir, &["worktree", "list", "--porcelain"])
        .map_err(|e| git_failed("git worktree list", e))?;
    let wanted = format!("branch refs/heads/{branch}");
    let mut n = 0;
    let mut path = PathBuf::new();
    for line in listing.lines() {
        if let Some(entry) = line.strip_prefix("worktree ") {
            n += 1;
            path = if n == 1 {
                repo_dir.to_path_buf()
            } else {
                PathBuf::from(entry)
            };
        } else if line == wanted {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChosenLocation {
    pub path: PathBuf,
    pub kind: PrCheckoutKind,
    pub notes: Vec<String>,
}

/// Steps 3-4: fetch, then reuse / `gh pr checkout` / new worktree.
pub fn choose_location(
    tools: &PrReviewTools,
    ws: &Path,
    repo_dir: &Path,
    repo_name: &str,
    number: u64,
    branch: &str,
) -> Result<ChosenLocation, PrReviewError> {
    git(
        repo_dir,
        &[
            "fetch",
            "--quiet",
            "origin",
            &format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"),
        ],
    )
    .map_err(|e| git_failed(&format!("git fetch origin {branch}"), e))?;

    if let Some(path) = checked_out_at(repo_dir, branch)? {
        return Ok(ChosenLocation {
            notes: vec![format!(
                "{branch} is already checked out at {}",
                path.display()
            )],
            path,
            kind: PrCheckoutKind::Existing,
        });
    }
    let current = git(repo_dir, &["branch", "--show-current"]).unwrap_or_default();
    let tracked_changes = git(
        repo_dir,
        &[
            "status",
            "--porcelain",
            "--untracked-files=no",
            "--ignore-submodules=all",
        ],
    )
    .map_err(|e| git_failed("git status", e))?;
    if current == "main" && tracked_changes.is_empty() {
        let output = tools
            .gh()?
            .args(["pr", "checkout", &number.to_string()])
            .current_dir(repo_dir)
            .output()
            .map_err(|e| {
                err(
                    PrReviewErrorCode::GhUnavailable,
                    format!("Could not run the GitHub CLI (gh): {e}"),
                )
            })?;
        if !output.status.success() {
            return Err(git_failed(
                &format!("gh pr checkout {number} in {}", repo_dir.display()),
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        return Ok(ChosenLocation {
            path: repo_dir.to_path_buf(),
            kind: PrCheckoutKind::CanonicalCheckout,
            notes: vec![format!("Checked out {branch} in {}", repo_dir.display())],
        });
    }

    let root = worktrees_root(ws, &tools.worktrees_dir_env);
    let path = root.join(format!("{repo_name}-pr-{number}"));
    if path.exists() {
        return Err(err(
            PrReviewErrorCode::WorktreePathExists,
            format!(
                "{} exists but is not on {branch}; remove it or check out {branch} there.",
                path.display()
            ),
        ));
    }
    std::fs::create_dir_all(&root)
        .map_err(|e| git_failed(&format!("create {}", root.display()), e.to_string()))?;
    let path_arg = path.to_string_lossy().into_owned();
    let local_branch_exists = git(
        repo_dir,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok();
    let added = if local_branch_exists {
        git(repo_dir, &["worktree", "add", &path_arg, branch])
    } else {
        git(
            repo_dir,
            &[
                "worktree",
                "add",
                "--track",
                "-b",
                branch,
                &path_arg,
                &format!("origin/{branch}"),
            ],
        )
    };
    added.map_err(|e| git_failed(&format!("git worktree add {path_arg}"), e))?;
    let why = if current.is_empty() {
        "a detached HEAD".to_owned()
    } else {
        current
    };
    Ok(ChosenLocation {
        notes: vec![format!(
            "{} is on {why} or has changes; added worktree {}",
            repo_dir.display(),
            path.display()
        )],
        path: canonical(&path),
        kind: PrCheckoutKind::NewWorktree,
    })
}

/// launch.sh `behind`: commits `path`'s HEAD is behind `origin/<branch>`.
fn behind_by(path: &Path, branch: &str) -> u64 {
    git(
        path,
        &["rev-list", "--count", &format!("HEAD..origin/{branch}")],
    )
    .ok()
    .and_then(|count| count.trim().parse().ok())
    .unwrap_or(0)
}

/// launch.sh's `location` text. The devcontainer mount note is added only
/// when the workspace has a `.devcontainer` (Cofactr), where launch.sh's
/// wording applies verbatim.
pub fn describe_location(ws: &Path, root: &Path, path: &Path, branch: &str, behind: u64) -> String {
    let shown = path.display();
    let mut location = if path == ws {
        "the canonical workspace checkout".to_owned()
    } else if path.starts_with(ws.join(".worktrees")) || path.starts_with(root) {
        let base = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if ws.join(".devcontainer").is_dir() {
            format!("the worktree `{shown}` (`/workspace/.worktrees/{base}` in the devcontainer)")
        } else {
            format!("the worktree `{shown}`")
        }
    } else if path.starts_with(ws) {
        format!("the canonical checkout `{shown}`")
    } else {
        format!("`{shown}`")
    };
    if behind > 0 {
        location.push_str(&format!(
            ", {behind} commit(s) behind origin/{branch} — pull before you start"
        ));
    }
    location
}

/// launch.sh's prompts, verbatim, except that a review of the viewer's own PR
/// (`self_review`) reviews it as its author instead of as a reviewer.
pub fn seeded_prompt(
    action: PrReviewAction,
    url: &str,
    branch: &str,
    location: &str,
    self_review: bool,
) -> String {
    match action {
        PrReviewAction::CodeReview if self_review => format!(
            "/code-review {url} — my own PR; review it as its author, before or while I wait for reviewers: look for bugs, missing tests, risky changes and unclear diffs. Its branch `{branch}` is checked out at {location}; review the PR's commits against its base, not local changes. Report findings to me here. Fix clear problems on the PR branch only if I ask, and don't post review comments or approve on GitHub."
        ),
        PrReviewAction::FixCi => format!(
            "/fix-ci {url} — my PR. Its branch `{branch}` is checked out at {location}; do all the work there."
        ),
        PrReviewAction::CodeReview => format!(
            "/code-review {url} — someone else's PR; review it as a reviewer. Its branch `{branch}` is checked out at {location}; review the PR's commits against its base, not local changes. Report findings to me here and don't post anything to GitHub unless I ask."
        ),
    }
}

pub fn chat_title(action: PrReviewAction, repo_name: &str, number: u64, pr_title: &str) -> String {
    let verb = match action {
        PrReviewAction::CodeReview => "Review",
        PrReviewAction::FixCi => "Fix CI",
    };
    let pr_title = pr_title.trim();
    if pr_title.is_empty() {
        format!("{verb} {repo_name}#{number}")
    } else {
        format!("{verb} {repo_name}#{number}: {pr_title}")
    }
}

/// The config a review Chat runs with when the caller sends none: Claude
/// Code with Mast's default model (never the CLI's own default).
pub fn default_config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::ClaudeCode,
        model: mast_proto::resolve_model(HarnessId::ClaudeCode, None),
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
    }
}

// ── the service ─────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LaunchInputs<'a> {
    repo_owner: String,
    repo_name: String,
    number: u64,
    action: PrReviewAction,
    space_id: &'a Option<String>,
    work_item_id: &'a Option<String>,
    config: &'a Option<ChatConfig>,
    /// Only serialized when set, so earlier starts keep their inputs hash.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    fresh: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredReply {
    inputs_hash: String,
    /// The action this start ran (reuse matches PR + action). Absent in
    /// replies stored before reuse existed; [`stored_action`] falls back to
    /// the reply's title verb.
    #[serde(default)]
    action: Option<PrReviewAction>,
    reply: StartPrReviewReply,
}

/// The action a stored launch ran: the recorded one, else the verb its Chat
/// title was minted with ([`chat_title`]).
fn stored_action(stored: &StoredReply) -> Option<PrReviewAction> {
    stored.action.or_else(|| {
        let title = stored.reply.title.as_str();
        if title.starts_with("Fix CI ") {
            Some(PrReviewAction::FixCi)
        } else if title.starts_with("Review ") {
            Some(PrReviewAction::CodeReview)
        } else {
            None
        }
    })
}

/// The canonical (lowercase) PR URL of a launch's repo + number.
fn launch_pr_url(owner: &str, name: &str, number: u64) -> String {
    crate::chat_links::canonicalize_pr_url(&format!(
        "https://github.com/{owner}/{name}/pull/{number}"
    ))
}

fn check_part(label: &str, value: &str) -> Result<(), PrReviewError> {
    let ok = !value.is_empty()
        && value.len() <= 100
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(invalid(format!("{label} is not a valid GitHub name")))
    }
}

fn check_operation_id(operation_id: &str) -> Result<(), PrReviewError> {
    let ok = !operation_id.is_empty()
        && operation_id.chars().count() <= 64
        && operation_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
    if ok {
        Ok(())
    } else {
        Err(invalid(
            "operationId must be 1-64 characters of letters, digits, '-', '_', '.', ':'",
        ))
    }
}

/// The signed-in GitHub login (the engine's cached `PrViewer`); `None` when
/// it is unknown.
pub type ViewerLogin =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>;

/// `StartPrReview` on the device that owns the Space. Cheap to clone.
#[derive(Clone)]
pub struct PrReviewLauncher {
    workspace: WorkspaceHost,
    doc_host: DocHost,
    tools: PrReviewTools,
    viewer: Option<ViewerLogin>,
    gate: Arc<tokio::sync::Mutex<()>>,
}

impl PrReviewLauncher {
    pub fn new(workspace: WorkspaceHost, doc_host: DocHost, tools: PrReviewTools) -> Self {
        Self {
            workspace,
            doc_host,
            tools,
            viewer: None,
            gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Where the viewer's login comes from; without it every PR is reviewed
    /// as someone else's.
    pub fn with_viewer(mut self, viewer: ViewerLogin) -> Self {
        self.viewer = Some(viewer);
        self
    }

    /// Whether the viewer is the PR's author (an unknown login on either
    /// side is "no").
    async fn is_own_pr(&self, head: &PrHead) -> bool {
        let Some(author) = head.author.as_ref().filter(|a| !a.login.is_empty()) else {
            return false;
        };
        let Some(viewer) = &self.viewer else {
            return false;
        };
        viewer()
            .await
            .is_some_and(|login| login.eq_ignore_ascii_case(&author.login))
    }

    pub fn with_tools(mut self, tools: PrReviewTools) -> Self {
        self.tools = tools;
        self
    }

    fn reply_path(&self, operation_id: &str) -> PathBuf {
        self.workspace
            .store_root()
            .join(REPLY_DIR)
            .join(format!("{}.json", derive_id(operation_id, roles::REPLY)))
    }

    fn stored_reply(&self, operation_id: &str) -> Option<StoredReply> {
        let raw = std::fs::read(self.reply_path(operation_id)).ok()?;
        serde_json::from_slice(&raw).ok()
    }

    fn store_reply(&self, operation_id: &str, stored: &StoredReply) {
        let path = self.reply_path(operation_id);
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(stored)?)?;
            std::fs::rename(&tmp, &path)
        };
        if let Err(error) = write() {
            tracing::warn!(%error, "pr review reply sidecar write failed");
        }
    }

    /// Record the operation's inputs; a replay with other inputs conflicts.
    fn claim_inputs(&self, operation_id: &str, inputs_hash: &str) -> Result<(), PrReviewError> {
        let op_key = format!("{LEDGER_PREFIX}launch:{operation_id}");
        let inputs_key = format!("{LEDGER_PREFIX}launch-in:{inputs_hash}:{operation_id}");
        if self.workspace.ledger_holds(&op_key) {
            return if self.workspace.ledger_holds(&inputs_key) {
                Ok(())
            } else {
                Err(err(
                    PrReviewErrorCode::OperationConflict,
                    "operationId was already used for a different PR review start",
                ))
            };
        }
        self.workspace
            .ledger_mark(&inputs_key)
            .and_then(|_| self.workspace.ledger_mark(&op_key))
            .map(drop)
            .map_err(|e| invalid(e.to_string()))
    }

    /// This device's Spaces (or the pinned one) that could hold the repo.
    fn candidate_spaces(&self, space_id: Option<&str>) -> Result<Vec<Space>, PrReviewError> {
        let local = self.workspace.device_id().to_owned();
        match space_id {
            Some(space_id) => {
                let space = self
                    .workspace
                    .space(space_id)
                    .map_err(|e| invalid(e.to_string()))?
                    .ok_or_else(|| {
                        err(PrReviewErrorCode::RepoNotFound, "The Space was not found")
                    })?;
                if space.device_id != local {
                    return Err(err(
                        PrReviewErrorCode::DeviceOffline,
                        "The Space lives on another device",
                    ));
                }
                Ok(vec![space])
            }
            None => Ok(self
                .workspace
                .read_spaces()
                .map_err(|e| invalid(e.to_string()))?
                .into_iter()
                .filter(|space| space.device_id == local)
                .collect()),
        }
    }

    /// Run (or resume) a start on this device.
    pub async fn start(
        &self,
        p: &StartPrReviewParams,
    ) -> Result<StartPrReviewReply, PrReviewError> {
        check_operation_id(&p.operation_id)?;
        check_part("repoOwner", &p.repo_owner)?;
        check_part("repoName", &p.repo_name)?;
        if p.number == 0 {
            return Err(invalid("PR number must be positive"));
        }
        let _gate = self.gate.lock().await;
        let op = p.operation_id.as_str();
        let inputs_hash = mast_doc::registry::work_outline::inputs_hash(&LaunchInputs {
            repo_owner: p.repo_owner.to_ascii_lowercase(),
            repo_name: p.repo_name.to_ascii_lowercase(),
            number: p.number,
            action: p.action,
            space_id: &p.space_id,
            work_item_id: &p.work_item_id,
            config: &p.config,
            fresh: p.fresh,
        });
        let chat_id = derive_id(op, roles::CHAT);
        let message_id = derive_id(op, roles::MESSAGE);
        let command_id = derive_id(op, roles::COMMAND);

        if let Some(stored) = self.stored_reply(op) {
            if stored.inputs_hash != inputs_hash {
                return Err(err(
                    PrReviewErrorCode::OperationConflict,
                    "operationId was already used for a different PR review start",
                ));
            }
            return Ok(stored.reply);
        }
        // An existing live review Chat for this PR and action is reused:
        // no new Chat, checkout, `gh` call or Run. A confirmed Start with an
        // agent pick (`fresh`) always starts a new one.
        if let Some(reply) = self.reusable_review(p).filter(|_| !p.fresh) {
            if let Some(item) = &p.work_item_id {
                match self.workspace.work_item_space(item) {
                    Some(space) if space == reply.space_id => {}
                    Some(_) => {
                        return Err(invalid(
                            "The work item belongs to another project than the PR's checkout",
                        ));
                    }
                    None => return Err(invalid("Work item not found")),
                }
            }
            self.claim_inputs(op, &inputs_hash)?;
            if let Some(item) = &p.work_item_id {
                self.ensure_assignment(&reply.chat_id, item, op)?;
            }
            self.store_reply(
                op,
                &StoredReply {
                    inputs_hash,
                    action: Some(p.action),
                    reply: reply.clone(),
                },
            );
            return Ok(reply);
        }
        if self
            .workspace
            .registry_row_state(mast_doc::KIND_CHATS, &chat_id)
            == RowState::Tombstoned
        {
            return Err(invalid(
                "The Chat this review start created was deleted; start a new review",
            ));
        }

        // 1. The Space and the repository checkout (local, cheap).
        let slug = format!("{}/{}", p.repo_owner, p.repo_name).to_ascii_lowercase();
        let spaces = self.candidate_spaces(p.space_id.as_deref())?;
        let resolve_slug = slug.clone();
        let found = tokio::task::spawn_blocking(move || resolve_repo(&spaces, &resolve_slug))
            .await
            .map_err(|e| invalid(e.to_string()))?;
        let Some(found) = found else {
            let message = match &p.space_id {
                Some(_) => format!(
                    "No checkout of github.com/{slug} (by its origin remote) was found in this Space or its submodules"
                ),
                None => format!(
                    "No Space on this device holds a checkout of github.com/{slug} (matched by the origin remote of the Space folder or its submodules). Add the workspace that contains it as a project"
                ),
            };
            return Err(err(PrReviewErrorCode::RepoNotFound, message));
        };
        if let Some(item) = &p.work_item_id {
            match self.workspace.work_item_space(item) {
                Some(space) if space == found.space.id => {}
                Some(_) => {
                    return Err(invalid(
                        "The work item belongs to another project than the PR's checkout",
                    ));
                }
                None => return Err(invalid("Work item not found")),
            }
        }

        // 2-5. gh + git, off the runtime.
        let tools = self.tools.clone();
        let (owner, name, number) = (p.repo_owner.clone(), p.repo_name.clone(), p.number);
        let ws = found.ws.clone();
        let repo_dir = found.repo_dir.clone();
        let claim = {
            let launcher = self.clone();
            let op = op.to_owned();
            let inputs_hash = inputs_hash.clone();
            move || launcher.claim_inputs(&op, &inputs_hash)
        };
        let checkout = tokio::task::spawn_blocking(move || {
            let head = gh_pr_view(&tools, &owner, &name, number)?;
            if head.is_cross_repository {
                return Err(err(
                    PrReviewErrorCode::Fork,
                    format!(
                        "PR {number} comes from a fork; check it out by hand (Mast only checks out branches pushed to {owner}/{name})."
                    ),
                ));
            }
            claim()?;
            let branch = head.head_ref_name.clone();
            let chosen = choose_location(&tools, &ws, &repo_dir, &name, number, &branch)?;
            let behind = behind_by(&chosen.path, &branch);
            let root = worktrees_root(&ws, &tools.worktrees_dir_env);
            let location = describe_location(&ws, &root, &chosen.path, &branch, behind);
            let url = format!("https://github.com/{owner}/{name}/pull/{number}");
            Ok::<_, PrReviewError>((head, chosen, behind, url, location))
        })
        .await
        .map_err(|e| invalid(e.to_string()))??;
        let (head, chosen, behind, url, location) = checkout;
        let self_review =
            matches!(p.action, PrReviewAction::CodeReview) && self.is_own_pr(&head).await;
        let prompt = seeded_prompt(p.action, &url, &head.head_ref_name, &location, self_review);
        let branch = head.head_ref_name.clone();
        let mut notes = chosen.notes.clone();
        if behind > 0 {
            notes.push(format!(
                "{} is {behind} commit(s) behind origin/{branch} — pull before you start",
                chosen.path.display()
            ));
        }

        // 6. The Chat, at the Space root.
        let title = chat_title(
            p.action,
            &p.repo_name,
            p.number,
            head.title.as_deref().unwrap_or(""),
        );
        let cwd = found.space.path.clone();
        let mut config = p.config.clone().unwrap_or_else(default_config);
        if config.fill_default_model() {
            tracing::debug!(chat = %chat_id, "review chat config had no model; filled in the Mast default");
        }
        let engine = |e: crate::EngineError| invalid(e.to_string());
        let existing = self.workspace.chat(&chat_id).map_err(engine)?;
        if existing.is_none() {
            self.workspace
                .create_chat_with_parent(
                    &chat_id,
                    Some(&found.space.id),
                    None,
                    Some(config.clone()),
                    Some(cwd.clone()),
                    None,
                )
                .map_err(engine)?;
        }
        let titled = existing
            .as_ref()
            .and_then(|chat| chat.title.as_deref())
            .is_some_and(|title| !title.trim().is_empty());
        if !titled {
            self.workspace
                .rename_chat(&chat_id, &title)
                .map_err(engine)?;
        }
        self.workspace
            .set_chat_link(
                &chat_id,
                ChatLinkKind::Pr,
                Some(&url),
                mast_proto::ChatLinkSource::ReviewLauncher,
            )
            .map_err(engine)?;
        if let Some(item) = &p.work_item_id {
            self.ensure_assignment(&chat_id, item, op)?;
        }
        let request = RunRequest {
            prompt: prompt.clone(),
            harness: Some(config.harness),
            model: config.model.clone(),
            reasoning: config.reasoning,
            model_options: config.model_options.clone(),
            cwd: cwd.clone(),
            sandbox: config.sandbox,
            auto_approve: config.auto_approve,
            resume: None,
            attachments: Vec::new(),
            worktree: None,
            instructions: None,
        };
        let (_, landed) = self
            .doc_host
            .message_command_state(&chat_id, &message_id)
            .map_err(engine)?;
        if !landed {
            self.doc_host
                .queue_command_with_id(
                    &chat_id,
                    &command_id,
                    SessionCommandPayload::Run {
                        request,
                        message_id: message_id.clone(),
                    },
                )
                .map_err(engine)?;
        }

        let reply = StartPrReviewReply {
            chat_id,
            space_id: found.space.id.clone(),
            device_id: found.space.device_id.clone(),
            title,
            pr_url: url,
            branch,
            checkout_path: chosen.path.to_string_lossy().into_owned(),
            checkout_kind: chosen.kind,
            cwd,
            behind_by: behind,
            prompt,
            notes,
            reused: false,
        };
        self.store_reply(
            op,
            &StoredReply {
                inputs_hash,
                action: Some(p.action),
                reply: reply.clone(),
            },
        );
        Ok(reply)
    }

    /// The reply of a live review Chat this device launched earlier for the
    /// same PR and action, with `reused` set. The launcher's stored replies
    /// are the record of what it launched (they carry the action and the
    /// checkout values); a candidate must also still exist, be unarchived,
    /// hold a durable link to the PR (canonical, so casing never matters),
    /// sit in the requested Space, and not have been re-categorized away from
    /// `pr_review` (an unset category counts: the RPC layer classifies after
    /// the launch returns). The most recently active candidate wins.
    fn reusable_review(&self, p: &StartPrReviewParams) -> Option<StartPrReviewReply> {
        let target = launch_pr_url(&p.repo_owner, &p.repo_name, p.number);
        let dir = self.workspace.store_root().join(REPLY_DIR);
        let mut best: Option<(chrono::DateTime<chrono::Utc>, StartPrReviewReply)> = None;
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let Ok(raw) = std::fs::read(entry.path()) else {
                continue;
            };
            let Ok(stored) = serde_json::from_slice::<StoredReply>(&raw) else {
                continue;
            };
            if stored_action(&stored) != Some(p.action)
                || crate::chat_links::canonicalize_pr_url(&stored.reply.pr_url) != target
            {
                continue;
            }
            let Ok(Some(chat)) = self.workspace.chat(&stored.reply.chat_id) else {
                continue;
            };
            if chat.archived
                || p.space_id
                    .as_deref()
                    .is_some_and(|space| chat.space_id.as_deref() != Some(space))
            {
                continue;
            }
            let linked = self.workspace.chat_pr_links(&chat.id).is_ok_and(|links| {
                links.iter().any(|link| {
                    link.source == mast_proto::ChatLinkSource::ReviewLauncher
                        && crate::chat_links::canonicalize_pr_url(&link.url) == target
                })
            });
            if !linked {
                continue;
            }
            let active = chat.last_message_at.unwrap_or(chat.created_at);
            if best.as_ref().is_some_and(|(at, _)| *at >= active) {
                continue;
            }
            let mut reply = stored.reply;
            if let Some(title) = chat.title.as_deref().filter(|t| !t.trim().is_empty()) {
                reply.title = title.to_owned();
            }
            // The checkout side effects are not repeated, so the behind-by
            // count and its notes would be stale: report none.
            reply.behind_by = 0;
            reply.notes = Vec::new();
            reply.reused = true;
            best = Some((active, reply));
        }
        best.map(|(_, reply)| reply)
    }

    fn ensure_assignment(
        &self,
        chat_id: &str,
        work_item_id: &str,
        operation_id: &str,
    ) -> Result<(), PrReviewError> {
        let row = self
            .workspace
            .registry_row(mast_doc::KIND_WORK_ITEM_CHAT_ASSIGNMENTS, chat_id)
            .filter(|row| !row.deleted);
        if row.is_some() {
            // Assigned already (this start's earlier attempt, or the user
            // moved it since): never stomp.
            return Ok(());
        }
        let actor = WorkOutlineActor {
            kind: WorkOutlineActorKind::User,
            device_id: self.workspace.device_id().to_owned(),
            chat_id: None,
            message_id: None,
        };
        self.workspace
            .work_outline_mutate(
                &WorkOutlineMutation::AssignChatToWorkItem(AssignChatToWorkItemParams {
                    chat_id: chat_id.to_owned(),
                    work_item_id: work_item_id.to_owned(),
                    operation_id: derive_id(operation_id, roles::ASSIGNMENT),
                }),
                &actor,
                &[],
            )
            .map(drop)
            .map_err(|e| invalid(format!("Could not add the Chat to the work item: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_slugs_cover_every_remote_spelling() {
        for url in [
            "https://github.com/Cofactr/flagship",
            "https://github.com/Cofactr/flagship.git",
            "https://github.com/Cofactr/flagship/",
            "git@github.com:Cofactr/flagship.git",
            "ssh://git@github.com/Cofactr/flagship.git",
            "https://token@github.com/Cofactr/flagship.git",
        ] {
            assert_eq!(
                github_slug(url).as_deref(),
                Some("cofactr/flagship"),
                "{url}"
            );
        }
        assert_eq!(github_slug("https://gitlab.com/Cofactr/flagship"), None);
        assert_eq!(github_slug("/tmp/remotes/flagship.git"), None);
        assert_eq!(github_slug("https://github.com/Cofactr"), None);
    }

    #[test]
    fn location_wording_matches_launch_sh() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        std::fs::create_dir_all(ws.join(".devcontainer")).unwrap();
        let root = ws.join(".worktrees");
        assert_eq!(
            describe_location(&ws, &root, &ws, "b", 0),
            "the canonical workspace checkout"
        );
        assert_eq!(
            describe_location(&ws, &root, &ws.join("flagship"), "b", 0),
            format!("the canonical checkout `{}`", ws.join("flagship").display())
        );
        let wt = root.join("flagship-pr-7");
        assert_eq!(
            describe_location(&ws, &root, &wt, "b", 2),
            format!(
                "the worktree `{}` (`/workspace/.worktrees/flagship-pr-7` in the devcontainer), 2 commit(s) behind origin/b — pull before you start",
                wt.display()
            )
        );
        let elsewhere = tmp.path().join("elsewhere");
        assert_eq!(
            describe_location(&ws, &root, &elsewhere, "b", 0),
            format!("`{}`", elsewhere.display())
        );
    }

    #[test]
    fn titles_name_the_repo_and_pr() {
        assert_eq!(
            chat_title(PrReviewAction::CodeReview, "flagship", 12, "Fix the cache"),
            "Review flagship#12: Fix the cache"
        );
        assert_eq!(
            chat_title(PrReviewAction::FixCi, "ui", 3, ""),
            "Fix CI ui#3"
        );
    }
}

#[cfg(test)]
mod default_model_tests {
    use super::*;

    #[test]
    fn the_default_review_config_names_the_mast_default_model() {
        let config = default_config();
        assert_eq!(config.harness, HarnessId::ClaudeCode);
        assert_eq!(config.model.as_deref(), Some("claude-opus-5-5"));
    }

    #[test]
    fn a_caller_config_without_a_model_is_filled_in() {
        let mut config = ChatConfig {
            model: None,
            ..default_config()
        };
        assert!(config.fill_default_model());
        assert_eq!(config.model.as_deref(), Some("claude-opus-5-5"));
        assert!(!config.fill_default_model(), "an explicit model is kept");
    }
}
