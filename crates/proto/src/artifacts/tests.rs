use serde_json::json;

use super::*;
use crate::work_outline::WorkOutlineActorKind;

fn view() -> ArtifactView {
    ArtifactView {
        id: "a".repeat(32),
        space_id: "space-1".into(),
        device_id: "dev-1".into(),
        work_item_id: Some("item-1".into()),
        chat_id: Some("chat-1".into()),
        title: "Receiving flow — plan".into(),
        kind: ArtifactKind::HtmlFragment,
        role: Some("plan".into()),
        folder: "receiving-flow-item-1".into(),
        stored_path: "/repo/.zeron/artifacts/receiving-flow-item-1/flow.fragment.html".into(),
        origin_path: Some("/tmp/flow.fragment.html".into()),
        preview_path: Some(
            "/repo/.zeron/artifacts/receiving-flow-item-1/flow.preview.html".into(),
        ),
        companions: vec!["a.fragment.html".into()],
        current_revision: 2,
        revisions: vec![ArtifactRevision {
            id: 2,
            hash: "ab".into(),
            note: "tightened".into(),
            at: 5,
            by: WorkOutlineActor {
                kind: WorkOutlineActorKind::Agent,
                device_id: "dev-1".into(),
                chat_id: Some("chat-1".into()),
                message_id: None,
            },
            size_bytes: 10,
            op: None,
        }],
        pinned_as_plan: true,
        answers: Vec::new(),
        promoted_path: Some("/repo/docs/plans/flow.fragment.html".into()),
        archived_at: None,
        created_at: 1,
        updated_at: 5,
    }
}

#[test]
fn artifact_view_field_names_are_camel_case() {
    let value = serde_json::to_value(view()).unwrap();
    for field in [
        "id",
        "spaceId",
        "deviceId",
        "workItemId",
        "chatId",
        "title",
        "kind",
        "role",
        "folder",
        "storedPath",
        "originPath",
        "previewPath",
        "companions",
        "currentRevision",
        "revisions",
        "pinnedAsPlan",
        "promotedPath",
        "createdAt",
        "updatedAt",
    ] {
        assert!(value.get(field).is_some(), "missing {field}: {value}");
    }
    assert_eq!(value["kind"], "htmlFragment");
    let revision = &value["revisions"][0];
    for field in ["id", "hash", "note", "at", "by", "sizeBytes"] {
        assert!(revision.get(field).is_some(), "missing revision {field}");
    }
    assert!(revision.get("op").is_none(), "op is omitted when absent");
    assert_eq!(revision["by"]["kind"], "agent");
    assert!(value.get("archivedAt").is_none());
    let round: ArtifactView = serde_json::from_value(value).unwrap();
    assert_eq!(round, view());
}

#[test]
fn artifact_view_decodes_with_only_identity_fields() {
    let view: ArtifactView =
        serde_json::from_value(json!({ "id": "x", "spaceId": "s", "kind": "newKind" })).unwrap();
    assert_eq!(view.kind, ArtifactKind::Unknown);
    assert_eq!(view.current_revision, 0);
    assert!(view.revisions.is_empty() && !view.pinned_as_plan);
}

#[test]
fn kinds_are_lenient_and_accept_kebab_spellings() {
    for (raw, kind) in [
        ("markdown", ArtifactKind::Markdown),
        ("htmlFragment", ArtifactKind::HtmlFragment),
        ("html-fragment", ArtifactKind::HtmlFragment),
        ("htmlPage", ArtifactKind::HtmlPage),
        ("html-page", ArtifactKind::HtmlPage),
        ("mermaid", ArtifactKind::Mermaid),
        ("image", ArtifactKind::Image),
        ("options", ArtifactKind::Options),
        ("future", ArtifactKind::Unknown),
    ] {
        let parsed: ArtifactKind = serde_json::from_value(json!(raw)).unwrap();
        assert_eq!(parsed, kind, "{raw}");
    }
    assert_eq!(ArtifactKind::parse("html-fragment"), Some(ArtifactKind::HtmlFragment));
    assert_eq!(ArtifactKind::parse("bogus"), None);
    assert_eq!(
        serde_json::to_value(ArtifactKind::HtmlPage).unwrap(),
        json!("htmlPage")
    );
    for kind in [
        ArtifactKind::Markdown,
        ArtifactKind::HtmlFragment,
        ArtifactKind::HtmlPage,
        ArtifactKind::Mermaid,
        ArtifactKind::Image,
        ArtifactKind::Options,
    ] {
        assert_eq!(serde_json::to_value(kind).unwrap(), json!(kind.as_str()));
    }
}

