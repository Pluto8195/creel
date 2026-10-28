//! PR review sync wire contract (version 1).
//!
//! Contract doc: `docs/plans/work-outline/pr-sync-contract.md`.
//!
//! - Per Space the user picks ONE work item as the PR review sync
//!   destination (`setPrReviewSyncDestination`, a `WorkOutlineMutate` op;
//!   `WorkOutlineSpaceSettings::pr_review_sync_work_item_id`). Every open PR
//!   that requests the user's review in a GitHub repository of the Space (the
//!   Space's own checkout plus every submodule / nested repository under its
//!   folder, matched by `origin` remote) gets a child work item under that
//!   destination, linked with a `github` / `pullRequest` external reference.
//! - The engine on the Space's device keeps those items, their status, and
//!   one system-created Required Action per review round in step with
//!   GitHub (through the `gh` CLI). Nothing is written to GitHub.
//! - The RPCs (method names in `zeron_rpc::methods`, `PR_*`) are local/IPC
//!   and never device-forwarded. Failures are strings
//!   `github/<code>: <message>` ([`GithubError::from_wire_message`]).
//!
//! Wire conventions follow `work_outline.rs`: camelCase fields, every
//! non-identity field decodes when absent, enums from GitHub are lenient.

use serde::{Deserialize, Serialize};

use crate::work_outline::{WorkItemStatus, limits};

/// `provider` of a Work Outline external reference that points at GitHub.
pub const GITHUB_PROVIDER: &str = "github";
/// `referenceType` of a GitHub pull request reference (`externalId` =
/// `<owner>/<repo>#<number>` as GitHub spells it, `url` = the PR URL,
/// `cachedTitle` = the PR title).
pub const GITHUB_PULL_REQUEST_REFERENCE: &str = "pullRequest";
/// The capability a device advertises when it runs the PR review sync,
/// serves the `PR_*` RPCs, and understands `setPrReviewSyncDestination`.
pub const PR_REVIEW_SYNC_CAPABILITY_V1: &str = "pr-review-sync-v1";
/// Separator between `<repo>#<number>` and the PR title in a synced item's
/// title.
pub const PR_TITLE_SEPARATOR: &str = " · ";
/// Prefix of a synced Required Action's `dedupeKey`
/// (`pr-review:<owner>/<repo>#<n>:<round>`).
pub const PR_REVIEW_DEDUPE_PREFIX: &str = "pr-review:";
/// Resolution notes the sync records on its Required Actions.
pub const PR_RESOLUTION_REVIEW_SUBMITTED: &str = "Review submitted";
pub const PR_RESOLUTION_MERGED: &str = "PR merged";
pub const PR_RESOLUTION_CLOSED: &str = "PR closed";
pub const PR_RESOLUTION_WITHDRAWN: &str = "Review request withdrawn";

/// `<owner>/<repo>#<number>` (the reference `externalId`).
pub fn pr_external_id(owner: &str, repo: &str, number: u64) -> String {
    format!("{}/{}#{number}", owner.trim(), repo.trim())
}

/// The case-insensitive identity of a PR (`externalId`, lowercased): the
/// sync's derived ids and every match use it.
pub fn pr_key(owner: &str, repo: &str, number: u64) -> String {
    pr_external_id(owner, repo, number).to_ascii_lowercase()
}

/// The title a synced item carries: `"<repo>#<number> · <PR title>"`,
/// whitespace-collapsed and cut to `limits::MAX_TITLE_CHARS`.
pub fn pr_item_title(repo: &str, number: u64, title: &str) -> String {
    let head = format!("{}#{number}", repo.trim());
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let full = if title.is_empty() {
        head
    } else {
        format!("{head}{PR_TITLE_SEPARATOR}{title}")
    };
    if full.chars().count() <= limits::MAX_TITLE_CHARS {
        return full;
    }
    let mut out: String = full
        .chars()
        .take(limits::MAX_TITLE_CHARS.saturating_sub(1))
        .collect();
    out.push('…');
    out
}

/// `pr-review:<owner>/<repo>#<n>:<round>` (lowercased owner/repo).
pub fn pr_review_dedupe_key(owner: &str, repo: &str, number: u64, round: u32) -> String {
    format!(
        "{PR_REVIEW_DEDUPE_PREFIX}{}:{round}",
        pr_key(owner, repo, number)
    )
}

// --------------------------------------------------------------------------
// GitHub entities
// --------------------------------------------------------------------------

/// One pull request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestRef {
    pub owner: String,
    pub repo: String,
    pub number: u64,
    /// `https://github.com/<owner>/<repo>/pull/<number>`.
    #[serde(default)]
    pub url: String,
}

