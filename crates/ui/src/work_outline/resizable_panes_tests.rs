// The Overview's resizable Work Item Details panel: drag, clamps, reset,
// persistence, window resize and row layout. Included from `overview.rs`'s
// test module so it shares its Overview fixtures.
use super::*;

use crate::overview::details_width::{
    DETAILS_DEFAULT_W, DETAILS_MIN_W, details_max_width, details_width,
};

const WINDOW_W: f32 = 1400.0;

fn resize_window(vcx: &mut gpui::VisualTestContext, width: f32) {
    vcx.simulate_resize(gpui::size(px(width), px(900.0)));
    draw(vcx);
    draw(vcx);
}

fn panel_width(vcx: &mut gpui::VisualTestContext) -> f32 {
    f32::from(vcx.debug_bounds("work-item-details").unwrap().size.width)
}

fn drag_handle(vcx: &mut gpui::VisualTestContext, dx: f32) {
    let at = vcx.debug_bounds("details-panel-resize").unwrap().center();
    let left = Some(gpui::MouseButton::Left);
    let mods = gpui::Modifiers::default();
    vcx.simulate_mouse_down(at, gpui::MouseButton::Left, mods);
    // The first move starts the drag; later ones carry it.
    vcx.simulate_mouse_move(at + gpui::point(px(dx.signum() * 4.0), px(0.0)), left, mods);
    vcx.simulate_mouse_move(at + gpui::point(px(dx), px(0.0)), left, mods);
    vcx.simulate_mouse_up(at + gpui::point(px(dx), px(0.0)), gpui::MouseButton::Left, mods);
    draw(vcx);
    draw(vcx);
}

fn double_click_handle(vcx: &mut gpui::VisualTestContext) {
    let at = vcx.debug_bounds("details-panel-resize").unwrap().center();
    for click_count in [1, 2] {
        vcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: at,
            click_count,
            ..Default::default()
        });
        vcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: at,
            click_count,
            ..Default::default()
        });
    }
    draw(vcx);
    draw(vcx);
}

fn details_overview<'a>(
    cx: &'a mut gpui::TestAppContext,
    dir: &std::path::Path,
) -> (Entity<Overview>, &'a mut gpui::VisualTestContext) {
    let (overview, _calls, vcx) = instruction_overview(
        cx,
        dir,
        crate::work_outline::store::fixture_snapshot("s1"),
        reply_ok,
    );
    resize_window(vcx, WINDOW_W);
    open_details(&overview, vcx, API);
    (overview, vcx)
}

#[test]
fn limits_are_320_to_sixty_percent_and_keep_the_body_minimum() {
    assert_eq!(details_max_width(Some(1400.0)), 840.0);
    // 60% would leave the body under its minimum: the body wins.
    assert_eq!(
        details_max_width(Some(700.0)),
        (700.0 - super::super::linear_host::OVERVIEW_BODY_MIN_W).max(DETAILS_MIN_W)
    );
    // Tiny windows still get the minimum.
    assert_eq!(details_max_width(Some(300.0)), DETAILS_MIN_W);
    assert_eq!(details_width(10.0, Some(1400.0)), DETAILS_MIN_W);
    assert_eq!(details_width(5000.0, Some(1400.0)), 840.0);
    assert_eq!(details_width(f32::NAN, Some(1400.0)), DETAILS_DEFAULT_W);
}

#[gpui::test]
fn dragging_the_handle_resizes_and_clamps(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, vcx) = details_overview(cx, dir.path());
    assert_eq!(panel_width(vcx), DETAILS_DEFAULT_W);
    // The panel grows leftwards.
    drag_handle(vcx, -60.0);
    assert_eq!(panel_width(vcx), DETAILS_DEFAULT_W + 60.0);
    drag_handle(vcx, 30.0);
    assert_eq!(panel_width(vcx), DETAILS_DEFAULT_W + 30.0);
    // Past either limit it clamps.
    drag_handle(vcx, -2000.0);
    assert_eq!(panel_width(vcx), details_max_width(Some(WINDOW_W)));
    drag_handle(vcx, 2000.0);
    assert_eq!(panel_width(vcx), DETAILS_MIN_W);
}

#[gpui::test]
fn double_clicking_the_handle_resets_the_width(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, vcx) = details_overview(cx, dir.path());
    drag_handle(vcx, -150.0);
    assert_ne!(panel_width(vcx), DETAILS_DEFAULT_W);
    double_click_handle(vcx);
    assert_eq!(panel_width(vcx), DETAILS_DEFAULT_W);
    overview.read_with(vcx, |o, _| {
        assert_eq!(o.details_width_state().width, DETAILS_DEFAULT_W)
    });
}

#[gpui::test]
fn the_width_persists_across_a_fresh_overview(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    {
        let (_overview, vcx) = details_overview(cx, dir.path());
        drag_handle(vcx, -100.0);
        assert_eq!(panel_width(vcx), DETAILS_DEFAULT_W + 100.0);
    }
    let (_fresh, vcx) = details_overview(cx, dir.path());
    assert_eq!(panel_width(vcx), DETAILS_DEFAULT_W + 100.0);
    // Reset persists too.
    double_click_handle(vcx);
    let (_again, vcx) = details_overview(cx, dir.path());
    assert_eq!(panel_width(vcx), DETAILS_DEFAULT_W);
}

#[gpui::test]
fn a_narrow_window_clamps_the_shown_width_not_the_preference(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, vcx) = details_overview(cx, dir.path());
    drag_handle(vcx, -400.0);
    let preferred = DETAILS_DEFAULT_W + 400.0;
    assert_eq!(panel_width(vcx), preferred);
    resize_window(vcx, 900.0);
    assert_eq!(panel_width(vcx), details_max_width(Some(900.0)));
    assert!(panel_width(vcx) < preferred);
    overview.read_with(vcx, |o, _| {
        assert_eq!(o.details_width_state().width, preferred)
    });
    resize_window(vcx, WINDOW_W);
    assert_eq!(panel_width(vcx), preferred);
}

/// Rows stay inside the body column, at the shared height, at both limits.
#[gpui::test]
fn outline_rows_lay_out_at_min_and_max_widths(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (_overview, vcx) = details_overview(cx, dir.path());
    let keys = [API, API_AUTH, UI, HOUSEKEEPING].map(|id| row_key::item("s1", id));
    let heights = |vcx: &mut gpui::VisualTestContext| {
        keys.iter()
            .filter_map(|key| row_bounds(vcx, format!("outline-row-{key}")))
            .map(|b| f32::from(b.size.height))
            .collect::<Vec<_>>()
    };
    let baseline = heights(vcx);
    assert!(!baseline.is_empty(), "rows render");
    for dx in [2000.0, -2000.0] {
        drag_handle(vcx, dx);
        let panel_left = vcx.debug_bounds("work-item-details").unwrap().left();
        assert_eq!(heights(vcx), baseline, "row heights at {dx}");
        for key in &keys {
            if let Some(row) = row_bounds(vcx, format!("outline-row-{key}")) {
                assert!(row.right() <= panel_left + px(0.5), "{key} stays left of the panel");
                assert!(row.size.width > px(0.0));
            }
        }
    }
}
