//! Find (⌘F) and the section index in the Artifact tab: headless gpui tests
//! with a fake web host (no WKWebView) and the real Markdown preview.
use std::time::Duration;

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use zeron_proto::{ArtifactContentEncoding, ArtifactKind};

use super::find::{FrameCommand, command_script};
use super::tab::{ArtifactTab, FakeWebHost, Presenter, WebProbe};
use super::tests::{Shown, artifact, content_reply, id, init_ui, reply_with, scripted, snapshot};
use crate::settings::platform_combo;
use crate::state::AppState;

fn key(combo: &str) -> String {
    platform_combo(combo)
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    kind: ArtifactKind,
    content: &str,
) -> (Entity<ArtifactTab>, WebProbe, &'a mut VisualTestContext) {
    cx.update(|cx| {
        crate::composer::init(cx, Default::default());
        super::tab::bind_keys(cx);
    });
    let reply = reply_with(content_reply(
        kind,
        2,
        content,
        ArtifactContentEncoding::Utf8,
    ));
    let (actions, _calls) = scripted(reply);
    let shown: Shown = Default::default();
    let probe = WebProbe::default();
    let factory = FakeWebHost::probed_factory(shown, probe.clone());
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
    // The Markdown preview parses after a short debounce.
    vcx.executor().advance_clock(Duration::from_millis(300));
    settle(vcx);
    settle(vcx);
    (tab, probe, vcx)
}

fn settle(vcx: &mut VisualTestContext) {
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
}

fn focus_tab(tab: &Entity<ArtifactTab>, vcx: &mut VisualTestContext) {
    vcx.update(|window, cx| {
        let handle = tab.read(cx).find_focus_handle().clone();
        window.focus(&handle, cx);
    });
}

fn find_open(tab: &Entity<ArtifactTab>, vcx: &mut VisualTestContext) -> bool {
    tab.read_with(vcx, |tab, _| tab.find_state().open)
}

fn label(tab: &Entity<ArtifactTab>, vcx: &mut VisualTestContext) -> String {
    tab.read_with(vcx, |tab, _| tab.find_state().label())
}

fn type_query(vcx: &mut VisualTestContext, text: &str) {
    let keys: Vec<String> = text.chars().map(|c| c.to_string()).collect();
    vcx.simulate_keystrokes(&keys.join(" "));
    settle(vcx);
}

const PLAN_MD: &str = "# Plan\n\nThe plan has a plan.\n\n## Steps\n\nStep one of the PLAN.\n\n\
## Risks\n\nNone.\n\n### Detail\n\nMore detail about risks.\n";

const PLAN_HTML: &str = "<!doctype html><html><head></head><body><h1>Plan</h1><p>plan</p>\
<h2>Steps</h2><p>one</p><h2>Risks</h2><h3>Detail</h3></body></html>";

fn scripts(probe: &WebProbe) -> Vec<String> {
    probe.scripts.borrow().clone()
}

// --------------------------------------------------------------------------
// Key routing
// --------------------------------------------------------------------------

#[gpui::test]
fn cmd_f_in_an_artifact_opens_find_and_focuses_the_field(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown, PLAN_MD);
    focus_tab(&tab, vcx);
    assert!(!find_open(&tab, vcx));
    vcx.simulate_keystrokes(&key("mod-f"));
    settle(vcx);
    assert!(find_open(&tab, vcx));
    assert!(
        vcx.debug_bounds("artifact-find").is_some(),
        "find bar draws"
    );
    // Typing goes to the field.
    type_query(vcx, "plan");
    assert_eq!(
        tab.read_with(vcx, |tab, _| tab.find_state().query.clone()),
        "plan"
    );
}

#[gpui::test]
fn cmd_f_works_from_any_focus_inside_the_tab_but_not_outside_it(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown, PLAN_MD);
    // Focus on the Markdown preview inside the tab.
    vcx.update(|window, cx| {
        let preview = tab.read(cx).markdown_for_tests().expect("preview");
        let handle = preview.read(cx).focus.clone();
        window.focus(&handle, cx);
    });
    vcx.simulate_keystrokes(&key("mod-f"));
    settle(vcx);
    assert!(find_open(&tab, vcx), "⌘F from the preview");

    // With nothing in the tab focused the key is not the tab's.
    let (tab2, _probe2, vcx2) = open(cx, ArtifactKind::Markdown, PLAN_MD);
    vcx2.update(|window, _| window.blur());
    vcx2.simulate_keystrokes(&key("mod-f"));
    settle(vcx2);
    assert!(!find_open(&tab2, vcx2));
}

