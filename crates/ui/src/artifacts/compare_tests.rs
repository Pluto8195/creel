//! Revision compare view tests (headless gpui, fake web host and renderer).
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{Entity, TestAppContext, VisualTestContext};
use zeron_proto::{ArtifactContentEncoding, ArtifactKind, ArtifactViewport};

use super::compare::{CompareMode, Flavor, Side};
use super::pixel_diff::Region;
use super::render_check::{ArtifactRenderer, RenderFuture, RenderOutput};
use super::tab::ArtifactTab;
use super::tests::{
    Calls, Reply, Shown, artifact, content_calls, content_reply, id, init_ui, open_tab,
};

const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

/// A renderer answering synthetic PNGs: white, with a rectangle painted when
/// the document contains `CHANGED`.
#[derive(Default)]
struct PngRenderer {
    documents: RefCell<Vec<(String, ArtifactViewport)>>,
}

const CHANGED_RECT: (u32, u32, u32, u32) = (40, 60, 100, 30);

/// The change in a "TALL" render, far below the base 1400 px viewport.
const TALL_HEIGHT: u32 = 3000;
const TALL_CHANGED_RECT: (u32, u32, u32, u32) = (40, 2500, 100, 30);

fn synthetic_png(changed: bool, tall: bool) -> Vec<u8> {
    let height = if tall { TALL_HEIGHT } else { 200 };
    let mut image = image::RgbaImage::from_pixel(520, height, image::Rgba([255, 255, 255, 255]));
    if changed {
        let (x0, y0, w, h) = if tall {
            TALL_CHANGED_RECT
        } else {
            CHANGED_RECT
        };
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                image.put_pixel(x, y, image::Rgba([200, 0, 0, 255]));
            }
        }
    }
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

impl ArtifactRenderer for Rc<PngRenderer> {
    fn render(
        &self,
        document: String,
        viewport: ArtifactViewport,
        _: &mut gpui::AsyncApp,
    ) -> RenderFuture {
        let changed = document.contains("CHANGED");
        let tall = document.contains("TALL");
        let content_height = document.contains("VERYTALL").then_some(40_000);
        self.documents.borrow_mut().push((document, viewport));
        Box::pin(async move {
            Ok(RenderOutput {
                png: Some(synthetic_png(changed, tall)),
                report: content_height.map(|content_height| super::document::ProbeReport {
                    content_height: Some(content_height),
                    ..Default::default()
                }),
            })
        })
    }
}

fn fixture_text(kind: ArtifactKind, revision: u32) -> (String, ArtifactContentEncoding) {
    let utf8 = ArtifactContentEncoding::Utf8;
    match (kind, revision) {
        (ArtifactKind::Image, _) => (PNG_1X1.to_owned(), ArtifactContentEncoding::Base64),
        (ArtifactKind::Mermaid, 1) => ("graph TD; A-->B".to_owned(), utf8),
        (ArtifactKind::Mermaid, _) => ("graph TD; A-->B; B-->CHANGED".to_owned(), utf8),
        (ArtifactKind::HtmlPage, 1) => ("<html><body><p>old</p></body></html>".to_owned(), utf8),
        (ArtifactKind::HtmlPage, _) => {
            ("<html><body><p>CHANGED</p></body></html>".to_owned(), utf8)
        }
        (_, 1) => ("<p>old</p>".to_owned(), utf8),
        (_, _) => ("<p>CHANGED</p>".to_owned(), utf8),
    }
}

fn by_revision(kind: ArtifactKind) -> Reply {
    Arc::new(move |_, params| {
        let revision = params["revision"].as_u64().unwrap_or(2) as u32;
        let (text, encoding) = fixture_text(kind, revision);
        Ok(content_reply(kind, revision, &text, encoding))
    })
}

fn settle(vcx: &mut VisualTestContext) {
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
}

fn open(
    cx: &mut TestAppContext,
    kind: ArtifactKind,
    reply: Reply,
) -> (
    Entity<ArtifactTab>,
    Calls,
    Shown,
    Rc<PngRenderer>,
    &mut VisualTestContext,
) {
    let (tab, calls, shown, vcx) = open_tab(cx, artifact(&id(1), "Thing", kind), reply);
    let renderer = Rc::new(PngRenderer::default());
    tab.update(vcx, |tab, _| {
        tab.set_renderer_for_tests(Rc::new(renderer.clone()))
    });
    (tab, calls, shown, renderer, vcx)
}

fn revision_pair(tab: &Entity<ArtifactTab>, vcx: &mut VisualTestContext) -> (u32, u32) {
    tab.read_with(vcx, |tab, _| {
        let view = tab.compare_view().expect("compare view");
        (view.left, view.right)
    })
}

fn mode(tab: &Entity<ArtifactTab>, vcx: &mut VisualTestContext) -> CompareMode {
    tab.read_with(vcx, |tab, _| tab.compare_view().expect("compare view").mode)
}

