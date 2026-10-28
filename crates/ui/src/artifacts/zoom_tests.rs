//! Zoom in the Artifact tab: keys, limits, per-artifact memory, the toolbar
//! control, pinch and ⌘-scroll, and the web page's report. Headless gpui
//! tests with a fake web host and the real Markdown preview.
use std::time::Duration;

use gpui::{
    AppContext as _, Entity, Modifiers, PinchEvent, ScrollDelta, ScrollWheelEvent, TestAppContext,
    TouchPhase, VisualTestContext, point, px,
};
use zeron_proto::{ArtifactContentEncoding, ArtifactKind};

use super::find::{FrameCommand, command_script};
use super::tab::{ArtifactTab, FakeWebHost, WebProbe};
use super::tests::{artifact, content_reply, id, init_ui, reply_with, scripted, snapshot};
use super::zoom::{self, ZoomMemory};
use crate::settings::platform_combo;
use crate::state::AppState;

const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

fn key(combo: &str) -> String {
    platform_combo(combo)
}

fn settle(vcx: &mut VisualTestContext) {
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
}

fn open_in<'a>(
    cx: &'a mut TestAppContext,
    kind: ArtifactKind,
    content: &str,
    encoding: ArtifactContentEncoding,
) -> (Entity<ArtifactTab>, WebProbe, &'a mut VisualTestContext) {
    cx.update(|cx| {
        crate::composer::init(cx, Default::default());
        super::tab::bind_keys(cx);
    });
    let reply = reply_with(content_reply(kind, 2, content, encoding));
    let (actions, _calls) = scripted(reply);
    let probe = WebProbe::default();
    let factory = FakeWebHost::probed_factory(Default::default(), probe.clone());
    let state = cx.new(|_| AppState::new());
    let item = artifact(&id(1), "Plan", kind);
    state.update(cx, |state, _| {
        state.local_device_id = Some("dev".into());
        state.work_outline_actions_override = Some(actions);
        state.set_test_artifacts(snapshot(vec![item.clone()]));
    });
    let artifact_id = item.id.clone();
    let (tab, vcx) = cx.add_window_view(move |window, cx| {
        ArtifactTab::new(state, None, artifact_id, factory, window, cx)
    });
    settle(vcx);
    vcx.executor().advance_clock(Duration::from_millis(300));
    settle(vcx);
    settle(vcx);
    focus(&tab, vcx);
    (tab, probe, vcx)
}

fn open(
    cx: &mut TestAppContext,
    kind: ArtifactKind,
) -> (Entity<ArtifactTab>, WebProbe, &mut VisualTestContext) {
    let (content, encoding) = match kind {
        ArtifactKind::Image => (PNG, ArtifactContentEncoding::Base64),
        ArtifactKind::HtmlPage => (
            "<!doctype html><html><body><h1>Plan</h1></body></html>",
            ArtifactContentEncoding::Utf8,
        ),
        _ => ("# Plan\n\nBody text.\n", ArtifactContentEncoding::Utf8),
    };
    open_in(cx, kind, content, encoding)
}

fn focus(tab: &Entity<ArtifactTab>, vcx: &mut VisualTestContext) {
    vcx.update(|window, cx| {
        let handle = tab.read(cx).find_focus_handle().clone();
        window.focus(&handle, cx);
    });
}

fn level(tab: &Entity<ArtifactTab>, vcx: &mut VisualTestContext) -> f32 {
    tab.read_with(vcx, |tab, _| tab.zoom_level())
}

fn preview_zoom(tab: &Entity<ArtifactTab>, vcx: &mut VisualTestContext) -> f32 {
    tab.read_with(vcx, |tab, cx| tab.markdown_zoom_for_tests(cx))
}

fn press(vcx: &mut VisualTestContext, combo: &str) {
    vcx.simulate_keystrokes(&key(combo));
    settle(vcx);
}

fn body_center(vcx: &mut VisualTestContext, selector: &'static str) -> gpui::Point<gpui::Pixels> {
    vcx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} not drawn"))
        .center()
}

fn pinch(vcx: &mut VisualTestContext, at: gpui::Point<gpui::Pixels>, delta: f32) {
    vcx.simulate_event(PinchEvent {
        position: at,
        delta,
        modifiers: Modifiers::default(),
        phase: TouchPhase::Moved,
    });
    settle(vcx);
}

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

