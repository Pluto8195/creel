//! Per-chat diff targets: the checkouts the Changes pane shows for a chat
//! besides (or instead of) its own `cwd` checkout.
//!
//! A target is a git checkout (a linked worktree or a submodule checkout) the
//! chat actually works in. The agent tool `bind_chat_worktree` and the Changes
//! pane's "Diff checkout…" picker write the same per-chat registry row
//! (`chatDiffTargets`, see `m0-contract.md` § diff-target amendment). Targets
//! never gate dispatch and never move `chat.cwd`; they only choose what is
//! diffed.
//!
//! The live view is the engine-wide `WatchChatDiffTargets` stream
//! ([`ChatDiffTargetsView`] per chat, resolved to checkout ids and branches by
//! diff-sync); the diff itself is the existing scoped
//! `WatchCheckoutDiffs {cwd, checkoutId}` of the target checkout.

use serde::{Deserialize, Serialize};

/// Who wrote a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChatDiffTargetSource {
    /// The `bind_chat_worktree` agent tool.
    Agent,
    /// The Changes pane's "Diff checkout…" picker.
    Manual,
}

/// One resolved target of a chat: a stored target whose checkout currently
/// exists, belongs to the chat's repository family, and has an identity.
/// Targets that no longer resolve (worktree removed) are omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDiffTargetView {
    /// Canonical checkout top-level path (the diff watch `cwd`).
    pub path: String,
    /// The diff watch `checkoutId` (`CheckoutIdentity.id`).
    pub checkout_id: String,
    pub source: ChatDiffTargetSource,
    /// This is the checkout the chat's Changes pane defaults to. At most one
    /// target per chat is selected; none selected means the chat's own
    /// checkout.
    #[serde(default)]
    pub selected: bool,
    /// Repository display name (basename of the repository's main checkout).
    #[serde(default)]
    pub repo_name: String,
    /// Current branch of the checkout; `None` when HEAD is detached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
}

/// One chat's resolved targets (one frame item of `WatchChatDiffTargets`;
/// chats without a resolvable target are omitted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDiffTargetsView {
    pub chat_id: String,
    pub targets: Vec<ChatDiffTargetView>,
    /// Branch / PR range targets (`m0-contract.md` § "Diff a branch or PR").
    /// Resolved like `targets`: only those whose repository checkout still
    /// resolves and whose head ref still exists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ref_targets: Vec<ChatRefTargetView>,
}

/// What a ref target names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RefDiffKind {
    /// A pull request, labelled "PR #n".
    Pr,
    /// A branch, labelled "branch <name>".
    Branch,
}

/// One resolved branch / PR range target: `merge-base(base_ref, head_ref)...
/// head_ref` of a repository, read from its refs only (no checkout).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRefTargetView {
    /// Stable id (the `selectRef` / `unbindRef` argument).
    pub id: String,
    pub kind: RefDiffKind,
    /// "PR #6487" or "branch michael/eng-3012-x".
    pub label: String,
    /// The repository checkout the range is read from (the diff watch / RPC
    /// `cwd`): the chat's own checkout or one of its submodules, never a
    /// linked worktree.
    pub path: String,
    /// `CheckoutIdentity.id` of `path`.
    pub checkout_id: String,
    pub repo_name: String,
    /// Head branch name (without `refs/...`).
    pub head_branch: String,
    /// Full ref the head is read from (`refs/remotes/origin/<b>`, ...).
    pub head_ref: String,
    /// Base branch name as shown ("main").
    pub base_branch: String,
    /// Full ref the base is read from.
    pub base_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    #[serde(default)]
    pub selected: bool,
}

/// What a [`DiffCheckoutCandidate`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffCheckoutKind {
    /// The chat's own checkout (`chat.cwd`'s top level).
    Own,
    /// A submodule checkout under the chat's checkout.
    Submodule,
    /// A linked worktree of the chat's repository or of one of its
    /// submodules.
    Worktree,
}

/// One entry of the "Diff checkout…" picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffCheckoutCandidate {
    /// Canonical checkout top-level path.
    pub path: String,
    pub kind: DiffCheckoutKind,
    /// Repository display name.
    pub repo_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    /// The checkout is currently one of the chat's bound targets.
    #[serde(default)]
    pub bound: bool,
    /// The checkout is the chat's selected diff target (or, for the `own`
    /// entry, nothing else is selected).
    #[serde(default)]
    pub selected: bool,
}

/// Params of `ListChatDiffCheckouts`: `{chatId}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListChatDiffCheckoutsParams {
    pub chat_id: String,
}

/// Reply of `ListChatDiffCheckouts`. Own checkout first, then submodule
/// checkouts, then worktrees (root repository's, then each submodule's, in
/// `git worktree list` order). Unsorted by ticket: the UI ranks ticket matches.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListChatDiffCheckoutsReply {
    pub candidates: Vec<DiffCheckoutCandidate>,
}

