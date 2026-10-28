//! Execution-scope validation (Work Outline Milestone 3; contract section 6,
//! `docs/plans/work-outline/m0-execution-scope.md` sections 2 and 5).
//!
//! [`validate`] checks a resolved execution configuration on the Space's
//! OWNING device: device, live Space, execution root, each repository (path,
//! topology id, root-commit fingerprint), each pinned worktree (identity,
//! path, registration, prunable), expected branch, member containment, and
//! structure. It collects every failure, never names an alternative
//! checkout, and is read-only: no worktree is created, no branch is created
//! or switched, nothing is written. Git access goes through the injectable
//! [`GitProbe`]; [`ReposGitProbe`] is the production probe over the existing
//! `Repos` git subprocess helper.
//!
//! Identities (audit 2.1):
//! - repository = `repo:` + sha256(device NUL canonical git common dir) (the
//!   topology id) + fingerprint = a root commit of HEAD's history (the id is
//!   path-derived, so only the fingerprint detects a re-cloned path);
//! - checkout = `CheckoutIdentity.id` = sha256(device NUL canonical git dir).
//!
//! Main checkouts are identified by `git-dir == git-common-dir`, never by the
//! position in `git worktree list`: inside a submodule git lists the main
//! worktree as `.git/modules/<name>` (audit 2.2).
//!
//! [`ExecutionScopeService::validate_for_dispatch`] is the helper the Milestone
//! 3 start workflow and the dispatch chokepoints call (stage 2 wires it into
//! `DocHost`).

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use zeron_proto::{
    ExecutionHostCapabilities, ExecutionRepositoryIdentity, ExecutionValidationError,
    ExecutionWorktreeIdentity, RepositoryBinding, ResolvedExecutionConfiguration, Space,
    ValidatedExecution, ValidatedRepository, execution_configuration_shape_errors,
    is_absolute_device_path,
};

use crate::EngineError;
use crate::repos::Repos;
use crate::repository_discovery::{parse_worktree_porcelain_z, stable_id};
use crate::workspace_host::WorkspaceHost;

/// Prefix of a dispatch rejection message (`execution-scope:<code>: …`), so a
/// client can map a `Rejected` command back to remediation actions.
pub const REJECTION_PREFIX: &str = "execution-scope:";

// ── probe ───────────────────────────────────────────────────────────────────

/// What is at a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    Missing,
    File,
    Directory,
}

/// Git facts of the checkout whose top level is a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutFacts {
    /// Canonical `--show-toplevel`.
    pub top_level: PathBuf,
    /// Canonical `--git-dir` (worktree-specific).
    pub git_dir: PathBuf,
    /// Canonical `--git-common-dir`.
    pub common_dir: PathBuf,
    /// `CheckoutIdentity.id`.
    pub checkout_id: String,
}

impl CheckoutFacts {
    /// The repository's main checkout (`git-dir == git-common-dir`).
    pub fn is_main(&self) -> bool {
        self.git_dir == self.common_dir
    }
}

/// One registered worktree of a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeFacts {
    /// As git reports it (NOT canonical; for a submodule's main checkout
    /// this is the module git dir).
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    pub prunable: bool,
    pub locked: bool,
}

/// Read-only git and filesystem access for [`validate`].
#[async_trait]
pub trait GitProbe: Send + Sync {
    /// The device whose paths this probe reads (checkout and repository ids
    /// are device-scoped).
    fn device_id(&self) -> &str;
    async fn path_kind(&self, path: &Path) -> PathKind;
    /// Facts of the checkout at `path`, or `None` when `path` is not inside
    /// a work tree. The caller compares `top_level` with `path`.
    async fn checkout(&self, path: &Path) -> Option<CheckoutFacts>;
    /// Root commits reachable from HEAD at `path`, sorted; empty for a
    /// repository without commits.
    async fn root_commits(&self, path: &Path, common_dir: &Path) -> Vec<String>;
    /// `git worktree list` of the repository at `path`.
    async fn worktrees(&self, path: &Path) -> Vec<WorktreeFacts>;
    /// Current branch (`None` when detached) and HEAD sha at `path`.
    async fn head(&self, path: &Path) -> (Option<String>, Option<String>);
    /// The main checkout of the repository whose common dir is
    /// `common_dir` (`<main>/.git`, or a submodule's `core.worktree`).
    async fn main_checkout(&self, common_dir: &Path) -> Option<PathBuf>;
}