#[gpui::test]
fn keys_zoom_in_out_and_reset(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown);
    assert_eq!(level(&tab, vcx), 1.0);
    press(vcx, "mod-=");
    assert!(approx(level(&tab, vcx), 1.1));
    press(vcx, "mod-+");
    assert!(approx(level(&tab, vcx), 1.2));
    press(vcx, "mod-shift-=");
    assert!(approx(level(&tab, vcx), 1.3));
    assert!(approx(preview_zoom(&tab, vcx), 1.3), "the preview follows");
    press(vcx, "mod--");
    assert!(approx(level(&tab, vcx), 1.2));
    press(vcx, "mod-0");
    assert_eq!(level(&tab, vcx), 1.0);
    assert_eq!(preview_zoom(&tab, vcx), 1.0);
}

#[gpui::test]
fn zoom_stops_at_25_and_800_percent(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown);
    for _ in 0..100 {
        vcx.simulate_keystrokes(&key("mod-="));
    }
    assert_eq!(level(&tab, vcx), zoom::MAX_ZOOM);
    for _ in 0..100 {
        vcx.simulate_keystrokes(&key("mod--"));
    }
    assert_eq!(level(&tab, vcx), zoom::MIN_ZOOM);
}

#[gpui::test]
fn each_artifact_remembers_its_zoom_for_the_session(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown);
    press(vcx, "mod-=");
    press(vcx, "mod-=");
    let remembered = vcx.update(|_, cx| ZoomMemory::recall(cx, &id(1)));
    assert!(approx(remembered, 1.2));
    assert_eq!(vcx.update(|_, cx| ZoomMemory::recall(cx, &id(2))), 1.0);
    drop(tab);
    // A new tab on the same artifact starts there.
    let state = cx.new(|_| AppState::new());
    let factory = FakeWebHost::probed_factory(Default::default(), WebProbe::default());
    let (tab, vcx) = cx.add_window_view(move |window, cx| {
        ArtifactTab::new(state, None, id(1), factory, window, cx)
    });
    assert!(approx(level(&tab, vcx), 1.2));
    // Back at 100% the entry is dropped.
    tab.update(vcx, |tab, cx| tab.set_zoom(1.0, None, cx));
    assert_eq!(vcx.update(|_, cx| ZoomMemory::recall(cx, &id(1))), 1.0);
}

#[gpui::test]
fn the_toolbar_control_shows_the_level_and_clicks_zoom(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown);
    assert!(vcx.debug_bounds("artifact-zoom").is_some());
    assert_eq!(
        tab.read_with(vcx, |tab, _| zoom::label(tab.zoom_level())),
        "100%"
    );
    let plus = body_center(vcx, "artifact-zoom-in");
    vcx.simulate_click(plus, Modifiers::default());
    settle(vcx);
    assert!(approx(level(&tab, vcx), 1.1));
    let minus = body_center(vcx, "artifact-zoom-out");
    vcx.simulate_click(minus, Modifiers::default());
    vcx.simulate_click(minus, Modifiers::default());
    settle(vcx);
    assert!(approx(level(&tab, vcx), 0.9));
    let reset = body_center(vcx, "artifact-zoom-reset");
    vcx.simulate_click(reset, Modifiers::default());
    settle(vcx);
    assert_eq!(level(&tab, vcx), 1.0);
}

#[gpui::test]
fn the_control_and_keys_are_absent_where_nothing_zooms(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown);
    tab.update(vcx, |tab, cx| tab.toggle_source(cx));
    settle(vcx);
    assert!(vcx.debug_bounds("artifact-zoom").is_none());
    press(vcx, "mod-=");
    assert_eq!(level(&tab, vcx), 1.0);
}

#[gpui::test]
fn pinch_over_the_body_zooms_continuously(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown);
    let at = body_center(vcx, "artifact-markdown");
    pinch(vcx, at, 0.1);
    assert!(approx(level(&tab, vcx), 1.1));
    pinch(vcx, at, 0.1);
    assert!(approx(level(&tab, vcx), 1.21));
    pinch(vcx, at, -0.5);
    assert!(approx(level(&tab, vcx), 0.605));
    // Outside the body (the toolbar) does nothing.
    pinch(vcx, point(px(2.0), px(2.0)), 0.5);
    assert!(approx(level(&tab, vcx), 0.605));
}

