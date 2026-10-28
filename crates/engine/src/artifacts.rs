//! Planning artifacts (phase 1: design A + B; contract
//! `docs/plans/artifacts-contract.md`, agent conventions
//! `docs/planning-artifact-conventions.md`).
//!
//! - **Location.** [`ArtifactsService::canonical_root`]: the main checkout of
//!   the work item's resolved primary repository (never a worktree), else the
//!   Space folder's repository main checkout, else the Space folder. Files
//!   live in `<canonical>/.mast/artifacts/<folder>/` (or the legacy
//!   `.zeron/artifacts/<folder>/` when that folder already exists); both are appended
//!   once to `git rev-parse --git-path info/exclude`.
//! - **Registry.** One `artifacts` row per artifact (metadata only); every
//!   revision's bytes go to `<profile store>/artifacts/<id>/<sha256>`.
//! - **Writers.** `artifact_publish` (agents), `ArtifactRegister` (user
//!   import), the marker fallback (assistant text), and the watcher
//!   (external edits) all go through [`ArtifactsService::record`] under one
//!   gate, so the watcher never mistakes Mast's own write for an edit.
//! - **Render checks.** [`RenderBroker`]: the headed UI holds
//!   `WatchArtifactRenderRequests` open; a request goes to the newest live
//!   subscriber and waits up to 30 s for `ArtifactRenderResult`.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use base64::Engine as _;
use chrono::Utc;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::Deserialize;
use serde_json::{Value, json};
use mast_proto::artifacts::{
    ArtifactAddCommentParams, ArtifactAddCommentReply, ArtifactComment,
    ArtifactResolveCommentParams, ArtifactResolveCommentReply, CommentStatus, MAX_COMMENTS,
    MAX_COMMENT_CHARS, MAX_QUOTE_CHARS, kind_supports_comments, next_comment_id,
    normalize_comment_id, resolve_anchor,
    ARTIFACTS_DIR, LEGACY_ARTIFACTS_DIR, LEGACY_ARTIFACT_URL_SCHEME, LEGACY_ZERON_DIR, ArtifactContentEncoding, ArtifactContentParams, ArtifactContentReply,
    ArtifactError, ArtifactErrorCode, ArtifactKind, ArtifactPinReply, ArtifactPromoteReply,
    ArtifactPublishReply, ArtifactRegisterParams, ArtifactRenderCheckResult,
    ArtifactRenderCompanion, ArtifactRenderRequest, ArtifactRenderResultParams,
    ArtifactRenderTheme, ArtifactRenderViewport, ArtifactRevision, ArtifactView, ArtifactViewport,
    ArtifactsSnapshot, EDITED_WITHOUT_NOTE, IMPORTED_NOTE, OptionsManifest, PROMOTE_DIR, PlanScope,
    SPACE_FOLDER, MAST_DIR, artifact_id, artifact_limits, artifact_links, chat_folder, clean_note,
    clean_title, content_hash, count_open_questions, count_unverified_claims,
    is_safe_relative_path, mime_type_for, publish_operation_id, tagged_file_name, work_item_folder,
};
use mast_proto::{ExecutionScope, Space, WorkOutlineActor, WorkOutlineActorKind};

use crate::workspace_host::WorkspaceHost;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn clean_answer(answer: &str) -> String {
    let answer = answer.trim();
    match answer.char_indices().nth(mast_proto::artifacts::MAX_ANSWER_CHARS) {
        Some((cut, _)) => answer[..cut].to_owned(),
        None => answer.to_owned(),
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

type Outcome<T> = Result<T, ArtifactError>;

fn io_error(what: &str, path: &Path, error: impl std::fmt::Display) -> ArtifactError {
    ArtifactError::io(format!("{what} {}: {error}", path.display()))
}

/// How often the owning device polls stored files for external edits.
pub const WATCH_INTERVAL: Duration = Duration::from_secs(2);
/// Ledger key prefix of the marker fallback's "imported once" marks.
const MARKER_LEDGER_PREFIX: &str = "artifact-marker:";

// ── git helpers (synchronous; callers run on blocking threads) ──────────────

fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = mast_harness::git_command()
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The main checkout of the repository containing `path` (never a linked
/// worktree): `<common dir>/..` for a regular repository, the module's
/// `core.worktree` for a submodule. `None` outside git.
pub fn main_checkout(path: &Path) -> Option<PathBuf> {
    let common = git(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common = canonical(Path::new(&common));
    if common.file_name().is_some_and(|name| name == ".git") {
        return common.parent().map(Path::to_path_buf);
    }
    let config = common.join("config");
    let worktree = git(
        path,
        &[
            "config",
            "--file",
            &config.to_string_lossy(),
            "--get",
            "core.worktree",
        ],
    )
    .filter(|w| !w.is_empty());
    if let Some(worktree) = worktree {
        return Some(canonical(&common.join(worktree)));
    }
    git(path, &["rev-parse", "--show-toplevel"]).map(|top| canonical(Path::new(&top)))
}

/// Whether an `info/exclude` line already lists `dir` (`.mast`, `.zeron`).
fn exclude_lists(line: &str, dir: &str) -> bool {
    [
        dir.to_owned(),
        format!("{dir}/"),
        format!("/{dir}"),
        format!("/{dir}/"),
        format!("{dir}/*"),
        format!("/{dir}/*"),
    ]
    .iter()
    .any(|candidate| candidate == line)
}

/// Append `.mast/` and the legacy `.zeron/` to the checkout's `info/exclude` (resolved with `git
/// rev-parse --git-path`, so submodules get their module's file) unless it
/// is already listed (each entry separately). Never touches `.gitignore`. `Ok(false)` when nothing
/// was written (already listed, or not a git checkout).
pub fn ensure_excluded(checkout: &Path) -> Outcome<bool> {
    let Some(raw) = git(
        checkout,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "info/exclude",
        ],
    ) else {
        return Ok(false);
    };
    let mut exclude = PathBuf::from(raw);
    if exclude.is_relative() {
        exclude = checkout.join(exclude);
    }
    let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
    let missing: Vec<&str> = [MAST_DIR, LEGACY_ZERON_DIR]
        .into_iter()
        .filter(|dir| {
            !existing
                .lines()
                .map(str::trim)
                .any(|line| exclude_lists(line, dir))
        })
        .collect();
    if missing.is_empty() {
        return Ok(false);
    }
    if let Some(parent) = exclude.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_error("create", parent, e))?;
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    for dir in missing {
        text.push_str(&format!("{dir}/\n"));
    }
    std::fs::write(&exclude, text).map_err(|e| io_error("write", &exclude, e))?;
    Ok(true)
}

/// Write `bytes` to `path` atomically (temp file + rename), creating parents.
fn write_atomic(path: &Path, bytes: &[u8]) -> Outcome<()> {
    let parent = path
        .parent()
        .ok_or_else(|| ArtifactError::invalid(format!("{} has no parent", path.display())))?;
    std::fs::create_dir_all(parent).map_err(|e| io_error("create", parent, e))?;
    let tmp = parent.join(format!(
        ".{}.mast-tmp-{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("artifact"),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&tmp, bytes).map_err(|e| io_error("write", &tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io_error("rename into", path, e)
    })
}

/// Read a regular file of at most `MAX_FILE_BYTES`.
fn read_file(path: &Path) -> Outcome<Vec<u8>> {
    let meta = std::fs::metadata(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ArtifactError::not_found(format!("{} does not exist", path.display()))
        } else {
            io_error("stat", path, e)
        }
    })?;
    if !meta.is_file() {
        return Err(ArtifactError::invalid(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if meta.len() > artifact_limits::MAX_FILE_BYTES {
        return Err(ArtifactError::new(
            ArtifactErrorCode::TooLarge,
            format!(
                "{} is {} bytes; artifacts are limited to {} bytes",
                path.display(),
                meta.len(),
                artifact_limits::MAX_FILE_BYTES
            ),
        ));
    }
    std::fs::read(path).map_err(|e| io_error("read", path, e))
}

fn file_name_of(path: &Path) -> Outcome<String> {
    path.file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ArtifactError::invalid(format!("{} has no file name", path.display())))
}

/// File stem without `.fragment.html` / `.preview.html` / extension.
fn default_title_for(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("artifact");
    let html = name.to_ascii_lowercase().ends_with(".html");
    html.then(|| mast_proto::visualize::default_title(name))
        .flatten()
        .or_else(|| {
            Path::new(name)
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "Artifact".to_owned())
}

/// The canonical checkout an artifact's stored file lives in (the ancestor
/// that holds `.mast/artifacts`, or the legacy `.zeron/artifacts`).
fn canonical_of_stored(stored: &Path) -> Option<PathBuf> {
    stored
        .ancestors()
        .find(|a| a.ends_with(ARTIFACTS_DIR) || a.ends_with(LEGACY_ARTIFACTS_DIR))
        .and_then(|a| a.parent())
        .and_then(|a| a.parent())
        .map(Path::to_path_buf)
}

/// Home-relative locations whose contents are never copied into the artifact
/// store (directories match everything below them).
const DENIED_HOME_PATHS: &[&str] = &[
    ".ssh",
    ".gnupg",
    "Library/Keychains",
    ".aws",
    ".config/gh",
    ".netrc",
    ".docker/config.json",
];

fn is_env_file_name(name: &str) -> bool {
    name == ".env" || name.starts_with(".env.")
}

fn denied(path: &Path, why: &str) -> ArtifactError {
    ArtifactError::new(
        ArtifactErrorCode::Denied,
        format!("{} cannot be published: {why}", path.display()),
    )
}

/// Authorize a local file as an artifact SOURCE: any regular file on this
/// device's filesystem, except the deny-list (credential stores, `.env`
/// files, files over the size limit). Symlinks are resolved first and the
/// deny-list applies to the real path. Returns the real path.
fn check_source(path: &Path, home: Option<&Path>) -> Outcome<PathBuf> {
    let real = std::fs::canonicalize(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ArtifactError::not_found(format!("{} does not exist", path.display()))
        } else {
            io_error("resolve", path, e)
        }
    })?;
    let meta = std::fs::metadata(&real).map_err(|e| io_error("stat", &real, e))?;
    if !meta.is_file() {
        return Err(denied(&real, "it is not a regular file"));
    }
    let named_env = |p: &Path| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(is_env_file_name)
    };
    if named_env(&real) || named_env(path) {
        return Err(denied(&real, "`.env` files may hold secrets"));
    }
    if let Some(home) = home {
        let homes = [home.to_path_buf(), canonical(home)];
        for rel in DENIED_HOME_PATHS {
            for home in &homes {
                let entry = home.join(rel);
                if real.starts_with(&entry) || real.starts_with(canonical(&entry)) {
                    return Err(denied(
                        &real,
                        &format!("~/{rel} holds credentials and is never published"),
                    ));
                }
            }
        }
    }
    if meta.len() > artifact_limits::MAX_FILE_BYTES {
        return Err(denied(
            &real,
            &format!(
                "it is {} bytes; artifacts are limited to {} bytes",
                meta.len(),
                artifact_limits::MAX_FILE_BYTES
            ),
        ));
    }
    Ok(real)
}

