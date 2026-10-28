//! Per-chat diff targets: which checkout the Changes pane shows for a chat
//! besides (or instead of) its own `cwd` checkout (contract § "chat diff
//! targets"). Written by the `bind_chat_worktree` agent tool and the Changes
//! pane's "Diff checkout…" picker; never gates dispatch and never moves
//! `chat.cwd`.
//!
//! The API is synchronous: it runs blocking `git` subprocesses (all spawned
//! through [`mast_harness::git_command`]), so async callers use
//! `spawn_blocking`.
//!
//! The *allowed set* a chat may bind is its checkout family: the chat's own
//! checkout (top level of `chat.cwd`), every initialized submodule checkout
//! under it (recursive), and every registered linked worktree of the chat's
//! repository and of each submodule. A path must canonicalize to exactly one
//! member; nothing outside the family is probed.

use std::collections::HashSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio::sync::watch;
use mast_proto::{
    Chat, ChatDiffTargetEntry, ChatDiffTargetSource, ChatDiffTargetsReply, ChatDiffTargetsRow,
    DiffCheckoutCandidate, DiffCheckoutKind, ListChatDiffCheckoutsReply, MAX_CHAT_DIFF_TARGET_OPS,
    MAX_CHAT_DIFF_TARGETS, StoredChatDiffTarget,
};

use crate::execution_scope::{canonical_lenient, repository_id};
use crate::repos::{Repos, hex, is_automatic_access_blocked};
use crate::workspace_host::WorkspaceHost;

mod refs;
pub use refs::RefBindOutcome;

/// Longest path accepted from a caller.
pub const MAX_DIFF_TARGET_PATH_LEN: usize = 4096;
/// Most submodule checkouts discovered under one chat checkout.
const MAX_SUBMODULES: usize = 64;
/// Most candidates one listing returns.
const MAX_CANDIDATES: usize = 200;
/// Deepest submodule nesting followed.
const MAX_SUBMODULE_DEPTH: usize = 8;
/// Paths listed in a refusal message so the caller can pick a valid one.
const MAX_PATHS_IN_MESSAGE: usize = 8;

// ── errors ──────────────────────────────────────────────────────────────────

/// Why a diff-target operation was refused. Messages are written for an agent
/// to read and fix its call; [`DiffTargetError::code`] is the stable key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffTargetError {
    /// No chat with this id.
    ChatNotFound(String),
    /// The chat is hosted on another device.
    NotLocal { chat_device: String },
    /// The chat has no usable working directory or it is not a git checkout.
    NoCheckout(String),
    /// Malformed path argument (empty, relative, too long, missing…).
    Invalid(String),
    /// HOME or a privacy-protected folder.
    Blocked(String),
    /// The path is not inside the chat's repository family.
    OutsideRepository(String),
    /// The path is inside an allowed checkout but is not a checkout itself.
    NotAWorktree(String),
    /// The chat already holds [`MAX_CHAT_DIFF_TARGETS`] targets.
    Limit(String),
    /// The operation id was already used for a different path.
    OperationConflict(String),
    /// Storage or git failure.
    Internal(String),
}

impl DiffTargetError {
    /// Stable machine-readable key.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ChatNotFound(_) => "chat-not-found",
            Self::NotLocal { .. } => "not-local",
            Self::NoCheckout(_) => "no-checkout",
            Self::Invalid(_) => "invalid-path",
            Self::Blocked(_) => "blocked-path",
            Self::OutsideRepository(_) => "outside-repository",
            Self::NotAWorktree(_) => "not-a-checkout",
            Self::Limit(_) => "too-many-targets",
            Self::OperationConflict(_) => "operation-conflict",
            Self::Internal(_) => "internal",
        }
    }
}

impl fmt::Display for DiffTargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChatNotFound(id) => write!(f, "No chat with id {id}."),
            Self::NotLocal { chat_device } => write!(
                f,
                "This chat runs on another device ({chat_device}); diff targets can only be set \
                 on the device that hosts the chat."
            ),
            Self::NoCheckout(message)
            | Self::Invalid(message)
            | Self::Blocked(message)
            | Self::OutsideRepository(message)
            | Self::NotAWorktree(message)
            | Self::Limit(message)
            | Self::OperationConflict(message)
            | Self::Internal(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for DiffTargetError {}

// ── service ─────────────────────────────────────────────────────────────────

/// What [`DiffTargets::bind`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindOutcome {
    /// The chat's targets after the call.
    pub reply: ChatDiffTargetsReply,
    /// The operation id was already applied: nothing was written.
    pub replayed: bool,
    /// Canonical checkout path that was bound.
    pub path: String,
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    pub repo_name: String,
    /// The bound checkout is now the chat's selected target. `false` only
    /// when the path is the chat's own checkout, which clears the selection
    /// instead of storing a duplicate target.
    pub selected: bool,
}

