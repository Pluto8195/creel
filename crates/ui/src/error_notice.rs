//! The shared failure surface: a persistent, copyable error card.
//!
//! Failures such as `git worktree add` refusals carry multi-line stderr that
//! users must paste into a bug report or a chat. The card therefore shows the
//! error in a loud red treatment (icon + title line), the full detail in a
//! wrapped monospace block that scrolls instead of truncating, and a labeled
//! **Copy** button that writes the exact title + detail to the clipboard and
//! flips to "Copied". It never dismisses itself: the owner clears it (the
//! optional Dismiss button calls the owner back).

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, ClipboardItem, ElementId, FontWeight, IntoElement, MouseButton, RenderOnce, SharedString,
    Window, div, prelude::*, px,
};

use crate::theme::Theme;

/// How long the Copy button says "Copied".
const COPIED_FEEDBACK: Duration = Duration::from_millis(1800);
/// Detail taller than this scrolls inside the card.
const DETAIL_MAX_HEIGHT: f32 = 220.0;

/// The text Copy puts on the clipboard: the title, a blank line, then the
/// full untruncated detail (just the title when there is no detail).
pub(crate) fn copy_text(title: &str, detail: &str) -> String {
    let detail = detail.trim_end();
    if detail.is_empty() {
        title.to_owned()
    } else if title.trim().is_empty() {
        detail.to_owned()
    } else {
        format!("{title}\n\n{detail}")
    }
}

/// Split one error string into a title line and the full detail: the first
/// line is the title and the whole string stays as detail when it spans
/// several lines or is long; a short single-line error is title-only.
pub(crate) fn split_error(text: &str) -> (SharedString, SharedString) {
    let text = text.trim();
    match text.split_once('\n') {
        Some((first, rest)) => (
            first.trim().to_owned().into(),
            rest.trim().to_owned().into(),
        ),
        None => (text.to_owned().into(), SharedString::default()),
    }
}

/// Whether a free-text sidebar notice reports a failure (so it gets the
/// persistent, copyable card) rather than a status like "Path copied".
pub(crate) fn text_reports_failure(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "fail",
        "couldn",
        "could not",
        "can't",
        "cannot",
        "can\u{2019}t",
        "error",
        "unable",
        "not connected",
        "rejected",
        "refused",
        "fatal",
        "hung up",
        "denied",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

type Dismiss = Arc<dyn Fn(&mut Window, &mut App) + 'static>;

/// A persistent, copyable error card (see the module docs).
#[derive(IntoElement)]
pub(crate) struct ErrorNotice {
    id: ElementId,
    theme: Theme,
    title: SharedString,
    detail: SharedString,
    /// What Copy writes; the exact source text when built from one string.
    copy: Option<SharedString>,
    on_dismiss: Option<Dismiss>,
}

impl ErrorNotice {
    pub(crate) fn new(
        id: impl Into<ElementId>,
        theme: &Theme,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            theme: theme.clone(),
            title: title.into(),
            detail: detail.into(),
            copy: None,
            on_dismiss: None,
        }
    }

    /// One error string: first line is the title, any further lines the
    /// detail (the whole text is what Copy writes either way).
    pub(crate) fn from_text(id: impl Into<ElementId>, theme: &Theme, text: &str) -> Self {
        let (title, detail) = split_error(text);
        let mut notice = Self::new(id, theme, title, detail);
        notice.copy = Some(text.trim().to_owned().into());
        notice
    }

    /// Override what Copy writes (the exact source text of the error).
    pub(crate) fn with_copy(mut self, copy: impl Into<SharedString>) -> Self {
        self.copy = Some(copy.into());
        self
    }

    pub(crate) fn on_dismiss(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Arc::new(f));
        self
    }
}