#[gpui::test]
fn compare_picks_the_mode_and_flavor_per_kind(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let cases = [
        (ArtifactKind::HtmlFragment, Flavor::Visual),
        (ArtifactKind::HtmlPage, Flavor::Visual),
        (ArtifactKind::Mermaid, Flavor::Visual),
        (ArtifactKind::Image, Flavor::Visual),
        (ArtifactKind::Markdown, Flavor::Markdown),
    ];
    for (kind, flavor) in cases {
        let (tab, _, shown, _, vcx) = open(cx, kind, by_revision(kind));
        let before = shown.borrow().len();
        tab.update_in(vcx, |tab, window, cx| tab.compare_with(1, window, cx));
        settle(vcx);
        tab.read_with(vcx, |tab, _| {
            let view = tab.compare_view().expect("compare view");
            assert_eq!(view.flavor, flavor, "{kind:?}");
            assert_eq!(view.mode, CompareMode::SideBySide, "{kind:?} default");
            assert!(view.data().is_some(), "{kind:?} loaded");
            assert_eq!(tab.shows_web(), flavor == Flavor::Visual, "{kind:?}");
        });
        let documents = shown.borrow();
        if flavor == Flavor::Visual {
            // One host document with both revisions in sandboxed frames.
            assert_eq!(documents.len(), before + 1, "{kind:?}");
            let doc = &documents.last().unwrap().1;
            assert_eq!(doc.matches("<iframe").count(), 2, "{kind:?}");
            assert_eq!(doc.matches("sandbox=\"allow-scripts\"").count(), 2);
            assert!(doc.contains("zeronSyncTo"));
        } else {
            assert_eq!(documents.len(), before, "markdown renders natively");
            assert!(tab.read_with(vcx, |tab, _| tab.compare_view().unwrap().blocks().is_some()));
        }
    }
}

#[gpui::test]
fn the_last_mode_chosen_is_remembered_per_kind(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _, _, _, vcx) = open(
        cx,
        ArtifactKind::Mermaid,
        by_revision(ArtifactKind::Mermaid),
    );
    tab.update_in(vcx, |tab, window, cx| tab.compare_with(1, window, cx));
    settle(vcx);
    tab.update_in(vcx, |tab, window, cx| {
        tab.set_compare_mode(CompareMode::Source, window, cx)
    });
    tab.update_in(vcx, |tab, window, cx| tab.close_compare(window, cx));
    tab.update_in(vcx, |tab, window, cx| tab.compare_with(1, window, cx));
    settle(vcx);
    assert_eq!(
        mode(&tab, vcx),
        CompareMode::Source,
        "Mermaid remembers Source"
    );
    // Another kind keeps its own default.
    let (image, _, _, _, vcx) = open(cx, ArtifactKind::Image, by_revision(ArtifactKind::Image));
    image.update_in(vcx, |tab, window, cx| tab.compare_with(1, window, cx));
    settle(vcx);
    assert_eq!(mode(&image, vcx), CompareMode::SideBySide);
    // A mode the kind does not offer is ignored.
    let (md, _, _, _, vcx) = open(
        cx,
        ArtifactKind::Markdown,
        by_revision(ArtifactKind::Markdown),
    );
    md.update_in(vcx, |tab, window, cx| tab.compare_with(1, window, cx));
    settle(vcx);
    md.update_in(vcx, |tab, window, cx| {
        tab.set_compare_mode(CompareMode::Onion, window, cx)
    });
    assert_eq!(mode(&md, vcx), CompareMode::SideBySide);
}

#[gpui::test]
fn the_compare_mode_survives_a_restart(cx: &mut TestAppContext) {
    let dir = init_ui(cx);
    let settings_dir = dir.path().join("settings");
    cx.update(|cx| {
        super::compare::ComparePrefs::remember(ArtifactKind::Mermaid, CompareMode::Source, cx);
    });
    // A fresh process reads the file back.
    let loaded = crate::settings::UiSettings::load(&settings_dir);
    cx.update(|cx| {
        crate::settings::init(loaded, &settings_dir, cx);
        assert_eq!(
            super::compare::ComparePrefs::mode_for(ArtifactKind::Mermaid, cx),
            CompareMode::Source
        );
        // Another kind is untouched; an unoffered stored mode falls back.
        assert_eq!(
            super::compare::ComparePrefs::mode_for(ArtifactKind::Image, cx),
            CompareMode::SideBySide
        );
        super::compare::ComparePrefs::remember(ArtifactKind::Markdown, CompareMode::Onion, cx);
        assert_eq!(
            super::compare::ComparePrefs::mode_for(ArtifactKind::Markdown, cx),
            Flavor::Markdown.default_mode()
        );
    });
}

