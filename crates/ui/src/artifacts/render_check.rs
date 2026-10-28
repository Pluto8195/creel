//! Render-check executor (design B): the headed UI keeps
//! `WatchArtifactRenderRequests` open and answers every request with
//! `ArtifactRenderResult`.
//!
//! Each request is rendered exactly as the Artifact tab renders it — the same
//! visualize wrapper, sandbox, and CSP ([`super::document`]), in the theme
//! the request names (else the current one) — plus the in-frame probe that
//! collects console errors, CSP violations, failed loads, Mermaid errors, an
//! empty render, and layout warnings. The page renders in an offscreen web
//! view and is captured with the web view's own snapshot API
//! (`WKWebView takeSnapshot` on macOS): no screen capture, no Screen
//! Recording permission. Other platforms answer `renderUnavailable`.
//!
//! At most [`RenderConfig::concurrency`] offscreen views run at once; each
//! request is answered within [`RenderConfig::timeout`] (below the engine's
//! 30 s), failures included.
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

use futures::StreamExt as _;
use gpui::{AsyncApp, Context, Task, WeakEntity};
use zeron_proto::artifacts::{ArtifactErrorCode, artifact_limits};
use zeron_proto::{
    ArtifactContentEncoding, ArtifactError, ArtifactKind, ArtifactRenderCheckResult,
    ArtifactRenderRequest, ArtifactRenderResultParams, ArtifactRenderScreenshot,
    ArtifactRenderTheme, ArtifactRenderViewport, ArtifactViewport,
};
use zeron_rpc::{RpcError, methods};

use super::document::{self, ProbeReport};
use super::layout_heuristics;
use super::model::{self, WebKind};
use crate::browser::fragment::FragmentTheme;
use crate::state::{AppState, EngineHandle};
use crate::theme::Theme;

/// What an offscreen render produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RenderOutput {
    pub(crate) png: Option<Vec<u8>>,
    /// The probe's report; `None` when it never reported.
    pub(crate) report: Option<ProbeReport>,
}

pub(crate) type RenderFuture = Pin<Box<dyn Future<Output = Result<RenderOutput, String>>>>;

/// Renders a complete host document offscreen at `viewport` and snapshots
/// it. The macOS implementation is [`webkit::WebKitRenderer`]; tests use a
/// fake.
pub(crate) trait ArtifactRenderer {
    fn render(
        &self,
        document: String,
        viewport: ArtifactViewport,
        cx: &mut AsyncApp,
    ) -> RenderFuture;

    /// Like [`Self::render`], but the snapshot grows past `viewport` to the
    /// document's full height (see [`document::compare_render_plan`]).
    /// Renderers that cannot grow just render the viewport.
    fn render_full_height(
        &self,
        document: String,
        viewport: ArtifactViewport,
        cx: &mut AsyncApp,
    ) -> RenderFuture {
        self.render(document, viewport, cx)
    }
}

/// Executor limits.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RenderConfig {
    /// Answer within this (the engine waits 30 s).
    pub(crate) timeout: Duration,
    /// Offscreen views at once.
    pub(crate) concurrency: usize,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(artifact_limits::RENDER_TIMEOUT_SECS.saturating_sub(5)),
            concurrency: 2,
        }
    }
}

