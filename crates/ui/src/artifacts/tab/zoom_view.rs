//! Zoom in the Artifact tab: the level (remembered per artifact for the
//! session), the keys, pinch and ⌘-scroll, and the control in the toolbar.
//!
//! - Markdown: the preview scales its text, code and column width
//!   (`MarkdownPreview::set_zoom` plus a rem-scaling [`zoom_surface`]).
//! - HTML, diagrams: CSS zoom inside the web frame (`find::FRAME_SCRIPT`),
//!   which also handles pinch and ⌘-scroll there. The page reports its zoom
//!   back through the poll, and the tab follows.
//! - Images: drawn at the fit size times the zoom inside a scroll area.
//!
//! 100% is the default view (fit, for images).

use std::sync::Arc;

use gpui::{
    AnyElement, Context, Image, ObjectFit, Pixels, Point, SharedString, Window, div, point,
    prelude::*, px,
};

use super::{ArtifactTab, Presenter};
use crate::artifacts::find::{FrameCommand, command_script};
use crate::artifacts::zoom::{self, ZoomInput, ZoomMemory};
use crate::surface_chrome;
use crate::theme::Theme;

/// Padding around a zoomed image.
const IMAGE_PADDING: f32 = 12.0;

/// The natural size of an encoded image, read from its header.
pub(super) fn natural_size(bytes: &[u8]) -> Option<(f32, f32)> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let (width, height) = reader.into_dimensions().ok()?;
    (width > 0 && height > 0).then_some((width as f32, height as f32))
}

/// The size an image of `natural` pixels is drawn at: shrunk to fit
/// `viewport` (never enlarged), times `level`.
pub(crate) fn image_display_size(
    natural: (f32, f32),
    viewport: (f32, f32),
    level: f32,
) -> (f32, f32) {
    let fit = (viewport.0 / natural.0)
        .min(viewport.1 / natural.1)
        .clamp(0.0, 1.0);
    (natural.0 * fit * level, natural.1 * fit * level)
}

impl ArtifactTab {
    #[cfg(test)]
    pub(crate) fn markdown_zoom_for_tests(&self, cx: &gpui::App) -> f32 {
        self.markdown
            .as_ref()
            .map_or(0.0, |preview| preview.read(cx).zoom())
    }

    #[cfg(test)]
    pub(crate) fn markdown_item_heights_for_tests(&self, cx: &gpui::App) -> Vec<f32> {
        let list = self.markdown.as_ref().unwrap().read(cx).list_state();
        (0..list.item_count())
            .filter_map(|ix| list.bounds_for_item(ix))
            .map(|bounds| f32::from(bounds.size.height))
            .collect()
    }

    pub(crate) fn zoom_level(&self) -> f32 {
        self.zoom
    }

    /// Whether the body on screen zooms.
    pub(crate) fn zoom_applies(&self) -> bool {
        matches!(
            self.presenter(),
            Presenter::Markdown | Presenter::Web(_) | Presenter::Image
        ) || self.compare_zooms()
            || self.side_shows_web()
    }

    /// ⌘+ / ⌘−: the next 10%. `false` when the body doesn't zoom.
    pub(crate) fn zoom_step(&mut self, direction: i32, cx: &mut Context<Self>) -> bool {
        if !self.zoom_applies() {
            return false;
        }
        self.set_zoom(zoom::step(self.zoom, direction), None, cx);
        true
    }

    /// ⌘0: back to the default (fit / 100%).
    pub(crate) fn zoom_reset(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.zoom_applies() {
            return false;
        }
        self.set_zoom(1.0, None, cx);
        true
    }

    pub(super) fn apply_zoom_input(&mut self, input: ZoomInput, cx: &mut Context<Self>) {
        let ZoomInput::To { level, anchor } = input;
        self.set_zoom(level, Some(anchor), cx);
    }

    /// Set the level, keeping the content under `anchor` (a window point;
    /// the middle of the view when `None`) where it is.
    pub(crate) fn set_zoom(
        &mut self,
        level: f32,
        anchor: Option<Point<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let level = zoom::clamp(level);
        let old = self.zoom;
        if (level - old).abs() < 1e-4 {
            return;
        }
        self.zoom = level;
        ZoomMemory::remember(cx, &self.artifact_id, level);
        match self.presenter() {
            Presenter::Markdown => {
                if let Some(preview) = &self.markdown {
                    preview.update(cx, |preview, cx| preview.set_zoom(level, cx));
                }
            }
            Presenter::Web(_) => self.send_web_zoom(level, cx),
            Presenter::Image => self.anchor_image_scroll(old, level, anchor),
            // Both compare panes take the level together: the Markdown
            // previews directly, the two frames through the host document.
            Presenter::Compare => {
                if let Some((left, right)) = self.compare.as_ref().and_then(|v| v.markdown.clone())
                {
                    for preview in [left, right] {
                        preview.update(cx, |preview, cx| preview.set_zoom(level, cx));
                    }
                } else if self.compare_shows_web() {
                    self.send_web_zoom(level, cx);
                }
            }
            Presenter::SideBySide => self.send_web_zoom(level, cx),
            _ => {}
        }
        cx.notify();
    }