#[gpui::test]
fn revision_pickers_load_the_chosen_revisions(cx: &mut TestAppContext) {
    let kind = ArtifactKind::HtmlFragment;
    let _dir = init_ui(cx);
    let (tab, calls, _, _, vcx) = open(cx, kind, by_revision(kind));
    // The default pair: previous vs current (r1 vs r2).
    tab.update_in(vcx, |tab, window, cx| tab.compare_previous(window, cx));
    settle(vcx);
    assert_eq!(revision_pair(&tab, vcx), (1, 2));
    let revisions = |calls: &Calls| -> Vec<Option<u64>> {
        content_calls(calls)
            .iter()
            .map(|call| call["revision"].as_u64())
            .collect()
    };
    // Initial current-file read, then the left and right revisions.
    assert_eq!(revisions(&calls), [None, Some(1), Some(2)]);
    // Pick the right side's revision from its picker.
    let right = vcx
        .debug_bounds("artifact-compare-right")
        .expect("right picker");
    vcx.simulate_click(right.center(), gpui::Modifiers::default());
    settle(vcx);
    let row = vcx
        .debug_bounds("artifact-compare-right-r1")
        .expect("r1 row");
    vcx.simulate_click(row.center(), gpui::Modifiers::default());
    settle(vcx);
    assert_eq!(revision_pair(&tab, vcx), (1, 1));
    let all = revisions(&calls);
    assert_eq!(&all[all.len() - 2..], [Some(1), Some(1)]);
    // And the left side.
    tab.update_in(vcx, |tab, window, cx| {
        tab.set_compare_target(Side::Left, 2, window, cx)
    });
    settle(vcx);
    assert_eq!(revision_pair(&tab, vcx), (2, 1));
    let all = revisions(&calls);
    assert_eq!(&all[all.len() - 2..], [Some(2), Some(1)]);
}

#[gpui::test]
fn compare_with_picker_offers_the_previous_revision(cx: &mut TestAppContext) {
    let kind = ArtifactKind::Markdown;
    let _dir = init_ui(cx);
    let (tab, _, _, _, vcx) = open(cx, kind, by_revision(kind));
    let button = vcx
        .debug_bounds("artifact-compare")
        .expect("compare button");
    vcx.simulate_click(button.center(), gpui::Modifiers::default());
    settle(vcx);
    let previous = vcx
        .debug_bounds("artifact-compare-previous")
        .expect("previous row");
    vcx.simulate_click(previous.center(), gpui::Modifiers::default());
    settle(vcx);
    assert_eq!(revision_pair(&tab, vcx), (1, 2));
}

#[gpui::test]
fn highlight_changes_diffs_two_offscreen_renders(cx: &mut TestAppContext) {
    let kind = ArtifactKind::HtmlFragment;
    let _dir = init_ui(cx);
    let (tab, _, shown, renderer, vcx) = open(cx, kind, by_revision(kind));
    tab.update_in(vcx, |tab, window, cx| tab.compare_previous(window, cx));
    settle(vcx);
    assert!(
        renderer.documents.borrow().is_empty(),
        "nothing renders until asked"
    );
    tab.update_in(vcx, |tab, window, cx| tab.toggle_highlight(window, cx));
    settle(vcx);
    // Both revisions rendered at the same viewport.
    let documents = renderer.documents.borrow();
    assert_eq!(documents.len(), 2);
    assert_eq!(documents[0].1, documents[1].1);
    assert_eq!(
        documents[0].1.width,
        super::document::COMPARE_VIEWPORT_WIDTH
    );
    assert!(!documents[0].0.contains("CHANGED") && documents[1].0.contains("CHANGED"));
    drop(documents);
    tab.read_with(vcx, |tab, _| {
        let view = tab.compare_view().unwrap();
        assert_eq!(view.render_summary().as_deref(), Some("1 region changed"));
        assert_eq!(
            view.regions(),
            [Region {
                x: CHANGED_RECT.0,
                y: CHANGED_RECT.1,
                width: CHANGED_RECT.2,
                height: CHANGED_RECT.3
            }]
        );
    });
    // The overlay is in the right pane's frame, not the left's.
    let doc = shown.borrow().last().unwrap().1.clone();
    assert_eq!(doc.matches("data-zeron-changes").count(), 1);
    assert!(vcx.debug_bounds("artifact-compare-result").is_some());
}

#[test]
fn coverage_notice_math() {
    use super::document::{COMPARE_MAX_HEIGHT, compare_coverage, coverage_notice};
    assert_eq!(compare_coverage(3000), None);
    assert_eq!(compare_coverage(COMPARE_MAX_HEIGHT), None);
    assert_eq!(
        compare_coverage(COMPARE_MAX_HEIGHT + 1),
        Some((COMPARE_MAX_HEIGHT, COMPARE_MAX_HEIGHT + 1))
    );
    assert_eq!(
        coverage_notice((16_000, 42_000)),
        "Compared the first 16,000 px of 42,000"
    );
    assert_eq!(
        coverage_notice((16_000, 100_123)),
        "Compared the first 16,000 px of 100,123"
    );
}

