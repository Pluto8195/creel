//! The Artifact pane tab: header (title, role, revision and note, "edited
//! without a note", plan badge, the work item's Linear project when pinned),
//! a toolbar (revision picker, Compare with…, Open standalone, Reveal in
//! Finder, View source, Pin as plan / Unpin, Promote to docs/plans/), and the
//! body rendered by kind:
//!
//! - markdown: the native Markdown preview (`zeron-file:` and `path:line`
//!   links open files);
//! - htmlFragment / htmlPage / mermaid: the sandboxed web document
//!   ([`super::document`]) in an embedded Browser surface;
//! - image: a native image;
//! - options: the option list; each opens its fragment.
//!
//! "Compare" opens the kind-aware compare view ([`compare_view`]).
//!
//! Content is read with `ArtifactContent` (forwarded to the owning device).
//! The web view is behind [`ArtifactWebHost`] so tests use a fake renderer.
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, SharedString, Subscription, Task,
    WeakEntity, Window, div, prelude::*, px,
};
use zeron_proto::{
    ArtifactContentParams, ArtifactContentReply, ArtifactKind, ArtifactPinParams,
    ArtifactPromoteParams, ArtifactView, TextDiffLine,
};

pub(crate) mod compare_view;
pub(crate) mod find_view;
pub(crate) mod options_view;
pub(crate) mod plan_view;
pub(crate) mod questions_view;
mod zoom_view;
pub(crate) use compare_view::CompareView;
pub(crate) use find_view::{KEY_CONTEXT, bind_keys};

use super::actions::ArtifactUiError;
use super::compare;
use super::document;
use super::model::{self, ArtifactBody, WebKind};
use super::render_check::ArtifactRenderer;
use crate::browser::fragment::FragmentTheme;
use crate::state::AppState;
use crate::theme::Theme;
use crate::{icons, surface_chrome};

/// What the tab asks its owner (the Shell) to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArtifactTabEvent {
    /// Open a checkout-relative file at a line (Markdown links).
    OpenFile {
        path: String,
        line: Option<u32>,
        column: Option<u32>,
    },
    /// A web link inside the artifact.
    OpenUrl(String),
    /// Any other link activated inside the artifact, as written. The owner
    /// classifies it ([`super::links::classify`]) against `root` (the
    /// artifact's canonical checkout) and the chat's checkout.
    OpenLink { url: String, root: Option<String> },
    /// Title or state changed (tab strip label).
    Changed,
}

/// The web renderer behind fragments, pages, and Mermaid diagrams.
pub(crate) trait ArtifactWebHost {
    fn show(&mut self, title: &str, document: String, window: &mut Window, cx: &mut App);
    fn element(&self) -> AnyElement;
    /// The native surface, when there is one (the Shell drives its
    /// presentation like a Browser tab's).
    fn browser(&self) -> Option<Entity<crate::browser::BrowserSurface>> {
        None
    }
    /// Run a script in the page's outer document (find commands).
    fn run_script(&mut self, _script: &str, _cx: &mut App) {}
    /// Read the page's last report ([`super::find::READ_STATE_SCRIPT`]).
    fn read_state(&mut self, _cx: &mut App) -> Task<Option<String>> {
        Task::ready(None)
    }
    /// Take keyboard focus out of the page so a GPUI field can have it.
    fn focus_chrome(&mut self, _cx: &mut App) {}
    /// Give keyboard focus back to the page (plain arrows scroll it again).
    fn focus_page(&mut self, _cx: &mut App) {}
    /// Forward Escape from the page to GPUI (while the find bar is open).
    fn forward_escape(&mut self, _on: bool, _cx: &mut App) {}
    /// Whether scripts reach the page (find and the section index need it).
    fn supports_scripts(&self) -> bool {
        true
    }
}

pub(crate) type WebHostFactory =
    Rc<dyn Fn(WeakEntity<ArtifactTab>, &mut Window, &mut App) -> Box<dyn ArtifactWebHost>>;

/// The product web host: an embedded Browser surface in static-document mode
/// with its own ephemeral website data store.
pub(crate) struct BrowserWebHost {
    browser: Entity<crate::browser::BrowserSurface>,
    _subscription: Subscription,
}

/// The keys the page hands to GPUI besides the browser combos: ⌥↑ / ⌥↓ always
/// (section jumps), Escape while the find bar is open.
pub(crate) fn page_forwarded_keys(escape: bool) -> &'static [&'static str] {
    if escape {
        &[
            "alt-up",
            "alt-down",
            "escape",
            "cmd-=",
            "cmd-+",
            "cmd-shift-=",
            "cmd-shift-+",
            "cmd--",
            "cmd-0",
        ]
    } else {
        &[
            "alt-up",
            "alt-down",
            "cmd-=",
            "cmd-+",
            "cmd-shift-=",
            "cmd-shift-+",
            "cmd--",
            "cmd-0",
        ]
    }
}

impl BrowserWebHost {
    pub(crate) fn factory() -> WebHostFactory {
        Rc::new(|tab, window, cx| {
            let browser = cx.new(|cx| {
                let mut browser = crate::browser::BrowserSurface::new(
                    crate::browser::BrowserContext::default(),
                    false,
                    window,
                    cx,
                );
                browser.set_embedded(true, cx);
                browser.set_extra_forwarded_keys(page_forwarded_keys(false));
                browser
            });
            let subscription = window.subscribe(&browser, cx, move |_, event, _, cx| match event {
                crate::browser::BrowserEvent::NewTab(Some(url)) => {
                    let event = ArtifactTabEvent::OpenUrl(url.clone());
                    tab.update(cx, |_, cx| cx.emit(event)).ok();
                }
                crate::browser::BrowserEvent::DocumentLink(url) => {
                    tab.update(cx, |tab, cx| {
                        let event = ArtifactTabEvent::OpenLink {
                            url: url.clone(),
                            root: tab.canonical_root(cx),
                        };
                        cx.emit(event)
                    })
                    .ok();
                }
                crate::browser::BrowserEvent::Changed => {
                    tab.update(cx, |_, cx| cx.emit(ArtifactTabEvent::Changed))
                        .ok();
                }
                _ => {}
            });
            Box::new(BrowserWebHost {
                browser,
                _subscription: subscription,
            }) as Box<dyn ArtifactWebHost>
        })
    }
}