#[gpui::test]
fn cmd_f_falls_through_when_the_view_has_nothing_to_search(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(
        cx,
        ArtifactKind::Options,
        r#"{"title":"L","options":[{"id":"a","label":"Grid","path":"a.fragment.html"}]}"#,
    );
    focus_tab(&tab, vcx);
    vcx.simulate_keystrokes(&key("mod-f"));
    settle(vcx);
    assert_eq!(
        tab.read_with(vcx, |tab, _| tab.presenter()),
        Presenter::Options
    );
    assert!(!find_open(&tab, vcx));
}

// --------------------------------------------------------------------------
// Markdown find
// --------------------------------------------------------------------------

#[gpui::test]
fn markdown_find_counts_highlights_and_steps_with_wraparound(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown, PLAN_MD);
    focus_tab(&tab, vcx);
    vcx.simulate_keystrokes(&key("mod-f"));
    type_query(vcx, "plan");
    // "Plan" (h1), "plan" x2, "PLAN" → 4, case-insensitive.
    assert_eq!(label(&tab, vcx), "1 of 4");
    let preview_count = tab.read_with(vcx, |tab, cx| {
        tab.markdown_for_tests().unwrap().read(cx).find_count()
    });
    assert_eq!(preview_count, 4);

    // Enter / ⌘G go forward, ⇧Enter / ⇧⌘G back, both wrap.
    vcx.simulate_keystrokes("enter");
    assert_eq!(label(&tab, vcx), "2 of 4");
    vcx.simulate_keystrokes(&key("mod-g"));
    assert_eq!(label(&tab, vcx), "3 of 4");
    vcx.simulate_keystrokes("enter");
    vcx.simulate_keystrokes("enter");
    assert_eq!(label(&tab, vcx), "1 of 4", "wraps past the last match");
    vcx.simulate_keystrokes("shift-enter");
    assert_eq!(label(&tab, vcx), "4 of 4", "wraps before the first match");
    vcx.simulate_keystrokes(&key("mod-shift-g"));
    assert_eq!(label(&tab, vcx), "3 of 4");
}

#[gpui::test]
fn markdown_find_says_no_matches_and_an_empty_query_clears(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown, PLAN_MD);
    focus_tab(&tab, vcx);
    vcx.simulate_keystrokes(&key("mod-f"));
    type_query(vcx, "zzz");
    assert_eq!(label(&tab, vcx), "No matches");
    // Stepping with no matches does nothing.
    vcx.simulate_keystrokes("enter");
    assert_eq!(label(&tab, vcx), "No matches");
    vcx.simulate_keystrokes("backspace backspace backspace");
    settle(vcx);
    assert_eq!(label(&tab, vcx), "");
    assert_eq!(
        tab.read_with(vcx, |tab, cx| tab
            .markdown_for_tests()
            .unwrap()
            .read(cx)
            .find_count()),
        0
    );
}

#[gpui::test]
fn escape_closes_the_bar_and_clears_the_matches(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown, PLAN_MD);
    focus_tab(&tab, vcx);
    vcx.simulate_keystrokes(&key("mod-f"));
    type_query(vcx, "plan");
    assert!(find_open(&tab, vcx));
    vcx.simulate_keystrokes("escape");
    settle(vcx);
    assert!(!find_open(&tab, vcx));
    assert!(vcx.debug_bounds("artifact-find").is_none(), "bar is gone");
    assert_eq!(
        tab.read_with(vcx, |tab, cx| tab
            .markdown_for_tests()
            .unwrap()
            .read(cx)
            .find_count()),
        0
    );
    // ⌘G with the bar closed is not the tab's key.
    vcx.simulate_keystrokes(&key("mod-g"));
    assert!(!find_open(&tab, vcx));
}

// --------------------------------------------------------------------------
// HTML find (through the page's scripts)
// --------------------------------------------------------------------------

