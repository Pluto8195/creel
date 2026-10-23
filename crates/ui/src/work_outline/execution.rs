//! Execution configuration presentation (Milestone 3): pure, GUI-free logic
//! behind the Work Item Details **Execution** section, the exception markers,
//! and the per-member branch/diff selector of bound Chats.
//!
//! Binding references: `docs/plans/work-outline/m0-contract.md` section 6 and
//! "Milestone 3 execution discoveries". Resolution, the shape check, and the
//! error vocabulary (`message()` / `remediation()`) come from `zeron_proto`;
//! nothing here re-implements them.
use std::collections::BTreeSet;

use gpui::SharedString;
use zeron_proto::{
    ChatExecutionBindingView, ExecutionConfiguration, ExecutionConfigurationView,
    ExecutionRepositoryIdentity, ExecutionScope, ExecutionSourceScope, ExecutionValidationError,
    ExecutionWorktreeIdentity, RepositoryBinding, ResolvedExecutionConfiguration,
    ValidateExecutionConfigurationReply, ValidatedRepository, WorkOutlineError,
    resolve_execution_configuration,
};

use super::actions::{OperationSlot, is_unsupported};
use super::store::WorkOutlineSnapshot;

/// A folder pick on the Space's owning device (the Shell's New-project
/// browser in pick mode, `Shell::open_folder_pick`). `on_pick` receives the
/// absolute path of the folder the user confirmed; closing the browser any
/// other way calls nothing.
pub(crate) struct ExecutionFolderPick {
    pub title: SharedString,
    pub confirm_label: SharedString,
    pub device_id: String,
    pub start_path: Option<String>,
    /// Refuse to confirm the device's home directory itself (execution
    /// roots: validation rejects HOME with `homeDirectoryRoot`).
    pub reject_home_directory: bool,
    pub on_pick: Box<dyn FnOnce(String, &mut gpui::App) + 'static>,
}

/// Why the folder picker will not confirm the home directory as an
/// execution root (the validator's `homeDirectoryRoot` remediation).
pub(crate) const HOME_ROOT_REFUSAL: &str = "Your home directory can't be an execution root. Choose a project folder inside your home \
     directory.";

/// Whether `path` is the device's home directory: the home the browser
/// resolved when known, else the conventional `/Users/<name>`,
/// `/home/<name>`, `/root`, or `~` shapes (the browser may have opened at a
/// start path and never resolved home). Folders inside it are not.
pub(crate) fn is_device_home(path: &str, known_home: Option<&str>) -> bool {
    let path = path.trim_end_matches('/');
    if path == "~" {
        return true;
    }
    if let Some(home) = known_home.map(|home| home.trim_end_matches('/'))
        && !home.is_empty()
        && home == path
    {
        return true;
    }
    if path == "/root" {
        return true;
    }
    ["/Users/", "/home/"].iter().any(|prefix| {
        path.strip_prefix(prefix)
            .is_some_and(|name| !name.is_empty() && !name.contains('/'))
    })
}

/// What a folder pick is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FolderPickPurpose {
    ExecutionRoot,
    Repository,
}

/// The scope a details surface edits: the Space default for the Space-root
/// variant (`item_id` empty), else the work item.
pub(crate) fn scope_for(space_id: &str, item_id: &str) -> ExecutionScope {
    if item_id.is_empty() {
        ExecutionScope::Space {
            space_id: space_id.to_owned(),
        }
    } else {
        ExecutionScope::WorkItem {
            work_item_id: item_id.to_owned(),
        }
    }
}

/// The source scope a configuration written to `scope` reports.
pub(crate) fn source_of(scope: &ExecutionScope) -> ExecutionSourceScope {
    match scope {
        ExecutionScope::Space { space_id } => ExecutionSourceScope::SpaceDefault {
            space_id: space_id.clone(),
        },
        ExecutionScope::WorkItem { work_item_id } => ExecutionSourceScope::WorkItem {
            work_item_id: work_item_id.clone(),
        },
    }
}

/// Where the resolved configuration of `viewing` comes from, as a label:
/// "This item", "Inherited from <item>", "Space default", "Implicit Space
/// folder".
pub(crate) fn source_label(
    snapshot: &WorkOutlineSnapshot,
    viewing: &ExecutionScope,
    resolved: &ResolvedExecutionConfiguration,
) -> String {
    match &resolved.source_scope {
        ExecutionSourceScope::WorkItem { work_item_id } => match viewing {
            ExecutionScope::WorkItem { work_item_id: own } if own == work_item_id => {
                "This item".to_owned()
            }
            _ => {
                let title = snapshot
                    .item(work_item_id)
                    .map(|item| item.title.trim())
                    .filter(|title| !title.is_empty())
                    .unwrap_or("Untitled");
                format!("Inherited from {title}")
            }
        },
        ExecutionSourceScope::SpaceDefault { .. } => "Space default".to_owned(),
        ExecutionSourceScope::ImplicitSpaceRoot { .. } => "Implicit Space folder".to_owned(),
        ExecutionSourceScope::Unknown => "Unknown source".to_owned(),
    }
}

/// Whether `scope` has its own `executionConfigurations` row (Reset to
/// inherited applies only then).
pub(crate) fn owns_row(snapshot: &WorkOutlineSnapshot, scope: &ExecutionScope) -> bool {
    let id = scope.row_id();
    snapshot
        .execution_configurations
        .iter()
        .any(|view| view.id == id)
}

/// The last path segment (the repository or folder name).
pub(crate) fn path_name(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    trimmed
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(trimmed)
        .to_owned()
}

