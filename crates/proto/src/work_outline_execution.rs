//! Work Outline execution configuration (Milestone 3): wire types for the
//! `executionConfigurations` and `chatExecutionBindings` registry kinds, the
//! `configureExecution` mutation, the `resolveExecutionConfiguration` query,
//! the `ValidateExecutionConfiguration` method, and the PURE functions every
//! layer shares:
//!
//! - [`check_execution_configuration`]: the write-time shape check (sizes,
//!   absolute paths, duplicate repositories, one primary at most);
//! - [`resolve_execution_configuration`]: whole-row resolution over the
//!   materialized (repaired) tree: nearest work item with a row, then its
//!   ancestors, then the Space default, then the implicit
//!   `{ executionRoot: Space.path }`.
//!
//! Binding references: `docs/plans/work-outline/m0-contract.md` section 6,
//! `m0-execution-scope.md` sections 4-5, `m0-registry-persistence.md` 7.4.
//! Wire conventions match `work_outline.rs`: camelCase fields, defaulted
//! non-identity fields, lenient snapshot enums (unknown variants read as
//! `unknown`), strict request enums.
//!
//! Validation itself needs the owning device's filesystem and git, so it
//! lives in the engine (`crates/engine/src/execution_scope.rs`); this module
//! only defines its result and error vocabulary. No variant is a fallback:
//! every failure blocks, and none names an alternative checkout to use.

use serde::{Deserialize, Serialize};

use crate::work_outline::{WorkItemView, WorkOutlineActor};

/// The only configuration schema this build writes and validates.
pub const EXECUTION_CONFIGURATION_SCHEMA: u32 = 1;

/// Budgets of one execution configuration (registry audit section 6).
pub mod execution_limits {
    /// Serialized `configuration` JSON, in UTF-16 units (JS `.length`).
    pub const MAX_CONFIGURATION_UTF16_UNITS: usize = 8_000;
    /// Repository bindings per configuration.
    pub const MAX_BINDINGS: usize = 16;
    /// Any path (execution root, repository, worktree).
    pub const MAX_PATH_CHARS: usize = 1_024;
    /// Repository id, worktree id, fingerprint.
    pub const MAX_ID_CHARS: usize = 256;
    /// `expectedRef` (branch name, `refs/heads/...`, or a commit sha).
    pub const MAX_REF_CHARS: usize = 256;
}

fn schema_v1() -> u32 {
    EXECUTION_CONFIGURATION_SCHEMA
}

/// The environment a work item's (or Space's) Chats run in: one execution
/// root (the harness cwd and writable workspace) plus repository bindings.
/// Stored whole in ONE registry field so a root can never pair with another
/// writer's bindings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionConfiguration {
    /// Absolute path on the Space's owning device.
    pub execution_root: String,
    #[serde(default)]
    pub bindings: Vec<RepositoryBinding>,
    #[serde(default = "schema_v1")]
    pub schema: u32,
}

impl ExecutionConfiguration {
    /// The implicit Space-root configuration (no row anywhere).
    pub fn implicit(space_path: impl Into<String>) -> Self {
        Self {
            execution_root: space_path.into(),
            bindings: Vec::new(),
            schema: EXECUTION_CONFIGURATION_SCHEMA,
        }
    }

    /// The primary binding, when exactly one is marked primary.
    pub fn primary(&self) -> Option<&RepositoryBinding> {
        let mut primaries = self.bindings.iter().filter(|b| b.primary);
        let first = primaries.next()?;
        primaries.next().is_none().then_some(first)
    }
}