#[test]
fn kind_defaults_from_the_extension() {
    for (path, kind) in [
        ("/a/plan.md", ArtifactKind::Markdown),
        ("/a/flow.fragment.html", ArtifactKind::HtmlFragment),
        ("/a/flow.preview.html", ArtifactKind::HtmlFragment),
        ("/a/page.html", ArtifactKind::HtmlPage),
        ("/a/states.mmd", ArtifactKind::Mermaid),
        ("/a/shot.PNG", ArtifactKind::Image),
        ("/a/layouts.options.json", ArtifactKind::Options),
        ("/a/notes.txt", ArtifactKind::Markdown),
    ] {
        assert_eq!(ArtifactKind::from_path(path), kind, "{path}");
    }
}

#[test]
fn rpc_params_and_replies_use_camel_case() {
    let params = ArtifactContentParams {
        artifact_id: "a".into(),
        revision: Some(3),
        companion: Some("x.fragment.html".into()),
        target_device_id: Some("dev".into()),
    };
    let value = serde_json::to_value(&params).unwrap();
    assert_eq!(
        value,
        json!({ "artifactId": "a", "revision": 3, "companion": "x.fragment.html",
                "targetDeviceId": "dev" })
    );
    let reply = ArtifactContentReply {
        artifact_id: "a".into(),
        kind: ArtifactKind::Image,
        revision: 1,
        hash: "h".into(),
        mime_type: "image/png".into(),
        encoding: ArtifactContentEncoding::Base64,
        content: "AA==".into(),
        size_bytes: 1,
        wrapper_html: None,
    };
    let value = serde_json::to_value(&reply).unwrap();
    for field in [
        "artifactId",
        "kind",
        "revision",
        "hash",
        "mimeType",
        "encoding",
        "content",
        "sizeBytes",
    ] {
        assert!(value.get(field).is_some(), "missing {field}");
    }
    assert_eq!(value["encoding"], "base64");

    let register: ArtifactRegisterParams = serde_json::from_value(json!({
        "spaceId": "s", "workItemId": "w", "path": "work/plan.md", "title": "Plan",
        "kind": "markdown", "role": "plan"
    }))
    .unwrap();
    assert_eq!(register.kind, Some(ArtifactKind::Markdown));
    let publish = ArtifactPublishReply {
        artifact_id: "a".into(),
        revision: 1,
        url: "zeron-artifact:a".into(),
        stored_path: "/p".into(),
        changed: true,
    };
    assert_eq!(
        serde_json::to_value(&publish).unwrap(),
        json!({ "artifactId": "a", "revision": 1, "url": "zeron-artifact:a",
                "storedPath": "/p", "changed": true })
    );
    assert_eq!(
        serde_json::to_value(ArtifactPromoteReply {
            artifact_id: "a".into(),
            promoted_path: "/r/docs/plans/p.md".into()
        })
        .unwrap(),
        json!({ "artifactId": "a", "promotedPath": "/r/docs/plans/p.md" })
    );
    let pin: ArtifactPinParams =
        serde_json::from_value(json!({ "artifactId": "a", "pinned": true })).unwrap();
    assert!(pin.pinned);
    let snapshot = ArtifactsSnapshot {
        space_id: "s".into(),
        revision: 2,
        artifacts: vec![view()],
    };
    let value = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(value["spaceId"], "s");
    assert_eq!(value["artifacts"][0]["pinnedAsPlan"], true);
}