#[gpui::test]
fn command_scroll_zooms_and_plain_scroll_does_not(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown);
    let at = body_center(vcx, "artifact-markdown");
    let scroll = |vcx: &mut VisualTestContext, modifiers: Modifiers, dy: f32| {
        vcx.simulate_event(ScrollWheelEvent {
            position: at,
            delta: ScrollDelta::Pixels(point(px(0.0), px(dy))),
            modifiers,
            touch_phase: TouchPhase::Moved,
        });
        settle(vcx);
    };
    scroll(vcx, Modifiers::default(), 30.0);
    assert_eq!(level(&tab, vcx), 1.0);
    scroll(vcx, Modifiers::command(), 20.0);
    assert!(level(&tab, vcx) > 1.1);
    scroll(vcx, Modifiers::control(), -20.0);
    assert!(approx(level(&tab, vcx), 1.0));
}

#[gpui::test]
fn images_zoom_with_keys_and_pinch(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Image);
    assert!(vcx.debug_bounds("artifact-image").is_some());
    press(vcx, "mod-=");
    assert!(approx(level(&tab, vcx), 1.1));
    let at = body_center(vcx, "artifact-image");
    pinch(vcx, at, 1.0);
    assert!(approx(level(&tab, vcx), 2.2));
    press(vcx, "mod-0");
    assert_eq!(level(&tab, vcx), 1.0);
}

