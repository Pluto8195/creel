//! ⌘F find and the section index in the Artifact tab.
//!
//! The logic lives in `artifacts/find.rs` (match model, scripts) and
//! `artifacts/outline.rs` (headings, index); this file wires them to the two
//! renderers: the Markdown preview (a native block list) and the web view
//! (the sandboxed frame, driven through `ArtifactWebHost`).
//!
//! Keys: ⌘F / ⌘G / ⇧⌘G and ⌥↑ / ⌥↓ (previous / next section) are bound in the
//! `ArtifactTab` key context, so they apply only while focus is inside the
//! tab (the web view forwards them when it has focus); everywhere else ⌘F
//! still focuses the outline search. ⌥⌘S focuses the section rail, where
//! ↑ / ↓ / Home / End move through the sections and Enter / Esc hand focus
//! back to the document.
//! Enter / ⇧Enter step and Esc closes while the tab has focus.
use std::time::Duration;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, KeyBinding, Stateful, Subscription, Task,
    Window, div, prelude::*, px,
};

use super::{ArtifactBody, ArtifactTab, Mode, Presenter, WebKind};
use crate::artifacts::find::{FindState, FrameCommand, FrameState, command_script};
use crate::artifacts::outline::{self, RawHeading, SectionIndex, SectionStep};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::files::markdown_preview::{MarkdownPreview, MarkdownPreviewEvent};
use crate::theme::Theme;
use crate::{icons, surface_chrome};

gpui::actions!(
    artifact_find,
    [
        FindOpen,
        FindNext,
        FindPrevious,
        SectionPrevious,
        SectionNext,
        FocusIndex
    ]
);

/// The key context of the Artifact tab; find keys bind inside it.
pub(crate) const KEY_CONTEXT: &str = "ArtifactTab";

/// Width of the section rail.
const RAIL_WIDTH: f32 = 200.0;
/// How often the page is asked where it is (section in view, match count).
const POLL_MS: u64 = 350;
/// How soon after a find command the page is asked for the new count.
const COUNT_DELAY_MS: u64 = 70;

/// Bind ⌘F / ⌘G / ⇧⌘G (Ctrl elsewhere) in the Artifact tab. Registered after
/// the app's global ⌘F so a tab with focus wins ties.
pub(crate) fn bind_keys(cx: &mut App) {
    use crate::settings::platform_combo;
    cx.bind_keys([
        KeyBinding::new(&platform_combo("mod-f"), FindOpen, Some(KEY_CONTEXT)),
        KeyBinding::new(&platform_combo("mod-g"), FindNext, Some(KEY_CONTEXT)),
        KeyBinding::new(
            &platform_combo("mod-shift-g"),
            FindPrevious,
            Some(KEY_CONTEXT),
        ),
        KeyBinding::new("alt-up", SectionPrevious, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-down", SectionNext, Some(KEY_CONTEXT)),
        KeyBinding::new(&platform_combo("mod-alt-s"), FocusIndex, Some(KEY_CONTEXT)),
    ]);
}

/// What find and the index talk to right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    Markdown,
    Web,
}