/// The platform renderer, or `None` where there is no offscreen web view.
pub(crate) fn platform_renderer() -> Option<Rc<dyn ArtifactRenderer>> {
    #[cfg(target_os = "macos")]
    {
        Some(Rc::new(webkit::WebKitRenderer))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// The document a request renders as, or why it renders natively (no web
/// view: Markdown and unknown kinds). Pure.
pub(crate) fn request_document(
    request: &ArtifactRenderRequest,
    theme: &FragmentTheme,
) -> Result<String, RenderSkip> {
    let text = || match request.encoding {
        ArtifactContentEncoding::Utf8 => Ok(request.content.clone()),
        ArtifactContentEncoding::Base64 => model::decode_base64(&request.content)
            .and_then(|bytes| String::from_utf8(bytes).map_err(|_| "not UTF-8 text".into()))
            .map_err(RenderSkip::Invalid),
    };
    let title = if request.title.trim().is_empty() {
        request
            .path
            .rsplit('/')
            .next()
            .unwrap_or("Artifact")
            .to_owned()
    } else {
        request.title.clone()
    };
    let kind = match request.kind {
        ArtifactKind::Unknown => ArtifactKind::from_path(&request.path),
        kind => kind,
    };
    let web = |kind: WebKind, source: &str, mime: &str| {
        document::web_document(kind, source, &title, mime, theme, false, true)
    };
    match kind {
        ArtifactKind::HtmlFragment => Ok(web(WebKind::Fragment, &text()?, "text/html")),
        ArtifactKind::HtmlPage => Ok(web(WebKind::Page, &text()?, "text/html")),
        ArtifactKind::Mermaid => Ok(web(WebKind::Mermaid, &text()?, "text/plain")),
        ArtifactKind::Image => {
            let base64 = match request.encoding {
                ArtifactContentEncoding::Base64 => request.content.clone(),
                ArtifactContentEncoding::Utf8 => {
                    use base64::Engine as _;
                    base64::engine::general_purpose::STANDARD.encode(request.content.as_bytes())
                }
            };
            Ok(web(
                WebKind::Image,
                &base64,
                zeron_proto::artifacts::mime_type_for(&request.path),
            ))
        }
        ArtifactKind::Options => {
            // Every option's fragment, stacked under its label (the tab shows
            // one at a time; side by side is phase 5).
            let manifest = zeron_proto::artifacts::OptionsManifest::parse(&text()?)
                .map_err(|error| RenderSkip::Invalid(error.message))?;
            let mut body = String::new();
            for option in &manifest.options {
                let fragment = request
                    .companions
                    .iter()
                    .find(|companion| companion.path == option.path)
                    .map(|companion| companion.content.as_str())
                    .unwrap_or("");
                let label = if option.label.is_empty() {
                    &option.id
                } else {
                    &option.label
                };
                body.push_str(&format!(
                    "<section data-zeron-option=\"{}\"><h3>{}</h3>\n{}\n</section>\n",
                    crate::browser::fragment::escape_attr(&option.id),
                    crate::browser::fragment::escape_text(label),
                    fragment
                ));
            }
            Ok(web(WebKind::Fragment, &body, "text/html"))
        }
        ArtifactKind::Markdown | ArtifactKind::Unknown => Err(RenderSkip::Native),
    }
}

/// Why a request has no web render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RenderSkip {
    /// Rendered natively by Mast (Markdown): nothing to snapshot.
    Native,
    Invalid(String),
}

fn wire_error(code: ArtifactErrorCode, message: impl Into<String>) -> String {
    ArtifactError::new(code, message).to_wire_message()
}

/// A theme a request renders in: what was asked, what it resolves to, and
/// the tokens for the wrapper.
#[derive(Debug, Clone)]
pub(crate) struct ThemeChoice {
    pub(crate) requested: ArtifactRenderTheme,
    /// `light` or `dark`.
    pub(crate) label: &'static str,
    pub(crate) theme: FragmentTheme,
}

/// Serve one request: build each viewport × theme document, render them one
/// after another with `renderer` within `timeout` overall, and turn the
/// outcome into the `ArtifactRenderResult` params. `themes` holds a choice for
/// every theme the request names. `sleep` is the timeout clock (injected for
/// tests).
pub(crate) async fn serve<Sleep, SleepFut>(
    request: ArtifactRenderRequest,
    renderer: Option<Rc<dyn ArtifactRenderer>>,
    themes: Vec<ThemeChoice>,
    timeout: Duration,
    sleep: Sleep,
    cx: &mut AsyncApp,
) -> ArtifactRenderResultParams
where
    Sleep: FnOnce(Duration) -> SleepFut,
    SleepFut: Future<Output = ()>,
{
    let request_id = request.request_id.clone();
    let failed = |error: String| ArtifactRenderResultParams {
        request_id: request_id.clone(),
        result: None,
        error: Some(error),
    };
    let Some(renderer) = renderer else {
        return failed(wire_error(
            ArtifactErrorCode::RenderUnavailable,
            "this Mast window cannot render artifacts offscreen on this platform",
        ));
    };
    let (plan, cap_warning) = request.renders();
    let mut renders = Vec::new();
    for (viewport, requested) in &plan {
        let Some(choice) = themes
            .iter()
            .find(|choice| choice.requested == *requested)
            .or(themes.first())
        else {
            return failed(wire_error(
                ArtifactErrorCode::RenderFailed,
                "no theme to render in",
            ));
        };
        match request_document(&request, &choice.theme) {
            Ok(document) => renders.push((viewport.clone(), choice.label, document)),
            Err(RenderSkip::Native) => {
                return ArtifactRenderResultParams {
                    request_id,
                    result: Some(ArtifactRenderCheckResult {
                        warnings: vec![
                            "Markdown renders natively in Mast; there is no web render to check"
                                .to_owned(),
                        ],
                        ..Default::default()
                    }),
                    error: None,
                };
            }
            Err(RenderSkip::Invalid(message)) => {
                return failed(wire_error(ArtifactErrorCode::RenderFailed, message));
            }
        }
    }
    let renderer = &renderer;
    let all = async {
        let mut finished = Vec::new();
        for (viewport, theme, document) in renders {
            let output = renderer.render(document, viewport.size(), cx).await;
            finished.push((viewport, theme, output));
        }
        finished
    };
    futures::pin_mut!(all);
    let deadline = sleep(timeout);
    futures::pin_mut!(deadline);
    let outcome = match futures::future::select(all, deadline).await {
        futures::future::Either::Left((finished, _)) => Some(finished),
        futures::future::Either::Right(((), _)) => None,
    };
    let Some(finished) = outcome else {
        return failed(wire_error(
            ArtifactErrorCode::RenderTimeout,
            format!("the render did not finish within {} s", timeout.as_secs()),
        ));
    };
    let mut outputs = Vec::new();
    for (viewport, theme, output) in finished {
        match output {
            Ok(output) => outputs.push((viewport, theme, output)),
            Err(message) => return failed(wire_error(ArtifactErrorCode::RenderFailed, message)),
        }
    }
    let mut result = check_results(outputs);
    result.warnings.extend(cap_warning);
    ArtifactRenderResultParams {
        request_id,
        result: Some(result),
        error: None,
    }
}

/// The agent-facing result of one finished render. Pure.
pub(crate) fn check_result(output: RenderOutput) -> ArtifactRenderCheckResult {
    check_results(vec![(
        ArtifactRenderViewport {
            width: 0,
            height: 0,
            label: None,
        },
        "",
        output,
    )])
}

/// The agent-facing result of every finished render (viewport, theme label,
/// output). Errors and warnings repeated across renders are reported once;
/// layout findings carry their viewport and theme. Pure.
pub(crate) fn check_results(
    outputs: Vec<(ArtifactRenderViewport, &'static str, RenderOutput)>,
) -> ArtifactRenderCheckResult {
    use base64::Engine as _;
    let many = outputs.len() > 1;
    let mut result = ArtifactRenderCheckResult::default();
    let mut layout_dropped = false;
    let mut add = |list: &mut Vec<String>, text: String| {
        if !list.contains(&text) {
            list.push(text);
        }
    };
    for (viewport, theme, output) in outputs {
        let label = viewport.display_label();
        let at = if many {
            format!(" [{label}, {theme}]")
        } else {
            String::new()
        };
        match output.report {
            Some(report) => {
                for error in report.errors {
                    add(&mut result.errors, error);
                }
                for warning in report.warnings {
                    add(&mut result.warnings, format!("{warning}{at}"));
                }
                let raw = layout_heuristics::classify(&report.metrics, viewport.size().width);
                layout_dropped |=
                    layout_heuristics::tag(&mut result.layout_warnings, raw, &label, theme);
            }
            None => add(
                &mut result.errors,
                "The render probe did not report: the page's scripts may have failed or never finished loading"
                    .to_owned(),
            ),
        }
        match output.png {
            Some(png) => {
                let data = base64::engine::general_purpose::STANDARD.encode(png);
                if result.screenshot_png_base64.is_none() {
                    result.screenshot_png_base64 = Some(data.clone());
                }
                let size = viewport.size();
                result.screenshots.push(ArtifactRenderScreenshot {
                    viewport: label,
                    width: size.width,
                    height: size.height,
                    theme: theme.to_owned(),
                    png_base64: data,
                });
            }
            None => add(
                &mut result.errors,
                format!("No screenshot could be taken{at}"),
            ),
        }
    }
    if layout_dropped {
        result.warnings.push(format!(
            "More than {} layout findings; only the first are listed",
            zeron_proto::artifacts::artifact_limits::MAX_LAYOUT_WARNINGS
        ));
    }
    result
}

/// The themes a request renders in (each distinct one it plans).
pub(crate) fn request_themes(request: &ArtifactRenderRequest, cx: &gpui::App) -> Vec<ThemeChoice> {
    let mut choices: Vec<ThemeChoice> = Vec::new();
    for (_, requested) in request.renders().0 {
        if choices.iter().all(|choice| choice.requested != requested) {
            choices.push(ThemeChoice {
                requested,
                label: if request_theme_is_dark(requested, cx) {
                    "dark"
                } else {
                    "light"
                },
                theme: request_theme(requested, cx),
            });
        }
    }
    choices
}

fn request_theme_is_dark(theme: ArtifactRenderTheme, cx: &gpui::App) -> bool {
    match theme {
        ArtifactRenderTheme::Dark => true,
        ArtifactRenderTheme::Light => false,
        ArtifactRenderTheme::System => Theme::of(cx).appearance.is_dark(),
    }
}

/// The theme a request renders in: its own, else the current one.
pub(crate) fn request_theme(theme: ArtifactRenderTheme, cx: &gpui::App) -> FragmentTheme {
    let current = Theme::of(cx);
    match theme {
        ArtifactRenderTheme::Dark if !current.appearance.is_dark() => {
            FragmentTheme::from_theme(&Theme::dark())
        }
        ArtifactRenderTheme::Light if current.appearance.is_dark() => {
            FragmentTheme::from_theme(&Theme::light())
        }
        _ => FragmentTheme::from_theme(current),
    }
}

/// Keep `WatchArtifactRenderRequests` open for as long as the engine is
/// attached and serve every request on a bounded worker pool. A host
/// without the method (`unknown method`) stops the loop.
pub(crate) fn spawn_render_requests(
    cx: &mut Context<AppState>,
    handle: EngineHandle,
    renderer: Option<Rc<dyn ArtifactRenderer>>,
    config: RenderConfig,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        let (jobs, queue) = futures::channel::mpsc::unbounded::<ArtifactRenderRequest>();
        let queue = Rc::new(futures::lock::Mutex::new(queue));
        let mut workers = Vec::new();
        for _ in 0..config.concurrency.max(1) {
            let queue = queue.clone();
            let renderer = renderer.clone();
            let this = this.clone();
            workers.push(cx.spawn(async move |cx| {
                loop {
                    let next = queue.lock().await.next().await;
                    let Some(request) = next else {
                        return;
                    };
                    if !serve_and_answer(&this, request, renderer.clone(), config, cx).await {
                        return;
                    }
                }
            }));
        }
        let mut attempt: u32 = 0;
        loop {
            let params = serde_json::json!({ "client": "zeron-ui" });
            let mut subscription = match handle
                .client()
                .subscribe_checked(methods::WATCH_ARTIFACT_RENDER_REQUESTS, params)
                .await
            {
                Ok(subscription) => subscription,
                Err(RpcError::UnknownMethod(_)) => {
                    tracing::debug!("artifact render requests unsupported by this engine");
                    return;
                }
                Err(err) => {
                    tracing::debug!(error = %err, "artifact render requests unavailable");
                    if this.update(cx, |_, _| {}).is_err() {
                        return;
                    }
                    let seconds = 2u64.saturating_mul(1u64 << attempt.min(4)).min(30);
                    cx.background_executor()
                        .timer(Duration::from_secs(seconds))
                        .await;
                    attempt = attempt.saturating_add(1);
                    continue;
                }
            };
            attempt = 0;
            while let Some(value) = subscription.recv().await {
                match serde_json::from_value::<ArtifactRenderRequest>(value) {
                    Ok(request) => {
                        if jobs.unbounded_send(request).is_err() {
                            return;
                        }
                    }
                    Err(err) => tracing::warn!(error = %err, "dropping malformed render request"),
                }
            }
            if this.update(cx, |_, _| {}).is_err() {
                return;
            }
            cx.background_executor().timer(Duration::from_secs(2)).await;
        }
    })
}

