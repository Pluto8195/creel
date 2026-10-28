//! Artifact UI tests (GUI-free logic and headless gpui views).
use std::sync::Arc;

use mast_proto::{ArtifactKind, ArtifactRevision, ArtifactView, ArtifactsSnapshot};

use super::model::{self, LinkResolution};
use super::store::{ArtifactStore, ArtifactsLoad};

pub(crate) const SPACE: &str = "s1";

pub(crate) fn artifact(id: &str, title: &str, kind: ArtifactKind) -> ArtifactView {
    ArtifactView {
        id: id.into(),
        space_id: SPACE.into(),
        device_id: "dev".into(),
        work_item_id: Some("w-api".into()),
        chat_id: Some("chat-api".into()),
        title: title.into(),
        kind,
        role: Some("doc".into()),
        folder: "api-12345678".into(),
        stored_path: format!("/repo/.zeron/artifacts/api-12345678/{title}.md"),
        current_revision: 2,
        revisions: vec![
            ArtifactRevision {
                id: 2,
                note: "second".into(),
                at: 2_000,
                ..Default::default()
            },
            ArtifactRevision {
                id: 1,
                note: "first".into(),
                at: 1_000,
                ..Default::default()
            },
        ],
        created_at: 1_000,
        updated_at: 2_000,
        ..Default::default()
    }
}

pub(crate) fn id(n: u8) -> String {
    format!("{n:032x}")
}

pub(crate) fn snapshot(artifacts: Vec<ArtifactView>) -> ArtifactsSnapshot {
    ArtifactsSnapshot {
        space_id: SPACE.into(),
        revision: 1,
        artifacts,
    }
}

pub(crate) fn ready_store(artifacts: Vec<ArtifactView>) -> ArtifactStore {
    let mut store = ArtifactStore::new();
    store.set(SPACE, ArtifactsLoad::Ready(Arc::new(snapshot(artifacts))));
    store
}

#[test]
fn store_ignores_frame_counter_and_keeps_snapshot_through_failures() {
    let mut store = ArtifactStore::new();
    let a = artifact(&id(1), "Plan", ArtifactKind::Markdown);
    assert!(store.set(
        SPACE,
        ArtifactsLoad::Ready(Arc::new(snapshot(vec![a.clone()])))
    ));
    let mut next = snapshot(vec![a.clone()]);
    next.revision = 9;
    assert!(!store.set(SPACE, ArtifactsLoad::Ready(Arc::new(next))));
    assert!(!store.set(SPACE, ArtifactsLoad::Failed("boom".into())));
    assert!(store.snapshot(SPACE).is_some());
    assert!(store.set(SPACE, ArtifactsLoad::Unsupported));
    assert!(store.snapshot(SPACE).is_none());
    assert!(matches!(
        ArtifactsLoad::from_watch_error("unknown method: WatchArtifacts"),
        ArtifactsLoad::Unsupported
    ));
}

#[test]
fn details_order_puts_the_plan_first_then_newest() {
    let mut old = artifact(&id(1), "Old", ArtifactKind::Markdown);
    old.updated_at = 10;
    let mut new = artifact(&id(2), "New", ArtifactKind::HtmlFragment);
    new.updated_at = 30;
    let mut plan = artifact(&id(3), "Plan", ArtifactKind::Markdown);
    plan.updated_at = 1;
    plan.pinned_as_plan = true;
    let mut archived = artifact(&id(4), "Gone", ArtifactKind::Markdown);
    archived.archived_at = Some(5);
    let mut other_item = artifact(&id(5), "Other", ArtifactKind::Markdown);
    other_item.work_item_id = Some("w-ui".into());
    let store = ready_store(vec![old, new, plan, archived, other_item]);
    let titles: Vec<_> = store
        .for_work_item(SPACE, "w-api")
        .iter()
        .map(|a| a.title.clone())
        .collect();
    assert_eq!(titles, ["Plan", "New", "Old"]);
    assert_eq!(store.count_for_work_item(SPACE, "w-api"), 3);
    assert_eq!(store.plan_of(SPACE, "w-api").unwrap().title, "Plan");
}

