//! The PR page's model (⌘P / ⇧⌘P): the pure half of pr-dash's two tabs
//! ("Authored by me", "Review requested"). Rows arrive classified from the
//! engine's `PrDashboard` RPC; this module owns what the page derives from
//! them: needs-you badges, bucket chips, the text filter, grouping, and the
//! per-column strings of docs/plans/work-outline/pr-dash-spec.md §4-§7. The
//! rendering lives in `chat_type_view/pr_tabs.rs`.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use zeron_proto::{
    ChatLinkSource, PrDashBucket, PrDashChat, PrDashReply, PrDashRow, PrDashTab, PrDashDiff,
};

use zeron_proto::{needs_you_count, ordered_rows};

/// How often the visible tab refetches while the page is rendered.
pub(crate) const DASH_REFRESH: Duration = Duration::from_secs(60);

/// Tests: answers `PrDashboard` params (as JSON) in place of the engine.
#[cfg(test)]
pub(crate) type PrDashScript =
    std::sync::Arc<dyn Fn(&serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync>;

/// One tab of the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PageTab {
    Dash(PrDashTab),
    /// The classification-based review chat table / destination list.
    Chats,
}

impl PageTab {
    pub(crate) const ALL: [PageTab; 3] = [
        PageTab::Dash(PrDashTab::Authored),
        PageTab::Dash(PrDashTab::ReviewRequested),
        PageTab::Chats,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Dash(tab) => tab.label(),
            Self::Chats => "Review chats",
        }
    }

    /// Stable id fragment for element ids and debug selectors.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Dash(PrDashTab::Authored) => "authored",
            Self::Dash(PrDashTab::ReviewRequested) => "review-requested",
            Self::Chats => "chats",
        }
    }
}

pub(crate) fn tab_index(tab: PrDashTab) -> usize {
    match tab {
        PrDashTab::Authored => 0,
        PrDashTab::ReviewRequested => 1,
    }
}

/// `needs-you`, `to-review`, … (the wire name).
pub(crate) fn bucket_key(bucket: PrDashBucket) -> String {
    serde_json::to_value(bucket)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Everything one tab keeps: its last good rows, fetch state and filters.
#[derive(Debug)]
pub(crate) struct DashTabState {
    pub rows: Vec<PrDashRow>,
    pub me: String,
    pub loaded: bool,
    pub pending: bool,
    pub fetched_at: Option<Instant>,
    pub error: Option<String>,
    /// Chip selection (multi-select, OR). Empty = every bucket.
    pub active_buckets: Vec<PrDashBucket>,
    pub hide_drafts: bool,
    pub group: bool,
}

impl Default for DashTabState {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            me: String::new(),
            loaded: false,
            pending: false,
            fetched_at: None,
            error: None,
            active_buckets: Vec::new(),
            hide_drafts: true,
            group: true,
        }
    }
}