/// Serve one request and post its result; `false` once the state is gone.
async fn serve_and_answer(
    this: &WeakEntity<AppState>,
    request: ArtifactRenderRequest,
    renderer: Option<Rc<dyn ArtifactRenderer>>,
    config: RenderConfig,
    cx: &mut AsyncApp,
) -> bool {
    let Ok(themes) = this.update(cx, |_, cx| request_themes(&request, cx)) else {
        return false;
    };
    let executor = cx.background_executor().clone();
    let params = serve(
        request,
        renderer,
        themes,
        config.timeout,
        move |duration| executor.timer(duration),
        cx,
    )
    .await;
    let Ok(call) = this.update(cx, |state, _| {
        state.artifact_actions().render_result(params)
    }) else {
        return false;
    };
    if let Err(error) = call.await {
        tracing::warn!(error = %error.text(), "posting an artifact render result failed");
    }
    true
}

/// The macOS offscreen renderer: a `WKWebView` in a borderless window placed
/// far off every screen (WebKit lays out and paints only views in a window),
/// with its own ephemeral website data store; captured with
/// `takeSnapshotWithConfiguration:` — WebKit's own snapshot of its content,
/// not screen capture.
#[cfg(target_os = "macos")]
pub(crate) mod webkit {
    use std::time::{Duration, Instant};