#[test]
fn links_resolve_by_id_and_by_stored_origin_or_preview_path() {
    let mut a = artifact(&id(1), "Plan", ArtifactKind::Markdown);
    a.origin_path = Some("/work/chat/plan.md".into());
    let mut f = artifact(&id(2), "Flow", ArtifactKind::HtmlFragment);
    f.stored_path = "/repo/.zeron/artifacts/api-12345678/flow.fragment.html".into();
    f.preview_path = Some("/repo/.zeron/artifacts/api-12345678/flow.preview.html".into());
    let store = ready_store(vec![a, f]);
    let resolve =
        |target: &str| model::resolve_link(target, &store, Some("/work/chat"), Some(SPACE));
    assert_eq!(resolve(&id(1)), LinkResolution::Open(id(1)));
    assert_eq!(resolve(&id(9)), LinkResolution::UnknownId(id(9)));
    assert_eq!(resolve("plan.md"), LinkResolution::Open(id(1)));
    assert_eq!(resolve("./sub/../plan.md"), LinkResolution::Open(id(1)));
    assert_eq!(
        resolve("/repo/.zeron/artifacts/api-12345678/flow.preview.html"),
        LinkResolution::Open(id(2))
    );
    assert_eq!(
        resolve("notes/new.md"),
        LinkResolution::Register("notes/new.md".into())
    );
    assert_eq!(model::link_target("zeron-artifact:abc"), Some("abc"));
    assert_eq!(model::link_target("zeron-file:abc"), None);
}

// --------------------------------------------------------------------------
// Artifact tab (headless gpui, fake web renderer)
// --------------------------------------------------------------------------

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use mast_proto::{ArtifactContentEncoding, ArtifactContentReply};
use mast_rpc::RpcError;

use super::document::MERMAID_SRC;
use super::model::WebKind;
use super::tab::{ArtifactTab, FakeWebHost, Presenter};
use crate::state::AppState;
use crate::theme::Theme;
use crate::work_outline::WorkOutlineActions;

pub(crate) type Calls = Arc<Mutex<Vec<(String, serde_json::Value)>>>;
pub(crate) type Reply =
    Arc<dyn Fn(&str, &serde_json::Value) -> Result<serde_json::Value, RpcError> + Send + Sync>;
pub(crate) type Shown = Rc<RefCell<Vec<(String, String)>>>;

pub(crate) fn init_ui(cx: &mut TestAppContext) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let settings_dir = dir.path().join("settings");
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::settings::init(crate::settings::UiSettings::default(), &settings_dir, cx);
    });
    dir
}

/// A scripted engine: records `(method, params)` and answers with `reply`.
pub(crate) fn scripted(reply: Reply) -> (WorkOutlineActions, Calls) {
    let calls: Calls = Arc::default();
    let sink = calls.clone();
    let actions = WorkOutlineActions::scripted(Arc::new(move |method, params| {
        sink.lock()
            .unwrap()
            .push((method.to_owned(), params.clone()));
        reply(method, &params)
    }));
    (actions, calls)
}

pub(crate) fn content_reply(
    kind: ArtifactKind,
    revision: u32,
    content: &str,
    encoding: ArtifactContentEncoding,
) -> serde_json::Value {
    serde_json::to_value(ArtifactContentReply {
        artifact_id: id(1),
        kind,
        revision,
        hash: "h".into(),
        mime_type: if kind == ArtifactKind::Image {
            "image/png".into()
        } else {
            "text/plain".into()
        },
        encoding,
        content: content.into(),
        size_bytes: content.len() as u64,
        wrapper_html: None,
    })
    .unwrap()
}

pub(crate) fn open_tab(
    cx: &mut TestAppContext,
    artifact: ArtifactView,
    reply: Reply,
) -> (Entity<ArtifactTab>, Calls, Shown, &mut VisualTestContext) {
    let (actions, calls) = scripted(reply);
    let shown: Shown = Rc::default();
    let factory = FakeWebHost::factory(shown.clone());
    let state = cx.new(|_| AppState::new());
    state.update(cx, |state, _| {
        state.local_device_id = Some("dev".into());
        state.work_outline_actions_override = Some(actions);
        state.set_test_artifacts(snapshot(vec![artifact.clone()]));
    });
    let artifact_id = artifact.id.clone();
    let (tab, vcx) = cx.add_window_view(move |window, cx| {
        ArtifactTab::new(state, None, artifact_id, factory, window, cx)
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
    (tab, calls, shown, vcx)
}

pub(crate) fn content_calls(calls: &Calls) -> Vec<serde_json::Value> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, _)| method == mast_rpc::methods::ARTIFACT_CONTENT)
        .map(|(_, params)| params.clone())
        .collect()
}