impl PullRequestRef {
    pub fn new(owner: impl Into<String>, repo: impl Into<String>, number: u64) -> Self {
        let owner = owner.into();
        let repo = repo.into();
        let url = format!("https://github.com/{owner}/{repo}/pull/{number}");
        Self {
            owner,
            repo,
            number,
            url,
        }
    }

    /// `<owner>/<repo>#<number>`.
    pub fn external_id(&self) -> String {
        pr_external_id(&self.owner, &self.repo, self.number)
    }

    /// The lowercased [`Self::external_id`].
    pub fn key(&self) -> String {
        pr_key(&self.owner, &self.repo, self.number)
    }

    /// Parse an `externalId` (`<owner>/<repo>#<number>`).
    pub fn parse_external_id(external_id: &str) -> Option<Self> {
        let (repo, number) = external_id.trim().split_once('#')?;
        let (owner, name) = repo.split_once('/')?;
        let number: u64 = number.parse().ok().filter(|n| *n > 0)?;
        if owner.is_empty() || name.is_empty() || name.contains('/') {
            return None;
        }
        Some(Self::new(owner, name, number))
    }
}

/// A PR's state. Lenient: an unknown state reads as `open`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestState {
    Draft,
    Merged,
    Closed,
    #[default]
    #[serde(other)]
    Open,
}

impl PullRequestState {
    /// Merged or closed.
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Merged | Self::Closed)
    }
}

/// The PR's CI rollup. Lenient: unknown reads as `none`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestCiStatus {
    Passing,
    Failing,
    Pending,
    #[default]
    #[serde(other)]
    None,
}

/// GitHub's review decision. Lenient: unknown reads as `reviewRequired`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestReviewDecision {
    Approved,
    ChangesRequested,
    #[serde(other)]
    ReviewRequired,
}

/// The signed-in user's relation to the PR's review. Lenient: unknown
/// reads as `none`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestMyReviewState {
    /// Review requested; the user has not reviewed this PR before.
    Requested,
    /// Review requested again after the user already reviewed.
    ReRequested,
    /// The user's latest review (no request pending).
    Commented,
    Approved,
    ChangesRequested,
    #[default]
    #[serde(other)]
    None,
}

impl PullRequestMyReviewState {
    pub fn is_requested(self) -> bool {
        matches!(self, Self::Requested | Self::ReRequested)
    }
}

/// Live status of one PR (chips, the PR panels, the sync).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestStatusSummary {
    #[serde(rename = "ref")]
    pub pr_ref: PullRequestRef,
    #[serde(default)]
    pub title: String,
    /// The author's login.
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub state: PullRequestState,
    #[serde(default)]
    pub ci_status: PullRequestCiStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_decision: Option<PullRequestReviewDecision>,
    #[serde(default)]
    pub my_review_state: PullRequestMyReviewState,
    /// Logins (and `org/team` slugs) whose review is requested.
    #[serde(default)]
    pub requested_reviewers: Vec<String>,
    /// ISO-8601 timestamp.
    #[serde(default)]
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_ref_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref_name: Option<String>,
}

// --------------------------------------------------------------------------
// The sync's per-item record
// --------------------------------------------------------------------------

/// The review-request state the sync last saw for a synced PR item.
/// Lenient: unknown reads as `requested`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PrReviewRequestState {
    /// The user submitted a review and no re-request is pending.
    Submitted,
    /// The request was removed without a review from the user.
    Withdrawn,
    Merged,
    Closed,
    /// The user's review was requested and not yet given. Declared last
    /// because `#[serde(other)]` must sit on the last variant.
    #[default]
    #[serde(other)]
    Requested,
}

/// `workItems.externalSyncState` (LWW JSON, written only by the system
/// actor): what the PR review sync last synced on this item. The sync writes
/// the item's status only when `requestState` or `round` changes, so a
/// status the user set in between stands until GitHub's state changes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemExternalSyncState {
    /// `github`.
    #[serde(default)]
    pub provider: String,
    /// The PR's `externalId` (`<owner>/<repo>#<number>`).
    #[serde(default)]
    pub external_id: String,
    /// Review round: 1 for the first request, +1 for every re-request.
    #[serde(default)]
    pub round: u32,
    #[serde(default)]
    pub request_state: PrReviewRequestState,
    /// The status the sync last wrote (differs from the item's status when
    /// the user changed it since).
    #[serde(default)]
    pub synced_status: WorkItemStatus,
    /// When the current round started (epoch ms, this device's clock).
    #[serde(default)]
    pub round_started_at: i64,
    /// When the sync last wrote this record (epoch ms).
    #[serde(default)]
    pub synced_at: i64,
    /// Set (to `merged` or `closed`, the PR state that caused it) once the
    /// sync has archived the item for that state. The sync never archives
    /// an item for the same round and state twice, so an item the user
    /// restores stays restored; a new round resets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_by_sync: Option<PrReviewRequestState>,
    /// Linked Linear item only (`provider` = `linear`): why the ticket no
    /// longer matches ("Started in Linear", "Moved to project X", …) on a
    /// child the user has touched, which the sync therefore keeps. `None`
    /// while the ticket matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    /// Linked Linear item only: the sync archived this untouched child
    /// because its ticket stopped matching.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived_by_linear_sync: bool,
}