impl DashTabState {
    /// A fetch finished. A failure keeps the last good rows and shows the
    /// message inline; so does a reply carrying `error`.
    pub(crate) fn apply(&mut self, result: Result<PrDashReply, String>) {
        self.pending = false;
        self.loaded = true;
        self.fetched_at = Some(Instant::now());
        match result {
            Ok(reply) => {
                self.me = reply.me;
                self.rows = reply.rows;
                self.error = reply.error;
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub(crate) fn toggle_bucket(&mut self, bucket: PrDashBucket) {
        if let Some(at) = self.active_buckets.iter().position(|b| *b == bucket) {
            self.active_buckets.remove(at);
        } else {
            self.active_buckets.push(bucket);
        }
    }

    pub(crate) fn fresh(&self) -> bool {
        self.fetched_at
            .is_some_and(|at| at.elapsed() < DASH_REFRESH - Duration::from_secs(5))
    }
}

/// The tab title's count: `"{urgent} / {n}"` (alert) or `"{n}"`, over rows
/// admitted by hide-drafts (spec §6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Badge {
    pub text: String,
    pub alert: bool,
}

pub(crate) fn badge(tab: PrDashTab, rows: &[PrDashRow], hide_drafts: bool) -> Badge {
    let admitted = admitted_count(rows, hide_drafts);
    let urgent = needs_you_count(tab, rows, hide_drafts);
    if urgent > 0 {
        Badge {
            text: format!("{urgent} / {admitted}"),
            alert: true,
        }
    } else {
        Badge {
            text: admitted.to_string(),
            alert: false,
        }
    }
}

pub(crate) fn admitted_count(rows: &[PrDashRow], hide_drafts: bool) -> usize {
    rows.iter().filter(|row| !hide_drafts || !row.draft).count()
}

/// One chip per bucket of `tab` (in order) with at least one admitted row.
pub(crate) fn chips(
    tab: PrDashTab,
    rows: &[PrDashRow],
    hide_drafts: bool,
) -> Vec<(PrDashBucket, usize)> {
    tab.buckets()
        .iter()
        .filter_map(|bucket| {
            let count = rows
                .iter()
                .filter(|row| (!hide_drafts || !row.draft) && row.bucket == *bucket)
                .count();
            (count > 0).then_some((*bucket, count))
        })
        .collect()
}

/// Spec §4.3: lowercased `title repo head base ticket #number author
/// reviewer logins labels`.
pub(crate) fn haystack(row: &PrDashRow) -> String {
    let mut parts: Vec<&str> = vec![
        &row.title,
        &row.repo,
        &row.head_ref,
        &row.base_ref,
        row.ticket.as_deref().unwrap_or_default(),
    ];
    let number = format!("#{}", row.number);
    parts.push(&number);
    parts.push(&row.author);
    parts.extend(row.reviewers.iter().map(|r| r.login.as_str()));
    parts.extend(row.labels.iter().map(String::as_str));
    parts.join(" ").to_lowercase()
}

/// Every whitespace-separated term must be a substring of the haystack.
pub(crate) fn matches_search(row: &PrDashRow, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let haystack = haystack(row);
    query.split_whitespace().all(|term| haystack.contains(term))
}

/// The page's filter inputs for one tab.
pub(crate) struct Filters<'a> {
    pub hide_drafts: bool,
    pub group: bool,
    pub buckets: &'a [PrDashBucket],
    pub query: &'a str,
}

/// Rows the tab shows, in display order: hide-drafts and grouping/sorting
/// from the shared `ordered_rows`, then the active chips (OR), then the text
/// filter. Chips whose bucket has no admitted row are ignored so a stale
/// selection cannot blank the list.
pub(crate) fn visible_rows<'a>(
    tab: PrDashTab,
    rows: &'a [PrDashRow],
    filters: &Filters<'_>,
) -> Vec<&'a PrDashRow> {
    let present: Vec<PrDashBucket> = chips(tab, rows, filters.hide_drafts)
        .into_iter()
        .map(|(bucket, _)| bucket)
        .collect();
    let active: Vec<PrDashBucket> = filters
        .buckets
        .iter()
        .copied()
        .filter(|bucket| present.contains(bucket))
        .collect();
    ordered_rows(tab, rows, filters.hide_drafts, filters.group)
        .into_iter()
        .filter(|row| active.is_empty() || active.contains(&row.bucket))
        .filter(|row| matches_search(row, filters.query))
        .collect()
}

/// A line of the list: a group header or a row.
#[derive(Clone, Copy, Debug)]
pub(crate) enum DashItem<'a> {
    Header { bucket: PrDashBucket, count: usize },
    Row(&'a PrDashRow),
}