pub(crate) fn reply_with(value: serde_json::Value) -> Reply {
    Arc::new(move |_, _| Ok(value.clone()))
}

#[gpui::test]
fn each_kind_renders_with_its_component(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let utf8 = ArtifactContentEncoding::Utf8;
    let cases: Vec<(ArtifactKind, String, ArtifactContentEncoding, Presenter)> = vec![
        (ArtifactKind::Markdown, "# Plan\n\nBody".into(), utf8, Presenter::Markdown),
        (
            ArtifactKind::HtmlFragment,
            "<div>flow</div>".into(),
            utf8,
            Presenter::Web(WebKind::Fragment),
        ),
        (
            ArtifactKind::HtmlPage,
            "<!doctype html><html><head></head><body>page</body></html>".into(),
            utf8,
            Presenter::Web(WebKind::Page),
        ),
        (
            ArtifactKind::Mermaid,
            "graph TD; A-->B".into(),
            utf8,
            Presenter::Web(WebKind::Mermaid),
        ),
        (
            ArtifactKind::Image,
            // 1×1 PNG.
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==".into(),
            ArtifactContentEncoding::Base64,
            Presenter::Image,
        ),
        (
            ArtifactKind::Options,
            r#"{"title":"Layouts","options":[{"id":"a","label":"Grid","path":"a.fragment.html"}]}"#.into(),
            utf8,
            Presenter::Options,
        ),
    ];
    for (kind, content, encoding, expected) in cases {
        let artifact = artifact(&id(1), "Thing", kind);
        let reply = reply_with(content_reply(kind, 2, &content, encoding));
        let (tab, calls, shown, vcx) = open_tab(cx, artifact, reply);
        let presenter = tab.read_with(vcx, |tab, _| tab.presenter());
        assert_eq!(presenter, expected, "{kind:?}");
        let calls = content_calls(&calls);
        assert_eq!(calls[0]["artifactId"], id(1));
        assert!(calls[0].get("revision").is_none(), "current file first");
        // Same-device artifacts are read locally (no forwarding).
        assert!(calls[0].get("targetDeviceId").is_none());
        let documents = shown.borrow();
        match kind {
            ArtifactKind::HtmlFragment => {
                assert_eq!(documents.len(), 1);
                assert!(documents[0].1.contains("sandbox=\"allow-scripts\""));
                assert!(documents[0].1.contains("&lt;div&gt;flow&lt;/div&gt;"));
            }
            ArtifactKind::HtmlPage => {
                assert!(documents[0].1.contains("srcdoc="));
                assert!(documents[0].1.contains("sandbox=\"allow-scripts\""));
            }
            ArtifactKind::Mermaid => {
                assert!(documents[0].1.contains(MERMAID_SRC));
            }
            _ => assert!(documents.is_empty(), "{kind:?} renders natively"),
        }
    }
}

#[gpui::test]
fn options_open_their_fragment_through_the_companion_read(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let manifest =
        r#"{"title":"Layouts","options":[{"id":"a","label":"Grid","path":"a.fragment.html"}]}"#;
    let reply: Reply = Arc::new(move |_, params| {
        Ok(if params.get("companion").is_some() {
            content_reply(
                ArtifactKind::HtmlFragment,
                0,
                "<p>grid</p>",
                ArtifactContentEncoding::Utf8,
            )
        } else {
            content_reply(
                ArtifactKind::Options,
                2,
                manifest,
                ArtifactContentEncoding::Utf8,
            )
        })
    });
    let (tab, calls, shown, vcx) = open_tab(
        cx,
        artifact(&id(1), "Layouts", ArtifactKind::Options),
        reply,
    );
    let bounds = vcx.debug_bounds("artifact-option-a").expect("option row");
    vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        tab.read_with(vcx, |tab, _| tab.presenter()),
        Presenter::Web(WebKind::Fragment)
    );
    assert_eq!(content_calls(&calls)[1]["companion"], "a.fragment.html");
    assert!(shown.borrow()[0].1.contains("&lt;p&gt;grid&lt;/p&gt;"));
}