/// State owned by `find_view`: the find bar, the section index, and the
/// poll that reads the page.
pub(crate) struct FindNav {
    pub(crate) find: FindState,
    pub(crate) input: Entity<ComposerInput>,
    markdown_index: SectionIndex,
    web_index: SectionIndex,
    pub(crate) index_open: bool,
    /// Focus of the section rail (↑ / ↓ / Home / End step through it).
    rail_focus: FocusHandle,
    /// The section last jumped to. Short sections at the end of a page can't
    /// reach the top of the view, so the position alone would put the
    /// highlight (and the next step) on an earlier section.
    pin: Option<usize>,
    /// The page is scrolled to its end (reported by the frame).
    web_at_bottom: bool,
    /// Headings in the HTML source (`h1`–`h3`, document order).
    html_headings: Vec<RawHeading>,
    /// Headings the live page reported.
    live_heads: Vec<(u8, String)>,
    /// The heading position the page says is in view.
    web_active: Option<usize>,
    /// The (query, count) last taken from the page.
    web_count: Option<(String, usize)>,
    poll: Option<Task<()>>,
    read: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl FindNav {
    pub(crate) fn new(cx: &mut Context<ArtifactTab>) -> Self {
        let input = cx.new(|cx| {
            ComposerInput::with_context("Find in artifact", "PaletteSearch", cx)
                .with_text_metrics(11.0, 16.0)
                .with_single_line()
                .with_accessibility_role(gpui::Role::TextInput)
        });
        let edited = cx.subscribe(&input, |tab, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                tab.find_text_edited(cx);
            }
        });
        FindNav {
            find: FindState::default(),
            input,
            markdown_index: SectionIndex::default(),
            web_index: SectionIndex::default(),
            index_open: true,
            rail_focus: cx.focus_handle(),
            pin: None,
            web_at_bottom: false,
            html_headings: Vec::new(),
            live_heads: Vec::new(),
            web_active: None,
            web_count: None,
            poll: None,
            read: None,
            _subscriptions: vec![edited],
        }
    }
}

impl ArtifactTab {
    // ------------------------------------------------------------------
    // Which surface
    // ------------------------------------------------------------------

    /// The renderer find drives in the current view, if it can search it.
    pub(crate) fn find_surface(&self) -> Option<Surface> {
        match self.presenter() {
            Presenter::Markdown => Some(Surface::Markdown),
            Presenter::Web(_) if self.web_scriptable() => Some(Surface::Web),
            _ => None,
        }
    }

    fn web_scriptable(&self) -> bool {
        self.web.as_ref().is_some_and(|web| web.supports_scripts())
    }

    /// Whether the section index can show for the current view (the plan
    /// itself, not an option's fragment, not source or compare).
    fn index_surface(&self) -> Option<Surface> {
        if self.mode != Mode::Rendered || self.option_shown.is_some() {
            return None;
        }
        match self.presenter() {
            Presenter::Markdown => Some(Surface::Markdown),
            Presenter::Web(WebKind::Fragment | WebKind::Page) if self.web_scriptable() => {
                Some(Surface::Web)
            }
            _ => None,
        }
    }

    pub(crate) fn find_state(&self) -> &FindState {
        &self.nav.find
    }

    /// The section index of the view on screen (empty when it has none).
    pub(crate) fn section_index(&self) -> &SectionIndex {
        static EMPTY: SectionIndex = SectionIndex {
            sections: Vec::new(),
        };
        match self.index_surface() {
            Some(Surface::Markdown) => &self.nav.markdown_index,
            Some(Surface::Web) => &self.nav.web_index,
            None => &EMPTY,
        }
    }

    #[cfg(test)]
    pub(crate) fn markdown_for_tests(&self) -> Option<Entity<MarkdownPreview>> {
        self.markdown.clone()
    }

    #[cfg(test)]
    pub(crate) fn nav_input_for_tests(&self) -> &Entity<ComposerInput> {
        &self.nav.input
    }

    #[cfg(test)]
    pub(crate) fn rail_focused_for_tests(&self, window: &Window) -> bool {
        self.nav.rail_focus.is_focused(window)
    }

    pub(crate) fn find_focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    // ------------------------------------------------------------------
    // Find
    // ------------------------------------------------------------------