#[gpui::test]
fn very_tall_renders_say_how_much_was_compared(cx: &mut TestAppContext) {
    let kind = ArtifactKind::HtmlFragment;
    let _dir = init_ui(cx);
    let reply: Reply = Arc::new(move |_, params| {
        let revision = params["revision"].as_u64().unwrap_or(2) as u32;
        let text = if revision == 1 {
            "<p>TALL VERYTALL old</p>"
        } else {
            "<p>TALL VERYTALL CHANGED</p>"
        };
        Ok(content_reply(
            kind,
            revision,
            text,
            ArtifactContentEncoding::Utf8,
        ))
    });
    let (tab, _, _, _, vcx) = open(cx, kind, reply);
    tab.update_in(vcx, |tab, window, cx| tab.compare_with(1, window, cx));
    settle(vcx);
    tab.update_in(vcx, |tab, window, cx| tab.toggle_highlight(window, cx));
    settle(vcx);
    tab.read_with(vcx, |tab, _| {
        let summary = tab.compare_view().unwrap().render_summary().unwrap();
        assert!(
            summary.ends_with("Compared the first 16,000 px of 40,000"),
            "{summary}"
        );
    });
}

#[gpui::test]
fn highlight_covers_changes_below_the_base_viewport(cx: &mut TestAppContext) {
    let kind = ArtifactKind::HtmlFragment;
    let _dir = init_ui(cx);
    let tall: Reply = Arc::new(move |_, params| {
        let revision = params["revision"].as_u64().unwrap_or(2) as u32;
        let text = if revision == 1 {
            "<p>TALL old</p>"
        } else {
            "<p>TALL CHANGED</p>"
        };
        Ok(content_reply(
            kind,
            revision,
            text,
            ArtifactContentEncoding::Utf8,
        ))
    });
    let (tab, _, shown, _, vcx) = open(cx, kind, tall);
    tab.update_in(vcx, |tab, window, cx| tab.compare_with(1, window, cx));
    settle(vcx);
    tab.update_in(vcx, |tab, window, cx| tab.toggle_highlight(window, cx));
    settle(vcx);
    let (x, y, w, h) = TALL_CHANGED_RECT;
    tab.read_with(vcx, |tab, _| {
        let view = tab.compare_view().unwrap();
        assert_eq!(
            view.regions(),
            [Region {
                x,
                y,
                width: w,
                height: h
            }]
        );
        assert!(y > super::document::COMPARE_VIEWPORT_HEIGHT);
        let rendered = view.rendered().unwrap();
        assert_eq!(rendered.diff.height, TALL_HEIGHT);
    });
    // The box is in the frame document at its real depth.
    let doc = shown.borrow().last().unwrap().1.clone();
    assert!(
        doc.contains(&format!("top:{}px", y - 16)),
        "overlay at depth"
    );
    // Onion skin stacks the whole render.
    tab.update_in(vcx, |tab, window, cx| {
        tab.set_compare_mode(CompareMode::Onion, window, cx)
    });
    settle(vcx);
    let onion = vcx.debug_bounds("artifact-compare-onion").expect("onion");
    assert!(onion.size.height > gpui::px(0.0));
}

#[test]
fn tall_render_region_math() {
    use super::compare::{onion_height, overlay_html};
    use super::document::{
        COMPARE_MAX_HEIGHT, COMPARE_MAX_PIXELS, COMPARE_VIEWPORT_HEIGHT, COMPARE_VIEWPORT_WIDTH,
        compare_render_plan,
    };
    // Short documents keep the base viewport at native scale.
    assert_eq!(
        compare_render_plan(300, 2.0),
        (COMPARE_VIEWPORT_HEIGHT, None)
    );
    // Tall ones grow to their content at Retina scale while it fits.
    assert_eq!(compare_render_plan(6000, 2.0), (6000, None));
    // Past the cap the height stops, and the snapshot downsizes to fit.
    let (height, width) = compare_render_plan(50_000, 2.0);
    assert_eq!(height, COMPARE_MAX_HEIGHT);
    assert!(width.is_none() || width.unwrap() >= COMPARE_VIEWPORT_WIDTH);
    for scale in [1.0, 2.0, 3.0] {
        let (height, width) = compare_render_plan(50_000, scale);
        let pixel_width = width.unwrap_or((COMPARE_VIEWPORT_WIDTH as f32 * scale) as u32);
        let pixel_height =
            (height as f32 * pixel_width as f32 / COMPARE_VIEWPORT_WIDTH as f32) as u64;
        assert!(
            u64::from(pixel_width) * pixel_height <= COMPARE_MAX_PIXELS + 100_000,
            "{scale}x fits: {pixel_width}x{pixel_height}"
        );
        assert!(pixel_width >= COMPARE_VIEWPORT_WIDTH, "never below 1x");
    }
    // A very high scale is downscaled before the cap would be exceeded.
    assert!(compare_render_plan(8000, 6.0).1.is_some());
    // Onion height follows the render, at 1x and at Retina.
    assert_eq!(onion_height(520, 3000), 3000.0);
    assert_eq!(onion_height(1040, 6000), 3000.0);
    // Regions deep in a Retina render land at CSS depth, inside the frame.
    let deep = Region {
        x: 80,
        y: 9000,
        width: 200,
        height: 60,
    };
    let html = overlay_html(&[deep], 2.0, (16, 16), 488);
    assert!(
        html.contains("top:4484px") && html.contains("height:30px"),
        "{html}"
    );
}