#[test]
fn render_types_use_camel_case_and_defaults() {
    let request: ArtifactRenderRequest =
        serde_json::from_value(json!({ "requestId": "r1" })).unwrap();
    assert_eq!(request.viewport, ArtifactViewport::default());
    assert_eq!(request.viewport.width, 1280);
    assert_eq!(request.theme, ArtifactRenderTheme::System);
    let value = serde_json::to_value(ArtifactRenderRequest {
        request_id: "r1".into(),
        artifact_id: Some("a".into()),
        title: "T".into(),
        kind: ArtifactKind::Mermaid,
        path: "/p.mmd".into(),
        encoding: ArtifactContentEncoding::Utf8,
        content: "graph TD".into(),
        companions: vec![],
        viewport: ArtifactViewport {
            width: 800,
            height: 600,
        },
        theme: ArtifactRenderTheme::Dark,
        ..Default::default()
    })
    .unwrap();
    for field in [
        "requestId",
        "artifactId",
        "title",
        "kind",
        "path",
        "encoding",
        "content",
        "viewport",
        "theme",
    ] {
        assert!(value.get(field).is_some(), "missing {field}");
    }
    assert_eq!(value["theme"], "dark");
    let result: ArtifactRenderResultParams = serde_json::from_value(json!({
        "requestId": "r1",
        "result": { "screenshotPngBase64": "iVBOR", "errors": ["boom"], "warnings": [] }
    }))
    .unwrap();
    let inner = result.result.unwrap();
    assert_eq!(inner.screenshot_png_base64.as_deref(), Some("iVBOR"));
    assert_eq!(inner.errors, vec!["boom".to_owned()]);
    let clamped = ArtifactViewport {
        width: 10,
        height: 99_999,
    }
    .clamped();
    assert_eq!((clamped.width, clamped.height), (200, 4096));
    let theme: ArtifactRenderTheme = serde_json::from_value(json!("sepia")).unwrap();
    assert_eq!(theme, ArtifactRenderTheme::System);
}

#[test]
fn render_plans_are_viewport_major_and_capped_at_four_images() {
    let mut request: ArtifactRenderRequest =
        serde_json::from_value(json!({ "requestId": "r1" })).unwrap();
    // Old shape: the single viewport and theme.
    let (plan, warning) = request.renders();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].0.width, 1280);
    assert!(warning.is_none());

    request.viewports = ArtifactRenderViewport::defaults();
    request.themes = vec![ArtifactRenderTheme::Light, ArtifactRenderTheme::Dark];
    let (plan, warning) = request.renders();
    assert!(warning.is_none());
    let order: Vec<_> = plan
        .iter()
        .map(|(v, t)| (v.display_label(), *t))
        .collect();
    assert_eq!(
        order,
        [
            ("narrow".to_owned(), ArtifactRenderTheme::Light),
            ("narrow".to_owned(), ArtifactRenderTheme::Dark),
            ("wide".to_owned(), ArtifactRenderTheme::Light),
            ("wide".to_owned(), ArtifactRenderTheme::Dark),
        ]
    );

    request.viewports.push(ArtifactRenderViewport {
        width: 10,
        height: 99_999,
        label: Some("  ".into()),
    });
    let (plan, warning) = request.renders();
    assert_eq!(plan.len(), 4);
    assert!(warning.unwrap().contains("4 of 6"));

    // Unlabelled viewports read as their clamped size.
    let clamped = ArtifactRenderViewport {
        width: 10,
        height: 99_999,
        label: None,
    }
    .clamped();
    assert_eq!(clamped.display_label(), "200x4096");
}

#[test]
fn errors_round_trip_through_the_wire_string() {
    let error = ArtifactError::new(ArtifactErrorCode::RenderUnavailable, "no window");
    let wire = error.to_wire_message();
    assert_eq!(wire, "artifact/renderUnavailable: no window");
    assert_eq!(ArtifactError::from_wire_message(&wire), Some(error));
    let nested = ArtifactError::from_wire_message("AgentToolCall: artifact/notFound: gone").unwrap();
    assert_eq!(nested.code, ArtifactErrorCode::NotFound);
    assert_eq!(nested.message, "gone");
    assert_eq!(
        ArtifactError::from_wire_message("artifact/brandNew: x")
            .unwrap()
            .code,
        ArtifactErrorCode::Internal
    );
    assert!(ArtifactError::from_wire_message("work-outline/invalid: x").is_none());
}

#[test]
fn identity_is_scope_title_and_role() {
    let a = artifact_id("s", Some("item"), Some("chat-1"), "Receiving  Plan", Some("plan"));
    assert_eq!(a.len(), 32);
    assert!(is_artifact_id(&a));
    // The chat does not matter once there is a work item; title is
    // normalized.
    assert_eq!(
        a,
        artifact_id("s", Some("item"), Some("chat-2"), " receiving plan ", Some("Plan"))
    );
    assert_ne!(a, artifact_id("s", Some("item"), None, "Receiving plan", Some("doc")));
    assert_ne!(a, artifact_id("s", Some("other"), None, "Receiving plan", Some("plan")));
    assert_ne!(
        artifact_id("s", None, Some("c1"), "x", None),
        artifact_id("s", None, Some("c2"), "x", None)
    );
    let op = publish_operation_id("chat-1", "publish plan v2");
    assert!(op.len() <= 64, "{op}");
    assert!(op.starts_with("artifact-"));
    assert_eq!(op, publish_operation_id("chat-1", "publish plan v2"));
    assert_ne!(op, publish_operation_id("chat-2", "publish plan v2"));
}