#[gpui::test]
fn html_find_sends_the_query_and_follows_the_pages_count(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, probe, vcx) = open(cx, ArtifactKind::HtmlPage, PLAN_HTML);
    focus_tab(&tab, vcx);
    vcx.simulate_keystrokes(&key("mod-f"));
    settle(vcx);
    assert!(find_open(&tab, vcx));
    assert_eq!(probe.chrome_focused.get(), 1, "page gave up the keyboard");
    assert!(
        probe.escape_forwarded.get(),
        "Esc reaches the bar from the page"
    );

    type_query(vcx, "plan");
    assert!(
        scripts(&probe).contains(&command_script(&FrameCommand::Find("plan"))),
        "{:?}",
        scripts(&probe)
    );
    // The page answers.
    *probe.state.borrow_mut() = Some(r#"{"q":"plan","count":3}"#.into());
    vcx.executor().advance_clock(Duration::from_millis(500));
    settle(vcx);
    assert_eq!(label(&tab, vcx), "1 of 3");
    assert!(scripts(&probe).contains(&command_script(&FrameCommand::Goto(0))));

    // Next / previous go to the page, wrapping on the host side.
    vcx.simulate_keystrokes("enter");
    assert!(scripts(&probe).contains(&command_script(&FrameCommand::Goto(1))));
    vcx.simulate_keystrokes(&key("mod-g"));
    vcx.simulate_keystrokes(&key("mod-g"));
    assert_eq!(label(&tab, vcx), "1 of 3");
    vcx.simulate_keystrokes(&key("mod-shift-g"));
    assert_eq!(label(&tab, vcx), "3 of 3");
    assert_eq!(
        scripts(&probe).last().unwrap(),
        &command_script(&FrameCommand::Goto(2))
    );
}

#[gpui::test]
fn html_find_ignores_a_stale_count_and_close_clears_the_page(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, probe, vcx) = open(cx, ArtifactKind::HtmlPage, PLAN_HTML);
    focus_tab(&tab, vcx);
    vcx.simulate_keystrokes(&key("mod-f"));
    type_query(vcx, "pl");
    // The page still reports the answer to an older query.
    *probe.state.borrow_mut() = Some(r#"{"q":"p","count":9}"#.into());
    vcx.executor().advance_clock(Duration::from_millis(500));
    settle(vcx);
    assert_eq!(
        label(&tab, vcx),
        "No matches",
        "9 belongs to \"p\", not \"pl\""
    );

    vcx.simulate_keystrokes("escape");
    settle(vcx);
    assert!(!find_open(&tab, vcx));
    assert_eq!(
        scripts(&probe).last().unwrap(),
        &command_script(&FrameCommand::Clear)
    );
    assert!(!probe.escape_forwarded.get(), "Esc is the page's again");
}

// --------------------------------------------------------------------------
// Section index
// --------------------------------------------------------------------------

fn index_titles(tab: &Entity<ArtifactTab>, vcx: &mut VisualTestContext) -> Vec<String> {
    tab.read_with(vcx, |tab, _| {
        tab.section_index()
            .sections
            .iter()
            .map(|section| section.title.clone())
            .collect()
    })
}

#[gpui::test]
fn the_index_lists_markdown_headings_and_jumps_to_a_section(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let long: String = (0..60)
        .map(|n| format!("Filler paragraph {n}.\n\n"))
        .collect();
    let source =
        format!("# Plan\n\n{long}## Steps\n\n{long}## Risks\n\n{long}### Detail\n\nEnd.\n");
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown, &source);
    assert_eq!(
        index_titles(&tab, vcx),
        ["Plan", "Steps", "Risks", "Detail"]
    );
    assert!(tab.read_with(vcx, |tab, _| tab.index_shown()));
    assert!(vcx.debug_bounds("artifact-index").is_some(), "rail draws");
    assert_eq!(
        tab.read_with(vcx, |tab, cx| tab.current_section(cx)),
        Some(0)
    );

    // Click "Risks": the list jumps there and the row highlights.
    let row = vcx.debug_bounds("artifact-index-row-2").expect("Risks row");
    vcx.simulate_click(row.center(), gpui::Modifiers::default());
    settle(vcx);
    let top = tab.read_with(vcx, |tab, cx| {
        tab.markdown_for_tests().unwrap().read(cx).top_block()
    });
    let risks = tab.read_with(vcx, |tab, _| tab.section_index().sections[2].target);
    assert_eq!(top, risks);
    assert_eq!(
        tab.read_with(vcx, |tab, cx| tab.current_section(cx)),
        Some(2),
        "the section in view is highlighted"
    );
}