#[gpui::test]
fn html_zoom_goes_to_the_page_and_follows_its_reports(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, probe, vcx) = open(cx, ArtifactKind::HtmlPage);
    press(vcx, "mod-=");
    assert!(approx(level(&tab, vcx), 1.1));
    assert!(
        probe
            .scripts
            .borrow()
            .contains(&command_script(&FrameCommand::Zoom(1.1))),
        "{:?}",
        probe.scripts.borrow()
    );
    // The page reports its first zoom (a fresh document); later pinch inside
    // the page is adopted.
    *probe.state.borrow_mut() = Some(r#"{"zoom":1100}"#.into());
    vcx.executor().advance_clock(Duration::from_millis(500));
    settle(vcx);
    assert!(approx(level(&tab, vcx), 1.1));
    *probe.state.borrow_mut() = Some(r#"{"zoom":2500}"#.into());
    vcx.executor().advance_clock(Duration::from_millis(500));
    settle(vcx);
    assert!(approx(level(&tab, vcx), 2.5));
    assert!(approx(
        vcx.update(|_, cx| ZoomMemory::recall(cx, &id(1))),
        2.5
    ));
    press(vcx, "mod-0");
    assert_eq!(level(&tab, vcx), 1.0);
    assert!(
        probe
            .scripts
            .borrow()
            .contains(&command_script(&FrameCommand::Zoom(1.0)))
    );
}

#[gpui::test]
fn a_reloaded_page_gets_the_remembered_zoom_once_it_reports(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    cx.update(|cx| ZoomMemory::remember(cx, &id(1), 1.5));
    let (tab, probe, vcx) = open(cx, ArtifactKind::HtmlPage);
    assert!(approx(level(&tab, vcx), 1.5));
    // Nothing is sent before the page has loaded and reported.
    assert!(
        !probe
            .scripts
            .borrow()
            .iter()
            .any(|script| script.contains("zeronZoom"))
    );
    *probe.state.borrow_mut() = Some(r#"{"zoom":1000}"#.into());
    vcx.executor().advance_clock(Duration::from_millis(500));
    settle(vcx);
    assert!(
        probe
            .scripts
            .borrow()
            .contains(&command_script(&FrameCommand::Zoom(1.5)))
    );
    assert!(
        approx(level(&tab, vcx), 1.5),
        "the stale 100% is not adopted"
    );
}

#[test]
fn the_page_script_zooms_with_the_same_anchor_formula() {
    let script = super::find::FRAME_SCRIPT;
    assert!(script.contains("(se.scrollTop + y) * k - y"));
    assert!(script.contains("gesturechange"));
    assert!(script.contains("e.ctrlKey || e.metaKey"));
    assert_eq!(
        command_script(&FrameCommand::Zoom(1.25)),
        "window.__zeronFrameCmd&&window.__zeronFrameCmd({\"zeronZoom\":1.25});"
    );
}

#[test]
fn drag_pan_follows_the_pointer_and_clamps() {
    let max = (200.0, 100.0);
    // Dragging right/down moves the content with the pointer: offset grows
    // toward zero, and cannot pass it.
    assert_eq!(
        zoom::drag_pan_offset((-150.0, -80.0), (10.0, 10.0), (30.0, 15.0), max),
        (-130.0, -75.0)
    );
    assert_eq!(
        zoom::drag_pan_offset((-50.0, -20.0), (0.0, 0.0), (500.0, 500.0), max),
        (0.0, 0.0)
    );
    // Dragging the other way stops at the far edge.
    assert_eq!(
        zoom::drag_pan_offset((-150.0, -80.0), (100.0, 100.0), (-900.0, -900.0), max),
        (-200.0, -100.0)
    );
    // An axis with no overflow stays put.
    assert_eq!(
        zoom::drag_pan_offset((0.0, 0.0), (0.0, 0.0), (40.0, 40.0), (0.0, 100.0)),
        (0.0, 0.0)
    );
}

#[gpui::test]
fn zoomed_images_pan_by_dragging_and_still_scroll(cx: &mut TestAppContext) {
    use base64::Engine as _;
    let _dir = init_ui(cx);
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(3000, 2000, image::Rgba([10, 120, 200, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
    let (tab, _probe, vcx) = open_in(
        cx,
        ArtifactKind::Image,
        &encoded,
        ArtifactContentEncoding::Base64,
    );
    let offset = |tab: &Entity<ArtifactTab>, vcx: &mut VisualTestContext| {
        tab.read_with(vcx, |tab, _| tab.image_offset_for_tests())
    };
    let at = body_center(vcx, "artifact-image");
    // At fit nothing pans.
    tab.update(vcx, |tab, cx| tab.image_pan_for_tests("down", at, cx));
    tab.update(vcx, |tab, cx| {
        tab.image_pan_for_tests("move", at + point(px(-50.0), px(-50.0)), cx)
    });
    assert_eq!(offset(&tab, vcx), (0.0, 0.0));
    tab.update(vcx, |tab, cx| tab.image_pan_for_tests("up", at, cx));

    press(vcx, "mod-=");
    for _ in 0..10 {
        press(vcx, "mod-=");
    }
    settle(vcx);
    settle(vcx);
    assert!(level(&tab, vcx) > 1.5);
    // Dragging down moves the content down with the pointer: the offset
    // moves toward zero by the drag distance.
    let start = offset(&tab, vcx);
    let max = tab.read_with(vcx, |tab, _| tab.image_max_offset_for_tests());
    assert!(max.1 > 100.0, "pannable: {max:?}");
    tab.update(vcx, |tab, cx| tab.image_pan_for_tests("down", at, cx));
    tab.update(vcx, |tab, cx| {
        tab.image_pan_for_tests("move", at + point(px(0.0), px(30.0)), cx)
    });
    let (_, y) = offset(&tab, vcx);
    assert!((y - (start.1 + 30.0)).abs() < 0.5, "panned: {start:?} -> {y}");
    // Further than the content goes: clamped to its far edge.
    tab.update(vcx, |tab, cx| {
        tab.image_pan_for_tests("move", at + point(px(-90_000.0), px(-90_000.0)), cx)
    });
    let (cx_, cy_) = offset(&tab, vcx);
    assert!(
        (cx_ + max.0).abs() < 0.5 && (cy_ + max.1).abs() < 0.5,
        "{cx_} {cy_} {max:?}"
    );
    tab.update(vcx, |tab, cx| tab.image_pan_for_tests("up", at, cx));
    // Moving with no drag does nothing; the wheel still scrolls.
    tab.update(vcx, |tab, cx| {
        tab.image_pan_for_tests("move", at + point(px(100.0), px(100.0)), cx)
    });
    assert_eq!(offset(&tab, vcx), (cx_, cy_));
    vcx.simulate_event(ScrollWheelEvent {
        position: at,
        delta: ScrollDelta::Pixels(point(px(0.0), px(40.0))),
        ..Default::default()
    });
    settle(vcx);
    assert!(offset(&tab, vcx).1 > cy_, "wheel scrolled back");
}