// ── publish requests ────────────────────────────────────────────────────────

/// One write of an artifact (publish, import, marker, watcher).
#[derive(Debug, Clone)]
pub struct PublishRequest {
    pub space_id: String,
    pub work_item_id: Option<String>,
    pub chat_id: Option<String>,
    /// Absolute source file (already authorized by the caller).
    pub source: PathBuf,
    pub title: String,
    pub kind: Option<ArtifactKind>,
    pub role: Option<String>,
    pub note: String,
    pub pin_as_plan: bool,
    /// Idempotency: a revision recorded with this operation is returned
    /// instead of writing again.
    pub operation_id: Option<String>,
    pub by: WorkOutlineActor,
}

/// The authenticated agent behind an artifact tool call.
#[derive(Debug, Clone)]
pub struct AgentCaller {
    pub chat_id: String,
    pub run_id: String,
    pub space_id: String,
    pub device_id: String,
    /// The Chat's work item, if any (the scope; else the Chat).
    pub work_item_id: Option<String>,
}

impl AgentCaller {
    fn actor(&self) -> WorkOutlineActor {
        WorkOutlineActor {
            kind: WorkOutlineActorKind::Agent,
            device_id: self.device_id.clone(),
            chat_id: Some(self.chat_id.clone()),
            message_id: None,
        }
    }

    /// Whether `artifact` is in this caller's scope: its work item, or (with
    /// no item) the Chat's own artifacts.
    fn owns(&self, artifact: &ArtifactView) -> bool {
        if artifact.space_id != self.space_id {
            return false;
        }
        match (&self.work_item_id, &artifact.work_item_id) {
            (Some(mine), Some(theirs)) => mine == theirs,
            (_, None) => artifact.chat_id.as_deref() == Some(self.chat_id.as_str()),
            (None, Some(_)) => false,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishArgs {
    path: String,
    title: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    pin_as_plan: bool,
    #[serde(default)]
    addressed_comments: Vec<String>,
    #[serde(default)]
    idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenderCheckArgs {
    #[serde(default)]
    artifact_id: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    viewport: Option<ArtifactViewport>,
    #[serde(default)]
    viewports: Option<Vec<ArtifactRenderViewport>>,
    #[serde(default)]
    theme: Option<ArtifactRenderTheme>,
    #[serde(default)]
    themes: Option<Vec<ArtifactRenderTheme>>,
    #[serde(default)]
    kind: Option<String>,
}

impl RenderCheckArgs {
    /// `viewports` (when non-empty), else the single `viewport` (replacing
    /// the default list), else the narrow + wide defaults. Clamped.
    fn effective_viewports(&self) -> Vec<ArtifactRenderViewport> {
        let list = match (&self.viewports, self.viewport) {
            (Some(list), _) if !list.is_empty() => list.clone(),
            (_, Some(single)) => vec![ArtifactRenderViewport {
                width: single.width,
                height: single.height,
                label: None,
            }],
            _ => ArtifactRenderViewport::defaults(),
        };
        list.into_iter()
            .map(ArtifactRenderViewport::clamped)
            .collect()
    }

    /// `themes` (when non-empty), else the single `theme`, else the current
    /// theme only.
    fn effective_themes(&self) -> Vec<ArtifactRenderTheme> {
        match (&self.themes, self.theme) {
            (Some(list), _) if !list.is_empty() => list.clone(),
            (_, Some(single)) => vec![single],
            _ => vec![ArtifactRenderTheme::System],
        }
    }
}

fn parse_args<T: serde::de::DeserializeOwned>(arguments: &Value) -> Outcome<T> {
    let arguments = if arguments.is_null() {
        json!({})
    } else {
        arguments.clone()
    };
    serde_json::from_value(arguments)
        .map_err(|e| ArtifactError::invalid(format!("invalid arguments: {e}")))
}

// ── render broker ───────────────────────────────────────────────────────────

type RenderReply = Result<ArtifactRenderCheckResult, String>;

/// Engine → UI render-check routing. The headed UI holds
/// `WatchArtifactRenderRequests` open; requests go to the newest live
/// subscriber; replies arrive through `ArtifactRenderResult`.
pub struct RenderBroker {
    subscribers: Mutex<
        Vec<(
            u64,
            tokio::sync::mpsc::UnboundedSender<ArtifactRenderRequest>,
        )>,
    >,
    pending: Mutex<HashMap<String, std::sync::mpsc::Sender<RenderReply>>>,
    next: AtomicU64,
    rate: Mutex<HashMap<String, VecDeque<Instant>>>,
    timeout: Mutex<Duration>,
}

impl Default for RenderBroker {
    fn default() -> Self {
        Self {
            subscribers: Mutex::new(Vec::new()),
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            rate: Mutex::new(HashMap::new()),
            timeout: Mutex::new(Duration::from_secs(artifact_limits::RENDER_TIMEOUT_SECS)),
        }
    }
}

struct SubscriberGuard {
    broker: Arc<RenderBroker>,
    id: u64,
}

impl Drop for SubscriberGuard {
    fn drop(&mut self) {
        lock(&self.broker.subscribers).retain(|(id, _)| *id != self.id);
    }
}

impl RenderBroker {
    /// Override the reply timeout (tests).
    pub fn set_timeout(&self, timeout: Duration) {
        *lock(&self.timeout) = timeout;
    }

    /// Live render-capable subscribers.
    pub fn subscriber_count(&self) -> usize {
        let mut subscribers = lock(&self.subscribers);
        subscribers.retain(|(_, tx)| !tx.is_closed());
        subscribers.len()
    }

    /// `WatchArtifactRenderRequests`: register the caller until the stream
    /// is dropped.
    pub fn subscribe(self: &Arc<Self>) -> BoxStream<'static, Value> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        lock(&self.subscribers).push((id, tx));
        let guard = SubscriberGuard {
            broker: self.clone(),
            id,
        };
        futures::stream::unfold((rx, guard), |(mut rx, guard)| async move {
            let request = rx.recv().await?;
            let value = serde_json::to_value(&request).ok()?;
            Some((value, (rx, guard)))
        })
        .boxed()
    }

    /// `ArtifactRenderResult`: deliver one reply. `false` for an unknown or
    /// expired request.
    pub fn respond(&self, params: ArtifactRenderResultParams) -> bool {
        let Some(tx) = lock(&self.pending).remove(&params.request_id) else {
            return false;
        };
        let reply = match (params.result, params.error) {
            (_, Some(error)) => Err(error),
            (Some(result), None) => Ok(result),
            (None, None) => Err("the renderer returned nothing".into()),
        };
        tx.send(reply).is_ok()
    }

    /// Per-Run rate limit (`RENDER_CHECKS_PER_MINUTE`).
    pub fn check_rate(&self, run_id: &str) -> Outcome<()> {
        let mut rate = lock(&self.rate);
        let window = rate.entry(run_id.to_owned()).or_default();
        let now = Instant::now();
        while window
            .front()
            .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(60))
        {
            window.pop_front();
        }
        if window.len() >= artifact_limits::RENDER_CHECKS_PER_MINUTE {
            return Err(ArtifactError::new(
                ArtifactErrorCode::RateLimited,
                format!(
                    "at most {} render checks per minute per run; wait and try again",
                    artifact_limits::RENDER_CHECKS_PER_MINUTE
                ),
            ));
        }
        window.push_back(now);
        Ok(())
    }

    /// Send one request to the newest live UI and wait (blocking) for its
    /// answer.
    pub fn request(
        &self,
        mut request: ArtifactRenderRequest,
    ) -> Outcome<ArtifactRenderCheckResult> {
        let request_id = format!(
            "render-{}-{}",
            self.next.fetch_add(1, Ordering::Relaxed),
            uuid::Uuid::new_v4().simple()
        );
        request.request_id = request_id.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        lock(&self.pending).insert(request_id.clone(), tx);
        let sent = {
            let mut subscribers = lock(&self.subscribers);
            subscribers.retain(|(_, tx)| !tx.is_closed());
            subscribers
                .iter()
                .rev()
                .any(|(_, tx)| tx.send(request.clone()).is_ok())
        };
        if !sent {
            lock(&self.pending).remove(&request_id);
            return Err(ArtifactError::new(
                ArtifactErrorCode::RenderUnavailable,
                "no Mast window is open on this device to render the artifact; say so instead \
                 of claiming it was checked",
            ));
        }
        let timeout = *lock(&self.timeout);
        match rx.recv_timeout(timeout) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(ArtifactError::new(
                ArtifactErrorCode::RenderFailed,
                format!("the renderer failed: {error}"),
            )),
            Err(_) => {
                lock(&self.pending).remove(&request_id);
                Err(ArtifactError::new(
                    ArtifactErrorCode::RenderTimeout,
                    format!("no render result within {}s", timeout.as_secs()),
                ))
            }
        }
    }
}

// ── service ─────────────────────────────────────────────────────────────────

/// Last seen `(mtime, size, hash)` of a stored file (watcher cache).
type SeenFile = (Option<SystemTime>, u64, String);

struct Inner {
    workspace: WorkspaceHost,
    /// `<profile store>/artifacts`.
    snapshots: PathBuf,
    /// Questions of each artifact's current revision, by artifact id and
    /// revision hash.
    questions: Mutex<HashMap<String, (String, Vec<mast_proto::PlanQuestion>)>>,
    /// Serializes every artifact write and the watcher; holds the watcher
    /// cache keyed by stored path.
    gate: Mutex<HashMap<String, SeenFile>>,
    render: Arc<RenderBroker>,
    /// The user's home directory (deny-list anchor); `$HOME` by default.
    home: Mutex<Option<PathBuf>>,
}

/// The engine's planning artifact service (one per engine; cheap to clone).
#[derive(Clone)]
pub struct ArtifactsService {
    inner: Arc<Inner>,
}

impl ArtifactsService {
    pub fn new(workspace: WorkspaceHost) -> Self {
        let snapshots = workspace.store_root().join("artifacts");
        Self {
            inner: Arc::new(Inner {
                workspace,
                snapshots,
                questions: Mutex::new(HashMap::new()),
                gate: Mutex::new(HashMap::new()),
                render: Arc::new(RenderBroker::default()),
                home: Mutex::new(std::env::var_os("HOME").map(PathBuf::from)),
            }),
        }
    }