/// Reads and writes the `chatDiffTargets` rows and validates paths against
/// the chat's checkout family. Cheap to clone.
#[derive(Clone)]
pub struct DiffTargets {
    repos: Repos,
    workspace: WorkspaceHost,
    device_id: String,
    changes: Arc<watch::Sender<u64>>,
    /// `gh` for the PR lookups of "Diff branch / PR…" (tests point it at a
    /// fake).
    tools: crate::pr_review_launch::PrReviewTools,
}

impl DiffTargets {
    pub fn new(repos: Repos, workspace: WorkspaceHost, device_id: impl Into<String>) -> Self {
        Self {
            repos,
            workspace,
            device_id: device_id.into(),
            changes: Arc::new(watch::channel(0).0),
            tools: crate::pr_review_launch::PrReviewTools::default(),
        }
    }

    /// Use `tools` (the `gh` executable) for PR lookups.
    pub fn with_pr_tools(mut self, tools: crate::pr_review_launch::PrReviewTools) -> Self {
        self.tools = tools;
        self
    }

    pub fn repos(&self) -> &Repos {
        &self.repos
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// A counter bumped after every effective write (bind, unbind, selection
    /// change). Diff-sync re-reads [`Self::rows`] when it moves.
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// Every stored row (diff-sync resolves them into views).
    pub fn rows(&self) -> Vec<ChatDiffTargetsRow> {
        self.workspace.all_chat_diff_targets().unwrap_or_default()
    }

    /// The chat's stored targets (empty when it never bound one).
    pub fn stored(&self, chat_id: &str) -> ChatDiffTargetsReply {
        reply_of(
            &self
                .workspace
                .chat_diff_targets(chat_id)
                .ok()
                .flatten()
                .unwrap_or_default(),
        )
    }

    /// The "Diff checkout…" picker: the chat's own checkout, its submodule
    /// checkouts, then linked worktrees of the root repository and of each
    /// submodule.
    pub fn list_checkouts(
        &self,
        chat_id: &str,
    ) -> Result<ListChatDiffCheckoutsReply, DiffTargetError> {
        let chat = self.local_chat(chat_id)?;
        let family = Family::resolve(&chat, true)?;
        let row = self
            .workspace
            .chat_diff_targets(chat_id)
            .map_err(internal)?
            .unwrap_or_default();
        let selected = effective_selected(&row);
        let candidates = family
            .checkouts
            .iter()
            .map(|checkout| {
                let path = path_string(&checkout.path);
                let bound = row.targets.iter().any(|t| t.path == path);
                let is_selected = match checkout.kind {
                    DiffCheckoutKind::Own => selected.is_none(),
                    _ => selected == Some(path.as_str()),
                };
                DiffCheckoutCandidate {
                    path,
                    kind: checkout.kind,
                    repo_name: checkout.repo_name.clone(),
                    branch: checkout.branch.clone(),
                    head_sha: checkout.head_sha.clone(),
                    bound,
                    selected: is_selected,
                }
            })
            .collect();
        Ok(ListChatDiffCheckoutsReply { candidates })
    }

    /// Bind `path` as a diff target of the chat and select it (see the module
    /// docs for the allowed set). A known `operation_id` writes nothing and
    /// reports `replayed`.
    pub fn bind(
        &self,
        chat_id: &str,
        path: &str,
        source: ChatDiffTargetSource,
        operation_id: Option<&str>,
    ) -> Result<BindOutcome, DiffTargetError> {
        let requested = parse_path_argument(path)?;
        let chat = self.local_chat(chat_id)?;
        let family = Family::resolve(&chat, false)?;
        let checkout = family.member(&requested)?;
        let facts = checkout_facts(&checkout.path)
            .filter(|facts| facts.top == checkout.path)
            .ok_or_else(|| {
                DiffTargetError::NotAWorktree(format!(
                    "{} is registered as a checkout of this repository but is not a usable git \
                     work tree (missing or damaged). Run `git worktree prune` or pick another \
                     path. {}",
                    checkout.path.display(),
                    family.allowed_message()
                ))
            })?;
        let path_text = path_string(&checkout.path);
        let is_own = checkout.kind == DiffCheckoutKind::Own;
        let worktree_id = worktree_id(&self.device_id, &facts.git_dir);
        let repo_id = repository_id(&self.device_id, &facts.common_dir);
        let (branch, head_sha) = head_of(&checkout.path);
        let repo_name = checkout.repo_name.clone();

        let mut replayed = false;
        let mut failure: Option<DiffTargetError> = None;
        let changed = self
            .workspace
            .update_chat_diff_targets(chat_id, |row| {
                if let Some(op) = operation_id {
                    if row.ops.iter().any(|known| known == op) {
                        if is_own || row.targets.iter().any(|t| t.path == path_text) {
                            replayed = true;
                        } else {
                            failure = Some(DiffTargetError::OperationConflict(format!(
                                "idempotencyKey was already used to bind a different path. Use a \
                                 new idempotencyKey for {path_text}."
                            )));
                        }
                        return;
                    }
                }
                // At most one of the two selections is set.
                row.selected_ref = None;
                if is_own {
                    row.selected = None;
                } else {
                    match row.targets.iter_mut().find(|t| t.path == path_text) {
                        Some(existing) => {
                            existing.repo_id = repo_id.clone();
                            existing.worktree_id = worktree_id.clone();
                        }
                        None => {
                            if row.targets.len() >= MAX_CHAT_DIFF_TARGETS {
                                // Make room only by dropping targets whose
                                // checkout no longer exists on disk.
                                row.targets.retain(|t| Path::new(&t.path).is_dir());
                            }
                            if row.targets.len() >= MAX_CHAT_DIFF_TARGETS {
                                failure = Some(DiffTargetError::Limit(format!(
                                    "This chat already has {MAX_CHAT_DIFF_TARGETS} diff targets. \
                                     Remove one you no longer need (unbind it in the Changes \
                                     pane's Diff checkout… picker) before binding {path_text}."
                                )));
                                return;
                            }
                            row.targets.push(StoredChatDiffTarget {
                                path: path_text.clone(),
                                source,
                                repo_id: repo_id.clone(),
                                worktree_id: worktree_id.clone(),
                                bound_at: crate::now_ms(),
                            });
                        }
                    }
                    row.selected = Some(path_text.clone());
                }
                if let Some(op) = operation_id {
                    row.ops.push(op.to_owned());
                    trim_ops(&mut row.ops);
                }
            })
            .map_err(internal)?;
        if let Some(failure) = failure {
            return Err(failure);
        }
        if changed {
            self.bump();
        }
        Ok(BindOutcome {
            reply: self.stored(chat_id),
            replayed,
            path: path_text,
            branch,
            head_sha,
            repo_name,
            selected: !is_own,
        })
    }

    /// Forget `path` as a target (clears the selection when it was selected).
    /// A path that is not a target is a no-op. Accepts stored paths even when
    /// the checkout is gone.
    pub fn unbind(
        &self,
        chat_id: &str,
        path: &str,
    ) -> Result<ChatDiffTargetsReply, DiffTargetError> {
        let requested = parse_path_argument(path)?;
        self.local_chat(chat_id)?;
        let literal = path_string(&requested);
        let lenient = path_string(&canonical_lenient(&requested));
        let changed = self
            .workspace
            .update_chat_diff_targets(chat_id, |row| {
                let before = row.targets.len();
                row.targets
                    .retain(|t| t.path != literal && t.path != lenient);
                if row.targets.len() != before
                    && row
                        .selected
                        .as_deref()
                        .is_some_and(|s| s == literal || s == lenient)
                {
                    row.selected = None;
                }
            })
            .map_err(internal)?;
        if changed {
            self.bump();
        }
        Ok(self.stored(chat_id))
    }

    /// Show the chat's own checkout again; bound targets stay listed.
    pub fn clear_selection(&self, chat_id: &str) -> Result<ChatDiffTargetsReply, DiffTargetError> {
        self.local_chat(chat_id)?;
        let changed = self
            .workspace
            .update_chat_diff_targets(chat_id, |row| {
                row.selected = None;
                row.selected_ref = None;
            })
            .map_err(internal)?;
        if changed {
            self.bump();
        }
        Ok(self.stored(chat_id))
    }

    fn bump(&self) {
        self.changes.send_modify(|count| *count += 1);
    }

    fn local_chat(&self, chat_id: &str) -> Result<Chat, DiffTargetError> {
        let chat = self
            .workspace
            .chat(chat_id)
            .map_err(internal)?
            .ok_or_else(|| DiffTargetError::ChatNotFound(chat_id.to_owned()))?;
        if chat.device_id != self.device_id {
            return Err(DiffTargetError::NotLocal {
                chat_device: chat.device_id,
            });
        }
        Ok(chat)
    }
}

fn internal(error: impl fmt::Display) -> DiffTargetError {
    DiffTargetError::Internal(error.to_string())
}

fn trim_ops(ops: &mut Vec<String>) {
    let over = ops.len().saturating_sub(MAX_CHAT_DIFF_TARGET_OPS);
    ops.drain(..over);
}

/// The selected path when it still names a stored target.
fn effective_selected(row: &ChatDiffTargetsRow) -> Option<&str> {
    row.selected
        .as_deref()
        .filter(|selected| row.targets.iter().any(|t| t.path == *selected))
}

fn reply_of(row: &ChatDiffTargetsRow) -> ChatDiffTargetsReply {
    let selected = effective_selected(row);
    let selected_ref = row
        .selected_ref
        .as_deref()
        .filter(|id| row.ref_targets.iter().any(|t| t.id == *id));
    ChatDiffTargetsReply {
        ref_targets: row
            .ref_targets
            .iter()
            .map(|t| mast_proto::ChatRefTargetEntry {
                id: t.id.clone(),
                kind: t.kind,
                label: crate::diff_sync::ref_target_label(t),
                selected: selected_ref == Some(t.id.as_str()),
            })
            .collect(),
        targets: row
            .targets
            .iter()
            .map(|t| ChatDiffTargetEntry {
                path: t.path.clone(),
                source: t.source,
                selected: selected == Some(t.path.as_str()),
            })
            .collect(),
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn worktree_id(device_id: &str, canonical_git_dir: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(device_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(canonical_git_dir.to_string_lossy().as_bytes());
    hex(&hasher.finalize())
}

/// Syntax checks on a caller-supplied path.
fn parse_path_argument(path: &str) -> Result<PathBuf, DiffTargetError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(DiffTargetError::Invalid(
            "path is empty. Pass the absolute path of the worktree directory, e.g. the one you \
             gave to `git worktree add`."
                .into(),
        ));
    }
    if trimmed.len() > MAX_DIFF_TARGET_PATH_LEN {
        return Err(DiffTargetError::Invalid(format!(
            "path is longer than {MAX_DIFF_TARGET_PATH_LEN} characters."
        )));
    }
    if trimmed.contains('\0') {
        return Err(DiffTargetError::Invalid(
            "path contains a NUL character.".into(),
        ));
    }
    let candidate = PathBuf::from(trimmed);
    if !candidate.is_absolute() {
        return Err(DiffTargetError::Invalid(format!(
            "path {trimmed:?} is relative. Pass an absolute path (for example the output of \
             `cd <worktree> && pwd`)."
        )));
    }
    Ok(candidate)
}

// ── checkout family ─────────────────────────────────────────────────────────

/// One member of a chat's checkout family.
#[derive(Debug, Clone)]
struct Checkout {
    /// Canonical top-level path.
    path: PathBuf,
    kind: DiffCheckoutKind,
    repo_name: String,
    branch: Option<String>,
    head_sha: Option<String>,
}

#[derive(Debug)]
struct Family {
    own: PathBuf,
    /// Own checkout first, then submodules, then worktrees.
    checkouts: Vec<Checkout>,
}

impl Family {
    /// Discover the family of `chat`. `heads` fills branch and HEAD of own
    /// and submodule checkouts (worktree entries always carry them).
    fn resolve(chat: &Chat, heads: bool) -> Result<Self, DiffTargetError> {
        let cwd = chat.cwd.as_deref().map(str::trim).unwrap_or("");
        if cwd.is_empty() || !Path::new(cwd).is_absolute() {
            return Err(DiffTargetError::NoCheckout(
                "This chat has no working directory on this device, so there is no repository \
                 to take a worktree from."
                    .into(),
            ));
        }
        let cwd_path = Path::new(cwd);
        if is_automatic_access_blocked(&canonical_lenient(cwd_path)) {
            return Err(DiffTargetError::Blocked(format!(
                "The chat's working directory {cwd} is the home folder or a protected folder; \
                 diff targets are not supported there."
            )));
        }
        let not_a_checkout = || {
            DiffTargetError::NoCheckout(format!(
                "The chat's working directory {cwd} is not inside a git checkout, so there is no \
                 repository to take a worktree from."
            ))
        };
        if !cwd_path.is_dir() {
            return Err(not_a_checkout());
        }
        let facts = checkout_facts(cwd_path).ok_or_else(not_a_checkout)?;
        let own = facts.top.clone();
        if is_automatic_access_blocked(&own) {
            return Err(DiffTargetError::Blocked(format!(
                "The chat's checkout {} is the home folder or a protected folder; diff targets \
                 are not supported there.",
                own.display()
            )));
        }
        let root_main = main_checkout_of(&facts.common_dir).unwrap_or_else(|| own.clone());
        let root_name = base_name(&root_main);

        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut checkouts: Vec<Checkout> = Vec::new();
        seen.insert(own.clone());
        let (branch, head_sha) = if heads { head_of(&own) } else { (None, None) };
        checkouts.push(Checkout {
            path: own.clone(),
            kind: DiffCheckoutKind::Own,
            repo_name: root_name.clone(),
            branch,
            head_sha,
        });

        let mut submodules: Vec<(PathBuf, Facts)> = Vec::new();
        discover_submodules(&own, 0, &mut submodules);
        for (path, _) in &submodules {
            if !seen.insert(path.clone()) {
                continue;
            }
            let (branch, head_sha) = if heads { head_of(path) } else { (None, None) };
            checkouts.push(Checkout {
                path: path.clone(),
                kind: DiffCheckoutKind::Submodule,
                repo_name: base_name(path),
                branch,
                head_sha,
            });
        }

        // Linked worktrees: the root repository's, then each submodule's.
        let mut repositories: Vec<(PathBuf, PathBuf, String)> =
            vec![(own.clone(), facts.common_dir.clone(), root_name)];
        for (path, sub_facts) in &submodules {
            repositories.push((path.clone(), sub_facts.common_dir.clone(), base_name(path)));
        }
        for (repo_dir, common_dir, repo_name) in repositories {
            for entry in list_worktrees(&repo_dir) {
                if entry.prunable {
                    continue;
                }
                let Ok(listed) = std::fs::canonicalize(&entry.path) else {
                    continue;
                };
                let (path, branch, head_sha) = if listed == common_dir {
                    // A submodule's main entry is its module git dir: the
                    // checkout is the module's `core.worktree`.
                    match main_checkout_of(&common_dir) {
                        Some(main) if main.is_dir() => {
                            let (branch, head_sha) = head_of(&main);
                            (main, branch, head_sha)
                        }
                        _ => continue,
                    }
                } else if entry.bare {
                    continue;
                } else {
                    (listed, entry.branch, entry.head_sha)
                };
                if !path.is_dir() || !seen.insert(path.clone()) {
                    continue;
                }
                checkouts.push(Checkout {
                    path,
                    kind: DiffCheckoutKind::Worktree,
                    repo_name: repo_name.clone(),
                    branch,
                    head_sha,
                });
            }
        }
        checkouts.truncate(MAX_CANDIDATES);
        Ok(Self { own, checkouts })
    }

    /// The member whose canonical path equals `requested`, or the refusal
    /// explaining what went wrong.
    fn member(&self, requested: &Path) -> Result<&Checkout, DiffTargetError> {
        let shown = requested.display();
        let canonical = std::fs::canonicalize(requested).map_err(|_| {
            DiffTargetError::Invalid(format!(
                "path {shown} does not exist. Create the worktree first (`git worktree add`) and \
                 pass its absolute path. {}",
                self.allowed_message()
            ))
        })?;
        if !canonical.is_dir() {
            return Err(DiffTargetError::Invalid(format!(
                "path {shown} is not a directory. Pass the worktree's top-level directory. {}",
                self.allowed_message()
            )));
        }
        if is_automatic_access_blocked(&canonical) {
            return Err(DiffTargetError::Blocked(format!(
                "path {shown} is the home folder or a protected folder and cannot be a diff \
                 target."
            )));
        }
        if let Some(found) = self.checkouts.iter().find(|c| c.path == canonical) {
            return Ok(found);
        }
        if canonical
            .components()
            .any(|c| c == Component::Normal(".git".as_ref()))
        {
            return Err(DiffTargetError::NotAWorktree(format!(
                "path {shown} is git metadata, not a checkout. Pass the worktree directory that \
                 contains your files. {}",
                self.allowed_message()
            )));
        }
        if let Some(parent) = self
            .checkouts
            .iter()
            .filter(|c| canonical.starts_with(&c.path))
            .max_by_key(|c| c.path.components().count())
        {
            return Err(DiffTargetError::NotAWorktree(format!(
                "path {shown} is a folder inside the checkout {}, not a checkout's top level. \
                 Pass the top-level directory of the worktree (the one `git worktree list` \
                 shows). {}",
                parent.path.display(),
                self.allowed_message()
            )));
        }
        Err(DiffTargetError::OutsideRepository(format!(
            "path {shown} (resolved to {}) is not a checkout of this chat's repository \
             ({}) or of one of its submodules. {}",
            canonical.display(),
            self.own.display(),
            self.allowed_message()
        )))
    }

    fn allowed_message(&self) -> String {
        let mut shown: Vec<String> = self
            .checkouts
            .iter()
            .take(MAX_PATHS_IN_MESSAGE)
            .map(|c| c.path.display().to_string())
            .collect();
        if self.checkouts.len() > MAX_PATHS_IN_MESSAGE {
            shown.push(format!(
                "… and {} more",
                self.checkouts.len() - MAX_PATHS_IN_MESSAGE
            ));
        }
        format!(
            "Allowed: the chat's checkout, its submodule checkouts, and registered git worktrees \
             of those repositories ({}).",
            shown.join(", ")
        )
    }
}

fn base_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

// ── git ─────────────────────────────────────────────────────────────────────

/// Canonical paths of the checkout rooted at a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Facts {
    top: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
}

/// Raw stdout of a successful `git` run in `dir`.
fn git_stdout(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = mast_harness::git_command()
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn git_text(dir: &Path, args: &[&str]) -> Option<String> {
    let out = git_stdout(dir, args)?;
    Some(String::from_utf8_lossy(&out).trim().to_owned())
}

/// Top level, git dir and common dir of the work tree containing `dir`;
/// `None` outside a work tree (including inside a bare git dir).
fn checkout_facts(dir: &Path) -> Option<Facts> {
    let out = git_text(
        dir,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-dir",
            "--git-common-dir",
        ],
    )?;
    let mut lines = out.lines();
    let top = lines.next()?;
    let git_dir = lines.next()?;
    let common_dir = lines.next()?;
    let canon = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p));
    Some(Facts {
        top: canon(top),
        git_dir: canon(git_dir),
        common_dir: canon(common_dir),
    })
}

