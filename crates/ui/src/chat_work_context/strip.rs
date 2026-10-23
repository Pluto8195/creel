//! Rendering of the work-context strip ([`super::WorkContext`]). Pure of
//! fetching: everything comes from [`StripData`] (copies of the PR, Linear
//! and artifact stores), so a refresh that returns the same data renders the
//! same strip and a refresh that fails keeps the last data (the stores keep
//! it; `PrEntry::Loading` / `Failed` only ever stand for "nothing yet").
//! Wording and colors come from `github::chip` and `github::row_icons`.

use std::collections::HashMap;
use std::rc::Rc;

use chrono::{DateTime, Utc};
use gpui::{
    AnyElement, App, Hsla, SharedString, Stateful, WeakEntity, Window, div, prelude::*, px,
};
use zeron_proto::linear::LinearProjectSummary;
use zeron_proto::{PullRequestRef, PullRequestStatusSummary};

use super::{
    ArtifactBrief, ImplementationContext, IssueRef, PlanningContext, PrReviewContext, Variant,
    WorkContext,
};
use crate::github::chip;
use crate::github::row_icons::{self, Tone, tone_color};
use crate::github::store::PrEntry;
use crate::icons::{self, icon};
use crate::linear::store::IssueEntry;
use crate::theme::Theme;
use crate::work_outline::chips::TextTooltip;

/// Reviewers shown on the collapsed line before "+N".
const COLLAPSED_REVIEWERS: usize = 2;
/// Lines of the ticket description in the expanded panel.
const DESCRIPTION_LINES: usize = 3;

pub(crate) type Handler<T> = Rc<dyn Fn(T, &mut Window, &mut App)>;

/// What a click does; the Shell host supplies these.
#[derive(Clone)]
pub(crate) struct Actions {
    pub toggle: Rc<dyn Fn(&mut Window, &mut App)>,
    /// A PR, ticket or project URL (the Shell's web-link preference).
    pub open_url: Handler<String>,
    /// Reveal a work item in the Outline.
    pub open_item: Handler<String>,
    /// Open an artifact in the Artifact tab.
    pub open_artifact: Handler<String>,
    /// For the shared Linear chips.
    pub shell: Option<WeakEntity<crate::shell::Shell>>,
}

/// Copies of the stores the strip reads.
#[derive(Clone)]
pub(crate) struct StripData {
    /// By `PullRequestRef::key`.
    pub prs: HashMap<String, PrEntry>,
    pub issue: Option<IssueEntry>,
    pub project: Option<LinearProjectSummary>,
    /// The viewer's GitHub login, when known (the author reads "you").
    pub viewer: Option<String>,
    pub now: DateTime<Utc>,
}

impl StripData {
    fn entry(&self, pr: &PullRequestRef) -> Option<&PrEntry> {
        self.prs.get(&pr.key())
    }
}

// ----------------------------------------------------------------------
// Pure field text
// ----------------------------------------------------------------------

