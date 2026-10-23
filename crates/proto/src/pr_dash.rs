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
    /// The chat was started by the review launcher: its link to this PR has
    /// source `review_launcher` (no chat classification is consulted).
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
    match tab {
        PrDashTab::Authored => classify_authored(row),
        PrDashTab::ReviewRequested => classify_incoming(row),
    }
}

fn plural<'a>(n: u64, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

/// Everything in the conversation still pointed at the viewer: inline threads
/// and top-level remarks nobody has heard an answer to.
fn open_reasons(row: &PrDashRow) -> Vec<String> {
    let mut reasons = Vec::new();
    if row.unresolved > 0 {
        let n = row.unresolved;
        reasons.push(format!("{n} unresolved {}", plural(n, "thread", "threads")));
    }
    if row.unanswered_comments > 0 {
        let n = row.unanswered_comments;
        reasons.push(format!(
            "{n} unanswered {}",
            plural(n, "comment", "comments")
        ));
    }
    reasons
}

fn classify_authored(row: &PrDashRow) -> (PrDashBucket, Vec<String>) {
    use PrDashBucket::*;
    match row.state.as_str() {
        "MERGED" => return (Merged, vec!["merged".to_owned()]),
        "CLOSED" => return (Closed, vec!["closed without merging".to_owned()]),
        _ => {}
    }
    let approved = row.review_decision.as_deref() == Some("APPROVED");
    let ci = row.ci.as_deref();
    let mut reasons = Vec::new();
    if !row.draft && approved && row.mergeable.as_deref() == Some("CONFLICTING") {
        reasons.push("approved, but has a merge conflict".to_owned());
    }
    if matches!(ci, Some("FAILURE" | "ERROR")) {
        let n = row.failing_checks.len();
        reasons.push(if n > 0 {
            format!("{n} check{} failing", if n == 1 { "" } else { "s" })
        } else {
            "CI failing".to_owned()
        });
    }
    if row.blocking_changes {
        reasons.push("changes requested".to_owned());
    }
    if !reasons.is_empty() {
        return (NeedsYou, reasons);
    }
    if row.unresolved > 0 || row.unanswered_comments > 0 {
        return (Comments, open_reasons(row));
    }
    if !row.draft
        && approved
        && matches!(ci, None | Some("SUCCESS"))
        && row.mergeable.as_deref() == Some("MERGEABLE")
    {
        return (Ready, vec!["approved, green".to_owned()]);
    }
    if row.draft {
        return (Draft, vec!["draft".to_owned()]);
    }
    if row.reviewers.is_empty() {
        return (NoReviewer, vec!["nobody asked to review".to_owned()]);
    }
    if matches!(ci, Some("PENDING" | "EXPECTED")) {
        return (CiRunning, vec!["checks running".to_owned()]);
    }
    (InReview, vec!["waiting on reviewer".to_owned()])
}

/// "approved by notrueblood, you": who actually gave the verdict.
fn verdict_reason(row: &PrDashRow, state: &str, label: &str) -> String {
    let mut who: Vec<&str> = row
        .reviewers
        .iter()
        .filter(|r| r.state == state)
        .map(|r| r.login.as_str())
        .collect();
    if row.my_review.as_deref() == Some(state) {
        who.push("you");
    }
    if who.is_empty() {
        label.to_owned()
    } else {
        format!("{label} by {}", who.join(", "))
    }
}

fn commented_reason(row: &PrDashRow) -> String {
    if row.unresolved == 0 {
        return "you commented, no reply since".to_owned();
    }
    let total = row.unresolved + row.answered_threads;
    format!(
        "{} of {total} of your threads replied to so far",
        row.unresolved
    )
}