/// `branch`, or `HEAD (<sha7>)` when detached, or `HEAD`.
pub(crate) fn ref_label(branch: Option<&str>, head_sha: Option<&str>) -> String {
    match (branch.filter(|b| !b.is_empty()), head_sha) {
        (Some(branch), _) => branch.to_owned(),
        (None, Some(sha)) if !sha.is_empty() => {
            format!("HEAD ({})", sha.chars().take(7).collect::<String>())
        }
        _ => "HEAD".to_owned(),
    }
}

/// One-line description of a binding's role and checkout.
pub(crate) fn binding_summary(binding: &RepositoryBinding) -> String {
    let mut parts = Vec::new();
    match (binding.primary, binding.member) {
        (true, _) => parts.push("Primary".to_owned()),
        (false, true) => parts.push("Member".to_owned()),
        (false, false) => parts.push("Bound".to_owned()),
    }
    match &binding.worktree_path {
        Some(path) => parts.push(format!("worktree {}", path_name(path))),
        None => parts.push("repository path".to_owned()),
    }
    if let Some(expected) = binding.expected_ref.as_deref() {
        parts.push(format!("expects {expected}"));
    }
    parts.join(" \u{b7} ")
}

// --------------------------------------------------------------------------
// Draft
// --------------------------------------------------------------------------

/// One repository row of the edit form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BindingDraft {
    pub binding: RepositoryBinding,
    /// Checkouts offered for the worktree choice (from
    /// `DescribeExecutionRepository`; empty until described).
    pub checkouts: Vec<ExecutionWorktreeIdentity>,
    pub describing: bool,
    pub describe_error: Option<String>,
}

impl BindingDraft {
    pub(crate) fn existing(binding: RepositoryBinding) -> Self {
        Self {
            binding,
            checkouts: Vec::new(),
            describing: false,
            describe_error: None,
        }
    }

    /// Worktree choices: every resolvable, non-prunable checkout.
    pub(crate) fn worktree_choices(&self) -> Vec<&ExecutionWorktreeIdentity> {
        self.checkouts
            .iter()
            .filter(|checkout| checkout.checkout_id.is_some() && !checkout.prunable)
            .collect()
    }
}

/// A binding for the repository `identity` describes. `repoPath` is the
/// repository's main checkout; picking a linked worktree pins it.
pub(crate) fn binding_from_identity(identity: &ExecutionRepositoryIdentity) -> BindingDraft {
    let main_path = if identity.main_checkout {
        identity.repo_path.clone()
    } else {
        identity
            .worktrees
            .iter()
            .find(|checkout| checkout.main)
            .map(|checkout| checkout.path.clone())
            .unwrap_or_else(|| identity.repo_path.clone())
    };
    let (worktree_id, worktree_path) = if identity.main_checkout || identity.checkout_id.is_empty()
    {
        (None, None)
    } else {
        (
            Some(identity.checkout_id.clone()),
            Some(identity.repo_path.clone()),
        )
    };
    BindingDraft {
        binding: RepositoryBinding {
            repo_id: identity.repo_id.clone(),
            repo_fingerprint: identity.repo_fingerprint.clone(),
            repo_path: main_path,
            member: true,
            primary: false,
            worktree_id,
            worktree_path,
            expected_ref: None,
        },
        checkouts: identity.worktrees.clone(),
        describing: false,
        describe_error: None,
    }
}

/// The open execution editor (explicit Save/Cancel; never saved on blur).
#[derive(Debug, Clone)]
pub(crate) struct ExecutionDraft {
    pub execution_root: String,
    pub bindings: Vec<BindingDraft>,
    /// The resolved configuration when the draft opened (dirty = differs).
    pub original: ExecutionConfiguration,
    pub op: OperationSlot,
}

impl ExecutionDraft {
    /// A draft prefilled with the scope's current resolved configuration.
    pub(crate) fn from_configuration(configuration: &ExecutionConfiguration) -> Self {
        Self {
            execution_root: configuration.execution_root.clone(),
            bindings: configuration
                .bindings
                .iter()
                .cloned()
                .map(BindingDraft::existing)
                .collect(),
            original: configuration.clone(),
            op: OperationSlot::default(),
        }
    }

    /// The configuration the draft would save, with `root` and the
    /// per-binding expected refs read from the inputs (blank = none).
    pub(crate) fn configuration(&self, root: &str, refs: &[String]) -> ExecutionConfiguration {
        ExecutionConfiguration {
            execution_root: root.trim().to_owned(),
            bindings: self
                .bindings
                .iter()
                .enumerate()
                .map(|(ix, draft)| {
                    let mut binding = draft.binding.clone();
                    binding.expected_ref = refs
                        .get(ix)
                        .map(|text| text.trim().to_owned())
                        .filter(|text| !text.is_empty());
                    binding
                })
                .collect(),
            schema: zeron_proto::EXECUTION_CONFIGURATION_SCHEMA,
        }
    }

    /// Append a described repository. Refuses a repository already bound.
    pub(crate) fn add(&mut self, binding: BindingDraft) -> Result<usize, String> {
        if self
            .bindings
            .iter()
            .any(|existing| existing.binding.repo_id == binding.binding.repo_id)
        {
            return Err(format!(
                "{} is already bound",
                path_name(&binding.binding.repo_path)
            ));
        }
        if self.bindings.len() >= zeron_proto::execution_limits::MAX_BINDINGS {
            return Err(format!(
                "At most {} repositories can be bound",
                zeron_proto::execution_limits::MAX_BINDINGS
            ));
        }
        self.bindings.push(binding);
        Ok(self.bindings.len() - 1)
    }

    pub(crate) fn remove(&mut self, ix: usize) {
        if ix < self.bindings.len() {
            self.bindings.remove(ix);
        }
    }

    pub(crate) fn toggle_member(&mut self, ix: usize) {
        if let Some(draft) = self.bindings.get_mut(ix) {
            draft.binding.member = !draft.binding.member;
        }
    }

