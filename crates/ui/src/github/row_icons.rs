//! The compact PR cluster of an Outline row. Fixed-width slots, right side
//! of the row, so they line up as columns:
//!
//! `author · needs (attention) · review · CI · PR`
//!
//! - author: the PR author's login (muted, truncated; "you" when the viewer
//!   is known to be the author);
//! - needs: the item's attention indicator (it replaces the "N active · N
//!   user action required" text on these rows);
//! - review (your part): eye = review requested, refresh = re-review
//!   requested (both in the attention color), check = approved (green),
//!   close-circle = changes requested (orange); empty = nothing for you;
//! - CI: a small dot (green passing, red failing, amber running; none
//!   without checks), a dot rather than ✓/✕ so it can't read as the review;
//! - PR: the pull-request icon colored by state; click opens the PR.
//!
//! Every slot has its own tooltip; hovering the cluster shows the full line.
//! On a narrow row the title keeps [`TITLE_MIN_WIDTH`] and slots drop in the
//! order CI, review, author ([`visible_slots`]); needs and PR never drop.
//! The mapping from state to icon and color is pure so it is unit-tested;
//! [`legend_rows`] feeds the `?` reference from the same tables.
use gpui::{AnyElement, Hsla, SharedString, WeakEntity, div, prelude::*, px};
use zeron_proto::{
    PullRequestCiStatus, PullRequestMyReviewState, PullRequestRef, PullRequestState,
    PullRequestStatusSummary,
};

use super::chip;
use super::store::PrEntry;
use crate::theme::Theme;
use crate::work_outline::chips::TextTooltip;

/// The title keeps at least this much width; slots drop to make room.
pub(crate) const TITLE_MIN_WIDTH: f32 = 120.0;
/// Authors longer than this are cut with an ellipsis (the tooltip has the
/// full login).
pub(crate) const AUTHOR_MAX_CHARS: usize = 10;

const SLOT_GAP: f32 = 4.0;
const SLOT_HEIGHT: f32 = 16.0;
const AUTHOR_WIDTH: f32 = 58.0;
const NEEDS_WIDTH: f32 = 16.0;
const REVIEW_WIDTH: f32 = 12.0;
const CI_WIDTH: f32 = 8.0;
const PR_WIDTH: f32 = 14.0;
/// What the row spends outside the title, the cluster and the other chips:
/// list padding, row padding and border, the status glyph and the gaps.
const ROW_CHROME: f32 = 12.0 + 8.0 + 2.0 + 12.0 + 12.0;

/// A semantic color of an icon, resolved against the theme in one place so
/// the legend and the row never disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    Attention,
    Success,
    Danger,
    Warning,
    Orange,
    Muted,
    Faint,
    Merged,
}

pub(crate) fn tone_color(tone: Tone, theme: &Theme) -> Hsla {
    match tone {
        Tone::Attention => crate::work_outline::chips::attention_color(theme),
        Tone::Success => theme.success,
        Tone::Danger => theme.danger,
        Tone::Warning => theme.warning,
        Tone::Orange => gpui::hsla(25.0 / 360.0, 0.9, 0.55, 1.0),
        Tone::Muted => theme.text_muted,
        Tone::Faint => theme.text_faint,
        Tone::Merged => theme.code_text,
    }
}

/// The PR slot: the pull-request icon, colored by state.
pub(crate) fn pr_icon(state: PullRequestState) -> (&'static str, Tone) {
    let tone = match state {
        PullRequestState::Open => Tone::Success,
        PullRequestState::Draft => Tone::Muted,
        PullRequestState::Merged => Tone::Merged,
        PullRequestState::Closed => Tone::Danger,
    };
    (crate::icons::PULL_REQUEST, tone)
}

/// The CI slot's dot color; `None` when there are no checks.
pub(crate) fn ci_dot(ci: PullRequestCiStatus) -> Option<Tone> {
    match ci {
        PullRequestCiStatus::Passing => Some(Tone::Success),
        PullRequestCiStatus::Failing => Some(Tone::Danger),
        PullRequestCiStatus::Pending => Some(Tone::Warning),
        PullRequestCiStatus::None => None,
    }
}