#[gpui::test]
fn identical_renders_read_as_no_visual_change(cx: &mut TestAppContext) {
    let kind = ArtifactKind::HtmlFragment;
    let _dir = init_ui(cx);
    let same: Reply = Arc::new(move |_, params| {
        let revision = params["revision"].as_u64().unwrap_or(2) as u32;
        Ok(content_reply(
            kind,
            revision,
            "<p>same</p>",
            ArtifactContentEncoding::Utf8,
        ))
    });
    let (tab, _, shown, _, vcx) = open(cx, kind, same);
    tab.update_in(vcx, |tab, window, cx| tab.compare_previous(window, cx));
    tab.update_in(vcx, |tab, window, cx| tab.toggle_highlight(window, cx));
    settle(vcx);
    tab.read_with(vcx, |tab, _| {
        let view = tab.compare_view().unwrap();
        assert_eq!(view.render_summary().as_deref(), Some("No visual change"));
        assert!(view.regions().is_empty());
    });
    assert_eq!(
        shown
            .borrow()
            .last()
            .unwrap()
            .1
            .matches("data-zeron-changes")
            .count(),
        0
    );
}

#[gpui::test]
fn without_a_renderer_highlight_says_so(cx: &mut TestAppContext) {
    let kind = ArtifactKind::HtmlFragment;
    let _dir = init_ui(cx);
    let (tab, _, _, vcx) = open_tab(cx, artifact(&id(1), "Thing", kind), by_revision(kind));
    tab.update_in(vcx, |tab, window, cx| tab.compare_previous(window, cx));
    tab.update_in(vcx, |tab, window, cx| tab.toggle_highlight(window, cx));
    settle(vcx);
    tab.read_with(vcx, |tab, _| {
        assert!(matches!(
            tab.compare_view().unwrap().renders,
            super::tab::compare_view::RenderState::Failed(_)
        ));
    });
}

#[gpui::test]
fn onion_skin_stacks_the_renders_with_an_opacity(cx: &mut TestAppContext) {
    let kind = ArtifactKind::Mermaid;
    let _dir = init_ui(cx);
    let (tab, _, _, renderer, vcx) = open(cx, kind, by_revision(kind));
    tab.update_in(vcx, |tab, window, cx| tab.compare_previous(window, cx));
    settle(vcx);
    tab.update_in(vcx, |tab, window, cx| {
        tab.set_compare_mode(CompareMode::Onion, window, cx)
    });
    settle(vcx);
    assert_eq!(renderer.documents.borrow().len(), 2);
    tab.read_with(vcx, |tab, _| {
        let view = tab.compare_view().unwrap();
        assert_eq!(view.mode, CompareMode::Onion);
        assert!(view.rendered().is_some());
        assert_eq!(view.onion.percent(), 50);
        assert!(!tab.shows_web(), "onion skin is native images");
    });
    assert!(vcx.debug_bounds("artifact-compare-onion").is_some());
    let step = vcx
        .debug_bounds("artifact-compare-onion-75")
        .expect("75% step");
    vcx.simulate_click(step.center(), gpui::Modifiers::default());
    settle(vcx);
    assert_eq!(
        tab.read_with(vcx, |tab, _| tab.compare_view().unwrap().onion.percent()),
        75
    );
    tab.update(vcx, |tab, cx| tab.set_onion_opacity(2.0, cx));
    assert_eq!(
        tab.read_with(vcx, |tab, _| tab.compare_view().unwrap().onion.percent()),
        100
    );
}

const MD_OLD: &str = "# Plan\n\n## 1. Goals\n\nShip it.\n\n## 2. Data model\n\nTables: parts.\n\n## 3. Rollout\n\nWeek one.\n";

