//! The live PR chip (Outline rows, Details, the GitHub panel, ⇧⌘P): the
//! state badge (open / draft / merged / closed in theme colors), CI, the
//! review decision, and the user's own review state. Click opens the PR per
//! the web-link preference. Pure label helpers first, so the wording is
//! unit-tested.
use gpui::{AnyElement, Hsla, SharedString, WeakEntity, div, prelude::*, px};
use zeron_proto::{
    PullRequestCiStatus, PullRequestMyReviewState, PullRequestRef, PullRequestReviewDecision,
    PullRequestState, PullRequestStatusSummary,
};

use super::store::PrEntry;
use crate::theme::Theme;
use crate::work_outline::chips::TextTooltip;

pub(crate) fn state_label(state: PullRequestState) -> &'static str {
    match state {
        PullRequestState::Open => "Open",
        PullRequestState::Draft => "Draft",
        PullRequestState::Merged => "Merged",
        PullRequestState::Closed => "Closed",
    }
}

/// The state badge color (the change-request badge's tones; draft is muted).
pub(crate) fn state_color(state: PullRequestState, theme: &Theme) -> Hsla {
    match state {
        PullRequestState::Open => theme.success,
        PullRequestState::Draft => theme.text_muted,
        PullRequestState::Merged => theme.code_text,
        PullRequestState::Closed => theme.danger,
    }
}

/// `None` when the PR has no checks.
pub(crate) fn ci_label(ci: PullRequestCiStatus) -> Option<&'static str> {
    match ci {
        PullRequestCiStatus::Passing => Some("CI passing"),
        PullRequestCiStatus::Failing => Some("CI failing"),
        PullRequestCiStatus::Pending => Some("CI pending"),
        PullRequestCiStatus::None => None,
    }
}

/// The compact CI glyph.
pub(crate) fn ci_glyph(ci: PullRequestCiStatus) -> Option<&'static str> {
    match ci {
        PullRequestCiStatus::Passing => Some("\u{2713}"),
        PullRequestCiStatus::Failing => Some("\u{2715}"),
        PullRequestCiStatus::Pending => Some("\u{25cf}"),
        PullRequestCiStatus::None => None,
    }
}

pub(crate) fn ci_color(ci: PullRequestCiStatus, theme: &Theme) -> Hsla {
    match ci {
        PullRequestCiStatus::Passing => theme.success,
        PullRequestCiStatus::Failing => theme.danger,
        PullRequestCiStatus::Pending => theme.warning,
        PullRequestCiStatus::None => theme.text_faint,
    }
}

pub(crate) fn review_decision_label(
    decision: Option<PullRequestReviewDecision>,
) -> Option<&'static str> {
    match decision? {
        PullRequestReviewDecision::Approved => Some("Approved"),
        PullRequestReviewDecision::ChangesRequested => Some("Changes requested"),
        PullRequestReviewDecision::ReviewRequired => Some("Review required"),
    }
}

/// The user's relation to the review; `None` when there is none.
pub(crate) fn my_review_label(state: PullRequestMyReviewState) -> Option<&'static str> {
    match state {
        PullRequestMyReviewState::Requested => Some("Review requested"),
        PullRequestMyReviewState::ReRequested => Some("Re-review requested"),
        PullRequestMyReviewState::Approved => Some("Approved by you"),
        PullRequestMyReviewState::ChangesRequested => Some("Changes requested by you"),
        PullRequestMyReviewState::Commented => Some("Commented by you"),
        PullRequestMyReviewState::None => None,
    }
}

/// The chip's text: `#42 · Open · ✓ · Review requested` (the review
/// decision only when the user has no review state of their own).
pub(crate) fn chip_text(status: &PullRequestStatusSummary) -> String {
    let mut parts = vec![
        format!("#{}", status.pr_ref.number),
        state_label(status.state).to_string(),
    ];
    parts.extend(ci_glyph(status.ci_status).map(str::to_string));
    match my_review_label(status.my_review_state) {
        Some(mine) => parts.push(mine.to_string()),
        None if !status.state.is_finished() => {
            parts.extend(review_decision_label(status.review_decision).map(str::to_string))
        }
        None => {}
    }
    parts.join(" \u{00b7} ")
}

/// The chip's tooltip: title, author, every status, and the click.
pub(crate) fn tooltip_text(status: &PullRequestStatusSummary) -> String {
    let mut lines = vec![format!(
        "{} \u{00b7} {}",
        status.pr_ref.external_id(),
        status.title
    )];
    let mut meta = vec![state_label(status.state).to_string()];
    meta.extend(ci_label(status.ci_status).map(str::to_string));
    meta.extend(review_decision_label(status.review_decision).map(str::to_string));
    meta.extend(my_review_label(status.my_review_state).map(str::to_string));
    if !status.author.is_empty() {
        meta.push(format!("by {}", status.author));
    }
    lines.push(meta.join(" \u{00b7} "));
    lines.push("Click to open on GitHub".to_string());
    lines.join("\n")
}