/// What one PR shows, as plain data (wording tested without a window).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PrFields {
    /// `repo #42`.
    pub number: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub icon: (&'static str, Tone),
    pub state: Option<&'static str>,
    /// Your review: icon, tone and label.
    pub my_review: Option<(&'static str, Tone, &'static str)>,
    pub ci: Option<(Tone, &'static str)>,
    pub decision: Option<&'static str>,
    pub reviewers: Vec<String>,
    pub branches: Option<String>,
    pub updated: Option<String>,
    /// Shown instead of the missing fields while there is no data yet.
    pub note: Option<String>,
}

fn time_ago(iso: &str, now: DateTime<Utc>) -> Option<String> {
    let then = DateTime::parse_from_rfc3339(iso).ok()?.with_timezone(&Utc);
    Some(crate::state::format_time_ago(then, now))
}

/// The fields of one PR from its cache entry. `Loading` and `Failed` keep
/// the number and the matching note; the store holds the last good data
/// across refreshes, so those two only appear before any data arrived.
pub(crate) fn pr_fields(
    pr: &PullRequestRef,
    entry: Option<&PrEntry>,
    viewer: Option<&str>,
    now: DateTime<Utc>,
) -> PrFields {
    let number = format!("{} #{}", pr.repo, pr.number);
    match entry {
        Some(PrEntry::Ready { status, .. }) => {
            let status: &PullRequestStatusSummary = status;
            PrFields {
                number,
                title: Some(status.title.clone()).filter(|title| !title.trim().is_empty()),
                author: row_icons::author_text(&status.author, viewer),
                icon: row_icons::pr_icon(status.state),
                state: Some(chip::state_label(status.state)),
                my_review: row_icons::review_icon(status.my_review_state).zip(
                    chip::my_review_label(status.my_review_state),
                )
                .map(|((path, tone), label)| (path, tone, label)),
                ci: row_icons::ci_dot(status.ci_status).zip(chip::ci_label(status.ci_status)),
                decision: (!status.state.is_finished())
                    .then(|| chip::review_decision_label(status.review_decision))
                    .flatten(),
                reviewers: status.requested_reviewers.clone(),
                branches: status
                    .head_ref_name
                    .as_deref()
                    .zip(status.base_ref_name.as_deref())
                    .map(|(head, base)| format!("{head} \u{2192} {base}")),
                updated: time_ago(&status.updated_at, now),
                note: None,
            }
        }
        failed_or_loading => {
            let note = match failed_or_loading {
                Some(PrEntry::Failed { error, .. }) => error.user_text(),
                _ => "Loading from GitHub\u{2026}".to_owned(),
            };
            PrFields {
                number,
                title: None,
                author: None,
                icon: (icons::PULL_REQUEST, Tone::Faint),
                state: None,
                my_review: None,
                ci: None,
                decision: None,
                reviewers: Vec::new(),
                branches: None,
                updated: None,
                note: Some(note),
            }
        }
    }
}

/// `a, b +1` for the collapsed line.
pub(crate) fn reviewers_text(reviewers: &[String], limit: usize) -> Option<String> {
    let first: Vec<&str> = reviewers.iter().take(limit).map(String::as_str).collect();
    if first.is_empty() {
        return None;
    }
    let mut text = first.join(", ");
    if reviewers.len() > limit {
        text.push_str(&format!(" +{}", reviewers.len() - limit));
    }
    Some(text)
}

/// `r3 \u{00b7} 2d ago`.
pub(crate) fn revision_text(artifact: &ArtifactBrief, now: DateTime<Utc>) -> String {
    let age = DateTime::from_timestamp_millis(artifact.at)
        .map(|at| format!(" \u{00b7} {} ago", crate::state::format_time_ago(at, now)))
        .unwrap_or_default();
    if artifact.revision == 0 {
        age.trim_start_matches(" \u{00b7} ").to_owned()
    } else {
        format!("r{}{age}", artifact.revision)
    }
}

/// The first lines of a ticket description, with "…" when cut.
pub(crate) fn description_excerpt(description: &str, lines: usize) -> String {
    let all: Vec<&str> = description
        .lines()
        .map(str::trim_end)
        .skip_while(|line| line.trim().is_empty())
        .collect();
    let mut shown: Vec<&str> = all.iter().take(lines).copied().collect();
    while shown.last().is_some_and(|line| line.trim().is_empty()) {
        shown.pop();
    }
    let mut text = shown.join("\n");
    if all.len() > lines {
        text.push('\u{2026}');
    }
    text
}

// ----------------------------------------------------------------------
// Small building blocks
// ----------------------------------------------------------------------

fn tooltip(el: Stateful<gpui::Div>, text: impl Into<SharedString>) -> Stateful<gpui::Div> {
    let text: SharedString = text.into();
    el.tooltip(move |_, cx| cx.new(|_| TextTooltip(text.clone())).into())
}

fn base(id: impl Into<SharedString>) -> Stateful<gpui::Div> {
    let id: SharedString = id.into();
    let key = id.clone();
    div()
        .id(id)
        .debug_selector(move || key.to_string())
}

fn text_cell(id: &str, text: impl Into<SharedString>, color: Hsla) -> Stateful<gpui::Div> {
    base(id.to_owned())
        .flex_none()
        .truncate()
        .text_color(color)
        .child(text.into())
}

fn dot(color: Hsla) -> gpui::Div {
    div().flex_none().size(px(6.0)).rounded_full().bg(color)
}

/// A clickable cell: a click runs `handler` without toggling the strip.
fn link(
    el: Stateful<gpui::Div>,
    theme: &Theme,
    handler: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<gpui::Div> {
    el.cursor_pointer()
        .rounded(px(3.0))
        .px(px(3.0))
        .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            handler(window, cx)
        })
}