    /// Radio: make `ix` the only primary; choosing the current primary again
    /// clears it (a configuration may have no primary). Never two.
    pub(crate) fn set_primary(&mut self, ix: usize) {
        let already = self.bindings.get(ix).is_some_and(|d| d.binding.primary);
        for (i, draft) in self.bindings.iter_mut().enumerate() {
            draft.binding.primary = i == ix && !already;
        }
    }

    /// Pin the checkout `checkout_id` at `path`, or (`None`) use the
    /// repository path.
    pub(crate) fn choose_worktree(&mut self, ix: usize, choice: Option<(String, String)>) {
        if let Some(draft) = self.bindings.get_mut(ix) {
            match choice {
                Some((id, path)) => {
                    draft.binding.worktree_id = Some(id);
                    draft.binding.worktree_path = Some(path);
                }
                None => {
                    draft.binding.worktree_id = None;
                    draft.binding.worktree_path = None;
                }
            }
        }
    }
}

// --------------------------------------------------------------------------
// Drift (saving changes the resolution of items with bound Chats)
// --------------------------------------------------------------------------

/// What saving (or resetting) a scope's configuration does to item Chats.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ExecutionDriftImpact {
    /// Bound, associated Chats that would newly differ from their item's
    /// configuration (they keep their captured one), sorted.
    pub chat_ids: Vec<String>,
    /// Live items whose resolved configuration changes, sorted.
    pub item_ids: Vec<String>,
}

impl ExecutionDriftImpact {
    pub(crate) fn needs_confirmation(&self) -> bool {
        !self.chat_ids.is_empty()
    }

    pub(crate) fn body(&self) -> String {
        let chats = plural(self.chat_ids.len(), "Chat", "Chats");
        let items = plural(self.item_ids.len(), "work item", "work items");
        format!(
            "This changes the execution configuration of {items}. {chats} already running there \
             keep their captured configuration; Zeron does not retarget them. They will show as \
             exceptions on the item."
        )
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// The impact of writing `next` (or resetting, `None`) to `scope`.
pub(crate) fn execution_drift(
    snapshot: &WorkOutlineSnapshot,
    scope: &ExecutionScope,
    next: Option<&ExecutionConfiguration>,
) -> ExecutionDriftImpact {
    let Some(space) = snapshot.execution_space.as_ref() else {
        return ExecutionDriftImpact::default();
    };
    let row_id = scope.row_id();
    let mut after: Vec<ExecutionConfigurationView> = snapshot
        .execution_configurations
        .iter()
        .filter(|view| view.id != row_id)
        .cloned()
        .collect();
    if let Some(configuration) = next {
        after.push(ExecutionConfigurationView {
            id: row_id,
            space_id: snapshot.space_id.clone(),
            work_item_id: match scope {
                ExecutionScope::WorkItem { work_item_id } => Some(work_item_id.clone()),
                ExecutionScope::Space { .. } => None,
            },
            configuration: Some(configuration.clone()),
            invalid_reason: None,
            updated_by: None,
            updated_at: 0,
        });
    }
    let mut item_ids = BTreeSet::new();
    let mut resolved_after = std::collections::HashMap::new();
    for item in snapshot
        .items
        .iter()
        .filter(|item| item.archived_at.is_none())
    {
        let item_scope = ExecutionScope::WorkItem {
            work_item_id: item.id.clone(),
        };
        let before = resolve_execution_configuration(
            &item_scope,
            space,
            &snapshot.items,
            &snapshot.execution_configurations,
        );
        let now = resolve_execution_configuration(&item_scope, space, &snapshot.items, &after);
        let content = |r: &Option<ResolvedExecutionConfiguration>| {
            r.as_ref()
                .map(|r| (r.configuration.clone(), r.invalid_reason.clone()))
        };
        if content(&before) != content(&now) {
            item_ids.insert(item.id.clone());
            resolved_after.insert(item.id.clone(), now);
        }
    }
    let chat_ids: BTreeSet<String> = snapshot
        .chat_execution_bindings
        .iter()
        .filter(|binding| !binding.exception)
        .filter_map(|binding| {
            let item = binding.work_item_id.as_ref()?;
            let now = resolved_after.get(item)?.as_ref()?;
            (now.configuration != binding.resolved.configuration || now.invalid_reason.is_some())
                .then(|| binding.chat_id.clone())
        })
        .collect();
    ExecutionDriftImpact {
        chat_ids: chat_ids.into_iter().collect(),
        item_ids: item_ids.into_iter().collect(),
    }
}

// --------------------------------------------------------------------------
// Validation
// --------------------------------------------------------------------------

/// The headline of a validation reply. `deviceOffline` and
/// `hostUnsupported` are their own states: nothing on disk was checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ValidationHeadline {
    Ready,
    DeviceOffline { device_id: String },
    HostUnsupported { device_id: String },
    Blocked { count: usize },
}

/// One blocking issue: the error's own message and remediation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidationIssue {
    pub code: &'static str,
    pub message: String,
    pub remediation: String,
}

pub(crate) fn validation_headline(
    reply: &ValidateExecutionConfigurationReply,
) -> ValidationHeadline {
    if reply.ok {
        return ValidationHeadline::Ready;
    }
    for report in &reply.errors {
        match &report.error {
            ExecutionValidationError::DeviceOffline { device_id, .. } => {
                return ValidationHeadline::DeviceOffline {
                    device_id: device_id.clone(),
                };
            }
            ExecutionValidationError::HostUnsupported { device_id, .. } => {
                return ValidationHeadline::HostUnsupported {
                    device_id: device_id.clone(),
                };
            }
            _ => {}
        }
    }
    ValidationHeadline::Blocked {
        count: reply.errors.len().max(1),
    }
}