/// The review slot (your part): icon and color; `None` when there is
/// nothing for you (no review state, or only a comment).
pub(crate) fn review_icon(state: PullRequestMyReviewState) -> Option<(&'static str, Tone)> {
    match state {
        PullRequestMyReviewState::Requested => Some((crate::icons::EYE, Tone::Attention)),
        PullRequestMyReviewState::ReRequested => Some((crate::icons::REFRESH, Tone::Attention)),
        PullRequestMyReviewState::Approved => Some((crate::icons::CHECK, Tone::Success)),
        PullRequestMyReviewState::ChangesRequested => {
            Some((crate::icons::CLOSE_CIRCLE, Tone::Orange))
        }
        PullRequestMyReviewState::Commented | PullRequestMyReviewState::None => None,
    }
}

/// The author slot's text: the login cut to [`AUTHOR_MAX_CHARS`] with an
/// ellipsis, or "you" when `viewer` is the author. `None` without an author.
pub(crate) fn author_text(author: &str, viewer: Option<&str>) -> Option<String> {
    let author = author.trim();
    if author.is_empty() {
        return None;
    }
    if viewer.is_some_and(|viewer| viewer.trim().eq_ignore_ascii_case(author)) {
        return Some("you".to_string());
    }
    if author.chars().count() <= AUTHOR_MAX_CHARS {
        return Some(author.to_string());
    }
    let head: String = author.chars().take(AUTHOR_MAX_CHARS - 1).collect();
    Some(format!("{head}\u{2026}"))
}

pub(crate) fn author_tooltip(author: &str, viewer: Option<&str>) -> String {
    match author_text(author, viewer).as_deref() {
        Some("you") => format!("Author: you ({author})"),
        Some(_) => format!("Author: {author}"),
        None => "Author unknown".to_string(),
    }
}

/// The PR icon's tooltip.
pub(crate) fn pr_tooltip(status: &PullRequestStatusSummary) -> String {
    format!(
        "Pull request {} \u{00b7} {}\n{}\nClick to open on GitHub",
        status.pr_ref.external_id(),
        chip::state_label(status.state),
        status.title
    )
}

/// The cluster's tooltip: `#42 · Open · CI failing · Re-review requested ·
/// by alex · 2 chats` (the review decision only when you have no review state
/// of your own, as in the details chip).
pub(crate) fn group_line(
    status: &PullRequestStatusSummary,
    chat_count: u32,
    viewer: Option<&str>,
) -> String {
    let mut parts = vec![
        format!("#{}", status.pr_ref.number),
        chip::state_label(status.state).to_string(),
    ];
    parts.extend(chip::ci_label(status.ci_status).map(str::to_string));
    match chip::my_review_label(status.my_review_state) {
        Some(mine) => parts.push(mine.to_string()),
        None if !status.state.is_finished() => {
            parts.extend(chip::review_decision_label(status.review_decision).map(str::to_string))
        }
        None => {}
    }
    if let Some(author) = author_text(&status.author, viewer) {
        parts.push(if author == "you" {
            "by you".to_string()
        } else {
            format!("by {}", status.author.trim())
        });
    }
    if chat_count > 0 {
        parts.push(format!(
            "{chat_count} chat{}",
            if chat_count == 1 { "" } else { "s" }
        ));
    }
    parts.join(" \u{00b7} ")
}

/// Which droppable slots are shown. Needs and PR are not listed: they are
/// always shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VisibleSlots {
    pub author: bool,
    pub review: bool,
    pub ci: bool,
}

impl VisibleSlots {
    pub(crate) const ALL: Self = Self {
        author: true,
        review: true,
        ci: true,
    };

    /// The width of the cluster with these slots (fixed widths and gaps).
    pub(crate) fn width(self) -> f32 {
        let mut slots = vec![NEEDS_WIDTH, PR_WIDTH];
        if self.author {
            slots.push(AUTHOR_WIDTH);
        }
        if self.review {
            slots.push(REVIEW_WIDTH);
        }
        if self.ci {
            slots.push(CI_WIDTH);
        }
        slots.iter().sum::<f32>() + SLOT_GAP * (slots.len() as f32 - 1.0)
    }
}

/// The slots that fit in `room` pixels: CI drops first, then review, then
/// author. Needs and PR stay even when they don't fit.
pub(crate) fn visible_slots(room: f32) -> VisibleSlots {
    let mut slots = VisibleSlots::ALL;
    if slots.width() > room {
        slots.ci = false;
    }
    if slots.width() > room {
        slots.review = false;
    }
    if slots.width() > room {
        slots.author = false;
    }
    slots
}