/// What `SetChatDiffTarget` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SetChatDiffTargetAction {
    /// Bind `path` (source `manual`) and make it the selected target.
    #[default]
    Select,
    /// Forget `path` as a target (clears the selection when it was selected).
    Unbind,
    /// Clear the selection: the chat's own checkout is shown again. Bound
    /// targets stay listed in the member picker.
    ClearSelection,
    /// Resolve `input` (a PR number, a PR URL, or a branch name) to a branch /
    /// PR range target, fetch its refs read-only, store it and select it.
    BindRef,
    /// Select the stored ref target `refId` (clears the checkout selection).
    SelectRef,
    /// Forget the ref target `refId`.
    UnbindRef,
}

/// Params of `SetChatDiffTarget`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetChatDiffTargetParams {
    pub chat_id: String,
    #[serde(default)]
    pub action: SetChatDiffTargetAction,
    /// Required for `select` and `unbind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Required for `bindRef`: a PR number (`6487`, `#6487`), a PR URL, or a
    /// branch name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// `bindRef`: compare against this branch instead of the PR's base (or
    /// `main`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    /// Required for `selectRef` and `unbindRef`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
}

/// Reply of `SetChatDiffTarget` (and the engine-side result of the agent
/// tool): the chat's targets after the write.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDiffTargetsReply {
    pub targets: Vec<ChatDiffTargetEntry>,
    /// Stored ref targets (id, label, selected).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ref_targets: Vec<ChatRefTargetEntry>,
}

/// One stored ref target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRefTargetEntry {
    pub id: String,
    pub kind: RefDiffKind,
    pub label: String,
    #[serde(default)]
    pub selected: bool,
}

/// One stored target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDiffTargetEntry {
    /// Canonical checkout top-level path.
    pub path: String,
    pub source: ChatDiffTargetSource,
    #[serde(default)]
    pub selected: bool,
}

/// Most targets one chat can hold (one worktree per submodule, plus slack).
pub const MAX_CHAT_DIFF_TARGETS: usize = 16;
/// Most ref targets one chat can hold.
pub const MAX_CHAT_REF_TARGETS: usize = 8;
/// Most recent operation ids a `chatDiffTargets` row remembers for replay.
pub const MAX_CHAT_DIFF_TARGET_OPS: usize = 16;

/// A target as stored in the `chatDiffTargets` row: validated at write time,
/// re-checked against `repo_id` / `worktree_id` whenever it is resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredChatDiffTarget {
    /// Canonical checkout top-level path.
    pub path: String,
    pub source: ChatDiffTargetSource,
    /// Topology repository id (`repo:` + sha256(device NUL canonical git
    /// common dir)) the checkout belonged to when bound.
    pub repo_id: String,
    /// `CheckoutIdentity.id` of the checkout when bound.
    pub worktree_id: String,
    /// Unix milliseconds.
    #[serde(default)]
    pub bound_at: i64,
}

/// A branch / PR range target as stored in the `chatDiffTargets` row
/// (`ref_targets`). Refs are stored fully qualified so the range can be read
/// without any lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredRefDiffTarget {
    /// Stable id: `pr:<repo id>#<n>` or `br:<repo id>:<branch>`, hashed to fit
    /// an operation id.
    pub id: String,
    pub kind: RefDiffKind,
    /// Canonical path of the repository checkout the refs live in.
    pub path: String,
    /// Topology repository id of `path` when bound.
    pub repo_id: String,
    /// Head branch name.
    pub head_branch: String,
    /// Full head ref.
    pub head_ref: String,
    /// Base branch name.
    pub base_branch: String,
    /// Full base ref.
    pub base_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    /// Unix milliseconds.
    #[serde(default)]
    pub bound_at: i64,
}

/// The decoded `chatDiffTargets` row of one chat (row id = chat id).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDiffTargetsRow {
    pub chat_id: String,
    #[serde(default)]
    pub targets: Vec<StoredChatDiffTarget>,
    /// Path of the selected target; `None` = the chat's own checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<String>,
    /// Operation ids already applied (newest last, at most
    /// [`MAX_CHAT_DIFF_TARGET_OPS`]): a retry with a known id writes nothing.
    #[serde(default)]
    pub ops: Vec<String>,
    #[serde(default)]
    pub updated_at: i64,
    /// Branch / PR range targets (≤ [`MAX_CHAT_REF_TARGETS`]); rows written
    /// before this field existed read as empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ref_targets: Vec<StoredRefDiffTarget>,
    /// Id of the selected ref target. At most one of `selected` and
    /// `selected_ref` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_ref: Option<String>,
}

/// One entry of the "Diff branch / PR…" picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefDiffCandidate {
    pub kind: RefDiffKind,
    /// "PR #6487" / "branch michael/eng-3012-x".
    pub label: String,
    /// Extra line: the PR URL, or the repository name.
    pub detail: String,
    /// What to pass as `bindRef` `input` (the PR URL or the branch name).
    pub input: String,
    pub repo_name: String,
    /// A PR linked to this chat (`chatPrLinks`); these sort first.
    #[serde(default)]
    pub linked: bool,
    /// The chat's ticket id appears in the branch name.
    #[serde(default)]
    pub suggested: bool,
    /// Already stored as a ref target of this chat.
    #[serde(default)]
    pub bound: bool,
}

/// Reply of `ListChatRefDiffCandidates {chatId}`: linked PRs first (in link
/// order), then branches matching the chat's ticket (newest first).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListChatRefDiffCandidatesReply {
    pub candidates: Vec<RefDiffCandidate>,
}