/// The reply's issues, in reply order. The texts are the host's rendered
/// `message` / `remediation`, falling back to the local vocabulary when a
/// field is empty; neither ever suggests another checkout.
pub(crate) fn validation_issues(
    reply: &ValidateExecutionConfigurationReply,
) -> Vec<ValidationIssue> {
    reply
        .errors
        .iter()
        .map(|report| ValidationIssue {
            code: report.error.code(),
            message: if report.message.is_empty() {
                report.error.message()
            } else {
                report.message.clone()
            },
            remediation: if report.remediation.is_empty() {
                report.error.remediation().to_owned()
            } else {
                report.remediation.clone()
            },
        })
        .collect()
}

/// The validation result shown under the Execution section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum ValidationState {
    #[default]
    Idle,
    Running,
    Done(ValidateExecutionConfigurationReply),
    /// The call itself failed (transport, or a host without the method).
    Failed(String),
}

/// Text for a failed execution call.
pub(crate) fn execution_error_text(error: &WorkOutlineError) -> String {
    if is_unsupported(error) {
        "Execution configuration isn\u{2019}t available on this host yet".to_owned()
    } else if error.message.is_empty() {
        error.code.as_str().to_owned()
    } else {
        error.message.clone()
    }
}

// --------------------------------------------------------------------------
// Exceptions
// --------------------------------------------------------------------------

/// The captured bindings of an item's exception Chats, in Chat-id order.
pub(crate) fn exception_bindings<'a>(
    snapshot: &'a WorkOutlineSnapshot,
    item_id: &str,
) -> Vec<&'a ChatExecutionBindingView> {
    snapshot
        .execution_exception_chats(item_id)
        .iter()
        .filter_map(|chat_id| snapshot.chat_execution_binding(chat_id))
        .collect()
}

/// "root · ui, flagship" for a captured configuration.
pub(crate) fn captured_summary(configuration: &ExecutionConfiguration) -> String {
    let repos: Vec<String> = configuration
        .bindings
        .iter()
        .map(|binding| path_name(&binding.repo_path))
        .collect();
    if repos.is_empty() {
        configuration.execution_root.clone()
    } else {
        format!(
            "{} \u{b7} {}",
            configuration.execution_root,
            repos.join(", ")
        )
    }
}

pub(crate) fn exception_tooltip(count: usize) -> SharedString {
    format!(
        "{} use{} a different execution configuration",
        plural(count, "Chat", "Chats"),
        if count == 1 { "s" } else { "" }
    )
    .into()
}

// --------------------------------------------------------------------------
// Per-member branch/diff (bound Chats)
// --------------------------------------------------------------------------

/// Where a pill of the Changes pane (and the composer's branch indicator)
/// comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum MemberKind {
    /// A Work Outline execution binding's primary/member repository.
    #[default]
    Binding,
    /// A chat diff target (a worktree the agent or the user picked).
    Target,
    /// The chat's own checkout (`chat.cwd`), listed beside diff targets.
    Own,
    /// A branch / PR range target ("Diff branch / PR…"): read from refs, no
    /// checkout. Its `checkout_*` are the repository checkout it is read from.
    Ref,
}

/// The range a [`MemberKind::Ref`] member diffs: `merge-base(base_ref,
/// head_ref)...head_ref`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RefRange {
    /// "PR #6487" / "branch michael/eng-3012-x".
    pub label: String,
    pub head_branch: String,
    pub head_ref: String,
    pub base_branch: String,
    pub base_ref: String,
}

/// One repository a bound Chat can show branch and diff state for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionMember {
    pub kind: MemberKind,
    pub repo_id: String,
    pub name: String,
    /// The diff watch `cwd` (the pinned worktree, else the repository path).
    pub checkout_path: String,
    /// The diff watch `checkoutId`; `None` until validation reports it.
    pub checkout_id: Option<String>,
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    pub primary: bool,
    /// `None` when validation confirmed this checkout; otherwise why not
    /// (the validation message and remediation, or "not confirmed yet").
    /// Surfaces show no diff or actions for an unconfirmed member.
    pub unconfirmed: Option<String>,
    /// Set for [`MemberKind::Ref`] members only.
    pub range: Option<RefRange>,
}

impl ExecutionMember {
    fn from_validated(repository: &ValidatedRepository) -> Self {
        Self {
            kind: MemberKind::Binding,
            repo_id: repository.repo_id.clone(),
            name: path_name(&repository.repo_path),
            checkout_path: repository.checkout_path.clone(),
            checkout_id: Some(repository.checkout_id.clone()).filter(|id| !id.is_empty()),
            branch: repository.branch.clone(),
            head_sha: repository.head_sha.clone(),
            primary: repository.primary,
            unconfirmed: None,
            range: None,
        }
    }

    fn from_binding(binding: &RepositoryBinding, unconfirmed: String) -> Self {
        Self {
            kind: MemberKind::Binding,
            repo_id: binding.repo_id.clone(),
            name: path_name(&binding.repo_path),
            checkout_path: binding.checkout_path().to_owned(),
            checkout_id: binding.worktree_id.clone(),
            branch: None,
            head_sha: None,
            primary: binding.primary,
            unconfirmed: Some(unconfirmed),
            range: None,
        }
    }

    /// A resolvable chat diff target. The engine only reports targets whose
    /// checkout exists and has an identity, so these are always confirmed.
    pub(crate) fn from_diff_target(target: &zeron_proto::ChatDiffTargetView) -> Self {
        Self {
            kind: MemberKind::Target,
            repo_id: diff_target_member_id(&target.checkout_id),
            name: if target.repo_name.is_empty() {
                path_name(&target.path)
            } else {
                target.repo_name.clone()
            },
            checkout_path: target.path.clone(),
            checkout_id: Some(target.checkout_id.clone()),
            branch: target.branch.clone(),
            head_sha: target.head_sha.clone(),
            primary: false,
            unconfirmed: None,
            range: None,
        }
    }