/// The width left for the cluster in a row `row_width` wide at `indent`,
/// once the title keeps [`TITLE_MIN_WIDTH`] and the other chips take
/// `other_chips` (an estimate: those chips size to their content).
pub(crate) fn slot_room(row_width: f32, indent: f32, other_chips: f32) -> f32 {
    row_width - ROW_CHROME - indent - other_chips - TITLE_MIN_WIDTH
}

/// One fixed-width slot; `content` centered (author: right-aligned).
fn slot(
    selector: String,
    width: f32,
    right_aligned: bool,
    content: Option<AnyElement>,
) -> AnyElement {
    div()
        .debug_selector(move || selector.clone())
        .flex_none()
        .w(px(width))
        .h(px(SLOT_HEIGHT))
        .flex()
        .items_center()
        .when(right_aligned, |el| el.justify_end())
        .when(!right_aligned, |el| el.justify_center())
        .children(content)
        .into_any_element()
}

/// A tooltip-carrying icon (the child of a slot; also the chat work-context
/// strip's icons).
pub(crate) fn icon_mark(
    id: String,
    path: &'static str,
    color: Hsla,
    tip: String,
) -> gpui::Stateful<gpui::Div> {
    let selector = id.clone();
    let tip: SharedString = tip.into();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .flex()
        .items_center()
        .child(crate::icons::icon(path).size(px(11.0)).text_color(color))
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
}

/// Everything the cluster needs from the row.
pub(crate) struct PrRowArgs<'a> {
    pub key: &'a str,
    pub pr: &'a PullRequestRef,
    pub entry: Option<&'a PrEntry>,
    pub shell: Option<WeakEntity<crate::shell::Shell>>,
    /// The viewer's GitHub login, when known (for "you").
    pub viewer: Option<&'a str>,
    /// The item's attention indicator (⚠ / count), when it needs the user.
    pub attention: Option<AnyElement>,
    pub chat_count: u32,
    /// "Including descendants: …" (shown as the tooltip's second line).
    pub totals_line: Option<String>,
    pub visible: VisibleSlots,
}

/// The PR cluster of one row. Without a fetched status (loading, failed) only
/// the PR icon and the attention slot carry content; the other slots stay
/// empty so the columns still line up.
pub(crate) fn pr_row_slots(args: PrRowArgs<'_>, theme: &Theme) -> AnyElement {
    let PrRowArgs {
        key,
        pr,
        entry,
        shell,
        viewer,
        attention,
        chat_count,
        totals_line,
        visible,
    } = args;
    let status = match entry {
        Some(PrEntry::Ready { status, .. }) => Some(status.as_ref()),
        _ => None,
    };
    let (pr_path, pr_color, pr_tip, line) = match (entry, status) {
        (_, Some(status)) => {
            let (path, tone) = pr_icon(status.state);
            (
                path,
                tone_color(tone, theme),
                pr_tooltip(status),
                group_line(status, chat_count, viewer),
            )
        }
        (Some(PrEntry::Failed { error, .. }), None) => {
            let text = format!("{}: {}", pr.external_id(), error.user_text());
            let color = if error.not_available() {
                theme.text_faint
            } else {
                theme.warning
            };
            (
                crate::icons::PULL_REQUEST,
                color,
                format!("{text}\nClick to open on GitHub"),
                text,
            )
        }
        _ => {
            let text = format!("{} (loading from GitHub)", pr.external_id());
            (
                crate::icons::PULL_REQUEST,
                theme.text_faint,
                format!("{text}\nClick to open on GitHub"),
                text,
            )
        }
    };
    let line = match totals_line {
        Some(totals) => format!("{line}\n{totals}"),
        None => line,
    };

    let author = status
        .and_then(|status| {
            author_text(&status.author, viewer).map(|text| (text, status.author.clone()))
        })
        .map(|(text, login)| {
            let selector = format!("outline-pr-author-{key}");
            let tip: SharedString = author_tooltip(&login, viewer).into();
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .min_w_0()
                .truncate()
                .text_size(crate::typography::ui_rems(10.0))
                .text_color(theme.text_faint)
                .child(SharedString::from(text))
                .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
                .into_any_element()
        });
    let review = status
        .and_then(|status| {
            review_icon(status.my_review_state)
                .zip(chip::my_review_label(status.my_review_state))
        })
        .map(|((path, tone), label)| {
            icon_mark(
                format!("outline-pr-review-{key}"),
                path,
                tone_color(tone, theme),
                label.to_string(),
            )
            .into_any_element()
        });
    let ci = status
        .and_then(|status| ci_dot(status.ci_status).zip(chip::ci_label(status.ci_status)))
        .map(|(tone, label)| {
            let selector = format!("outline-pr-ci-{key}");
            let tip: SharedString = label.to_string().into();
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .flex_none()
                .size(px(CI_WIDTH))
                .p(px(1.0))
                .child(
                    div()
                        .size_full()
                        .rounded_full()
                        .bg(tone_color(tone, theme)),
                )
                .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
                .into_any_element()
        });

    let pr_id = format!("outline-pr-{key}");
    let pr_selector = pr_id.clone();
    let pr_tip: SharedString = pr_tip.into();
    let pr_mark = div()
        .id(SharedString::from(pr_id))
        .debug_selector(move || pr_selector.clone())
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        .cursor_pointer()
        .hover(|el| el.bg(theme.element_hover))
        .child(
            crate::icons::icon(pr_path)
                .size(px(12.0))
                .text_color(pr_color),
        )
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(pr_tip.clone())).into())
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(chip::open_pr(shell, pr.url.clone()))
        .into_any_element();

    let line: SharedString = line.into();
    let group_selector = format!("outline-pr-slots-{key}");
    div()
        .id(SharedString::from(group_selector.clone()))
        .debug_selector(move || group_selector.clone())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SLOT_GAP))
        .when(visible.author, |el| {
            el.child(slot(
                format!("outline-pr-slot-author-{key}"),
                AUTHOR_WIDTH,
                true,
                author,
            ))
        })
        .child(slot(
            format!("outline-pr-slot-needs-{key}"),
            NEEDS_WIDTH,
            false,
            attention.map(|indicator| {
                let selector = format!("outline-pr-needs-{key}");
                div()
                    .debug_selector(move || selector.clone())
                    .flex_none()
                    .child(indicator)
                    .into_any_element()
            }),
        ))
        .when(visible.review, |el| {
            el.child(slot(
                format!("outline-pr-slot-review-{key}"),
                REVIEW_WIDTH,
                false,
                review,
            ))
        })
        .when(visible.ci, |el| {
            el.child(slot(
                format!("outline-pr-slot-ci-{key}"),
                CI_WIDTH,
                false,
                ci,
            ))
        })
        .child(slot(
            format!("outline-pr-slot-pr-{key}"),
            PR_WIDTH,
            false,
            Some(pr_mark),
        ))
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(line.clone())).into())
        .into_any_element()
}

