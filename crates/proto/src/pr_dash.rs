//! The PR page's data: a port of the Cofactr workspace's `pr-dash` (two PR
//! tabs, buckets, needs-you counts). The engine fetches and normalizes PRs
//! into [`PrDashRow`] facts; [`classify`], [`needs_you_count`] and
//! [`ordered_rows`] are pure functions over those facts, shared by the engine
//! and the UI. Spec: `docs/plans/work-outline/pr-dash-spec.md`.

use serde::{Deserialize, Serialize};

use crate::entities::ChatLinkSource;

/// The two tabs. `Authored` = `author:{me}`; `ReviewRequested` = the union of
/// `review-requested:{me}` and `reviewed-by:{me} -author:{me}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PrDashTab {
    Authored,
    ReviewRequested,
}

impl PrDashTab {
    pub const ALL: [PrDashTab; 2] = [PrDashTab::Authored, PrDashTab::ReviewRequested];

    pub fn label(self) -> &'static str {
        match self {
            Self::Authored => "Authored by me",
            Self::ReviewRequested => "Review requested",
        }
    }

    /// Buckets of this tab in chip / group order.
    pub fn buckets(self) -> &'static [PrDashBucket] {
        use PrDashBucket::*;
        match self {
            Self::Authored => &[
                NeedsYou, Comments, Ready, Merged, NoReviewer, InReview, CiRunning, Draft, Closed,
            ],
            Self::ReviewRequested => &[
                ToReview,
                ReReview,
                ChangesRequested,
                Approved,
                CommentedByYou,
                DraftIn,
                Merged,
                Closed,
            ],
        }
    }

    /// Buckets counted by the tab's "needs you" badge.
    pub fn alert_buckets(self) -> &'static [PrDashBucket] {
        match self {
            Self::Authored => &[PrDashBucket::NeedsYou],
            Self::ReviewRequested => &[PrDashBucket::ToReview, PrDashBucket::ReReview],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrDashBucket {
    // Authored tab.
    NeedsYou,
    Comments,
    Ready,
    NoReviewer,
    InReview,
    CiRunning,
    Draft,
    // Review-requested tab.
    ToReview,
    ReReview,
    ChangesRequested,
    Approved,
    CommentedByYou,
    DraftIn,
    // Both.
    Merged,
    Closed,
}

impl PrDashBucket {
    pub fn label(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
            Self::Comments => "Comments to address",
            Self::Ready => "Ready to merge",
            Self::NoReviewer => "No reviewer",
            Self::InReview => "In review",
            Self::CiRunning => "CI running",
            Self::Draft | Self::DraftIn => "Draft",
            Self::ToReview => "Waiting on you",
            Self::ReReview => "Re-review asked",
            Self::ChangesRequested => "Changes requested",
            Self::Approved => "Approved",
            Self::CommentedByYou => "You commented",
            Self::Merged => "Merged",
            Self::Closed => "Closed",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Self::NeedsYou | Self::ToReview => "●",
            Self::Comments | Self::ChangesRequested => "○",
            Self::Ready | Self::Approved => "✓",
            Self::NoReviewer => "∅",
            Self::InReview | Self::CommentedByYou => "·",
            Self::CiRunning => "◔",
            Self::Draft | Self::DraftIn => "✎",
            Self::ReReview => "◑",
            Self::Merged => "◆",
            Self::Closed => "✕",
        }
    }
}

/// One reviewer chip. `state` is a GitHub review state (`APPROVED`,
/// `CHANGES_REQUESTED`, `COMMENTED`, `DISMISSED`) or pr-dash's synthetic
/// `REQUESTED` / `RE_REQUESTED`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrDashReviewer {
    pub login: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrDashCheck {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Changed lines by kind. `code_churn` counts modified/deleted code lines
/// (additions + deletions of non-new files); `code_new_lines` the additions
/// of new code files. The blank/comment "inert line" discount of pr-dash is
/// not applied (it needs a diff request per PR).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrDashDiff {
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub code_churn: u64,
    pub code_new_lines: u64,
    pub test_lines: u64,
    pub generated_lines: u64,
    pub docs_lines: u64,
}