#[gpui::test]
fn markdown_compare_highlights_blocks_and_summarises_sections(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let reply: Reply = Arc::new(|_, params| {
        let revision = params["revision"].as_u64().unwrap_or(2) as u32;
        let text = if revision == 1 {
            MD_OLD.to_owned()
        } else {
            MD_OLD
                .replace("Tables: parts.", "Tables: parts, lots.")
                .replace("Week one.", "Week one.\n\n## 4. Risks\n\nSlow backfill.")
                + "\n> [!question] Q-7 \u{b7} Who owns the backfill?\n"
        };
        Ok(content_reply(
            ArtifactKind::Markdown,
            revision,
            &text,
            ArtifactContentEncoding::Utf8,
        ))
    });
    let (tab, _, _, _, vcx) = open(cx, ArtifactKind::Markdown, reply);
    tab.update_in(vcx, |tab, window, cx| tab.compare_with(1, window, cx));
    settle(vcx);
    tab.read_with(vcx, |tab, _| {
        let blocks = tab.compare_view().unwrap().blocks().expect("block diff");
        assert_eq!(
            blocks.summary.text(),
            "changed: \u{a7}2 Data model; added: \u{a7}4 Risks, Q-7"
        );
    });
    let summary = vcx
        .debug_bounds("artifact-compare-summary")
        .expect("summary row");
    assert!(summary.size.width > gpui::px(0.0));
    assert!(vcx.debug_bounds("artifact-compare-markdown-left").is_some());
    assert!(
        vcx.debug_bounds("artifact-compare-markdown-right")
            .is_some()
    );
    // Source tab: the line diff.
    tab.update_in(vcx, |tab, window, cx| {
        tab.set_compare_mode(CompareMode::Source, window, cx)
    });
    settle(vcx);
    assert!(vcx.debug_bounds("artifact-compare-diff").is_some());
}

fn tall_markdown(revision: u32) -> String {
    (0..80)
        .map(|n| format!("## Section {n}\n\nParagraph {n} r{revision}.\n"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[gpui::test]
fn markdown_compare_panes_scroll_together(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let reply: Reply = Arc::new(|_, params| {
        let revision = params["revision"].as_u64().unwrap_or(2) as u32;
        Ok(content_reply(
            ArtifactKind::Markdown,
            revision,
            &tall_markdown(revision),
            ArtifactContentEncoding::Utf8,
        ))
    });
    let (tab, _, _, _, vcx) = open(cx, ArtifactKind::Markdown, reply);
    tab.update_in(vcx, |tab, window, cx| tab.compare_with(1, window, cx));
    settle(vcx);
    // The preview parses after a short debounce.
    vcx.executor()
        .advance_clock(std::time::Duration::from_millis(300));
    settle(vcx);
    settle(vcx);
    let lists = |tab: &Entity<ArtifactTab>, vcx: &mut VisualTestContext| {
        tab.read_with(vcx, |tab, cx| {
            let (l, r) = tab.compare_view().unwrap().markdown.clone().unwrap();
            (l.read(cx).list_state(), r.read(cx).list_state())
        })
    };
    let (left, right) = lists(&tab, vcx);
    let ratio = |list: &gpui::ListState| {
        let top = list.logical_scroll_top();
        super::compare::scroll_ratio(top.item_ix, 0.0, list.item_count())
    };
    assert!(f32::from(left.max_offset_for_scrollbar().y) > 0.0, "tall");
    let wheel = |vcx: &mut VisualTestContext, selector: &'static str, dy: f32| {
        let bounds = vcx.debug_bounds(selector).expect("pane");
        vcx.simulate_event(gpui::ScrollWheelEvent {
            position: bounds.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(dy))),
            ..Default::default()
        });
        settle(vcx);
    };
    wheel(vcx, "artifact-compare-markdown-left", -400.0);
    assert!(ratio(&left) > 0.02, "left scrolled: {}", ratio(&left));
    assert!(
        (ratio(&left) - ratio(&right)).abs() < 0.01,
        "right follows: {} vs {}",
        ratio(&left),
        ratio(&right)
    );
    // The right pane drives back without ping-ponging past its own move.
    let before = ratio(&right);
    wheel(vcx, "artifact-compare-markdown-right", -400.0);
    assert!(ratio(&right) > before);
    assert!(
        (ratio(&left) - ratio(&right)).abs() < 0.01,
        "{before} {} {}",
        ratio(&left),
        ratio(&right)
    );
    wheel(vcx, "artifact-compare-markdown-right", 5000.0);
    assert!(ratio(&left) < 0.01 && ratio(&right) < 0.01, "back at top");
}