fn classify_incoming(row: &PrDashRow) -> (PrDashBucket, Vec<String>) {
    use PrDashBucket::*;
    match row.state.as_str() {
        "MERGED" => return (Merged, vec!["merged".to_owned()]),
        "CLOSED" => return (Closed, vec!["closed without merging".to_owned()]),
        _ => {}
    }
    if row.draft {
        return (DraftIn, vec!["draft".to_owned()]);
    }
    let mine = row.my_review.as_deref();
    // A reply on one of your threads while others still have your comment as
    // the last word means the author is partway through your feedback.
    let replies_pending = row.unresolved > 0 && row.answered_threads == 0;
    let back_with_me = replies_pending || row.unanswered_comments > 0;

    // GitHub's verdict is null when no review is required; fall back to the
    // reviews actually on the PR, a block outranking an approval.
    let mut decision = row.review_decision.as_deref();
    if decision.is_none() {
        let has =
            |state: &str| row.reviewers.iter().any(|r| r.state == state) || mine == Some(state);
        decision = if has("CHANGES_REQUESTED") {
            Some("CHANGES_REQUESTED")
        } else if has("APPROVED") {
            Some("APPROVED")
        } else {
            None
        };
    }

    let blocking =
        row.blocking_changes || (mine == Some("CHANGES_REQUESTED") && !row.requested_from_me);
    if blocking {
        return (
            ChangesRequested,
            vec![verdict_reason(
                row,
                "CHANGES_REQUESTED",
                "changes requested",
            )],
        );
    }
    if decision == Some("APPROVED") {
        return (Approved, vec![verdict_reason(row, "APPROVED", "approved")]);
    }

    if row.requested_from_me {
        if matches!(mine, Some("APPROVED" | "CHANGES_REQUESTED" | "COMMENTED")) {
            return (
                ReReview,
                vec!["re-review asked after you reviewed".to_owned()],
            );
        }
        if row
            .my_last_comment_at
            .as_deref()
            .is_some_and(|s| !s.is_empty())
            && !back_with_me
        {
            return (CommentedByYou, vec![commented_reason(row)]);
        }
        return (ToReview, vec!["asked to review".to_owned()]);
    }
    if back_with_me {
        return (ToReview, open_reasons(row));
    }
    if mine == Some("COMMENTED") || row.unresolved > 0 {
        return (CommentedByYou, vec![commented_reason(row)]);
    }
    (ToReview, vec!["waiting on you".to_owned()])
}

/// The tab badge's urgent count over rows admitted by the hide-drafts toggle.
pub fn needs_you_count(tab: PrDashTab, rows: &[PrDashRow], hide_drafts: bool) -> usize {
    let alert = tab.alert_buckets();
    rows.iter()
        .filter(|r| !(hide_drafts && r.draft))
        .filter(|r| alert.contains(&r.bucket))
        .count()
}

/// Newest first; parsed RFC 3339 instants compare as times, anything
/// unparsable falls back to string comparison (parsed sorts before unparsed
/// so the order stays total).
fn updated_desc(a: &str, b: &str) -> std::cmp::Ordering {
    use chrono::DateTime;
    match (
        DateTime::parse_from_rfc3339(a),
        DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(x), Ok(y)) => y.cmp(&x),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => b.cmp(a),
    }
}