    fn send_web_zoom(&mut self, level: f32, cx: &mut Context<Self>) {
        let script = command_script(&FrameCommand::Zoom(level));
        if let Some(web) = &mut self.web {
            web.run_script(&script, cx);
        }
    }

    /// Re-apply the level to a surface that was just (re)created.
    pub(super) fn apply_zoom_to_new_surface(&mut self, cx: &mut Context<Self>) {
        let level = self.zoom;
        if let Some(preview) = &self.markdown {
            preview.update(cx, |preview, cx| preview.set_zoom(level, cx));
        }
        if let Some((left, right)) = self.compare.as_ref().and_then(|view| view.markdown.clone()) {
            for preview in [left, right] {
                preview.update(cx, |preview, cx| preview.set_zoom(level, cx));
            }
        }
        // A fresh web document starts at 100%; it gets the level once it has
        // reported in (`adopt_web_zoom`).
        self.zoom_synced = false;
    }

    /// The page's reported zoom (per mille). The first report of a document
    /// restores the remembered level; later changes (pinch, ⌘-scroll inside
    /// the page) are adopted.
    pub(crate) fn adopt_web_zoom(&mut self, reported: Option<u32>, cx: &mut Context<Self>) {
        let Some(permille) = reported else {
            return;
        };
        if !self.zoom_synced {
            self.zoom_synced = true;
            self.zoom_reported = Some(permille);
            if !zoom::is_default(self.zoom) {
                self.send_web_zoom(self.zoom, cx);
            }
            return;
        }
        if self.zoom_reported == Some(permille) {
            return;
        }
        self.zoom_reported = Some(permille);
        let level = zoom::clamp(permille as f32 / 1000.0);
        if (level - self.zoom).abs() > 6e-4 {
            self.zoom = level;
            ZoomMemory::remember(cx, &self.artifact_id, level);
            cx.notify();
        }
    }

    /// Whether the zoomed image is larger than its view, so it can be panned.
    fn image_pannable(&self) -> bool {
        let max = self.image_scroll.max_offset();
        max.x > px(0.5) || max.y > px(0.5)
    }

    fn start_image_pan(&mut self, mouse: Point<Pixels>, cx: &mut Context<Self>) {
        if self.image_pannable() {
            self.image_pan = Some((mouse, self.image_scroll.offset()));
            cx.notify();
        }
    }

    fn move_image_pan(&mut self, mouse: Point<Pixels>, cx: &mut Context<Self>) {
        let Some((from, start)) = self.image_pan else {
            return;
        };
        let max = self.image_scroll.max_offset();
        let (x, y) = zoom::drag_pan_offset(
            (f32::from(start.x), f32::from(start.y)),
            (f32::from(from.x), f32::from(from.y)),
            (f32::from(mouse.x), f32::from(mouse.y)),
            (f32::from(max.x), f32::from(max.y)),
        );
        self.image_scroll.set_offset(point(px(x), px(y)));
        cx.notify();
    }

    fn end_image_pan(&mut self, cx: &mut Context<Self>) {
        if self.image_pan.take().is_some() {
            cx.notify();
        }
    }

    #[cfg(test)]
    pub(crate) fn image_offset_for_tests(&self) -> (f32, f32) {
        let offset = self.image_scroll.offset();
        (f32::from(offset.x), f32::from(offset.y))
    }

    #[cfg(test)]
    pub(crate) fn image_max_offset_for_tests(&self) -> (f32, f32) {
        let max = self.image_scroll.max_offset();
        (f32::from(max.x), f32::from(max.y))
    }

    #[cfg(test)]
    pub(crate) fn image_pan_for_tests(
        &mut self,
        action: &str,
        mouse: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        match action {
            "down" => self.start_image_pan(mouse, cx),
            "move" => self.move_image_pan(mouse, cx),
            _ => self.end_image_pan(cx),
        }
    }

    fn anchor_image_scroll(&mut self, old: f32, new: f32, anchor: Option<Point<Pixels>>) {
        let bounds = self.image_scroll.bounds();
        let anchor = anchor
            .map(|point| point - bounds.origin)
            .unwrap_or_else(|| point(bounds.size.width / 2.0, bounds.size.height / 2.0));
        let offset = self.image_scroll.offset();
        let x = zoom::anchor_offset(-f32::from(offset.x), f32::from(anchor.x), old, new);
        let y = zoom::anchor_offset(-f32::from(offset.y), f32::from(anchor.y), old, new);
        self.image_scroll.set_offset(point(px(-x), px(-y)));
    }