#[gpui::test]
fn revision_picker_loads_that_revision(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let reply: Reply = Arc::new(|_, params| {
        let revision = params["revision"].as_u64().unwrap_or(2) as u32;
        Ok(content_reply(
            ArtifactKind::Markdown,
            revision,
            &format!("rev {revision}"),
            ArtifactContentEncoding::Utf8,
        ))
    });
    let (tab, calls, _, vcx) =
        open_tab(cx, artifact(&id(1), "Plan", ArtifactKind::Markdown), reply);
    let picker = vcx
        .debug_bounds("artifact-revision-picker")
        .expect("picker");
    vcx.simulate_click(picker.center(), gpui::Modifiers::default());
    vcx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
    let row = vcx.debug_bounds("artifact-revision-1").expect("r1 row");
    vcx.simulate_click(row.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let calls = content_calls(&calls);
    assert_eq!(calls.last().unwrap()["revision"], 1);
    tab.read_with(vcx, |tab, cx| {
        assert_eq!(tab.shown_revision(), Some(1));
        let header = super::model::header(&tab.artifact(cx).unwrap(), Some(1));
        assert_eq!(header.note.as_deref(), Some("first"));
        assert!(!header.current);
    });
}

#[gpui::test]
fn compare_source_tab_uses_the_revision_text_diff(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let reply: Reply = Arc::new(|_, params| {
        let revision = params["revision"].as_u64().unwrap_or(2) as u32;
        let text = if revision == 1 { "a\nb\n" } else { "a\nc\n" };
        Ok(content_reply(
            ArtifactKind::Markdown,
            revision,
            text,
            ArtifactContentEncoding::Utf8,
        ))
    });
    let (tab, calls, _, vcx) =
        open_tab(cx, artifact(&id(1), "Plan", ArtifactKind::Markdown), reply);
    tab.update_in(vcx, |tab, window, cx| tab.compare_with(1, window, cx));
    vcx.run_until_parked();
    let revisions: Vec<_> = content_calls(&calls)
        .iter()
        .skip(1)
        .map(|call| call["revision"].as_u64())
        .collect();
    assert_eq!(
        revisions,
        [Some(1), Some(2)],
        "both sides are read by revision"
    );
    tab.update_in(vcx, |tab, window, cx| {
        tab.set_compare_mode(super::compare::CompareMode::Source, window, cx)
    });
    tab.read_with(vcx, |tab, _| {
        assert_eq!(tab.presenter(), Presenter::Compare);
        let view = tab.compare_view().expect("compare view");
        assert_eq!((view.left, view.right), (1, 2));
        let data = view.data().expect("compare ready");
        assert_eq!(
            data.lines.as_deref(),
            Some(mast_proto::diff_lines("a\nb\n", "a\nc\n").as_slice())
        );
    });
    vcx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
    assert!(vcx.debug_bounds("artifact-compare-diff").is_some());
}

#[gpui::test]
fn unknown_method_reads_as_not_available_yet(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let reply: Reply = Arc::new(|method, _| Err(RpcError::UnknownMethod(method.to_owned())));
    let (tab, _, _, vcx) = open_tab(cx, artifact(&id(1), "Plan", ArtifactKind::Markdown), reply);
    assert_eq!(
        tab.read_with(vcx, |tab, _| tab.presenter()),
        Presenter::Message(super::store::UNSUPPORTED_TEXT.to_owned())
    );
    // Pin answers the same way.
    tab.update(vcx, |tab, cx| tab.set_pinned(true, cx));
    vcx.run_until_parked();
    assert_eq!(
        tab.read_with(vcx, |tab, _| tab.notice().map(str::to_owned)),
        Some(super::store::UNSUPPORTED_TEXT.to_owned())
    );
}

#[gpui::test]
fn header_shows_edited_without_a_note_and_the_plan_badge(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let mut plan = artifact(&id(1), "Plan", ArtifactKind::Markdown);
    plan.pinned_as_plan = true;
    plan.revisions[0].note = mast_proto::artifacts::EDITED_WITHOUT_NOTE.into();
    let reply = reply_with(content_reply(
        ArtifactKind::Markdown,
        2,
        "x",
        ArtifactContentEncoding::Utf8,
    ));
    let (tab, _, _, vcx) = open_tab(cx, plan, reply);
    assert!(vcx.debug_bounds("artifact-edited-warning").is_some());
    assert!(vcx.debug_bounds("artifact-plan-badge").is_some());
    assert!(vcx.debug_bounds("artifact-linear-project").is_none());
    // Pinned as the plan: the work item's Linear project shows beside it.
    let mut outline = crate::work_outline::store::fixture_snapshot(SPACE);
    outline
        .external_references
        .push(mast_proto::WorkItemExternalReferenceView {
            id: "ref".into(),
            space_id: SPACE.into(),
            work_item_id: "w-api".into(),
            provider: mast_proto::linear::LINEAR_PROVIDER.into(),
            reference_type: mast_proto::linear::LINEAR_PROJECT_REFERENCE.into(),
            external_id: "p".into(),
            url: "https://linear.app/acme/project/p-1".into(),
            cached_title: Some("Receiving".into()),
            created_by: None,
            created_at: 1,
        });
    let state = tab.read_with(vcx, |tab, _| tab.state_for_tests());
    state.update(vcx, |state, _| {
        state.set_work_outline_visible(vec![SPACE.into()]);
        assert!(state.apply_work_outline_frame(SPACE, outline));
    });
    vcx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
    assert!(vcx.debug_bounds("artifact-linear-project").is_some());
}

// --------------------------------------------------------------------------
// Render-check executor (fake offscreen renderer)
// --------------------------------------------------------------------------

use super::render_check::{self, ArtifactRenderer, RenderConfig, RenderFuture, RenderOutput};
use mast_proto::{ArtifactRenderRequest, ArtifactRenderResultParams, ArtifactViewport};

/// Records each document and answers from `answer` (or holds the render
/// until released when `gated`).
#[derive(Default)]
struct FakeRenderer {
    documents: RefCell<Vec<(String, ArtifactViewport)>>,
    gates: RefCell<Vec<futures::channel::oneshot::Sender<()>>>,
    gated: bool,
    hang: bool,
    report: Option<super::document::ProbeReport>,
}

impl ArtifactRenderer for Rc<FakeRenderer> {
    fn render(
        &self,
        document: String,
        viewport: ArtifactViewport,
        _: &mut gpui::AsyncApp,
    ) -> RenderFuture {
        self.documents.borrow_mut().push((document, viewport));
        let report = self.report.clone();
        if self.hang {
            return Box::pin(futures::future::pending());
        }
        let gate = self.gated.then(|| {
            let (tx, rx) = futures::channel::oneshot::channel();
            self.gates.borrow_mut().push(tx);
            rx
        });
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.await.ok();
            }
            Ok(RenderOutput {
                png: Some(vec![0x89, b'P', b'N', b'G']),
                report,
            })
        })
    }
}