    /// Override the home directory the source deny-list is anchored to.
    pub fn set_home(&self, home: Option<PathBuf>) {
        *lock(&self.inner.home) = home;
    }

    /// Authorize `path` as an artifact source (see [`check_source`]).
    fn authorize_source(&self, path: &Path) -> Outcome<PathBuf> {
        let home = lock(&self.inner.home).clone();
        check_source(path, home.as_deref())
    }

    pub fn render(&self) -> &Arc<RenderBroker> {
        &self.inner.render
    }

    pub(crate) fn workspace(&self) -> &WorkspaceHost {
        &self.inner.workspace
    }

    pub(crate) fn device_id(&self) -> &str {
        self.workspace().device_id()
    }

    fn local_space(&self, space_id: &str) -> Outcome<Space> {
        let space = self
            .workspace()
            .space(space_id)
            .ok()
            .flatten()
            .ok_or_else(|| ArtifactError::not_found(format!("Space {space_id} not found")))?;
        if space.device_id != self.device_id() {
            return Err(ArtifactError::new(
                ArtifactErrorCode::WrongDevice,
                format!(
                    "Space {space_id} lives on device {}; call it there",
                    space.device_id
                ),
            ));
        }
        Ok(space)
    }

    fn local_artifact(&self, id: &str) -> Outcome<ArtifactView> {
        let artifact = self
            .workspace()
            .artifact(id)
            .ok_or_else(|| ArtifactError::not_found(format!("artifact {id} not found")))?;
        if artifact.device_id != self.device_id() {
            return Err(ArtifactError::new(
                ArtifactErrorCode::WrongDevice,
                format!(
                    "artifact {id} lives on device {}; call it there",
                    artifact.device_id
                ),
            ));
        }
        Ok(artifact)
    }

    fn snapshot_path(&self, artifact_id: &str, hash: &str) -> PathBuf {
        self.inner.snapshots.join(artifact_id).join(hash)
    }

    fn store_snapshot(&self, artifact_id: &str, hash: &str, bytes: &[u8]) -> Outcome<()> {
        let path = self.snapshot_path(artifact_id, hash);
        if path.is_file() {
            return Ok(());
        }
        write_atomic(&path, bytes)
    }

    fn companion_snapshot_path(&self, artifact_id: &str, hash: &str) -> PathBuf {
        self.inner
            .snapshots
            .join(artifact_id)
            .join(format!("companion-{hash}"))
    }

    fn companion_index_path(&self, artifact_id: &str, revision: u32) -> PathBuf {
        self.inner
            .snapshots
            .join(artifact_id)
            .join(format!("revision-{revision}.companions.json"))
    }

    fn store_companion_snapshot(&self, artifact_id: &str, hash: &str, bytes: &[u8]) -> Outcome<()> {
        let path = self.companion_snapshot_path(artifact_id, hash);
        if path.is_file() {
            return Ok(());
        }
        write_atomic(&path, bytes)
    }