/// One repository of an execution configuration.
///
/// Identity is the topology repository id (`repo:` + sha256(device NUL
/// canonical git common dir)) plus `repoFingerprint`, the root commit of the
/// repository's history: the id is path-derived, so only the fingerprint
/// tells a re-cloned repository at the same path apart.
///
/// `worktreeId` + `worktreePath` (both or neither) pin the exact checkout
/// (`CheckoutIdentity.id` = sha256(device NUL canonical git dir)); without
/// them the repository's own checkout at `repoPath` is used and the launch
/// flow offers a worktree choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryBinding {
    pub repo_id: String,
    /// Root commit sha; empty for a repository without commits.
    #[serde(default)]
    pub repo_fingerprint: String,
    /// Canonical main-checkout path (locator and display).
    pub repo_path: String,
    /// Included in diffs and repository context.
    #[serde(default)]
    pub member: bool,
    /// The focus repository (at most one).
    #[serde(default)]
    pub primary: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    /// Branch (`main`, `refs/heads/main`) or commit sha the checkout must be
    /// on. Zeron never switches to it; a mismatch blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_ref: Option<String>,
}

impl RepositoryBinding {
    /// The path the binding runs in: the pinned worktree, else `repoPath`.
    pub fn checkout_path(&self) -> &str {
        self.worktree_path.as_deref().unwrap_or(&self.repo_path)
    }
}

/// Which row a configuration is written to. Internally tagged by `type`:
/// `{"type":"space","spaceId":"…"}` or `{"type":"workItem","workItemId":"…"}`.
/// Strict (request enum).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ExecutionScope {
    #[serde(rename_all = "camelCase")]
    Space { space_id: String },
    #[serde(rename_all = "camelCase")]
    WorkItem { work_item_id: String },
}

/// Row-id prefix of a Space default configuration.
pub const SPACE_SCOPE_PREFIX: &str = "s:";
/// Row-id prefix of a work-item configuration.
pub const WORK_ITEM_SCOPE_PREFIX: &str = "w:";

impl ExecutionScope {
    /// The `executionConfigurations` row id: `s:{spaceId}` or `w:{workItemId}`.
    pub fn row_id(&self) -> String {
        match self {
            Self::Space { space_id } => format!("{SPACE_SCOPE_PREFIX}{space_id}"),
            Self::WorkItem { work_item_id } => format!("{WORK_ITEM_SCOPE_PREFIX}{work_item_id}"),
        }
    }

    /// Inverse of [`Self::row_id`].
    pub fn from_row_id(id: &str) -> Option<Self> {
        if let Some(space_id) = id.strip_prefix(SPACE_SCOPE_PREFIX) {
            return (!space_id.is_empty()).then(|| Self::Space {
                space_id: space_id.to_owned(),
            });
        }
        let work_item_id = id.strip_prefix(WORK_ITEM_SCOPE_PREFIX)?;
        (!work_item_id.is_empty()).then(|| Self::WorkItem {
            work_item_id: work_item_id.to_owned(),
        })
    }
}

/// Where a resolved configuration came from. Internally tagged by `type`.
/// Lenient: an unknown variant reads as `unknown`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ExecutionSourceScope {
    /// The row of this work item (the requested one or an ancestor).
    #[serde(rename_all = "camelCase")]
    WorkItem { work_item_id: String },
    /// The Space default row.
    #[serde(rename_all = "camelCase")]
    SpaceDefault { space_id: String },
    /// No row anywhere: `{ executionRoot: Space.path }`.
    #[serde(rename_all = "camelCase")]
    ImplicitSpaceRoot { space_id: String },
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

/// A configuration chosen for a scope, snapshotted with the Space's owning
/// device. This is what a Chat captures at launch and what validation reads
/// (never a later registry read: the command plane can outrun the registry).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedExecutionConfiguration {
    pub space_id: String,
    /// The Space's owning device (immutable `Space.deviceId`).
    #[serde(default)]
    pub device_id: String,
    pub configuration: ExecutionConfiguration,
    pub source_scope: ExecutionSourceScope,
    /// Set when the winning row could not be decoded or fails the shape
    /// check; validation then fails with `configInvalid` (fail closed: a
    /// broken nearer row never falls through to an ancestor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_reason: Option<String>,
}

/// The Space facts the resolver needs for the implicit root.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionSpace {
    pub space_id: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub device_id: String,
}

/// One materialized `executionConfigurations` row of a Space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionConfigurationView {
    /// Row id (`s:{spaceId}` / `w:{workItemId}`).
    pub id: String,
    pub space_id: String,
    /// `None` for the Space default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    /// `None` when the stored JSON could not be decoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<ExecutionConfiguration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<WorkOutlineActor>,
    #[serde(default)]
    pub updated_at: i64,
}