fn render_request(n: u8, kind: ArtifactKind, content: &str) -> ArtifactRenderRequest {
    ArtifactRenderRequest {
        request_id: format!("req-{n}"),
        artifact_id: Some(id(1)),
        title: "Flow".into(),
        kind,
        path: "/repo/.zeron/artifacts/x/flow.fragment.html".into(),
        content: content.into(),
        viewport: ArtifactViewport {
            width: 100,
            height: 9000,
        },
        ..Default::default()
    }
}

fn serve_now(
    cx: &mut TestAppContext,
    request: ArtifactRenderRequest,
    renderer: Option<Rc<dyn ArtifactRenderer>>,
) -> ArtifactRenderResultParams {
    let theme = cx.update(|cx| render_check::request_themes(&request, cx));
    let task = cx.spawn(async move |mut cx| {
        render_check::serve(
            request,
            renderer,
            theme,
            std::time::Duration::from_secs(25),
            |_| futures::future::ready(()),
            &mut cx,
        )
        .await
    });
    cx.run_until_parked();
    futures::executor::block_on(task)
}

#[gpui::test]
fn render_check_returns_screenshot_errors_and_warnings(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let renderer = Rc::new(FakeRenderer {
        report: Some(super::document::ProbeReport {
            errors: vec!["Console error: boom".into()],
            warnings: vec!["Horizontal overflow: content is 900px wide in a 600px viewport".into()],
            ..Default::default()
        }),
        ..Default::default()
    });
    // A timeout clock that never fires: the render always wins.
    let params = {
        let request = render_request(1, ArtifactKind::HtmlFragment, "<div>flow</div>");
        let theme = cx.update(|cx| render_check::request_themes(&request, cx));
        let dyn_renderer: Rc<dyn ArtifactRenderer> = Rc::new(renderer.clone());
        let task = cx.spawn(async move |mut cx| {
            render_check::serve(
                request,
                Some(dyn_renderer),
                theme,
                std::time::Duration::from_secs(25),
                |_| futures::future::pending::<()>(),
                &mut cx,
            )
            .await
        });
        cx.run_until_parked();
        futures::executor::block_on(task)
    };
    assert_eq!(params.request_id, "req-1");
    assert!(params.error.is_none());
    let result = params.result.unwrap();
    use base64::Engine as _;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(result.screenshot_png_base64.unwrap())
            .unwrap(),
        [0x89, b'P', b'N', b'G']
    );
    assert_eq!(result.errors, ["Console error: boom"]);
    assert_eq!(result.warnings.len(), 1);
    // Rendered exactly like the tab (visualize sandbox) plus the probe and
    // its collector; the viewport is clamped.
    let (document, viewport) = renderer.documents.borrow()[0].clone();
    assert!(document.contains("sandbox=\"allow-scripts\""));
    assert!(document.contains("window.__zeronProbe"));
    assert!(document.contains("securitypolicyviolation"));
    assert!(document.contains("&lt;div&gt;flow&lt;/div&gt;"));
    assert_eq!(
        viewport,
        ArtifactViewport {
            width: 200,
            height: 4096
        }
    );
}