/// Rows as the page shows them: drafts dropped when `hide_drafts`, then
/// (`group_by_bucket`) grouped in the tab's bucket order, each group sorted by
/// `updated_at` descending (stable, so equal keys keep search order). Buckets
/// not in the tab's list come last.
pub fn ordered_rows(
    tab: PrDashTab,
    rows: &[PrDashRow],
    hide_drafts: bool,
    group_by_bucket: bool,
) -> Vec<&PrDashRow> {
    let order = tab.buckets();
    let group = |r: &PrDashRow| {
        if group_by_bucket {
            order
                .iter()
                .position(|b| *b == r.bucket)
                .unwrap_or(order.len())
        } else {
            0
        }
    };
    let mut out: Vec<&PrDashRow> = rows.iter().filter(|r| !(hide_drafts && r.draft)).collect();
    out.sort_by(|a, b| {
        group(a)
            .cmp(&group(b))
            .then_with(|| updated_desc(&a.updated_at, &b.updated_at))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::PrDashBucket::*;
    use super::PrDashTab::{Authored, ReviewRequested};
    use super::*;

    fn open() -> PrDashRow {
        PrDashRow {
            state: "OPEN".into(),
            ..Default::default()
        }
    }

    fn reviewer(login: &str, state: &str) -> PrDashReviewer {
        PrDashReviewer {
            login: login.into(),
            state: state.into(),
        }
    }

    fn check(name: &str) -> PrDashCheck {
        PrDashCheck {
            name: name.into(),
            url: None,
        }
    }

    fn s(v: &str) -> Option<String> {
        Some(v.to_owned())
    }

    fn authored(r: &PrDashRow) -> PrDashBucket {
        classify(Authored, r).0
    }

    fn incoming(r: &PrDashRow) -> PrDashBucket {
        classify(ReviewRequested, r).0
    }

    /// An approved, green, mergeable, reviewed PR (the `ready` shape).
    fn ready_row() -> PrDashRow {
        PrDashRow {
            review_decision: s("APPROVED"),
            ci: s("SUCCESS"),
            mergeable: s("MERGEABLE"),
            reviewers: vec![reviewer("bob", "APPROVED")],
            ..open()
        }
    }

    #[test]
    fn authored_merged_and_closed_come_first() {
        let merged = PrDashRow {
            state: "MERGED".into(),
            ci: s("FAILURE"),
            blocking_changes: true,
            unresolved: 3,
            ..open()
        };
        assert_eq!(authored(&merged), Merged);
        let closed = PrDashRow {
            state: "CLOSED".into(),
            draft: true,
            ..open()
        };
        assert_eq!(authored(&closed), Closed);
    }

    #[test]
    fn authored_approved_conflict_is_needs_you() {
        let r = PrDashRow {
            mergeable: s("CONFLICTING"),
            ..ready_row()
        };
        let (b, reasons) = classify(Authored, &r);
        assert_eq!(b, NeedsYou);
        assert_eq!(reasons, vec!["approved, but has a merge conflict"]);
    }

    #[test]
    fn authored_unapproved_conflict_is_not_needs_you() {
        let r = PrDashRow {
            mergeable: s("CONFLICTING"),
            review_decision: s("REVIEW_REQUIRED"),
            reviewers: vec![reviewer("bob", "REQUESTED")],
            ..open()
        };
        assert_eq!(authored(&r), InReview);
    }

    #[test]
    fn authored_draft_conflict_is_not_a_reason() {
        let r = PrDashRow {
            draft: true,
            mergeable: s("CONFLICTING"),
            ..ready_row()
        };
        assert_eq!(classify(Authored, &r), (Draft, vec!["draft".to_owned()]));
    }

    #[test]
    fn authored_ci_failure_reasons() {
        let mut r = PrDashRow {
            ci: s("FAILURE"),
            ..open()
        };
        assert_eq!(
            classify(Authored, &r),
            (NeedsYou, vec!["CI failing".into()])
        );
        r.failing_checks = vec![check("lint")];
        assert_eq!(classify(Authored, &r).1, vec!["1 check failing"]);
        r.failing_checks = vec![check("a"), check("b"), check("c")];
        assert_eq!(classify(Authored, &r).1, vec!["3 checks failing"]);
    }

    #[test]
    fn authored_ci_error_is_needs_you() {
        let r = PrDashRow {
            ci: s("ERROR"),
            failing_checks: vec![check("a"), check("b")],
            ..open()
        };
        assert_eq!(
            classify(Authored, &r),
            (NeedsYou, vec!["2 checks failing".to_owned()])
        );
    }

    #[test]
    fn authored_changes_requested_is_needs_you_even_as_draft() {
        let r = PrDashRow {
            blocking_changes: true,
            draft: true,
            ..open()
        };
        assert_eq!(
            classify(Authored, &r),
            (NeedsYou, vec!["changes requested".to_owned()])
        );
    }

    #[test]
    fn authored_reasons_accumulate_in_order() {
        let r = PrDashRow {
            mergeable: s("CONFLICTING"),
            ci: s("FAILURE"),
            blocking_changes: true,
            review_decision: s("APPROVED"),
            ..open()
        };
        assert_eq!(
            classify(Authored, &r).1,
            vec![
                "approved, but has a merge conflict",
                "CI failing",
                "changes requested"
            ]
        );
    }

    #[test]
    fn authored_needs_you_beats_comments() {
        let r = PrDashRow {
            ci: s("FAILURE"),
            unresolved: 2,
            unanswered_comments: 1,
            ..open()
        };
        assert_eq!(authored(&r), NeedsYou);
    }

    #[test]
    fn authored_comments_via_unresolved() {
        let r = PrDashRow {
            unresolved: 2,
            ..ready_row()
        };
        let (b, reasons) = classify(Authored, &r);
        assert_eq!(b, Comments);
        assert_eq!(reasons, vec!["2 unresolved threads"]);
    }

    #[test]
    fn authored_comments_via_unanswered() {
        let r = PrDashRow {
            unanswered_comments: 1,
            ..open()
        };
        let (b, reasons) = classify(Authored, &r);
        assert_eq!(b, Comments);
        assert_eq!(reasons, vec!["1 unanswered comment"]);
        let both = PrDashRow {
            unresolved: 1,
            unanswered_comments: 4,
            draft: true,
            ..open()
        };
        assert_eq!(
            classify(Authored, &both),
            (
                Comments,
                vec![
                    "1 unresolved thread".to_owned(),
                    "4 unanswered comments".to_owned()
                ]
            )
        );
    }

    #[test]
    fn authored_ready() {
        assert_eq!(authored(&ready_row()), Ready);
        let no_ci = PrDashRow {
            ci: None,
            ..ready_row()
        };
        assert_eq!(authored(&no_ci), Ready);
    }

    #[test]
    fn authored_ready_is_blocked_by_ci_mergeable_and_draft() {
        let pending = PrDashRow {
            ci: s("PENDING"),
            ..ready_row()
        };
        assert_eq!(authored(&pending), CiRunning);
        let cancelled = PrDashRow {
            ci: s("CANCELLED"),
            ..ready_row()
        };
        assert_eq!(authored(&cancelled), InReview);
        let unknown = PrDashRow {
            mergeable: s("UNKNOWN"),
            ..ready_row()
        };
        assert_eq!(authored(&unknown), InReview);
        let none = PrDashRow {
            mergeable: None,
            ..ready_row()
        };
        assert_eq!(authored(&none), InReview);
        let draft = PrDashRow {
            draft: true,
            ..ready_row()
        };
        assert_eq!(authored(&draft), Draft);
    }

    #[test]
    fn authored_approved_by_hidden_bot_that_is_not_ready_is_no_reviewer() {
        let r = PrDashRow {
            reviewers: vec![],
            mergeable: s("UNKNOWN"),
            ..ready_row()
        };
        assert_eq!(authored(&r), NoReviewer);
    }

    #[test]
    fn authored_draft() {
        let r = PrDashRow {
            draft: true,
            reviewers: vec![reviewer("bob", "REQUESTED")],
            ..open()
        };
        assert_eq!(authored(&r), Draft);
    }

    #[test]
    fn authored_no_reviewer_and_draft_beats_it() {
        assert_eq!(authored(&open()), NoReviewer);
        let r = PrDashRow {
            draft: true,
            ..open()
        };
        assert_eq!(authored(&r), Draft);
    }

    #[test]
    fn authored_ci_running_pending_and_expected() {
        for ci in ["PENDING", "EXPECTED"] {
            let r = PrDashRow {
                ci: s(ci),
                reviewers: vec![reviewer("bob", "REQUESTED")],
                ..open()
            };
            assert_eq!(authored(&r), CiRunning, "{ci}");
        }
        // No reviewer outranks ci-running.
        let r = PrDashRow {
            ci: s("PENDING"),
            ..open()
        };
        assert_eq!(authored(&r), NoReviewer);
    }

    #[test]
    fn authored_in_review() {
        let r = PrDashRow {
            reviewers: vec![reviewer("bob", "COMMENTED")],
            ci: s("SUCCESS"),
            ..open()
        };
        assert_eq!(authored(&r), InReview);
    }

    // ---- review-requested tab ----

    #[test]
    fn incoming_merged_closed_and_draft() {
        let merged = PrDashRow {
            state: "MERGED".into(),
            draft: true,
            ..open()
        };
        assert_eq!(incoming(&merged), Merged);
        let closed = PrDashRow {
            state: "CLOSED".into(),
            ..open()
        };
        assert_eq!(incoming(&closed), Closed);
        let draft = PrDashRow {
            draft: true,
            blocking_changes: true,
            requested_from_me: true,
            ..open()
        };
        assert_eq!(classify(ReviewRequested, &draft).0, DraftIn);
    }

    #[test]
    fn incoming_changes_requested_via_blocking_changes() {
        let r = PrDashRow {
            blocking_changes: true,
            requested_from_me: true,
            reviewers: vec![
                reviewer("bob", "CHANGES_REQUESTED"),
                reviewer("amy", "APPROVED"),
            ],
            review_decision: s("APPROVED"),
            ..open()
        };
        let (b, reasons) = classify(ReviewRequested, &r);
        assert_eq!(b, ChangesRequested);
        assert_eq!(reasons, vec!["changes requested by bob"]);
    }

    #[test]
    fn incoming_changes_requested_via_my_review_only_when_not_requested() {
        let mine = PrDashRow {
            my_review: s("CHANGES_REQUESTED"),
            reviewers: vec![reviewer("bob", "CHANGES_REQUESTED")],
            ..open()
        };
        let (b, reasons) = classify(ReviewRequested, &mine);
        assert_eq!(b, ChangesRequested);
        assert_eq!(reasons, vec!["changes requested by bob, you"]);

        let only_me = PrDashRow {
            my_review: s("CHANGES_REQUESTED"),
            ..open()
        };
        assert_eq!(
            classify(ReviewRequested, &only_me).1,
            vec!["changes requested by you"]
        );

        let re_requested = PrDashRow {
            my_review: s("CHANGES_REQUESTED"),
            requested_from_me: true,
            ..open()
        };
        assert_eq!(incoming(&re_requested), ReReview);
    }

    #[test]
    fn incoming_approved_via_review_decision() {
        let r = PrDashRow {
            review_decision: s("APPROVED"),
            requested_from_me: true,
            reviewers: vec![reviewer("amy", "APPROVED"), reviewer("bob", "COMMENTED")],
            my_review: s("APPROVED"),
            ..open()
        };
        let (b, reasons) = classify(ReviewRequested, &r);
        assert_eq!(b, Approved);
        assert_eq!(reasons, vec!["approved by amy, you"]);
    }

    #[test]
    fn incoming_approved_via_fallback_when_decision_none() {
        let r = PrDashRow {
            review_decision: None,
            reviewers: vec![reviewer("amy", "APPROVED")],
            requested_from_me: true,
            ..open()
        };
        assert_eq!(incoming(&r), Approved);
        // My own approval alone counts too.
        let mine = PrDashRow {
            review_decision: None,
            my_review: s("APPROVED"),
            ..open()
        };
        assert_eq!(incoming(&mine), Approved);
        // REVIEW_REQUIRED does not take the fallback.
        let required = PrDashRow {
            review_decision: s("REVIEW_REQUIRED"),
            reviewers: vec![reviewer("amy", "APPROVED")],
            requested_from_me: true,
            ..open()
        };
        assert_eq!(incoming(&required), ToReview);
    }

    #[test]
    fn incoming_changes_in_fallback_does_not_approve() {
        // decision becomes CHANGES_REQUESTED (not APPROVED) -> falls through.
        let r = PrDashRow {
            review_decision: None,
            reviewers: vec![reviewer("amy", "APPROVED")],
            my_review: s("CHANGES_REQUESTED"),
            requested_from_me: true,
            ..open()
        };
        assert_eq!(incoming(&r), ReReview);
    }

    #[test]
    fn incoming_re_review_for_each_prior_verdict() {
        for mine in ["APPROVED", "CHANGES_REQUESTED", "COMMENTED"] {
            let r = PrDashRow {
                my_review: s(mine),
                requested_from_me: true,
                review_decision: s("REVIEW_REQUIRED"),
                ..open()
            };
            let (b, reasons) = classify(ReviewRequested, &r);
            assert_eq!(b, ReReview, "{mine}");
            assert_eq!(reasons, vec!["re-review asked after you reviewed"]);
        }
    }

    #[test]
    fn incoming_commented_by_you_when_requested_and_not_back_with_me() {
        let r = PrDashRow {
            requested_from_me: true,
            my_last_comment_at: s("2026-01-01T00:00:00Z"),
            ..open()
        };
        assert_eq!(incoming(&r), CommentedByYou);
        // Empty string does not count as having commented.
        let empty = PrDashRow {
            my_last_comment_at: s(""),
            ..r.clone()
        };
        assert_eq!(incoming(&empty), ToReview);
        // Replies pending on my threads (unresolved, none answered) -> back with me.
        let back = PrDashRow {
            unresolved: 1,
            ..r.clone()
        };
        assert_eq!(incoming(&back), ToReview);
        // One thread answered by me last -> replies_pending is false.
        let partway = PrDashRow {
            unresolved: 1,
            answered_threads: 1,
            ..r
        };
        assert_eq!(incoming(&partway), CommentedByYou);
    }

    #[test]
    fn incoming_to_review_when_requested() {
        let r = PrDashRow {
            requested_from_me: true,
            ..open()
        };
        assert_eq!(incoming(&r), ToReview);
        // Dismissed verdict falls through to the comment checks.
        let dismissed = PrDashRow {
            my_review: s("DISMISSED"),
            requested_from_me: true,
            ..open()
        };
        assert_eq!(incoming(&dismissed), ToReview);
        // Unanswered comments push requested+commented back to to-review.
        let remarks = PrDashRow {
            my_last_comment_at: s("2026-01-01T00:00:00Z"),
            unanswered_comments: 2,
            requested_from_me: true,
            ..open()
        };
        assert_eq!(incoming(&remarks), ToReview);
    }

    #[test]
    fn incoming_back_with_me_is_to_review_with_reasons() {
        let r = PrDashRow {
            my_review: s("COMMENTED"),
            unresolved: 2,
            ..open()
        };
        let (b, reasons) = classify(ReviewRequested, &r);
        assert_eq!(b, ToReview);
        assert_eq!(reasons, vec!["2 unresolved threads"]);
        let remarks = PrDashRow {
            my_review: s("COMMENTED"),
            unanswered_comments: 1,
            ..open()
        };
        assert_eq!(incoming(&remarks), ToReview);
    }

    #[test]
    fn incoming_commented_by_you_via_mine_or_unresolved() {
        let mine = PrDashRow {
            my_review: s("COMMENTED"),
            ..open()
        };
        let (b, reasons) = classify(ReviewRequested, &mine);
        assert_eq!(b, CommentedByYou);
        assert_eq!(reasons, vec!["you commented, no reply since"]);
        // unresolved with an answered thread: not replies_pending, still commented.
        let threads = PrDashRow {
            unresolved: 1,
            answered_threads: 2,
            ..open()
        };
        let (b, reasons) = classify(ReviewRequested, &threads);
        assert_eq!(b, CommentedByYou);
        assert_eq!(reasons, vec!["1 of 3 of your threads replied to so far"]);
    }

    #[test]
    fn incoming_to_review_fallback() {
        assert_eq!(
            classify(ReviewRequested, &open()),
            (ToReview, vec!["waiting on you".to_owned()])
        );
    }

    #[test]
    fn incoming_blocking_and_approved_outrank_a_request() {
        let r = PrDashRow {
            requested_from_me: true,
            blocking_changes: true,
            ..open()
        };
        assert_eq!(incoming(&r), ChangesRequested);
    }

    // ---- counts and ordering ----

    fn row(n: u64, bucket: PrDashBucket, draft: bool, updated_at: &str) -> PrDashRow {
        PrDashRow {
            number: n,
            bucket,
            draft,
            updated_at: updated_at.into(),
            ..open()
        }
    }

    fn numbers(rows: &[&PrDashRow]) -> Vec<u64> {
        rows.iter().map(|r| r.number).collect()
    }

    #[test]
    fn needs_you_count_authored() {
        let rows = vec![
            row(1, NeedsYou, false, ""),
            row(2, NeedsYou, true, ""),
            row(3, Comments, false, ""),
            row(4, ToReview, false, ""),
            row(5, Draft, true, ""),
        ];
        assert_eq!(needs_you_count(Authored, &rows, false), 2);
        assert_eq!(needs_you_count(Authored, &rows, true), 1);
        assert_eq!(needs_you_count(Authored, &[], true), 0);
    }

    #[test]
    fn needs_you_count_review_requested() {
        let rows = vec![
            row(1, ToReview, false, ""),
            row(2, ReReview, false, ""),
            row(3, ReReview, true, ""),
            row(4, CommentedByYou, false, ""),
            row(5, NeedsYou, false, ""),
            row(6, Approved, false, ""),
        ];
        assert_eq!(needs_you_count(ReviewRequested, &rows, false), 3);
        assert_eq!(needs_you_count(ReviewRequested, &rows, true), 2);
    }

    #[test]
    fn ordered_rows_grouped_in_bucket_order_then_updated_desc() {
        let rows = vec![
            row(1, InReview, false, "2026-01-01T00:00:00Z"),
            row(2, NeedsYou, false, "2026-01-02T00:00:00Z"),
            row(3, InReview, false, "2026-01-05T00:00:00Z"),
            row(4, NeedsYou, false, "2026-01-03T00:00:00Z"),
            row(5, Ready, false, "2026-01-04T00:00:00Z"),
        ];
        let out = ordered_rows(Authored, &rows, false, true);
        assert_eq!(numbers(&out), vec![4, 2, 5, 3, 1]);
    }

    #[test]
    fn ordered_rows_ungrouped_sorts_globally() {
        let rows = vec![
            row(1, InReview, false, "2026-01-01T00:00:00Z"),
            row(2, NeedsYou, false, "2026-01-02T00:00:00Z"),
            row(3, Ready, false, "2026-01-05T00:00:00Z"),
        ];
        let out = ordered_rows(Authored, &rows, false, false);
        assert_eq!(numbers(&out), vec![3, 2, 1]);
    }

    #[test]
    fn ordered_rows_ties_keep_input_order() {
        let t = "2026-01-01T00:00:00Z";
        let rows = vec![
            row(1, NeedsYou, false, t),
            row(2, NeedsYou, false, t),
            row(3, NeedsYou, false, t),
        ];
        assert_eq!(
            numbers(&ordered_rows(Authored, &rows, false, true)),
            vec![1, 2, 3]
        );
        assert_eq!(
            numbers(&ordered_rows(Authored, &rows, false, false)),
            vec![1, 2, 3]
        );
        // Same instant written with different offsets is a tie too.
        let rows = vec![
            row(1, NeedsYou, false, "2026-01-01T01:00:00+01:00"),
            row(2, NeedsYou, false, "2026-01-01T00:00:00Z"),
        ];
        assert_eq!(
            numbers(&ordered_rows(Authored, &rows, false, true)),
            vec![1, 2]
        );
    }

    #[test]
    fn ordered_rows_compares_instants_not_strings() {
        // As strings "…+09:00" sorts after "…Z", but it is the earlier instant.
        let rows = vec![
            row(1, NeedsYou, false, "2026-01-01T10:00:00+09:00"),
            row(2, NeedsYou, false, "2026-01-01T05:00:00Z"),
        ];
        assert_eq!(
            numbers(&ordered_rows(Authored, &rows, false, false)),
            vec![2, 1]
        );
    }

    #[test]
    fn ordered_rows_unparsable_timestamps_fall_back_to_strings() {
        let rows = vec![
            row(1, NeedsYou, false, "a"),
            row(2, NeedsYou, false, "c"),
            row(3, NeedsYou, false, "b"),
        ];
        assert_eq!(
            numbers(&ordered_rows(Authored, &rows, false, false)),
            vec![2, 3, 1]
        );
    }

    #[test]
    fn ordered_rows_hide_drafts() {
        let rows = vec![
            row(1, NeedsYou, true, "2026-01-03T00:00:00Z"),
            row(2, NeedsYou, false, "2026-01-02T00:00:00Z"),
            row(3, Draft, true, "2026-01-01T00:00:00Z"),
        ];
        assert_eq!(numbers(&ordered_rows(Authored, &rows, true, true)), vec![2]);
        assert_eq!(
            numbers(&ordered_rows(Authored, &rows, false, true)),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn ordered_rows_unknown_buckets_go_last() {
        let rows = vec![
            row(1, ToReview, false, "2026-01-09T00:00:00Z"),
            row(2, NeedsYou, false, "2026-01-01T00:00:00Z"),
        ];
        // ToReview is not an Authored bucket.
        assert_eq!(
            numbers(&ordered_rows(Authored, &rows, false, true)),
            vec![2, 1]
        );
        // Review tab order starts with ToReview.
        assert_eq!(
            numbers(&ordered_rows(ReviewRequested, &rows, false, true)),
            vec![1, 2]
        );
    }

    #[test]
    fn review_requested_group_order() {
        let rows = vec![
            row(1, Closed, false, "2026-01-09T00:00:00Z"),
            row(2, Approved, false, "2026-01-08T00:00:00Z"),
            row(3, ReReview, false, "2026-01-07T00:00:00Z"),
            row(4, DraftIn, false, "2026-01-06T00:00:00Z"),
            row(5, ToReview, false, "2026-01-05T00:00:00Z"),
        ];
        assert_eq!(
            numbers(&ordered_rows(ReviewRequested, &rows, false, true)),
            vec![5, 3, 2, 4, 1]
        );
    }
}
