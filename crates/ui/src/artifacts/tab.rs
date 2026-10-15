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
//! - options: the option list; each opens its fragment (side by side is
//!   phase 5).
//!
//! Content is read with `ArtifactContent` (forwarded to the owning device).
//! The web view is behind [`ArtifactWebHost`] so tests use a fake renderer.
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, SharedString, Subscription, Task, WeakEntity,
    Window, div, prelude::*, px,
};
use zeron_proto::{
    ArtifactContentParams, ArtifactContentReply, ArtifactKind, ArtifactPinParams,
    ArtifactPromoteParams, ArtifactView, TextDiffLine,
};

use super::actions::ArtifactUiError;
use super::document;
use super::model::{self, ArtifactBody, WebKind};
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
}

pub(crate) type WebHostFactory =
    Rc<dyn Fn(WeakEntity<ArtifactTab>, &mut Window, &mut App) -> Box<dyn ArtifactWebHost>>;

/// The product web host: an embedded Browser surface in static-document mode
/// with its own ephemeral website data store.
pub(crate) struct BrowserWebHost {
    browser: Entity<crate::browser::BrowserSurface>,
    _subscription: Subscription,
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
    Message(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Picker {
    Revisions,
    Compare,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CompareState {
    Loading,
    Ready {
        older: u32,
        newer: u32,
        lines: Vec<TextDiffLine>,
    },
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
enum Mode {
    Rendered,
    Source,
    Compare { other: u32, state: CompareState },
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
    _load: Option<Task<()>>,
    _side: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ArtifactTabEvent> for ArtifactTab {}

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
            if matches!(this.body, Some(ArtifactBody::Web { .. })) || this.option_shown.is_some() {
                let key = Some(render_key(cx));
                if this.rendered_key != key {
                    this.present(window, cx);
                }
            }
        });
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
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
            _load: None,
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
    }

    pub(crate) fn compare_state(&self) -> Option<&CompareState> {
        match &self.mode {
            Mode::Compare { state, .. } => Some(state),
            _ => None,
        }
    }

    /// The component the body renders with right now.
    pub(crate) fn presenter(&self) -> Presenter {
        match &self.mode {
            Mode::Source => return Presenter::Source,
            Mode::Compare { .. } => return Presenter::Compare,
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
        if matches!(self.mode, Mode::Compare { .. }) {
            self.mode = Mode::Rendered;
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
                self.image = Some(Arc::new(gpui::Image::from_bytes(
                    image_format(&mime_type),
                    bytes,
                )));
            }
            ArtifactBody::Options(_) | ArtifactBody::Text(_) | ArtifactBody::Invalid(_) => {}
        }
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
        let html = document::web_document(kind, source, &title, mime, &key.0, key.1, false);
        self.rendered_key = Some(key);
        if self.web.is_none() {
            let factory = self.web_factory.clone();
            self.web = Some(factory(cx.weak_entity(), window, cx));
        }
        if let Some(web) = &mut self.web {
            web.show(&title, html, window, cx);
        }
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

    /// "Compare with…": line diff between the shown revision and `other`
    /// (older on the left), reusing the instruction revision diff presenter.
    pub(crate) fn compare_with(&mut self, other: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        let Some(shown_text) = self.shown_text() else {
            self.notice = Some("Only text artifacts can be compared.".to_owned());
            cx.notify();
            return;
        };
        let shown = self
            .content
            .as_ref()
            .and_then(|reply| reply.as_ref().ok())
            .map(|reply| reply.revision)
            .or(self.shown)
            .unwrap_or(0);
        self.mode = Mode::Compare {
            other,
            state: CompareState::Loading,
        };
        let params = self.content_params(Some(other), None, cx);
        let call = self.state.read(cx).artifact_actions().content(params);
        self._side = Some(cx.spawn_in(window, async move |this, cx| {
            let reply = call.await;
            this.update(cx, |this, cx| {
                let Mode::Compare { other: wanted, .. } = this.mode else {
                    return;
                };
                if wanted != other {
                    return;
                }
                let state = match reply {
                    Err(error) => CompareState::Failed(error.text()),
                    Ok(reply) => match model::source_text(&reply) {
                        None => CompareState::Failed("Only text artifacts can be compared.".into()),
                        Some(other_text) => {
                            // `0` is the current file: newest.
                            let newer_is_shown = shown == 0 || shown > other;
                            let (older, newer, old_text, new_text) = if newer_is_shown {
                                (other, shown, other_text, shown_text.clone())
                            } else {
                                (shown, other, shown_text.clone(), other_text)
                            };
                            CompareState::Ready {
                                older,
                                newer,
                                lines: zeron_proto::diff_lines(&old_text, &new_text),
                            }
                        }
                    },
                };
                this.mode = Mode::Compare { other, state };
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(crate) fn close_compare(&mut self, cx: &mut Context<Self>) {
        self.mode = Mode::Rendered;
        cx.notify();
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
                    .children(linear),
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
            .when(text && artifact.revisions.len() > 1, |el| {
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
            })
            .child(div().flex_1())
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

    fn render_body(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
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
                    div()
                        .debug_selector(|| "artifact-markdown".into())
                        .size_full()
                        .child(preview)
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
            Presenter::Image => match &self.image {
                Some(image) => div()
                    .debug_selector(|| "artifact-image".into())
                    .size_full()
                    .p(px(12.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        gpui::img(image.clone())
                            .max_w_full()
                            .max_h_full()
                            .object_fit(gpui::ObjectFit::Contain),
                    )
                    .into_any_element(),
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
                            .child("Side-by-side comparison is coming later."),
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
            Presenter::Compare => {
                let Some(state) = self.compare_state() else {
                    return message(String::new());
                };
                let close = div()
                    .id("artifact-compare-close")
                    .debug_selector(|| "artifact-compare-close".into())
                    .flex_none()
                    .cursor_pointer()
                    .text_color(theme.text_muted)
                    .hover(|el| el.text_color(theme.text))
                    .child("Done")
                    .on_click(cx.listener(|this, _, _, cx| this.close_compare(cx)));
                let (title, content) = match state {
                    CompareState::Loading => ("Comparing\u{2026}".to_owned(), None),
                    CompareState::Failed(error) => (error.clone(), None),
                    CompareState::Ready {
                        older,
                        newer,
                        lines,
                    } => (
                        if *newer == 0 {
                            format!("r{older} \u{2192} current file")
                        } else {
                            format!("r{older} \u{2192} r{newer}")
                        },
                        Some(crate::work_outline::revision_diff::render_text_diff(
                            "artifact-compare-diff",
                            lines,
                            theme,
                        )),
                    ),
                };
                div()
                    .id("artifact-compare-view")
                    .size_full()
                    .overflow_y_scroll()
                    .p(px(12.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .text_size(crate::typography::ui_rems(11.5))
                            .child(
                                div()
                                    .flex_1()
                                    .text_color(theme.text)
                                    .child(SharedString::from(title)),
                            )
                            .child(close),
                    )
                    .children(content)
                    .into_any_element()
            }
        }
    }
}

impl gpui::Render for ArtifactTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let body = self.render_body(&theme, cx);
        root.child(header)
            .child(toolbar)
            .children(picker)
            .children(notice)
            .child(div().flex_1().min_h_0().relative().child(body))
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

/// A fake web renderer for tests: records every document it is asked to
/// show.
#[cfg(test)]
pub(crate) struct FakeWebHost {
    pub(crate) shown: Rc<std::cell::RefCell<Vec<(String, String)>>>,
}

#[cfg(test)]
impl FakeWebHost {
    pub(crate) fn factory(shown: Rc<std::cell::RefCell<Vec<(String, String)>>>) -> WebHostFactory {
        Rc::new(move |_, _, _| {
            Box::new(FakeWebHost {
                shown: shown.clone(),
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
}
