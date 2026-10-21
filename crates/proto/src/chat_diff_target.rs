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
}

/// Reply of `SetChatDiffTarget` (and the engine-side result of the agent
/// tool): the chat's targets after the write.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDiffTargetsReply {
    pub targets: Vec<ChatDiffTargetEntry>,
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