// --------------------------------------------------------------------------
// RPC params and replies (method names: `zeron_rpc::methods::PR_*`)
// --------------------------------------------------------------------------

/// `PrStatus {owner, repo, number, forceRefresh?}` →
/// [`PullRequestStatusSummary`]. Served from a ~2 minute per-PR cache
/// unless `forceRefresh`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrStatusParams {
    pub owner: String,
    pub repo: String,
    pub number: u64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub force_refresh: bool,
}

/// `PrViewer` (no params) → [`PrViewerReply`]: the GitHub login `gh` is
/// signed in as, cached for the engine run. Rows use it to show "you" for
/// the viewer's own PRs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrViewerReply {
    pub login: String,
}

/// `PrReviewSyncNow {spaceId}` → [`PrReviewSyncNowReply`]: run the Space's
/// sync now (bypassing caches). `github/invalid` when the Space has no
/// destination, its destination is missing or archived, or the Space lives
/// on another device.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrReviewSyncNowParams {
    pub space_id: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrReviewSyncNowReply {
    /// Work items created.
    #[serde(default)]
    pub created: u32,
    /// Work items whose status or sync record changed.
    #[serde(default)]
    pub updated: u32,
    /// Required Actions the sync resolved.
    #[serde(default)]
    pub resolved: u32,
}

/// `PrMyOpen {spaceId}` → `[PullRequestStatusSummary]`: the user's open PRs
/// in the Space's repositories, most recently updated first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrMyOpenParams {
    pub space_id: String,
}

/// `PrReviewRequested {spaceId}` → `[PullRequestStatusSummary]`: open PRs
/// in the Space's repositories that request the user's review.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrReviewRequestedParams {
    pub space_id: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}

// --------------------------------------------------------------------------
// Errors
// --------------------------------------------------------------------------

/// Machine-readable GitHub failure. Lenient: unknown reads as `invalid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GithubErrorCode {
    /// The GitHub CLI (`gh`) is not installed on the device.
    GhMissing,
    /// `gh` is not signed in (or its token was rejected): run `gh auth login`.
    Unauthorized,
    /// GitHub rate-limited the request.
    RateLimited,
    /// The PR or repository does not exist (or the account cannot see it).
    NotFound,
    /// Transport failure, timeout, or unexpected `gh` output.
    Network,
    /// Malformed request, or the Space cannot sync (no destination, the
    /// destination is gone, another device owns the Space).
    #[serde(other)]
    Invalid,
}

impl GithubErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GhMissing => "ghMissing",
            Self::Unauthorized => "unauthorized",
            Self::RateLimited => "rateLimited",
            Self::NotFound => "notFound",
            Self::Network => "network",
            Self::Invalid => "invalid",
        }
    }

    pub fn parse(code: &str) -> Self {
        match code {
            "ghMissing" => Self::GhMissing,
            "unauthorized" => Self::Unauthorized,
            "rateLimited" => Self::RateLimited,
            "notFound" => Self::NotFound,
            "network" => Self::Network,
            _ => Self::Invalid,
        }
    }
}

/// A failed GitHub operation, wire-encoded as `github/<code>: <message>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubError {
    pub code: GithubErrorCode,
    #[serde(default)]
    pub message: String,
}

const ERROR_PREFIX: &str = "github/";

impl GithubError {
    pub fn new(code: GithubErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// `github/<code>: <message>`.
    pub fn to_wire_message(&self) -> String {
        format!("{ERROR_PREFIX}{}: {}", self.code.as_str(), self.message)
    }

    /// Parse `github/<code>: <message>` anywhere in `text` (transport layers
    /// may prefix it); `None` for any other string.
    pub fn from_wire_message(text: &str) -> Option<Self> {
        let at = text.find(ERROR_PREFIX)?;
        let rest = &text[at + ERROR_PREFIX.len()..];
        let (code, message) = rest.split_once(": ").unwrap_or((rest, ""));
        Some(Self {
            code: GithubErrorCode::parse(code),
            message: message.to_owned(),
        })
    }
}

impl std::fmt::Display for GithubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GithubError {}

#[cfg(test)]
mod tests;
