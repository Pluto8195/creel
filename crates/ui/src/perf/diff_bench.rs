//! Diff pane over a 5000-line patch: patch parse, row flatten, first paint
//! and scrolling.

use super::*;
use crate::perf::{data, measure, measure_n};

#[test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_diff_parse_5000_lines() {
    let patch = data::unified_diff(5000);
    eprintln!(
        "BENCH diff | patch bytes={} lines={}",
        patch.len(),
        patch.lines().count()
    );
    measure("diff", "parse_patch", || parse_patch(&patch));
    measure_n("diff", "parse_patch", 5, |_| {
        std::hint::black_box(parse_patch(&patch));
    });
    measure("diff", "parse_diff_tree (parse+layout)", || {
        parse_diff_tree(&patch, &[])
    });
}

#[gpui::test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_diff_pane_5000_lines(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let patch = data::unified_diff(5000);
    let (changes, vcx) = cx.add_window_view(|_, cx| {
        let state = cx.new(|_| AppState::new());
        Changes::for_checkout(state, "/work/repo".into(), cx)
    });
    let draw = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
    };
    measure("diff", "load patch (sync + background parse + flatten)", || {
        changes.update(vcx, |c, cx| {
            c.scoped = Some(CheckoutDiff {
                checkout_id: "co".into(),
                device_id: "local".into(),
                cwd: "/work/repo".into(),
                patch: patch.clone(),
                files: Vec::new(),
                submodules: Vec::new(),
                additions: 3000,
                deletions: 1000,
                truncated: false,
                checksum: "bench".into(),
                updated_at: chrono::Utc::now(),
            });
            // `ensure_scoped` keeps a scoped diff only when it was fetched for
            // the current key; there is no engine here to fetch it.
            c.scoped_for = Some(format!("local|||/work/repo|{}||", c.scope.mode()) + "|");
            c.sync(cx);
        });
        vcx.run_until_parked();
    });
    let (rows, parsed, scoped) =
        changes.read_with(vcx, |c, _| (c.rows.len(), c.parsed.is_some(), c.scoped.is_some()));
    eprintln!("BENCH diff | rows={rows} parsed={parsed} scoped={scoped}");
    measure("diff", "first draw", || draw(vcx));
    measure_n("diff", "idle redraw", 30, |_| draw(vcx));
    let list = changes.read_with(vcx, |c, _| c.list.clone());
    measure_n("diff", "scroll 700px + draw", 100, |_| {
        list.scroll_by(px(700.0));
        draw(vcx);
    });
}