impl ExecutionConfigurationView {
    pub fn scope(&self) -> Option<ExecutionScope> {
        ExecutionScope::from_row_id(&self.id)
    }
}

/// The configuration a Chat captured at launch (`chatExecutionBindings`,
/// keyed by Chat id, written once). Later configuration edits never retarget
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatExecutionBindingView {
    pub chat_id: String,
    pub space_id: String,
    pub resolved: ResolvedExecutionConfiguration,
    #[serde(default)]
    pub captured_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_by: Option<WorkOutlineActor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// The work item the Chat is associated with now (snapshot only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    /// The captured configuration differs from the associated item's current
    /// resolved configuration (snapshot only; `false` when unassociated).
    #[serde(default)]
    pub exception: bool,
}

/// Per-item exception marker: associated Chats whose captured binding
/// differs from the item's current resolved configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemExecutionException {
    pub work_item_id: String,
    /// Sorted.
    #[serde(default)]
    pub chat_ids: Vec<String>,
}

// --------------------------------------------------------------------------
// Requests
// --------------------------------------------------------------------------

/// `configureExecution`: write (`configuration`) or reset (`null`) the row of
/// one scope. Shape-only: paths may live on another device, so nothing here
/// touches the filesystem. Reply `{}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigureExecutionParams {
    pub scope: ExecutionScope,
    /// `null` resets the scope (tombstones its row).
    #[serde(default)]
    pub configuration: Option<ExecutionConfiguration>,
    pub operation_id: String,
}

/// Host facts that relax validation. Zeron never widens the sandbox on its
/// own (contract 6.4.3): `additionalDirectories` is true only when the
/// harness that will run can be given member paths outside the root.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionHostCapabilities {
    #[serde(default)]
    pub additional_directories: bool,
}

/// Params of `ValidateExecutionConfiguration` (forwardable). Exactly one of
/// `scope`, `chatId` (its captured binding), or `resolved` (a snapshot the
/// caller already resolved; how a forwarded call carries it) is set. Runs on
/// the Space's owning device; another device forwards it there when the
/// owner advertises `work-outline-multi-repo-v1`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateExecutionConfigurationParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ExecutionScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<ResolvedExecutionConfiguration>,
    #[serde(default)]
    pub host_capabilities: ExecutionHostCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<String>,
}

/// Params of `DescribeExecutionRepository` (forwardable): the identity a
/// binding should record for the repository at `path` on its device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DescribeExecutionRepositoryParams {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<String>,
}

/// Identity of one repository on its device, plus its checkouts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionRepositoryIdentity {
    pub repo_id: String,
    #[serde(default)]
    pub repo_fingerprint: String,
    /// Canonical top level of the checkout at the requested path.
    pub repo_path: String,
    /// `CheckoutIdentity.id` of the checkout at the requested path.
    #[serde(default)]
    pub checkout_id: String,
    /// The requested path is the repository's main checkout
    /// (`git-dir == git-common-dir`).
    #[serde(default)]
    pub main_checkout: bool,
    /// Every registered checkout of the repository.
    #[serde(default)]
    pub worktrees: Vec<ExecutionWorktreeIdentity>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionWorktreeIdentity {
    pub path: String,
    /// Absent when the checkout cannot be resolved (for example prunable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default)]
    pub main: bool,
    #[serde(default)]
    pub prunable: bool,
    #[serde(default)]
    pub locked: bool,
}

// --------------------------------------------------------------------------
// Validation results
// --------------------------------------------------------------------------

