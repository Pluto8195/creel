//! Markdown preview of a ~300 KB plan: parse, async set_source, first paint
//! and scrolling.

use super::*;
use crate::perf::{data, measure, measure_n};
use gpui::TestAppContext;
use std::time::Duration;

#[test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_markdown_parse_300kb() {
    let source = data::markdown_plan(300 * 1024);
    eprintln!("BENCH markdown | source bytes={}", source.len());
    measure("markdown", "parse_full 300KB", || {
        crate::markdown::parser::parse_full(&source)
    });
    measure_n("markdown", "parse_full 300KB", 5, |_| {
        std::hint::black_box(crate::markdown::parser::parse_full(&source));
    });
}

#[gpui::test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_markdown_preview_300kb(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let source = data::markdown_plan(300 * 1024);
    let (view, vcx) =
        cx.add_window_view(|_, cx| MarkdownPreview::new("plan.md".into(), Rc::new(|_, _| {}), cx));
    let draw = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
    };
    measure(
        "markdown",
        "set_source + settle (parse, background)",
        || {
            view.update(vcx, |v, cx| v.set_source(source, false, cx));
            vcx.run_until_parked();
            vcx.executor().advance_clock(Duration::from_secs(1));
            vcx.run_until_parked();
        },
    );
    let blocks = view.read_with(vcx, |v, _| v.tree.blocks.len());
    eprintln!("BENCH markdown | blocks={blocks}");
    measure("markdown", "first draw", || draw(vcx));
    measure_n("markdown", "idle redraw", 30, |_| draw(vcx));
    let list = view.read_with(vcx, |v, _| v.list_state());
    measure_n("markdown", "scroll 700px + draw", 100, |_| {
        list.scroll_by(px(700.0));
        draw(vcx);
    });
}