/// Branch (short name, `None` when detached) and HEAD sha of a checkout.
fn head_of(dir: &Path) -> (Option<String>, Option<String>) {
    let branch =
        git_text(dir, &["symbolic-ref", "--short", "-q", "HEAD"]).filter(|b| !b.is_empty());
    let sha = git_text(dir, &["rev-parse", "--verify", "-q", "HEAD"]).filter(|s| !s.is_empty());
    (branch, sha)
}

/// The main checkout of the repository whose canonical common dir is given:
/// `<main>/.git`, or a submodule's `core.worktree`.
fn main_checkout_of(common_dir: &Path) -> Option<PathBuf> {
    if common_dir.file_name().is_some_and(|name| name == ".git") {
        return common_dir.parent().map(Path::to_path_buf);
    }
    let config = common_dir.join("config");
    let config = config.to_string_lossy().into_owned();
    let out = mast_harness::git_command()
        .args(["config", "--file", &config, "--get", "core.worktree"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let worktree = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!worktree.is_empty()).then(|| canonical_lenient(&common_dir.join(worktree)))
}

/// One record of `git worktree list --porcelain -z`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WorktreeEntry {
    path: PathBuf,
    head_sha: Option<String>,
    branch: Option<String>,
    bare: bool,
    prunable: bool,
}

