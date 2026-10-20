//! The kind-aware revision compare view of the Artifact tab.
//!
//! - HTML fragment / page / Mermaid / image: both revisions rendered side by
//!   side at the same width in ONE sandboxed web document with synchronized
//!   scrolling ([`compare::compare_document`]); **Highlight changes** renders
//!   both offscreen at the same viewport (the render-check renderer), diffs the
//!   pixels ([`pixel_diff`]) and overlays boxes on the right pane; **Onion
//!   skin** stacks the two snapshots under an opacity slider;
//! - Markdown: both previews with changed blocks tinted and a section summary
//!   ([`markdown_diff`]);
//! - Options: companions are stored for the current copy only, so the compare
//!   is option against option (left and right option pickers), then as the
//!   visual kinds;
//! - every kind: a Source tab with the text line diff.
//!
//! The last mode chosen per kind is remembered ([`ComparePrefs`]).
use super::*;
use crate::artifacts::compare::{self, CompareMode, ComparePrefs, Flavor, OnionState, Side};
use crate::artifacts::document::{COMPARE_VIEWPORT_HEIGHT, COMPARE_VIEWPORT_WIDTH};
use crate::artifacts::markdown_diff::{self, BlockDiff, BlockStatus};
use crate::artifacts::pixel_diff::{self, DiffConfig, PixelDiff, Region};
use zeron_proto::ArtifactViewport;

/// One side of the compare, loaded.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SideContent {
    /// The revision (revision compare) the side shows; `0` = the current file.
    pub(crate) revision: u32,
    /// "r2 · note", "Current file", or the option's label.
    pub(crate) label: String,
    pub(crate) text: Option<String>,
    /// Base64 and MIME type of image content.
    pub(crate) image: Option<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompareOption {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompareData {
    pub(crate) left: SideContent,
    pub(crate) right: SideContent,
    /// The text diff (`None` for binary content).
    pub(crate) lines: Option<Vec<TextDiffLine>>,
    /// Markdown only.
    pub(crate) blocks: Option<BlockDiff>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CompareLoad {
    Loading,
    Failed(String),
    Ready(Box<CompareData>),
}

/// Both revisions rendered offscreen at the same viewport, and their diff.
pub(crate) struct Rendered {
    pub(crate) left: Arc<gpui::Image>,
    pub(crate) right: Arc<gpui::Image>,
    pub(crate) diff: PixelDiff,
}

pub(crate) enum RenderState {
    Idle,
    Rendering,
    Ready(Box<Rendered>),
    Failed(String),
}

pub(crate) struct CompareView {
    pub(crate) kind: ArtifactKind,
    pub(crate) flavor: Flavor,
    /// Revision compare: the revisions on the left and right (`0` = the
    /// current file).
    pub(crate) left: u32,
    pub(crate) right: u32,
    /// Options compare: the indices of the options on the left and right.
    pub(crate) option_pair: (usize, usize),
    pub(crate) options: Vec<CompareOption>,
    pub(crate) mode: CompareMode,
    pub(crate) highlight: bool,
    pub(crate) onion: OnionState,
    /// The side whose picker list is open.
    pub(crate) picking: Option<Side>,
    pub(crate) load: CompareLoad,
    pub(crate) renders: RenderState,
    pub(crate) markdown: Option<(
        Entity<crate::files::markdown_preview::MarkdownPreview>,
        Entity<crate::files::markdown_preview::MarkdownPreview>,
    )>,
    /// Scroll ratio (0..=1) last applied to the follower pane.
    synced_scroll: f32,
    generation: u64,
}

impl CompareView {
    pub(crate) fn data(&self) -> Option<&CompareData> {
        match &self.load {
            CompareLoad::Ready(data) => Some(data),
            _ => None,
        }
    }

    pub(crate) fn blocks(&self) -> Option<&BlockDiff> {
        self.data().and_then(|data| data.blocks.as_ref())
    }

    pub(crate) fn rendered(&self) -> Option<&Rendered> {
        match &self.renders {
            RenderState::Ready(rendered) => Some(rendered),
            _ => None,
        }
    }

    /// "N regions changed" / "No visual change", once rendered.
    pub(crate) fn render_summary(&self) -> Option<String> {
        self.rendered().map(|rendered| rendered.diff.summary())
    }

    pub(crate) fn regions(&self) -> &[Region] {
        self.rendered()
            .map(|rendered| rendered.diff.regions.as_slice())
            .unwrap_or(&[])
    }

    fn wants_renders(&self) -> bool {
        self.flavor.is_visual() && (self.highlight || self.mode == CompareMode::Onion)
    }
}

/// How a visual kind is wrapped for the web view.
fn web_kind(kind: ArtifactKind) -> Option<WebKind> {
    match kind {
        ArtifactKind::HtmlFragment | ArtifactKind::Options => Some(WebKind::Fragment),
        ArtifactKind::HtmlPage => Some(WebKind::Page),
        ArtifactKind::Mermaid => Some(WebKind::Mermaid),
        ArtifactKind::Image => Some(WebKind::Image),
        ArtifactKind::Markdown | ArtifactKind::Unknown => None,
    }
}

/// What the web wrapper is given for a side (the text, or an image's base64).
fn web_source(side: &SideContent) -> (String, String) {
    match &side.image {
        Some((base64, mime)) => (base64.clone(), mime.clone()),
        None => (
            side.text.clone().unwrap_or_default(),
            "text/html".to_owned(),
        ),
    }
}

fn png_image(bytes: Vec<u8>) -> Arc<gpui::Image> {
    Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes))
}