fn open_url_link(
    el: Stateful<gpui::Div>,
    url: String,
    actions: &Actions,
    theme: &Theme,
) -> Stateful<gpui::Div> {
    let open = actions.open_url.clone();
    link(el, theme, move |window, cx| open(url.clone(), window, cx))
}

fn pr_icon_mark(
    id: &str,
    fields: &PrFields,
    status: Option<&PullRequestStatusSummary>,
    theme: &Theme,
) -> AnyElement {
    let (path, tone) = fields.icon;
    let tip = status.map_or_else(
        || fields.number.clone(),
        row_icons::pr_tooltip,
    );
    row_icons::icon_mark(id.to_owned(), path, tone_color(tone, theme), tip).into_any_element()
}

fn separator(theme: &Theme) -> AnyElement {
    div()
        .flex_none()
        .text_color(theme.text_faint)
        .child("\u{00b7}")
        .into_any_element()
}

// ----------------------------------------------------------------------
// One PR on the collapsed line
// ----------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Density {
    /// The review strip: author, your review, CI, decision, reviewers,
    /// branches, updated.
    Full,
    /// The implementation strip, whose ticket already took the room: state,
    /// CI and decision.
    Compact,
}

fn pr_segments(
    prefix: &str,
    pr: &PullRequestRef,
    data: &StripData,
    density: Density,
    actions: &Actions,
    theme: &Theme,
) -> Vec<AnyElement> {
    let entry = data.entry(pr);
    let fields = pr_fields(pr, entry, data.viewer.as_deref(), data.now);
    let status = entry.and_then(PrEntry::ready);
    let mut out: Vec<AnyElement> = vec![pr_icon_mark(
        &format!("{prefix}-pr-icon"),
        &fields,
        status,
        theme,
    )];
    let number = open_url_link(
        text_cell(
            &format!("{prefix}-pr-number"),
            fields.number.clone(),
            theme.text,
        ),
        pr.url.clone(),
        actions,
        theme,
    );
    out.push(
        tooltip(number, format!("Open {} on GitHub", pr.external_id())).into_any_element(),
    );
    if let Some(note) = &fields.note {
        out.push(text_cell(&format!("{prefix}-pr-note"), note.clone(), theme.text_faint).into_any_element());
        return out;
    }
    if density == Density::Compact
        && let Some(state) = fields.state
    {
        out.push(text_cell(&format!("{prefix}-pr-state"), state, theme.text_muted).into_any_element());
    }
    if density == Density::Full {
        if let Some(title) = &fields.title {
            out.push(
                base(format!("{prefix}-pr-title"))
                    .flex_1()
                    .min_w(px(row_icons::TITLE_MIN_WIDTH / 2.0))
                    .truncate()
                    .text_color(theme.text_muted)
                    .child(title.clone())
                    .into_any_element(),
            );
        }
        if let Some(author) = &fields.author {
            let tip = status
                .map(|status| row_icons::author_tooltip(&status.author, data.viewer.as_deref()))
                .unwrap_or_default();
            out.push(
                tooltip(
                    text_cell(&format!("{prefix}-pr-author"), author.clone(), theme.text_muted),
                    tip,
                )
                .into_any_element(),
            );
        }
        if let Some((path, tone, label)) = fields.my_review {
            out.push(
                row_icons::icon_mark(
                    format!("{prefix}-pr-my-review"),
                    path,
                    tone_color(tone, theme),
                    label.to_owned(),
                )
                .into_any_element(),
            );
        }
    }
    if let Some((tone, label)) = fields.ci {
        out.push(
            tooltip(
                base(format!("{prefix}-pr-ci"))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(3.0))
                    .child(dot(tone_color(tone, theme)))
                    .child(div().child(label)),
                label,
            )
            .into_any_element(),
        );
    }
    if let Some(decision) = fields.decision {
        out.push(
            text_cell(&format!("{prefix}-pr-decision"), decision, theme.text_muted)
                .into_any_element(),
        );
    }
    if density == Density::Full {
        if let Some(reviewers) = reviewers_text(&fields.reviewers, COLLAPSED_REVIEWERS) {
            out.push(
                tooltip(
                    text_cell(
                        &format!("{prefix}-pr-reviewers"),
                        format!("Reviewers: {reviewers}"),
                        theme.text_faint,
                    ),
                    format!("Requested reviewers: {}", fields.reviewers.join(", ")),
                )
                .into_any_element(),
            );
        }
        if let Some(branches) = &fields.branches {
            out.push(
                text_cell(&format!("{prefix}-pr-branches"), branches.clone(), theme.text_faint)
                    .max_w(px(220.0))
                    .into_any_element(),
            );
        }
        if let Some(updated) = &fields.updated {
            out.push(
                text_cell(
                    &format!("{prefix}-pr-updated"),
                    format!("updated {updated}"),
                    theme.text_faint,
                )
                .into_any_element(),
            );
        }
    }
    out
}