/// `rows` (already in display order) with a header before each bucket run
/// when grouped.
pub(crate) fn items<'a>(rows: &[&'a PrDashRow], group: bool) -> Vec<DashItem<'a>> {
    let mut out = Vec::with_capacity(rows.len() + 4);
    let mut current: Option<PrDashBucket> = None;
    for row in rows {
        if group && current != Some(row.bucket) {
            current = Some(row.bucket);
            out.push(DashItem::Header {
                bucket: row.bucket,
                count: rows.iter().filter(|r| r.bucket == row.bucket).count(),
            });
        }
        out.push(DashItem::Row(row));
    }
    out
}

pub(crate) fn header_text(bucket: PrDashBucket, count: usize) -> String {
    format!("{} {} · {count}", bucket.glyph(), bucket.label())
}

pub(crate) fn chip_text(bucket: PrDashBucket, count: usize) -> String {
    format!("{} {count}", bucket.label())
}

pub(crate) fn summary_text(shown: usize, total: usize) -> String {
    format!("{shown} of {total}")
}

/// Spec §5 `compact()`.
pub(crate) fn compact(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 10_000 {
        let text = format!("{:.1}", n as f64 / 1000.0);
        format!("{}k", text.strip_suffix(".0").unwrap_or(&text))
    } else {
        format!("{}k", (n as f64 / 1000.0).round() as u64)
    }
}

/// Only new code files changed (spec §3.6).
pub(crate) fn pure_addition(diff: &PrDashDiff) -> bool {
    diff.code_new_lines > 0 && diff.code_churn == 0
}

/// The Churn column in code-only mode: the main text and the excluded-lines
/// superscript (`+{compact}` of test + generated + docs lines).
pub(crate) fn churn(diff: &PrDashDiff) -> (String, Option<String>) {
    let main = if pure_addition(diff) {
        format!("⊕{} new", compact(diff.code_new_lines))
    } else if diff.code_new_lines > 0 {
        format!(
            "{} ⊕{}",
            compact(diff.code_churn),
            compact(diff.code_new_lines)
        )
    } else {
        compact(diff.code_churn)
    };
    let excluded = diff.test_lines + diff.generated_lines + diff.docs_lines;
    (main, (excluded > 0).then(|| format!("+{}", compact(excluded))))
}

/// Spec §5 `rel()`: `now|{m}m|{h}h|{d}d|{n}mo`, negative clamps to 0.
pub(crate) fn relative_time(updated_at: &str, now: DateTime<Utc>) -> String {
    let Ok(at) = DateTime::parse_from_rfc3339(updated_at) else {
        return String::new();
    };
    let secs = (now - at.with_timezone(&Utc)).num_seconds().max(0);
    if secs < 60 {
        "now".to_owned()
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else if secs < 30 * 86_400 {
        format!("{}d", secs / 86_400)
    } else {
        format!("{}mo", secs / (30 * 86_400))
    }
}

/// How a glyph or tag is colored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Plain,
    Ok,
    Bad,
    Warn,
    Muted,
}

/// Reviewer chip glyph (spec §5).
pub(crate) fn reviewer_glyph(state: &str) -> (&'static str, Tone) {
    match state {
        "APPROVED" => ("✓", Tone::Ok),
        "CHANGES_REQUESTED" => ("✗", Tone::Bad),
        "COMMENTED" => ("…", Tone::Warn),
        "DISMISSED" => ("◌", Tone::Muted),
        "REQUESTED" => ("○", Tone::Muted),
        "RE_REQUESTED" => ("↻", Tone::Warn),
        _ => ("·", Tone::Muted),
    }
}

/// CI column glyph (spec §5).
pub(crate) fn ci_glyph(ci: Option<&str>) -> (&'static str, Tone) {
    match ci {
        Some("SUCCESS") => ("✓", Tone::Ok),
        Some("FAILURE" | "ERROR") => ("✗", Tone::Bad),
        Some("PENDING" | "EXPECTED") => ("◔", Tone::Warn),
        Some("CANCELLED") => ("⊘", Tone::Muted),
        _ => ("—", Tone::Muted),
    }
}