    /// Relative path → content hash of one revision's option files; `None`
    /// for revisions recorded before they were snapshotted.
    fn read_companion_index(
        &self,
        artifact_id: &str,
        revision: u32,
    ) -> Option<std::collections::BTreeMap<String, String>> {
        let bytes = std::fs::read(self.companion_index_path(artifact_id, revision)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn write_companion_index(
        &self,
        artifact_id: &str,
        revision: u32,
        index: &std::collections::BTreeMap<String, String>,
    ) -> Outcome<()> {
        let bytes = serde_json::to_vec(index).map_err(|e| ArtifactError::io(e.to_string()))?;
        write_atomic(&self.companion_index_path(artifact_id, revision), &bytes)
    }

    // ── location ────────────────────────────────────────────────────────────

    /// The canonical checkout for a scope: the main checkout of the work
    /// item's resolved primary repository (the only binding when none is
    /// primary), else of the Space folder's repository, else the Space
    /// folder itself.
    pub fn canonical_root(&self, space: &Space, work_item_id: Option<&str>) -> PathBuf {
        let space_path = PathBuf::from(&space.path);
        let start = work_item_id
            .and_then(|item| {
                let resolved = self
                    .workspace()
                    .resolve_execution(&ExecutionScope::WorkItem {
                        work_item_id: item.to_owned(),
                    })
                    .ok()?;
                if resolved.invalid_reason.is_some() {
                    return None;
                }
                let configuration = &resolved.configuration;
                let binding =
                    configuration
                        .primary()
                        .or(match configuration.bindings.as_slice() {
                            [only] => Some(only),
                            _ => None,
                        })?;
                Some(PathBuf::from(&binding.repo_path))
            })
            .unwrap_or(space_path);
        main_checkout(&start).unwrap_or_else(|| canonical(&start))
    }

    /// The directory a new artifact of `folder` is written to: `.mast/` by
    /// default, but the legacy `.zeron/artifacts/<folder>` when that folder
    /// already exists (on disk or through a registered sibling) so one work
    /// item's artifacts never split across two roots.
    fn artifact_dir(&self, space_id: &str, canonical: &Path, folder: &str) -> PathBuf {
        let new = canonical.join(ARTIFACTS_DIR).join(folder);
        if new.exists() {
            return new;
        }
        let legacy = canonical.join(LEGACY_ARTIFACTS_DIR).join(folder);
        let legacy_root = canonical.join(LEGACY_ARTIFACTS_DIR);
        let sibling_in_legacy = self
            .workspace()
            .artifacts_of_space(space_id)
            .into_iter()
            .any(|a| a.folder == folder && Path::new(&a.stored_path).starts_with(&legacy_root));
        if legacy.exists() || sibling_in_legacy {
            legacy
        } else {
            new
        }
    }

    /// The folder (under `.mast/artifacts/`) of a scope: reuses the folder
    /// earlier artifacts of the same work item recorded, so renames never
    /// move files.
    fn folder_for(
        &self,
        space: &Space,
        work_item_id: Option<&str>,
        chat_id: Option<&str>,
    ) -> Outcome<String> {
        if let Some(item) = work_item_id {
            if let Some(existing) = self
                .workspace()
                .artifacts_of_space(&space.id)
                .into_iter()
                .find(|a| a.work_item_id.as_deref() == Some(item) && !a.folder.is_empty())
            {
                return Ok(existing.folder);
            }
            let inputs = self.workspace().work_outline_inputs(&space.id);
            let snapshot = mast_doc::materialize_work_outline(&space.id, &inputs);
            let title = snapshot
                .item(item)
                .map(|i| i.title.clone())
                .ok_or_else(|| {
                    ArtifactError::not_found(format!("work item {item} not found in this Space"))
                })?;
            return Ok(work_item_folder(&title, item));
        }
        Ok(match chat_id {
            Some(chat) => chat_folder(chat),
            None => SPACE_FOLDER.to_owned(),
        })
    }

    // ── recording ───────────────────────────────────────────────────────────

    /// Publish or import one file: copy it into the scope's folder, snapshot
    /// a revision when the content changed, write the row, and pin it when
    /// asked. Idempotent per `operation_id`.
    pub fn record(&self, request: PublishRequest) -> Outcome<ArtifactPublishReply> {
        let space = self.local_space(&request.space_id)?;
        let title = clean_title(&request.title);
        if title.is_empty() {
            return Err(ArtifactError::invalid("title is empty"));
        }
        let role = request
            .role
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(|r| {
                r.chars()
                    .take(artifact_limits::MAX_ROLE_CHARS)
                    .collect::<String>()
            });
        if !request.source.is_absolute() {
            return Err(ArtifactError::invalid(format!(
                "{} is not an absolute path",
                request.source.display()
            )));
        }
        // A link to either half of a fragment/preview pair opens the
        // fragment: publish the fragment when it exists.
        let mut source = request.source.clone();
        let mut kind = request.kind;
        let source_text = source.to_string_lossy().into_owned();
        if let Some(mast_proto::visualize::HtmlArtifactKind::Preview) =
            mast_proto::visualize::html_artifact_kind(&source_text)
        {
            match mast_proto::visualize::fragment_sibling(&source_text) {
                Some(fragment) if Path::new(&fragment).is_file() => {
                    source = PathBuf::from(fragment);
                    kind = Some(ArtifactKind::HtmlFragment);
                }
                _ => {
                    kind = kind.or(Some(ArtifactKind::HtmlPage));
                }
            }
        }
        let kind = kind
            .filter(|k| *k != ArtifactKind::Unknown)
            .unwrap_or_else(|| ArtifactKind::from_path(&source.to_string_lossy()));
        let bytes = read_file(&source)?;
        if !kind.is_binary() && std::str::from_utf8(&bytes).is_err() {
            return Err(ArtifactError::invalid(format!(
                "{} is not UTF-8 text (kind {})",
                source.display(),
                kind.as_str()
            )));
        }
        let manifest = if kind == ArtifactKind::Options {
            Some(OptionsManifest::parse(&String::from_utf8_lossy(&bytes))?)
        } else {
            None
        };
        let work_item_id = request.work_item_id.clone();
        let scope_chat = if work_item_id.is_some() {
            None
        } else {
            request.chat_id.as_deref()
        };
        let id = artifact_id(
            &space.id,
            work_item_id.as_deref(),
            scope_chat,
            &title,
            role.as_deref(),
        );
        let hash = content_hash(&bytes);

        let mut seen = lock(&self.inner.gate);
        let existing = self.workspace().artifact(&id);
        if let (Some(existing), Some(op)) = (&existing, &request.operation_id)
            && let Some(revision) = existing
                .revisions
                .iter()
                .find(|r| r.op.as_deref() == Some(op.as_str()))
        {
            return Ok(ArtifactPublishReply {
                artifact_id: id.clone(),
                revision: revision.id,
                url: existing.url(),
                stored_path: existing.stored_path.clone(),
                changed: true,
            });
        }
        if let Some(existing) = &existing
            && existing.device_id != self.device_id()
        {
            return Err(ArtifactError::new(
                ArtifactErrorCode::WrongDevice,
                format!("artifact {id} lives on device {}", existing.device_id),
            ));
        }
        let now = now_ms();
        let mut view = match existing.clone() {
            Some(view) => view,
            None => {
                let canonical = self.canonical_root(&space, work_item_id.as_deref());
                ensure_excluded(&canonical)?;
                let folder =
                    self.folder_for(&space, work_item_id.as_deref(), request.chat_id.as_deref())?;
                let dir = self.artifact_dir(&space.id, &canonical, &folder);
                let mut name = file_name_of(&source)?;
                let taken = self
                    .workspace()
                    .artifacts_of_space(&space.id)
                    .into_iter()
                    .any(|a| {
                        a.folder == folder
                            && Path::new(&a.stored_path)
                                .file_name()
                                .and_then(|n| n.to_str())
                                == Some(name.as_str())
                    });
                if taken {
                    name = tagged_file_name(&name, &id[..6]);
                }
                ArtifactView {
                    id: id.clone(),
                    space_id: space.id.clone(),
                    device_id: self.device_id().to_owned(),
                    work_item_id: work_item_id.clone(),
                    chat_id: request.chat_id.clone(),
                    title: title.clone(),
                    kind,
                    role: role.clone(),
                    folder,
                    stored_path: dir.join(name).to_string_lossy().into_owned(),
                    origin_path: Some(request.source.to_string_lossy().into_owned()),
                    created_at: now,
                    updated_at: now,
                    ..ArtifactView::default()
                }
            }
        };
        let stored = PathBuf::from(&view.stored_path);
        let dir = stored
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| ArtifactError::invalid("stored path has no folder"))?;

        // Content.
        if source != stored {
            let current = std::fs::read(&stored).ok();
            if current.as_deref() != Some(bytes.as_slice()) {
                write_atomic(&stored, &bytes)?;
            }
        }
        // A fragment's standalone preview travels with it.
        if kind == ArtifactKind::HtmlFragment
            && let Some(preview) =
                mast_proto::visualize::preview_sibling(&source.to_string_lossy())
            && Path::new(&preview).is_file()
        {
            let stored_preview =
                mast_proto::visualize::preview_sibling(&view.stored_path).map(PathBuf::from);
            if let Some(stored_preview) = stored_preview {
                if Path::new(&preview) != stored_preview {
                    let preview_bytes = read_file(&self.authorize_source(Path::new(&preview))?)?;
                    write_atomic(&stored_preview, &preview_bytes)?;
                }
                view.preview_path = Some(stored_preview.to_string_lossy().into_owned());
            }
        }
        // An options manifest's fragments are copied next to it.
        let mut companion_hashes = std::collections::BTreeMap::<String, String>::new();
        let mut companion_bytes: Vec<(String, Vec<u8>)> = Vec::new();
        if let Some(manifest) = &manifest {
            let source_dir = source.parent().unwrap_or(Path::new("/"));
            let mut companions = Vec::new();
            for option in &manifest.options {
                if !is_safe_relative_path(&option.path) {
                    continue;
                }
                let from = source_dir.join(&option.path);
                let to = dir.join(&option.path);
                let companion = read_file(&self.authorize_source(&from)?)?;
                if from != to {
                    write_atomic(&to, &companion)?;
                }
                companion_hashes.insert(option.path.clone(), content_hash(&companion));
                companion_bytes.push((content_hash(&companion), companion));
                companions.push(option.path.clone());
            }
            view.companions = companions;
        }

        // An options revision is its manifest plus its fragments: a fragment
        // edit under an unchanged manifest is a new revision too. Revisions
        // recorded before fragments were snapshotted have no index; they are
        // backfilled, never treated as changed.
        let current_index = view
            .current()
            .and_then(|r| self.read_companion_index(&id, r.id));
        let companions_changed = manifest.is_some()
            && current_index
                .as_ref()
                .is_some_and(|index| *index != companion_hashes);
        let changed =
            view.current().map(|r| r.hash.as_str()) != Some(hash.as_str()) || companions_changed;
        if manifest.is_some()
            && !changed
            && current_index.is_none()
            && let Some(current) = view.current()
        {
            for (companion_hash, companion) in &companion_bytes {
                self.store_companion_snapshot(&id, companion_hash, companion)?;
            }
            self.write_companion_index(&id, current.id, &companion_hashes)?;
        }
        if changed {
            self.store_snapshot(&id, &hash, &bytes)?;
            let next = view
                .revisions
                .iter()
                .map(|r| r.id)
                .max()
                .unwrap_or(0)
                .max(view.current_revision)
                + 1;
            view.revisions.insert(
                0,
                ArtifactRevision {
                    id: next,
                    hash: hash.clone(),
                    note: clean_note(&request.note),
                    at: now,
                    by: request.by.clone(),
                    size_bytes: bytes.len() as u64,
                    op: request.operation_id.clone(),
                },
            );
            if manifest.is_some() {
                for (companion_hash, companion) in &companion_bytes {
                    self.store_companion_snapshot(&id, companion_hash, companion)?;
                }
                self.write_companion_index(&id, next, &companion_hashes)?;
            }
            view.current_revision = next;
        }
        view.kind = kind;
        view.title = title;
        view.archived_at = None;
        let before = existing.as_ref();
        if changed || before != Some(&view) {
            view.updated_at = now;
            view = self
                .workspace()
                .put_artifact(&view)
                .map_err(|e| ArtifactError::io(e.to_string()))?;
        }
        if request.pin_as_plan && !view.pinned_as_plan {
            self.workspace()
                .set_artifact_pin(&id, true)
                .map_err(|e| ArtifactError::io(e.to_string()))?;
        }
        // Mast's own write is not an external edit.
        let meta = std::fs::metadata(&stored).ok();
        seen.insert(
            view.stored_path.clone(),
            (
                meta.as_ref().and_then(|m| m.modified().ok()),
                meta.map_or(0, |m| m.len()),
                hash,
            ),
        );
        Ok(ArtifactPublishReply {
            url: view.url(),
            artifact_id: id,
            revision: view.current_revision,
            stored_path: view.stored_path,
            changed,
        })
    }

    // ── watcher ─────────────────────────────────────────────────────────────

    /// One watcher pass over this device's stored files: a content change
    /// Mast did not make becomes a revision noted "edited without a note".
    /// Returns how many revisions it recorded.
    pub fn scan_external_edits(&self) -> usize {
        let mut seen = lock(&self.inner.gate);
        let mut recorded = 0;
        for mut view in self.workspace().all_artifacts() {
            if view.device_id != self.device_id() || view.archived_at.is_some() {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&view.stored_path) else {
                continue;
            };
            let stamp = (meta.modified().ok(), meta.len());
            if let Some((mtime, size, _)) = seen.get(&view.stored_path)
                && (*mtime, *size) == stamp
            {
                continue;
            }
            let Ok(bytes) = read_file(Path::new(&view.stored_path)) else {
                continue;
            };
            let hash = content_hash(&bytes);
            seen.insert(view.stored_path.clone(), (stamp.0, stamp.1, hash.clone()));
            if view.current().map(|r| r.hash.as_str()) == Some(hash.as_str()) {
                continue;
            }
            if self.store_snapshot(&view.id, &hash, &bytes).is_err() {
                continue;
            }
            let now = now_ms();
            let next = view
                .current_revision
                .max(view.revisions.iter().map(|r| r.id).max().unwrap_or(0))
                + 1;
            view.revisions.insert(
                0,
                ArtifactRevision {
                    id: next,
                    hash,
                    note: EDITED_WITHOUT_NOTE.to_owned(),
                    at: now,
                    by: WorkOutlineActor {
                        kind: WorkOutlineActorKind::System,
                        device_id: self.device_id().to_owned(),
                        chat_id: None,
                        message_id: None,
                    },
                    size_bytes: bytes.len() as u64,
                    op: None,
                },
            );
            view.current_revision = next;
            view.updated_at = now;
            match self.workspace().put_artifact(&view) {
                Ok(_) => recorded += 1,
                Err(error) => {
                    tracing::warn!(artifact = %view.id, %error, "artifact watcher write failed")
                }
            }
        }
        recorded
    }

    /// Poll stored files every [`WATCH_INTERVAL`] for the process lifetime.
    pub fn spawn_watch_loop(&self) {
        let service = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(WATCH_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let service = service.clone();
                let _ = tokio::task::spawn_blocking(move || service.scan_external_edits()).await;
            }
        });
    }

    // ── reads ───────────────────────────────────────────────────────────────

    /// `ArtifactContent` on the owning device.
    pub fn content(&self, params: &ArtifactContentParams) -> Outcome<ArtifactContentReply> {
        let artifact = self.local_artifact(&params.artifact_id)?;
        let stored = PathBuf::from(&artifact.stored_path);
        if let Some(companion) = params.companion.as_deref() {
            let dir = stored.parent().unwrap_or(Path::new("/"));
            let preview_name = artifact
                .preview_path
                .as_deref()
                .and_then(|p| Path::new(p).file_name())
                .and_then(|n| n.to_str());
            let allowed = is_safe_relative_path(companion)
                && (artifact.companions.iter().any(|c| c == companion)
                    || preview_name == Some(companion));
            if !allowed {
                return Err(ArtifactError::not_found(format!(
                    "{companion:?} is not a companion of this artifact"
                )));
            }
            let path = dir.join(companion);
            let kind = ArtifactKind::from_path(companion);
            if let Some(number) = params.revision
                && artifact.revision(number).is_none()
            {
                return Err(ArtifactError::not_found(format!(
                    "revision {number} of artifact {} is not kept",
                    artifact.id
                )));
            }
            if let Some(number) = params.revision
                && number != artifact.current_revision
                && preview_name != Some(companion)
            {
                // A past revision's option file comes from its snapshot index.
                let hash = self
                    .read_companion_index(&artifact.id, number)
                    .and_then(|index| index.get(companion).cloned())
                    .ok_or_else(|| {
                        ArtifactError::not_found(format!(
                            "option file {companion:?} was not saved with revision {number} \
                             (it predates per-revision option files)"
                        ))
                    })?;
                let bytes = std::fs::read(self.companion_snapshot_path(&artifact.id, &hash))
                    .map_err(|_| {
                        ArtifactError::not_found(format!(
                            "the snapshot of {companion:?} at revision {number} is missing on this device"
                        ))
                    })?;
                return Ok(encode_reply(&artifact, kind, number, &path, bytes));
            }
            let bytes = read_file(&path)?;
            let revision = params.revision.unwrap_or(0);
            return Ok(encode_reply(&artifact, kind, revision, &path, bytes));
        }
        let (revision, bytes) = match params.revision {
            Some(number) => {
                let revision = artifact.revision(number).ok_or_else(|| {
                    ArtifactError::not_found(format!(
                        "revision {number} of artifact {} is not kept",
                        artifact.id
                    ))
                })?;
                let bytes = std::fs::read(self.snapshot_path(&artifact.id, &revision.hash))
                    .or_else(|_| {
                        std::fs::read(&stored).and_then(|bytes| {
                            if content_hash(&bytes) == revision.hash {
                                Ok(bytes)
                            } else {
                                Err(std::io::Error::other("snapshot missing"))
                            }
                        })
                    })
                    .map_err(|_| {
                        ArtifactError::not_found(format!(
                            "the snapshot of revision {number} is missing on this device"
                        ))
                    })?;
                (number, bytes)
            }
            None => match read_file(&stored) {
                Ok(bytes) => {
                    let hash = content_hash(&bytes);
                    let revision = match artifact.current() {
                        Some(current) if current.hash == hash => current.id,
                        _ => 0,
                    };
                    (revision, bytes)
                }
                Err(error) => {
                    let current = artifact.current().ok_or(error)?;
                    let bytes = std::fs::read(self.snapshot_path(&artifact.id, &current.hash))
                        .map_err(|e| io_error("read snapshot of", &stored, e))?;
                    (current.id, bytes)
                }
            },
        };
        Ok(encode_reply(
            &artifact,
            artifact.kind,
            revision,
            &stored,
            bytes,
        ))
    }