fn more_badge(id: &str, extra: usize, theme: &Theme) -> Option<AnyElement> {
    (extra > 0).then(|| {
        tooltip(
            text_cell(id, format!("+{extra}"), theme.text_muted),
            format!("{extra} more pull request{} in the details", plural(extra)),
        )
        .into_any_element()
    })
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn ticket_chip(
    ticket: &IssueRef,
    data: &StripData,
    actions: &Actions,
    theme: &Theme,
) -> AnyElement {
    crate::work_outline::outline_view::linear_outline::issue_chip(
        "work-context-ticket".into(),
        &ticket.identifier,
        data.issue.as_ref(),
        ticket.url.clone(),
        actions.shell.clone(),
        theme,
    )
}

// ----------------------------------------------------------------------
// The collapsed line
// ----------------------------------------------------------------------

fn collapsed_children(
    context: &WorkContext,
    data: &StripData,
    actions: &Actions,
    theme: &Theme,
) -> Vec<AnyElement> {
    match &context.variant {
        Variant::PrReview(review) => review_line(review, data, actions, theme),
        Variant::Implementation(implementation) => {
            implementation_line(implementation, data, actions, theme)
        }
        Variant::Planning(planning) => planning_line(planning, data, actions, theme),
    }
}

fn review_line(
    review: &PrReviewContext,
    data: &StripData,
    actions: &Actions,
    theme: &Theme,
) -> Vec<AnyElement> {
    let mut out = Vec::new();
    if let Some(first) = review.prs.first() {
        out.extend(pr_segments("work-context", first, data, Density::Full, actions, theme));
    }
    out.extend(more_badge(
        "work-context-more",
        review.prs.len().saturating_sub(1),
        theme,
    ));
    if let Some(ticket) = &review.ticket {
        out.push(ticket_chip(ticket, data, actions, theme));
    }
    out
}

fn implementation_line(
    implementation: &ImplementationContext,
    data: &StripData,
    actions: &Actions,
    theme: &Theme,
) -> Vec<AnyElement> {
    let mut out = Vec::new();
    if let Some(ticket) = &implementation.ticket {
        out.push(ticket_chip(ticket, data, actions, theme));
        if let Some(IssueEntry::Ready { issue, .. }) = &data.issue {
            out.push(
                base("work-context-ticket-title")
                    .flex_1()
                    .min_w(px(60.0))
                    .truncate()
                    .text_color(theme.text_muted)
                    .child(issue.title.clone())
                    .into_any_element(),
            );
        } else {
            out.push(div().flex_1().into_any_element());
        }
        out.push(separator(theme));
    }
    match implementation.prs.first() {
        Some(first) => {
            out.extend(pr_segments(
                "work-context",
                first,
                data,
                Density::Compact,
                actions,
                theme,
            ));
            out.extend(more_badge(
                "work-context-more",
                implementation.prs.len() - 1,
                theme,
            ));
        }
        None => out.push(
            text_cell("work-context-no-pr", "No PR yet", theme.text_faint).into_any_element(),
        ),
    }
    out
}

fn planning_line(
    planning: &PlanningContext,
    data: &StripData,
    actions: &Actions,
    theme: &Theme,
) -> Vec<AnyElement> {
    let mut out = Vec::new();
    if let Some(project) = &data.project_ref(planning) {
        out.push(
            crate::work_outline::outline_view::linear_outline::project_chip(
                "work-context-project".into(),
                project.name.clone(),
                project.url.clone(),
                actions.shell.clone(),
                theme,
            ),
        );
        if let Some(state) = data.project.as_ref().and_then(|p| p.state.clone()) {
            out.push(text_cell("work-context-project-state", state, theme.text_muted).into_any_element());
        }
        out.push(separator(theme));
    }
    match &planning.plan {
        Some(plan) => {
            out.push(
                icon(icons::DOCUMENT)
                    .size(px(11.0))
                    .text_color(theme.text_faint)
                    .into_any_element(),
            );
            let open = actions.open_artifact.clone();
            let id = plan.id.clone();
            out.push(
                tooltip(
                    link(
                        base("work-context-plan")
                            .flex_1()
                            .min_w(px(60.0))
                            .truncate()
                            .text_color(theme.text)
                            .child(plan.title.clone()),
                        theme,
                        move |window, cx| open(id.clone(), window, cx),
                    ),
                    "Open the pinned plan",
                )
                .into_any_element(),
            );
            out.push(
                text_cell(
                    "work-context-plan-revision",
                    revision_text(plan, data.now),
                    theme.text_faint,
                )
                .into_any_element(),
            );
        }
        None => out.push(
            text_cell("work-context-no-plan", "No plan yet", theme.text_faint).into_any_element(),
        ),
    }
    let others = planning
        .artifacts
        .iter()
        .filter(|artifact| Some(&artifact.id) != planning.plan.as_ref().map(|plan| &plan.id))
        .count();
    if others > 0 {
        out.push(
            tooltip(
                text_cell(
                    "work-context-artifact-count",
                    format!("+{others} artifact{}", plural(others)),
                    theme.text_muted,
                ),
                "More artifacts of this item and its children, in the details",
            )
            .into_any_element(),
        );
    }
    out
}

impl StripData {
    /// The project of a planning context, preferring the fetched summary's
    /// name over the reference's cached title.
    fn project_ref(&self, planning: &PlanningContext) -> Option<super::ProjectRef> {
        let project = planning.project.clone()?;
        Some(match &self.project {
            Some(summary) if !summary.name.trim().is_empty() => super::ProjectRef {
                name: summary.name.clone(),
                ..project
            },
            _ => project,
        })
    }
}

// ----------------------------------------------------------------------
// The details panel
// ----------------------------------------------------------------------

fn panel_row(
    id: String,
    label: &'static str,
    value: AnyElement,
    theme: &Theme,
) -> AnyElement {
    div()
        .flex()
        .items_baseline()
        .gap(px(8.0))
        .child(
            div()
                .flex_none()
                .w(px(92.0))
                .text_color(theme.text_faint)
                .child(label),
        )
        .child(
            base(id)
                .flex_1()
                .min_w_0()
                .text_color(theme.text_muted)
                .child(value),
        )
        .into_any_element()
}

fn panel_text(text: impl Into<SharedString>) -> AnyElement {
    div().child(text.into()).into_any_element()
}

fn panel_link(
    id: &str,
    text: impl Into<SharedString>,
    theme: &Theme,
    handler: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    link(
        base(id.to_owned()).text_color(theme.accent).child(text.into()),
        theme,
        handler,
    )
    .into_any_element()
}

fn pr_panel(
    index: usize,
    pr: &PullRequestRef,
    pr_item_id: Option<&str>,
    data: &StripData,
    actions: &Actions,
    theme: &Theme,
) -> AnyElement {
    let entry = data.entry(pr);
    let fields = pr_fields(pr, entry, data.viewer.as_deref(), data.now);
    let status = entry.and_then(PrEntry::ready);
    let key = format!("work-context-panel-pr-{index}");
    let open = actions.open_url.clone();
    let url = pr.url.clone();
    let mut rows: Vec<AnyElement> = Vec::new();
    rows.push(
        div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(pr_icon_mark(&format!("{key}-icon"), &fields, status, theme))
            .child(panel_link(
                &format!("{key}-number"),
                format!("{} #{}", pr.repo, pr.number),
                theme,
                move |window, cx| open(url.clone(), window, cx),
            ))
            .children(fields.title.clone().map(|title| {
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text)
                    .child(title)
                    .into_any_element()
            }))
            .into_any_element(),
    );
    if let Some(note) = &fields.note {
        rows.push(panel_row(format!("{key}-note"), "Status", panel_text(note.clone()), theme));
    } else {
        let mut line = |label: &'static str, id: &str, value: Option<String>| {
            if let Some(value) = value {
                rows.push(panel_row(format!("{key}-{id}"), label, panel_text(value), theme));
            }
        };
        line("State", "state", fields.state.map(str::to_owned));
        line(
            "Author",
            "author",
            status.map(|s| s.author.clone()).filter(|a| !a.is_empty()).map(|author| {
                if fields.author.as_deref() == Some("you") {
                    format!("{author} (you)")
                } else {
                    author
                }
            }),
        );
        line("CI", "ci", fields.ci.map(|(_, label)| label.to_owned()));
        line("Decision", "decision", fields.decision.map(str::to_owned));
        line("Your review", "my-review", fields.my_review.map(|(_, _, label)| label.to_owned()));
        line(
            "Reviewers",
            "reviewers",
            (!fields.reviewers.is_empty()).then(|| fields.reviewers.join(", ")),
        );
        line("Branches", "branches", fields.branches.clone());
        line("Updated", "updated", fields.updated.clone().map(|ago| format!("{ago} ago")));
    }
    if let Some(item_id) = pr_item_id {
        let open_item = actions.open_item.clone();
        let item_id = item_id.to_owned();
        rows.push(panel_row(
            format!("{key}-item"),
            "Outline",
            panel_link(
                &format!("{key}-item-link"),
                "Show the PR\u{2019}s item in the Outline",
                theme,
                move |window, cx| open_item(item_id.clone(), window, cx),
            ),
            theme,
        ));
    }
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .children(rows)
        .into_any_element()
}