#[gpui::test]
fn markdown_compare_follows_scrollbar_drag_and_programmatic_scroll(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let reply: Reply = Arc::new(|_, params| {
        let revision = params["revision"].as_u64().unwrap_or(2) as u32;
        Ok(content_reply(
            ArtifactKind::Markdown,
            revision,
            &tall_markdown(revision),
            ArtifactContentEncoding::Utf8,
        ))
    });
    let (tab, _, _, _, vcx) = open(cx, ArtifactKind::Markdown, reply);
    tab.update_in(vcx, |tab, window, cx| tab.compare_with(1, window, cx));
    settle(vcx);
    vcx.executor()
        .advance_clock(std::time::Duration::from_millis(300));
    settle(vcx);
    settle(vcx);
    let (left, right) = tab.read_with(vcx, |tab, cx| {
        let (l, r) = tab.compare_view().unwrap().markdown.clone().unwrap();
        (l.read(cx).list_state(), r.read(cx).list_state())
    });
    let ratio = |list: &gpui::ListState| {
        let top = list.logical_scroll_top();
        super::compare::scroll_ratio(top.item_ix, 0.0, list.item_count())
    };
    // Scrollbar drag on the left pane: no wheel event, no scroll handler.
    left.scrollbar_drag_started();
    left.set_offset_from_scrollbar(gpui::point(gpui::px(0.0), gpui::px(-600.0)));
    left.scrollbar_drag_ended();
    settle(vcx);
    settle(vcx);
    assert!(ratio(&left) > 0.02, "dragged: {}", ratio(&left));
    assert!(
        (ratio(&left) - ratio(&right)).abs() < 0.02,
        "right follows the drag: {} vs {}",
        ratio(&left),
        ratio(&right)
    );
    // Programmatic scroll (what PageDown and scroll-to-block use) on the right.
    right.scroll_to(gpui::ListOffset {
        item_ix: 10,
        offset_in_item: gpui::px(0.0),
    });
    settle(vcx);
    settle(vcx);
    assert!(
        (ratio(&left) - ratio(&right)).abs() < 0.02,
        "left follows scroll_to: {} vs {}",
        ratio(&left),
        ratio(&right)
    );
    assert!(ratio(&left) < 0.2, "moved back up: {}", ratio(&left));
}

#[test]
fn scroll_ratio_math_maps_item_positions_between_lists() {
    use super::compare::{scroll_ratio, scroll_target};
    assert_eq!(scroll_ratio(5, 0.5, 10), 0.55);
    assert_eq!(scroll_ratio(0, 0.0, 0), 0.0);
    assert_eq!(scroll_ratio(12, 0.0, 10), 1.0);
    assert_eq!(scroll_target(0.5, 20), (10, 0.0));
    assert_eq!(scroll_target(1.0, 20), (19, 1.0));
    assert_eq!(scroll_target(0.0, 0), (0, 0.0));
    // Half way through a 10-block list is half way through a 40-block one.
    let (ix, frac) = scroll_target(scroll_ratio(5, 0.0, 10), 40);
    assert_eq!((ix, frac), (20, 0.0));
}

const OPTIONS: &str = r#"{"title":"Layouts","options":[
  {"id":"a","label":"Grid","path":"a.fragment.html"},
  {"id":"b","label":"List","path":"b.fragment.html"},
  {"id":"c","label":"Cards","path":"c.fragment.html"}]}"#;

#[gpui::test]
fn options_compare_picks_which_options_to_compare(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let reply: Reply = Arc::new(|_, params| {
        Ok(match params.get("companion").and_then(|c| c.as_str()) {
            Some(path) => content_reply(
                ArtifactKind::HtmlFragment,
                0,
                &format!("<p>{path}</p>"),
                ArtifactContentEncoding::Utf8,
            ),
            None => content_reply(
                ArtifactKind::Options,
                2,
                OPTIONS,
                ArtifactContentEncoding::Utf8,
            ),
        })
    });
    let (tab, calls, shown, _, vcx) = open(cx, ArtifactKind::Options, reply);
    let compare = vcx
        .debug_bounds("artifact-compare")
        .expect("compare options");
    vcx.simulate_click(compare.center(), gpui::Modifiers::default());
    settle(vcx);
    let companions = |calls: &Calls| -> Vec<String> {
        content_calls(calls)
            .iter()
            .filter_map(|call| call["companion"].as_str().map(str::to_owned))
            .collect()
    };
    tab.read_with(vcx, |tab, _| {
        let view = tab.compare_view().unwrap();
        assert_eq!(view.flavor, Flavor::Options);
        assert_eq!(view.option_pair, (0, 1));
        assert_eq!(view.mode, CompareMode::SideBySide);
        assert!(tab.shows_web());
    });
    assert_eq!(companions(&calls), ["a.fragment.html", "b.fragment.html"]);
    let doc = shown.borrow().last().unwrap().1.clone();
    assert_eq!(doc.matches("<iframe").count(), 2);
    // Pick the right option from its picker.
    let right = vcx
        .debug_bounds("artifact-compare-right")
        .expect("right picker");
    vcx.simulate_click(right.center(), gpui::Modifiers::default());
    settle(vcx);
    let row = vcx
        .debug_bounds("artifact-compare-right-option-c")
        .expect("option c row");
    vcx.simulate_click(row.center(), gpui::Modifiers::default());
    settle(vcx);
    tab.read_with(vcx, |tab, _| {
        assert_eq!(tab.compare_view().unwrap().option_pair, (0, 2));
        let data = tab.compare_view().unwrap().data().unwrap();
        assert_eq!(data.left.label, "Grid");
        assert_eq!(data.right.label, "Cards");
        assert_eq!(data.left.text.as_deref(), Some("<p>a.fragment.html</p>"));
        assert_eq!(data.right.text.as_deref(), Some("<p>c.fragment.html</p>"));
    });
    let all = companions(&calls);
    assert_eq!(
        &all[all.len() - 2..],
        ["a.fragment.html", "c.fragment.html"]
    );
    // Out-of-range picks are ignored.
    tab.update_in(vcx, |tab, window, cx| {
        tab.set_compare_target(Side::Left, 9, window, cx)
    });
    assert_eq!(
        tab.read_with(vcx, |tab, _| tab.compare_view().unwrap().option_pair),
        (0, 2)
    );
}