    /// The current text of an artifact (stored file, else the current
    /// snapshot).
    fn current_bytes(&self, artifact: &ArtifactView) -> Outcome<Vec<u8>> {
        read_file(Path::new(&artifact.stored_path)).or_else(|error| {
            let current = artifact.current().ok_or(error)?;
            std::fs::read(self.snapshot_path(&artifact.id, &current.hash))
                .map_err(|e| io_error("read snapshot of", Path::new(&artifact.stored_path), e))
        })
    }

    /// The questions in the artifact's current revision, read from its
    /// stored snapshot (falling back to the file) and cached by revision
    /// hash. Empty for kinds without questions or unreadable content.
    pub(crate) fn plan_questions(&self, artifact: &ArtifactView) -> Vec<mast_proto::PlanQuestion> {
        let Some(current) = artifact.current() else {
            return Vec::new();
        };
        if let Some((hash, questions)) = lock(&self.inner.questions).get(&artifact.id)
            && *hash == current.hash
        {
            return questions.clone();
        }
        let bytes = std::fs::read(self.snapshot_path(&artifact.id, &current.hash))
            .ok()
            .or_else(|| self.current_bytes(artifact).ok())
            .unwrap_or_default();
        let questions = mast_proto::extract_questions(artifact.kind, &String::from_utf8_lossy(&bytes));
        lock(&self.inner.questions).insert(artifact.id.clone(), (current.hash.clone(), questions.clone()));
        questions
    }

    /// `ArtifactDrift`: commits made after the artifact's latest revision on
    /// the checkouts the work item's chats work in (their cwd and bound diff
    /// targets). Checkouts on the repository's main / master branch are
    /// skipped: their history is everyone's, not this work's.
    pub fn drift(&self, artifact_id: &str) -> Outcome<mast_proto::ArtifactDriftReply> {
        let artifact = self.local_artifact(artifact_id)?;
        let since = artifact.revisions.first().map_or(artifact.updated_at, |r| r.at);
        let mut reply = mast_proto::ArtifactDriftReply {
            artifact_id: artifact.id.clone(),
            since,
            ..Default::default()
        };
        let Some(item) = artifact.work_item_id.clone() else {
            return Ok(reply);
        };
        let outline = crate::work_outline::WorkOutlineService::new(self.workspace().clone());
        let mut paths: Vec<PathBuf> = Vec::new();
        for assignment in outline
            .snapshot(&artifact.space_id)
            .assignments
            .iter()
            .filter(|a| a.work_item_id.as_deref() == Some(item.as_str()))
        {
            if let Ok(Some(chat)) = self.workspace().chat(&assignment.chat_id)
                && let Some(cwd) = chat.cwd
            {
                paths.push(PathBuf::from(cwd));
            }
            if let Ok(Some(row)) = self.workspace().chat_diff_targets(&assignment.chat_id) {
                paths.extend(row.targets.iter().map(|t| PathBuf::from(&t.path)));
            }
        }
        let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        let after = (since / 1000).to_string();
        for path in paths {
            let Some(top) = git(&path, &["rev-parse", "--show-toplevel"]).map(PathBuf::from) else {
                continue;
            };
            if !seen.insert(canonical(&top)) {
                continue;
            }
            let branch = git(&top, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
            if matches!(branch.as_str(), "" | "main" | "master" | "HEAD") {
                continue;
            }
            let count: u32 = git(&top, &["rev-list", "--count", &format!("--after={after}"), "HEAD"])
                .and_then(|c| c.parse().ok())
                .unwrap_or(0);
            if count > reply.commits_since {
                reply.commits_since = count;
                reply.branch = Some(branch);
                reply.latest_commit_at = git(&top, &["log", "-1", "--format=%ct"])
                    .and_then(|t| t.parse::<i64>().ok())
                    .map(|t| t * 1000);
            }
        }
        Ok(reply)
    }

    /// `ArtifactAnswerQuestion`: record (or replace) the answer to question
    /// `question_id`. Only the `answers` field is written, from any device;
    /// the owning device's reconcile pass resolves the Required Action.
    pub fn answer_question(
        &self,
        params: &mast_proto::ArtifactAnswerQuestionParams,
        by: WorkOutlineActor,
    ) -> Outcome<mast_proto::ArtifactAnswerQuestionReply> {
        let artifact = self
            .workspace()
            .artifact(&params.artifact_id)
            .ok_or_else(|| ArtifactError::not_found(format!("artifact {} not found", params.artifact_id)))?;
        let id = params.question_id.trim().to_ascii_uppercase();
        if !id.starts_with("Q-") || id.len() < 3 {
            return Err(ArtifactError::invalid("question id must look like Q-3"));
        }
        let answer = clean_answer(&params.answer);
        if answer.is_empty() {
            return Err(ArtifactError::invalid("answer must not be empty"));
        }
        self.record_answer(&artifact, &id, answer, by)?;
        Ok(mast_proto::ArtifactAnswerQuestionReply {
            artifact_id: artifact.id,
            question_id: id,
        })
    }

    /// `ArtifactAddComment`: store a comment on an HTML or Mermaid artifact
    /// (the `comments` field only; the document is never edited).
    pub fn add_comment(
        &self,
        params: &ArtifactAddCommentParams,
        by: WorkOutlineActor,
    ) -> Outcome<ArtifactAddCommentReply> {
        let artifact = self
            .workspace()
            .artifact(&params.artifact_id)
            .ok_or_else(|| ArtifactError::not_found(format!("artifact {} not found", params.artifact_id)))?;
        if !kind_supports_comments(artifact.kind) {
            return Err(ArtifactError::invalid("comments are for HTML and Mermaid artifacts"));
        }
        let text = clip_chars(params.text.trim(), MAX_COMMENT_CHARS);
        if text.is_empty() {
            return Err(ArtifactError::invalid("comment text must not be empty"));
        }
        let anchor = params.anchor.cleaned();
        if anchor.is_empty() {
            return Err(ArtifactError::invalid(
                "comment anchor needs an id or quoted text",
            ));
        }
        let quote = match params.quote.split_whitespace().collect::<Vec<_>>().join(" ") {
            q if q.is_empty() => anchor.exact.clone(),
            q => q,
        };
        let quote = clip_chars(&quote, MAX_QUOTE_CHARS);
        let mut comments = artifact.comments.clone();
        let comment = ArtifactComment {
            id: next_comment_id(&comments),
            anchor,
            quote,
            text,
            status: CommentStatus::Open,
            revision: artifact.current_revision,
            at: now_ms(),
            by,
            resolved_at: None,
            resolved_by: None,
            addressed_in: None,
        };
        comments.push(comment.clone());
        while comments.len() > MAX_COMMENTS {
            let at = comments.iter().position(|c| !c.is_open()).unwrap_or(0);
            comments.remove(at);
        }
        self.store_comments(&artifact.id, &comments)?;
        Ok(ArtifactAddCommentReply {
            artifact_id: artifact.id,
            comment,
        })
    }

    /// `ArtifactResolveComment`: a person resolves or reopens a comment.
    pub fn resolve_comment(
        &self,
        params: &ArtifactResolveCommentParams,
        _by: WorkOutlineActor,
    ) -> Outcome<ArtifactResolveCommentReply> {
        let artifact = self
            .workspace()
            .artifact(&params.artifact_id)
            .ok_or_else(|| ArtifactError::not_found(format!("artifact {} not found", params.artifact_id)))?;
        let id = normalize_comment_id(&params.comment_id)
            .ok_or_else(|| ArtifactError::invalid("comment id must look like C-3"))?;
        let mut comments = artifact.comments.clone();
        let comment = comments
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| ArtifactError::not_found(format!("comment {id} not found")))?;
        if params.resolved {
            comment.status = CommentStatus::Resolved;
            comment.resolved_at = Some(now_ms());
            comment.resolved_by = Some("user".to_owned());
        } else {
            comment.status = CommentStatus::Open;
            comment.resolved_at = None;
            comment.resolved_by = None;
            comment.addressed_in = None;
        }
        self.store_comments(&artifact.id, &comments)?;
        Ok(ArtifactResolveCommentReply {
            artifact_id: artifact.id,
            comment_id: id,
            resolved: params.resolved,
        })
    }

    fn store_comments(&self, id: &str, comments: &[ArtifactComment]) -> Outcome<()> {
        self.workspace()
            .set_artifact_comments(id, comments)
            .map_err(|e| ArtifactError::io(e.to_string()))?
            .ok_or_else(|| ArtifactError::not_found(format!("artifact {id} not found")))?;
        Ok(())
    }

    /// Mark the open comments in `ids` resolved by the agent, addressed in
    /// `revision`. Unknown ids are ignored.
    fn mark_comments_addressed(&self, artifact_id: &str, ids: &[String], revision: u32) -> Outcome<()> {
        let Some(artifact) = self.workspace().artifact(artifact_id) else {
            return Ok(());
        };
        let wanted: Vec<String> = ids.iter().filter_map(|i| normalize_comment_id(i)).collect();
        let mut comments = artifact.comments.clone();
        let mut any = false;
        for c in comments.iter_mut().filter(|c| c.is_open() && wanted.contains(&c.id)) {
            c.status = CommentStatus::Resolved;
            c.resolved_at = Some(now_ms());
            c.resolved_by = Some("agent".to_owned());
            c.addressed_in = Some(revision);
            any = true;
        }
        if any {
            self.store_comments(artifact_id, &comments)?;
        }
        Ok(())
    }