/// One reason a configuration cannot run. Internally tagged by `code`.
/// Lenient: an unknown code reads as `unknown`. No variant names a fallback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "camelCase")]
pub enum ExecutionValidationError {
    /// The configuration belongs to another device than the one validating
    /// (or the Space's owner disagrees with the snapshot).
    #[serde(rename_all = "camelCase")]
    DeviceMismatch {
        expected_device_id: String,
        found_device_id: String,
    },
    /// The owning device cannot be reached.
    #[serde(rename_all = "camelCase")]
    DeviceOffline {
        device_id: String,
        #[serde(default)]
        detail: String,
    },
    /// The owning device runs a build without multi-repository validation.
    #[serde(rename_all = "camelCase")]
    HostUnsupported {
        device_id: String,
        capability: String,
    },
    #[serde(rename_all = "camelCase")]
    SpaceMissing { space_id: String },
    #[serde(rename_all = "camelCase")]
    RootMissing { path: String },
    #[serde(rename_all = "camelCase")]
    RootNotDirectory { path: String },
    #[serde(rename_all = "camelCase")]
    RepositoryMissing { repo_id: String, path: String },
    /// The repository at the path is not the configured one (other common
    /// dir, or a different history: fingerprint mismatch).
    #[serde(rename_all = "camelCase")]
    RepositoryIdentityMismatch {
        repo_id: String,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        found_repo_id: Option<String>,
        #[serde(default)]
        expected_fingerprint: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        found_fingerprint: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    WorktreeMissing {
        repo_id: String,
        worktree_id: String,
        path: String,
    },
    /// The worktree directory is gone but git still lists it (prunable).
    #[serde(rename_all = "camelCase")]
    WorktreePruned {
        repo_id: String,
        worktree_id: String,
        path: String,
    },
    /// The worktree identity now lives at another path (`git worktree
    /// move`). Reported, never followed.
    #[serde(rename_all = "camelCase")]
    WorktreeMoved {
        repo_id: String,
        worktree_id: String,
        expected_path: String,
        found_path: String,
    },
    #[serde(rename_all = "camelCase")]
    BranchMismatch {
        repo_id: String,
        path: String,
        expected_ref: String,
        /// Current branch, or `HEAD` (+ sha) when detached.
        found_ref: String,
    },
    /// A bound checkout lies outside the execution root and the harness
    /// cannot be given additional directories.
    #[serde(rename_all = "camelCase")]
    MemberOutsideRoot {
        repo_id: String,
        path: String,
        execution_root: String,
    },
    #[serde(rename_all = "camelCase")]
    MultiplePrimary { repo_ids: Vec<String> },
    #[serde(rename_all = "camelCase")]
    ConfigInvalid { reason: String },
    /// A bound Chat's stored native session ran in another directory (or in
    /// an unknown one), so resuming it would continue a conversation from
    /// elsewhere. Never converted into a silent fresh session.
    #[serde(rename_all = "camelCase")]
    ResumeRootMismatch {
        expected_path: String,
        #[serde(default)]
        found_path: String,
    },
    /// Something tried to move a bound Chat to another directory (a cwd
    /// change, a worktree directive, a new worktree). A bound Chat keeps the
    /// execution root it captured at launch.
    #[serde(rename_all = "camelCase")]
    BoundChatRetarget {
        chat_id: String,
        execution_root: String,
        #[serde(default)]
        requested_path: String,
    },
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

impl ExecutionValidationError {
    /// The wire code (`worktreeMissing`, ...).
    pub fn code(&self) -> &'static str {
        match self {
            Self::DeviceMismatch { .. } => "deviceMismatch",
            Self::DeviceOffline { .. } => "deviceOffline",
            Self::HostUnsupported { .. } => "hostUnsupported",
            Self::SpaceMissing { .. } => "spaceMissing",
            Self::RootMissing { .. } => "rootMissing",
            Self::RootNotDirectory { .. } => "rootNotDirectory",
            Self::RepositoryMissing { .. } => "repositoryMissing",
            Self::RepositoryIdentityMismatch { .. } => "repositoryIdentityMismatch",
            Self::WorktreeMissing { .. } => "worktreeMissing",
            Self::WorktreePruned { .. } => "worktreePruned",
            Self::WorktreeMoved { .. } => "worktreeMoved",
            Self::BranchMismatch { .. } => "branchMismatch",
            Self::MemberOutsideRoot { .. } => "memberOutsideRoot",
            Self::MultiplePrimary { .. } => "multiplePrimary",
            Self::ConfigInvalid { .. } => "configInvalid",
            Self::ResumeRootMismatch { .. } => "resumeRootMismatch",
            Self::BoundChatRetarget { .. } => "boundChatRetarget",
            Self::Unknown => "unknown",
        }
    }

    /// One sentence saying what is wrong.
    pub fn message(&self) -> String {
        match self {
            Self::DeviceMismatch {
                expected_device_id,
                found_device_id,
            } => format!(
                "This execution configuration belongs to device {expected_device_id}, not \
                 {found_device_id}"
            ),
            Self::DeviceOffline { device_id, detail } if detail.is_empty() => {
                format!("Device {device_id} is offline")
            }
            Self::DeviceOffline { device_id, detail } => {
                format!("Device {device_id} is offline ({detail})")
            }
            Self::HostUnsupported {
                device_id,
                capability,
            } => format!("Device {device_id} does not support {capability}"),
            Self::SpaceMissing { space_id } => format!("Space {space_id} no longer exists"),
            Self::RootMissing { path } => format!("The execution root {path} does not exist"),
            Self::RootNotDirectory { path } => {
                format!("The execution root {path} is not a folder")
            }
            Self::RepositoryMissing { path, .. } => {
                format!("The repository at {path} is missing")
            }
            Self::RepositoryIdentityMismatch {
                path,
                found_repo_id,
                expected_fingerprint,
                found_fingerprint,
                ..
            } => {
                if found_repo_id.is_some() {
                    format!("{path} now holds a different repository")
                } else if found_fingerprint.as_deref() != Some(expected_fingerprint.as_str()) {
                    format!("{path} holds a repository with a different history (re-cloned?)")
                } else {
                    format!("{path} is not the configured repository")
                }
            }
            Self::WorktreeMissing { path, .. } => {
                format!("The configured worktree {path} is missing")
            }
            Self::WorktreePruned { path, .. } => format!(
                "The configured worktree {path} was deleted; Git still lists it as prunable"
            ),
            Self::WorktreeMoved {
                expected_path,
                found_path,
                ..
            } => format!("The configured worktree moved from {expected_path} to {found_path}"),
            Self::BranchMismatch {
                path,
                expected_ref,
                found_ref,
                ..
            } => format!("{path} is on {found_ref}, not the expected {expected_ref}"),
            Self::MemberOutsideRoot {
                path,
                execution_root,
                ..
            } => format!("{path} is outside the execution root {execution_root}"),
            Self::MultiplePrimary { repo_ids } => format!(
                "{} repositories are marked primary; only one can be",
                repo_ids.len()
            ),
            Self::ConfigInvalid { reason } => {
                format!("The execution configuration is invalid: {reason}")
            }
            Self::ResumeRootMismatch {
                expected_path,
                found_path,
            } if found_path.is_empty() => format!(
                "This Chat's previous session has no recorded folder, so it cannot be resumed in \
                 {expected_path}"
            ),
            Self::ResumeRootMismatch {
                expected_path,
                found_path,
            } => format!(
                "This Chat's previous session ran in {found_path}, not its execution root \
                 {expected_path}"
            ),
            Self::BoundChatRetarget {
                execution_root,
                requested_path,
                ..
            } if requested_path.is_empty() => format!(
                "This Chat is bound to its execution root {execution_root} and cannot get a new \
                 worktree"
            ),
            Self::BoundChatRetarget {
                execution_root,
                requested_path,
                ..
            } => format!(
                "This Chat is bound to its execution root {execution_root} and cannot move to \
                 {requested_path}"
            ),
            Self::Unknown => "The execution configuration cannot be used".into(),
        }
    }

    /// What the user can do about it (never "use another checkout").
    pub fn remediation(&self) -> &'static str {
        match self {
            Self::DeviceMismatch { .. } => {
                "Start or resume this Chat from the device that owns the Space."
            }
            Self::DeviceOffline { .. } => {
                "Bring the owning device online, then try again. Zeron will not run the work \
                 elsewhere."
            }
            Self::HostUnsupported { .. } => "Update Zeron on the owning device, then try again.",
            Self::SpaceMissing { .. } => {
                "Recreate or re-add the Space, then configure execution again."
            }
            Self::RootMissing { .. } | Self::RootNotDirectory { .. } => {
                "Restore the folder or choose another execution root in the execution \
                 configuration."
            }
            Self::RepositoryMissing { .. } => {
                "Restore the repository at its path or update the repository binding."
            }
            Self::RepositoryIdentityMismatch { .. } => {
                "Re-bind the repository in the execution configuration if this is the \
                 repository you want, or restore the original one."
            }
            Self::WorktreeMissing { .. } => {
                "Recreate the worktree, select another worktree, or choose the repository's main \
                 checkout in the execution configuration."
            }
            Self::WorktreePruned { .. } => {
                "Run `git worktree prune` and recreate the worktree, or select another worktree \
                 in the execution configuration."
            }
            Self::WorktreeMoved { .. } => {
                "Update the worktree path in the execution configuration, or move the worktree \
                 back."
            }
            Self::BranchMismatch { .. } => {
                "Switch the checkout to the expected branch yourself, or update the expected \
                 branch in the execution configuration. Zeron never switches branches."
            }
            Self::MemberOutsideRoot { .. } => {
                "Move the worktree under the execution root, widen the execution root, or use a \
                 harness that supports additional directories."
            }
            Self::MultiplePrimary { .. } => {
                "Mark exactly one repository as primary in the execution configuration."
            }
            Self::ConfigInvalid { .. } | Self::Unknown => {
                "Edit or reset the execution configuration."
            }
            Self::ResumeRootMismatch { .. } => {
                "Start a new Chat from the work item to begin a fresh session in the execution \
                 root. Zeron will not resume or restart this conversation elsewhere."
            }
            Self::BoundChatRetarget { .. } => {
                "Start a new Chat from the work item with the folder or worktree you want; this \
                 Chat keeps the execution root it started with."
            }
        }
    }