/// A Zeron chat linked to a PR (shown on the row; click opens it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrDashChat {
    pub chat_id: String,
    pub title: String,
    pub source: ChatLinkSource,
    #[serde(default)]
    pub archived: bool,
    /// The chat was started by the review launcher (`pr_review`).
    #[serde(default)]
    pub is_review: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// Normalized facts of one PR (`pr-dash`'s `normalize()` output) plus its
/// classification. Everything `classify` reads is a plain field here so
/// fixtures can construct rows directly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrDashRow {
    pub number: u64,
    pub title: String,
    /// GitHub's own casing, e.g. `https://github.com/Cofactr/flagship/pull/6487`.
    pub url: String,
    /// `owner/name`.
    pub repo_full: String,
    /// The name part (the Repo column).
    pub repo: String,
    pub author: String,
    /// `OPEN` | `MERGED` | `CLOSED`.
    pub state: String,
    pub draft: bool,
    pub head_ref: String,
    pub base_ref: String,
    /// First `ENG|COF|FND|ASK|OPS|BIZ-n` in the title, else the head branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub diff: PrDashDiff,
    /// GitHub `reviewDecision`: APPROVED / CHANGES_REQUESTED / REVIEW_REQUIRED.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_decision: Option<String>,
    /// MERGEABLE / CONFLICTING / UNKNOWN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mergeable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_state: Option<String>,
    /// SUCCESS / FAILURE / ERROR / PENDING / EXPECTED / CANCELLED, `None` = no CI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci: Option<String>,
    #[serde(default)]
    pub failing_checks: Vec<PrDashCheck>,
    #[serde(default)]
    pub cancelled_checks: Vec<PrDashCheck>,
    #[serde(default)]
    pub reviewers: Vec<PrDashReviewer>,
    pub blocking_changes: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_review: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_last_comment_at: Option<String>,
    pub requested_from_me: bool,
    pub unresolved: u64,
    pub unanswered_comments: u64,
    pub answered_threads: u64,
    pub outdated_threads: u64,
    pub comments_total: u64,
    /// 1-based position in a stack of PRs (0 = not stacked).
    #[serde(default)]
    pub stack_pos: u32,
    #[serde(default)]
    pub stack_size: u32,
    pub bucket: PrDashBucket,
    /// Why a Needs-you / Comments PR is there ("2 checks failing", …).
    #[serde(default)]
    pub reasons: Vec<String>,
    /// Zeron chats linked to this PR, best first.
    #[serde(default)]
    pub chats: Vec<PrDashChat>,
}

impl Default for PrDashBucket {
    fn default() -> Self {
        Self::InReview
    }
}

/// Params of `PrDashboard`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrDashParams {
    pub tab: PrDashTab,
    /// Skip the engine's short cache.
    #[serde(default)]
    pub force: bool,
}

/// Reply of `PrDashboard`: rows already classified, in search order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrDashReply {
    /// The GitHub login the queries ran as.
    pub me: String,
    pub rows: Vec<PrDashRow>,
    /// RFC 3339 time the rows were fetched.
    pub fetched_at: String,
    /// A fetch failure shown inline; `rows` then holds the last good data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// pr-dash `classify()` / `classify_incoming()`: the bucket of `row` on `tab`
/// plus the reasons shown on Needs-you / Comments rows. Reads only the facts.
pub fn classify(tab: PrDashTab, row: &PrDashRow) -> (PrDashBucket, Vec<String>) {
    let _ = (tab, row);
    todo!("ported by the pr-dash classify worker")
}

/// The tab badge's urgent count over rows admitted by the hide-drafts toggle.
pub fn needs_you_count(tab: PrDashTab, rows: &[PrDashRow], hide_drafts: bool) -> usize {
    let _ = (tab, rows, hide_drafts);
    todo!("ported by the pr-dash classify worker")
}

/// Rows as the page shows them: drafts dropped when `hide_drafts`, then
/// (`group_by_bucket`) grouped in the tab's bucket order, each group sorted by
/// `updated_at` descending (stable, so equal keys keep search order).
pub fn ordered_rows(
    tab: PrDashTab,
    rows: &[PrDashRow],
    hide_drafts: bool,
    group_by_bucket: bool,
) -> Vec<&PrDashRow> {
    let _ = (tab, rows, hide_drafts, group_by_bucket);
    todo!("ported by the pr-dash classify worker")
}