    /// Open comments for `artifact_get_plan`, anchors checked against the
    /// current revision's source.
    fn plan_comments(&self, plan: &ArtifactView, source: Option<&str>) -> Vec<Value> {
        plan.comments
            .iter()
            .filter(|c| c.is_open())
            .map(|c| {
                let status = match source {
                    Some(src) if kind_supports_comments(plan.kind) => {
                        if resolve_anchor(plan.kind, src, &c.anchor).is_found() {
                            "found"
                        } else {
                            "outdated"
                        }
                    }
                    _ => "unknown",
                };
                json!({
                    "id": c.id,
                    "text": c.text,
                    "quote": c.quote,
                    "anchor": c.anchor,
                    "revision": c.revision,
                    "at": c.at,
                    "anchorStatus": status,
                })
            })
            .collect()
    }

    /// Store `answer` for `question_id` on the row (replacing an earlier one).
    pub(crate) fn record_answer(
        &self,
        artifact: &ArtifactView,
        question_id: &str,
        answer: String,
        by: WorkOutlineActor,
    ) -> Outcome<()> {
        let mut answers = artifact.answers.clone();
        answers.retain(|a| !a.id.eq_ignore_ascii_case(question_id));
        answers.push(mast_proto::ArtifactQuestionAnswer {
            id: question_id.to_owned(),
            answer,
            at: now_ms(),
            by,
            revision: artifact.current_revision,
        });
        let excess = answers.len().saturating_sub(mast_proto::artifacts::MAX_ANSWERS);
        answers.drain(..excess);
        self.workspace()
            .set_artifact_answers(&artifact.id, &answers)
            .map_err(|e| ArtifactError::io(e.to_string()))?
            .ok_or_else(|| ArtifactError::not_found(format!("artifact {} not found", artifact.id)))?;
        Ok(())
    }

    /// `ArtifactPickOption`: record, change or clear (no `optionId`) the
    /// pick on an `options` artifact. Only the `picked` field is written.
    /// A manifest `question` that is a `Q-n` id also records the pick as that plan question's
    /// answer (so its Required Action resolves) and a cleared pick removes
    /// it again; the caller (RPC) then resolves any `artopt:` request.
    pub fn pick_option(
        &self,
        params: &mast_proto::ArtifactPickOptionParams,
        by: WorkOutlineActor,
    ) -> Outcome<mast_proto::ArtifactPickOptionReply> {
        use mast_proto::artifacts::{ArtifactOptionPick, MAX_PICK_CHARS, MAX_PICK_ID_CHARS};
        let artifact = self
            .workspace()
            .artifact(&params.artifact_id)
            .ok_or_else(|| ArtifactError::not_found(format!("artifact {} not found", params.artifact_id)))?;
        if artifact.kind != ArtifactKind::Options {
            return Err(ArtifactError::invalid("only an options artifact can have a pick"));
        }
        let cut = |text: &str, max: usize| -> String { text.trim().chars().take(max).collect() };
        let option_id = params
            .option_id
            .as_deref()
            .map(|id| cut(id, MAX_PICK_ID_CHARS))
            .filter(|id| !id.is_empty());
        let pick = option_id.map(|option_id| ArtifactOptionPick {
            label: cut(&params.label, MAX_PICK_CHARS),
            note: params
                .note
                .as_deref()
                .map(|n| cut(n, MAX_PICK_CHARS))
                .filter(|n| !n.is_empty()),
            option_id,
            at: now_ms(),
            by,
            revision: artifact.current_revision,
        });
        self.workspace()
            .set_artifact_pick(&artifact.id, pick.as_ref())
            .map_err(|e| ArtifactError::io(e.to_string()))?
            .ok_or_else(|| ArtifactError::not_found(format!("artifact {} not found", artifact.id)))?;
        self.sync_pick_answer(&artifact, pick.as_ref());
        Ok(mast_proto::ArtifactPickOptionReply {
            artifact_id: artifact.id,
            picked: pick,
        })
    }

    /// Mirror the pick into the plan question the manifest names, if any.
    /// Best effort: the pick itself is already stored.
    fn sync_pick_answer(&self, artifact: &ArtifactView, pick: Option<&mast_proto::ArtifactOptionPick>) {
        let Some(question_id) = self
            .current_bytes(artifact)
            .ok()
            .and_then(|b| OptionsManifest::parse(&String::from_utf8_lossy(&b)).ok())
            .and_then(|m| m.linked_question_id())
        else {
            return;
        };
        let Some(item) = artifact.work_item_id.as_deref() else {
            return;
        };
        let Some(plan) = self
            .workspace()
            .artifacts_of_space(&artifact.space_id)
            .into_iter()
            .find(|a| a.pinned_as_plan && a.archived_at.is_none() && a.work_item_id.as_deref() == Some(item))
        else {
            return;
        };
        let result = match pick {
            Some(pick) => {
                let label = if pick.label.is_empty() { String::new() } else { format!(": {}", pick.label) };
                let text = clean_answer(&format!("Picked option {}{label}", pick.option_id));
                self.record_answer(&plan, &question_id, text, pick.by.clone())
            }
            None => {
                let mut answers = plan.answers.clone();
                answers.retain(|a| !a.id.eq_ignore_ascii_case(&question_id));
                self.workspace()
                    .set_artifact_answers(&plan.id, &answers)
                    .map(|_| ())
                    .map_err(|e| ArtifactError::io(e.to_string()))
            }
        };
        if let Err(error) = result {
            tracing::warn!(artifact = %artifact.id, "option pick: question answer not synced: {error:?}");
        }
    }