#[gpui::test]
fn the_index_follows_the_scroll_position(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let long: String = (0..60)
        .map(|n| format!("Filler paragraph {n}.\n\n"))
        .collect();
    let source = format!("# Plan\n\n{long}## Steps\n\n{long}## Risks\n\n{long}");
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown, &source);
    let steps = tab.read_with(vcx, |tab, _| tab.section_index().sections[1].target);
    // Scroll partway into "Steps" (a paragraph after its heading).
    tab.update(vcx, |tab, cx| {
        tab.markdown_for_tests().unwrap().update(cx, |preview, cx| {
            preview.scroll_to_block(steps + 5, cx);
        });
    });
    settle(vcx);
    assert_eq!(
        tab.read_with(vcx, |tab, cx| tab.current_section(cx)),
        Some(1)
    );
}

#[gpui::test]
fn plans_with_fewer_than_three_headings_get_no_index(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(
        cx,
        ArtifactKind::Markdown,
        "# Plan\n\ntext\n\n## One\n\ntext\n",
    );
    assert_eq!(index_titles(&tab, vcx), ["Plan", "One"]);
    assert!(!tab.read_with(vcx, |tab, _| tab.index_shown()));
    assert!(vcx.debug_bounds("artifact-index").is_none());

    let (tab, _probe, vcx) = open(
        cx,
        ArtifactKind::HtmlFragment,
        "<h2>A</h2><p>x</p><h2>B</h2>",
    );
    assert!(!tab.read_with(vcx, |tab, _| tab.index_shown()));
    assert!(vcx.debug_bounds("artifact-index").is_none());
}

#[gpui::test]
fn the_index_can_collapse_to_a_strip_and_back(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown, PLAN_MD);
    let open_width = vcx.debug_bounds("artifact-index").expect("rail").size.width;
    let toggle = vcx.debug_bounds("artifact-index-toggle").expect("toggle");
    vcx.simulate_click(toggle.center(), gpui::Modifiers::default());
    settle(vcx);
    let closed_width = vcx
        .debug_bounds("artifact-index")
        .expect("strip")
        .size
        .width;
    assert!(closed_width < open_width);
    assert!(
        vcx.debug_bounds("artifact-index-row-0").is_none(),
        "rows are hidden"
    );
    let toggle = vcx.debug_bounds("artifact-index-toggle").expect("toggle");
    vcx.simulate_click(toggle.center(), gpui::Modifiers::default());
    settle(vcx);
    assert!(vcx.debug_bounds("artifact-index-row-0").is_some());
}

#[gpui::test]
fn html_index_comes_from_the_source_and_scrolls_the_page(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, probe, vcx) = open(cx, ArtifactKind::HtmlPage, PLAN_HTML);
    assert_eq!(
        index_titles(&tab, vcx),
        ["Plan", "Steps", "Risks", "Detail"]
    );
    let row = vcx.debug_bounds("artifact-index-row-2").expect("Risks row");
    vcx.simulate_click(row.center(), gpui::Modifiers::default());
    settle(vcx);
    // "Risks" is the third h1–h3 in the page.
    assert_eq!(
        scripts(&probe).last().unwrap(),
        &command_script(&FrameCommand::Scroll(2))
    );
    assert_eq!(
        tab.read_with(vcx, |tab, cx| tab.current_section(cx)),
        Some(2)
    );

    // The page reports it has scrolled to "Steps".
    *probe.state.borrow_mut() = Some(r#"{"active":1}"#.into());
    vcx.executor().advance_clock(Duration::from_millis(500));
    settle(vcx);
    assert_eq!(
        tab.read_with(vcx, |tab, cx| tab.current_section(cx)),
        Some(1)
    );
}

#[gpui::test]
fn html_headings_the_page_built_with_script_replace_the_source_list(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, probe, vcx) = open(
        cx,
        ArtifactKind::HtmlFragment,
        "<h1>Plan</h1><div id=more></div><script>/* adds headings */</script>",
    );
    assert!(
        !tab.read_with(vcx, |tab, _| tab.index_shown()),
        "only one in the source"
    );
    *probe.state.borrow_mut() =
        Some(r#"{"heads":[[1,"Plan"],[2,"Built A"],[2,"Built B"]],"active":0}"#.into());
    vcx.executor().advance_clock(Duration::from_millis(500));
    settle(vcx);
    assert_eq!(index_titles(&tab, vcx), ["Plan", "Built A", "Built B"]);
    assert!(tab.read_with(vcx, |tab, _| tab.index_shown()));
}

#[gpui::test]
fn images_and_diagrams_have_no_index(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Mermaid, "graph TD; A-->B");
    assert!(!tab.read_with(vcx, |tab, _| tab.index_shown()));
}