    /// A resolvable branch / PR range target, shown as one pill labelled
    /// "PR #n" / "branch <name>".
    pub(crate) fn from_ref_target(target: &zeron_proto::ChatRefTargetView) -> Self {
        Self {
            kind: MemberKind::Ref,
            repo_id: ref_target_member_id(&target.id),
            name: target.label.clone(),
            checkout_path: target.path.clone(),
            checkout_id: Some(target.checkout_id.clone()),
            branch: Some(target.head_branch.clone()),
            head_sha: target.head_sha.clone(),
            primary: false,
            unconfirmed: None,
            range: Some(RefRange {
                label: target.label.clone(),
                head_branch: target.head_branch.clone(),
                head_ref: target.head_ref.clone(),
                base_branch: target.base_branch.clone(),
                base_ref: target.base_ref.clone(),
            }),
        }
    }

    /// The chat's own checkout (`chat.cwd`, `chat.checkout_id`, `chat.branch`),
    /// shown only beside diff targets so the user can go back to it.
    pub(crate) fn own_checkout(chat: &zeron_proto::Chat) -> Option<Self> {
        let cwd = chat.cwd.clone().filter(|cwd| !cwd.is_empty())?;
        Some(Self {
            kind: MemberKind::Own,
            repo_id: own_member_id(&chat.id),
            name: path_name(&cwd),
            checkout_path: cwd,
            checkout_id: chat.checkout_id.clone(),
            branch: chat.branch.clone(),
            head_sha: None,
            primary: false,
            unconfirmed: None,
            range: None,
        })
    }

    /// "ui · main" (or "ui · HEAD (abc1234)"); a ref target is just its
    /// "PR #n" / "branch <name>" label.
    pub(crate) fn label(&self) -> String {
        if let Some(range) = &self.range {
            return range.label.clone();
        }
        format!(
            "{} \u{b7} {}",
            self.name,
            ref_label(self.branch.as_deref(), self.head_sha.as_deref())
        )
    }
}

/// Stable member key of a branch / PR range target (`ref:<id>`).
pub(crate) fn ref_target_member_id(id: &str) -> String {
    format!("ref:{id}")
}

/// Stable member key of a diff target (`target:<checkoutId>`).
pub(crate) fn diff_target_member_id(checkout_id: &str) -> String {
    format!("target:{checkout_id}")
}

/// Stable member key of a chat's own checkout (`own:<chat id>`).
pub(crate) fn own_member_id(chat_id: &str) -> String {
    format!("own:{chat_id}")
}

/// The primary and member repositories of a bound Chat, primary first, then
/// configuration order. With a successful validation reply the branch, HEAD,
/// and checkout identity come from its `repositories`; otherwise from the
/// captured binding (paths only), marked unconfirmed with the reason.
pub(crate) fn execution_members(
    binding: &ChatExecutionBindingView,
    reply: Option<&ValidateExecutionConfigurationReply>,
) -> Vec<ExecutionMember> {
    let unconfirmed = || match reply.map(validation_issues).and_then(|issues| {
        issues
            .into_iter()
            .next()
            .map(|issue| format!("{}. {}", issue.message, issue.remediation))
    }) {
        Some(reason) => reason,
        None => "Zeron has not confirmed this checkout yet.".to_owned(),
    };
    let mut members: Vec<ExecutionMember> = match reply.and_then(|r| r.validated.as_ref()) {
        Some(validated) => validated
            .repositories
            .iter()
            .filter(|repository| repository.member || repository.primary)
            .map(ExecutionMember::from_validated)
            .collect(),
        None => binding
            .resolved
            .configuration
            .bindings
            .iter()
            .filter(|binding| binding.member || binding.primary)
            .map(|binding| ExecutionMember::from_binding(binding, unconfirmed()))
            .collect(),
    };
    // Stable: primary first, configuration order otherwise.
    members.sort_by_key(|member| !member.primary);
    members
}

/// Per bound Chat: the latest per-member validation and the picked member.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ChatExecutionState {
    pub reply: Option<ValidateExecutionConfigurationReply>,
    pub error: Option<String>,
    pub loading: bool,
    /// Repository id picked in the branch indicator or Changes pane.
    pub selected: Option<String>,
    /// The binding capture the reply belongs to.
    pub binding_key: String,
}

/// The member after `current` in display order (wrapping), for the branch
/// indicator's click-to-cycle.
pub(crate) fn next_member<'a>(
    members: &'a [ExecutionMember],
    current: Option<&str>,
) -> Option<&'a ExecutionMember> {
    let ix = current
        .and_then(|repo| members.iter().position(|m| m.repo_id == repo))
        .map_or(0, |ix| (ix + 1) % members.len().max(1));
    members.get(ix)
}

/// The member to show when none was picked: the primary, else the first.
pub(crate) fn default_member(members: &[ExecutionMember]) -> Option<&ExecutionMember> {
    members
        .iter()
        .find(|member| member.primary)
        .or_else(|| members.first())
}

/// A fixture Space with execution configurations, captured bindings, and one
/// exception, layered over the store fixture:
///
/// ```text
/// Launch v2   own row: /work/alpha · ui (primary, member) + flagship (member)
///   API       own row: /work/alpha/flagship · flagship (primary)
///               chat-api: captured an older root → exception
///     Auth    inherits API   chat-auth: captured API's configuration
///   UI        inherits Launch chat-ui: captured Launch's configuration
/// Housekeeping  implicit Space folder (/work/alpha)
/// ```
#[cfg(test)]
pub(crate) mod fixture {
    use super::super::store::fixture_ids::*;
    use super::super::store::fixture_snapshot;
    use super::*;
    use zeron_proto::{ExecutionSpace, ValidatedExecution, WorkItemExecutionException};

