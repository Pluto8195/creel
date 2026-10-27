//! Zoom for the Artifact tab: the level and its math, the keys, the
//! per-artifact memory, and the element that turns pinch and ⌘-scroll into
//! zoom steps for natively drawn surfaces (Markdown, images). HTML and
//! diagrams zoom inside their web frame (`find::FRAME_SCRIPT`), which has no
//! gpui events to listen to.

use std::collections::HashMap;

use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    PinchEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent, TouchPhase, Window,
};

pub(crate) const MIN_ZOOM: f32 = 0.25;
pub(crate) const MAX_ZOOM: f32 = 8.0;
/// One key press: 10 percentage points.
pub(crate) const ZOOM_STEP: f32 = 0.1;
/// Zoom per scrolled pixel with ⌘ or Ctrl held (continuous, exponential).
const SCROLL_RATE: f32 = 0.01;

/// Keys (stored `mod-…` form); `=` and `+` both zoom in, with or without ⇧.
pub(crate) const ZOOM_IN_COMBOS: [&str; 4] = ["mod-=", "mod-+", "mod-shift-=", "mod-shift-+"];
pub(crate) const ZOOM_OUT_COMBOS: [&str; 2] = ["mod--", "mod-shift--"];
pub(crate) const ZOOM_RESET_COMBO: &str = "mod-0";

gpui::actions!(artifact_zoom, [ZoomIn, ZoomOut, ZoomReset]);

/// Bind the zoom keys inside the Artifact tab. ⌘0 also jumps to a session
/// elsewhere in the app; the tab's context is deeper, so it wins while an
/// artifact has focus.
pub(crate) fn bind_keys(cx: &mut App) {
    use crate::settings::platform_combo;
    let context = Some(super::tab::KEY_CONTEXT);
    let mut bindings = Vec::new();
    for combo in ZOOM_IN_COMBOS {
        bindings.push(gpui::KeyBinding::new(
            &platform_combo(combo),
            ZoomIn,
            context,
        ));
    }
    for combo in ZOOM_OUT_COMBOS {
        bindings.push(gpui::KeyBinding::new(
            &platform_combo(combo),
            ZoomOut,
            context,
        ));
    }
    bindings.push(gpui::KeyBinding::new(
        &platform_combo(ZOOM_RESET_COMBO),
        ZoomReset,
        context,
    ));
    cx.bind_keys(bindings);
}

/// The keys a web page hands to GPUI so zoom works while it has focus.
pub(crate) const PAGE_FORWARDED_KEYS: [&str; 6] = [
    "cmd-=",
    "cmd-+",
    "cmd-shift-=",
    "cmd-shift-+",
    "cmd--",
    "cmd-0",
];

/// Clamp to the supported range and snap float noise away.
pub(crate) fn clamp(level: f32) -> f32 {
    let level = if level.is_finite() { level } else { 1.0 };
    let level = level.clamp(MIN_ZOOM, MAX_ZOOM);
    (level * 10_000.0).round() / 10_000.0
}

/// The next key step: lands on the next multiple of 10%.
pub(crate) fn step(level: f32, direction: i32) -> f32 {
    let tenths = level * 10.0;
    let next = if direction > 0 {
        (tenths + 1e-3).floor() + 1.0
    } else {
        (tenths - 1e-3).ceil() - 1.0
    };
    clamp(next / 10.0)
}

/// A pinch of `delta` (0.1 = 10% wider) applied to `level`.
pub(crate) fn pinch(level: f32, delta: f32) -> f32 {
    clamp(level * (1.0 + delta).max(0.01))
}

/// ⌘-scroll or Ctrl-scroll: scrolling up zooms in.
pub(crate) fn scroll_zoom(level: f32, pixels_up: f32) -> f32 {
    clamp(level * (pixels_up * SCROLL_RATE).exp())
}

/// The scroll offset (distance from the content's start, along one axis)
/// that keeps the content point under `anchor` (distance from the viewport's
/// start) still when the zoom changes from `old` to `new`.
pub(crate) fn anchor_offset(scroll: f32, anchor: f32, old: f32, new: f32) -> f32 {
    if old <= 0.0 {
        return scroll;
    }
    ((scroll + anchor) * new / old - anchor).max(0.0)
}

/// "125%".
pub(crate) fn label(level: f32) -> String {
    format!("{}%", (level * 100.0).round() as i32)
}

pub(crate) fn is_default(level: f32) -> bool {
    (level - 1.0).abs() < 1e-3
}

/// The zoom of each artifact opened in this session, kept after its tab
/// closes.
#[derive(Default)]
pub(crate) struct ZoomMemory(HashMap<String, f32>);

impl gpui::Global for ZoomMemory {}

impl ZoomMemory {
    pub(crate) fn recall(cx: &App, artifact_id: &str) -> f32 {
        cx.try_global::<ZoomMemory>()
            .and_then(|memory| memory.0.get(artifact_id).copied())
            .unwrap_or(1.0)
    }

    pub(crate) fn remember(cx: &mut App, artifact_id: &str, level: f32) {
        let memory = cx.default_global::<ZoomMemory>();
        if is_default(level) {
            memory.0.remove(artifact_id);
        } else {
            memory.0.insert(artifact_id.to_owned(), level);
        }
    }
}

/// What the gesture layer asks for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ZoomInput {
    /// A new level, anchored at a point in window coordinates.
    To { level: f32, anchor: Point<Pixels> },
}

