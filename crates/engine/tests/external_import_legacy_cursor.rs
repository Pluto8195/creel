//! Import cursors written by builds that still classified chats carry
//! `category`/`origin`/`classifierVersion`/`classifierSource`/
//! `jevInconclusiveVersion`/`toolCounts`/`skillsLoaded` (and `manual`-style
//! overrides). Nothing reads or writes those fields any more, but such a
//! cursor must still load, keep driving sync, and survive every rewrite with
//! the legacy fields intact (no stored data is lost).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;
use mast_engine::{EngineCore, EngineProfile, HarnessId, default_registry};

fn assemble(profile: EngineProfile) -> EngineCore {
    EngineCore::assemble_with_profile(profile, Arc::new(default_registry()), HarnessId::Mock, None)
        .expect("assemble profile")
}

fn user_line(uuid: &str, parent: &str, ts: &str, text: &str) -> String {
    format!(
        r#"{{"parentUuid":"{parent}","isSidechain":false,"type":"user","message":{{"role":"user","content":"{text}"}},"uuid":"{uuid}","timestamp":"{ts}","cwd":"/work/project","sessionId":"sess-1"}}"#
    )
}

fn assistant_line(uuid: &str, parent: &str, ts: &str, text: &str) -> String {
    format!(
        r#"{{"parentUuid":"{parent}","isSidechain":false,"type":"assistant","message":{{"model":"claude-x","content":[{{"type":"text","text":"{text}"}}]}},"uuid":"{uuid}","timestamp":"{ts}","cwd":"/work/project","sessionId":"sess-1"}}"#
    )
}

fn initial() -> String {
    [
        user_line("u1", "null", "2026-01-01T00:00:01.000Z", "hello"),
        assistant_line("a1", "u1", "2026-01-01T00:00:02.000Z", "hi"),
    ]
    .join("\n")
        + "\n"
}

/// One new human message: closes a1's turn, so exactly u2 is safe to sync.
fn growth() -> String {
    user_line("u2", "a1", "2026-01-01T00:00:03.000Z", "more please") + "\n"
}

fn cursor_path(root: &Path, chat: &str) -> PathBuf {
    root.join("external_import_cursors")
        .join(format!("{chat}.json"))
}

fn read_cursor(root: &Path, chat: &str) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(cursor_path(root, chat)).unwrap()).unwrap()
}

fn write_cursor(root: &Path, chat: &str, v: &serde_json::Value) {
    std::fs::write(cursor_path(root, chat), serde_json::to_vec(v).unwrap()).unwrap();
}

fn message_count(root: &Path, chat: &str) -> usize {
    let store = mast_sync::DocsStore::open(root).unwrap();
    let bytes = store.load_snapshot(chat).unwrap().unwrap();
    let loro = loro::LoroDoc::new();
    loro.import(&bytes).unwrap();
    mast_doc::SessionDoc::from_doc(loro)
        .read_entries()
        .unwrap()
        .len()
}

/// The classification bookkeeping an old build left in a cursor, plus a
/// field no build has ever heard of.
fn legacy_fields() -> serde_json::Value {
    json!({
        "category": "debug",
        "origin": "sdk_driven",
        "classifierVersion": 7,
        "classifierSource": "manual",
        "jevInconclusiveVersion": 7,
        "toolCounts": { "Edit": 2, "Read": 5 },
        "skillsLoaded": ["code-review", "sentry-api"],
        "manual": true,
        "someFutureField": { "nested": [1, 2, 3] },
    })
}

#[tokio::test]
async fn cursor_with_legacy_classification_fields_loads_syncs_and_keeps_them() {
    let dir = tempfile::tempdir().unwrap();
    let profile = EngineProfile::development(dir.path(), "dev-org", "dev-user");
    let root = profile.store_root().to_path_buf();
    let core = assemble(profile);
    let path = dir.path().join("s.jsonl");
    std::fs::write(&path, initial()).unwrap();
    core.external_import
        .import("legacy-chat", "sess-1", &path)
        .unwrap();

    // A fresh import writes none of the classification fields.
    let fresh = read_cursor(&root, "legacy-chat");
    for key in [
        "category",
        "origin",
        "classifierVersion",
        "classifierSource",
        "jevInconclusiveVersion",
        "toolCounts",
        "skillsLoaded",
    ] {
        assert!(fresh.get(key).is_none(), "fresh cursor wrote {key}");
    }

    // Graft the legacy fields onto the stored cursor, plus a stale native
    // classification sidecar that nothing reads any more.
    let mut cursor = fresh.clone();
    for (key, value) in legacy_fields().as_object().unwrap() {
        cursor[key] = value.clone();
    }
    write_cursor(&root, "legacy-chat", &cursor);
    let sidecars = root.join("native_chat_classifications");
    std::fs::create_dir_all(&sidecars).unwrap();
    std::fs::write(sidecars.join("legacy-chat.json"), b"{ not even json").unwrap();

    // Still loads: the importer resolves the transcript through the cursor.
    assert_eq!(
        core.external_import
            .transcript_path_for("legacy-chat")
            .unwrap(),
        Some(path.clone())
    );

    // A sync rewrites the cursor: unrelated state advances, legacy survives.
    let mut f = std::fs::read_to_string(&path).unwrap();
    f.push_str(&growth());
    std::fs::write(&path, f).unwrap();
    let result = core.external_import.sync("legacy-chat").unwrap();
    assert_eq!(result.new_message_count, 1);
    assert_eq!(message_count(&root, "legacy-chat"), 3);

    let rewritten = read_cursor(&root, "legacy-chat");
    for (key, value) in legacy_fields().as_object().unwrap() {
        assert_eq!(&rewritten[key], value, "legacy field {key} was lost");
    }
    assert!(rewritten["linesConsumed"].as_u64() > cursor["linesConsumed"].as_u64());
    assert_eq!(rewritten["transcriptPath"], cursor["transcriptPath"]);
    assert_eq!(rewritten["externalSessionId"], "sess-1");
    assert!(rewritten["lastSyncedLen"].is_u64());

    // The no-new-content rewrite path preserves them too, and the stale
    // sidecar was left alone.
    assert_eq!(
        core.external_import
            .sync("legacy-chat")
            .unwrap()
            .new_message_count,
        0
    );
    let again = read_cursor(&root, "legacy-chat");
    for (key, value) in legacy_fields().as_object().unwrap() {
        assert_eq!(&again[key], value, "legacy field {key} was lost");
    }
    assert!(sidecars.join("legacy-chat.json").exists());
    core.shutdown().await;
}
