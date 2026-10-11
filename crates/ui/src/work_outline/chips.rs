//! Shared element builders for work-item metadata on List rows, Canvas
//! cards, Outline rows, and Work Item Details.
//!
//! Attention ("User Action Required") uses the sidebar's theme vocabulary —
//! `theme.accent`, the color the sidebar gives an Awaiting Input Chat — not
//! the Overview's hardcoded critical red (m0-contract §7). Every size takes
//! the caller's zoom so Canvas cards scale like the rest of the card face.
use gpui::{AnyElement, Render, SharedString, Window, div, prelude::*, px};

use super::filter::WorkItemPathLabel;
use super::store::{WorkItemEffectiveStatus, WorkItemStatus};
use crate::theme::Theme;

/// Plain text tooltip (the Overview's own tooltip type is private to it).
pub(crate) struct TextTooltip(pub SharedString);

impl Render for TextTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_popup();
        div()
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_dialog)
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text)
            .child(self.0.clone())
    }
}

/// The attention color: sidebar Awaiting Input.
pub(crate) fn attention_color(theme: &Theme) -> gpui::Hsla {
    theme.accent
}

/// Compact work-item path chip. `id` must be unique per frame (tooltips are
/// stateful elements).
pub(crate) fn path_chip(
    id: impl Into<SharedString>,
    path: &WorkItemPathLabel,
    theme: &Theme,
    zoom: f32,
) -> AnyElement {
    let tooltip: SharedString = format!("Work item: {}", path.full).into();
    div()
        .id(id.into())
        .debug_selector(|| "work-item-path-chip".into())
        .min_w_0()
        .max_w(px(160.0 * zoom))
        .px(px(5.0 * zoom))
        .py(px(1.0 * zoom))
        .rounded(px(4.0 * zoom))
        .border_1()
        .border_color(theme.border)
        .truncate()
        .text_size(crate::typography::ui_rems(9.0 * zoom))
        .text_color(theme.text_muted)
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(tooltip.clone())).into())
        .child(SharedString::from(path.short.clone()))
        .into_any_element()
}

/// "User Action Required" indicator: an accent dot, plus the count when more
/// than one request is open. `None` when nothing needs the user.
pub(crate) fn attention_indicator(
    id: impl Into<SharedString>,
    required: bool,
    count: u32,
    theme: &Theme,
    zoom: f32,
) -> Option<AnyElement> {
    if !required && count == 0 {
        return None;
    }
    let color = attention_color(theme);
    let tooltip: SharedString = match count {
        0 | 1 => "User action required".to_string(),
        n => format!("User action required ({n})"),
    }
    .into();
    Some(
        div()
            .id(id.into())
            .debug_selector(|| "work-item-attention".into())
            .flex_none()
            .flex()
            .items_center()
            .gap(px(3.0 * zoom))
            .text_size(crate::typography::ui_rems(9.5 * zoom))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(color)
            .child(
                div()
                    .flex_none()
                    .size(px(6.0 * zoom))
                    .rounded_full()
                    .bg(color),
            )
            .when(count > 1, |el| {
                el.child(SharedString::from(count.to_string()))
            })
            .tooltip(move |_, cx| cx.new(|_| TextTooltip(tooltip.clone())).into())
            .into_any_element(),
    )
}

/// Tooltip shared by every instruction drift mark.
pub(crate) const INSTRUCTION_DRIFT_TOOLTIP: &str =
    "Instructions changed since this Chat\u{2019}s applied revision. Click to compare and apply.";

/// "Instructions changed" mark for a drifted Chat: the warning triangle the
/// Outline chip and chat header use, optionally with the label. `id` doubles
/// as the debug selector (unique per frame). It sits on an
/// existing line (title row), so it never changes a card's size. `on_click`
/// opens Compare; the mark stops mouse-down propagation so a Canvas card does
/// not start a drag or toggle under it.
pub(crate) fn instruction_drift_mark(
    id: impl Into<SharedString>,
    with_label: bool,
    theme: &Theme,
    zoom: f32,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let id: SharedString = id.into();
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector)
        .flex_none()
        .flex()
        .items_center()
        .gap(px(2.0 * zoom))
        .px(px(2.0 * zoom))
        .rounded(px(3.0 * zoom))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(10.0 * zoom))
        .text_color(theme.warning)
        .hover(|el| el.bg(theme.element_hover))
        .child(
            crate::icons::icon(crate::icons::DANGER_TRIANGLE)
                .size(px(10.0 * zoom))
                .text_color(theme.warning),
        )
        .when(with_label, |el| el.child("Instructions changed"))
        .tooltip(|_, cx| {
            cx.new(|_| TextTooltip(INSTRUCTION_DRIFT_TOOLTIP.into()))
                .into()
        })
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx);
        })
        .into_any_element()
}

/// The execution-exception mark for a Chat running with a captured
/// configuration that differs from its item's current one (List rows,
/// Canvas cards). Compact (no label) so it shares the title line and never
/// feeds `tile_size`.
pub(crate) fn execution_exception_mark(
    id: impl Into<SharedString>,
    theme: &Theme,
    zoom: f32,
) -> AnyElement {
    let id: SharedString = id.into();
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector)
        .flex_none()
        .flex()
        .items_center()
        .px(px(2.0 * zoom))
        .child(
            crate::icons::icon(crate::icons::FOLDER_WITH_FILES)
                .size(px(10.0 * zoom))
                .text_color(theme.accent),
        )
        .tooltip(|_, cx| {
            cx.new(|_| {
                TextTooltip(
                    "This Chat uses a different execution configuration than its work item".into(),
                )
            })
            .into()
        })
        .into_any_element()
}

/// Glyph + label + color for an effective status (Outline rows, details).
pub(crate) fn status_style(
    status: WorkItemEffectiveStatus,
    theme: &Theme,
) -> (&'static str, &'static str, gpui::Hsla) {
    match status {
        WorkItemEffectiveStatus::Pending => ("○", "Pending", theme.text_muted),
        WorkItemEffectiveStatus::Active => ("◐", "Active", theme.busy),
        WorkItemEffectiveStatus::UserActionRequired => {
            ("●", "User Action Required", attention_color(theme))
        }
        WorkItemEffectiveStatus::Completed => ("✓", "Completed", theme.success),
    }
}

pub(crate) fn base_status_label(status: WorkItemStatus) -> &'static str {
    match status {
        WorkItemStatus::Pending => "Pending",
        WorkItemStatus::Active => "Active",
        WorkItemStatus::Completed => "Completed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attention_uses_the_theme_accent_not_the_overview_red() {
        let theme = Theme::default();
        assert_eq!(attention_color(&theme), theme.accent);
        let (_, label, color) = status_style(WorkItemEffectiveStatus::UserActionRequired, &theme);
        assert_eq!(label, "User Action Required");
        assert_eq!(color, theme.accent);
        let critical: gpui::Hsla = gpui::rgb(0xd03b3b).into();
        assert_ne!(color, critical);
    }
}