// --------------------------------------------------------------------------
// Section keys
// --------------------------------------------------------------------------

/// Four sections of filler, the last one too short to reach the top of the
/// view.
fn long_plan() -> String {
    let long: String = (0..60)
        .map(|n| format!("Filler paragraph {n}.\n\n"))
        .collect();
    format!("# Plan\n\n{long}## Steps\n\n{long}## Risks\n\n{long}### Detail\n\nEnd.\n")
}

fn section(tab: &Entity<ArtifactTab>, vcx: &mut VisualTestContext) -> Option<usize> {
    tab.read_with(vcx, |tab, cx| tab.current_section(cx))
}

fn top_block(tab: &Entity<ArtifactTab>, vcx: &mut VisualTestContext) -> usize {
    tab.read_with(vcx, |tab, cx| {
        tab.markdown_for_tests().unwrap().read(cx).top_block()
    })
}

fn target(tab: &Entity<ArtifactTab>, vcx: &mut VisualTestContext, ix: usize) -> usize {
    tab.read_with(vcx, |tab, _| tab.section_index().sections[ix].target)
}

fn focus_preview(tab: &Entity<ArtifactTab>, vcx: &mut VisualTestContext) {
    vcx.update(|window, cx| {
        let preview = tab.read(cx).markdown_for_tests().expect("preview");
        let handle = preview.read(cx).focus.clone();
        window.focus(&handle, cx);
    });
}

fn rail_focused(tab: &Entity<ArtifactTab>, vcx: &mut VisualTestContext) -> bool {
    vcx.update(|window, cx| tab.read(cx).rail_focused_for_tests(window))
}

/// The page reports where it is, as it does after loading and on scroll.
fn page_reports(probe: &WebProbe, vcx: &mut VisualTestContext, json: &str) {
    *probe.state.borrow_mut() = Some(json.into());
    vcx.executor().advance_clock(Duration::from_millis(500));
    settle(vcx);
}

fn press(vcx: &mut VisualTestContext, keys: &str) {
    vcx.simulate_keystrokes(keys);
    settle(vcx);
}

/// Count the keystrokes nothing handled (those that reach the document).
fn count_unhandled_keys(vcx: &mut VisualTestContext) -> std::rc::Rc<std::cell::Cell<u32>> {
    let seen = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let counter = seen.clone();
    vcx.update(|_, cx| {
        cx.observe_keystrokes(move |_, _, _| counter.set(counter.get() + 1))
            .detach();
    });
    seen
}

#[gpui::test]
fn rail_arrows_move_the_highlight_and_scroll_without_wrapping(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown, &long_plan());
    focus_tab(&tab, vcx);
    assert!(!rail_focused(&tab, vcx));
    press(vcx, &key("mod-alt-s"));
    assert!(rail_focused(&tab, vcx), "⌥⌘S focuses the rail");
    assert_eq!(section(&tab, vcx), Some(0));
    let reached = count_unhandled_keys(vcx);

    press(vcx, "down");
    assert_eq!(reached.get(), 0, "the rail consumes its keys");
    assert_eq!(section(&tab, vcx), Some(1));
    assert_eq!(
        top_block(&tab, vcx),
        target(&tab, vcx, 1),
        "the page scrolled"
    );
    press(vcx, "down");
    assert_eq!(section(&tab, vcx), Some(2));
    assert_eq!(top_block(&tab, vcx), target(&tab, vcx, 2));
    press(vcx, "up");
    assert_eq!(section(&tab, vcx), Some(1));

    // End: the last section is too short to scroll to the top, but it is
    // the highlighted one, and ↑ steps from it.
    press(vcx, "end");
    assert_eq!(section(&tab, vcx), Some(3));
    press(vcx, "down");
    assert_eq!(section(&tab, vcx), Some(3), "no wraparound at the end");
    press(vcx, "up");
    assert_eq!(section(&tab, vcx), Some(2));
    assert_eq!(top_block(&tab, vcx), target(&tab, vcx, 2));

    press(vcx, "home");
    assert_eq!(section(&tab, vcx), Some(0));
    assert_eq!(top_block(&tab, vcx), target(&tab, vcx, 0));
    press(vcx, "up");
    assert_eq!(section(&tab, vcx), Some(0), "no wraparound at the start");
    assert!(rail_focused(&tab, vcx), "the rail keeps focus");
}