impl ArtifactTab {
    /// Replace the offscreen renderer (tests inject a fake).
    #[cfg(test)]
    pub(crate) fn set_renderer_for_tests(&mut self, renderer: Rc<dyn ArtifactRenderer>) {
        self.renderer = Some(renderer);
    }

    pub(crate) fn compare_view(&self) -> Option<&CompareView> {
        self.compare.as_ref()
    }

    /// Whether the compare body is the shared web view right now.
    pub(super) fn compare_shows_web(&self) -> bool {
        self.mode == Mode::Compare
            && self
                .compare
                .as_ref()
                .is_some_and(|view| view.flavor.is_visual() && view.mode == CompareMode::SideBySide)
    }

    /// The revision on screen (`0` = the current file).
    fn shown_or_current(&self, artifact: &ArtifactView) -> u32 {
        self.content
            .as_ref()
            .and_then(|reply| reply.as_ref().ok())
            .map(|reply| reply.revision)
            .or(self.shown)
            .unwrap_or(artifact.current_revision)
    }

    /// "Compare with…": the shown revision against `other` (older on the
    /// left).
    pub(crate) fn compare_with(&mut self, other: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        let Some(artifact) = self.artifact(cx) else {
            return;
        };
        let (older, newer) = compare::ordered(self.shown_or_current(&artifact), other);
        self.open_compare(&artifact, older, newer, window, cx);
    }

    /// The default compare: the revision before the shown one against it.
    pub(crate) fn compare_previous(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        let Some(artifact) = self.artifact(cx) else {
            return;
        };
        match compare::default_pair(&artifact, self.shown_or_current(&artifact)) {
            Some((older, newer)) => self.open_compare(&artifact, older, newer, window, cx),
            None => {
                self.notice = Some("There is no earlier revision to compare with.".to_owned());
                cx.notify();
            }
        }
    }

    /// Options: compare two of the manifest's options (the first two to
    /// start with).
    pub(crate) fn compare_options(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        let Some(artifact) = self.artifact(cx) else {
            return;
        };
        let options: Vec<CompareOption> = match &self.body {
            Some(ArtifactBody::Options(manifest)) => manifest
                .options
                .iter()
                .map(|option| CompareOption {
                    id: option.id.clone(),
                    label: if option.label.is_empty() {
                        option.path.clone()
                    } else {
                        option.label.clone()
                    },
                    path: option.path.clone(),
                })
                .collect(),
            _ => Vec::new(),
        };
        if options.len() < 2 {
            self.notice = Some("There are no two options to compare.".to_owned());
            cx.notify();
            return;
        }
        self.start_compare(&artifact, (0, 0), options, window, cx);
    }

    fn open_compare(
        &mut self,
        artifact: &ArtifactView,
        left: u32,
        right: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if artifact.kind == ArtifactKind::Options {
            return self.compare_options(window, cx);
        }
        self.start_compare(artifact, (left, right), Vec::new(), window, cx);
    }

    fn start_compare(
        &mut self,
        artifact: &ArtifactView,
        revisions: (u32, u32),
        options: Vec<CompareOption>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let kind = artifact.kind;
        let flavor = compare::flavor(kind);
        self.mode = Mode::Compare;
        self.compare = Some(CompareView {
            kind,
            flavor,
            left: revisions.0,
            right: revisions.1,
            option_pair: (0, 1),
            options,
            mode: ComparePrefs::mode_for(kind, cx),
            highlight: false,
            onion: OnionState::default(),
            picking: None,
            load: CompareLoad::Loading,
            renders: RenderState::Idle,
            markdown: None,
            synced_scroll: 0.0,
            generation: 0,
        });
        self.load_compare(window, cx);
    }