    use block2::RcBlock;
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::AnyObject;
    use objc2::{MainThreadMarker, MainThreadOnly, class, msg_send};
    use objc2_app_kit::NSImage;
    use objc2_foundation::{NSData, NSError, NSNumber, NSPoint, NSRect, NSSize, NSString};
    use objc2_web_kit::{WKSnapshotConfiguration, WKWebView};

    use super::{ArtifactRenderer, ProbeReport, RenderFuture, RenderOutput};
    use crate::artifacts::document::{
        MERMAID_WAIT_MS, READ_PROBE_SCRIPT, SETTLE_MS, compare_render_plan,
    };
    use zeron_proto::ArtifactViewport;

    pub(crate) struct WebKitRenderer;

    /// The probe reports after load + settle (+ Mermaid); wait a little
    /// longer than that before snapshotting without a report.
    fn probe_budget() -> Duration {
        Duration::from_millis(u64::from(SETTLE_MS + MERMAID_WAIT_MS) + 6_000)
    }

    struct Offscreen {
        window: Retained<AnyObject>,
        view: Retained<WKWebView>,
    }

    impl Drop for Offscreen {
        fn drop(&mut self) {
            unsafe {
                let _: () = msg_send![&*self.view, stopLoading];
                let _: () = msg_send![&*self.window, orderOut: None::<&AnyObject>];
                let _: () = msg_send![&*self.window, close];
            }
        }
    }