impl ArtifactWebHost for BrowserWebHost {
    fn show(&mut self, title: &str, document: String, window: &mut Window, cx: &mut App) {
        let title = title.to_owned();
        self.browser.update(cx, |browser, cx| {
            browser.show_document(title, document, window, cx)
        });
    }

    fn element(&self) -> AnyElement {
        self.browser.clone().into_any_element()
    }

    fn browser(&self) -> Option<Entity<crate::browser::BrowserSurface>> {
        Some(self.browser.clone())
    }

    fn run_script(&mut self, script: &str, cx: &mut App) {
        self.browser.read(cx).run_script(script);
    }

    fn read_state(&mut self, cx: &mut App) -> Task<Option<String>> {
        self.browser.update(cx, |browser, cx| {
            browser.read_script(super::find::READ_STATE_SCRIPT, cx)
        })
    }

    fn focus_chrome(&mut self, cx: &mut App) {
        self.browser.read(cx).focus_host_chrome();
    }

    fn forward_escape(&mut self, on: bool, cx: &mut App) {
        self.browser.update(cx, |browser, _| {
            browser.set_extra_forwarded_keys(page_forwarded_keys(on))
        });
    }

    fn focus_page(&mut self, cx: &mut App) {
        self.browser.read(cx).focus_page();
    }

    fn supports_scripts(&self) -> bool {
        // The script bridge is macOS-only for now.
        cfg!(target_os = "macos")
    }
}

/// Whether the Markdown preview hands `url` (a non-web link) to the artifact
/// router. Anchors, and relative paths that do not resolve, keep the
/// preview's own document-relative handling; everything else (files at a
/// line, artifacts, mail, blocked schemes) is routed.
pub(crate) fn markdown_link_is_routed(url: &str, root: Option<&str>) -> bool {
    use super::links::{ArtifactLink, BlockedReason, classify};
    match classify(url, root, None) {
        ArtifactLink::Anchor | ArtifactLink::Web(_) => false,
        ArtifactLink::Blocked(BlockedReason::Path) => url.trim().starts_with('/'),
        _ => true,
    }
}