    pub(crate) const DEVICE: &str = "d";
    pub(crate) const ROOT: &str = "/work/alpha";
    pub(crate) const REPO_UI: &str = "repo:ui";
    pub(crate) const REPO_FLAGSHIP: &str = "repo:flagship";

    pub(crate) fn binding(
        repo: &str,
        path: &str,
        member: bool,
        primary: bool,
    ) -> RepositoryBinding {
        RepositoryBinding {
            repo_id: repo.into(),
            repo_fingerprint: format!("root-{repo}"),
            repo_path: path.into(),
            member,
            primary,
            worktree_id: None,
            worktree_path: None,
            expected_ref: None,
        }
    }

    pub(crate) fn launch_configuration() -> ExecutionConfiguration {
        ExecutionConfiguration {
            execution_root: ROOT.into(),
            bindings: vec![
                binding(REPO_UI, "/work/alpha/ui", true, true),
                binding(REPO_FLAGSHIP, "/work/alpha/flagship", true, false),
            ],
            schema: 1,
        }
    }

    pub(crate) fn api_configuration() -> ExecutionConfiguration {
        ExecutionConfiguration {
            execution_root: "/work/alpha/flagship".into(),
            bindings: vec![binding(REPO_FLAGSHIP, "/work/alpha/flagship", true, true)],
            schema: 1,
        }
    }

    fn row(
        scope: ExecutionScope,
        configuration: ExecutionConfiguration,
    ) -> ExecutionConfigurationView {
        let work_item_id = match &scope {
            ExecutionScope::WorkItem { work_item_id } => Some(work_item_id.clone()),
            ExecutionScope::Space { .. } => None,
        };
        ExecutionConfigurationView {
            id: scope.row_id(),
            space_id: "s1".into(),
            work_item_id,
            configuration: Some(configuration),
            invalid_reason: None,
            updated_by: None,
            updated_at: 0,
        }
    }

    pub(crate) fn captured(
        chat: &str,
        item: &str,
        source: &str,
        configuration: ExecutionConfiguration,
        exception: bool,
    ) -> ChatExecutionBindingView {
        ChatExecutionBindingView {
            chat_id: chat.into(),
            space_id: "s1".into(),
            resolved: ResolvedExecutionConfiguration {
                space_id: "s1".into(),
                device_id: DEVICE.into(),
                configuration,
                source_scope: ExecutionSourceScope::WorkItem {
                    work_item_id: source.into(),
                },
                invalid_reason: None,
            },
            captured_at: 1,
            captured_by: None,
            operation_id: Some(format!("op-{chat}")),
            work_item_id: Some(item.into()),
            exception,
        }
    }

    pub(crate) fn snapshot(space_id: &str) -> WorkOutlineSnapshot {
        assert_eq!(space_id, "s1", "the execution fixture is Space s1");
        let mut snapshot = fixture_snapshot(space_id);
        snapshot.execution_space = Some(ExecutionSpace {
            space_id: space_id.into(),
            path: ROOT.into(),
            device_id: DEVICE.into(),
        });
        snapshot.execution_configurations = vec![
            row(
                ExecutionScope::WorkItem {
                    work_item_id: API.into(),
                },
                api_configuration(),
            ),
            row(
                ExecutionScope::WorkItem {
                    work_item_id: LAUNCH.into(),
                },
                launch_configuration(),
            ),
        ];
        let old = ExecutionConfiguration {
            execution_root: "/work/alpha/flagship-old".into(),
            bindings: vec![binding(
                REPO_FLAGSHIP,
                "/work/alpha/flagship-old",
                true,
                true,
            )],
            schema: 1,
        };
        snapshot.chat_execution_bindings = vec![
            captured(CHAT_API, API, API, old, true),
            captured(CHAT_AUTH, API_AUTH, API, api_configuration(), false),
            captured(CHAT_UI, UI, LAUNCH, launch_configuration(), false),
        ];
        snapshot.execution_exceptions = vec![WorkItemExecutionException {
            work_item_id: API.into(),
            chat_ids: vec![CHAT_API.into()],
        }];
        snapshot
    }

    /// An `AppState` whose `chat-ui` (bound to Launch's configuration) and
    /// `chat-loose-a` (unbound) live in Space `s1` with this fixture loaded;
    /// validation answers [`launch_validated`]. Returns the recorded
    /// `(method, params)` calls.
    pub(crate) fn bound_state(
        state: &mut crate::state::AppState,
    ) -> std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>> {
        state.spaces = vec![
            serde_json::from_value(serde_json::json!({
                "id": "s1", "deviceId": DEVICE, "path": ROOT,
                "createdAt": "2026-01-01T00:00:00Z"
            }))
            .unwrap(),
        ];
        state.chats = [CHAT_UI, CHAT_LOOSE_A]
            .iter()
            .map(|id| {
                serde_json::from_value(serde_json::json!({
                    "id": id, "deviceId": DEVICE, "archived": false, "spaceId": "s1",
                    "cwd": ROOT, "branch": "main", "checkoutId": "checkout:root",
                    "createdAt": "2026-01-01T00:00:00Z",
                }))
                .unwrap()
            })
            .collect();
        state.set_work_outline_visible(vec!["s1".into()]);
        assert!(state.apply_work_outline_frame("s1", snapshot("s1")));
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = calls.clone();
        state.work_outline_actions_override =
            Some(crate::work_outline::WorkOutlineActions::scripted(
                std::sync::Arc::new(move |method, params| {
                    sink.lock()
                        .unwrap()
                        .push((method.to_owned(), params.clone()));
                    Ok(serde_json::to_value(launch_validated()).unwrap())
                }),
            ));
        calls
    }