#[gpui::test]
fn clicking_a_rail_row_focuses_the_rail_and_enter_or_escape_leave_it(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown, &long_plan());
    let row = vcx.debug_bounds("artifact-index-row-1").expect("row");
    vcx.simulate_click(row.center(), gpui::Modifiers::default());
    settle(vcx);
    assert!(rail_focused(&tab, vcx), "a click in the rail focuses it");
    assert_eq!(section(&tab, vcx), Some(1));
    press(vcx, "down");
    assert_eq!(section(&tab, vcx), Some(2));

    for leave in ["enter", "escape"] {
        press(vcx, &key("mod-alt-s"));
        assert!(rail_focused(&tab, vcx));
        press(vcx, leave);
        assert!(!rail_focused(&tab, vcx), "{leave} leaves the rail");
        let in_preview = vcx.update(|window, cx| {
            let preview = tab.read(cx).markdown_for_tests().expect("preview");
            preview.read(cx).focus.is_focused(window)
        });
        assert!(in_preview, "{leave} returns focus to the document");
    }
    assert_eq!(section(&tab, vcx), Some(2), "leaving does not move");
}

#[gpui::test]
fn alt_arrows_jump_sections_from_the_markdown_document(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown, &long_plan());
    focus_preview(&tab, vcx);
    assert_eq!(section(&tab, vcx), Some(0));

    press(vcx, "alt-up");
    assert_eq!(section(&tab, vcx), Some(0), "nothing before the first");
    press(vcx, "alt-down");
    assert_eq!(section(&tab, vcx), Some(1));
    assert_eq!(top_block(&tab, vcx), target(&tab, vcx, 1));
    press(vcx, "alt-down alt-down");
    assert_eq!(section(&tab, vcx), Some(3), "the rail highlight follows");
    press(vcx, "alt-down");
    assert_eq!(section(&tab, vcx), Some(3), "no wraparound");
    press(vcx, "alt-up");
    assert_eq!(section(&tab, vcx), Some(2));
    assert_eq!(top_block(&tab, vcx), target(&tab, vcx, 2));
    assert!(!rail_focused(&tab, vcx), "the document keeps focus");
}

#[gpui::test]
fn alt_arrows_start_from_the_top_when_the_page_opens_above_the_first_heading(
    cx: &mut TestAppContext,
) {
    let _dir = init_ui(cx);
    let filler: String = (0..60).map(|n| format!("Filler {n}.\n\n")).collect();
    let source =
        format!("Intro text.\n\n{filler}## One\n\n{filler}## Two\n\n{filler}## Three\n\n{filler}");
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown, &source);
    focus_preview(&tab, vcx);
    press(vcx, "alt-up");
    assert_eq!(top_block(&tab, vcx), 0, "nothing above the first section");
    press(vcx, "alt-down");
    assert_eq!(top_block(&tab, vcx), target(&tab, vcx, 0), "lands on One");
    press(vcx, "alt-down");
    assert_eq!(top_block(&tab, vcx), target(&tab, vcx, 1));
    assert_eq!(section(&tab, vcx), Some(1));
}

#[gpui::test]
fn plain_arrows_still_reach_the_document(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown, &long_plan());
    focus_preview(&tab, vcx);
    let reached = count_unhandled_keys(vcx);
    press(vcx, "down up home end");
    assert_eq!(reached.get(), 4, "nothing in the tab consumed them");
    assert_eq!(section(&tab, vcx), Some(0), "and no section moved");
    press(vcx, "alt-down");
    assert_eq!(section(&tab, vcx), Some(1), "⌥↓ is the tab's");
}