fn ticket_panel(
    ticket: &IssueRef,
    data: &StripData,
    actions: &Actions,
    theme: &Theme,
    with_description: bool,
) -> AnyElement {
    let mut rows: Vec<AnyElement> = vec![panel_row(
        "work-context-panel-ticket".into(),
        "Ticket",
        ticket_chip(ticket, data, actions, theme),
        theme,
    )];
    if let Some(IssueEntry::Ready { issue, .. }) = &data.issue {
        rows.push(panel_row(
            "work-context-panel-ticket-title".into(),
            "Title",
            panel_text(issue.title.clone()),
            theme,
        ));
        if with_description
            && let Some(description) = issue.description.as_deref().filter(|d| !d.trim().is_empty())
        {
            rows.push(panel_row(
                "work-context-panel-ticket-description".into(),
                "Description",
                panel_text(description_excerpt(description, DESCRIPTION_LINES)),
                theme,
            ));
        }
    }
    let open = actions.open_url.clone();
    let url = ticket.url.clone();
    rows.push(panel_row(
        "work-context-panel-ticket-open".into(),
        "Linear",
        panel_link(
            "work-context-panel-ticket-link",
            format!("Open {} in Linear", ticket.identifier),
            theme,
            move |window, cx| open(url.clone(), window, cx),
        ),
        theme,
    ));
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .children(rows)
        .into_any_element()
}

