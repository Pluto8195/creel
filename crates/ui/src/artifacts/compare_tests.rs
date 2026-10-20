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

fn synthetic_png(changed: bool) -> Vec<u8> {
    let mut image = image::RgbaImage::from_pixel(520, 200, image::Rgba([255, 255, 255, 255]));
    if changed {
        let (x0, y0, w, h) = CHANGED_RECT;
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
        self.documents.borrow_mut().push((document, viewport));
        Box::pin(async move {
            Ok(RenderOutput {
                png: Some(synthetic_png(changed)),
                report: None,
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