    fn open(mtm: MainThreadMarker, document: &str, viewport: ArtifactViewport) -> Offscreen {
        let size = NSSize::new(f64::from(viewport.width), f64::from(viewport.height));
        let frame = NSRect::new(NSPoint::new(-32_000.0, -32_000.0), size);
        unsafe {
            let configuration = objc2_web_kit::WKWebViewConfiguration::new(mtm);
            configuration.setWebsiteDataStore(
                &objc2_web_kit::WKWebsiteDataStore::nonPersistentDataStore(mtm),
            );
            let view = WKWebView::initWithFrame_configuration(
                WKWebView::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), size),
                &configuration,
            );
            let allocated: Allocated<AnyObject> = msg_send![class!(NSWindow), alloc];
            // Borderless (0), buffered backing (2), not deferred.
            let window: Retained<AnyObject> = msg_send![
                allocated,
                initWithContentRect: frame,
                styleMask: 0usize,
                backing: 2usize,
                defer: false
            ];
            let _: () = msg_send![&*window, setReleasedWhenClosed: false];
            let _: () = msg_send![&*window, setIgnoresMouseEvents: true];
            let _: () = msg_send![&*window, setHasShadow: false];
            let _: () = msg_send![&*window, setExcludedFromWindowsMenu: true];
            // Transient | IgnoresCycle: never part of Mission Control or ⌘`.
            let _: () = msg_send![&*window, setCollectionBehavior: (1usize << 3) | (1usize << 6)];
            let _: () = msg_send![&*window, setContentView: &*view];
            let _: () = msg_send![&*window, setFrameOrigin: frame.origin];
            // Ordered behind everything, off every screen: WebKit treats the
            // view as in a window and paints it; nothing is visible.
            let _: () = msg_send![&*window, orderBack: None::<&AnyObject>];
            view.loadHTMLString_baseURL(&NSString::from_str(document), None);
            Offscreen { window, view }
        }
    }

    async fn evaluate(view: &WKWebView, script: &str) -> Option<String> {
        let (tx, rx) = futures::channel::oneshot::channel::<Option<String>>();
        let tx = std::cell::RefCell::new(Some(tx));
        let block = RcBlock::new(move |result: *mut AnyObject, _error: *mut NSError| {
            let text = if result.is_null() {
                None
            } else {
                unsafe {
                    let is_string: bool = msg_send![result, isKindOfClass: class!(NSString)];
                    is_string.then(|| (*(result as *const NSString)).to_string())
                }
            };
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(text);
            }
        });
        unsafe {
            view.evaluateJavaScript_completionHandler(&NSString::from_str(script), Some(&block));
        }
        rx.await.ok().flatten()
    }

    async fn snapshot(
        mtm: MainThreadMarker,
        view: &WKWebView,
        width: Option<u32>,
    ) -> Result<Vec<u8>, String> {
        let (tx, rx) = futures::channel::oneshot::channel::<Result<Vec<u8>, String>>();
        let tx = std::cell::RefCell::new(Some(tx));
        let block = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
            let outcome = if image.is_null() {
                Err(if error.is_null() {
                    "the web view returned no snapshot".to_owned()
                } else {
                    unsafe { (*error).localizedDescription().to_string() }
                })
            } else {
                unsafe { png(&*image) }
                    .ok_or_else(|| "the snapshot could not be encoded".to_owned())
            };
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(outcome);
            }
        });
        unsafe {
            let configuration = WKSnapshotConfiguration::new(mtm);
            let _: () = msg_send![&*configuration, setAfterScreenUpdates: true];
            if let Some(width) = width {
                // Points of output width: fewer pixels than the native scale.
                let width = NSNumber::numberWithDouble(f64::from(width));
                let _: () = msg_send![&*configuration, setSnapshotWidth: &*width];
            }
            view.takeSnapshotWithConfiguration_completionHandler(Some(&configuration), &block);
        }
        rx.await
            .map_err(|_| "the snapshot was cancelled".to_owned())?
    }

    unsafe fn png(image: &NSImage) -> Option<Vec<u8>> {
        unsafe {
            let tiff: Option<Retained<NSData>> = msg_send![image, TIFFRepresentation];
            let tiff = tiff?;
            let rep: Option<Retained<AnyObject>> =
                msg_send![class!(NSBitmapImageRep), imageRepWithData: &*tiff];
            let rep = rep?;
            let properties: Retained<AnyObject> = msg_send![class!(NSDictionary), dictionary];
            // NSBitmapImageFileTypePNG = 4.
            let data: Option<Retained<NSData>> =
                msg_send![&*rep, representationUsingType: 4usize, properties: &*properties];
            Some(data?.to_vec())
        }
    }

    /// Grow the offscreen window and view to `height` CSS pixels, keeping the
    /// width and the far-off-screen origin.
    fn resize(offscreen: &Offscreen, width: u32, height: u32) {
        let size = NSSize::new(f64::from(width), f64::from(height));
        unsafe {
            let frame = NSRect::new(NSPoint::new(-32_000.0, -32_000.0), size);
            let _: () = msg_send![&*offscreen.window, setFrame: frame, display: true];
            offscreen
                .view
                .setFrame(NSRect::new(NSPoint::new(0.0, 0.0), size));
        }
    }

    fn backing_scale(offscreen: &Offscreen) -> f32 {
        let scale: f64 = unsafe { msg_send![&*offscreen.window, backingScaleFactor] };
        scale as f32
    }

    async fn render_document(
        document: String,
        viewport: ArtifactViewport,
        full_height: bool,
        executor: gpui::BackgroundExecutor,
    ) -> Result<RenderOutput, String> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| "offscreen rendering must run on the main thread".to_owned())?;
        let offscreen = open(mtm, &document, viewport);
        let deadline = Instant::now() + probe_budget();
        let mut report: Option<ProbeReport> = None;
        while Instant::now() < deadline {
            executor.timer(Duration::from_millis(100)).await;
            if let Some(json) = evaluate(&offscreen.view, READ_PROBE_SCRIPT).await {
                report = serde_json::from_str(&json).ok();
                if report.is_some() {
                    break;
                }
            }
        }
        let mut snapshot_width = None;
        if full_height
            && let Some(content) = report.as_ref().and_then(|report| report.content_height)
            && content > viewport.height
        {
            let (height, width) = compare_render_plan(content, backing_scale(&offscreen));
            if height > viewport.height {
                resize(&offscreen, viewport.width, height);
                // Let the frame reflow and repaint at the new size.
                executor.timer(Duration::from_millis(300)).await;
            }
            snapshot_width = width;
        }
        let png = snapshot(mtm, &offscreen.view, snapshot_width).await;
        drop(offscreen);
        match png {
            Ok(png) => Ok(RenderOutput {
                png: Some(png),
                report,
            }),
            Err(error) if report.is_some() => {
                tracing::warn!(%error, "artifact snapshot failed");
                Ok(RenderOutput { png: None, report })
            }
            Err(error) => Err(error),
        }
    }

    impl ArtifactRenderer for WebKitRenderer {
        fn render(
            &self,
            document: String,
            viewport: ArtifactViewport,
            cx: &mut gpui::AsyncApp,
        ) -> RenderFuture {
            let executor = cx.background_executor().clone();
            Box::pin(render_document(document, viewport, false, executor))
        }

        fn render_full_height(
            &self,
            document: String,
            viewport: ArtifactViewport,
            cx: &mut gpui::AsyncApp,
        ) -> RenderFuture {
            let executor = cx.background_executor().clone();
            Box::pin(render_document(document, viewport, true, executor))
        }
    }

    // `MainThreadOnly` brings `alloc(mtm)` into scope.
    #[allow(unused)]
    fn _assert_main_thread_only<T: MainThreadOnly>() {}
}