    /// The error with its rendered texts (the wire form of one issue).
    pub fn report(self) -> ExecutionValidationReport {
        ExecutionValidationReport {
            message: self.message(),
            remediation: self.remediation().to_owned(),
            error: self,
        }
    }
}

/// One validation failure with its rendered message and remediation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionValidationReport {
    pub error: ExecutionValidationError,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub remediation: String,
}

/// One validated repository: where to run and what it is on right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidatedRepository {
    pub repo_id: String,
    pub repo_path: String,
    /// Canonical checkout root (the pinned worktree or the main checkout):
    /// the `cwd` of this member's diff watch.
    pub checkout_path: String,
    /// `CheckoutIdentity.id`: the `checkoutId` of this member's diff watch.
    pub checkout_id: String,
    #[serde(default)]
    pub member: bool,
    #[serde(default)]
    pub primary: bool,
    /// `None` when HEAD is detached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    /// The binding pins an exact worktree (otherwise the launch flow offers
    /// the worktree choice).
    #[serde(default)]
    pub worktree_configured: bool,
}

/// A configuration that passed validation on its owning device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidatedExecution {
    pub space_id: String,
    pub device_id: String,
    /// Canonical execution root: the harness cwd.
    pub execution_root: String,
    /// Checkout identity of the root when it is a Git checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_checkout_id: Option<String>,
    /// Every binding, in configuration order.
    #[serde(default)]
    pub repositories: Vec<ValidatedRepository>,
    pub source_scope: ExecutionSourceScope,
}

