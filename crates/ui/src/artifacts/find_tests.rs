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
