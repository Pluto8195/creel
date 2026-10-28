//! The width of the Overview's right-hand Work Item Details (and Required
//! Actions) panel: a persisted preference (an entry of the Overview's
//! `UI_FLAGS_FILE`, like the Linear column's), the edge drag, the
//! double-click reset, and the clamp against the window. The preference is
//! never rewritten by a narrow window; only the width actually shown is
//! clamped, so widening the window again restores the choice.
//!
//! A drag only notifies the Overview (never `AppState`), so the observer
//! gating and `overview_fingerprint` are untouched.
use std::collections::BTreeSet;

use gpui::{AnyElement, Context, MouseButton, Window, div, prelude::*, px};

use super::Overview;
use super::linear_host::OVERVIEW_BODY_MIN_W;
use crate::theme::Theme;

pub(crate) const DETAILS_MIN_W: f32 = 320.0;
pub(crate) const DETAILS_DEFAULT_W: f32 = 380.0;
/// Largest share of the Overview the panel may take.
pub(crate) const DETAILS_MAX_FRACTION: f32 = 0.6;
/// Upper bound before the Overview's own width is known.
const DETAILS_ABS_MAX_W: f32 = 900.0;
/// Hit target of the panel's left-edge grabber.
const RESIZE_HANDLE_W: f32 = 6.0;
/// UI-flag prefix holding the width in whole px (`detailsPanelWidth:420`).
const WIDTH_PREFIX: &str = "detailsPanelWidth:";

/// Drag marker for the panel's edge grabber.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DetailsPanelResize;

struct DetailsResizeGhost;

impl gpui::Render for DetailsResizeGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// The widest the panel may be in an Overview `overview_w` wide (`None`
/// before the first paint): 60% of it, and never so wide the body drops
/// below [`OVERVIEW_BODY_MIN_W`]; the minimum always wins.
pub(crate) fn details_max_width(overview_w: Option<f32>) -> f32 {
    let Some(total) = overview_w else {
        return DETAILS_ABS_MAX_W;
    };
    (total * DETAILS_MAX_FRACTION)
        .min(total - OVERVIEW_BODY_MIN_W)
        .floor()
        .max(DETAILS_MIN_W)
}

/// `preferred` clamped to what fits this window.
pub(crate) fn details_width(preferred: f32, overview_w: Option<f32>) -> f32 {
    if !preferred.is_finite() {
        return DETAILS_DEFAULT_W.min(details_max_width(overview_w));
    }
    preferred.clamp(DETAILS_MIN_W, details_max_width(overview_w))
}

/// The persisted preference; `None` when absent or unparseable.
pub(crate) fn width_from_flags(flags: &BTreeSet<String>) -> Option<f32> {
    flags
        .iter()
        .filter_map(|flag| flag.strip_prefix(WIDTH_PREFIX))
        .find_map(|value| value.parse::<f32>().ok())
        .filter(|width| width.is_finite())
        .map(|width| width.max(DETAILS_MIN_W))
}

/// Replace the width entry (every older one is purged).
pub(crate) fn set_width_flag(flags: &mut BTreeSet<String>, width: f32) {
    flags.retain(|flag| !flag.starts_with(WIDTH_PREFIX));
    flags.insert(format!(
        "{WIDTH_PREFIX}{}",
        width.max(DETAILS_MIN_W).round()
    ));
}

/// The panel's width state on the Overview.
pub(crate) struct DetailsWidthState {
    /// The user's preference (persisted); clamped when shown.
    pub(crate) width: f32,
    /// Pointer x and shown width when an edge drag began.
    resize_from: Option<(f32, f32)>,
    /// Width shown last frame (the drag's base).
    shown: f32,
}

impl Default for DetailsWidthState {
    fn default() -> Self {
        Self {
            width: DETAILS_DEFAULT_W,
            resize_from: None,
            shown: DETAILS_DEFAULT_W,
        }
    }
}

impl Overview {
    #[cfg(test)]
    pub(crate) fn details_width_state(&self) -> &DetailsWidthState {
        &self.details_width
    }

    pub(super) fn restore_details_width(&mut self, flags: &BTreeSet<String>) {
        if let Some(width) = width_from_flags(flags) {
            self.details_width.width = width;
            self.details_width.shown = width;
        }
    }

    /// The width the right slot shows this frame.
    pub(super) fn shown_details_width(&mut self) -> f32 {
        let shown = details_width(self.details_width.width, self.overview_width());
        self.details_width.shown = shown;
        shown
    }

    /// Edge drag: the shown width minus the pointer's travel (the panel
    /// grows leftwards), clamped to the window.
    pub(super) fn details_resize_drag(&mut self, x: f32, cx: &mut Context<Self>) {
        let Some((start_x, start_w)) = self.details_width.resize_from else {
            return;
        };
        let width = details_width(start_w - (x - start_x), self.overview_width());
        if (width - self.details_width.width).abs() >= 0.5 {
            self.details_width.width = width;
            cx.notify();
        }
    }

    pub(super) fn finish_details_resize(&mut self, cx: &mut Context<Self>) {
        if self.details_width.resize_from.take().is_some() {
            self.persist_details_width(cx);
        }
    }

    pub(super) fn reset_details_width(&mut self, cx: &mut Context<Self>) {
        self.details_width.resize_from = None;
        self.details_width.width = DETAILS_DEFAULT_W;
        self.persist_details_width(cx);
        cx.notify();
    }

    pub(super) fn persist_details_width(&self, cx: &Context<Self>) {
        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
            return;
        };
        let file = crate::overview_positions::UI_FLAGS_FILE;
        let mut flags = crate::overview_positions::load_string_set(&data_dir, file);
        set_width_flag(&mut flags, self.details_width.width);
        self.save_string_set(file, &flags, cx);
    }

    /// The right slot's panel wrapped at `width` with the grabber on its
    /// left edge.
    pub(super) fn details_slot(
        &self,
        panel: AnyElement,
        width: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover = theme.border_strong;
        let active = self.details_width.resize_from.is_some();
        let handle = div()
            .id("details-panel-resize")
            .debug_selector(|| "details-panel-resize".into())
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .w(px(RESIZE_HANDLE_W))
            .cursor_col_resize()
            .when(active, |el| el.bg(hover.opacity(0.7)))
            .hover(move |style| style.bg(hover.opacity(0.7)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                    this.details_width.resize_from =
                        Some((f32::from(event.position.x), this.details_width.shown));
                    cx.notify();
                }),
            )
            .on_drag(DetailsPanelResize, |_, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| DetailsResizeGhost)
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseUpEvent, _, cx| {
                    if event.click_count == 2 {
                        this.reset_details_width(cx);
                    } else {
                        this.finish_details_resize(cx);
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_details_resize(cx)),
            );
        div()
            .relative()
            .h_full()
            .flex_none()
            .w(px(width))
            .child(panel)
            .child(handle)
            .into_any_element()
    }
}