fn artifact_row(
    index: usize,
    artifact: &ArtifactBrief,
    data: &StripData,
    actions: &Actions,
    theme: &Theme,
) -> AnyElement {
    let open = actions.open_artifact.clone();
    let id = artifact.id.clone();
    let title = if artifact.pinned {
        format!("{} (plan)", artifact.title)
    } else {
        artifact.title.clone()
    };
    div()
        .flex()
        .items_baseline()
        .gap(px(8.0))
        .child(link(
            base(format!("work-context-panel-artifact-{index}"))
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme.accent)
                .child(title),
            theme,
            move |window, cx| open(id.clone(), window, cx),
        ))
        .child(
            div()
                .flex_none()
                .text_color(theme.text_faint)
                .child(format!("{} \u{00b7} {}", artifact.kind, revision_text(artifact, data.now))),
        )
        .into_any_element()
}

fn panel(
    context: &WorkContext,
    data: &StripData,
    actions: &Actions,
    theme: &Theme,
) -> AnyElement {
    let mut sections: Vec<AnyElement> = Vec::new();
    match &context.variant {
        Variant::PrReview(review) => {
            for (index, pr) in review.prs.iter().enumerate() {
                sections.push(pr_panel(
                    index,
                    pr,
                    review.pr_item_id.as_deref(),
                    data,
                    actions,
                    theme,
                ));
            }
            if let Some(ticket) = &review.ticket {
                sections.push(ticket_panel(ticket, data, actions, theme, false));
            }
        }
        Variant::Implementation(implementation) => {
            if let Some(ticket) = &implementation.ticket {
                sections.push(ticket_panel(ticket, data, actions, theme, true));
            }
            if implementation.prs.is_empty() {
                sections.push(panel_text("No PR yet"));
            }
            for (index, pr) in implementation.prs.iter().enumerate() {
                sections.push(pr_panel(
                    index,
                    pr,
                    implementation.pr_item_id.as_deref(),
                    data,
                    actions,
                    theme,
                ));
            }
        }
        Variant::Planning(planning) => {
            if let Some(project) = data.project_ref(planning) {
                let open = actions.open_url.clone();
                let url = project.url.clone();
                let mut rows = vec![panel_row(
                    "work-context-panel-project".into(),
                    "Project",
                    panel_link(
                        "work-context-panel-project-link",
                        project.name.clone(),
                        theme,
                        move |window, cx| open(url.clone(), window, cx),
                    ),
                    theme,
                )];
                if let Some(state) = data.project.as_ref().and_then(|p| p.state.clone()) {
                    rows.push(panel_row(
                        "work-context-panel-project-state".into(),
                        "State",
                        panel_text(state),
                        theme,
                    ));
                }
                sections.push(div().flex().flex_col().gap(px(2.0)).children(rows).into_any_element());
            }
            if planning.artifacts.is_empty() {
                sections.push(panel_text("No plan yet"));
            } else {
                sections.push(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(div().text_color(theme.text_faint).child("Artifacts"))
                        .children(planning.artifacts.iter().enumerate().map(|(index, artifact)| {
                            artifact_row(index, artifact, data, actions, theme)
                        }))
                        .into_any_element(),
                );
            }
        }
    }
    div()
        .id("work-context-panel")
        .debug_selector(|| "work-context-panel".into())
        .flex()
        .flex_col()
        .gap(px(8.0))
        .pt(px(6.0))
        .pb(px(2.0))
        .px(px(4.0))
        .text_size(crate::typography::ui_rems(11.0))
        .children(sections)
        .into_any_element()
}