/// Which component the body shows (tests read this).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Presenter {
    Loading,
    Markdown,
    Web(WebKind),
    Image,
    Options,
    Text,
    Source,
    Compare,
    /// 2-4 options in columns with pick buttons (phase 5 F).
    SideBySide,
    Message(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Picker {
    Revisions,
    Compare,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Rendered,
    Source,
    Compare,
    SideBySide,
}

pub(crate) struct ArtifactTab {
    state: Entity<AppState>,
    shell: Option<WeakEntity<crate::shell::Shell>>,
    pub(crate) artifact_id: String,
    /// The revision on screen; `None` = the current file.
    shown: Option<u32>,
    content: Option<Result<ArtifactContentReply, ArtifactUiError>>,
    body: Option<ArtifactBody>,
    loading: bool,
    mode: Mode,
    picker: Option<Picker>,
    notice: Option<String>,
    busy: bool,
    web: Option<Box<dyn ArtifactWebHost>>,
    web_factory: WebHostFactory,
    markdown: Option<Entity<crate::files::markdown_preview::MarkdownPreview>>,
    image: Option<Arc<gpui::Image>>,
    /// The option whose fragment is on screen (options kind).
    option_shown: Option<String>,
    rendered_key: Option<(FragmentTheme, bool)>,
    generation: u64,
    /// The open compare view (`Mode::Compare`).
    compare: Option<CompareView>,
    /// Renders both revisions offscreen for "Highlight changes" and onion
    /// skin: the platform's web renderer, or a fake in tests.
    renderer: Option<Rc<dyn ArtifactRenderer>>,
    /// ⌘F find and the section index (`tab/find_view.rs`).
    focus: FocusHandle,
    nav: find_view::FindNav,
    /// Plan question answers, drift and the changed-since marker.
    questions: questions_view::QuestionState,
    /// Canonical plan panel state (phase 5 H).
    plan: plan_view::PlanState,
    /// Options side by side and the pick (phase 5 F).
    side: options_view::SideBySide,
    /// Zoom (1.0 = 100%, fit for images), remembered per artifact.
    zoom: f32,
    /// The zoom the web page last reported (per mille), and whether the
    /// current document has been brought to `zoom`.
    zoom_reported: Option<u32>,
    zoom_synced: bool,
    image_scroll: gpui::ScrollHandle,
    /// A drag-pan in progress: the pointer and the scroll offset it began at.
    image_pan: Option<(gpui::Point<gpui::Pixels>, gpui::Point<gpui::Pixels>)>,
    /// The image's natural size, from its header.
    image_size: Option<(f32, f32)>,
    _load: Option<Task<()>>,
    _compare: Option<Task<()>>,
    _compare_render: Option<Task<()>>,
    _side: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ArtifactTabEvent> for ArtifactTab {}

type StateSlice = (Option<ArtifactView>, Option<String>, Option<usize>, Option<String>);

/// What the tab reads from `AppState` while rendering.
fn state_slice(state: &crate::state::AppState, artifact_id: &str) -> StateSlice {
    let artifact = state.artifacts().artifact(artifact_id).cloned();
    let outline = artifact.as_ref().and_then(|artifact| {
        state
            .work_outline(&artifact.space_id)
            .snapshot()
            .map(|snapshot| std::sync::Arc::as_ptr(snapshot) as usize)
    });
    (
        artifact,
        state.local_device_id.clone(),
        outline,
        state.artifacts().space_ids().next().map(str::to_owned),
    )
}

impl ArtifactTab {
    pub(crate) fn new(
        state: Entity<AppState>,
        shell: Option<WeakEntity<crate::shell::Shell>>,
        artifact_id: String,
        web_factory: WebHostFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let theme = cx.observe_global_in::<Theme>(window, |this, window, cx| {
            if this.compare_shows_web() {
                if this.rendered_key != Some(render_key(cx)) {
                    this.refresh_compare_theme(window, cx);
                }
            } else if this.side_shows_web() {
                if this.rendered_key != Some(render_key(cx)) {
                    this.refresh_side_theme(window, cx);
                }
            } else if this.mode != Mode::Compare && this.mode != Mode::SideBySide
                && (matches!(this.body, Some(ArtifactBody::Web { .. }))
                    || this.option_shown.is_some())
            {
                let key = Some(render_key(cx));
                if this.rendered_key != key {
                    this.present(window, cx);
                }
            }
        });
        // Repaint only when this artifact's metadata, the local device, its
        // Space's Outline snapshot or the first watched Space changed.
        let observe = cx.observe(&state, {
            let artifact_id = artifact_id.clone();
            let mut seen = None;
            move |_, state, cx| {
                let slice = state_slice(state.read(cx), &artifact_id);
                if seen.as_ref() != Some(&slice) {
                    seen = Some(slice);
                    cx.notify();
                }
            }
        });
        let remembered_zoom = super::zoom::ZoomMemory::recall(cx, &artifact_id);
        let mut tab = Self {
            state,
            shell,
            artifact_id,
            shown: None,
            content: None,
            body: None,
            loading: false,
            mode: Mode::Rendered,
            picker: None,
            notice: None,
            busy: false,
            web: None,
            web_factory,
            markdown: None,
            image: None,
            option_shown: None,
            rendered_key: None,
            generation: 0,
            compare: None,
            renderer: default_renderer(),
            focus: cx.focus_handle(),
            nav: find_view::FindNav::new(cx),
            questions: Default::default(),
            plan: Default::default(),
            side: Default::default(),
            zoom: remembered_zoom,
            zoom_reported: None,
            zoom_synced: false,
            image_scroll: gpui::ScrollHandle::new(),
            image_pan: None,
            image_size: None,
            _load: None,
            _compare: None,
            _compare_render: None,
            _side: None,
            _subscriptions: vec![theme, observe],
        };
        tab.load(None, window, cx);
        tab
    }

    /// The artifact's latest metadata from the watched snapshots.
    pub(crate) fn artifact(&self, cx: &App) -> Option<ArtifactView> {
        self.state
            .read(cx)
            .artifacts()
            .artifact(&self.artifact_id)
            .cloned()
    }

    /// The artifact's canonical checkout (the parent of `.zeron/`), the root
    /// its links resolve against first.
    pub(crate) fn canonical_root(&self, cx: &App) -> Option<String> {
        let artifact = self.artifact(cx)?;
        model::canonical_root(&artifact.stored_path).map(str::to_owned)
    }

    pub(crate) fn title(&self, cx: &App) -> SharedString {
        self.artifact(cx)
            .map(|artifact| artifact.title)
            .unwrap_or_else(|| "Artifact".to_owned())
            .into()
    }

    #[cfg(test)]
    pub(crate) fn state_for_tests(&self) -> Entity<AppState> {
        self.state.clone()
    }

    pub(crate) fn shown_revision(&self) -> Option<u32> {
        self.shown
    }

    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// The embedded web view, once one exists (it outlives mode switches).
    pub(crate) fn web_browser(&self) -> Option<Entity<crate::browser::BrowserSurface>> {
        self.web.as_ref().and_then(|web| web.browser())
    }

    /// Whether the body shows the web view right now (Source, Compare, and
    /// native kinds hide it).
    pub(crate) fn shows_web(&self) -> bool {
        matches!(self.presenter(), Presenter::Web(_))
            || self.compare_shows_web()
            || self.side_shows_web()
    }

    /// The component the body renders with right now.
    pub(crate) fn presenter(&self) -> Presenter {
        match self.mode {
            Mode::Source => return Presenter::Source,
            Mode::Compare => return Presenter::Compare,
            Mode::SideBySide => return Presenter::SideBySide,
            Mode::Rendered => {}
        }
        if self.option_shown.is_some() {
            return Presenter::Web(WebKind::Fragment);
        }
        match (&self.content, &self.body) {
            (None, _) if self.loading => Presenter::Loading,
            (Some(Err(error)), _) => Presenter::Message(error.text()),
            (_, Some(ArtifactBody::Markdown(_))) => Presenter::Markdown,
            (_, Some(ArtifactBody::Web { kind, .. })) => Presenter::Web(*kind),
            (_, Some(ArtifactBody::Image { .. })) => Presenter::Image,
            (_, Some(ArtifactBody::Options(_))) => Presenter::Options,
            (_, Some(ArtifactBody::Text(_))) => Presenter::Text,
            (_, Some(ArtifactBody::Invalid(message))) => Presenter::Message(message.clone()),
            _ if self.loading => Presenter::Loading,
            _ => Presenter::Message("Artifact not found".to_owned()),
        }
    }

    fn content_params(
        &self,
        revision: Option<u32>,
        companion: Option<String>,
        cx: &App,
    ) -> ArtifactContentParams {
        let state = self.state.read(cx);
        let device = state
            .artifacts()
            .artifact(&self.artifact_id)
            .map(|artifact| artifact.device_id.clone());
        let target_device_id =
            device.filter(|device| state.local_device_id.as_deref() != Some(device.as_str()));
        ArtifactContentParams {
            artifact_id: self.artifact_id.clone(),
            revision,
            companion,
            target_device_id,
        }
    }

    /// Load `revision` (`None` = the current file) and show it.
    pub(crate) fn load(
        &mut self,
        revision: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.generation += 1;
        let generation = self.generation;
        self.shown = revision;
        self.loading = true;
        self.picker = None;
        self.option_shown = None;
        if self.mode == Mode::SideBySide {
            self.mode = Mode::Rendered;
        }
        if self.mode == Mode::Compare {
            self.mode = Mode::Rendered;
            self.compare = None;
            self._compare = None;
            self._compare_render = None;
        }
        let params = self.content_params(revision, None, cx);
        let call = self.state.read(cx).artifact_actions().content(params);
        self._load = Some(cx.spawn_in(window, async move |this, cx| {
            let reply = call.await;
            this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                this.loaded(reply, window, cx);
            })
            .ok();
        }));
        cx.notify();
    }

    fn loaded(
        &mut self,
        reply: Result<ArtifactContentReply, ArtifactUiError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.loading = false;
        let kind = self
            .artifact(cx)
            .map(|artifact| artifact.kind)
            .unwrap_or_default();
        self.body = reply
            .as_ref()
            .ok()
            .map(|reply| model::body_for(kind, reply));
        self.content = Some(reply);
        self.present(window, cx);
        cx.emit(ArtifactTabEvent::Changed);
        cx.notify();
    }

    fn document_title(&self, cx: &App) -> String {
        self.title(cx).to_string()
    }

    /// Hand the loaded body to its component.
    fn present(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(body) = self.body.clone() else {
            return;
        };
        match body {
            ArtifactBody::Markdown(text) => {
                let preview = self.markdown_preview(cx);
                preview.update(cx, |preview, cx| preview.set_source(text, false, cx));
                self.apply_zoom_to_new_surface(cx);
            }
            ArtifactBody::Web { kind, source } => {
                let mime = self
                    .content
                    .as_ref()
                    .and_then(|reply| reply.as_ref().ok())
                    .map(|reply| reply.mime_type.clone())
                    .unwrap_or_default();
                self.show_web(kind, &source, &mime, window, cx);
            }
            ArtifactBody::Image { bytes, mime_type } => {
                self.image_size = zoom_view::natural_size(&bytes);
                self.image = Some(Arc::new(gpui::Image::from_bytes(
                    image_format(&mime_type),
                    bytes,
                )));
            }
            ArtifactBody::Options(_) | ArtifactBody::Text(_) | ArtifactBody::Invalid(_) => {}
        }
        self.document_changed(cx);
        cx.notify();
    }

    fn show_web(
        &mut self,
        kind: WebKind,
        source: &str,
        mime: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = render_key(cx);
        let title = self.document_title(cx);
        let source = super::question_html::with_answers(source, &self.questions.html_answers);
        let html = document::web_document(kind, &source, &title, mime, &key.0, key.1, false);
        self.rendered_key = Some(key);
        if self.web.is_none() {
            let factory = self.web_factory.clone();
            self.web = Some(factory(cx.weak_entity(), window, cx));
        }
        if let Some(web) = &mut self.web {
            web.show(&title, html, window, cx);
        }
        self.apply_zoom_to_new_surface(cx);
    }

    fn markdown_preview(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Entity<crate::files::markdown_preview::MarkdownPreview> {
        if let Some(preview) = &self.markdown {
            return preview.clone();
        }
        let stored = self
            .artifact(cx)
            .map(|artifact| artifact.stored_path)
            .unwrap_or_default();
        let path = model::markdown_document_path(&stored);
        let root = model::canonical_root(&stored).map(str::to_owned);
        let weak = cx.weak_entity();
        let preview = cx.new(|cx| {
            let open = weak.clone();
            let mut preview = crate::files::markdown_preview::MarkdownPreview::new(
                path,
                Rc::new(move |path, cx| {
                    open.update(cx, |_, cx| {
                        cx.emit(ArtifactTabEvent::OpenFile {
                            path,
                            line: None,
                            column: None,
                        })
                    })
                    .ok();
                }),
                cx,
            );
            let files = weak.clone();
            preview.set_file_link_handler(Rc::new(move |activation, cx| {
                let url = &activation.target.original;
                if !markdown_link_is_routed(url, root.as_deref()) {
                    return false;
                }
                let event = ArtifactTabEvent::OpenLink {
                    url: url.clone(),
                    root: root.clone(),
                };
                files.update(cx, |_, cx| cx.emit(event)).is_ok()
            }));
            let web = weak.clone();
            preview.set_web_link_handler(Rc::new(move |activation, cx| {
                if let Ok(url) = activation.target.navigation.clone() {
                    web.update(cx, |_, cx| cx.emit(ArtifactTabEvent::OpenUrl(url)))
                        .ok();
                }
            }));
            preview
        });
        self.markdown = Some(preview.clone());
        self.watch_markdown(&preview, cx);
        preview
    }

    // ------------------------------------------------------------------
    // Toolbar actions
    // ------------------------------------------------------------------

    pub(crate) fn toggle_picker(&mut self, picker: Picker, cx: &mut Context<Self>) {
        self.picker = if self.picker == Some(picker) {
            None
        } else {
            Some(picker)
        };
        cx.notify();
    }

    pub(crate) fn toggle_source(&mut self, cx: &mut Context<Self>) {
        self.mode = if self.mode == Mode::Source {
            Mode::Rendered
        } else {
            Mode::Source
        };
        cx.notify();
    }

    /// The text on screen (the shown revision).
    fn shown_text(&self) -> Option<String> {
        self.content
            .as_ref()?
            .as_ref()
            .ok()
            .and_then(model::source_text)
    }

    fn is_local(&self, artifact: &ArtifactView, cx: &App) -> bool {
        self.state.read(cx).local_device_id.as_deref() == Some(artifact.device_id.as_str())
    }

    /// The local file "Open standalone" opens: the preview export, else the
    /// page itself.
    pub(crate) fn standalone_path(&self, cx: &App) -> Option<String> {
        let artifact = self.artifact(cx)?;
        if !self.is_local(&artifact, cx) {
            return None;
        }
        artifact.preview_path.clone().or_else(|| {
            (artifact.kind == ArtifactKind::HtmlPage).then(|| artifact.stored_path.clone())
        })
    }

    fn open_standalone(&mut self, cx: &mut Context<Self>) {
        if let Some(url) = self
            .standalone_path(cx)
            .and_then(|path| crate::browser::file_url(std::path::Path::new(&path)))
        {
            cx.open_url(&url);
        }
    }

    fn reveal(&mut self, cx: &mut Context<Self>) {
        if let Some(artifact) = self.artifact(cx)
            && self.is_local(&artifact, cx)
        {
            cx.reveal_path(std::path::Path::new(&artifact.stored_path));
        }
    }

    pub(crate) fn set_pinned(&mut self, pinned: bool, cx: &mut Context<Self>) {
        let call = self
            .state
            .read(cx)
            .artifact_actions()
            .pin(ArtifactPinParams {
                artifact_id: self.artifact_id.clone(),
                pinned,
            });
        self.busy = true;
        self._side = Some(cx.spawn(async move |this, cx| {
            let reply = call.await;
            this.update(cx, |this, cx| {
                this.busy = false;
                this.notice = reply.err().map(|error| error.text());
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(crate) fn promote(&mut self, cx: &mut Context<Self>) {
        let Some(artifact) = self.artifact(cx) else {
            return;
        };
        let target_device_id = (!self.is_local(&artifact, cx)).then(|| artifact.device_id.clone());
        let call = self
            .state
            .read(cx)
            .artifact_actions()
            .promote(ArtifactPromoteParams {
                artifact_id: self.artifact_id.clone(),
                target_device_id,
            });
        self.busy = true;
        self._side = Some(cx.spawn(async move |this, cx| {
            let reply = call.await;
            this.update(cx, |this, cx| {
                this.busy = false;
                this.notice = Some(match reply {
                    Ok(reply) => format!("Copied to {} (uncommitted)", reply.promoted_path),
                    Err(error) => error.text(),
                });
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn show_option(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let params = self.content_params(None, Some(path.clone()), cx);
        let call = self.state.read(cx).artifact_actions().content(params);
        self._side = Some(cx.spawn_in(window, async move |this, cx| {
            let reply = call.await;
            this.update_in(cx, |this, window, cx| match reply {
                Ok(reply) => {
                    let source = model::source_text(&reply).unwrap_or_default();
                    this.option_shown = Some(path);
                    this.show_web(WebKind::Fragment, &source, "text/html", window, cx);
                    cx.notify();
                }
                Err(error) => {
                    this.notice = Some(error.text());
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn back_to_options(&mut self, cx: &mut Context<Self>) {
        self.option_shown = None;
        cx.notify();
    }

    // ------------------------------------------------------------------
    // Rendering
    // ------------------------------------------------------------------

    fn render_header(&self, artifact: &ArtifactView, theme: &Theme, cx: &App) -> AnyElement {
        let header = model::header(
            artifact,
            self.content
                .as_ref()
                .and_then(|reply| reply.as_ref().ok())
                .map(|reply| reply.revision)
                .or(self.shown),
        );
        let linear = header
            .pinned
            .then(|| {
                let item = artifact.work_item_id.as_deref()?;
                let load = self.state.read(cx).work_outline(&artifact.space_id);
                let reference = load.snapshot()?.linear_project_reference(item)?.clone();
                Some(
                    crate::work_outline::outline_view::linear_outline::project_chip(
                        "artifact-linear-project".into(),
                        reference
                            .cached_title
                            .clone()
                            .unwrap_or_else(|| "Linear project".into()),
                        reference.url.clone(),
                        self.shell.clone(),
                        theme,
                    ),
                )
            })
            .flatten();
        let revision_text = if header.revision == 0 {
            "Current file (not yet recorded)".to_owned()
        } else {
            model::revision_label(header.revision, header.note.as_deref())
        };
        div()
            .flex_none()
            .px(px(12.0))
            .py(px(8.0))
            .flex()
            .flex_col()
            .gap(px(3.0))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        icons::icon(model::kind_icon(artifact.kind))
                            .size(px(14.0))
                            .flex_none()
                            .text_color(theme.accent),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(13.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .debug_selector(|| "artifact-tab-title".into())
                            .child(SharedString::from(header.title.clone())),
                    )
                    .when_some(header.role.clone(), |el, role| {
                        el.child(chip(role, theme.text_muted, theme))
                    })
                    .when(header.pinned, |el| {
                        el.child(
                            div()
                                .debug_selector(|| "artifact-plan-badge".into())
                                .child(chip("Plan".into(), theme.accent, theme)),
                        )
                    })
                    .children(linear)
                    .children(self.picked_now(artifact).map(|pick| {
                        div()
                            .debug_selector(|| "artifact-header-picked".into())
                            .child(chip(Self::pick_label(&pick), theme.accent, theme))
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .debug_selector(|| "artifact-revision-line".into())
                            .child(SharedString::from(revision_text)),
                    )
                    .when(!header.current && header.revision != 0, |el| {
                        el.child(div().flex_none().text_color(theme.text_faint).child(
                            SharedString::from(format!(
                                "current is r{}",
                                artifact.current_revision
                            )),
                        ))
                    })
                    .when(header.edited_without_note, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(px(3.0))
                                .text_color(theme.warning)
                                .debug_selector(|| "artifact-edited-warning".into())
                                .child(icons::icon(icons::DANGER_TRIANGLE).size(px(11.0)))
                                .child("Edited without a note"),
                        )
                    })
                    .when_some(header.promoted_path.clone(), |el, path| {
                        el.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.text_faint)
                                .child(SharedString::from(format!("Promoted: {path}"))),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_toolbar(
        &self,
        artifact: &ArtifactView,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let local = self.is_local(artifact, cx);
        let standalone = self.standalone_path(cx).is_some();
        let text = !artifact.kind.is_binary();
        let compare_available = if artifact.kind == ArtifactKind::Options {
            matches!(&self.body, Some(ArtifactBody::Options(manifest)) if manifest.options.len() > 1)
        } else {
            artifact.revisions.len() > 1
        };
        let button = |id: &'static str, label: &'static str, glyph: &'static str, enabled: bool| {
            crate::files::toolbar_button(id, label)
                .debug_selector(move || id.into())
                .when(!enabled, |el| el.cursor_default().opacity(0.35))
                .child(
                    icons::icon(glyph)
                        .size(px(surface_chrome::ICON_SIZE))
                        .text_color(theme.text_muted),
                )
        };
        let shown = self
            .content
            .as_ref()
            .and_then(|reply| reply.as_ref().ok())
            .map(|reply| reply.revision)
            .or(self.shown)
            .unwrap_or(artifact.current_revision);
        let revision_label: SharedString = if shown == 0 {
            "Current".into()
        } else {
            format!("r{shown}").into()
        };
        let picker_button = |id: &'static str, label: SharedString, open: bool| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .flex_none()
                .h(px(surface_chrome::CONTROL_SIZE))
                .px(px(6.0))
                .rounded(px(surface_chrome::CONTROL_RADIUS))
                .flex()
                .items_center()
                .gap(px(3.0))
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_muted)
                .when(open, |el| el.bg(crate::theme::wash(0.10)))
                .hover(|el| el.bg(crate::theme::wash(0.14)).text_color(theme.text))
                .child(label)
                .child(icons::icon(icons::ALT_ARROW_DOWN).size(px(9.0)))
        };
        let pinned = artifact.pinned_as_plan;
        surface_chrome::toolbar(theme)
            .child(
                picker_button(
                    "artifact-revision-picker",
                    revision_label,
                    self.picker == Some(Picker::Revisions),
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_picker(Picker::Revisions, cx))),
            )
            .when(compare_available, |el| {
                if artifact.kind == ArtifactKind::Options {
                    el.child(
                        picker_button("artifact-side-by-side", "Side by side".into(), false)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_side_by_side(window, cx)),
                            ),
                    )
                    .child(
                        picker_button("artifact-compare", "Compare options".into(), false)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.compare_options(window, cx)),
                            ),
                    )
                } else {
                    el.child(
                        picker_button(
                            "artifact-compare",
                            "Compare with\u{2026}".into(),
                            self.picker == Some(Picker::Compare),
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| this.toggle_picker(Picker::Compare, cx)),
                        ),
                    )
                }
            })
            .child(div().flex_1())
            .children(self.render_zoom_control(theme, cx))
            .child(
                button(
                    "artifact-standalone",
                    "Open standalone",
                    icons::ARROW_UP_RIGHT,
                    standalone,
                )
                .when(standalone, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| this.open_standalone(cx)))
                }),
            )
            .child(
                button("artifact-reveal", "Reveal in Finder", icons::FOLDER, local)
                    .when(local, |el| {
                        el.on_click(cx.listener(|this, _, _, cx| this.reveal(cx)))
                    }),
            )
            .child(
                button("artifact-source", "View source", icons::FILE_CODE, text).when(text, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| this.toggle_source(cx)))
                }),
            )
            .child(
                button(
                    "artifact-pin",
                    if pinned { "Unpin plan" } else { "Pin as plan" },
                    icons::PIN,
                    !self.busy,
                )
                .when(pinned, |el| el.bg(crate::theme::wash(0.10)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.busy {
                        this.set_pinned(!pinned, cx)
                    }
                })),
            )
            .child(
                button(
                    "artifact-promote",
                    "Promote to docs/plans/",
                    icons::DOCUMENT_ADD,
                    !self.busy,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if !this.busy {
                        this.promote(cx)
                    }
                })),
            )
            .into_any_element()
    }

    fn render_picker(
        &self,
        artifact: &ArtifactView,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let picker = self.picker?;
        let now = model::now_ms();
        let shown = self.shown.unwrap_or(artifact.current_revision);
        let previous = (picker == Picker::Compare)
            .then(|| compare::default_pair(artifact, shown))
            .flatten()
            .map(|(older, _)| {
                div()
                    .id("artifact-compare-previous")
                    .debug_selector(|| "artifact-compare-previous".into())
                    .px(px(10.0))
                    .py(px(4.0))
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.text)
                    .hover(|el| el.bg(theme.element_hover))
                    .child(SharedString::from(format!("Previous revision (r{older})")))
                    .on_click(cx.listener(|this, _, window, cx| this.compare_previous(window, cx)))
            });
        let rows = artifact.revisions.iter().filter_map(|revision| {
            let id = revision.id;
            if picker == Picker::Compare && id == shown {
                return None;
            }
            let label = model::revision_label(
                id,
                Some(revision.note.as_str()).filter(|note| !note.trim().is_empty()),
            );
            let age = model::age_label(now, revision.at);
            let selector = match picker {
                Picker::Revisions => format!("artifact-revision-{id}"),
                Picker::Compare => format!("artifact-compare-{id}"),
            };
            Some(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .px(px(10.0))
                    .py(px(4.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.text)
                    .hover(|el| el.bg(theme.element_hover))
                    .when(picker == Picker::Revisions && id == shown, |el| {
                        el.bg(crate::theme::wash(0.08))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(label)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.text_faint)
                            .child(SharedString::from(age)),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| match picker {
                        Picker::Revisions => {
                            let current = this
                                .artifact(cx)
                                .is_some_and(|artifact| artifact.current_revision == id);
                            this.load(if current { None } else { Some(id) }, window, cx)
                        }
                        Picker::Compare => this.compare_with(id, window, cx),
                    })),
            )
        });
        Some(
            div()
                .flex_none()
                .max_h(px(220.0))
                .id("artifact-picker-list")
                .overflow_y_scroll()
                .py(px(4.0))
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.surface_raised)
                .children(previous)
                .children(rows)
                .into_any_element(),
        )
    }

    fn render_text(text: String, theme: &Theme, selector: &'static str) -> AnyElement {
        div()
            .id(selector)
            .debug_selector(move || selector.into())
            .size_full()
            .overflow_scroll()
            .p(px(12.0))
            .font_family(theme.font_mono.clone())
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text)
            .whitespace_nowrap()
            .children(
                text.lines()
                    .map(|line| div().child(SharedString::from(line.to_owned())))
                    .collect::<Vec<_>>(),
            )
            .into_any_element()
    }

    fn render_body(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let message = |text: String| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.0))
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted)
                .debug_selector(|| "artifact-message".into())
                .child(SharedString::from(text))
                .into_any_element()
        };
        match self.presenter() {
            Presenter::Loading => message("Loading\u{2026}".into()),
            Presenter::Message(text) => message(text),
            Presenter::Markdown => self
                .markdown
                .clone()
                .map(|preview| {
                    let weak = cx.weak_entity();
                    super::zoom::zoom_surface(
                        self.zoom,
                        true,
                        move |input, _, cx| {
                            weak.update(cx, |this, cx| this.apply_zoom_input(input, cx))
                                .ok();
                        },
                        div()
                            .debug_selector(|| "artifact-markdown".into())
                            .size_full()
                            .child(preview),
                    )
                    .into_any_element()
                })
                .unwrap_or_else(|| message("Loading\u{2026}".into())),
            Presenter::Web(_) => {
                let back = self.option_shown.as_ref().map(|path| {
                    div()
                        .id("artifact-options-back")
                        .debug_selector(|| "artifact-options-back".into())
                        .flex_none()
                        .px(px(12.0))
                        .py(px(4.0))
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .cursor_pointer()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_muted)
                        .hover(|el| el.text_color(theme.text))
                        .child(icons::icon(icons::ARROW_LEFT).size(px(11.0)))
                        .child(SharedString::from(format!("All options \u{b7} {path}")))
                        .on_click(cx.listener(|this, _, _, cx| this.back_to_options(cx)))
                });
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .children(back)
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .debug_selector(|| "artifact-web".into())
                            .children(self.web.as_ref().map(|web| web.element())),
                    )
                    .into_any_element()
            }
            Presenter::Image => match self.image.clone() {
                Some(image) => self.render_image_body(&image, window, cx),
                None => message("Loading\u{2026}".into()),
            },
            Presenter::Options => {
                let Some(ArtifactBody::Options(manifest)) = &self.body else {
                    return message("Loading\u{2026}".into());
                };
                let rows = manifest.options.iter().map(|option| {
                    let path = option.path.clone();
                    let selector = format!("artifact-option-{}", option.id);
                    div()
                        .id(SharedString::from(selector.clone()))
                        .debug_selector(move || selector.clone())
                        .px(px(10.0))
                        .py(px(6.0))
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(theme.border)
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .cursor_pointer()
                        .hover(|el| el.bg(theme.element_hover))
                        .child(
                            div()
                                .flex_none()
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(theme.text_faint)
                                .child(SharedString::from(option.id.clone())),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(crate::typography::ui_rems(12.5))
                                .text_color(theme.text)
                                .child(SharedString::from(if option.label.is_empty() {
                                    option.path.clone()
                                } else {
                                    option.label.clone()
                                })),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(theme.accent)
                                .child("Open"),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.show_option(path.clone(), window, cx)
                        }))
                });
                div()
                    .id("artifact-options")
                    .debug_selector(|| "artifact-options".into())
                    .size_full()
                    .overflow_y_scroll()
                    .p(px(12.0))
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .when(!manifest.title.is_empty(), |el| {
                        el.child(
                            div()
                                .text_size(crate::typography::ui_rems(13.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(SharedString::from(manifest.title.clone())),
                        )
                    })
                    .when_some(manifest.question.clone(), |el, question| {
                        el.child(
                            div()
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.text_muted)
                                .child(SharedString::from(question)),
                        )
                    })
                    .children(rows)
                    .child(
                        div()
                            .pt(px(4.0))
                            .text_size(crate::typography::ui_rems(10.5))
                            .text_color(theme.text_faint)
                            .child("Compare two options with \u{201c}Compare options\u{201d}."),
                    )
                    .into_any_element()
            }
            Presenter::Text => match &self.body {
                Some(ArtifactBody::Text(text)) => {
                    Self::render_text(text.clone(), theme, "artifact-text")
                }
                _ => message(String::new()),
            },
            Presenter::Source => match self.shown_text() {
                Some(text) => Self::render_text(text, theme, "artifact-source-view"),
                None => message("No text source for this artifact.".into()),
            },
            Presenter::Compare => self.render_compare(theme, cx),
            Presenter::SideBySide => self.render_side_by_side(theme, cx),
        }
    }
}