    /// The natively drawn image at the current zoom.
    pub(super) fn render_image_body(
        &self,
        image: &Arc<Image>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let viewport = self.image_scroll.bounds().size;
        let display = self
            .image_size
            .filter(|_| viewport.width > px(1.0))
            .map(|natural| {
                image_display_size(
                    natural,
                    (f32::from(viewport.width), f32::from(viewport.height)),
                    self.zoom,
                )
            });
        if display.is_none() && !zoom::is_default(self.zoom) {
            // The scroll area hasn't been measured yet: draw once more.
            let entity = cx.entity();
            window.defer(cx, move |_, cx| entity.update(cx, |_, cx| cx.notify()));
        }
        let picture = match display {
            Some((width, height)) => gpui::img(image.clone())
                .w(px(width))
                .h(px(height))
                .flex_none()
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            None => gpui::img(image.clone())
                .max_w_full()
                .max_h_full()
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
        };
        let weak = cx.weak_entity();
        let pannable = self.image_pannable();
        let panning = self.image_pan.is_some();
        zoom::zoom_surface(
            self.zoom,
            false,
            move |input, _, cx| {
                weak.update(cx, |this, cx| this.apply_zoom_input(input, cx))
                    .ok();
            },
            div()
                .debug_selector(|| "artifact-image".into())
                .size_full()
                .p(px(IMAGE_PADDING))
                .child(
                    div()
                        .id("artifact-image-scroll")
                        .size_full()
                        .overflow_scroll()
                        .track_scroll(&self.image_scroll)
                        .when(pannable, |el| {
                            el.cursor(if panning {
                                gpui::CursorStyle::ClosedHand
                            } else {
                                gpui::CursorStyle::OpenHand
                            })
                        })
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                                this.start_image_pan(event.position, cx)
                            }),
                        )
                        .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                            if event.dragging() {
                                this.move_image_pan(event.position, cx);
                            } else {
                                this.end_image_pan(cx);
                            }
                        }))
                        .on_mouse_up(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| this.end_image_pan(cx)),
                        )
                        .on_mouse_up_out(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| this.end_image_pan(cx)),
                        )
                        .child(
                            div()
                                .min_w_full()
                                .min_h_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(picture),
                        ),
                ),
        )
        .into_any_element()
    }

    /// "−  125%  +" in the toolbar, for bodies that zoom. Dim at 100%.
    pub(super) fn render_zoom_control(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.zoom_applies() {
            return None;
        }
        let level = self.zoom;
        let default = zoom::is_default(level);
        let color = if default {
            theme.text_faint
        } else {
            theme.accent
        };
        let button = |id: &'static str, text: SharedString, enabled: bool| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .flex_none()
                .h(px(surface_chrome::CONTROL_SIZE))
                .min_w(px(surface_chrome::CONTROL_SIZE))
                .px(px(4.0))
                .rounded(px(surface_chrome::CONTROL_RADIUS))
                .flex()
                .items_center()
                .justify_center()
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_muted)
                .when(enabled, |el| {
                    el.cursor_pointer()
                        .hover(|el| el.bg(crate::theme::wash(0.14)).text_color(theme.text))
                })
                .when(!enabled, |el| el.cursor_default().opacity(0.35))
                .child(text)
        };
        Some(
            div()
                .debug_selector(|| "artifact-zoom".into())
                .flex_none()
                .flex()
                .items_center()
                .gap(px(1.0))
                .mr(px(4.0))
                .child(
                    button(
                        "artifact-zoom-out",
                        "\u{2212}".into(),
                        level > zoom::MIN_ZOOM,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.zoom_step(-1, cx);
                    })),
                )
                .child(
                    button("artifact-zoom-reset", zoom::label(level).into(), !default)
                        .min_w(px(40.0))
                        .text_color(color)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.zoom_reset(cx);
                        })),
                )
                .child(
                    button("artifact-zoom-in", "+".into(), level < zoom::MAX_ZOOM).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.zoom_step(1, cx);
                        }),
                    ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_fit_but_never_grow_past_natural_size() {
        assert_eq!(
            image_display_size((200.0, 100.0), (800.0, 600.0), 1.0),
            (200.0, 100.0)
        );
        assert_eq!(
            image_display_size((1600.0, 800.0), (800.0, 600.0), 1.0),
            (800.0, 400.0)
        );
        assert_eq!(
            image_display_size((1600.0, 800.0), (800.0, 600.0), 2.0),
            (1600.0, 800.0)
        );
        assert_eq!(
            image_display_size((200.0, 100.0), (800.0, 600.0), 0.5),
            (100.0, 50.0)
        );
    }
}