impl RenderOnce for ErrorNotice {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            theme,
            title,
            detail,
            copy,
            on_dismiss,
        } = self;
        let accent = theme.danger;
        let muted = theme.danger_muted;
        let copied = window.use_keyed_state(
            SharedString::from(format!("error-notice-copied-{id}")),
            cx,
            |_, _| false,
        );
        let is_copied = *copied.read(cx);
        let text = copy.map_or_else(|| copy_text(&title, &detail), |text| text.to_string());

        let button = |button_id: SharedString, label: &'static str| {
            let selector = button_id.to_string();
            div()
                .id(button_id)
                .debug_selector(move || selector.clone())
                .flex_none()
                .px(px(8.0))
                .py(px(2.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(accent.opacity(0.35))
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(muted)
                .cursor_pointer()
                .hover(move |s| s.bg(accent.opacity(0.14)))
                // The card may sit in a click-to-dismiss container; the
                // buttons must not also trigger it.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(label)
        };

        // The selector flips with the label so tests can see the feedback.
        let copy_selector = if is_copied { "copied" } else { "copy" };
        let copy_button = button(
            format!("{id}-{copy_selector}").into(),
            if is_copied { "Copied" } else { "Copy" },
        )
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
            copied.update(cx, |value, cx| {
                *value = true;
                cx.notify();
            });
            let copied = copied.clone();
            cx.spawn(async move |cx| {
                cx.background_executor().timer(COPIED_FEEDBACK).await;
                copied.update(cx, |value, cx| {
                    *value = false;
                    cx.notify();
                });
            })
            .detach();
        });

        let dismiss_button = on_dismiss.map(|dismiss| {
            button(format!("{id}-dismiss").into(), "Dismiss").on_click(move |_, window, cx| {
                cx.stop_propagation();
                dismiss(window, cx);
            })
        });

        let card_selector = id.to_string();
        div()
            .id(id.clone())
            .debug_selector(move || card_selector.clone())
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(accent.opacity(0.55))
            .bg(accent.opacity(0.10))
            .px(px(12.0))
            .py(px(10.0))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(8.0))
                    .child(
                        crate::icons::icon(crate::icons::DANGER_TRIANGLE)
                            .size(px(16.0))
                            .mt(px(1.0))
                            .text_color(accent),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(crate::typography::ui_rems(13.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(muted)
                            .child(title),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .gap(px(6.0))
                            .child(copy_button)
                            .children(dismiss_button),
                    ),
            )
            .when(!detail.is_empty(), |el| {
                el.child(
                    div()
                        .id((id, "detail"))
                        .min_w_0()
                        .w_full()
                        .max_h(px(DETAIL_MAX_HEIGHT))
                        .overflow_y_scroll()
                        .font_family(theme.font_mono.clone())
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text.opacity(0.85))
                        .child(detail),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Render, TestAppContext, VisualTestContext};

    const GIT_ERROR: &str = "git worktree add /tmp/wt failed\nPreparing worktree (new branch 'x')\n\
        git-lfs filter-process: git-lfs: command not found\n\
        fatal: the remote end hung up unexpectedly";

    struct Host {
        text: SharedString,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::of(cx).clone();
            div()
                .w(px(420.0))
                .child(ErrorNotice::from_text("err", &theme, &self.text))
        }
    }

    fn host<'a>(cx: &'a mut TestAppContext, text: &str) -> &'a mut VisualTestContext {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let text = text.to_owned();
        let (_, vcx) = cx.add_window_view(move |_, _| Host { text: text.into() });
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        vcx
    }

    #[test]
    fn copy_text_is_title_then_the_full_detail() {
        assert_eq!(copy_text("Boom", ""), "Boom");
        assert_eq!(copy_text("Boom", "a\nb\n"), "Boom\n\na\nb");
        assert_eq!(copy_text("", "only detail"), "only detail");
        // Long detail is never truncated in the model.
        let long = "x".repeat(50_000);
        assert_eq!(copy_text("T", &long).len(), 50_000 + 3);
    }

    #[test]
    fn multi_line_errors_split_into_title_and_full_detail() {
        let (title, detail) = split_error(GIT_ERROR);
        assert_eq!(title, "git worktree add /tmp/wt failed");
        assert!(detail.contains("git-lfs: command not found"));
        assert!(detail.ends_with("hung up unexpectedly"));
        let (title, detail) = split_error("  one line  ");
        assert_eq!((title.as_ref(), detail.as_ref()), ("one line", ""));
    }

    #[test]
    fn failure_wording_is_told_apart_from_status_text() {
        assert!(text_reports_failure("Couldn\u{2019}t rename chat: boom"));
        assert!(text_reports_failure("Sign in failed: nope"));
        assert!(text_reports_failure("Engine not connected"));
        assert!(!text_reports_failure("Path copied"));
        assert!(!text_reports_failure("Syncing\u{2026}"));
    }

    #[gpui::test]
    fn copy_writes_the_exact_full_text_and_shows_copied(cx: &mut TestAppContext) {
        let vcx = host(cx, GIT_ERROR);
        assert!(vcx.debug_bounds("err-copied").is_none());
        let at = vcx.debug_bounds("err-copy").expect("Copy button").center();
        vcx.simulate_click(at, gpui::Modifiers::default());
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        let copied = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied.as_deref(), Some(GIT_ERROR));
        assert!(vcx.debug_bounds("err-copied").is_some(), "Copied feedback");
        // The feedback is transient; the card itself is not.
        vcx.executor().advance_clock(COPIED_FEEDBACK * 2);
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(vcx.debug_bounds("err-copy").is_some());
    }

    #[gpui::test]
    fn long_detail_renders_in_a_bounded_scrollable_block(cx: &mut TestAppContext) {
        let detail = (0..400)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let vcx = host(cx, &format!("Boom\n{detail}"));
        let bounds = vcx.debug_bounds("err").expect("card drawn");
        assert!(
            bounds.size.height < px(DETAIL_MAX_HEIGHT + 120.0),
            "the card caps its height and scrolls instead of growing: {:?}",
            bounds.size
        );
        // Copy still carries every line.
        let at = vcx.debug_bounds("err-copy").unwrap().center();
        vcx.simulate_click(at, gpui::Modifiers::default());
        let copied = vcx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap();
        assert!(copied.ends_with("line 399"));
        assert_eq!(copied.lines().count(), 401);
    }
}