impl gpui::Render for ArtifactTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let artifact = self.artifact(cx);
        let root = div()
            .id("artifact-tab")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg);
        let Some(artifact) = artifact else {
            let text = match self.state.read(cx).artifacts().space_ids().next() {
                _ if self
                    .content
                    .as_ref()
                    .is_some_and(|reply| reply.as_ref().is_err_and(|e| e.unsupported)) =>
                {
                    super::store::UNSUPPORTED_TEXT.to_owned()
                }
                _ => "This artifact isn\u{2019}t in any watched project.".to_owned(),
            };
            return root.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .debug_selector(|| "artifact-message".into())
                    .child(SharedString::from(text)),
            );
        };
        self.track_questions(&artifact, window, cx);
        let badges = self.render_badges(&artifact, &theme, cx);
        let plan_linear = self.render_plan_linear(&artifact, &theme, cx);
        let header = self.render_header(&artifact, &theme, cx);
        let toolbar = self.render_toolbar(&artifact, &theme, cx);
        let picker = self.render_picker(&artifact, &theme, cx);
        let notice = self.notice.clone().map(|notice| {
            div()
                .flex_none()
                .px(px(12.0))
                .py(px(5.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .border_b_1()
                .border_color(theme.border)
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_muted)
                .debug_selector(|| "artifact-notice".into())
                .child(div().flex_1().min_w_0().child(SharedString::from(notice)))
                .child(
                    div()
                        .id("artifact-notice-dismiss")
                        .cursor_pointer()
                        .child(icons::icon(icons::CLOSE).size(px(10.0)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.notice = None;
                            cx.notify();
                        })),
                )
        });
        let body = self.render_body(&theme, window, cx);
        let find_bar = self.render_find_bar(&theme, cx);
        let body = self.with_section_index(body, &theme, window, cx);
        self.with_find_keys(root, cx)
            .on_action(cx.listener(|this, _: &super::zoom::ZoomIn, _, cx| {
                if !this.zoom_step(1, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &super::zoom::ZoomOut, _, cx| {
                if !this.zoom_step(-1, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &super::zoom::ZoomReset, _, cx| {
                if !this.zoom_reset(cx) {
                    cx.propagate();
                }
            }))
            .child(header)
            .children(badges)
            .children(plan_linear)
            .child(toolbar)
            .children(picker)
            .children(notice)
            .children(find_bar)
            .child(div().flex_1().min_h_0().relative().child(body))
    }
}

/// The platform's offscreen renderer (none in tests, which inject a fake).
fn default_renderer() -> Option<Rc<dyn ArtifactRenderer>> {
    #[cfg(test)]
    {
        None
    }
    #[cfg(not(test))]
    {
        super::render_check::platform_renderer()
    }
}

fn chip(text: String, color: gpui::Hsla, theme: &Theme) -> AnyElement {
    div()
        .flex_none()
        .px(px(5.0))
        .rounded(px(3.0))
        .border_1()
        .border_color(theme.border)
        .text_size(crate::typography::ui_rems(10.0))
        .text_color(color)
        .child(SharedString::from(text))
        .into_any_element()
}

fn render_key(cx: &App) -> (FragmentTheme, bool) {
    (
        FragmentTheme::from_theme(Theme::of(cx)),
        crate::motion::reduced_motion(cx),
    )
}

pub(crate) fn image_format(mime: &str) -> gpui::ImageFormat {
    match mime {
        "image/jpeg" => gpui::ImageFormat::Jpeg,
        "image/gif" => gpui::ImageFormat::Gif,
        "image/webp" => gpui::ImageFormat::Webp,
        "image/svg+xml" => gpui::ImageFormat::Svg,
        "image/bmp" => gpui::ImageFormat::Bmp,
        _ => gpui::ImageFormat::Png,
    }
}

/// What a [`FakeWebHost`] saw and answers: the scripts run in the page, the
/// state it reports back, how often chrome took focus, and whether Escape is
/// forwarded.
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct WebProbe {
    pub(crate) scripts: Rc<std::cell::RefCell<Vec<String>>>,
    pub(crate) state: Rc<std::cell::RefCell<Option<String>>>,
    pub(crate) chrome_focused: Rc<std::cell::Cell<u32>>,
    pub(crate) page_focused: Rc<std::cell::Cell<u32>>,
    pub(crate) escape_forwarded: Rc<std::cell::Cell<bool>>,
}

/// A fake web renderer for tests: records every document it is asked to
/// show.
#[cfg(test)]
pub(crate) struct FakeWebHost {
    pub(crate) shown: Rc<std::cell::RefCell<Vec<(String, String)>>>,
    pub(crate) probe: WebProbe,
}

#[cfg(test)]
impl FakeWebHost {
    pub(crate) fn factory(shown: Rc<std::cell::RefCell<Vec<(String, String)>>>) -> WebHostFactory {
        Self::probed_factory(shown, WebProbe::default())
    }

    pub(crate) fn probed_factory(
        shown: Rc<std::cell::RefCell<Vec<(String, String)>>>,
        probe: WebProbe,
    ) -> WebHostFactory {
        Rc::new(move |_, _, _| {
            Box::new(FakeWebHost {
                shown: shown.clone(),
                probe: probe.clone(),
            }) as Box<dyn ArtifactWebHost>
        })
    }
}

#[cfg(test)]
impl ArtifactWebHost for FakeWebHost {
    fn show(&mut self, title: &str, document: String, _: &mut Window, _: &mut App) {
        self.shown.borrow_mut().push((title.to_owned(), document));
    }

    fn element(&self) -> AnyElement {
        div()
            .debug_selector(|| "artifact-fake-web".into())
            .size_full()
            .into_any_element()
    }

    fn run_script(&mut self, script: &str, _: &mut App) {
        self.probe.scripts.borrow_mut().push(script.to_owned());
    }

    fn read_state(&mut self, _: &mut App) -> Task<Option<String>> {
        Task::ready(self.probe.state.borrow().clone())
    }

    fn focus_chrome(&mut self, _: &mut App) {
        self.probe
            .chrome_focused
            .set(self.probe.chrome_focused.get() + 1);
    }

    fn focus_page(&mut self, _: &mut App) {
        self.probe
            .page_focused
            .set(self.probe.page_focused.get() + 1);
    }

    fn forward_escape(&mut self, on: bool, _: &mut App) {
        self.probe.escape_forwarded.set(on);
    }
}