pub(crate) fn ci_failing(row: &PrDashRow) -> bool {
    matches!(row.ci.as_deref(), Some("FAILURE" | "ERROR"))
}

/// The Open column: unresolved threads plus unanswered comments, and the
/// `+n` superscript of answered and outdated threads.
pub(crate) fn open_count(row: &PrDashRow) -> (u64, Option<u64>) {
    let extra = row.answered_threads + row.outdated_threads;
    (
        row.unresolved + row.unanswered_comments,
        (extra > 0).then_some(extra),
    )
}

/// A tag after the title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tag {
    pub text: String,
    pub tone: Tone,
    pub url: Option<String>,
}

impl Tag {
    fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
            url: None,
        }
    }
}

fn stacked(row: &PrDashRow) -> bool {
    !row.base_ref.is_empty() && !["main", "master", "develop", "dev"].contains(&row.base_ref.as_str())
}

/// Spec §5 title tags in order.
pub(crate) fn title_tags(row: &PrDashRow) -> Vec<Tag> {
    let mut tags = Vec::new();
    if row.draft {
        tags.push(Tag::new("draft", Tone::Muted));
    }
    if row.stack_pos > 0 && row.stack_size > 0 {
        tags.push(Tag::new(
            format!("{}/{}", row.stack_pos, row.stack_size),
            Tone::Plain,
        ));
    } else if stacked(row) {
        tags.push(Tag::new(format!("↵ {}", row.base_ref), Tone::Plain));
    }
    if let Some(ticket) = &row.ticket {
        tags.push(Tag::new(ticket.clone(), Tone::Plain));
    }
    if pure_addition(&row.diff) {
        tags.push(Tag::new("all new files", Tone::Plain));
    }
    let shown_reasons: &[String] = if matches!(
        row.bucket,
        PrDashBucket::NeedsYou | PrDashBucket::Comments
    ) {
        &row.reasons
    } else {
        &[]
    };
    for reason in shown_reasons {
        tags.push(Tag::new(reason.clone(), Tone::Warn));
    }
    let conflicting = row.mergeable.as_deref() == Some("CONFLICTING")
        || row.merge_state.as_deref() == Some("DIRTY");
    if conflicting
        && !shown_reasons
            .iter()
            .any(|reason| reason.to_lowercase().contains("conflict"))
    {
        tags.push(Tag::new("conflict", Tone::Bad));
    }
    for check in row.failing_checks.iter().take(3) {
        tags.push(Tag {
            text: check.name.clone(),
            tone: Tone::Bad,
            url: check.url.clone(),
        });
    }
    if !row.cancelled_checks.is_empty() {
        tags.push(Tag::new(
            format!("{} cancelled", row.cancelled_checks.len()),
            Tone::Muted,
        ));
    }
    for label in row.labels.iter().take(3) {
        tags.push(Tag::new(label.clone(), Tone::Plain));
    }
    tags
}

/// Linked-chat chip text: the title, truncated.
pub(crate) fn chat_chip_label(chat: &PrDashChat) -> String {
    const MAX: usize = 28;
    let title = chat.title.trim();
    let title = if title.is_empty() { "Untitled chat" } else { title };
    if title.chars().count() > MAX {
        let cut: String = title.chars().take(MAX - 1).collect();
        format!("{}…", cut.trim_end())
    } else {
        title.to_owned()
    }
}

/// Hover text of a linked-chat chip: how the chat was matched.
pub(crate) fn link_source_text(source: ChatLinkSource) -> &'static str {
    match source {
        ChatLinkSource::Manual => "Linked manually",
        ChatLinkSource::CreatedInChat => "PR created in this chat",
        ChatLinkSource::Mentioned => "Mentioned in conversation",
        other => other.inferred_label().unwrap_or("Linked"),
    }
}