    /// `WatchArtifacts`: the Space's artifacts now, then one frame per
    /// change (coalesced; skipped when no artifact row changed).
    pub fn watch(&self, space_id: String) -> BoxStream<'static, Value> {
        struct State {
            workspace: WorkspaceHost,
            changes: tokio::sync::watch::Receiver<u64>,
            space_id: String,
            last: Option<Vec<ArtifactView>>,
            marker: Option<u64>,
            revision: u64,
        }
        let state = State {
            changes: self.workspace().watch_changes(),
            workspace: self.workspace().clone(),
            space_id,
            last: None,
            marker: None,
            revision: 0,
        };
        futures::stream::unfold(state, |mut state| async move {
            loop {
                if state.last.is_some() {
                    state.changes.changed().await.ok()?;
                    tokio::time::sleep(Duration::from_millis(75)).await;
                    state.changes.borrow_and_update();
                    if state.marker == Some(state.workspace.artifacts_generation()) {
                        continue;
                    }
                }
                let marker = state.workspace.artifacts_generation();
                let artifacts = state.workspace.artifacts_of_space(&state.space_id);
                state.marker = Some(marker);
                if state.last.as_ref() == Some(&artifacts) {
                    continue;
                }
                state.revision += 1;
                let snapshot = ArtifactsSnapshot {
                    space_id: state.space_id.clone(),
                    revision: state.revision,
                    artifacts: artifacts.clone(),
                };
                let value = serde_json::to_value(&snapshot).ok()?;
                state.last = Some(artifacts);
                return Some((value, state));
            }
        })
        .boxed()
    }

    // ── user-side RPCs ──────────────────────────────────────────────────────

    /// `ArtifactRegister`: import an existing local file (any regular file
    /// outside the source deny-list) as revision 1, keeping its origin.
    pub fn register(&self, params: &ArtifactRegisterParams) -> Outcome<ArtifactPublishReply> {
        let space = self.local_space(&params.space_id)?;
        let work_item_id = params
            .work_item_id
            .clone()
            .filter(|id| !id.trim().is_empty());
        if let Some(item) = &work_item_id
            && self.workspace().work_item_space(item).as_deref() != Some(space.id.as_str())
        {
            return Err(ArtifactError::not_found(format!(
                "work item {item} is not in this Space"
            )));
        }
        let chat_id = params.chat_id.clone().filter(|id| !id.trim().is_empty());
        if let Some(chat) = &chat_id {
            let in_space = self
                .workspace()
                .chat(chat)
                .ok()
                .flatten()
                .is_some_and(|c| c.space_id.as_deref() == Some(space.id.as_str()));
            if !in_space {
                return Err(ArtifactError::not_found(format!(
                    "chat {chat} is not in this Space"
                )));
            }
        }
        let raw = params.path.trim();
        if raw.is_empty() {
            return Err(ArtifactError::invalid("path is empty"));
        }
        let path = if Path::new(raw).is_absolute() {
            PathBuf::from(raw)
        } else {
            Path::new(&space.path).join(raw)
        };
        let path = self.authorize_source(&path)?;
        let title = params
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| default_title_for(&path));
        self.record(PublishRequest {
            space_id: space.id.clone(),
            work_item_id,
            chat_id,
            source: path,
            title,
            kind: params.kind,
            role: params.role.clone(),
            note: IMPORTED_NOTE.to_owned(),
            pin_as_plan: false,
            operation_id: None,
            by: WorkOutlineActor {
                kind: WorkOutlineActorKind::User,
                device_id: self.device_id().to_owned(),
                chat_id: None,
                message_id: None,
            },
        })
    }

    /// `ArtifactPromote`: copy the current revision to
    /// `<canonical>/docs/plans/` (uncommitted, never overwriting a different
    /// file) and record the path.
    pub fn promote(&self, artifact_id: &str) -> Outcome<ArtifactPromoteReply> {
        let _gate = lock(&self.inner.gate);
        let mut artifact = self.local_artifact(artifact_id)?;
        let stored = PathBuf::from(&artifact.stored_path);
        let canonical = canonical_of_stored(&stored).ok_or_else(|| {
            ArtifactError::invalid("the artifact is not stored under a canonical checkout")
        })?;
        let bytes = match artifact.current() {
            Some(current) => std::fs::read(self.snapshot_path(&artifact.id, &current.hash))
                .or_else(|_| std::fs::read(&stored))
                .map_err(|e| io_error("read", &stored, e))?,
            None => read_file(&stored)?,
        };
        let name = file_name_of(&stored)?;
        let dir = canonical.join(PROMOTE_DIR);
        let mut target = dir.join(&name);
        let mut attempt = 1;
        loop {
            match std::fs::read(&target) {
                Ok(existing) if existing == bytes => break,
                Ok(_) => {
                    attempt += 1;
                    if attempt > 100 {
                        return Err(ArtifactError::io("too many promoted copies"));
                    }
                    target = dir.join(tagged_file_name(&name, &attempt.to_string()));
                }
                Err(_) => {
                    write_atomic(&target, &bytes)?;
                    break;
                }
            }
        }
        let promoted = target.to_string_lossy().into_owned();
        if artifact.promoted_path.as_deref() != Some(promoted.as_str()) {
            artifact.promoted_path = Some(promoted.clone());
            artifact.updated_at = now_ms();
            self.workspace()
                .put_artifact(&artifact)
                .map_err(|e| ArtifactError::io(e.to_string()))?;
        }
        Ok(ArtifactPromoteReply {
            artifact_id: artifact.id,
            promoted_path: promoted,
        })
    }

    /// `ArtifactPin`.
    pub fn pin(&self, artifact_id: &str, pinned: bool) -> Outcome<ArtifactPinReply> {
        let unpinned = self
            .workspace()
            .set_artifact_pin(artifact_id, pinned)
            .map_err(|e| ArtifactError::io(e.to_string()))?
            .ok_or_else(|| ArtifactError::not_found(format!("artifact {artifact_id} not found")))?;
        Ok(ArtifactPinReply {
            artifact_id: artifact_id.to_owned(),
            pinned,
            unpinned,
        })
    }

    // ── agent tools ─────────────────────────────────────────────────────────

    /// Resolve an agent-supplied path (absolute, or relative to the Chat's
    /// cwd) and authorize it as a source: any regular local file outside the
    /// deny-list.
    fn agent_path(&self, caller: &AgentCaller, raw: &str) -> Outcome<PathBuf> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(ArtifactError::invalid("path is empty"));
        }
        let space = self.local_space(&caller.space_id)?;
        let chat_cwd = self
            .workspace()
            .chat(&caller.chat_id)
            .ok()
            .flatten()
            .and_then(|c| c.cwd)
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| space.path.clone());
        let path = if Path::new(raw).is_absolute() {
            PathBuf::from(raw)
        } else {
            Path::new(&chat_cwd).join(raw)
        };
        self.authorize_source(&path)
    }

    /// `artifact_publish`.
    pub fn agent_publish(&self, caller: &AgentCaller, arguments: &Value) -> Outcome<Value> {
        let args: PublishArgs = parse_args(arguments)?;
        let key =
            mast_proto::agent_tools::validate_idempotency_key(args.idempotency_key.as_deref())
                .map_err(ArtifactError::invalid)?;
        let kind = match args
            .kind
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
        {
            Some(raw) => Some(ArtifactKind::parse(raw).ok_or_else(|| {
                ArtifactError::invalid(format!(
                    "kind must be markdown, html-fragment, html-page, mermaid, image, or \
                     options, not {raw:?}"
                ))
            })?),
            None => None,
        };
        let source = self.agent_path(caller, &args.path)?;
        let reply = self.record(PublishRequest {
            space_id: caller.space_id.clone(),
            work_item_id: caller.work_item_id.clone(),
            chat_id: Some(caller.chat_id.clone()),
            source,
            title: args.title,
            kind,
            role: args.role,
            note: args.note.unwrap_or_default(),
            pin_as_plan: args.pin_as_plan,
            operation_id: Some(publish_operation_id(&caller.chat_id, key)),
            by: caller.actor(),
        })?;
        if !args.addressed_comments.is_empty() {
            self.mark_comments_addressed(&reply.artifact_id, &args.addressed_comments, reply.revision)?;
        }
        Ok(json!({
            "artifactId": reply.artifact_id,
            "revision": reply.revision,
            "url": reply.url,
            "storedPath": reply.stored_path,
            "changed": reply.changed,
            "note": "Link it in your message as [title](mast-artifact:<artifactId>). For visual \
                     kinds, run artifact_render_check before calling it done.",
        }))
    }

    /// `artifact_get_plan`: the pinned plan of the Chat's work item (else of
    /// the Chat), or `null`.
    pub fn agent_get_plan(&self, caller: &AgentCaller) -> Outcome<Value> {
        let picked_options = self.picked_options(caller);
        let plan = self
            .workspace()
            .artifacts_of_space(&caller.space_id)
            .into_iter()
            .filter(|a| a.pinned_as_plan && a.archived_at.is_none())
            .find(|a| match (&caller.work_item_id, a.plan_scope()) {
                (Some(item), Some(PlanScope::WorkItem(theirs))) => item == theirs,
                (None, Some(PlanScope::Chat(chat))) => chat == caller.chat_id,
                _ => false,
            });
        // The RPC transport cannot carry a bare `null` result (`{ok: null}`
        // decodes as no reply), so "no plan" is an object with a null id.
        let Some(plan) = plan else {
            return Ok(json!({
                "artifactId": null,
                "note": "No plan is pinned for this work item yet. Publish one with \
                         artifact_publish {role: \"plan\", pinAsPlan: true}.",
                "pickedOptions": picked_options,
            }));
        };
        let bytes = if plan.device_id == self.device_id() {
            self.current_bytes(&plan).ok()
        } else {
            None
        };
        let content = bytes.map(|b| String::from_utf8_lossy(&b).into_owned());
        let (open_questions, unverified_claims) = content
            .as_deref()
            .map(|text| (count_open_questions(text), count_unverified_claims(text)))
            .unwrap_or((0, 0));
        Ok(json!({
            "artifactId": plan.id,
            "title": plan.title,
            "kind": plan.kind,
            "revision": plan.current_revision,
            "storedPath": plan.stored_path,
            "url": plan.url(),
            "content": content,
            "openQuestions": open_questions,
            "unverifiedClaims": unverified_claims,
            "answeredQuestions": plan.answers.iter().map(|a| json!({
                "id": a.id, "answer": a.answer, "at": a.at, "revision": a.revision,
            })).collect::<Vec<_>>(),
            "comments": self.plan_comments(&plan, content.as_deref()),
            "pickedOptions": picked_options,
        }))
    }

    /// Options artifacts in the caller's scope with a pick:
    /// `[{artifactId, title, optionId, label, note?, at, revision}]`.
    fn picked_options(&self, caller: &AgentCaller) -> Vec<Value> {
        self.workspace()
            .artifacts_of_space(&caller.space_id)
            .into_iter()
            .filter(|a| a.archived_at.is_none() && a.kind == ArtifactKind::Options)
            .filter(|a| match (&caller.work_item_id, a.plan_scope()) {
                (Some(item), Some(PlanScope::WorkItem(theirs))) => item == theirs,
                (None, Some(PlanScope::Chat(chat))) => chat == caller.chat_id,
                _ => false,
            })
            .filter_map(|a| {
                let pick = a.picked.clone()?;
                Some(json!({
                    "artifactId": a.id,
                    "title": a.title,
                    "optionId": pick.option_id,
                    "label": pick.label,
                    "note": pick.note,
                    "at": pick.at,
                    "revision": pick.revision,
                }))
            })
            .collect()
    }

    /// `artifact_render_check`.
    pub fn agent_render_check(&self, caller: &AgentCaller, arguments: &Value) -> Outcome<Value> {
        let args: RenderCheckArgs = parse_args(arguments)?;
        let viewports = args.effective_viewports();
        let themes = args.effective_themes();
        let artifact_id = args.artifact_id.clone().filter(|id| !id.trim().is_empty());
        let path = args.path.clone().filter(|p| !p.trim().is_empty());
        let mut request = ArtifactRenderRequest {
            viewport: viewports[0].size(),
            theme: themes[0],
            viewports,
            themes,
            ..ArtifactRenderRequest::default()
        };
        match (artifact_id, path) {
            (Some(id), _) => {
                let artifact = self
                    .workspace()
                    .artifact(&id)
                    .ok_or_else(|| ArtifactError::not_found(format!("artifact {id} not found")))?;
                if !caller.owns(&artifact) {
                    return Err(ArtifactError::forbidden(
                        "this artifact belongs to another work item or chat",
                    ));
                }
                let artifact = self.local_artifact(&id)?;
                let bytes = self.current_bytes(&artifact)?;
                let dir = Path::new(&artifact.stored_path)
                    .parent()
                    .unwrap_or(Path::new("/"))
                    .to_path_buf();
                request.artifact_id = Some(artifact.id.clone());
                request.title = artifact.title.clone();
                request.kind = artifact.kind;
                request.path = artifact.stored_path.clone();
                request.companions = artifact
                    .companions
                    .iter()
                    .filter_map(|c| {
                        let bytes = read_file(&dir.join(c)).ok()?;
                        Some(ArtifactRenderCompanion {
                            path: c.clone(),
                            content: String::from_utf8_lossy(&bytes).into_owned(),
                        })
                    })
                    .collect();
                set_content(&mut request, bytes);
            }
            (None, Some(raw)) => {
                let path = self.agent_path(caller, &raw)?;
                let bytes = read_file(&path)?;
                let kind = args
                    .kind
                    .as_deref()
                    .and_then(ArtifactKind::parse)
                    .unwrap_or_else(|| ArtifactKind::from_path(&path.to_string_lossy()));
                request.title = default_title_for(&path);
                request.kind = kind;
                request.path = path.to_string_lossy().into_owned();
                if kind == ArtifactKind::Options {
                    let manifest = OptionsManifest::parse(&String::from_utf8_lossy(&bytes))?;
                    let dir = path.parent().unwrap_or(Path::new("/"));
                    request.companions = manifest
                        .options
                        .iter()
                        .filter_map(|o| {
                            let bytes = read_file(&dir.join(&o.path)).ok()?;
                            Some(ArtifactRenderCompanion {
                                path: o.path.clone(),
                                content: String::from_utf8_lossy(&bytes).into_owned(),
                            })
                        })
                        .collect();
                }
                set_content(&mut request, bytes);
            }
            (None, None) => {
                return Err(ArtifactError::invalid("pass artifactId or path"));
            }
        }
        self.inner.render.check_rate(&caller.run_id)?;
        let artifact_id = request.artifact_id.clone();
        let result = self.inner.render.request(request)?;
        // `screenshot` stays the first image for older callers.
        let first = result
            .screenshots
            .first()
            .map(|shot| shot.png_base64.clone())
            .or(result.screenshot_png_base64);
        Ok(json!({
            "artifactId": artifact_id,
            "errors": result.errors,
            "warnings": result.warnings,
            "layoutWarnings": result.layout_warnings,
            "screenshots": result.screenshots.iter().map(|shot| json!({
                "viewport": shot.viewport,
                "width": shot.width,
                "height": shot.height,
                "theme": shot.theme,
                "png": shot.png_base64,
            })).collect::<Vec<_>>(),
            "screenshot": first.map(|data| json!({
                "mimeType": "image/png",
                "data": data,
            })),
            "note": "Layout warnings are heuristics and can be wrong or miss problems: fix \
                     every error, look at every screenshot, and check again before you say \
                     the artifact is done.",
        }))
    }

    // ── marker fallback ─────────────────────────────────────────────────────

    /// Register files an assistant message links with `mast-artifact:<path>` (or the legacy `zeron-artifact:`)
    /// (and Codex `visualize` references) once per (chat, path). Best effort:
    /// failures are logged. Runs on a blocking thread when a runtime exists.
    pub fn note_assistant_text(&self, chat_id: &str, text: &str) {
        let has_marker = text.contains(mast_proto::artifacts::ARTIFACT_URL_SCHEME)
            || text.contains(LEGACY_ARTIFACT_URL_SCHEME);
        let has_visualize = text.contains(mast_proto::visualize::ENVELOPE_PREFIX)
            || text.contains(mast_proto::visualize::LINE_PREFIX);
        if !has_marker && !has_visualize {
            return;
        }
        let service = self.clone();
        let chat_id = chat_id.to_owned();
        let text = text.to_owned();
        let work = move || {
            let imported = service.import_markers(&chat_id, &text);
            if imported > 0 {
                tracing::info!(chat = %chat_id, imported, "registered linked artifacts");
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn_blocking(work);
            }
            Err(_) => work(),
        }
    }

    /// The synchronous body of [`Self::note_assistant_text`]: returns how
    /// many files it registered.
    pub fn import_markers(&self, chat_id: &str, text: &str) -> usize {
        let Some(chat) = self.workspace().chat(chat_id).ok().flatten() else {
            return 0;
        };
        let Some(space_id) = chat.space_id.clone().filter(|s| !s.is_empty()) else {
            return 0;
        };
        let Ok(space) = self.local_space(&space_id) else {
            return 0;
        };
        let caller = AgentCaller {
            chat_id: chat_id.to_owned(),
            run_id: String::new(),
            space_id: space.id.clone(),
            device_id: self.device_id().to_owned(),
            work_item_id: self.workspace().chat_assigned_work_item(chat_id, &space.id),
        };
        let mut wanted: Vec<(String, String, Option<ArtifactKind>)> = artifact_links(text)
            .into_iter()
            .filter(|link| !link.is_id())
            .map(|link| (link.target, link.label, None))
            .collect();
        for reference in mast_proto::visualize::references(text) {
            wanted.push((
                reference.path,
                reference.title,
                Some(ArtifactKind::HtmlFragment),
            ));
        }
        let mut imported = 0;
        for (target, label, kind) in wanted {
            let Ok(path) = self.agent_path(&caller, &target) else {
                tracing::debug!(chat = %chat_id, %target, "artifact link outside the chat's folders");
                continue;
            };
            let key = format!(
                "{MARKER_LEDGER_PREFIX}{}",
                mast_proto::derive_id(
                    &format!("{chat_id}\u{0}{}", path.display()),
                    "artifact-marker"
                )
            );
            if self.workspace().ledger_holds(&key) || !path.is_file() {
                continue;
            }
            let title = if label.trim().is_empty() {
                default_title_for(&path)
            } else {
                label
            };
            let result = self.record(PublishRequest {
                space_id: space.id.clone(),
                work_item_id: caller.work_item_id.clone(),
                chat_id: Some(chat_id.to_owned()),
                source: path.clone(),
                title,
                kind,
                role: None,
                note: IMPORTED_NOTE.to_owned(),
                pin_as_plan: false,
                operation_id: None,
                by: caller.actor(),
            });
            match result {
                Ok(_) => {
                    let _ = self.workspace().ledger_mark(&key);
                    imported += 1;
                }
                Err(error) => {
                    tracing::info!(chat = %chat_id, path = %path.display(), %error, "artifact link not registered")
                }
            }
        }
        imported
    }
}