#[test]
fn folders_and_file_names() {
    assert_eq!(
        work_item_folder("Receiving flow: v2 (draft)", "0123456789abcdef"),
        "receiving-flow-v2-draft-01234567"
    );
    assert_eq!(work_item_folder("***", "abc"), "abc");
    assert_eq!(chat_folder("chat/../x"), "_chats/chat_.._x");
    assert_eq!(
        tagged_file_name("flow.fragment.html", "ab12"),
        "flow-ab12.fragment.html"
    );
    assert_eq!(tagged_file_name("plan.md", "ab12"), "plan-ab12.md");
    assert_eq!(tagged_file_name("README", "ab12"), "README-ab12");
    assert_eq!(content_hash(b"abc").len(), 64);
}

#[test]
fn artifact_links_find_ids_and_paths() {
    let id = "0123456789abcdef0123456789abcdef";
    let text = format!(
        "See [Receiving plan](zeron-artifact:{id}) and [Flow](zeron-artifact:/tmp/flow.fragment.html \"x\")\n\
         plus [rel](zeron-artifact:<work/my%20plan.md>) and again [dup](zeron-artifact:{id}).\n\
         Not a link: zeron-artifact:/nope"
    );
    let links = artifact_links(&text);
    assert_eq!(links.len(), 3, "{links:?}");
    assert!(links[0].is_id());
    assert_eq!(links[0].label, "Receiving plan");
    assert_eq!(links[1].target, "/tmp/flow.fragment.html");
    assert!(!links[1].is_id());
    assert_eq!(links[2].target, "work/my plan.md");
}

#[test]
fn options_manifests_are_validated() {
    let manifest = OptionsManifest::parse(
        r#"{"title":"Layouts","question":"Q-2","options":[
            {"id":"A","label":"Dense","path":"receiving.option-a.fragment.html"},
            {"id":"B","label":"Grouped","path":"sub/b.fragment.html"}]}"#,
    )
    .unwrap();
    assert_eq!(manifest.options.len(), 2);
    assert_eq!(manifest.question.as_deref(), Some("Q-2"));
    for bad in [
        r#"{"options":[{"id":"A","path":"../x.html"}]}"#,
        r#"{"options":[{"id":"A","path":"/abs.html"}]}"#,
        r#"{"options":[]}"#,
        "not json",
    ] {
        assert!(OptionsManifest::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn counts_open_questions_and_unverified_claims() {
    let text = "\
# Plan

> [!question] Q-1 · Merge by MPN?
> blocking: true
> status: open

> [!question] Q-2 · Old one
> status: resolved
> decision: yes

> [!question] Q-3 · No status line

<div data-zeron-question=\"Q-4\" data-status=\"open\">x</div>
<div data-zeron-question=\"Q-5\" data-status=\"resolved\">y</div>

Latency is fine {unverified}. See [doc](https://x \"unverified\") and
<span data-claim=\"unverified\">z</span> but [m](a.rs:1 \"measured\").
";
    assert_eq!(count_open_questions(text), 3);
    assert_eq!(count_unverified_claims(text), 3);
}

#[test]
fn snapshot_helpers_pick_the_plan_first() {
    let mut plan = view();
    let mut other = view();
    other.id = "b".repeat(32);
    other.pinned_as_plan = false;
    plan.title = "Plan".into();
    let snapshot = ArtifactsSnapshot {
        space_id: "space-1".into(),
        revision: 1,
        artifacts: vec![other.clone(), plan.clone()],
    };
    let listed = snapshot.for_work_item("item-1");
    assert_eq!(listed[0].id, plan.id);
    assert_eq!(snapshot.plan_of("item-1").unwrap().id, plan.id);
    assert_eq!(snapshot.for_chat("chat-1").len(), 2);
    assert_eq!(plan.url(), format!("zeron-artifact:{}", plan.id));
    assert_eq!(plan.current().unwrap().id, 2);
}