/// Parse `git worktree list --porcelain -z`: NUL-terminated fields, records
/// separated by an empty field.
fn parse_worktree_list_z(raw: &str) -> Vec<WorktreeEntry> {
    raw.split("\0\0")
        .filter_map(|record| {
            let mut entry = WorktreeEntry {
                path: PathBuf::new(),
                head_sha: None,
                branch: None,
                bare: false,
                prunable: false,
            };
            let mut has_path = false;
            for field in record.split('\0').filter(|f| !f.is_empty()) {
                if let Some(value) = field.strip_prefix("worktree ") {
                    entry.path = PathBuf::from(value);
                    has_path = true;
                } else if let Some(value) = field.strip_prefix("HEAD ") {
                    entry.head_sha = Some(value.to_owned()).filter(|v| !v.is_empty());
                } else if let Some(value) = field.strip_prefix("branch ") {
                    entry.branch = Some(
                        value
                            .strip_prefix("refs/heads/")
                            .unwrap_or(value)
                            .to_owned(),
                    );
                } else if field == "bare" {
                    entry.bare = true;
                } else if field == "prunable" || field.starts_with("prunable ") {
                    entry.prunable = true;
                }
            }
            has_path.then_some(entry)
        })
        .collect()
}

fn list_worktrees(repo_dir: &Path) -> Vec<WorktreeEntry> {
    git_stdout(repo_dir, &["worktree", "list", "--porcelain", "-z"])
        .map(|raw| parse_worktree_list_z(&String::from_utf8_lossy(&raw)))
        .unwrap_or_default()
}