    pub(crate) fn close_compare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = Mode::Rendered;
        self.compare = None;
        self._compare = None;
        self._compare_render = None;
        // The shared web view showed the compare document.
        self.rendered_key = None;
        self.present(window, cx);
        cx.notify();
    }

    /// (Re)load both sides.
    fn load_compare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(artifact) = self.artifact(cx) else {
            return;
        };
        let Some(view) = self.compare.as_mut() else {
            return;
        };
        view.generation += 1;
        let generation = view.generation;
        view.load = CompareLoad::Loading;
        view.renders = RenderState::Idle;
        view.markdown = None;
        view.picking = None;
        self._compare_render = None;
        let (left, right, pair) = (view.left, view.right, view.option_pair);
        let options = view.options.clone();
        let flavor = view.flavor;
        let (params_left, params_right) = if flavor == Flavor::Options {
            let path = |ix: usize| options.get(ix).map(|option| option.path.clone());
            (
                self.content_params(None, path(pair.0), cx),
                self.content_params(None, path(pair.1), cx),
            )
        } else {
            (
                self.content_params(compare::revision_param(left), None, cx),
                self.content_params(compare::revision_param(right), None, cx),
            )
        };
        let labels = if flavor == Flavor::Options {
            let label = |ix: usize| {
                options
                    .get(ix)
                    .map(|option| option.label.clone())
                    .unwrap_or_default()
            };
            (label(pair.0), label(pair.1))
        } else {
            (
                revision_text(&artifact, left),
                revision_text(&artifact, right),
            )
        };
        let actions = self.state.read(cx).artifact_actions();
        let left_call = actions.content(params_left);
        let right_call = actions.content(params_right);
        let kind = artifact.kind;
        self._compare = Some(cx.spawn_in(window, async move |this, cx| {
            let (left_reply, right_reply) = futures::future::join(left_call, right_call).await;
            this.update_in(cx, |this, window, cx| {
                this.compare_loaded(
                    generation,
                    kind,
                    (left_reply, right_reply),
                    labels,
                    (left, right),
                    window,
                    cx,
                );
            })
            .ok();
        }));
        cx.notify();
    }

    #[allow(clippy::too_many_arguments)]
    fn compare_loaded(
        &mut self,
        generation: u64,
        kind: ArtifactKind,
        replies: (
            Result<ArtifactContentReply, ArtifactUiError>,
            Result<ArtifactContentReply, ArtifactUiError>,
        ),
        labels: (String, String),
        revisions: (u32, u32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.compare.as_mut() else {
            return;
        };
        if view.generation != generation {
            return;
        }
        let side = |reply: ArtifactContentReply, label: String, revision: u32| {
            let image_kind = if kind == ArtifactKind::Unknown {
                reply.kind
            } else {
                kind
            } == ArtifactKind::Image;
            SideContent {
                revision,
                label,
                text: model::source_text(&reply),
                image: (image_kind
                    && reply.encoding == zeron_proto::ArtifactContentEncoding::Base64)
                    .then(|| (reply.content.clone(), reply.mime_type.clone())),
            }
        };
        view.load = match replies {
            (Err(error), _) | (_, Err(error)) => CompareLoad::Failed(error.text()),
            (Ok(left), Ok(right)) => {
                let left = side(left, labels.0, revisions.0);
                let right = side(right, labels.1, revisions.1);
                let lines = match (&left.text, &right.text) {
                    (Some(old), Some(new)) => Some(zeron_proto::diff_lines(old, new)),
                    _ => None,
                };
                let blocks = (view.flavor == Flavor::Markdown).then(|| {
                    markdown_diff::diff_blocks(
                        left.text.as_deref().unwrap_or(""),
                        right.text.as_deref().unwrap_or(""),
                    )
                });
                CompareLoad::Ready(Box::new(CompareData {
                    left,
                    right,
                    lines,
                    blocks,
                }))
            }
        };
        self.refresh_compare(window, cx);
        cx.notify();
    }

    /// Bring what the current mode shows up to date with the loaded data.
    fn refresh_compare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.compare.as_ref() else {
            return;
        };
        if view.data().is_none() {
            return;
        }
        let (flavor, mode, wants_renders) = (view.flavor, view.mode, view.wants_renders());
        if flavor == Flavor::Markdown {
            if mode == CompareMode::SideBySide {
                self.prepare_markdown_compare(cx);
            }
            return;
        }
        if mode == CompareMode::SideBySide && flavor.is_visual() {
            self.show_compare_document(window, cx);
        }
        if wants_renders {
            self.ensure_renders(window, cx);
        }
    }

    fn prepare_markdown_compare(&mut self, cx: &mut Context<Self>) {
        let stored = self
            .artifact(cx)
            .map(|artifact| artifact.stored_path)
            .unwrap_or_default();
        let path = model::markdown_document_path(&stored);
        let Some(view) = self.compare.as_ref() else {
            return;
        };
        let Some(data) = view.data() else {
            return;
        };
        let (old_text, new_text) = (
            data.left.text.clone().unwrap_or_default(),
            data.right.text.clone().unwrap_or_default(),
        );
        let (old_status, new_status) = data
            .blocks
            .as_ref()
            .map(|blocks| (blocks.old_status.clone(), blocks.new_status.clone()))
            .unwrap_or_default();
        let existing = view.markdown.clone();
        let (left, right) = existing.unwrap_or_else(|| {
            let make = |side: Side, cx: &mut Context<Self>| {
                let path = path.clone();
                let preview = cx.new(|cx| {
                    crate::files::markdown_preview::MarkdownPreview::new(
                        path,
                        Rc::new(|_, _| {}),
                        cx,
                    )
                });
                // The list is mid-update while its scroll handler runs, so
                // the other pane follows from a deferred call.
                let tab = cx.weak_entity();
                preview
                    .read(cx)
                    .list_state()
                    .set_scroll_handler(move |_, _, cx| {
                        let tab = tab.clone();
                        cx.defer(move |cx| {
                            let _ = tab.update(cx, |tab, cx| tab.sync_markdown_scroll(side, cx));
                        });
                    });
                preview
            };
            (make(Side::Left, cx), make(Side::Right, cx))
        });
        let theme = Theme::of(cx).clone();
        let tints = |statuses: &[BlockStatus]| -> Vec<Option<gpui::Hsla>> {
            statuses
                .iter()
                .map(|status| match status {
                    BlockStatus::Same => None,
                    BlockStatus::Added => Some(theme.diff_add.opacity(0.16)),
                    BlockStatus::Removed => Some(theme.diff_del.opacity(0.16)),
                    BlockStatus::Changed => Some(theme.warning.opacity(0.16)),
                })
                .collect()
        };
        let (old_tints, new_tints) = (tints(&old_status), tints(&new_status));
        left.update(cx, |preview, cx| {
            preview.set_source(old_text, false, cx);
            preview.set_block_tints(old_tints, cx);
        });
        right.update(cx, |preview, cx| {
            preview.set_source(new_text, false, cx);
            preview.set_block_tints(new_tints, cx);
        });
        if let Some(view) = self.compare.as_mut() {
            view.markdown = Some((left, right));
        }
    }

    /// Move the pane opposite `from` to the same scroll position (as a
    /// fraction of its blocks). The ratio last applied is remembered so a
    /// scroll that moved nothing does not bounce back.
    pub(crate) fn sync_markdown_scroll(&mut self, from: Side, cx: &App) {
        let Some(view) = self.compare.as_mut() else {
            return;
        };
        let Some((left, right)) = view.markdown.as_ref() else {
            return;
        };
        let (left, right) = (left.read(cx).list_state(), right.read(cx).list_state());
        let (source, follower) = match from {
            Side::Left => (left, right),
            Side::Right => (right, left),
        };
        let top = source.logical_scroll_top();
        let frac = source
            .bounds_for_item(top.item_ix)
            .map(|bounds| f32::from(top.offset_in_item) / f32::from(bounds.size.height).max(1.0))
            .unwrap_or(0.0);
        let ratio = compare::scroll_ratio(top.item_ix, frac, source.item_count());
        if (ratio - view.synced_scroll).abs() < 1e-5 {
            return;
        }
        view.synced_scroll = ratio;
        let (item_ix, frac) = compare::scroll_target(ratio, follower.item_count());
        let height = follower
            .bounds_for_item(item_ix)
            .map(|bounds| f32::from(bounds.size.height))
            .unwrap_or(0.0);
        follower.scroll_to(gpui::ListOffset {
            item_ix,
            offset_in_item: px(frac * height),
        });
    }

    /// The compare document (both frames, the change overlay when
    /// highlighting) in the shared web view.
    fn show_compare_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.compare.as_ref() else {
            return;
        };
        let (Some(data), Some(kind)) = (view.data(), web_kind(view.kind)) else {
            return;
        };
        let key = render_key(cx);
        let title = self.document_title(cx);
        let (left_source, left_mime) = web_source(&data.left);
        let (right_source, right_mime) = web_source(&data.right);
        let left = document::compare_frame(
            kind,
            &left_source,
            &title,
            &left_mime,
            &key.0,
            key.1,
            compare::SYNC_FRAME_SCRIPT,
            "",
        );
        let mut right = document::compare_frame(
            kind,
            &right_source,
            &title,
            &right_mime,
            &key.0,
            key.1,
            compare::SYNC_FRAME_SCRIPT,
            "",
        );
        if view.highlight
            && let Some(rendered) = view.rendered()
        {
            let scale = rendered.diff.width as f32 / COMPARE_VIEWPORT_WIDTH as f32;
            let overlay =
                compare::overlay_html(&rendered.diff.regions, scale, right.origin, right.width);
            if !overlay.is_empty() {
                right = document::compare_frame(
                    kind,
                    &right_source,
                    &title,
                    &right_mime,
                    &key.0,
                    key.1,
                    compare::SYNC_FRAME_SCRIPT,
                    &overlay,
                );
            }
        }
        let html =
            compare::compare_document(&left, &right, (&data.left.label, &data.right.label), &key.0);
        self.rendered_key = Some(key);
        if self.web.is_none() {
            let factory = self.web_factory.clone();
            self.web = Some(factory(cx.weak_entity(), window, cx));
        }
        if let Some(web) = &mut self.web {
            web.show(&title, html, window, cx);
        }
    }

    /// Render both sides offscreen at the same viewport and diff them (once
    /// per load).
    fn ensure_renders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = render_key(cx);
        let title = self.document_title(cx);
        let renderer = self.renderer.clone();
        let Some(view) = self.compare.as_mut() else {
            return;
        };
        if !matches!(view.renders, RenderState::Idle) {
            return;
        }
        let (Some(kind), Some(data)) = (web_kind(view.kind), view.data()) else {
            return;
        };
        let Some(renderer) = renderer else {
            view.renders = RenderState::Failed(
                "Offscreen rendering isn\u{2019}t available on this platform.".to_owned(),
            );
            cx.notify();
            return;
        };
        let document = |side: &SideContent| {
            let (source, mime) = web_source(side);
            document::web_document(kind, &source, &title, &mime, &key.0, false, true)
        };
        let (left_doc, right_doc) = (document(&data.left), document(&data.right));
        view.renders = RenderState::Rendering;
        let generation = view.generation;
        let viewport = ArtifactViewport {
            width: COMPARE_VIEWPORT_WIDTH,
            height: COMPARE_VIEWPORT_HEIGHT,
        };
        self._compare_render = Some(cx.spawn_in(window, async move |this, cx| {
            let left_render = renderer.render(left_doc, viewport, cx);
            let right_render = renderer.render(right_doc, viewport, cx);
            let (left, right) = futures::future::join(left_render, right_render).await;
            let outcome: Result<(Vec<u8>, Vec<u8>, PixelDiff), String> = match (left, right) {
                (Err(error), _) | (_, Err(error)) => Err(error),
                (Ok(left), Ok(right)) => match (left.png, right.png) {
                    (Some(left), Some(right)) => {
                        let (a, b) = (left.clone(), right.clone());
                        cx.background_executor()
                            .spawn(
                                async move { pixel_diff::diff_png(&a, &b, DiffConfig::default()) },
                            )
                            .await
                            .map(|diff| (left, right, diff))
                    }
                    _ => Err("The web view returned no snapshot.".to_owned()),
                },
            };
            this.update_in(cx, |this, window, cx| {
                this.renders_ready(generation, outcome, window, cx)
            })
            .ok();
        }));
        cx.notify();
    }

    fn renders_ready(
        &mut self,
        generation: u64,
        outcome: Result<(Vec<u8>, Vec<u8>, PixelDiff), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.compare.as_mut() else {
            return;
        };
        if view.generation != generation {
            return;
        }
        view.renders = match outcome {
            Ok((left, right, diff)) => RenderState::Ready(Box::new(Rendered {
                left: png_image(left),
                right: png_image(right),
                diff,
            })),
            Err(error) => {
                RenderState::Failed(format!("Couldn\u{2019}t render the revisions ({error})"))
            }
        };
        if view.mode == CompareMode::SideBySide && view.highlight {
            self.show_compare_document(window, cx);
        }
        cx.notify();
    }

    // ------------------------------------------------------------------
    // Compare toolbar actions
    // ------------------------------------------------------------------

    pub(crate) fn set_compare_mode(
        &mut self,
        mode: CompareMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.compare.as_mut() else {
            return;
        };
        if !view.flavor.modes().contains(&mode) {
            return;
        }
        view.mode = mode;
        let kind = view.kind;
        ComparePrefs::remember(kind, mode, cx);
        self.refresh_compare(window, cx);
        cx.notify();
    }

    pub(crate) fn toggle_highlight(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.compare.as_mut() else {
            return;
        };
        if !view.flavor.is_visual() {
            return;
        }
        view.highlight = !view.highlight;
        if view.mode == CompareMode::SideBySide {
            self.show_compare_document(window, cx);
        }
        self.refresh_compare(window, cx);
        cx.notify();
    }

    pub(crate) fn set_onion_opacity(&mut self, opacity: f32, cx: &mut Context<Self>) {
        if let Some(view) = self.compare.as_mut() {
            view.onion.set(opacity);
            cx.notify();
        }
    }

    pub(crate) fn toggle_compare_picker(&mut self, side: Side, cx: &mut Context<Self>) {
        if let Some(view) = self.compare.as_mut() {
            view.picking = if view.picking == Some(side) {
                None
            } else {
                Some(side)
            };
            cx.notify();
        }
    }

    /// Choose the revision (revision compare) or option index (options) on
    /// `side` and reload.
    pub(crate) fn set_compare_target(
        &mut self,
        side: Side,
        target: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.compare.as_mut() else {
            return;
        };
        view.picking = None;
        let slot = match (view.flavor == Flavor::Options, side) {
            (false, Side::Left) => &mut view.left,
            (false, Side::Right) => &mut view.right,
            (true, _) => {
                let ix = target as usize;
                if ix >= view.options.len() {
                    return;
                }
                let pair = &mut view.option_pair;
                let slot = if side == Side::Left {
                    &mut pair.0
                } else {
                    &mut pair.1
                };
                if *slot == ix {
                    cx.notify();
                    return;
                }
                *slot = ix;
                return self.load_compare(window, cx);
            }
        };
        if *slot == target {
            cx.notify();
            return;
        }
        *slot = target;
        self.load_compare(window, cx);
    }

    /// The theme changed under an open compare.
    pub(super) fn refresh_compare_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.compare_shows_web() {
            self.show_compare_document(window, cx);
        }
    }

    // ------------------------------------------------------------------
    // Rendering
    // ------------------------------------------------------------------

    pub(super) fn render_compare(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(view) = self.compare.as_ref() else {
            return div().into_any_element();
        };
        let artifact = self.artifact(cx);
        let picker = artifact
            .as_ref()
            .and_then(|artifact| self.render_compare_picker(view, artifact, theme, cx));
        div()
            .id("artifact-compare-view")
            .size_full()
            .flex()
            .flex_col()
            .child(self.render_compare_toolbar(view, artifact.as_ref(), theme, cx))
            .children(picker)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_compare_body(view, theme, cx)),
            )
            .into_any_element()
    }

    fn side_title(view: &CompareView, artifact: Option<&ArtifactView>, side: Side) -> String {
        let (revision, option) = match side {
            Side::Left => (view.left, view.option_pair.0),
            Side::Right => (view.right, view.option_pair.1),
        };
        if view.flavor == Flavor::Options {
            view.options
                .get(option)
                .map(|option| option.label.clone())
                .unwrap_or_default()
        } else {
            artifact
                .map(|artifact| revision_text(artifact, revision))
                .unwrap_or_else(|| format!("r{revision}"))
        }
    }

    fn render_compare_toolbar(
        &self,
        view: &CompareView,
        artifact: Option<&ArtifactView>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let short = |side: Side| -> String {
            match (view.flavor == Flavor::Options, side) {
                (true, Side::Left) => view
                    .options
                    .get(view.option_pair.0)
                    .map(|option| option.id.clone())
                    .unwrap_or_default(),
                (true, Side::Right) => view
                    .options
                    .get(view.option_pair.1)
                    .map(|option| option.id.clone())
                    .unwrap_or_default(),
                (false, Side::Left) => short_revision(view.left),
                (false, Side::Right) => short_revision(view.right),
            }
        };
        let picker_button = |side: Side, id: &'static str| {
            compare_button(
                id,
                format!("{} \u{25be}", short(side)),
                view.picking == Some(side),
                theme,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_compare_picker(side, cx)))
        };
        let modes = view.flavor.modes().iter().copied().map(|mode| {
            compare_button(
                mode.selector(),
                mode.label(view.flavor),
                view.mode == mode,
                theme,
            )
            .on_click(
                cx.listener(move |this, _, window, cx| this.set_compare_mode(mode, window, cx)),
            )
        });
        let visual = view.flavor.is_visual() && view.mode != CompareMode::Source;
        let result: Option<String> =
            if visual && (view.highlight || view.mode == CompareMode::Onion) {
                Some(match &view.renders {
                    RenderState::Idle | RenderState::Rendering => "Rendering\u{2026}".to_owned(),
                    RenderState::Failed(error) => error.clone(),
                    RenderState::Ready(rendered) => rendered.diff.summary(),
                })
            } else {
                None
            };
        let _ = artifact;
        div()
            .flex_none()
            .px(px(12.0))
            .py(px(6.0))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.0))
            .border_b_1()
            .border_color(theme.border)
            .text_size(crate::typography::ui_rems(11.5))
            .text_color(theme.text_muted)
            .child(picker_button(Side::Left, "artifact-compare-left"))
            .child("\u{2192}")
            .child(picker_button(Side::Right, "artifact-compare-right"))
            .child(div().w(px(8.0)))
            .children(modes)
            .when(visual, |el| {
                el.child(div().w(px(8.0))).child(
                    compare_button(
                        "artifact-compare-highlight",
                        "Highlight changes",
                        view.highlight,
                        theme,
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_highlight(window, cx))),
                )
            })
            .when_some(result, |el, result| {
                el.child(
                    div()
                        .debug_selector(|| "artifact-compare-result".into())
                        .text_color(theme.text)
                        .child(SharedString::from(result)),
                )
            })
            .child(div().flex_1())
            .child(
                compare_button("artifact-compare-close", "Done", false, theme)
                    .on_click(cx.listener(|this, _, window, cx| this.close_compare(window, cx))),
            )
            .into_any_element()
    }

    fn render_compare_picker(
        &self,
        view: &CompareView,
        artifact: &ArtifactView,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let side = view.picking?;
        let (prefix, current) = match side {
            Side::Left => ("left", (view.left, view.option_pair.0)),
            Side::Right => ("right", (view.right, view.option_pair.1)),
        };
        let now = model::now_ms();
        let entries: Vec<(u32, String, String, String, bool)> = if view.flavor == Flavor::Options {
            view.options
                .iter()
                .enumerate()
                .map(|(ix, option)| {
                    (
                        ix as u32,
                        format!("artifact-compare-{prefix}-option-{}", option.id),
                        option.label.clone(),
                        option.id.clone(),
                        ix == current.1,
                    )
                })
                .collect()
        } else {
            let mut ids: Vec<u32> = artifact.revisions.iter().map(|r| r.id).collect();
            if current.0 == 0 {
                ids.insert(0, 0);
            }
            ids.into_iter()
                .map(|id| {
                    let age = artifact
                        .revision(id)
                        .map(|revision| model::age_label(now, revision.at))
                        .unwrap_or_default();
                    (
                        id,
                        format!("artifact-compare-{prefix}-r{id}"),
                        revision_text(artifact, id),
                        age,
                        id == current.0,
                    )
                })
                .collect()
        };
        let rows = entries
            .into_iter()
            .map(|(target, selector, label, age, selected)| {
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
                    .when(selected, |el| el.bg(crate::theme::wash(0.08)))
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
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_compare_target(side, target, window, cx)
                    }))
            });
        Some(
            div()
                .id("artifact-compare-picker-list")
                .flex_none()
                .max_h(px(220.0))
                .overflow_y_scroll()
                .py(px(4.0))
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.surface_raised)
                .children(rows)
                .into_any_element(),
        )
    }

    fn render_compare_body(
        &self,
        view: &CompareView,
        theme: &Theme,
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
                .debug_selector(|| "artifact-compare-message".into())
                .child(SharedString::from(text))
                .into_any_element()
        };
        let data = match &view.load {
            CompareLoad::Loading => return message("Comparing\u{2026}".to_owned()),
            CompareLoad::Failed(error) => return message(error.clone()),
            CompareLoad::Ready(data) => data,
        };
        match (view.mode, view.flavor) {
            (CompareMode::Source, _) => match &data.lines {
                Some(lines) => div()
                    .id("artifact-compare-source")
                    .size_full()
                    .overflow_y_scroll()
                    .p(px(12.0))
                    .child(crate::work_outline::revision_diff::render_text_diff(
                        "artifact-compare-diff",
                        lines,
                        theme,
                    ))
                    .into_any_element(),
                None => message("No text source for this artifact.".to_owned()),
            },
            (CompareMode::SideBySide, Flavor::Markdown) => {
                self.render_markdown_compare(view, data, theme)
            }
            (CompareMode::SideBySide, _) => div()
                .size_full()
                .debug_selector(|| "artifact-compare-web".into())
                .children(self.web.as_ref().map(|web| web.element()))
                .into_any_element(),
            (CompareMode::Onion, _) => self.render_onion(view, theme, cx),
        }
    }

    fn render_markdown_compare(
        &self,
        view: &CompareView,
        data: &CompareData,
        theme: &Theme,
    ) -> AnyElement {
        let summary = data
            .blocks
            .as_ref()
            .map(|blocks| blocks.summary.text())
            .unwrap_or_default();
        let pane = |label: &str,
                    preview: Option<&Entity<crate::files::markdown_preview::MarkdownPreview>>,
                    selector: &'static str| {
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex_none()
                        .px(px(12.0))
                        .py(px(3.0))
                        .truncate()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(label.to_owned())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .debug_selector(move || selector.into())
                        .children(preview.cloned()),
                )
        };
        let (left, right) = view
            .markdown
            .as_ref()
            .map(|(left, right)| (Some(left), Some(right)))
            .unwrap_or((None, None));
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px(px(12.0))
                    .py(px(5.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.text)
                    .debug_selector(|| "artifact-compare-summary".into())
                    .child(SharedString::from(summary)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .gap(px(1.0))
                    .child(pane(
                        &data.left.label,
                        left,
                        "artifact-compare-markdown-left",
                    ))
                    .child(pane(
                        &data.right.label,
                        right,
                        "artifact-compare-markdown-right",
                    )),
            )
            .into_any_element()
    }

    fn render_onion(
        &self,
        view: &CompareView,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(rendered) = view.rendered() else {
            let text = match &view.renders {
                RenderState::Failed(error) => error.clone(),
                _ => "Rendering\u{2026}".to_owned(),
            };
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted)
                .debug_selector(|| "artifact-compare-message".into())
                .child(SharedString::from(text))
                .into_any_element();
        };
        let opacity = view.onion.opacity();
        let steps =
            [0u32, 25, 50, 75, 100].into_iter().map(|percent| {
                compare_button(
                    SharedString::from(format!("artifact-compare-onion-{percent}")),
                    format!("{percent}%"),
                    view.onion.percent() == percent,
                    theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_onion_opacity(percent as f32 / 100.0, cx)
                }))
            });
        let layer = |image: &Arc<gpui::Image>, opacity: f32| {
            gpui::img(image.clone())
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .object_fit(gpui::ObjectFit::Fill)
                .opacity(opacity)
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px(px(12.0))
                    .py(px(5.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child(format!(
                        "{} \u{2192} {}",
                        short_revision(view.left),
                        short_revision(view.right)
                    ))
                    .child("Opacity of the newer render")
                    .children(steps)
                    .child(
                        div()
                            .debug_selector(|| "artifact-compare-onion-value".into())
                            .child(SharedString::from(format!("{}%", view.onion.percent()))),
                    ),
            )
            .child(
                div()
                    .id("artifact-compare-onion")
                    .debug_selector(|| "artifact-compare-onion".into())
                    .flex_1()
                    .min_h_0()
                    .overflow_scroll()
                    .p(px(12.0))
                    .child(
                        div()
                            .relative()
                            .w(px(COMPARE_VIEWPORT_WIDTH as f32))
                            .h(px(COMPARE_VIEWPORT_HEIGHT as f32))
                            .child(layer(&rendered.left, 1.0))
                            .child(layer(&rendered.right, opacity)),
                    ),
            )
            .into_any_element()
    }
}

/// "r2" / "Current".
fn short_revision(revision: u32) -> String {
    if revision == 0 {
        "Current".to_owned()
    } else {
        format!("r{revision}")
    }
}

/// "r2 · note" / "Current file".
fn revision_text(artifact: &ArtifactView, revision: u32) -> String {
    if revision == 0 {
        return "Current file".to_owned();
    }
    model::revision_label(
        revision,
        artifact
            .revision(revision)
            .map(|recorded| recorded.note.trim())
            .filter(|note| !note.is_empty()),
    )
}

fn compare_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    active: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let id: SharedString = id.into();
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex_none()
        .h(px(surface_chrome::CONTROL_SIZE))
        .px(px(7.0))
        .rounded(px(surface_chrome::CONTROL_RADIUS))
        .flex()
        .items_center()
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(if active { theme.text } else { theme.text_muted })
        .when(active, |el| el.bg(crate::theme::wash(0.12)))
        .hover(|el| el.bg(crate::theme::wash(0.14)).text_color(theme.text))
        .child(label.into())
}