/// Reply of `ValidateExecutionConfiguration`: `ok` with `validated`, or
/// every failure found (validation collects all errors, it does not stop at
/// the first).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateExecutionConfigurationReply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validated: Option<ValidatedExecution>,
    #[serde(default)]
    pub errors: Vec<ExecutionValidationReport>,
    /// The configuration that was validated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<ResolvedExecutionConfiguration>,
}

impl ValidateExecutionConfigurationReply {
    pub fn from_result(
        resolved: Option<ResolvedExecutionConfiguration>,
        result: Result<ValidatedExecution, Vec<ExecutionValidationError>>,
    ) -> Self {
        match result {
            Ok(validated) => Self {
                ok: true,
                validated: Some(validated),
                errors: Vec::new(),
                resolved,
            },
            Err(errors) => Self {
                ok: false,
                validated: None,
                errors: errors
                    .into_iter()
                    .map(ExecutionValidationError::report)
                    .collect(),
                resolved,
            },
        }
    }
}

// --------------------------------------------------------------------------
// Pure functions
// --------------------------------------------------------------------------

/// An absolute path on ANY device (the configuring device may run another
/// OS than the owner): `/…`, `X:\…`, `X:/…`, or `\\…`.
pub fn is_absolute_device_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || path.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/'))
}