/// The production probe: `Repos::git` subprocesses, with root commits cached
/// per `(common dir, HEAD)` (the same HEAD has the same roots; a re-clone at
/// the same path has another HEAD, so the cache never hides it).
#[derive(Clone)]
pub struct ReposGitProbe {
    repos: Repos,
    device_id: String,
    roots: Arc<Mutex<RootCommitCache>>,
}

/// Root commits by `(canonical common dir, HEAD sha)`.
type RootCommitCache = HashMap<(PathBuf, String), Vec<String>>;

impl ReposGitProbe {
    pub fn new(repos: Repos, device_id: impl Into<String>) -> Self {
        Self {
            repos,
            device_id: device_id.into(),
            roots: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[async_trait]
impl GitProbe for ReposGitProbe {
    fn device_id(&self) -> &str {
        &self.device_id
    }

    async fn path_kind(&self, path: &Path) -> PathKind {
        let probe = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::fs::metadata(path.to_path_buf()),
        )
        .await;
        match probe {
            Ok(Ok(meta)) if meta.is_dir() => PathKind::Directory,
            Ok(Ok(_)) => PathKind::File,
            _ => PathKind::Missing,
        }
    }

    async fn checkout(&self, path: &Path) -> Option<CheckoutFacts> {
        let identity = self.repos.checkout_identity(path).await.ok()?;
        let common = self
            .repos
            .git(
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
                Some(path),
            )
            .await
            .ok()?;
        Some(CheckoutFacts {
            top_level: identity.root,
            git_dir: identity.git_dir,
            common_dir: canonical(Path::new(&common)),
            checkout_id: identity.id,
        })
    }

    async fn root_commits(&self, path: &Path, common_dir: &Path) -> Vec<String> {
        let Ok(Some(head)) = self.repos.head_sha(path).await else {
            return Vec::new();
        };
        let key = (common_dir.to_path_buf(), head);
        if let Some(roots) = self
            .roots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return roots.clone();
        }
        let Ok(out) = self
            .repos
            .git(&["rev-list", "--max-parents=0", "HEAD"], Some(path))
            .await
        else {
            return Vec::new();
        };
        let mut roots: Vec<String> = out.lines().map(str::to_owned).collect();
        roots.sort();
        self.roots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, roots.clone());
        roots
    }

    async fn worktrees(&self, path: &Path) -> Vec<WorktreeFacts> {
        // `git()` trims stdout; the NUL-separated stanzas survive that.
        let Ok(raw) = self
            .repos
            .git(&["worktree", "list", "--porcelain", "-z"], Some(path))
            .await
        else {
            return Vec::new();
        };
        parse_worktree_porcelain_z(&raw)
            .into_iter()
            .map(|record| WorktreeFacts {
                path: record.path,
                branch: record.branch,
                head_sha: record.head_sha,
                prunable: record.prunable,
                locked: record.locked,
            })
            .collect()
    }

    async fn head(&self, path: &Path) -> (Option<String>, Option<String>) {
        let branch = self
            .repos
            .current_branch(path)
            .await
            .ok()
            .filter(|b| b != "HEAD");
        let sha = self.repos.head_sha(path).await.ok().flatten();
        (branch, sha)
    }

    async fn main_checkout(&self, common_dir: &Path) -> Option<PathBuf> {
        if common_dir.file_name().is_some_and(|name| name == ".git") {
            return common_dir.parent().map(canonical);
        }
        let config = common_dir.join("config");
        let config = config.to_string_lossy().into_owned();
        let worktree = self
            .repos
            .git(
                &["config", "--file", &config, "--get", "core.worktree"],
                None,
            )
            .await
            .ok()?;
        (!worktree.is_empty()).then(|| canonical(&common_dir.join(worktree)))
    }
}

// ── path helpers ────────────────────────────────────────────────────────────

fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Canonical form of a path that may not exist: the longest existing
/// ancestor is canonicalized (so `/var/…` and `/private/var/…` compare equal
/// on macOS) and the missing tail is appended lexically.
pub(crate) fn canonical_lenient(path: &Path) -> PathBuf {
    let path = lexical(path);
    if let Ok(found) = std::fs::canonicalize(&path) {
        return found;
    }
    let mut tail = Vec::new();
    let mut cursor = path.as_path();
    while let Some(parent) = cursor.parent() {
        if let Some(name) = cursor.file_name() {
            tail.push(name.to_os_string());
        }
        if let Ok(mut found) = std::fs::canonicalize(parent) {
            for name in tail.iter().rev() {
                found.push(name);
            }
            return found;
        }
        cursor = parent;
    }
    path
}

/// Whether `path` (canonical) is the user's home directory, including
/// symlink aliases of it. A folder inside HOME is not.
pub(crate) fn is_home_directory(path: &Path) -> bool {
    let home = crate::repos::home_dir();
    canonical_lenient(path) == canonical_lenient(&home)
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The topology repository id of a canonical common dir.
pub fn repository_id(device_id: &str, common_dir: &Path) -> String {
    stable_id("repo", device_id, common_dir)
}

fn looks_like_sha(value: &str) -> bool {
    (7..=64).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether the checkout's current state satisfies `expected`: a branch name
/// (optionally `refs/heads/…`) equal to the current branch, or a commit sha
/// (prefix) of HEAD.
fn ref_matches(expected: &str, branch: Option<&str>, head: Option<&str>) -> bool {
    let expected = expected.trim();
    let name = expected.strip_prefix("refs/heads/").unwrap_or(expected);
    if branch == Some(name) {
        return true;
    }
    looks_like_sha(expected)
        && head.is_some_and(|sha| {
            sha.to_ascii_lowercase()
                .starts_with(&expected.to_ascii_lowercase())
        })
}

fn found_ref(branch: Option<&str>, head: Option<&str>) -> String {
    match (branch, head) {
        (Some(branch), _) => branch.to_owned(),
        (None, Some(sha)) => format!("HEAD ({})", &sha[..sha.len().min(12)]),
        (None, None) => "no commit".to_owned(),
    }
}

// ── validation ──────────────────────────────────────────────────────────────

/// The checkout facts at `path` when `path` is that checkout's top level
/// (an uninitialized submodule directory resolves to its superproject and
/// therefore does NOT count as a repository).
async fn checkout_at(probe: &dyn GitProbe, path: &Path) -> Option<CheckoutFacts> {
    let facts = probe.checkout(path).await?;
    (facts.top_level == canonical_lenient(path)).then_some(facts)
}

/// Find where the checkout `worktree_id` of the repository lives now.
async fn find_checkout(
    probe: &dyn GitProbe,
    repo_path: &Path,
    common_dir: &Path,
    worktree_id: &str,
    exclude: &Path,
) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = probe
        .worktrees(repo_path)
        .await
        .into_iter()
        .filter(|w| !w.prunable)
        .map(|w| w.path)
        .collect();
    if let Some(main) = probe.main_checkout(common_dir).await {
        candidates.push(main);
    }
    for candidate in candidates {
        let candidate = canonical_lenient(&candidate);
        if candidate == exclude || probe.path_kind(&candidate).await != PathKind::Directory {
            continue;
        }
        if let Some(facts) = checkout_at(probe, &candidate).await
            && facts.checkout_id == worktree_id
        {
            return Some(candidate);
        }
    }
    None
}

/// Validate one pinned worktree. `Ok(facts)` of the checkout to run in.
async fn validate_worktree(
    probe: &dyn GitProbe,
    binding: &RepositoryBinding,
    repo_path: &Path,
    common_dir: &Path,
    worktree_id: &str,
    worktree_path: &str,
) -> Result<CheckoutFacts, ExecutionValidationError> {
    let expected = canonical_lenient(Path::new(worktree_path));
    let missing = || ExecutionValidationError::WorktreeMissing {
        repo_id: binding.repo_id.clone(),
        worktree_id: worktree_id.to_owned(),
        path: worktree_path.to_owned(),
    };
    let moved_or_missing = |found: Option<PathBuf>| match found {
        Some(found) => ExecutionValidationError::WorktreeMoved {
            repo_id: binding.repo_id.clone(),
            worktree_id: worktree_id.to_owned(),
            expected_path: worktree_path.to_owned(),
            found_path: display(&found),
        },
        None => missing(),
    };
    match probe.path_kind(&expected).await {
        PathKind::Directory => {
            let facts = checkout_at(probe, &expected).await;
            match facts {
                Some(facts) if facts.checkout_id == worktree_id => {
                    if facts.common_dir != common_dir {
                        return Err(ExecutionValidationError::RepositoryIdentityMismatch {
                            repo_id: binding.repo_id.clone(),
                            path: worktree_path.to_owned(),
                            found_repo_id: Some(repository_id(
                                probe.device_id(),
                                &facts.common_dir,
                            )),
                            expected_fingerprint: binding.repo_fingerprint.clone(),
                            found_fingerprint: None,
                        });
                    }
                    if !facts.is_main() {
                        let listed = probe
                            .worktrees(repo_path)
                            .await
                            .into_iter()
                            .find(|w| canonical_lenient(&w.path) == expected);
                        match listed {
                            Some(w) if w.prunable => {
                                return Err(ExecutionValidationError::WorktreePruned {
                                    repo_id: binding.repo_id.clone(),
                                    worktree_id: worktree_id.to_owned(),
                                    path: worktree_path.to_owned(),
                                });
                            }
                            Some(_) => {}
                            None => return Err(missing()),
                        }
                    }
                    Ok(facts)
                }
                // Something else lives at the path now: report where the
                // configured identity went, never run in the impostor.
                _ => Err(moved_or_missing(
                    find_checkout(probe, repo_path, common_dir, worktree_id, &expected).await,
                )),
            }
        }
        PathKind::File => Err(missing()),
        PathKind::Missing => {
            let listed = probe
                .worktrees(repo_path)
                .await
                .into_iter()
                .find(|w| canonical_lenient(&w.path) == expected);
            if listed.is_some_and(|w| w.prunable) {
                return Err(ExecutionValidationError::WorktreePruned {
                    repo_id: binding.repo_id.clone(),
                    worktree_id: worktree_id.to_owned(),
                    path: worktree_path.to_owned(),
                });
            }
            Err(moved_or_missing(
                find_checkout(probe, repo_path, common_dir, worktree_id, &expected).await,
            ))
        }
    }
}

/// Validate one repository binding; pushes errors, returns the validated
/// repository when it passed.
async fn validate_binding(
    probe: &dyn GitProbe,
    binding: &RepositoryBinding,
    root: Option<&Path>,
    caps: ExecutionHostCapabilities,
    errors: &mut Vec<ExecutionValidationError>,
) -> Option<ValidatedRepository> {
    let repo_path = canonical_lenient(Path::new(&binding.repo_path));
    let repo_missing = || ExecutionValidationError::RepositoryMissing {
        repo_id: binding.repo_id.clone(),
        path: binding.repo_path.clone(),
    };
    if probe.path_kind(&repo_path).await != PathKind::Directory {
        errors.push(repo_missing());
        return None;
    }
    let Some(repo) = checkout_at(probe, &repo_path).await else {
        errors.push(repo_missing());
        return None;
    };
    let found_repo_id = repository_id(probe.device_id(), &repo.common_dir);
    if found_repo_id != binding.repo_id {
        errors.push(ExecutionValidationError::RepositoryIdentityMismatch {
            repo_id: binding.repo_id.clone(),
            path: binding.repo_path.clone(),
            found_repo_id: Some(found_repo_id),
            expected_fingerprint: binding.repo_fingerprint.clone(),
            found_fingerprint: None,
        });
        return None;
    }
    let roots = probe.root_commits(&repo_path, &repo.common_dir).await;
    let fingerprint_ok = if binding.repo_fingerprint.is_empty() {
        // Configured while the repository had no commits: it must still
        // have none (anything else is a different history).
        roots.is_empty()
    } else {
        roots.contains(&binding.repo_fingerprint)
    };
    if !fingerprint_ok {
        errors.push(ExecutionValidationError::RepositoryIdentityMismatch {
            repo_id: binding.repo_id.clone(),
            path: binding.repo_path.clone(),
            found_repo_id: None,
            expected_fingerprint: binding.repo_fingerprint.clone(),
            found_fingerprint: Some(roots.first().cloned().unwrap_or_default()),
        });
        return None;
    }

    let checkout = match (&binding.worktree_id, &binding.worktree_path) {
        (Some(id), Some(path)) => {
            match validate_worktree(probe, binding, &repo_path, &repo.common_dir, id, path).await {
                Ok(facts) => facts,
                Err(error) => {
                    errors.push(error);
                    return None;
                }
            }
        }
        // Shape errors (half a worktree) were reported already.
        (Some(_), None) | (None, Some(_)) => return None,
        (None, None) => repo,
    };

    let mut ok = true;
    if let Some(root) = root
        && !caps.additional_directories
        && !checkout.top_level.starts_with(root)
    {
        errors.push(ExecutionValidationError::MemberOutsideRoot {
            repo_id: binding.repo_id.clone(),
            path: display(&checkout.top_level),
            execution_root: display(root),
        });
        ok = false;
    }
    let (branch, head) = probe.head(&checkout.top_level).await;
    if let Some(expected) = &binding.expected_ref
        && !ref_matches(expected, branch.as_deref(), head.as_deref())
    {
        errors.push(ExecutionValidationError::BranchMismatch {
            repo_id: binding.repo_id.clone(),
            path: display(&checkout.top_level),
            expected_ref: expected.clone(),
            found_ref: found_ref(branch.as_deref(), head.as_deref()),
        });
        ok = false;
    }
    ok.then(|| ValidatedRepository {
        repo_id: binding.repo_id.clone(),
        repo_path: display(&repo_path),
        checkout_path: display(&checkout.top_level),
        checkout_id: checkout.checkout_id.clone(),
        member: binding.member,
        primary: binding.primary,
        branch,
        head_sha: head,
        worktree_configured: binding.worktree_id.is_some(),
    })
}

/// Validate `resolved` on this device (`device_id`, the probe's device).
/// `space` is the live Space row (`None` = missing or deleted). Collects
/// every failure. Device and Space failures stop early: the paths belong to
/// another device (or to nothing) and probing them here would be
/// meaningless.
pub async fn validate(
    resolved: &ResolvedExecutionConfiguration,
    device_id: &str,
    space: Option<&Space>,
    caps: ExecutionHostCapabilities,
    probe: &dyn GitProbe,
) -> Result<ValidatedExecution, Vec<ExecutionValidationError>> {
    let mut errors = Vec::new();
    match space {
        None => errors.push(ExecutionValidationError::SpaceMissing {
            space_id: resolved.space_id.clone(),
        }),
        Some(space) if space.device_id != resolved.device_id => {
            errors.push(ExecutionValidationError::DeviceMismatch {
                expected_device_id: space.device_id.clone(),
                found_device_id: resolved.device_id.clone(),
            });
        }
        Some(_) => {}
    }
    if resolved.device_id != device_id {
        errors.push(ExecutionValidationError::DeviceMismatch {
            expected_device_id: resolved.device_id.clone(),
            found_device_id: device_id.to_owned(),
        });
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    let configuration = &resolved.configuration;
    // An undecodable row resolves to an empty placeholder: report only why.
    if configuration.execution_root.is_empty()
        && configuration.schema == 0
        && let Some(reason) = &resolved.invalid_reason
    {
        return Err(vec![ExecutionValidationError::ConfigInvalid {
            reason: reason.clone(),
        }]);
    }
    let shape = execution_configuration_shape_errors(configuration);
    if shape.is_empty()
        && let Some(reason) = &resolved.invalid_reason
    {
        errors.push(ExecutionValidationError::ConfigInvalid {
            reason: reason.clone(),
        });
    }
    errors.extend(shape);
    // A relative or empty root would be probed against the engine's own
    // working directory: never.
    if !is_absolute_device_path(&configuration.execution_root) {
        return Err(errors);
    }

    let root_input = Path::new(&configuration.execution_root);
    let root = match probe.path_kind(root_input).await {
        PathKind::Directory => Some(canonical_lenient(root_input)),
        PathKind::File => {
            errors.push(ExecutionValidationError::RootNotDirectory {
                path: configuration.execution_root.clone(),
            });
            None
        }
        PathKind::Missing => {
            errors.push(ExecutionValidationError::RootMissing {
                path: configuration.execution_root.clone(),
            });
            None
        }
    };
    // The user's home directory is never an execution root: an agent there
    // probes every personal folder, and dispatch treats HOME as projectless.
    if let Some(root) = &root
        && is_home_directory(root)
    {
        errors.push(ExecutionValidationError::HomeDirectoryRoot {
            path: configuration.execution_root.clone(),
        });
        return Err(errors);
    }
    let root_checkout_id = match &root {
        Some(root) => checkout_at(probe, root)
            .await
            .map(|facts| facts.checkout_id),
        None => None,
    };

    let mut repositories = Vec::with_capacity(configuration.bindings.len());
    for binding in &configuration.bindings {
        if let Some(validated) =
            validate_binding(probe, binding, root.as_deref(), caps, &mut errors).await
        {
            repositories.push(validated);
        }
    }

    // A launch into a chosen worktree uses that worktree as its root: when
    // the worktree itself is reported (pruned, moved, missing), that is the
    // actionable error, not the generic "root missing".
    let root_is_reported_worktree = |errors: &[ExecutionValidationError]| {
        errors.iter().any(|error| match error {
            ExecutionValidationError::WorktreeMissing { path, .. }
            | ExecutionValidationError::WorktreePruned { path, .. } => {
                canonical_lenient(Path::new(path)) == canonical_lenient(root_input)
            }
            ExecutionValidationError::WorktreeMoved { expected_path, .. } => {
                canonical_lenient(Path::new(expected_path)) == canonical_lenient(root_input)
            }
            _ => false,
        })
    };
    if root.is_none() && root_is_reported_worktree(&errors) {
        errors.retain(|error| !matches!(error, ExecutionValidationError::RootMissing { .. }));
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let Some(root) = root else {
        return Err(errors);
    };
    Ok(ValidatedExecution {
        space_id: resolved.space_id.clone(),
        device_id: resolved.device_id.clone(),
        execution_root: display(&root),
        root_checkout_id,
        repositories,
        source_scope: resolved.source_scope.clone(),
    })
}

/// The identity a binding should record for the repository whose checkout
/// top level is `path` (forwardable `DescribeExecutionRepository`).
pub async fn describe_repository(
    probe: &dyn GitProbe,
    path: &Path,
) -> Result<ExecutionRepositoryIdentity, EngineError> {
    let requested = canonical_lenient(path);
    let facts = checkout_at(probe, &requested).await.ok_or_else(|| {
        EngineError::Other(format!(
            "{} is not the top level of a Git checkout",
            path.display()
        ))
    })?;
    let main = probe.main_checkout(&facts.common_dir).await;
    let repo_path = if facts.is_main() {
        facts.top_level.clone()
    } else {
        main.clone().unwrap_or_else(|| facts.top_level.clone())
    };
    let roots = probe.root_commits(&repo_path, &facts.common_dir).await;
    let mut worktrees = Vec::new();
    for (index, record) in probe.worktrees(&repo_path).await.into_iter().enumerate() {
        // The first stanza is the main worktree; inside a submodule git
        // reports it as the module git dir, so use the resolved main path.
        let path = if index == 0 {
            main.clone()
                .unwrap_or_else(|| canonical_lenient(&record.path))
        } else {
            canonical_lenient(&record.path)
        };
        let checkout = if record.prunable {
            None
        } else {
            checkout_at(probe, &path).await
        };
        worktrees.push(ExecutionWorktreeIdentity {
            path: display(&path),
            main: checkout.as_ref().is_some_and(CheckoutFacts::is_main),
            checkout_id: checkout.map(|facts| facts.checkout_id),
            branch: record.branch,
            head_sha: record.head_sha,
            prunable: record.prunable,
            locked: record.locked,
        });
    }
    Ok(ExecutionRepositoryIdentity {
        repo_id: repository_id(probe.device_id(), &facts.common_dir),
        repo_fingerprint: roots.first().cloned().unwrap_or_default(),
        repo_path: display(&repo_path),
        checkout_id: facts.checkout_id.clone(),
        main_checkout: facts.is_main(),
        worktrees,
    })
}

/// Render validation errors as one dispatch rejection message:
/// `execution-scope:<code>: <message> <remediation>` for the first error,
/// then ` (+N more)`.
pub fn rejection_message(errors: &[ExecutionValidationError]) -> String {
    let Some(first) = errors.first() else {
        return format!("{REJECTION_PREFIX}unknown: execution validation failed");
    };
    let mut message = format!(
        "{REJECTION_PREFIX}{}: {}. {}",
        first.code(),
        first.message(),
        first.remediation()
    );
    if errors.len() > 1 {
        message.push_str(&format!(" (+{} more)", errors.len() - 1));
    }
    message
}

// ── service ─────────────────────────────────────────────────────────────────

/// Validation over the registry replica and this device's repositories.
/// Cheap to clone; one per engine (and one per `DocHost` in stage 2).
#[derive(Clone)]
pub struct ExecutionScopeService {
    workspace: WorkspaceHost,
    probe: Arc<dyn GitProbe>,
}

impl ExecutionScopeService {
    pub fn new(workspace: WorkspaceHost, repos: Repos) -> Self {
        let device_id = workspace.device_id().to_owned();
        Self {
            workspace,
            probe: Arc::new(ReposGitProbe::new(repos, device_id)),
        }
    }

    /// With an explicit probe (tests).
    pub fn with_probe(workspace: WorkspaceHost, probe: Arc<dyn GitProbe>) -> Self {
        Self { workspace, probe }
    }

    pub fn device_id(&self) -> &str {
        self.workspace.device_id()
    }

    pub fn probe(&self) -> &dyn GitProbe {
        self.probe.as_ref()
    }

    /// Validate a snapshot on this device against the current Space row.
    pub async fn validate_resolved(
        &self,
        resolved: &ResolvedExecutionConfiguration,
        caps: ExecutionHostCapabilities,
    ) -> Result<ValidatedExecution, Vec<ExecutionValidationError>> {
        let space = self.workspace.space(&resolved.space_id).ok().flatten();
        validate(
            resolved,
            self.workspace.device_id(),
            space.as_ref(),
            caps,
            self.probe.as_ref(),
        )
        .await
    }

    /// The dispatch-time gate stage 2 wires into the start workflow, host
    /// `execute(Run)` (before `claim_chat` and worktree materialization),
    /// and `DocHost::dispatch_with_source_context`.
    ///
    /// - `Ok(None)`: the Chat has no captured binding (today's unbound
    ///   behavior applies unchanged).
    /// - `Ok(Some(validated))`: run with `cwd = validated.execution_root`.
    /// - `Err(errors)`: reject the command ([`rejection_message`]); never
    ///   fall back to another directory.
    ///
    /// The Chat must be hosted on this device and live in the binding's
    /// Space.
    pub async fn validate_for_dispatch(
        &self,
        chat_id: &str,
        caps: ExecutionHostCapabilities,
    ) -> Result<Option<ValidatedExecution>, Vec<ExecutionValidationError>> {
        let Some(binding) = self
            .workspace
            .chat_execution_binding_state(chat_id)
            .checked()
            .map_err(|error| vec![error])?
        else {
            return Ok(None);
        };
        let chat = self.workspace.chat(chat_id).ok().flatten();
        let mut errors = Vec::new();
        match &chat {
            None => errors.push(ExecutionValidationError::ConfigInvalid {
                reason: format!("Chat {chat_id} no longer exists"),
            }),
            Some(chat) => {
                if chat.device_id != binding.resolved.device_id {
                    errors.push(ExecutionValidationError::DeviceMismatch {
                        expected_device_id: binding.resolved.device_id.clone(),
                        found_device_id: chat.device_id.clone(),
                    });
                }
                if chat.space_id.as_deref() != Some(binding.space_id.as_str()) {
                    errors.push(ExecutionValidationError::ConfigInvalid {
                        reason: "the Chat moved to another Space after its binding was captured"
                            .into(),
                    });
                }
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        self.validate_resolved(&binding.resolved, caps)
            .await
            .map(Some)
    }
}

#[cfg(test)]
mod tests;