/// Open a web or PR link from a click handler through the Shell's single
/// entry point (web-link preference, side browser, route fallback).
/// `external` forces the external browser (⌘-click). The Shell is `shell`,
/// else the window's root; with neither it is the external browser.
pub(crate) fn open_link(
    shell: Option<&WeakEntity<crate::shell::Shell>>,
    url: &str,
    external: bool,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) {
    let shell = shell
        .and_then(WeakEntity::upgrade)
        .or_else(|| window.root::<crate::shell::Shell>().flatten());
    match shell {
        Some(shell) => shell.update(cx, |shell, cx| {
            shell.open_web_reference(url, external, window, cx)
        }),
        None => cx.open_url(url),
    }
}

/// Whether a click asks for the external browser (⌘-click; Ctrl-click off
/// macOS).
pub(crate) fn click_forces_external(event: &gpui::ClickEvent) -> bool {
    event.modifiers().secondary()
}

/// Open `url` through the Shell (web-link preference), else the browser.
/// ⌘-click always opens the external browser.
pub(crate) fn open_pr(
    shell: Option<WeakEntity<crate::shell::Shell>>,
    url: String,
) -> impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static {
    move |event, window, cx| {
        cx.stop_propagation();
        open_link(
            shell.as_ref(),
            &url,
            click_forces_external(event),
            window,
            cx,
        );
    }
}

/// The live chip of `pr` from its cache `entry` (loading / failed states
/// show `#n` with a faint or warning dot).
pub(crate) fn pr_chip(
    id: String,
    pr: &PullRequestRef,
    entry: Option<&PrEntry>,
    shell: Option<WeakEntity<crate::shell::Shell>>,
    theme: &Theme,
) -> AnyElement {
    let selector = id.clone();
    let (dot, text, tip, ci): (Hsla, String, String, Option<Hsla>) = match entry {
        Some(PrEntry::Ready { status, .. }) => (
            state_color(status.state, theme),
            chip_text(status),
            tooltip_text(status),
            ci_glyph(status.ci_status).map(|_| ci_color(status.ci_status, theme)),
        ),
        Some(PrEntry::Failed { error, .. }) => (
            if error.not_available() {
                theme.text_faint
            } else {
                theme.warning
            },
            format!("#{}", pr.number),
            format!(
                "{}: {}\nClick to open on GitHub",
                pr.external_id(),
                error.user_text()
            ),
            None,
        ),
        _ => (
            theme.text_faint,
            format!("#{}", pr.number),
            format!(
                "{} (loading from GitHub)\nClick to open on GitHub",
                pr.external_id()
            ),
            None,
        ),
    };
    let tip: SharedString = tip.into();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .max_w(px(240.0))
        .flex()
        .items_center()
        .gap(px(4.0))
        .px(px(4.0))
        .rounded(px(3.0))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(10.0))
        .text_color(theme.text_muted)
        .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
        .child(
            crate::icons::icon(crate::icons::PULL_REQUEST)
                .size(px(10.0))
                .text_color(dot),
        )
        .when_some(ci, |el, color| {
            el.child(div().flex_none().size(px(5.0)).rounded_full().bg(color))
        })
        .child(div().min_w_0().truncate().child(SharedString::from(text)))
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(open_pr(shell, pr.url.clone()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chip_text_names_state_ci_and_my_review() {
        let mut status = super::super::fixture::summary("app", 42, "Add auth");
        assert_eq!(
            chip_text(&status),
            "#42 \u{00b7} Open \u{00b7} \u{2713} \u{00b7} Review requested"
        );
        status.my_review_state = PullRequestMyReviewState::ReRequested;
        status.ci_status = PullRequestCiStatus::Failing;
        assert_eq!(
            chip_text(&status),
            "#42 \u{00b7} Open \u{00b7} \u{2715} \u{00b7} Re-review requested"
        );
        status.my_review_state = PullRequestMyReviewState::None;
        status.review_decision = Some(PullRequestReviewDecision::Approved);
        assert_eq!(
            chip_text(&status),
            "#42 \u{00b7} Open \u{00b7} \u{2715} \u{00b7} Approved"
        );
        status.state = PullRequestState::Merged;
        status.ci_status = PullRequestCiStatus::None;
        assert_eq!(chip_text(&status), "#42 \u{00b7} Merged");
        status.my_review_state = PullRequestMyReviewState::Approved;
        assert!(tooltip_text(&status).contains("Approved by you"));
        assert!(tooltip_text(&status).contains("acme/app#42 \u{00b7} Add auth"));
    }

    #[test]
    fn state_colors_follow_the_theme() {
        let theme = Theme::default();
        assert_eq!(state_color(PullRequestState::Open, &theme), theme.success);
        assert_eq!(state_color(PullRequestState::Closed, &theme), theme.danger);
        assert_eq!(
            ci_color(PullRequestCiStatus::Pending, &theme),
            theme.warning
        );
    }
}
