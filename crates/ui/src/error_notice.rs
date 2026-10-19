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

type Dismiss = Arc<dyn Fn(&mut Window, &mut App) + 'static>;

/// A persistent, copyable error card (see the module docs).
#[derive(IntoElement)]
pub(crate) struct ErrorNotice {
    id: ElementId,
    theme: Theme,
    title: SharedString,
    detail: SharedString,
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
            on_dismiss: None,
        }
    }

    /// One error string: first line is the title, any further lines the
    /// detail (the whole text is what Copy writes either way).
    pub(crate) fn from_text(id: impl Into<ElementId>, theme: &Theme, text: &str) -> Self {
        let (title, detail) = split_error(text);
        Self::new(id, theme, title, detail)
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
            on_dismiss,
        } = self;
        let accent = theme.danger;
        let muted = theme.danger_muted;
        let copied = window.use_keyed_state(
            SharedString::from(format!("error-notice-copied-{id:?}")),
            cx,
            |_, _| false,
        );
        let is_copied = *copied.read(cx);
        let text = copy_text(&title, &detail);

        let button = |button_id: SharedString, label: &'static str| {
            div()
                .id(button_id)
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

        let copy_button = button(
            format!("{id:?}-copy").into(),
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
            button(format!("{id:?}-dismiss").into(), "Dismiss").on_click(move |_, window, cx| {
                cx.stop_propagation();
                dismiss(window, cx);
            })
        });

        div()
            .id(id.clone())
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