fn check_path(what: &str, path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err(format!("{what} is required"));
    }
    if path.chars().count() > execution_limits::MAX_PATH_CHARS {
        return Err(format!(
            "{what} is limited to {} characters",
            execution_limits::MAX_PATH_CHARS
        ));
    }
    if !is_absolute_device_path(path) {
        return Err(format!("{what} must be an absolute path"));
    }
    Ok(())
}

fn check_short(what: &str, value: &str, required: bool, max: usize) -> Result<(), String> {
    if required && value.trim().is_empty() {
        return Err(format!("{what} is required"));
    }
    if value.chars().count() > max {
        return Err(format!("{what} is limited to {max} characters"));
    }
    Ok(())
}

/// The structural problems of a configuration, as validation errors
/// (`multiplePrimary` separately from `configInvalid`). Empty = well formed.
/// Shared by the writer (rejects) and the validator (blocks).
pub fn execution_configuration_shape_errors(
    configuration: &ExecutionConfiguration,
) -> Vec<ExecutionValidationError> {
    let mut errors = Vec::new();
    let mut invalid = |reason: String| {
        errors.push(ExecutionValidationError::ConfigInvalid { reason });
    };
    if configuration.schema != EXECUTION_CONFIGURATION_SCHEMA {
        invalid(format!(
            "schema {} is not supported (this build reads schema {})",
            configuration.schema, EXECUTION_CONFIGURATION_SCHEMA
        ));
    }
    if let Err(reason) = check_path("The execution root", &configuration.execution_root) {
        invalid(reason);
    }
    if configuration.bindings.len() > execution_limits::MAX_BINDINGS {
        invalid(format!(
            "at most {} repositories can be bound",
            execution_limits::MAX_BINDINGS
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for binding in &configuration.bindings {
        let label = if binding.repo_path.is_empty() {
            binding.repo_id.as_str()
        } else {
            binding.repo_path.as_str()
        };
        if let Err(reason) = check_short(
            "A repository id",
            &binding.repo_id,
            true,
            execution_limits::MAX_ID_CHARS,
        ) {
            invalid(reason);
        }
        if let Err(reason) = check_short(
            "A repository fingerprint",
            &binding.repo_fingerprint,
            false,
            execution_limits::MAX_ID_CHARS,
        ) {
            invalid(reason);
        }
        if let Err(reason) = check_path("A repository path", &binding.repo_path) {
            invalid(reason);
        }
        if !binding.repo_id.is_empty() && !seen.insert(binding.repo_id.as_str()) {
            invalid(format!("{label} is bound twice"));
        }
        match (&binding.worktree_id, &binding.worktree_path) {
            (None, None) => {}
            (Some(id), Some(path)) => {
                if let Err(reason) =
                    check_short("A worktree id", id, true, execution_limits::MAX_ID_CHARS)
                {
                    invalid(reason);
                }
                if let Err(reason) = check_path("A worktree path", path) {
                    invalid(reason);
                }
            }
            _ => invalid(format!(
                "{label}: a worktree needs both its id and its path"
            )),
        }
        if let Some(expected) = &binding.expected_ref
            && let Err(reason) = check_short(
                "An expected branch",
                expected,
                true,
                execution_limits::MAX_REF_CHARS,
            )
        {
            invalid(reason);
        }
    }
    let primaries: Vec<String> = configuration
        .bindings
        .iter()
        .filter(|b| b.primary)
        .map(|b| b.repo_id.clone())
        .collect();
    if primaries.len() > 1 {
        errors.push(ExecutionValidationError::MultiplePrimary {
            repo_ids: primaries,
        });
    }
    errors
}

/// Write-time check: the first structural problem as a sentence.
pub fn check_execution_configuration(configuration: &ExecutionConfiguration) -> Result<(), String> {
    match execution_configuration_shape_errors(configuration)
        .into_iter()
        .next()
    {
        None => Ok(()),
        Some(ExecutionValidationError::MultiplePrimary { .. }) => {
            Err("Only one repository can be the primary focus".into())
        }
        Some(ExecutionValidationError::ConfigInvalid { reason }) => Err(reason),
        Some(other) => Err(other.message()),
    }
}

/// Whole-row resolution (contract 6.1). `items` are materialized views
/// (their `parentWorkItemId` is the repaired parent, so the walk cannot
/// loop; it is bounded anyway). `configurations` are this Space's rows.
///
/// - `Space` scope: the Space default row, else implicit.
/// - `WorkItem` scope: the item's row, else the nearest ancestor's, else the
///   Space default, else implicit. `None` when the item is not in `items` or
///   belongs to another Space.
///
/// The winning row is used AS A WHOLE (no field merge). A winning row that
/// failed to decode still wins and carries `invalidReason` (fail closed).
pub fn resolve_execution_configuration(
    scope: &ExecutionScope,
    space: &ExecutionSpace,
    items: &[WorkItemView],
    configurations: &[ExecutionConfigurationView],
) -> Option<ResolvedExecutionConfiguration> {
    let row = |id: &str| {
        configurations
            .iter()
            .find(|view| view.id == id && view.space_id == space.space_id)
    };
    let resolved = |view: &ExecutionConfigurationView, source: ExecutionSourceScope| {
        let configuration = view
            .configuration
            .clone()
            .unwrap_or_else(|| ExecutionConfiguration {
                execution_root: String::new(),
                bindings: Vec::new(),
                schema: 0,
            });
        let invalid_reason = view.invalid_reason.clone().or_else(|| {
            view.configuration
                .is_none()
                .then(|| "the stored configuration could not be read".to_owned())
        });
        ResolvedExecutionConfiguration {
            space_id: space.space_id.clone(),
            device_id: space.device_id.clone(),
            configuration,
            source_scope: source,
            invalid_reason,
        }
    };
    if let ExecutionScope::WorkItem { work_item_id } = scope {
        let index: std::collections::HashMap<&str, &WorkItemView> =
            items.iter().map(|item| (item.id.as_str(), item)).collect();
        let start = index.get(work_item_id.as_str())?;
        if start.space_id != space.space_id {
            return None;
        }
        let mut current = Some(*start);
        let mut steps = 0usize;
        while let Some(item) = current {
            let id = ExecutionScope::WorkItem {
                work_item_id: item.id.clone(),
            }
            .row_id();
            if let Some(view) = row(&id) {
                return Some(resolved(
                    view,
                    ExecutionSourceScope::WorkItem {
                        work_item_id: item.id.clone(),
                    },
                ));
            }
            steps += 1;
            if steps > items.len() {
                break;
            }
            current = item
                .parent_work_item_id
                .as_deref()
                .and_then(|parent| index.get(parent).copied())
                .filter(|parent| parent.space_id == space.space_id);
        }
    }
    let space_row = ExecutionScope::Space {
        space_id: space.space_id.clone(),
    }
    .row_id();
    if let Some(view) = row(&space_row) {
        return Some(resolved(
            view,
            ExecutionSourceScope::SpaceDefault {
                space_id: space.space_id.clone(),
            },
        ));
    }
    Some(ResolvedExecutionConfiguration {
        space_id: space.space_id.clone(),
        device_id: space.device_id.clone(),
        configuration: ExecutionConfiguration::implicit(space.path.clone()),
        source_scope: ExecutionSourceScope::ImplicitSpaceRoot {
            space_id: space.space_id.clone(),
        },
        invalid_reason: None,
    })
}

#[cfg(test)]
mod tests;