/// Serve `request` against `renderer` (the timeout clock never fires).
fn serve_with(
    cx: &mut TestAppContext,
    request: ArtifactRenderRequest,
    renderer: &Rc<FakeRenderer>,
) -> ArtifactRenderResultParams {
    let themes = cx.update(|cx| render_check::request_themes(&request, cx));
    let dyn_renderer: Rc<dyn ArtifactRenderer> = Rc::new(renderer.clone());
    let task = cx.spawn(async move |mut cx| {
        render_check::serve(
            request,
            Some(dyn_renderer),
            themes,
            std::time::Duration::from_secs(25),
            |_| futures::future::pending::<()>(),
            &mut cx,
        )
        .await
    });
    cx.run_until_parked();
    futures::executor::block_on(task)
}

fn probe_report_with_a_clipped_title() -> super::document::ProbeReport {
    super::document::ProbeReport {
        errors: vec!["Console error: boom".into()],
        metrics: vec![super::layout_heuristics::ElementMetrics {
            sel: "h2.title".into(),
            text: "A title that does not fit".into(),
            w: 100,
            h: 20,
            sw: 300,
            cw: 100,
            sh: 20,
            ch: 20,
            clips: true,
            pos: "static".into(),
            parent: -1,
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[gpui::test]
fn render_check_renders_every_viewport_and_theme_and_keeps_the_single_screenshot(
    cx: &mut TestAppContext,
) {
    let _dir = init_ui(cx);
    let renderer = Rc::new(FakeRenderer {
        report: Some(probe_report_with_a_clipped_title()),
        ..Default::default()
    });
    let mut request = render_request(1, ArtifactKind::HtmlFragment, "<div>flow</div>");
    request.viewports = mast_proto::ArtifactRenderViewport::defaults();
    request.themes = vec![
        mast_proto::ArtifactRenderTheme::Light,
        mast_proto::ArtifactRenderTheme::Dark,
    ];
    let result = serve_with(cx, request, &renderer).result.unwrap();
    let seen: Vec<_> = result
        .screenshots
        .iter()
        .map(|s| (s.viewport.as_str(), s.width, s.height, s.theme.as_str()))
        .collect();
    assert_eq!(
        seen,
        [
            ("narrow", 800, 800, "light"),
            ("narrow", 800, 800, "dark"),
            ("wide", 1440, 900, "light"),
            ("wide", 1440, 900, "dark"),
        ]
    );
    // The renderer got each viewport size, in order.
    let sizes: Vec<_> = renderer
        .documents
        .borrow()
        .iter()
        .map(|(_, v)| (v.width, v.height))
        .collect();
    assert_eq!(sizes, [(800, 800), (800, 800), (1440, 900), (1440, 900)]);
    // The first image is also the old single `screenshot`.
    assert_eq!(
        result.screenshot_png_base64.as_deref(),
        Some(result.screenshots[0].png_base64.as_str())
    );
    // The same console error is reported once; layout findings carry their
    // viewport and theme.
    assert_eq!(result.errors, ["Console error: boom"]);
    let found: Vec<_> = result
        .layout_warnings
        .iter()
        .map(|w| (w.kind.as_str(), w.viewport.as_str(), w.theme.as_str()))
        .collect();
    assert_eq!(found.len(), 4);
    assert!(found.contains(&("clippedText", "wide", "dark")));
    assert_eq!(result.layout_warnings[0].selector, "h2.title");
    assert_eq!(
        result.layout_warnings[0].text_sample,
        "A title that does not fit"
    );
    assert!(result.warnings.is_empty());
}

#[gpui::test]
fn render_check_caps_images_at_four_and_says_so(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let renderer = Rc::new(FakeRenderer {
        report: Some(Default::default()),
        ..Default::default()
    });
    let mut request = render_request(1, ArtifactKind::HtmlFragment, "<p>x</p>");
    request.viewports = vec![
        mast_proto::ArtifactRenderViewport::defaults().remove(0),
        mast_proto::ArtifactRenderViewport::defaults().remove(1),
        mast_proto::ArtifactRenderViewport {
            width: 1920,
            height: 1080,
            label: Some("huge".into()),
        },
    ];
    request.themes = vec![
        mast_proto::ArtifactRenderTheme::Light,
        mast_proto::ArtifactRenderTheme::Dark,
    ];
    let result = serve_with(cx, request, &renderer).result.unwrap();
    assert_eq!(result.screenshots.len(), 4);
    assert_eq!(renderer.documents.borrow().len(), 4);
    assert_eq!(result.warnings.len(), 1);
    assert!(
        result.warnings[0].contains("4 of 6"),
        "{:?}",
        result.warnings
    );
    assert!(result.screenshots.iter().all(|s| s.viewport != "huge"));
}

#[gpui::test]
fn render_check_single_viewport_and_theme_stay_one_image(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let renderer = Rc::new(FakeRenderer {
        report: Some(Default::default()),
        ..Default::default()
    });
    // An older engine's request: no lists, one viewport (clamped), the
    // current theme.
    let request = render_request(1, ArtifactKind::HtmlFragment, "<p>x</p>");
    let result = serve_with(cx, request, &renderer).result.unwrap();
    assert_eq!(result.screenshots.len(), 1);
    assert_eq!(result.screenshots[0].viewport, "200x4096");
    assert!(["light", "dark"].contains(&result.screenshots[0].theme.as_str()));
    assert_eq!(
        result.screenshot_png_base64.as_deref(),
        Some(result.screenshots[0].png_base64.as_str())
    );
    assert!(result.warnings.is_empty());
}

#[gpui::test]
fn render_check_times_out_and_reports_missing_probes_and_platforms(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let hanging: Rc<dyn ArtifactRenderer> = Rc::new(Rc::new(FakeRenderer {
        hang: true,
        ..Default::default()
    }));
    let timed_out = serve_now(
        cx,
        render_request(1, ArtifactKind::Mermaid, "graph TD; A-->B"),
        Some(hanging),
    );
    assert!(timed_out.result.is_none());
    assert!(
        timed_out
            .error
            .as_deref()
            .unwrap()
            .starts_with("artifact/renderTimeout:")
    );

    // No probe report: still a result, flagged.
    let silent = Rc::new(FakeRenderer::default());
    let dyn_silent: Rc<dyn ArtifactRenderer> = Rc::new(silent.clone());
    let request = render_request(
        2,
        ArtifactKind::HtmlPage,
        "<html><head></head><body>x</body></html>",
    );
    let theme = cx.update(|cx| render_check::request_themes(&request, cx));
    let task = cx.spawn(async move |mut cx| {
        render_check::serve(
            request,
            Some(dyn_silent),
            theme,
            std::time::Duration::from_secs(25),
            |_| futures::future::pending::<()>(),
            &mut cx,
        )
        .await
    });
    cx.run_until_parked();
    let params = futures::executor::block_on(task);
    let result = params.result.unwrap();
    assert!(result.errors[0].contains("probe did not report"));
    // The page got the probe at the top of its head.
    assert!(
        silent.documents.borrow()[0]
            .0
            .contains("&lt;head&gt;&lt;script&gt;")
    );

    // No offscreen renderer on this platform.
    let unavailable = serve_now(
        cx,
        render_request(3, ArtifactKind::HtmlFragment, "<p>x</p>"),
        None,
    );
    assert!(
        unavailable
            .error
            .as_deref()
            .unwrap()
            .starts_with("artifact/renderUnavailable:")
    );

    // Markdown renders natively: answered with a warning, no screenshot.
    let any: Rc<dyn ArtifactRenderer> = Rc::new(Rc::new(FakeRenderer::default()));
    let markdown = serve_now(
        cx,
        render_request(4, ArtifactKind::Markdown, "# x"),
        Some(any),
    );
    let result = markdown.result.unwrap();
    assert!(result.screenshot_png_base64.is_none());
    assert_eq!(result.warnings.len(), 1);
}

#[gpui::test]
fn render_executor_serves_the_stream_with_bounded_concurrency(cx: &mut TestAppContext) {
    let _dir = init_ui(cx);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = runtime.enter();
    let (out, mut requests) = tokio::sync::mpsc::channel::<String>(64);
    let (replies, inbound) = tokio::sync::mpsc::channel::<String>(64);
    let renderer = Rc::new(FakeRenderer {
        gated: true,
        report: Some(Default::default()),
        ..Default::default()
    });
    let (actions, calls) = scripted(Arc::new(|_, _| Ok(serde_json::json!({ "accepted": true }))));
    let state = cx.new(|_| AppState::new());
    let dyn_renderer: Rc<dyn ArtifactRenderer> = Rc::new(renderer.clone());
    state.update(cx, |state, cx| {
        state.work_outline_actions_override = Some(actions);
        let handle =
            crate::state::EngineHandle::from_test_client(mast_rpc::RpcClient::new(out, inbound));
        state.start_artifact_render_executor(
            handle,
            Some(dyn_renderer),
            RenderConfig {
                timeout: std::time::Duration::from_secs(25),
                concurrency: 2,
            },
            cx,
        );
    });
    cx.run_until_parked();
    let frame: serde_json::Value = serde_json::from_str(&requests.try_recv().unwrap()).unwrap();
    assert_eq!(
        frame["method"],
        mast_rpc::methods::WATCH_ARTIFACT_RENDER_REQUESTS
    );
    assert_eq!(frame["params"]["client"], "mast-ui");
    for n in 1..=3 {
        let item = serde_json::json!({
            "id": frame["id"].clone(),
            "item": render_request(n, ArtifactKind::HtmlFragment, "<p>x</p>"),
        });
        runtime.block_on(async {
            replies.send(item.to_string()).await.unwrap();
            while replies.capacity() < replies.max_capacity() {
                tokio::task::yield_now().await;
            }
        });
    }
    cx.run_until_parked();
    assert_eq!(
        renderer.documents.borrow().len(),
        2,
        "two offscreen views at most"
    );
    let release = |renderer: &Rc<FakeRenderer>| {
        let gate = renderer.gates.borrow_mut().remove(0);
        gate.send(()).unwrap();
    };
    release(&renderer);
    cx.run_until_parked();
    assert_eq!(renderer.documents.borrow().len(), 3);
    release(&renderer);
    release(&renderer);
    cx.run_until_parked();
    let mut answered: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, _)| method == mast_rpc::methods::ARTIFACT_RENDER_RESULT)
        .map(|(_, params)| params["requestId"].as_str().unwrap().to_owned())
        .collect();
    answered.sort();
    assert_eq!(answered, ["req-1", "req-2", "req-3"]);
}