#[gpui::test]
fn html_alt_arrows_scroll_the_page_by_heading_and_the_rail_follows(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    for kind in [ArtifactKind::HtmlPage, ArtifactKind::HtmlFragment] {
        let (tab, probe, vcx) = open(cx, kind, PLAN_HTML);
        focus_tab(&tab, vcx);
        page_reports(&probe, vcx, r#"{"active":0}"#);
        let before = scripts(&probe).len();
        press(vcx, "alt-up");
        assert_eq!(
            scripts(&probe).len(),
            before,
            "{kind:?}: nothing before the first"
        );

        press(vcx, "alt-down");
        assert_eq!(
            scripts(&probe).last().unwrap(),
            &command_script(&FrameCommand::Scroll(1)),
            "{kind:?}"
        );
        assert_eq!(section(&tab, vcx), Some(1));
        press(vcx, "alt-down alt-down");
        assert_eq!(
            scripts(&probe).last().unwrap(),
            &command_script(&FrameCommand::Scroll(3))
        );
        assert_eq!(section(&tab, vcx), Some(3));
        let sent = scripts(&probe).len();
        press(vcx, "alt-down");
        assert_eq!(scripts(&probe).len(), sent, "{kind:?}: no wraparound");
        press(vcx, "alt-up");
        assert_eq!(
            scripts(&probe).last().unwrap(),
            &command_script(&FrameCommand::Scroll(2))
        );
        assert_eq!(section(&tab, vcx), Some(2));
    }
}

#[gpui::test]
fn html_rail_keys_step_and_hand_focus_back_to_the_page(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, probe, vcx) = open(cx, ArtifactKind::HtmlPage, PLAN_HTML);
    focus_tab(&tab, vcx);
    page_reports(&probe, vcx, r#"{"active":0}"#);
    press(vcx, &key("mod-alt-s"));
    assert!(rail_focused(&tab, vcx));
    assert_eq!(
        probe.chrome_focused.get(),
        1,
        "the page lets go of the keys"
    );
    press(vcx, "down");
    assert_eq!(
        scripts(&probe).last().unwrap(),
        &command_script(&FrameCommand::Scroll(1))
    );
    press(vcx, "end");
    assert_eq!(
        scripts(&probe).last().unwrap(),
        &command_script(&FrameCommand::Scroll(3))
    );
    assert_eq!(section(&tab, vcx), Some(3));
    press(vcx, "home");
    assert_eq!(
        scripts(&probe).last().unwrap(),
        &command_script(&FrameCommand::Scroll(0))
    );
    assert_eq!(probe.page_focused.get(), 0);
    press(vcx, "escape");
    assert!(!rail_focused(&tab, vcx));
    assert_eq!(probe.page_focused.get(), 1, "the page gets focus back");
}

#[gpui::test]
fn the_last_short_section_of_an_html_page_stays_highlighted(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, probe, vcx) = open(cx, ArtifactKind::HtmlPage, PLAN_HTML);
    focus_tab(&tab, vcx);
    page_reports(&probe, vcx, r#"{"active":0}"#);
    press(vcx, "alt-down alt-down alt-down");
    assert_eq!(section(&tab, vcx), Some(3));
    // The page can't scroll "Detail" to the top: it reports "Risks" at the
    // bottom of the page.
    page_reports(&probe, vcx, r#"{"active":2,"atBottom":true}"#);
    assert_eq!(section(&tab, vcx), Some(3));
    press(vcx, "alt-up");
    assert_eq!(
        scripts(&probe).last().unwrap(),
        &command_script(&FrameCommand::Scroll(2))
    );
    // Scrolled away by hand: the page's own report wins.
    page_reports(&probe, vcx, r#"{"active":1,"atBottom":false}"#);
    assert_eq!(section(&tab, vcx), Some(1));
}

#[gpui::test]
fn alt_arrows_with_fewer_than_three_headings_use_what_exists(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let filler: String = (0..60).map(|n| format!("Filler {n}.\n\n")).collect();
    let (tab, _probe, vcx) = open(
        cx,
        ArtifactKind::Markdown,
        &format!("# Plan\n\n{filler}## One\n\n{filler}"),
    );
    assert!(!tab.read_with(vcx, |tab, _| tab.index_shown()), "no rail");
    focus_preview(&tab, vcx);
    press(vcx, "alt-down");
    assert_eq!(top_block(&tab, vcx), target(&tab, vcx, 1));
    press(vcx, "alt-down");
    assert_eq!(top_block(&tab, vcx), target(&tab, vcx, 1), "no wraparound");
    press(vcx, "alt-up");
    assert_eq!(top_block(&tab, vcx), target(&tab, vcx, 0));
    press(vcx, "alt-up");
    assert_eq!(top_block(&tab, vcx), target(&tab, vcx, 0), "no wraparound");
    // ⌥⌘S has no rail to focus.
    press(vcx, &key("mod-alt-s"));
    assert!(!rail_focused(&tab, vcx));

    // No headings at all, or an HTML fragment with one: nothing happens.
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown, "Just prose.\n");
    focus_preview(&tab, vcx);
    press(vcx, "alt-down alt-up");
    assert_eq!(top_block(&tab, vcx), 0);
    assert_eq!(section(&tab, vcx), None);
    let (tab, probe, vcx) = open(cx, ArtifactKind::HtmlFragment, "<h2>Only</h2><p>x</p>");
    focus_tab(&tab, vcx);
    let before = scripts(&probe).len();
    press(vcx, "alt-down");
    assert_eq!(scripts(&probe).len(), before + 1, "the lone heading");
    page_reports(&probe, vcx, r#"{"active":0}"#);
    press(vcx, "alt-down alt-up");
    assert_eq!(scripts(&probe).len(), before + 1, "and nothing past it");
}

#[gpui::test]
fn section_keys_do_nothing_outside_a_plan_view_and_outside_the_tab(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, probe, vcx) = open(cx, ArtifactKind::Mermaid, "graph TD; A-->B");
    focus_tab(&tab, vcx);
    let reached = count_unhandled_keys(vcx);
    press(vcx, "alt-down");
    assert!(scripts(&probe).is_empty());
    assert_eq!(reached.get(), 1, "falls through to the next handler");

    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown, &long_plan());
    vcx.update(|window, _| window.blur());
    press(vcx, "alt-down");
    assert_eq!(
        section(&tab, vcx),
        Some(0),
        "not the tab's key without focus"
    );
}

#[gpui::test]
fn find_keys_still_work_with_the_section_keys_around(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown, &long_plan());
    focus_preview(&tab, vcx);
    press(vcx, &key("mod-f"));
    assert!(find_open(&tab, vcx));
    type_query(vcx, "filler");
    let total = tab.read_with(vcx, |tab, _| tab.find_state().count());
    assert!(total > 100, "{total} matches");
    // In the find field ⌥↓ belongs to the field: no section moves.
    press(vcx, "alt-down");
    assert_eq!(section(&tab, vcx), Some(0));
    // Enter, ⌘G and ⇧⌘G still step through matches.
    press(vcx, "enter");
    assert_eq!(
        tab.read_with(vcx, |tab, _| tab.find_state().current()),
        Some(1)
    );
    press(vcx, &key("mod-g"));
    assert_eq!(
        tab.read_with(vcx, |tab, _| tab.find_state().current()),
        Some(2)
    );
    press(vcx, &key("mod-shift-g"));
    assert_eq!(
        tab.read_with(vcx, |tab, _| tab.find_state().current()),
        Some(1)
    );

    // With the document focused and the bar open, ⌥↓ jumps sections and
    // the bar stays.
    focus_preview(&tab, vcx);
    press(vcx, "alt-down");
    assert_eq!(section(&tab, vcx), Some(1));
    assert!(find_open(&tab, vcx));

    // Escape in the rail leaves the rail only; Escape in the field closes.
    press(vcx, &key("mod-alt-s"));
    press(vcx, "escape");
    assert!(find_open(&tab, vcx), "the rail's Esc does not close find");
    vcx.update(|window, cx| {
        let handle = gpui::Focusable::focus_handle(tab.read(cx).nav_input_for_tests(), cx);
        window.focus(&handle, cx);
    });
    press(vcx, "escape");
    assert!(!find_open(&tab, vcx));
}