/// What a legend row draws before its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mark {
    Icon(&'static str),
    /// A small filled dot (CI, needs-you).
    Dot,
    /// Muted sample text (the author).
    Text(&'static str),
}

/// One entry of the `?` icon legend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LegendRow {
    pub mark: Mark,
    pub tone: Tone,
    pub label: &'static str,
}

/// The icon legend of the `?` reference, from the same tables the row uses
/// (PR and Linear rows; plain items and chats have none).
pub(crate) fn legend_rows() -> Vec<LegendRow> {
    let review = |state, label| {
        let (icon, tone) = review_icon(state).expect("a review icon");
        LegendRow {
            mark: Mark::Icon(icon),
            tone,
            label,
        }
    };
    let pr = |state, label| {
        let (icon, tone) = pr_icon(state);
        LegendRow {
            mark: Mark::Icon(icon),
            tone,
            label,
        }
    };
    let ci = |status, label| LegendRow {
        mark: Mark::Dot,
        tone: ci_dot(status).expect("a CI dot"),
        label,
    };
    let linear = |icon, label| LegendRow {
        mark: Mark::Icon(icon),
        tone: Tone::Muted,
        label,
    };
    vec![
        pr(PullRequestState::Open, "Pull request: open"),
        pr(PullRequestState::Draft, "Pull request: draft"),
        pr(PullRequestState::Merged, "Pull request: merged"),
        pr(PullRequestState::Closed, "Pull request: closed"),
        LegendRow {
            mark: Mark::Text("alex"),
            tone: Tone::Faint,
            label: "PR author (\u{201c}you\u{201d} when it is yours)",
        },
        LegendRow {
            mark: Mark::Dot,
            tone: Tone::Attention,
            label: "Needs your action (count when several)",
        },
        review(PullRequestMyReviewState::Requested, "Review requested"),
        review(
            PullRequestMyReviewState::ReRequested,
            "Re-review requested",
        ),
        review(PullRequestMyReviewState::Approved, "You approved"),
        review(
            PullRequestMyReviewState::ChangesRequested,
            "You requested changes",
        ),
        ci(PullRequestCiStatus::Passing, "CI passing"),
        ci(PullRequestCiStatus::Failing, "CI failing"),
        ci(PullRequestCiStatus::Pending, "CI running"),
        linear(crate::icons::CHECKLIST, "Linear: backlog / unstarted"),
        linear(crate::icons::CLOCK_CIRCLE, "Linear: started"),
        linear(crate::icons::CHECK, "Linear: completed"),
        linear(crate::icons::CLOSE_CIRCLE, "Linear: canceled"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::fixture::summary;
    use zeron_proto::PullRequestReviewDecision;

    #[test]
    fn pr_icon_and_color_for_every_state() {
        let theme = Theme::default();
        let pr = crate::icons::PULL_REQUEST;
        assert_eq!(pr_icon(PullRequestState::Open), (pr, Tone::Success));
        assert_eq!(pr_icon(PullRequestState::Draft), (pr, Tone::Muted));
        assert_eq!(pr_icon(PullRequestState::Merged), (pr, Tone::Merged));
        assert_eq!(pr_icon(PullRequestState::Closed), (pr, Tone::Danger));
        // Same colors as the details chip's state badge.
        for state in [
            PullRequestState::Open,
            PullRequestState::Draft,
            PullRequestState::Merged,
            PullRequestState::Closed,
        ] {
            assert_eq!(
                tone_color(pr_icon(state).1, &theme),
                chip::state_color(state, &theme)
            );
        }
    }

    #[test]
    fn ci_dot_for_every_state() {
        let theme = Theme::default();
        assert_eq!(ci_dot(PullRequestCiStatus::Passing), Some(Tone::Success));
        assert_eq!(ci_dot(PullRequestCiStatus::Failing), Some(Tone::Danger));
        assert_eq!(ci_dot(PullRequestCiStatus::Pending), Some(Tone::Warning));
        assert_eq!(ci_dot(PullRequestCiStatus::None), None);
        assert_eq!(tone_color(Tone::Success, &theme), theme.success);
        assert_eq!(tone_color(Tone::Danger, &theme), theme.danger);
        assert_eq!(tone_color(Tone::Warning, &theme), theme.warning);
    }

    #[test]
    fn review_icon_for_every_state() {
        let theme = Theme::default();
        use PullRequestMyReviewState as S;
        assert_eq!(
            review_icon(S::Requested),
            Some((crate::icons::EYE, Tone::Attention))
        );
        assert_eq!(
            review_icon(S::ReRequested),
            Some((crate::icons::REFRESH, Tone::Attention))
        );
        assert_eq!(
            review_icon(S::Approved),
            Some((crate::icons::CHECK, Tone::Success))
        );
        assert_eq!(
            review_icon(S::ChangesRequested),
            Some((crate::icons::CLOSE_CIRCLE, Tone::Orange))
        );
        assert_eq!(review_icon(S::Commented), None);
        assert_eq!(review_icon(S::None), None);
        assert_eq!(
            tone_color(Tone::Attention, &theme),
            crate::work_outline::chips::attention_color(&theme)
        );
        // Orange is neither the CI amber nor the danger red.
        assert_ne!(tone_color(Tone::Orange, &theme), theme.warning);
        assert_ne!(tone_color(Tone::Orange, &theme), theme.danger);
    }

    #[test]
    fn author_is_shown_truncated_or_as_you() {
        assert_eq!(author_text("alex", None).as_deref(), Some("alex"));
        assert_eq!(author_text("", None), None);
        assert_eq!(
            author_text("exactly-ten", None).as_deref(),
            Some("exactly-t\u{2026}")
        );
        assert_eq!(author_text("tenletters", None).as_deref(), Some("tenletters"));
        let long = author_text("alexander-the-great", None).unwrap();
        assert_eq!(long, "alexander\u{2026}");
        assert_eq!(long.chars().count(), AUTHOR_MAX_CHARS);
        // The viewer's own PR reads "you" (any case); no viewer, the login.
        assert_eq!(author_text("Alex", Some("alex")).as_deref(), Some("you"));
        assert_eq!(author_text("alex", Some("sam")).as_deref(), Some("alex"));
        assert_eq!(
            author_tooltip("alexander-the-great", None),
            "Author: alexander-the-great"
        );
        assert_eq!(author_tooltip("alex", Some("alex")), "Author: you (alex)");
    }

    #[test]
    fn group_line_names_everything_in_one_line() {
        let mut status = summary("app", 42, "Add auth");
        status.author = "alex".into();
        status.ci_status = PullRequestCiStatus::Failing;
        status.my_review_state = PullRequestMyReviewState::ReRequested;
        assert_eq!(
            group_line(&status, 2, None),
            "#42 \u{00b7} Open \u{00b7} CI failing \u{00b7} Re-review requested \u{00b7} by alex \u{00b7} 2 chats"
        );
        assert_eq!(
            group_line(&status, 1, Some("alex")),
            "#42 \u{00b7} Open \u{00b7} CI failing \u{00b7} Re-review requested \u{00b7} by you \u{00b7} 1 chat"
        );
        status.my_review_state = PullRequestMyReviewState::None;
        status.review_decision = Some(PullRequestReviewDecision::Approved);
        status.ci_status = PullRequestCiStatus::None;
        assert_eq!(
            group_line(&status, 0, None),
            "#42 \u{00b7} Open \u{00b7} Approved \u{00b7} by alex"
        );
        status.state = PullRequestState::Merged;
        assert_eq!(group_line(&status, 0, None), "#42 \u{00b7} Merged \u{00b7} by alex");
        assert!(pr_tooltip(&status).contains("acme/app#42 \u{00b7} Merged"));
        assert!(pr_tooltip(&status).ends_with("Click to open on GitHub"));
    }

    #[test]
    fn slots_drop_ci_then_review_then_author_and_never_needs_or_pr() {
        let all = VisibleSlots::ALL;
        assert_eq!(visible_slots(all.width()), all);
        assert_eq!(visible_slots(1000.0), all);
        let no_ci = VisibleSlots { ci: false, ..all };
        assert_eq!(visible_slots(all.width() - 0.5), no_ci);
        assert_eq!(visible_slots(no_ci.width()), no_ci);
        let no_review = VisibleSlots {
            review: false,
            ..no_ci
        };
        assert_eq!(visible_slots(no_ci.width() - 0.5), no_review);
        let minimal = VisibleSlots {
            author: false,
            ..no_review
        };
        assert_eq!(visible_slots(no_review.width() - 0.5), minimal);
        // Nothing fits: needs and PR (not part of the set) still show.
        assert_eq!(visible_slots(0.0), minimal);
        assert_eq!(visible_slots(-50.0), minimal);
        assert!(minimal.width() > 0.0);
        // A narrower cluster never takes more room.
        assert!(no_ci.width() < all.width() && minimal.width() < no_review.width());
    }

    #[test]
    fn room_leaves_the_title_its_minimum() {
        // A row exactly wide enough for the title minimum and the full
        // cluster keeps every slot; one pixel less drops CI.
        let indent = 8.0;
        let width = ROW_CHROME + indent + TITLE_MIN_WIDTH + VisibleSlots::ALL.width();
        assert_eq!(visible_slots(slot_room(width, indent, 0.0)), VisibleSlots::ALL);
        assert!(!visible_slots(slot_room(width - 1.0, indent, 0.0)).ci);
        // Other chips on the row take from the same room.
        assert!(!visible_slots(slot_room(width, indent, 30.0)).ci);
    }

    #[test]
    fn legend_covers_every_icon_with_the_rows_own_icons() {
        let rows = legend_rows();
        let labels: Vec<_> = rows.iter().map(|row| row.label).collect();
        for expected in [
            "Pull request: open",
            "Pull request: draft",
            "Pull request: merged",
            "Pull request: closed",
            "Review requested",
            "Re-review requested",
            "You approved",
            "You requested changes",
            "CI passing",
            "CI failing",
            "CI running",
            "Linear: started",
        ] {
            assert!(labels.contains(&expected), "{expected}");
        }
        let row = |label: &str| rows.iter().find(|row| row.label == label).unwrap();
        assert_eq!(row("Review requested").mark, Mark::Icon(crate::icons::EYE));
        assert_eq!(
            row("Re-review requested").mark,
            Mark::Icon(crate::icons::REFRESH)
        );
        assert_eq!(row("CI failing").mark, Mark::Dot);
        assert_eq!(row("CI failing").tone, Tone::Danger);
        assert_eq!(row("You approved").tone, Tone::Success);
        assert_eq!(row("You requested changes").tone, Tone::Orange);
        // Every row has a mark the row itself can draw: a known icon asset.
        for row in &rows {
            if let Mark::Icon(path) = row.mark {
                assert!(path.starts_with("icons/"), "{path}");
            }
        }
    }
}
