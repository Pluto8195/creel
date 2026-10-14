//! Artifact UI tests (GUI-free logic and headless gpui views).
use std::sync::Arc;

use zeron_proto::{ArtifactKind, ArtifactRevision, ArtifactView, ArtifactsSnapshot};

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