fn set_content(request: &mut ArtifactRenderRequest, bytes: Vec<u8>) {
    if request.kind.is_binary() {
        request.encoding = ArtifactContentEncoding::Base64;
        request.content = base64::engine::general_purpose::STANDARD.encode(bytes);
    } else {
        request.encoding = ArtifactContentEncoding::Utf8;
        request.content = String::from_utf8_lossy(&bytes).into_owned();
    }
}

fn encode_reply(
    artifact: &ArtifactView,
    kind: ArtifactKind,
    revision: u32,
    path: &Path,
    bytes: Vec<u8>,
) -> ArtifactContentReply {
    let hash = content_hash(&bytes);
    let size_bytes = bytes.len() as u64;
    let (encoding, content) = if kind.is_binary() {
        (
            ArtifactContentEncoding::Base64,
            base64::engine::general_purpose::STANDARD.encode(&bytes),
        )
    } else {
        (
            ArtifactContentEncoding::Utf8,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    };
    ArtifactContentReply {
        artifact_id: artifact.id.clone(),
        kind,
        revision,
        hash,
        mime_type: mime_type_for(&path.to_string_lossy()).to_owned(),
        encoding,
        content,
        size_bytes,
        wrapper_html: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_init(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "t"],
            vec!["commit", "-q", "--allow-empty", "-m", "root"],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(dir)
                    .args(&args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }

    #[test]
    fn main_checkout_of_a_worktree_is_the_main_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let root = canonical(tmp.path());
        let repo = root.join("repo");
        git_init(&repo);
        let wt = root.join("wt");
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["worktree", "add", "-q", "-b", "feature"])
                .arg(&wt)
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(main_checkout(&wt).unwrap(), repo);
        assert_eq!(main_checkout(&repo).unwrap(), repo);
        assert_eq!(main_checkout(&root.join("nowhere")), None);
    }

    #[test]
    fn exclude_is_appended_once_and_never_touches_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = canonical(tmp.path()).join("repo");
        git_init(&repo);
        std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
        assert!(ensure_excluded(&repo).unwrap());
        assert!(!ensure_excluded(&repo).unwrap());
        let exclude = std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
        assert_eq!(exclude.matches(".mast/").count(), 1, "{exclude}");
        assert_eq!(exclude.matches(".zeron/").count(), 1, "{exclude}");
        assert_eq!(
            std::fs::read_to_string(repo.join(".gitignore")).unwrap(),
            "target/\n"
        );
        // Ignored for real.
        for dir in [".zeron", ".mast"] {
            std::fs::create_dir_all(repo.join(dir).join("artifacts")).unwrap();
            std::fs::write(repo.join(dir).join("artifacts/x.md"), "x").unwrap();
        }
        let status = git(&repo, &["status", "--porcelain"]).unwrap();
        assert!(!status.contains(".zeron") && !status.contains(".mast"), "{status}");
    }

    #[test]
    fn exclude_only_adds_the_missing_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = canonical(tmp.path()).join("repo");
        git_init(&repo);
        let path = repo.join(".git/info/exclude");
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        text.push_str("/.zeron/\n");
        std::fs::write(&path, text).unwrap();
        assert!(ensure_excluded(&repo).unwrap());
        let exclude = std::fs::read_to_string(&path).unwrap();
        assert_eq!(exclude.matches(".zeron").count(), 1, "{exclude}");
        assert_eq!(exclude.matches(".mast/").count(), 1, "{exclude}");
        assert!(!ensure_excluded(&repo).unwrap());
    }

    #[test]
    fn canonical_of_stored_knows_both_roots() {
        for root in [".mast", ".zeron"] {
            let stored = Path::new("/repo").join(root).join("artifacts/item-1/plan.md");
            assert_eq!(canonical_of_stored(&stored), Some(PathBuf::from("/repo")));
        }
    }

    #[test]
    fn render_broker_routes_to_the_newest_subscriber_and_times_out() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let broker = Arc::new(RenderBroker::default());
            broker.set_timeout(Duration::from_millis(300));
            let unavailable = tokio::task::spawn_blocking({
                let broker = broker.clone();
                move || broker.request(ArtifactRenderRequest::default())
            })
            .await
            .unwrap();
            assert_eq!(
                unavailable.unwrap_err().code,
                ArtifactErrorCode::RenderUnavailable
            );
            let mut stream = broker.subscribe();
            assert_eq!(broker.subscriber_count(), 1);
            let waiter = tokio::task::spawn_blocking({
                let broker = broker.clone();
                move || {
                    broker.request(ArtifactRenderRequest {
                        title: "T".into(),
                        ..Default::default()
                    })
                }
            });
            let item = stream.next().await.unwrap();
            let request: ArtifactRenderRequest = serde_json::from_value(item).unwrap();
            assert_eq!(request.title, "T");
            assert!(broker.respond(ArtifactRenderResultParams {
                request_id: request.request_id.clone(),
                result: Some(ArtifactRenderCheckResult {
                    screenshot_png_base64: Some("iVBOR".into()),
                    errors: vec!["e".into()],
                    warnings: vec![],
                    ..Default::default()
                }),
                error: None,
            }));
            let result = waiter.await.unwrap().unwrap();
            assert_eq!(result.errors, vec!["e".to_owned()]);
            assert!(!broker.respond(ArtifactRenderResultParams {
                request_id: request.request_id,
                ..Default::default()
            }));
            // Unanswered → timeout.
            let timed_out = tokio::task::spawn_blocking({
                let broker = broker.clone();
                move || broker.request(ArtifactRenderRequest::default())
            });
            let _ = stream.next().await.unwrap();
            assert_eq!(
                timed_out.await.unwrap().unwrap_err().code,
                ArtifactErrorCode::RenderTimeout
            );
            drop(stream);
            assert_eq!(broker.subscriber_count(), 0);
        });
    }

    #[test]
    fn render_checks_are_rate_limited_per_run() {
        let broker = RenderBroker::default();
        for _ in 0..artifact_limits::RENDER_CHECKS_PER_MINUTE {
            broker.check_rate("run-1").unwrap();
        }
        assert_eq!(
            broker.check_rate("run-1").unwrap_err().code,
            ArtifactErrorCode::RateLimited
        );
        broker.check_rate("run-2").unwrap();
    }
}

fn clip_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => text[..cut].trim_end().to_owned(),
        None => text.to_owned(),
    }
}