#[test]
fn the_page_hands_over_the_section_keys_and_escape_only_with_find_open() {
    use super::tab::page_forwarded_keys;
    let without = page_forwarded_keys(false);
    let with = page_forwarded_keys(true);
    assert_eq!(&without[..2], ["alt-up", "alt-down"]);
    assert!(!without.contains(&"escape") && with.contains(&"escape"));
    // The zoom keys always reach the tab.
    for key in super::zoom::PAGE_FORWARDED_KEYS {
        assert!(without.contains(&key) && with.contains(&key), "{key}");
    }
    // Plain arrows are never handed over: they scroll the page.
    assert!(
        !page_forwarded_keys(true)
            .iter()
            .any(|key| matches!(*key, "up" | "down"))
    );
}

#[gpui::test]
fn alt_arrows_step_through_a_plan_short_enough_to_need_no_scrolling(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let (tab, _probe, vcx) = open(cx, ArtifactKind::Markdown, PLAN_MD);
    focus_preview(&tab, vcx);
    // Nothing scrolls, so the highlight is what moves.
    for want in [1, 2, 3, 3] {
        press(vcx, "alt-down");
        assert_eq!(section(&tab, vcx), Some(want));
    }
    press(vcx, "alt-up");
    assert_eq!(section(&tab, vcx), Some(2));
    press(vcx, "alt-up alt-up alt-up");
    assert_eq!(section(&tab, vcx), Some(0));
}