/// The chat the Review button reopens instead of starting a new one: the
/// best non-archived chat the review launcher started.
pub(crate) fn review_chat(row: &PrDashRow) -> Option<&PrDashChat> {
    row.chats.iter().find(|chat| chat.is_review && !chat.archived)
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    /// A row with the facts the page reads; `updated` is an RFC 3339 time.
    pub(crate) fn row(number: u64, bucket: PrDashBucket, updated: &str) -> PrDashRow {
        PrDashRow {
            number,
            title: format!("PR {number} title"),
            url: format!("https://github.com/acme/flagship/pull/{number}"),
            repo_full: "acme/flagship".into(),
            repo: "flagship".into(),
            author: "octo".into(),
            state: "OPEN".into(),
            head_ref: format!("feature-{number}"),
            base_ref: "main".into(),
            created_at: updated.into(),
            updated_at: updated.into(),
            bucket,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::row;
    use super::*;
    use zeron_proto::{PrDashCheck, PrDashReviewer};

    const T1: &str = "2026-10-20T10:00:00Z";
    const T2: &str = "2026-10-21T10:00:00Z";
    const T3: &str = "2026-10-22T10:00:00Z";

    #[test]
    fn compact_follows_the_spec() {
        assert_eq!(compact(7), "7");
        assert_eq!(compact(999), "999");
        assert_eq!(compact(1000), "1k");
        assert_eq!(compact(1234), "1.2k");
        assert_eq!(compact(9999), "10k");
        assert_eq!(compact(12_400), "12k");
        assert_eq!(compact(12_600), "13k");
    }

    #[test]
    fn churn_text_is_code_only_with_superscript() {
        let mut diff = PrDashDiff::default();
        diff.code_new_lines = 1200;
        assert_eq!(churn(&diff), ("⊕1.2k new".to_owned(), None));
        diff.code_churn = 40;
        assert_eq!(churn(&diff).0, "40 ⊕1.2k");
        diff.code_new_lines = 0;
        diff.test_lines = 90;
        diff.docs_lines = 1000;
        assert_eq!(churn(&diff), ("40".to_owned(), Some("+1.1k".to_owned())));
    }

    #[test]
    fn relative_time_thresholds() {
        let now = DateTime::parse_from_rfc3339("2026-10-23T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let rel = |at: &str| relative_time(at, now);
        assert_eq!(rel("2026-10-23T11:59:30Z"), "now");
        assert_eq!(rel("2026-10-23T12:30:00Z"), "now", "future clamps to now");
        assert_eq!(rel("2026-10-23T11:15:00Z"), "45m");
        assert_eq!(rel("2026-10-23T07:00:00Z"), "5h");
        assert_eq!(rel("2026-10-20T12:00:00Z"), "3d");
        assert_eq!(rel("2026-08-20T12:00:00Z"), "2mo");
    }

    #[test]
    fn badge_is_urgent_over_admitted_rows_only() {
        let mut draft = row(3, PrDashBucket::NeedsYou, T1);
        draft.draft = true;
        let rows = vec![
            row(1, PrDashBucket::NeedsYou, T1),
            row(2, PrDashBucket::InReview, T2),
            draft,
        ];
        let hidden = badge(PrDashTab::Authored, &rows, true);
        assert_eq!(hidden, Badge { text: "1 / 2".into(), alert: true });
        let shown = badge(PrDashTab::Authored, &rows, false);
        assert_eq!(shown, Badge { text: "2 / 3".into(), alert: true });
        let calm = badge(PrDashTab::Authored, &rows[1..2], true);
        assert_eq!(calm, Badge { text: "1".into(), alert: false });
        // The review tab alerts on to-review and re-review.
        let review = vec![
            row(4, PrDashBucket::ReReview, T1),
            row(5, PrDashBucket::Approved, T2),
        ];
        assert_eq!(
            badge(PrDashTab::ReviewRequested, &review, true),
            Badge { text: "1 / 2".into(), alert: true }
        );
    }

    #[test]
    fn chips_follow_bucket_order_and_skip_empty_buckets() {
        let rows = vec![
            row(1, PrDashBucket::InReview, T1),
            row(2, PrDashBucket::NeedsYou, T1),
            row(3, PrDashBucket::InReview, T2),
        ];
        assert_eq!(
            chips(PrDashTab::Authored, &rows, true),
            vec![(PrDashBucket::NeedsYou, 1), (PrDashBucket::InReview, 2)]
        );
    }

    #[test]
    fn visible_rows_group_in_bucket_order_and_sort_newest_first() {
        let rows = vec![
            row(1, PrDashBucket::InReview, T3),
            row(2, PrDashBucket::NeedsYou, T1),
            row(3, PrDashBucket::NeedsYou, T2),
            row(4, PrDashBucket::Ready, T3),
        ];
        let filters = |group, buckets: &'static [PrDashBucket], query| Filters {
            hide_drafts: true,
            group,
            buckets,
            query,
        };
        let numbers = |rows: Vec<&PrDashRow>| rows.iter().map(|r| r.number).collect::<Vec<_>>();
        assert_eq!(
            numbers(visible_rows(PrDashTab::Authored, &rows, &filters(true, &[], ""))),
            vec![3, 2, 4, 1]
        );
        assert_eq!(
            numbers(visible_rows(PrDashTab::Authored, &rows, &filters(false, &[], ""))),
            vec![1, 4, 3, 2]
        );
        assert_eq!(
            numbers(visible_rows(
                PrDashTab::Authored,
                &rows,
                &filters(true, &[PrDashBucket::Ready, PrDashBucket::InReview], "")
            )),
            vec![4, 1],
            "chips OR together"
        );
        // A chip with no rows left never blanks the list.
        assert_eq!(
            numbers(visible_rows(
                PrDashTab::Authored,
                &rows,
                &filters(true, &[PrDashBucket::Closed], "")
            ))
            .len(),
            4
        );
    }

    #[test]
    fn items_insert_headers_per_bucket_run() {
        let rows = vec![
            row(1, PrDashBucket::NeedsYou, T2),
            row(2, PrDashBucket::NeedsYou, T1),
            row(3, PrDashBucket::Ready, T1),
        ];
        let refs: Vec<&PrDashRow> = rows.iter().collect();
        let grouped = items(&refs, true);
        assert_eq!(grouped.len(), 5);
        assert!(matches!(
            grouped[0],
            DashItem::Header { bucket: PrDashBucket::NeedsYou, count: 2 }
        ));
        assert!(matches!(
            grouped[3],
            DashItem::Header { bucket: PrDashBucket::Ready, count: 1 }
        ));
        assert_eq!(items(&refs, false).len(), 3);
        assert_eq!(header_text(PrDashBucket::NeedsYou, 2), "● Needs you · 2");
        assert_eq!(chip_text(PrDashBucket::Ready, 4), "Ready to merge 4");
    }

    #[test]
    fn search_needs_every_term_in_the_spec_haystack() {
        let mut r = row(77, PrDashBucket::InReview, T1);
        r.title = "Add pricing cache".into();
        r.ticket = Some("ENG-12".into());
        r.labels = vec!["backend".into()];
        r.reviewers = vec![PrDashReviewer { login: "alice".into(), state: "APPROVED".into() }];
        for hit in ["pricing", "flagship", "feature-77", "eng-12", "#77", "OCTO", "alice", "backend", "pricing alice"] {
            assert!(matches_search(&r, hit), "{hit}");
        }
        assert!(!matches_search(&r, "pricing bob"));
        assert!(matches_search(&r, "   "));
    }

    #[test]
    fn title_tags_follow_the_column_spec() {
        let mut r = row(1, PrDashBucket::NeedsYou, T1);
        r.draft = true;
        r.stack_pos = 2;
        r.stack_size = 3;
        r.ticket = Some("ENG-9".into());
        r.diff.code_new_lines = 10;
        r.reasons = vec!["2 checks failing".into()];
        r.mergeable = Some("CONFLICTING".into());
        r.failing_checks = (1..=4)
            .map(|n| PrDashCheck { name: format!("check-{n}"), url: Some(format!("https://ci/{n}")) })
            .collect();
        r.cancelled_checks = vec![PrDashCheck { name: "slow".into(), url: None }];
        r.labels = (1..=5).map(|n| format!("l{n}")).collect();
        let texts: Vec<String> = title_tags(&r).into_iter().map(|t| t.text).collect();
        assert_eq!(
            texts,
            [
                "draft", "2/3", "ENG-9", "all new files", "2 checks failing", "conflict",
                "check-1", "check-2", "check-3", "1 cancelled", "l1", "l2", "l3"
            ]
        );
        // A reason that already mentions the conflict suppresses the tag, and
        // reasons only show on needs-you / comments rows.
        r.reasons = vec!["approved, but has a merge conflict".into()];
        assert!(!title_tags(&r).iter().any(|t| t.text == "conflict"));
        r.bucket = PrDashBucket::InReview;
        let tags = title_tags(&r);
        assert!(!tags.iter().any(|t| t.text.contains("approved")));
        assert!(tags.iter().any(|t| t.text == "conflict"));
        // Stacked onto another branch without a stack position.
        let mut s = row(2, PrDashBucket::InReview, T1);
        s.base_ref = "feature-1".into();
        assert_eq!(title_tags(&s)[0].text, "↵ feature-1");
    }

    #[test]
    fn open_count_and_glyphs() {
        let mut r = row(1, PrDashBucket::Comments, T1);
        r.unresolved = 2;
        r.unanswered_comments = 1;
        r.answered_threads = 1;
        r.outdated_threads = 2;
        assert_eq!(open_count(&r), (3, Some(3)));
        assert_eq!(ci_glyph(Some("ERROR")).0, "✗");
        assert_eq!(ci_glyph(None).0, "—");
        assert_eq!(reviewer_glyph("RE_REQUESTED").0, "↻");
    }

    #[test]
    fn review_chat_skips_archived_and_non_review_chats() {
        let chat = |id: &str, archived, is_review| PrDashChat {
            chat_id: id.into(),
            title: id.into(),
            source: ChatLinkSource::Manual,
            archived,
            is_review,
            updated_at: None,
        };
        let mut r = row(1, PrDashBucket::ToReview, T1);
        r.chats = vec![chat("work", false, false), chat("old", true, true)];
        assert!(review_chat(&r).is_none());
        r.chats.push(chat("rev", false, true));
        assert_eq!(review_chat(&r).unwrap().chat_id, "rev");
        let long = PrDashChat { title: "x".repeat(40), ..chat("c", false, false) };
        assert_eq!(chat_chip_label(&long).chars().count(), 28);
        assert_eq!(link_source_text(ChatLinkSource::Branch), "The chat's branch is this PR's branch");
        assert_eq!(link_source_text(ChatLinkSource::Manual), "Linked manually");
    }

    #[test]
    fn apply_keeps_the_last_good_rows_on_failure() {
        let mut state = DashTabState::default();
        state.apply(Ok(PrDashReply {
            me: "me".into(),
            rows: vec![row(1, PrDashBucket::InReview, T1)],
            fetched_at: T1.into(),
            error: None,
        }));
        assert!(state.loaded && state.error.is_none() && state.rows.len() == 1);
        state.apply(Err("boom".into()));
        assert_eq!(state.error.as_deref(), Some("boom"));
        assert_eq!(state.rows.len(), 1);
        state.apply(Ok(PrDashReply { me: "me".into(), rows: Vec::new(), fetched_at: T2.into(), error: Some("rate limited".into()) }));
        assert_eq!(state.error.as_deref(), Some("rate limited"));
    }
}