    /// A successful validation of the Launch configuration (chat-ui).
    pub(crate) fn launch_validated() -> ValidateExecutionConfigurationReply {
        let repository =
            |repo: &str, path: &str, primary: bool, branch: Option<&str>| ValidatedRepository {
                repo_id: repo.into(),
                repo_path: path.into(),
                checkout_path: path.into(),
                checkout_id: format!("checkout:{repo}"),
                member: true,
                primary,
                branch: branch.map(Into::into),
                head_sha: Some("0123456789abcdef".into()),
                worktree_configured: false,
            };
        ValidateExecutionConfigurationReply {
            ok: true,
            validated: Some(ValidatedExecution {
                space_id: "s1".into(),
                device_id: DEVICE.into(),
                execution_root: ROOT.into(),
                root_checkout_id: None,
                repositories: vec![
                    repository(REPO_UI, "/work/alpha/ui", true, Some("feature/ui")),
                    repository(REPO_FLAGSHIP, "/work/alpha/flagship", false, None),
                ],
                source_scope: ExecutionSourceScope::WorkItem {
                    work_item_id: LAUNCH.into(),
                },
            }),
            errors: Vec::new(),
            resolved: None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn home_directory_detection_for_the_root_picker() {
        use super::is_device_home;
        assert!(is_device_home("/Users/mikey", None));
        assert!(is_device_home("/Users/mikey/", None));
        assert!(is_device_home("/home/dev", None));
        assert!(is_device_home("/root", None));
        assert!(is_device_home("~", None));
        assert!(is_device_home("/srv/me", Some("/srv/me")));
        assert!(!is_device_home("/Users/mikey/Projects", None));
        assert!(!is_device_home("/Users", None));
        assert!(!is_device_home("/srv/me/app", Some("/srv/me")));
        assert!(!is_device_home("/srv/work", None));
    }

    use super::super::store::fixture_ids::*;
    use super::fixture::*;
    use super::*;

    fn item(id: &str) -> ExecutionScope {
        ExecutionScope::WorkItem {
            work_item_id: id.into(),
        }
    }

    #[test]
    fn source_labels_name_this_item_ancestors_space_default_and_implicit() {
        let mut snapshot = snapshot("s1");
        let label = |snapshot: &WorkOutlineSnapshot, id: &str| {
            let resolved = snapshot.resolve_execution(&item(id)).unwrap();
            source_label(snapshot, &item(id), &resolved)
        };
        assert_eq!(label(&snapshot, API), "This item");
        assert_eq!(label(&snapshot, API_AUTH), "Inherited from API");
        assert_eq!(label(&snapshot, UI), "Inherited from Launch v2");
        assert_eq!(label(&snapshot, HOUSEKEEPING), "Implicit Space folder");
        assert!(owns_row(&snapshot, &item(API)));
        assert!(!owns_row(&snapshot, &item(UI)));
        snapshot
            .execution_configurations
            .push(ExecutionConfigurationView {
                id: "s:s1".into(),
                space_id: "s1".into(),
                work_item_id: None,
                configuration: Some(ExecutionConfiguration::implicit("/work/beta")),
                invalid_reason: None,
                updated_by: None,
                updated_at: 0,
            });
        assert_eq!(label(&snapshot, HOUSEKEEPING), "Space default");
    }

    #[test]
    fn drafts_keep_one_primary_and_refuse_duplicates() {
        let mut draft = ExecutionDraft::from_configuration(&launch_configuration());
        draft.set_primary(1);
        let primaries: Vec<bool> = draft.bindings.iter().map(|b| b.binding.primary).collect();
        assert_eq!(primaries, [false, true]);
        // Choosing the primary again clears it (no primary is allowed).
        draft.set_primary(1);
        assert!(draft.bindings.iter().all(|b| !b.binding.primary));
        draft.toggle_member(0);
        assert!(!draft.bindings[0].binding.member);
        let duplicate = BindingDraft::existing(binding(REPO_UI, "/elsewhere/ui", true, false));
        assert_eq!(draft.add(duplicate).unwrap_err(), "ui is already bound");
        let config = draft.configuration(" /work/alpha ", &["main".into(), "  ".into()]);
        assert_eq!(config.execution_root, "/work/alpha");
        assert_eq!(config.bindings[0].expected_ref.as_deref(), Some("main"));
        assert_eq!(config.bindings[1].expected_ref, None);
        draft.choose_worktree(1, Some(("wt-1".into(), "/work/alpha/.worktrees/x".into())));
        assert_eq!(
            draft.bindings[1].binding.checkout_path(),
            "/work/alpha/.worktrees/x"
        );
        draft.choose_worktree(1, None);
        assert_eq!(draft.bindings[1].binding.worktree_id, None);
    }

    #[test]
    fn a_described_linked_worktree_binds_the_main_path_and_pins_the_worktree() {
        let identity = ExecutionRepositoryIdentity {
            repo_id: "repo:ui".into(),
            repo_fingerprint: "root".into(),
            repo_path: "/work/alpha/.worktrees/ui-x".into(),
            checkout_id: "checkout:x".into(),
            main_checkout: false,
            worktrees: vec![
                ExecutionWorktreeIdentity {
                    path: "/work/alpha/ui".into(),
                    checkout_id: Some("checkout:main".into()),
                    branch: Some("main".into()),
                    main: true,
                    ..Default::default()
                },
                ExecutionWorktreeIdentity {
                    path: "/work/alpha/.worktrees/ui-x".into(),
                    checkout_id: Some("checkout:x".into()),
                    branch: Some("x".into()),
                    ..Default::default()
                },
                ExecutionWorktreeIdentity {
                    path: "/gone".into(),
                    prunable: true,
                    ..Default::default()
                },
            ],
        };
        let draft = binding_from_identity(&identity);
        assert_eq!(draft.binding.repo_path, "/work/alpha/ui");
        assert_eq!(draft.binding.worktree_id.as_deref(), Some("checkout:x"));
        assert!(draft.binding.member && !draft.binding.primary);
        // Prunable or unresolvable checkouts are never offered.
        assert_eq!(draft.worktree_choices().len(), 2);
        let main = ExecutionRepositoryIdentity {
            main_checkout: true,
            repo_path: "/work/alpha/ui".into(),
            ..identity
        };
        assert_eq!(binding_from_identity(&main).binding.worktree_id, None);
    }

    #[test]
    fn drift_counts_only_bound_chats_that_would_newly_differ() {
        let snapshot = snapshot("s1");
        // Editing Launch changes Launch and UI (inherits); chat-ui is bound to
        // Launch's current configuration and would newly differ.
        let mut next = launch_configuration();
        next.bindings.pop();
        let impact = execution_drift(&snapshot, &item(LAUNCH), Some(&next));
        assert_eq!(impact.item_ids, [LAUNCH, UI]);
        assert_eq!(impact.chat_ids, [CHAT_UI]);
        assert!(impact.needs_confirmation());
        assert!(impact.body().contains("keep their captured configuration"));
        assert!(impact.body().contains("exceptions"));
        // Editing API: chat-auth (inherits API) would differ; chat-api is
        // already an exception and is not counted again.
        let impact = execution_drift(&snapshot, &item(API), Some(&launch_configuration()));
        assert_eq!(impact.item_ids, [API, API_AUTH]);
        assert_eq!(impact.chat_ids, [CHAT_AUTH]);
        // Housekeeping has no Chats: no confirmation.
        let impact = execution_drift(&snapshot, &item(HOUSEKEEPING), Some(&next));
        assert_eq!(impact.item_ids, [HOUSEKEEPING]);
        assert!(!impact.needs_confirmation());
        // Same content: nothing changes.
        let impact = execution_drift(&snapshot, &item(API), Some(&api_configuration()));
        assert_eq!(impact, ExecutionDriftImpact::default());
        // Reset API to inherited: API and Auth now inherit Launch.
        let impact = execution_drift(&snapshot, &item(API), None);
        assert_eq!(impact.chat_ids, [CHAT_AUTH]);
    }

    #[test]
    fn validation_headlines_keep_offline_and_unsupported_distinct() {
        let reply = |error: ExecutionValidationError| ValidateExecutionConfigurationReply {
            ok: false,
            validated: None,
            errors: vec![error.report()],
            resolved: None,
        };
        assert_eq!(
            validation_headline(&reply(ExecutionValidationError::DeviceOffline {
                device_id: "d".into(),
                detail: String::new(),
            })),
            ValidationHeadline::DeviceOffline {
                device_id: "d".into()
            }
        );
        assert_eq!(
            validation_headline(&reply(ExecutionValidationError::HostUnsupported {
                device_id: "d".into(),
                capability: "work-outline-multi-repo-v1".into(),
            })),
            ValidationHeadline::HostUnsupported {
                device_id: "d".into()
            }
        );
        let missing = reply(ExecutionValidationError::WorktreeMissing {
            repo_id: "repo:ui".into(),
            worktree_id: "wt".into(),
            path: "/work/alpha/.worktrees/x".into(),
        });
        assert_eq!(
            validation_headline(&missing),
            ValidationHeadline::Blocked { count: 1 }
        );
        let issues = validation_issues(&missing);
        assert_eq!(issues[0].code, "worktreeMissing");
        assert!(issues[0].message.contains("/work/alpha/.worktrees/x"));
        assert!(!issues[0].remediation.is_empty());
        assert_eq!(
            validation_headline(&launch_validated()),
            ValidationHeadline::Ready
        );
    }

    #[test]
    fn members_list_primary_first_with_branch_or_head() {
        let snapshot = snapshot("s1");
        let binding = snapshot.chat_execution_binding(CHAT_UI).unwrap();
        // Before validation: paths from the captured binding, no checkout id.
        let members = execution_members(binding, None);
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].checkout_path, "/work/alpha/ui");
        assert_eq!(members[0].checkout_id, None);
        assert_eq!(
            members[0].unconfirmed.as_deref(),
            Some("Zeron has not confirmed this checkout yet.")
        );
        // A failed validation: unconfirmed with its message and remediation.
        let failed = ValidateExecutionConfigurationReply::from_result(
            None,
            Err(vec![ExecutionValidationError::WorktreeMissing {
                repo_id: REPO_UI.into(),
                worktree_id: "wt".into(),
                path: "/work/alpha/ui".into(),
            }]),
        );
        let members = execution_members(binding, Some(&failed));
        let reason = members[0].unconfirmed.as_deref().unwrap();
        assert!(reason.contains("/work/alpha/ui"), "{reason}");
        assert!(reason.contains(&failed.errors[0].remediation), "{reason}");
        // With the reply: checkout ids and refs.
        let reply = launch_validated();
        let members = execution_members(binding, Some(&reply));
        assert_eq!(members[0].label(), "ui \u{b7} feature/ui");
        assert_eq!(members[1].label(), "flagship \u{b7} HEAD (0123456)");
        assert_eq!(
            members[1].checkout_id.as_deref(),
            Some("checkout:repo:flagship")
        );
        assert_eq!(default_member(&members).unwrap().repo_id, REPO_UI);
        assert!(members.iter().all(|member| member.unconfirmed.is_none()));
    }

    #[test]
    fn exception_bindings_carry_their_captured_configuration() {
        let snapshot = snapshot("s1");
        let exceptions = exception_bindings(&snapshot, API);
        assert_eq!(exceptions.len(), 1);
        assert_eq!(
            captured_summary(&exceptions[0].resolved.configuration),
            "/work/alpha/flagship-old \u{b7} flagship-old"
        );
        assert!(exception_bindings(&snapshot, UI).is_empty());
        assert_eq!(
            exception_tooltip(1).as_ref(),
            "1 Chat uses a different execution configuration"
        );
    }
}