    /// Open the find bar and put the caret in it. `false` when this view has
    /// nothing to search (the key then falls through to the next handler).
    pub(crate) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(surface) = self.find_surface() else {
            return false;
        };
        self.nav.find.open = true;
        if surface == Surface::Web {
            self.start_poll(cx);
            if let Some(web) = &mut self.web {
                web.focus_chrome(cx);
                web.forward_escape(true, cx);
            }
        }
        let handle = gpui::Focusable::focus_handle(self.nav.input.read(cx), cx);
        window.focus(&handle, cx);
        window.dispatch_action(Box::new(crate::composer::SelectAll), cx);
        // A query typed earlier is still in the field: show its matches.
        let text = self.nav.input.read(cx).text().to_owned();
        self.nav.find.set_query(&text);
        self.apply_find(cx);
        cx.notify();
        true
    }

    pub(crate) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.nav.find.open {
            return;
        }
        self.nav.find.close();
        self.nav.web_count = None;
        match self.find_surface() {
            Some(Surface::Markdown) => {
                if let Some(preview) = self.markdown.clone() {
                    preview.update(cx, |preview, cx| preview.clear_find(cx));
                    let focus = preview.read(cx).focus.clone();
                    window.focus(&focus, cx);
                }
            }
            Some(Surface::Web) => {
                self.web_command(&FrameCommand::Clear, cx);
                window.focus(&self.focus, cx);
            }
            None => window.focus(&self.focus, cx),
        }
        if let Some(web) = &mut self.web {
            web.forward_escape(false, cx);
        }
        cx.notify();
    }

    /// The query field changed.
    fn find_text_edited(&mut self, cx: &mut Context<Self>) {
        if !self.nav.find.open {
            return;
        }
        let text = self.nav.input.read(cx).text().to_owned();
        if text == self.nav.find.query {
            return;
        }
        self.nav.find.set_query(&text);
        self.nav.web_count = None;
        self.apply_find(cx);
        cx.notify();
    }

    /// Run the current query on the current surface.
    pub(crate) fn apply_find(&mut self, cx: &mut Context<Self>) {
        if !self.nav.find.open {
            return;
        }
        let query = self.nav.find.query.clone();
        match self.find_surface() {
            Some(Surface::Markdown) => {
                let Some(preview) = self.markdown.clone() else {
                    return;
                };
                let count = preview.update(cx, |preview, cx| preview.set_find_query(&query, cx));
                self.nav.find.set_count(count);
                let current = self.nav.find.current();
                preview.update(cx, |preview, cx| preview.set_find_current(current, cx));
            }
            Some(Surface::Web) => {
                if query.is_empty() {
                    self.web_command(&FrameCommand::Clear, cx);
                } else {
                    self.web_command(&FrameCommand::Find(&query), cx);
                    self.read_web_soon(COUNT_DELAY_MS, cx);
                }
            }
            None => {}
        }
    }

    /// ⌘G / Enter (`forward`) or ⇧⌘G / ⇧Enter: the next or previous match.
    pub(crate) fn find_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        if !self.nav.find.open {
            return;
        }
        let Some(current) = self.nav.find.step(forward) else {
            return;
        };
        match self.find_surface() {
            Some(Surface::Markdown) => {
                if let Some(preview) = self.markdown.clone() {
                    preview.update(cx, |preview, cx| {
                        preview.set_find_current(Some(current), cx)
                    });
                }
            }
            Some(Surface::Web) => self.web_command(&FrameCommand::Goto(current), cx),
            None => {}
        }
        cx.notify();
    }

    fn web_command(&mut self, command: &FrameCommand, cx: &mut Context<Self>) {
        let script = command_script(command);
        if let Some(web) = &mut self.web {
            web.run_script(&script, cx);
        }
    }

    // ------------------------------------------------------------------
    // The page's reports (web)
    // ------------------------------------------------------------------

    /// Ask the page where it is, now plus `delay_ms`.
    fn read_web_soon(&mut self, delay_ms: u64, cx: &mut Context<Self>) {
        let Some(web) = &mut self.web else {
            return;
        };
        let read = web.read_state(cx);
        self.nav.read = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(delay_ms))
                .await;
            let json = read.await;
            this.update(cx, |this, cx| this.apply_web_state(json, cx))
                .ok();
        }));
    }

    /// Poll the page while it is on screen: the section in view and the live
    /// heading list drive the index, the match count drives find.
    fn start_poll(&mut self, cx: &mut Context<Self>) {
        if self.nav.poll.is_some() {
            return;
        }
        self.nav.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(POLL_MS))
                    .await;
                let alive = this
                    .update(cx, |this, cx| {
                        if this.shows_web() {
                            // A page that was still loading dropped the
                            // query: send it again until it answers.
                            let find = &this.nav.find;
                            if find.open && find.is_searching() && this.nav.web_count.is_none() {
                                let query = find.query.clone();
                                this.web_command(&FrameCommand::Find(&query), cx);
                            }
                            this.read_web_soon(0, cx);
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        }));
    }

    /// Take a report from the page.
    pub(crate) fn apply_web_state(&mut self, json: Option<String>, cx: &mut Context<Self>) {
        let Some(state) = json.as_deref().and_then(FrameState::parse) else {
            return;
        };
        let mut changed = false;
        if state.heads != self.nav.live_heads {
            self.nav.live_heads = state.heads.clone();
            self.rebuild_index(cx);
            changed = true;
        }
        if state.active != self.nav.web_active {
            self.nav.web_active = state.active;
            changed = true;
        }
        if state.at_bottom != self.nav.web_at_bottom {
            self.nav.web_at_bottom = state.at_bottom;
            changed = true;
        }
        if self.nav.find.open
            && self.nav.find.is_searching()
            && let Some(count) = state.count_for(&self.nav.find.query)
        {
            let key = (self.nav.find.query.clone(), count);
            if self.nav.web_count.as_ref() != Some(&key) {
                self.nav.web_count = Some(key);
                self.nav.find.set_count(count);
                if let Some(current) = self.nav.find.current() {
                    self.web_command(&FrameCommand::Goto(current), cx);
                }
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    // ------------------------------------------------------------------
    // Section index
    // ------------------------------------------------------------------

    /// The body just changed: re-derive headings and re-run find.
    pub(super) fn document_changed(&mut self, cx: &mut Context<Self>) {
        self.nav.live_heads.clear();
        self.nav.web_active = None;
        self.nav.web_at_bottom = false;
        self.nav.pin = None;
        self.nav.web_count = None;
        self.nav.html_headings = match &self.body {
            Some(ArtifactBody::Web {
                kind: WebKind::Fragment | WebKind::Page,
                source,
            }) => outline::html_headings(source),
            _ => Vec::new(),
        };
        if self.web.is_some() {
            self.start_poll(cx);
        }
        self.rebuild_index(cx);
        self.apply_find(cx);
    }

    /// Follow the Markdown preview: a finished parse changes the headings, a
    /// recount (after a re-parse) changes the match total.
    pub(super) fn watch_markdown(
        &mut self,
        preview: &Entity<MarkdownPreview>,
        cx: &mut Context<Self>,
    ) {
        let subscription = cx.subscribe(preview, |tab, _, event, cx| match event {
            MarkdownPreviewEvent::Parsed => {
                tab.rebuild_index(cx);
                cx.notify();
            }
            MarkdownPreviewEvent::FindRecounted(count) => {
                if tab.nav.find.open && tab.find_surface() == Some(Surface::Markdown) {
                    tab.nav.find.set_count(*count);
                    let current = tab.nav.find.current();
                    if let Some(preview) = tab.markdown.clone() {
                        preview.update(cx, |preview, cx| preview.set_find_current(current, cx));
                    }
                    cx.notify();
                }
            }
        });
        self.nav._subscriptions.push(subscription);
    }

    fn rebuild_index(&mut self, cx: &mut Context<Self>) {
        if let Some(preview) = &self.markdown {
            let headings = preview.read(cx).section_headings();
            self.nav.markdown_index = SectionIndex::from_headings(&headings);
        }
        let headings =
            outline::reconcile_html(self.nav.html_headings.clone(), &self.nav.live_heads);
        self.nav.web_index = SectionIndex::from_headings(&headings);
    }

    /// The section in view: the Markdown list's top block, or the heading
    /// the page last reported.
    pub(crate) fn current_section(&self, cx: &App) -> Option<usize> {
        let computed = self.section_at_position(cx)?;
        Some(self.held_pin(computed, cx).unwrap_or(computed))
    }

    fn section_at_position(&self, cx: &App) -> Option<usize> {
        let surface = self.index_surface()?;
        let index = self.section_index();
        match surface {
            Surface::Markdown => {
                let top = self.markdown.as_ref()?.read(cx).top_block();
                index.active_at(top)
            }
            Surface::Web => index.section_for_target(self.nav.web_active.unwrap_or(0)),
        }
    }

    /// The section last jumped to, while it still stands for the view: the
    /// view is at the end of the page (where short sections can't reach the
    /// top, or nothing scrolls at all) and hasn't been scrolled past it.
    fn held_pin(&self, computed: usize, cx: &App) -> Option<usize> {
        let pin = self.nav.pin?;
        let at_end = match self.index_surface()? {
            Surface::Markdown => self
                .markdown
                .as_ref()
                .is_some_and(|preview| preview.read(cx).is_scrolled_to_end()),
            Surface::Web => self.nav.web_at_bottom,
        };
        (at_end && pin >= computed && pin < self.section_index().sections.len()).then_some(pin)
    }

    /// Whether the view is above the first heading (a step forward then
    /// lands on the first section rather than the second).
    fn before_first_section(&self, cx: &App) -> bool {
        let Some(first) = self.section_index().sections.first() else {
            return false;
        };
        let Some(computed) = self.section_at_position(cx) else {
            return false;
        };
        if self.held_pin(computed, cx).is_some() {
            return false;
        }
        match self.index_surface() {
            Some(Surface::Markdown) => self
                .markdown
                .as_ref()
                .is_some_and(|preview| preview.read(cx).top_block() < first.target),
            Some(Surface::Web) => self.nav.web_active.is_none(),
            None => false,
        }
    }

    /// Move to the previous / next / first / last section. `false` when this
    /// view has no sections to move through; at either end the key is
    /// consumed and nothing happens (no wraparound). Works with the rail
    /// hidden, over whatever headings the plan has.
    pub(crate) fn step_section(&mut self, step: SectionStep, cx: &mut Context<Self>) -> bool {
        if self.index_surface().is_none() || self.section_index().sections.is_empty() {
            return false;
        }
        let from = if self.before_first_section(cx) {
            None
        } else {
            self.current_section(cx)
        };
        if let Some(target) = self.section_index().step_target(from, step) {
            self.goto_section(target, cx);
        }
        true
    }

    /// Put focus on the section rail, opening it first when it is collapsed.
    /// `false` when there is no rail to focus.
    pub(crate) fn focus_index(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.index_shown() {
            return false;
        }
        self.nav.index_open = true;
        if let Some(web) = &mut self.web {
            web.focus_chrome(cx);
        }
        window.focus(&self.nav.rail_focus, cx);
        cx.notify();
        true
    }

    /// Hand focus from the rail back to the document.
    pub(crate) fn focus_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.find_surface() {
            Some(Surface::Markdown) => {
                if let Some(preview) = self.markdown.clone() {
                    let focus = preview.read(cx).focus.clone();
                    window.focus(&focus, cx);
                    return;
                }
                window.focus(&self.focus, cx);
            }
            Some(Surface::Web) => {
                window.focus(&self.focus, cx);
                if let Some(web) = &mut self.web {
                    web.focus_page(cx);
                }
            }
            None => window.focus(&self.focus, cx),
        }
    }

    /// Whether the index is on screen (enough headings, a supported view).
    pub(crate) fn index_shown(&self) -> bool {
        self.section_index().is_shown()
    }

    pub(crate) fn toggle_index(&mut self, cx: &mut Context<Self>) {
        self.nav.index_open = !self.nav.index_open;
        cx.notify();
    }

    /// Scroll to section `ix` of the index.
    pub(crate) fn goto_section(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(section) = self.section_index().sections.get(ix).cloned() else {
            return;
        };
        match self.index_surface() {
            Some(Surface::Markdown) => {
                if let Some(preview) = self.markdown.clone() {
                    preview.update(cx, |preview, cx| {
                        preview.scroll_to_block(section.target, cx)
                    });
                }
            }
            Some(Surface::Web) => {
                self.web_command(&FrameCommand::Scroll(section.target), cx);
                self.nav.web_active = Some(section.target);
            }
            None => return,
        }
        self.nav.pin = Some(ix);
        cx.notify();
    }

    // ------------------------------------------------------------------
    // Rendering
    // ------------------------------------------------------------------

    /// Key context, find actions, and the Enter / Esc handling of the bar.
    pub(super) fn with_find_keys(
        &self,
        root: Stateful<gpui::Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let context = if self.nav.find.open {
            "ArtifactTab ArtifactFind"
        } else {
            KEY_CONTEXT
        };
        root.key_context(context)
            .track_focus(&self.focus)
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if !this.focus.contains_focused(window, cx) {
                        window.focus(&this.focus, cx);
                    }
                }),
            )
            .on_action(cx.listener(|this, _: &FindOpen, window, cx| {
                if !this.open_find(window, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &FindNext, _, cx| {
                if this.nav.find.open {
                    this.find_step(true, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &FindPrevious, _, cx| {
                if this.nav.find.open {
                    this.find_step(false, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &SectionPrevious, window, cx| {
                if !this.section_key(SectionStep::Previous, window, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &SectionNext, window, cx| {
                if !this.section_key(SectionStep::Next, window, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &FocusIndex, window, cx| {
                if !this.focus_index(window, cx) {
                    cx.propagate();
                }
            }))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if !this.nav.find.open {
                    return;
                }
                let mods = event.keystroke.modifiers;
                let modified = mods.platform || mods.control || mods.alt;
                match event.keystroke.key.as_str() {
                    "escape" if !modified => this.close_find(window, cx),
                    "enter" if !modified => {
                        let in_field = gpui::Focusable::focus_handle(this.nav.input.read(cx), cx)
                            .is_focused(window);
                        if !in_field {
                            return;
                        }
                        this.find_step(!mods.shift, cx)
                    }
                    _ => return,
                }
                cx.stop_propagation();
            }))
    }

    /// ⌥↑ / ⌥↓: step through the sections unless the caret is in the find
    /// field (where the key belongs to the field).
    fn section_key(&mut self, step: SectionStep, window: &Window, cx: &mut Context<Self>) -> bool {
        if self.nav.find.open
            && gpui::Focusable::focus_handle(self.nav.input.read(cx), cx).is_focused(window)
        {
            return false;
        }
        self.step_section(step, cx)
    }

    pub(super) fn render_find_bar(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.nav.find.open {
            return None;
        }
        let find = &self.nav.find;
        let has_matches = find.count() > 0;
        let button = |id: &'static str, glyph: &'static str, enabled: bool| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .size(px(20.0))
                .flex_none()
                .rounded(px(surface_chrome::CONTROL_RADIUS))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .when(!enabled, |el| el.opacity(0.35))
                .hover(|el| el.bg(crate::theme::wash(0.14)))
                .child(
                    icons::icon(glyph)
                        .size(px(11.0))
                        .text_color(theme.text_muted),
                )
        };
        Some(
            div()
                .id("artifact-find")
                .debug_selector(|| "artifact-find".into())
                .flex_none()
                .w_full()
                .h(px(32.0))
                .px(px(surface_chrome::EDGE_INSET))
                .flex()
                .items_center()
                .gap(px(6.0))
                .border_b_1()
                .border_color(theme.border)
                .child(
                    surface_chrome::input()
                        .child(
                            icons::icon(icons::MAGNIFER)
                                .size(px(12.0))
                                .flex_none()
                                .text_color(theme.text_faint),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .h(px(16.0))
                                .overflow_hidden()
                                .child(self.nav.input.clone()),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .min_w(px(58.0))
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_muted)
                        .debug_selector(|| "artifact-find-count".into())
                        .child(gpui::SharedString::from(find.label())),
                )
                .child(
                    button("artifact-find-prev", icons::ALT_ARROW_UP, has_matches)
                        .on_click(cx.listener(|this, _, _, cx| this.find_step(false, cx))),
                )
                .child(
                    button("artifact-find-next", icons::ALT_ARROW_DOWN, has_matches)
                        .on_click(cx.listener(|this, _, _, cx| this.find_step(true, cx))),
                )
                .child(
                    button("artifact-find-close", icons::CLOSE, true)
                        .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
                )
                .into_any_element(),
        )
    }

    /// `body` with the section rail beside it when the plan has an index.
    pub(super) fn with_section_index(
        &self,
        body: AnyElement,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.index_shown() {
            return body;
        }
        let rail = self.render_index(theme, window, cx);
        div()
            .size_full()
            .flex()
            .flex_row()
            .child(rail)
            .child(div().flex_1().min_w_0().h_full().relative().child(body))
            .into_any_element()
    }

    fn render_index(&self, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let index = self.section_index();
        if !self.nav.index_open {
            return div()
                .id("artifact-index")
                .debug_selector(|| "artifact-index".into())
                .flex_none()
                .w(px(28.0))
                .h_full()
                .pt(px(6.0))
                .flex()
                .flex_col()
                .items_center()
                .border_r_1()
                .border_color(theme.border)
                .child(
                    div()
                        .id("artifact-index-toggle")
                        .debug_selector(|| "artifact-index-toggle".into())
                        .size(px(20.0))
                        .rounded(px(surface_chrome::CONTROL_RADIUS))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|el| el.bg(crate::theme::wash(0.14)))
                        .child(
                            icons::icon(icons::LIST)
                                .size(px(12.0))
                                .text_color(theme.text_muted),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_index(cx))),
                )
                .into_any_element();
        }
        let active = self.current_section(cx);
        let rail_focused = self.nav.rail_focus.is_focused(window);
        let base = index.base_level();
        let rows = index.sections.iter().enumerate().map(|(ix, section)| {
            let selector = format!("artifact-index-row-{ix}");
            let is_active = active == Some(ix);
            div()
                .id(gpui::SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .flex_none()
                .w_full()
                .pl(px(
                    8.0 + 10.0 * f32::from(section.level.saturating_sub(base))
                ))
                .pr(px(8.0))
                .py(px(3.0))
                .rounded(px(4.0))
                .truncate()
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(11.5))
                .text_color(if is_active {
                    theme.text
                } else {
                    theme.text_muted
                })
                .when(section.level == base, |el| {
                    el.font_weight(gpui::FontWeight::MEDIUM)
                })
                .when(is_active, |el| {
                    el.bg(crate::theme::wash(if rail_focused { 0.20 } else { 0.10 }))
                })
                .hover(|el| el.bg(crate::theme::wash(0.14)))
                .child(gpui::SharedString::from(section.title.clone()))
                .on_click(cx.listener(move |this, _, _, cx| this.goto_section(ix, cx)))
        });
        div()
            .id("artifact-index")
            .debug_selector(|| "artifact-index".into())
            .track_focus(&self.nav.rail_focus)
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if let Some(web) = &mut this.web {
                        web.focus_chrome(cx);
                    }
                    window.focus(&this.nav.rail_focus, cx);
                }),
            )
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let mods = event.keystroke.modifiers;
                if mods.platform || mods.control || mods.alt || mods.shift {
                    return;
                }
                match event.keystroke.key.as_str() {
                    "up" => this.step_section(SectionStep::Previous, cx),
                    "down" => this.step_section(SectionStep::Next, cx),
                    "home" => this.step_section(SectionStep::First, cx),
                    "end" => this.step_section(SectionStep::Last, cx),
                    "enter" | "escape" => {
                        this.focus_document(window, cx);
                        true
                    }
                    _ => false,
                };
                cx.stop_propagation();
            }))
            .flex_none()
            .w(px(RAIL_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_none()
                    .h(px(28.0))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_faint)
                    .child("Sections")
                    .child(
                        div()
                            .id("artifact-index-toggle")
                            .debug_selector(|| "artifact-index-toggle".into())
                            .size(px(20.0))
                            .rounded(px(surface_chrome::CONTROL_RADIUS))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|el| el.bg(crate::theme::wash(0.14)))
                            .child(
                                icons::icon(icons::ALT_ARROW_LEFT)
                                    .size(px(11.0))
                                    .text_color(theme.text_muted),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_index(cx))),
                    ),
            )
            .child(
                div()
                    .id("artifact-index-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(4.0))
                    .pb(px(8.0))
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .children(rows),
            )
            .into_any_element()
    }
}