/// Paths from `git config --null --file .gitmodules --get-regexp
/// ^submodule\..*\.path$` output (`key\nvalue\0` records).
fn parse_submodule_paths(raw: &[u8]) -> Vec<String> {
    raw.split(|byte| *byte == 0)
        .filter_map(|record| {
            let at = record.iter().position(|byte| *byte == b'\n')?;
            let value = String::from_utf8_lossy(&record[at + 1..]).into_owned();
            (!value.is_empty()).then_some(value)
        })
        .collect()
}

/// A declared submodule path that stays inside its parent (plain relative
/// components only).
fn relative_submodule_path(path: &str) -> Option<PathBuf> {
    let relative = Path::new(path);
    (!relative.as_os_str().is_empty()
        && relative
            .components()
            .all(|component| matches!(component, Component::Normal(_))))
    .then(|| relative.to_path_buf())
}

/// Initialized submodule checkouts under `root` (recursive, capped). Symlinks
/// are never followed and an uninitialized directory never falls through to
/// its parent repository.
fn discover_submodules(root: &Path, depth: usize, out: &mut Vec<(PathBuf, Facts)>) {
    if depth >= MAX_SUBMODULE_DEPTH || out.len() >= MAX_SUBMODULES {
        return;
    }
    let modules = root.join(".gitmodules");
    match std::fs::symlink_metadata(&modules) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {}
        _ => return,
    }
    let modules_arg = modules.to_string_lossy().into_owned();
    let Some(raw) = git_stdout(
        root,
        &[
            "config",
            "--null",
            "--file",
            &modules_arg,
            "--get-regexp",
            "^submodule\\..*\\.path$",
        ],
    ) else {
        return;
    };
    for declared in parse_submodule_paths(&raw) {
        if out.len() >= MAX_SUBMODULES {
            return;
        }
        let Some(relative) = relative_submodule_path(&declared) else {
            continue;
        };
        let Some(found) = resolve_submodule(root, &relative) else {
            continue;
        };
        if out.iter().any(|(path, _)| *path == found.0) {
            continue;
        }
        let nested_root = found.0.clone();
        out.push(found);
        discover_submodules(&nested_root, depth + 1, out);
    }
}