/// Wraps a native surface: pinch and ⌘ / Ctrl-scroll over it become
/// [`ZoomInput`]s (captured before the child scrolls), and the child is laid
/// out and painted with a rem size scaled by `scale`, so text sized in rems
/// grows with the zoom.
pub(crate) struct ZoomSurface {
    level: f32,
    scale_text: bool,
    on_zoom: std::rc::Rc<dyn Fn(ZoomInput, &mut Window, &mut App)>,
    child: gpui::AnyElement,
}

pub(crate) fn zoom_surface(
    level: f32,
    scale_text: bool,
    on_zoom: impl Fn(ZoomInput, &mut Window, &mut App) + 'static,
    child: impl IntoElement,
) -> ZoomSurface {
    ZoomSurface {
        level,
        scale_text,
        on_zoom: std::rc::Rc::new(on_zoom),
        child: child.into_any_element(),
    }
}

impl ZoomSurface {
    fn rem(&self, window: &Window) -> Option<Pixels> {
        (self.scale_text && !is_default(self.level)).then(|| window.rem_size() * self.level)
    }
}

impl Element for ZoomSurface {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let rem = self.rem(window);
        let child = &mut self.child;
        (
            window.with_rem_size(rem, |window| child.request_layout(window, cx)),
            (),
        )
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let rem = self.rem(window);
        let child = &mut self.child;
        window.with_rem_size(rem, |window| child.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let level = self.level;
        let rem = self.rem(window);
        let on_zoom = self.on_zoom.clone();
        window.on_mouse_event(move |event: &PinchEvent, phase, window, cx| {
            if phase != gpui::DispatchPhase::Capture || !bounds.contains(&event.position) {
                return;
            }
            if matches!(event.phase, TouchPhase::Cancelled) {
                return;
            }
            on_zoom(
                ZoomInput::To {
                    level: pinch(level, event.delta),
                    anchor: event.position,
                },
                window,
                cx,
            );
            cx.stop_propagation();
        });
        let on_zoom = self.on_zoom.clone();
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase != gpui::DispatchPhase::Capture
                || !bounds.contains(&event.position)
                || !(event.modifiers.platform || event.modifiers.control)
            {
                return;
            }
            let up = match event.delta {
                ScrollDelta::Pixels(delta) => f32::from(delta.y),
                ScrollDelta::Lines(delta) => delta.y * 16.0,
            };
            if up != 0.0 {
                on_zoom(
                    ZoomInput::To {
                        level: scroll_zoom(level, up),
                        anchor: event.position,
                    },
                    window,
                    cx,
                );
            }
            cx.stop_propagation();
        });
        let child = &mut self.child;
        window.with_rem_size(rem, |window| child.paint(window, cx));
    }
}

impl IntoElement for ZoomSurface {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_land_on_tenths() {
        assert_eq!(step(1.0, 1), 1.1);
        assert_eq!(step(1.1, 1), 1.2);
        assert_eq!(step(1.0, -1), 0.9);
        assert_eq!(step(1.137, 1), 1.2);
        assert_eq!(step(1.137, -1), 1.1);
        assert_eq!(step(1.1, -1), 1.0);
    }

    #[test]
    fn limits_are_25_to_800_percent() {
        assert_eq!(step(MAX_ZOOM, 1), MAX_ZOOM);
        assert_eq!(step(MIN_ZOOM, -1), MIN_ZOOM);
        assert_eq!(clamp(100.0), MAX_ZOOM);
        assert_eq!(clamp(0.0), MIN_ZOOM);
        assert_eq!(clamp(f32::NAN), 1.0);
        assert_eq!(pinch(7.9, 0.5), MAX_ZOOM);
        assert_eq!(scroll_zoom(0.3, -500.0), MIN_ZOOM);
    }

    #[test]
    fn pinch_and_scroll_are_continuous() {
        assert!((pinch(1.0, 0.1) - 1.1).abs() < 1e-4);
        assert!(pinch(1.0, -0.1) < 1.0);
        assert!(scroll_zoom(1.0, 10.0) > 1.0);
        assert!(scroll_zoom(1.0, -10.0) < 1.0);
        assert_eq!(scroll_zoom(1.0, 0.0), 1.0);
    }

    #[test]
    fn anchor_keeps_the_point_under_the_pointer() {
        // Pointer 100px into the viewport, scrolled 300px, zoom 1 -> 2: the
        // content point at 400 becomes 800, so scroll 700 keeps it at 100.
        assert_eq!(anchor_offset(300.0, 100.0, 1.0, 2.0), 700.0);
        // Zooming back restores the offset.
        assert_eq!(anchor_offset(700.0, 100.0, 2.0, 1.0), 300.0);
        // Zooming out past the start clamps at 0.
        assert_eq!(anchor_offset(0.0, 100.0, 1.0, 0.25), 0.0);
        // Anchor at the origin scales the scroll.
        assert_eq!(anchor_offset(40.0, 0.0, 1.0, 1.5), 60.0);
        // No change, no move.
        assert_eq!(anchor_offset(123.0, 57.0, 1.3, 1.3), 123.0);
    }

    #[test]
    fn labels_round_to_whole_percent() {
        assert_eq!(label(1.0), "100%");
        assert_eq!(label(1.25), "125%");
        assert_eq!(label(0.25), "25%");
        assert_eq!(label(8.0), "800%");
        assert!(is_default(1.0) && !is_default(1.1));
    }
}