#[gpui::test]
fn options_compare_one_option_across_revisions(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let reply: Reply = Arc::new(|_, params| {
        let revision = params.get("revision").and_then(|r| r.as_u64()).unwrap_or(2);
        Ok(match params.get("companion").and_then(|c| c.as_str()) {
            Some(path) => content_reply(
                ArtifactKind::HtmlFragment,
                revision as u32,
                &format!("<p>{path}@r{revision}</p>"),
                ArtifactContentEncoding::Utf8,
            ),
            None => content_reply(
                ArtifactKind::Options,
                2,
                OPTIONS,
                ArtifactContentEncoding::Utf8,
            ),
        })
    });
    let (tab, calls, shown, _, vcx) = open(cx, ArtifactKind::Options, reply);
    let compare = vcx
        .debug_bounds("artifact-compare")
        .expect("compare options");
    vcx.simulate_click(compare.center(), gpui::Modifiers::default());
    settle(vcx);
    let across = vcx
        .debug_bounds("artifact-compare-options-across")
        .expect("across toggle");
    vcx.simulate_click(across.center(), gpui::Modifiers::default());
    settle(vcx);
    tab.read_with(vcx, |tab, _| {
        let view = tab.compare_view().unwrap();
        assert_eq!(view.across, Some(0));
        assert_eq!((view.left, view.right), (1, 2));
        let data = view.data().unwrap();
        assert_eq!(data.left.text.as_deref(), Some("<p>a.fragment.html@r1</p>"));
        assert_eq!(
            data.right.text.as_deref(),
            Some("<p>a.fragment.html@r2</p>")
        );
        assert!(tab.shows_web());
    });
    let last_two: Vec<(Option<u64>, Option<String>)> = {
        let all = content_calls(&calls);
        all[all.len() - 2..]
            .iter()
            .map(|call| {
                (
                    call["revision"].as_u64(),
                    call["companion"].as_str().map(str::to_owned),
                )
            })
            .collect()
    };
    assert_eq!(
        last_two,
        [
            (Some(1), Some("a.fragment.html".to_owned())),
            (Some(2), Some("a.fragment.html".to_owned()))
        ]
    );
    assert_eq!(
        shown.borrow().last().unwrap().1.matches("<iframe").count(),
        2
    );
    // Cycle to the next option.
    let cycle = vcx
        .debug_bounds("artifact-compare-option-cycle")
        .expect("option cycler");
    vcx.simulate_click(cycle.center(), gpui::Modifiers::default());
    settle(vcx);
    tab.read_with(vcx, |tab, _| {
        let view = tab.compare_view().unwrap();
        assert_eq!(view.across, Some(1));
        let data = view.data().unwrap();
        assert_eq!(data.left.text.as_deref(), Some("<p>b.fragment.html@r1</p>"));
    });
    // Back to two options.
    let two = vcx
        .debug_bounds("artifact-compare-options-two")
        .expect("two toggle");
    vcx.simulate_click(two.center(), gpui::Modifiers::default());
    settle(vcx);
    tab.read_with(vcx, |tab, _| {
        let view = tab.compare_view().unwrap();
        assert_eq!(view.across, None);
        assert_eq!(view.data().unwrap().left.label, "Grid");
    });
}

#[gpui::test]
fn image_compare_has_no_text_source(cx: &mut TestAppContext) {
    let kind = ArtifactKind::Image;
    let _dir = init_ui(cx);
    let (tab, _, _, _, vcx) = open(cx, kind, by_revision(kind));
    tab.update_in(vcx, |tab, window, cx| tab.compare_previous(window, cx));
    tab.update_in(vcx, |tab, window, cx| {
        tab.set_compare_mode(CompareMode::Source, window, cx)
    });
    settle(vcx);
    assert!(tab.read_with(vcx, |tab, _| {
        tab.compare_view().unwrap().data().unwrap().lines.is_none()
    }));
    assert!(vcx.debug_bounds("artifact-compare-message").is_some());
}