// ----------------------------------------------------------------------
// The strip
// ----------------------------------------------------------------------

/// The strip of `context`: one line, plus the details panel when
/// `expanded`. A click on the line toggles it; the links inside don't.
pub(crate) fn render(
    context: &WorkContext,
    data: &StripData,
    expanded: bool,
    actions: &Actions,
    theme: &Theme,
) -> AnyElement {
    let toggle = actions.toggle.clone();
    let chevron = if expanded {
        icons::ALT_ARROW_DOWN
    } else {
        icons::ALT_ARROW_RIGHT
    };
    let line = div()
        .id("work-context-line")
        .debug_selector(|| "work-context-line".into())
        .flex()
        .items_center()
        .gap(px(6.0))
        .h(px(22.0))
        .overflow_hidden()
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(theme.text_muted)
        .on_click(move |_, window, cx| toggle(window, cx))
        .child(
            tooltip(
                base("work-context-chevron")
                    .flex_none()
                    .flex()
                    .items_center()
                    .child(icon(chevron).size(px(10.0)).text_color(theme.text_faint)),
                if expanded { "Hide details" } else { "Show details" },
            ),
        )
        .children(collapsed_children(context, data, actions, theme));
    div()
        .id("work-context-strip")
        .debug_selector(|| "work-context-strip".into())
        .flex_none()
        .flex()
        .flex_col()
        .child(line)
        .when(expanded, |el| el.child(panel(context, data, actions, theme)))
        .into_any_element()
}