fn resolve_submodule(root: &Path, relative: &Path) -> Option<(PathBuf, Facts)> {
    let mut candidate = root.to_path_buf();
    for component in relative.components() {
        candidate.push(component);
        let meta = std::fs::symlink_metadata(&candidate).ok()?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return None;
        }
    }
    let canonical = std::fs::canonicalize(&candidate).ok()?;
    let parent = std::fs::canonicalize(root).ok()?;
    if canonical == parent || !canonical.starts_with(&parent) {
        return None;
    }
    let facts = checkout_facts(&canonical)?;
    (facts.top == canonical).then_some((canonical, facts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_list_z_parses_branches_detached_bare_and_prunable() {
        let raw = concat!(
            "worktree /r/main\0HEAD aaaa\0branch refs/heads/main\0\0",
            "worktree /r/.worktrees/x y\0HEAD bbbb\0detached\0\0",
            "worktree /r/.git/modules/m\0bare\0\0",
            "worktree /gone\0HEAD cccc\0branch refs/heads/feat/one\0prunable gitdir file points to non-existent location\0\0",
        );
        let entries = parse_worktree_list_z(raw);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].path, PathBuf::from("/r/main"));
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert_eq!(entries[0].head_sha.as_deref(), Some("aaaa"));
        assert_eq!(entries[1].path, PathBuf::from("/r/.worktrees/x y"));
        assert_eq!(entries[1].branch, None);
        assert!(entries[2].bare);
        assert!(entries[3].prunable);
        assert_eq!(entries[3].branch.as_deref(), Some("feat/one"));
        assert!(parse_worktree_list_z("").is_empty());
    }

    #[test]
    fn submodule_paths_come_from_null_separated_config_records() {
        let raw = b"submodule.a.path\nflagship\0submodule.b.path\nlibs/ui\0";
        assert_eq!(parse_submodule_paths(raw), ["flagship", "libs/ui"]);
        assert!(parse_submodule_paths(b"").is_empty());
    }

    #[test]
    fn submodule_paths_must_stay_inside_the_parent() {
        assert!(relative_submodule_path("flagship").is_some());
        assert!(relative_submodule_path("libs/ui").is_some());
        assert!(relative_submodule_path("../escape").is_none());
        assert!(relative_submodule_path("/abs").is_none());
        assert!(relative_submodule_path("a/../b").is_none());
        assert!(relative_submodule_path("").is_none());
    }

    #[test]
    fn path_arguments_are_checked_before_any_git_call() {
        assert_eq!(
            parse_path_argument("  ").unwrap_err().code(),
            "invalid-path"
        );
        assert_eq!(
            parse_path_argument("relative/dir").unwrap_err().code(),
            "invalid-path"
        );
        assert_eq!(
            parse_path_argument(&format!("/{}", "a".repeat(MAX_DIFF_TARGET_PATH_LEN)))
                .unwrap_err()
                .code(),
            "invalid-path"
        );
        assert_eq!(
            parse_path_argument("/ok/path").unwrap(),
            PathBuf::from("/ok/path")
        );
    }

    #[test]
    fn selection_only_counts_when_it_names_a_stored_target() {
        let target = |path: &str| StoredChatDiffTarget {
            path: path.into(),
            source: ChatDiffTargetSource::Agent,
            repo_id: String::new(),
            worktree_id: String::new(),
            bound_at: 0,
        };
        let mut row = ChatDiffTargetsRow {
            chat_id: "c".into(),
            targets: vec![target("/a")],
            selected: Some("/gone".into()),
            ..Default::default()
        };
        assert_eq!(effective_selected(&row), None);
        assert!(!reply_of(&row).targets[0].selected);
        row.selected = Some("/a".into());
        assert_eq!(effective_selected(&row), Some("/a"));
        assert!(reply_of(&row).targets[0].selected);
    }

    #[test]
    fn operation_ids_keep_only_the_newest() {
        let mut ops: Vec<String> = (0..MAX_CHAT_DIFF_TARGET_OPS + 3)
            .map(|i| format!("op-{i}"))
            .collect();
        trim_ops(&mut ops);
        assert_eq!(ops.len(), MAX_CHAT_DIFF_TARGET_OPS);
        assert_eq!(ops[0], "op-3");
    }

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(
            DiffTargetError::Limit("x".into()).code(),
            "too-many-targets"
        );
        assert_eq!(
            DiffTargetError::NotLocal {
                chat_device: "d".into()
            }
            .code(),
            "not-local"
        );
        assert!(
            DiffTargetError::ChatNotFound("c1".into())
                .to_string()
                .contains("c1")
        );
    }
}
