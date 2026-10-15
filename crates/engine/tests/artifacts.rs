//! Planning artifacts (phase 1) against a real offline engine and temp git
//! repositories: location (primary repository main checkout, never a
//! worktree; submodule exclude via `--git-path`), publish / revisions /
//! idempotency / identity, plan pinning and `artifact_get_plan`, fragment
//! and preview pairs, options manifests, the marker fallback, the watcher,
//! promote, register, render-check routing, and agent scoping.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use zeron_engine::agent_tools::AgentTokenLease;
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_proto::artifacts::{EDITED_WITHOUT_NOTE, IMPORTED_NOTE};
use zeron_proto::{ArtifactKind, ArtifactView, ArtifactsSnapshot, HarnessId, derive_id, roles};
use zeron_rpc::{RpcClient, methods};

const SPACE: &str = "space-1";
const CHAT: &str = "chat-agent";
const OTHER_CHAT: &str = "chat-other";
const LONE_CHAT: &str = "chat-lone";

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn git_init(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@example.com"]);
    git(dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("README.md"), "readme\n").unwrap();
    git(dir, &["add", "README.md"]);
    git(dir, &["commit", "-q", "-m", "root"]);
}

struct Rig {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    core: EngineCore,
    client: RpcClient,
    /// The Space folder: a git repository's main checkout.
    space_path: PathBuf,
    /// A linked worktree of the Space repository (the agent's cwd).
    worktree: PathBuf,
    item: String,
    other_item: String,
    _leases: Vec<AgentTokenLease>,
    token: String,
    other_token: String,
    lone_token: String,
}

async fn mutate(client: &RpcClient, params: Value) {
    client.call(methods::MUTATE, params).await.unwrap();
}

async fn outline(client: &RpcClient, params: Value) -> Value {
    client
        .call(methods::WORK_OUTLINE_MUTATE, params)
        .await
        .unwrap()
}

async fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let space_path = root.join("project");
    git_init(&space_path);
    let worktree = root.join("project-wt");
    git(
        &space_path,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent",
            worktree.to_str().unwrap(),
        ],
    );
    let core = EngineCore::assemble(
        &root.join("data"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .expect("engine core assembles");
    let client = zeron_rpc::memory_client(core.rpc_service());
    mutate(
        &client,
        json!({ "op": "createSpace", "spaceId": SPACE, "deviceId": core.device_id,
                "path": space_path.to_string_lossy() }),
    )
    .await;
    for chat in [CHAT, OTHER_CHAT, LONE_CHAT] {
        mutate(
            &client,
            json!({ "op": "createChat", "chatId": chat, "spaceId": SPACE,
                    "deviceId": core.device_id, "cwd": worktree.to_string_lossy() }),
        )
        .await;
    }
    outline(
        &client,
        json!({ "op": "createWorkItem", "spaceId": SPACE, "title": "Receiving flow: v2",
                "operationId": "op-item" }),
    )
    .await;
    outline(
        &client,
        json!({ "op": "createWorkItem", "spaceId": SPACE, "title": "Other work",
                "operationId": "op-other" }),
    )
    .await;
    let item = derive_id("op-item", roles::ITEM);
    let other_item = derive_id("op-other", roles::ITEM);
    for (chat, target, op) in [
        (CHAT, &item, "op-assign-1"),
        (OTHER_CHAT, &other_item, "op-assign-2"),
    ] {
        outline(
            &client,
            json!({ "op": "assignChatToWorkItem", "chatId": chat, "workItemId": target,
                    "operationId": op }),
        )
        .await;
    }
    let tokens = core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-1", SPACE, &core.device_id);
    let other = tokens.mint(OTHER_CHAT, "run-2", SPACE, &core.device_id);
    let lone = tokens.mint(LONE_CHAT, "run-3", SPACE, &core.device_id);
    let token = lease.token().to_owned();
    let other_token = other.token().to_owned();
    let lone_token = lone.token().to_owned();
    Rig {
        _tmp: tmp,
        root,
        core,
        client,
        space_path,
        worktree,
        item,
        other_item,
        _leases: vec![lease, other, lone],
        token,
        other_token,
        lone_token,
    }
}

async fn tool(rig: &Rig, token: &str, name: &str, arguments: Value) -> Result<Value, String> {
    rig.client
        .call(
            zeron_proto::agent_tools::TOOL_CALL_METHOD,
            json!({ "token": token, "tool": name, "arguments": arguments }),
        )
        .await
        .map_err(|e| e.to_string())
}

async fn publish(rig: &Rig, token: &str, arguments: Value) -> Value {
    tool(rig, token, "artifact_publish", arguments)
        .await
        .unwrap()
}

fn artifact(rig: &Rig, id: &str) -> ArtifactView {
    rig.core.workspace.artifact(id).expect("artifact row")
}

/// `<profile store>/artifacts/<id>/<hash>` somewhere under the data dir.
fn find_snapshot(dir: &Path, id: &str, hash: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_snapshot(&path, id, hash) {
                return Some(found);
            }
        } else if path.file_name().and_then(|n| n.to_str()) == Some(hash)
            && path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()) == Some(id)
        {
            return Some(path);
        }
    }
    None
}

fn item_folder(rig: &Rig) -> PathBuf {
    rig.space_path.join(".zeron/artifacts").join(format!(
        "receiving-flow-v2-{}",
        &rig.item[..8]
    ))
}

#[tokio::test(flavor = "multi_thread")]
async fn publish_copies_into_the_main_checkout_and_records_revisions_idempotently() {
    let rig = rig().await;
    // The agent writes in its worktree; the artifact lands in the MAIN
    // checkout (the Space repository), never the worktree.
    let source = rig.worktree.join("work/plan.md");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, "# Plan v1\n").unwrap();
    let first = publish(
        &rig,
        &rig.token,
        json!({ "path": "work/plan.md", "title": "Receiving flow — plan", "role": "plan",
                "note": "first cut", "idempotencyKey": "plan-1" }),
    )
    .await;
    let id = first["artifactId"].as_str().unwrap().to_owned();
    assert_eq!(first["revision"], 1);
    assert_eq!(first["url"], format!("zeron-artifact:{id}"));
    assert_eq!(first["changed"], true);
    let stored = PathBuf::from(first["storedPath"].as_str().unwrap());
    assert_eq!(stored, item_folder(&rig).join("plan.md"));
    assert!(!stored.starts_with(&rig.worktree));
    assert_eq!(std::fs::read_to_string(&stored).unwrap(), "# Plan v1\n");
    // `.zeron/` is excluded once and the checkout stays clean.
    let exclude = std::fs::read_to_string(rig.space_path.join(".git/info/exclude")).unwrap();
    assert_eq!(exclude.matches(".zeron/").count(), 1, "{exclude}");
    assert!(!rig.space_path.join(".gitignore").exists());
    assert_eq!(git(&rig.space_path, &["status", "--porcelain"]), "");

    let row = artifact(&rig, &id);
    assert_eq!(row.work_item_id.as_deref(), Some(rig.item.as_str()));
    assert_eq!(row.chat_id.as_deref(), Some(CHAT));
    assert_eq!(row.kind, ArtifactKind::Markdown);
    assert_eq!(row.role.as_deref(), Some("plan"));
    assert_eq!(row.revisions[0].note, "first cut");
    assert_eq!(row.revisions[0].by.chat_id.as_deref(), Some(CHAT));
    assert_eq!(
        row.origin_path.as_deref(),
        Some(source.to_str().unwrap())
    );
    let snapshot = find_snapshot(&rig.root.join("data"), &id, &row.revisions[0].hash);
    assert!(snapshot.is_some(), "no profile-store snapshot for {id}");
    assert!(
        snapshot
            .unwrap()
            .ends_with(Path::new("artifacts").join(&id).join(&row.revisions[0].hash))
    );

    // A retry with the same key returns the first result and writes nothing,
    // even though the file changed meanwhile.
    std::fs::write(&source, "# Plan v2\n").unwrap();
    let retry = publish(
        &rig,
        &rig.token,
        json!({ "path": "work/plan.md", "title": "Receiving flow — plan", "role": "plan",
                "note": "first cut", "idempotencyKey": "plan-1" }),
    )
    .await;
    assert_eq!(retry["artifactId"], id.as_str());
    assert_eq!(retry["revision"], 1);
    assert_eq!(std::fs::read_to_string(&stored).unwrap(), "# Plan v1\n");

    // A new key with new content: revision 2 of the SAME artifact (same
    // title and role on the same item, title normalized).
    let second = publish(
        &rig,
        &rig.token,
        json!({ "path": source.to_string_lossy(), "title": "  receiving FLOW — plan ",
                "role": "plan", "note": "v2", "idempotencyKey": "plan-2" }),
    )
    .await;
    assert_eq!(second["artifactId"], id.as_str());
    assert_eq!(second["revision"], 2);
    assert_eq!(std::fs::read_to_string(&stored).unwrap(), "# Plan v2\n");
    // Unchanged content: no revision.
    let same = publish(
        &rig,
        &rig.token,
        json!({ "path": "work/plan.md", "title": "Receiving flow — plan", "role": "plan",
                "note": "noop", "idempotencyKey": "plan-3" }),
    )
    .await;
    assert_eq!(same["revision"], 2);
    assert_eq!(same["changed"], false);
    // Another role: another artifact, its file name kept unique in the folder.
    let doc = publish(
        &rig,
        &rig.token,
        json!({ "path": "work/plan.md", "title": "Receiving flow — plan", "role": "doc",
                "note": "copy", "idempotencyKey": "doc-1" }),
    )
    .await;
    assert_ne!(doc["artifactId"], id.as_str());
    let doc_stored = PathBuf::from(doc["storedPath"].as_str().unwrap());
    assert_eq!(doc_stored.parent(), stored.parent());
    assert_ne!(doc_stored, stored);

    // Content reads: current and revision 1.
    let current: zeron_proto::ArtifactContentReply = serde_json::from_value(
        rig.client
            .call(methods::ARTIFACT_CONTENT, json!({ "artifactId": id }))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(current.content, "# Plan v2\n");
    assert_eq!(current.revision, 2);
    let old: zeron_proto::ArtifactContentReply = serde_json::from_value(
        rig.client
            .call(
                methods::ARTIFACT_CONTENT,
                json!({ "artifactId": id, "revision": 1 }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(old.content, "# Plan v1\n");
    assert_eq!(old.mime_type, "text/markdown");

    // A second artifact never re-appends the exclude line.
    let exclude = std::fs::read_to_string(rig.space_path.join(".git/info/exclude")).unwrap();
    assert_eq!(exclude.matches(".zeron/").count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn location_follows_the_primary_binding_and_submodules_exclude_via_git_path() {
    let rig = rig().await;
    // A submodule of the Space repository, bound as the item's primary
    // repository: artifacts land in the submodule's checkout, and the
    // exclude goes to the module's own info/exclude.
    let upstream = rig.root.join("upstream");
    git_init(&upstream);
    git(
        &rig.space_path,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            upstream.to_str().unwrap(),
            "lib",
        ],
    );
    git(&rig.space_path, &["commit", "-q", "-m", "add lib"]);
    let sub = rig.space_path.join("lib");
    outline(
        &rig.client,
        json!({ "op": "configureExecution",
                "scope": { "type": "workItem", "workItemId": rig.item },
                "configuration": {
                    "executionRoot": rig.space_path.to_string_lossy(),
                    "bindings": [{ "repoId": "repo:lib", "repoFingerprint": "",
                                   "repoPath": sub.to_string_lossy(),
                                   "member": true, "primary": true }],
                },
                "operationId": "op-config" }),
    )
    .await;
    let source = rig.worktree.join("diagram.mmd");
    std::fs::write(&source, "graph TD; A-->B").unwrap();
    let reply = publish(
        &rig,
        &rig.token,
        json!({ "path": source.to_string_lossy(), "title": "States", "note": "n",
                "idempotencyKey": "k" }),
    )
    .await;
    let stored = PathBuf::from(reply["storedPath"].as_str().unwrap());
    assert!(stored.starts_with(sub.join(".zeron/artifacts")), "{}", stored.display());
    assert_eq!(artifact(&rig, reply["artifactId"].as_str().unwrap()).kind, ArtifactKind::Mermaid);
    let module_exclude = rig.space_path.join(".git/modules/lib/info/exclude");
    let text = std::fs::read_to_string(&module_exclude).unwrap();
    assert_eq!(text.matches(".zeron/").count(), 1, "{text}");
    assert_eq!(git(&sub, &["status", "--porcelain"]), "");
    assert!(!sub.join(".gitignore").exists());
    // The superproject's own exclude was not needed.
    let top = std::fs::read_to_string(rig.space_path.join(".git/info/exclude"))
        .unwrap_or_default();
    assert!(!top.contains(".zeron/"));
}

#[tokio::test(flavor = "multi_thread")]
async fn pinning_a_plan_unpins_the_previous_one_and_get_plan_reads_it() {
    let rig = rig().await;
    assert_eq!(
        tool(&rig, &rig.token, "artifact_get_plan", json!({}))
            .await
            .unwrap()["artifactId"],
        Value::Null
    );
    let a = rig.worktree.join("a.md");
    let b = rig.worktree.join("b.md");
    std::fs::write(&a, "# A\n").unwrap();
    std::fs::write(
        &b,
        "# B\n\n> [!question] Q-1 · Merge?\n> blocking: true\n> status: open\n\nFast {unverified}.\n",
    )
    .unwrap();
    let first = publish(
        &rig,
        &rig.token,
        json!({ "path": a.to_string_lossy(), "title": "Plan A", "role": "plan",
                "note": "n", "pinAsPlan": true, "idempotencyKey": "a" }),
    )
    .await;
    let second = publish(
        &rig,
        &rig.token,
        json!({ "path": b.to_string_lossy(), "title": "Plan B", "role": "plan",
                "note": "n", "pinAsPlan": true, "idempotencyKey": "b" }),
    )
    .await;
    let a_id = first["artifactId"].as_str().unwrap();
    let b_id = second["artifactId"].as_str().unwrap();
    assert!(!artifact(&rig, a_id).pinned_as_plan);
    assert!(artifact(&rig, b_id).pinned_as_plan);
    let plan = tool(&rig, &rig.token, "artifact_get_plan", json!({}))
        .await
        .unwrap();
    assert_eq!(plan["artifactId"], b_id);
    assert_eq!(plan["title"], "Plan B");
    assert_eq!(plan["revision"], 1);
    assert!(plan["content"].as_str().unwrap().starts_with("# B"));
    assert_eq!(plan["openQuestions"], 1);
    assert_eq!(plan["unverifiedClaims"], 1);
    assert_eq!(plan["storedPath"], second["storedPath"]);
    // Another item's chat sees no plan.
    assert_eq!(
        tool(&rig, &rig.other_token, "artifact_get_plan", json!({}))
            .await
            .unwrap()["artifactId"],
        Value::Null
    );
    // The user pins A back over RPC.
    let reply = rig
        .client
        .call(
            methods::ARTIFACT_PIN,
            json!({ "artifactId": a_id, "pinned": true }),
        )
        .await
        .unwrap();
    assert_eq!(reply["unpinned"], json!([b_id]));
    assert_eq!(
        tool(&rig, &rig.token, "artifact_get_plan", json!({}))
            .await
            .unwrap()["artifactId"],
        a_id
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn fragments_bring_their_preview_and_options_bring_their_fragments() {
    let rig = rig().await;
    let dir = rig.worktree.join("viz");
    std::fs::create_dir_all(dir.join("opts")).unwrap();
    std::fs::write(dir.join("flow.fragment.html"), "<div>flow</div>").unwrap();
    std::fs::write(
        dir.join("flow.preview.html"),
        "<html><body>flow</body></html>",
    )
    .unwrap();
    // Publishing the preview publishes the fragment (with its preview).
    let reply = publish(
        &rig,
        &rig.token,
        json!({ "path": dir.join("flow.preview.html").to_string_lossy(), "title": "Flow",
                "role": "diagram", "note": "n", "idempotencyKey": "flow" }),
    )
    .await;
    let row = artifact(&rig, reply["artifactId"].as_str().unwrap());
    assert_eq!(row.kind, ArtifactKind::HtmlFragment);
    assert!(row.stored_path.ends_with("flow.fragment.html"));
    let preview = row.preview_path.clone().expect("preview path");
    assert!(preview.ends_with("flow.preview.html"));
    assert_eq!(
        std::fs::read_to_string(&preview).unwrap(),
        "<html><body>flow</body></html>"
    );
    assert_eq!(
        std::fs::read_to_string(&row.stored_path).unwrap(),
        "<div>flow</div>"
    );
    // The preview is readable as a companion.
    let companion = rig
        .client
        .call(
            methods::ARTIFACT_CONTENT,
            json!({ "artifactId": row.id, "companion": "flow.preview.html" }),
        )
        .await
        .unwrap();
    assert_eq!(companion["content"], "<html><body>flow</body></html>");

    // Options: the manifest's fragments are copied next to it.
    std::fs::write(dir.join("opts/a.fragment.html"), "<p>A</p>").unwrap();
    std::fs::write(dir.join("b.fragment.html"), "<p>B</p>").unwrap();
    std::fs::write(
        dir.join("layouts.json"),
        r#"{"title":"Layouts","question":"Q-2","options":[
            {"id":"A","label":"Dense","path":"opts/a.fragment.html"},
            {"id":"B","label":"Grouped","path":"b.fragment.html"}]}"#,
    )
    .unwrap();
    let reply = publish(
        &rig,
        &rig.token,
        json!({ "path": dir.join("layouts.json").to_string_lossy(), "title": "Layouts",
                "kind": "options", "role": "comparison", "note": "n",
                "idempotencyKey": "opts" }),
    )
    .await;
    let row = artifact(&rig, reply["artifactId"].as_str().unwrap());
    assert_eq!(row.kind, ArtifactKind::Options);
    assert_eq!(
        row.companions,
        vec!["opts/a.fragment.html".to_owned(), "b.fragment.html".to_owned()]
    );
    let folder = Path::new(&row.stored_path).parent().unwrap();
    assert_eq!(
        std::fs::read_to_string(folder.join("opts/a.fragment.html")).unwrap(),
        "<p>A</p>"
    );
    assert_eq!(
        std::fs::read_to_string(folder.join("b.fragment.html")).unwrap(),
        "<p>B</p>"
    );
    // A manifest escaping its folder is refused.
    std::fs::write(
        dir.join("bad.json"),
        r#"{"options":[{"id":"A","path":"../x.html"}]}"#,
    )
    .unwrap();
    let err = tool(
        &rig,
        &rig.token,
        "artifact_publish",
        json!({ "path": dir.join("bad.json").to_string_lossy(), "title": "Bad",
                "kind": "options", "note": "n", "idempotencyKey": "bad" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("artifact/invalid"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn marker_links_and_visualize_references_register_once() {
    let rig = rig().await;
    let file = rig.worktree.join("notes/plan.md");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "# Notes\n").unwrap();
    let fragment = rig.worktree.join("viz.fragment.html");
    std::fs::write(&fragment, "<div>v</div>").unwrap();
    std::fs::write(rig.worktree.join(".env"), "K=v").unwrap();
    let text = format!(
        "Here is [Rollout notes](zeron-artifact:notes/plan.md) and \
         [missing](zeron-artifact:/nope/x.md) and [secret](zeron-artifact:.env).\n\
         visualize{{\"path\":\"{}\",\"title\":\"Viz\"}}\n",
        fragment.display()
    );
    assert_eq!(rig.core.artifacts.import_markers(CHAT, &text), 2);
    let all = rig.core.workspace.artifacts_of_space(SPACE);
    assert_eq!(all.len(), 2, "{all:?}");
    let notes = all.iter().find(|a| a.title == "Rollout notes").unwrap();
    assert_eq!(notes.work_item_id.as_deref(), Some(rig.item.as_str()));
    assert_eq!(notes.revisions[0].note, IMPORTED_NOTE);
    let viz = all.iter().find(|a| a.title == "Viz").unwrap();
    assert_eq!(viz.kind, ArtifactKind::HtmlFragment);
    // Once per (chat, path): a later message linking it again imports
    // nothing, even after the file changed.
    std::fs::write(&file, "# Notes v2\n").unwrap();
    assert_eq!(rig.core.artifacts.import_markers(CHAT, &text), 0);
    // An id link resolves to the existing artifact: nothing is registered.
    let by_id = format!("[Rollout notes](zeron-artifact:{})", notes.id);
    assert_eq!(rig.core.artifacts.import_markers(CHAT, &by_id), 0);
    assert_eq!(rig.core.workspace.artifacts_of_space(SPACE).len(), 2);
    // A chat without a work item gets `_chats/<chatId>`.
    let lone = format!("[Lone](zeron-artifact:{})", file.display());
    assert_eq!(rig.core.artifacts.import_markers(LONE_CHAT, &lone), 1);
    let lone_row = rig
        .core
        .workspace
        .artifacts_of_space(SPACE)
        .into_iter()
        .find(|a| a.title == "Lone")
        .unwrap();
    assert!(lone_row.work_item_id.is_none());
    assert_eq!(lone_row.folder, format!("_chats/{LONE_CHAT}"));
    assert!(
        lone_row
            .stored_path
            .starts_with(rig.space_path.join(".zeron/artifacts/_chats").to_str().unwrap())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn external_edits_become_revisions_without_a_note() {
    let rig = rig().await;
    let source = rig.worktree.join("plan.md");
    std::fs::write(&source, "v1\n").unwrap();
    let reply = publish(
        &rig,
        &rig.token,
        json!({ "path": "plan.md", "title": "Plan", "note": "n", "idempotencyKey": "k" }),
    )
    .await;
    let id = reply["artifactId"].as_str().unwrap();
    // Zeron's own write is not an edit.
    assert_eq!(rig.core.artifacts.scan_external_edits(), 0);
    std::fs::write(reply["storedPath"].as_str().unwrap(), "edited by hand\n").unwrap();
    assert_eq!(rig.core.artifacts.scan_external_edits(), 1);
    let row = artifact(&rig, id);
    assert_eq!(row.current_revision, 2);
    assert_eq!(row.revisions[0].note, EDITED_WITHOUT_NOTE);
    assert_eq!(
        row.revisions[0].by.kind,
        zeron_proto::WorkOutlineActorKind::System
    );
    assert_eq!(rig.core.artifacts.scan_external_edits(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn promote_copies_into_docs_plans_without_overwriting() {
    let rig = rig().await;
    let source = rig.worktree.join("plan.md");
    std::fs::write(&source, "v1\n").unwrap();
    let reply = publish(
        &rig,
        &rig.token,
        json!({ "path": "plan.md", "title": "Plan", "note": "n", "idempotencyKey": "k1" }),
    )
    .await;
    let id = reply["artifactId"].as_str().unwrap();
    let promoted = rig
        .client
        .call(methods::ARTIFACT_PROMOTE, json!({ "artifactId": id }))
        .await
        .unwrap();
    let target = rig.space_path.join("docs/plans/plan.md");
    assert_eq!(promoted["promotedPath"], target.to_string_lossy().as_ref());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v1\n");
    // Uncommitted.
    assert!(git(&rig.space_path, &["status", "--porcelain"]).contains("docs/"));
    // Same content again: same file.
    let again = rig
        .client
        .call(methods::ARTIFACT_PROMOTE, json!({ "artifactId": id }))
        .await
        .unwrap();
    assert_eq!(again["promotedPath"], promoted["promotedPath"]);
    // New content: the existing file is kept, a suffixed copy is written.
    std::fs::write(&source, "v2\n").unwrap();
    publish(
        &rig,
        &rig.token,
        json!({ "path": "plan.md", "title": "Plan", "note": "n", "idempotencyKey": "k2" }),
    )
    .await;
    let third = rig
        .client
        .call(methods::ARTIFACT_PROMOTE, json!({ "artifactId": id }))
        .await
        .unwrap();
    let suffixed = rig.space_path.join("docs/plans/plan-2.md");
    assert_eq!(third["promotedPath"], suffixed.to_string_lossy().as_ref());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v1\n");
    assert_eq!(std::fs::read_to_string(&suffixed).unwrap(), "v2\n");
    assert_eq!(
        artifact(&rig, id).promoted_path.as_deref(),
        Some(suffixed.to_str().unwrap())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn register_imports_an_existing_space_file_as_revision_one() {
    let rig = rig().await;
    let file = rig.space_path.join(".local/old-plan.md");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "# Old\n").unwrap();
    let reply = rig
        .client
        .call(
            methods::ARTIFACT_REGISTER,
            json!({ "spaceId": SPACE, "workItemId": rig.item, "path": ".local/old-plan.md",
                    "role": "plan" }),
        )
        .await
        .unwrap();
    assert_eq!(reply["revision"], 1);
    let row = artifact(&rig, reply["artifactId"].as_str().unwrap());
    assert_eq!(row.title, "old-plan");
    assert_eq!(row.origin_path.as_deref(), Some(file.to_str().unwrap()));
    assert_eq!(row.revisions[0].note, IMPORTED_NOTE);
    assert_eq!(
        row.revisions[0].by.kind,
        zeron_proto::WorkOutlineActorKind::User
    );
    assert!(row.stored_path.starts_with(item_folder(&rig).to_str().unwrap()));
    // Outside the Space: refused.
    let outside = rig.root.join("elsewhere.md");
    std::fs::write(&outside, "x").unwrap();
    // Outside the Space: allowed (any regular local file) ...
    let reply = rig
        .client
        .call(
            methods::ARTIFACT_REGISTER,
            json!({ "spaceId": SPACE, "path": outside.to_string_lossy(),
                    "workItemId": rig.item }),
        )
        .await
        .unwrap();
    let row = artifact(&rig, reply["artifactId"].as_str().unwrap());
    assert_eq!(row.title, "elsewhere");
    assert!(row.stored_path.starts_with(item_folder(&rig).to_str().unwrap()));
    // ... unless denied.
    let secret = rig.root.join(".env");
    std::fs::write(&secret, "K=v").unwrap();
    let err = rig
        .client
        .call(
            methods::ARTIFACT_REGISTER,
            json!({ "spaceId": SPACE, "path": secret.to_string_lossy() }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("artifact/denied"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn render_checks_route_to_the_ui_or_report_unavailable() {
    let rig = rig().await;
    rig.core
        .artifacts
        .render()
        .set_timeout(Duration::from_secs(5));
    let source = rig.worktree.join("flow.fragment.html");
    std::fs::write(&source, "<div>flow</div>").unwrap();
    let reply = publish(
        &rig,
        &rig.token,
        json!({ "path": "flow.fragment.html", "title": "Flow", "note": "n",
                "idempotencyKey": "k" }),
    )
    .await;
    let id = reply["artifactId"].as_str().unwrap().to_owned();

    // No UI: renderUnavailable.
    let err = tool(
        &rig,
        &rig.token,
        "artifact_render_check",
        json!({ "artifactId": id }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("artifact/renderUnavailable"), "{err}");

    // A fake headed UI subscribes and answers.
    let ui = zeron_rpc::memory_client(rig.core.rpc_service());
    let mut requests = ui
        .subscribe(methods::WATCH_ARTIFACT_RENDER_REQUESTS, json!({ "client": "test-ui" }))
        .await
        .unwrap();
    let responder = tokio::spawn(async move {
        let item = requests.recv().await.unwrap();
        let request: zeron_proto::ArtifactRenderRequest = serde_json::from_value(item).unwrap();
        assert_eq!(request.kind, ArtifactKind::HtmlFragment);
        assert_eq!(request.content, "<div>flow</div>");
        assert_eq!(request.viewport.width, 900);
        let accepted = ui
            .call(
                methods::ARTIFACT_RENDER_RESULT,
                json!({ "requestId": request.request_id,
                        "result": { "screenshotPngBase64": "iVBORw0KGgo=",
                                    "errors": [], "warnings": ["horizontal overflow"] } }),
            )
            .await
            .unwrap();
        assert_eq!(accepted["accepted"], true);
        (ui, requests)
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while rig.core.artifacts.render().subscriber_count() == 0 {
        assert!(tokio::time::Instant::now() < deadline, "UI never registered");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let result = tool(
        &rig,
        &rig.token,
        "artifact_render_check",
        json!({ "artifactId": id, "viewport": { "width": 900, "height": 600 } }),
    )
    .await
    .unwrap();
    assert_eq!(result["artifactId"], id.as_str());
    assert_eq!(result["warnings"][0], "horizontal overflow");
    assert_eq!(result["screenshot"]["mimeType"], "image/png");
    assert_eq!(result["screenshot"]["data"], "iVBORw0KGgo=");
    let (_ui, _requests) = responder.await.unwrap();

    // Another item's chat may not check this artifact.
    let err = tool(
        &rig,
        &rig.other_token,
        "artifact_render_check",
        json!({ "artifactId": id }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("artifact/forbidden"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn agents_are_scoped_to_their_chat_and_folders() {
    let rig = rig().await;
    // Directories are not regular files.
    let err = tool(
        &rig,
        &rig.token,
        "artifact_publish",
        json!({ "path": "/etc", "title": "Etc", "note": "n", "idempotencyKey": "k" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("artifact/denied"), "{err}");
    // A key is required.
    std::fs::write(rig.worktree.join("p.md"), "x").unwrap();
    let err = tool(
        &rig,
        &rig.token,
        "artifact_publish",
        json!({ "path": "p.md", "title": "P", "note": "n" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("artifact/invalid"), "{err}");
    // Each chat publishes into its own item: the same title from the other
    // item's chat is a different artifact in another folder.
    let mine = publish(
        &rig,
        &rig.token,
        json!({ "path": "p.md", "title": "P", "note": "n", "idempotencyKey": "k" }),
    )
    .await;
    let theirs = publish(
        &rig,
        &rig.other_token,
        json!({ "path": "p.md", "title": "P", "note": "n", "idempotencyKey": "k" }),
    )
    .await;
    assert_ne!(mine["artifactId"], theirs["artifactId"]);
    let theirs_row = artifact(&rig, theirs["artifactId"].as_str().unwrap());
    assert_eq!(
        theirs_row.work_item_id.as_deref(),
        Some(rig.other_item.as_str())
    );
    assert!(theirs_row.folder.ends_with(&rig.other_item[..8]));
    // The lone chat (no item) cannot render-check either.
    let err = tool(
        &rig,
        &rig.lone_token,
        "artifact_render_check",
        json!({ "artifactId": mine["artifactId"] }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("artifact/forbidden"), "{err}");
    // Unknown tokens are refused before any artifact work.
    let err = tool(&rig, "zat_forged", "artifact_get_plan", json!({}))
        .await
        .unwrap_err();
    assert!(err.contains("agent-tools/unauthenticated"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_artifacts_streams_the_space_s_artifacts() {
    let rig = rig().await;
    let mut stream = rig
        .client
        .subscribe(methods::WATCH_ARTIFACTS, json!({ "spaceId": SPACE }))
        .await
        .unwrap();
    let first: ArtifactsSnapshot =
        serde_json::from_value(stream.recv().await.unwrap()).unwrap();
    assert!(first.artifacts.is_empty());
    std::fs::write(rig.worktree.join("p.md"), "x").unwrap();
    let reply = publish(
        &rig,
        &rig.token,
        json!({ "path": "p.md", "title": "P", "note": "n", "pinAsPlan": true,
                "idempotencyKey": "k" }),
    )
    .await;
    let next = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot: ArtifactsSnapshot =
                serde_json::from_value(stream.recv().await.unwrap()).unwrap();
            if snapshot.plan_of(&rig.item).is_some() {
                return snapshot;
            }
        }
    })
    .await
    .expect("a frame with the pinned artifact");
    assert!(next.revision > first.revision);
    assert_eq!(
        next.plan_of(&rig.item).unwrap().id,
        reply["artifactId"].as_str().unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn publish_accepts_any_local_file_outside_the_deny_list() {
    let rig = rig().await;
    // A fake HOME with credential folders; the file lives outside every root.
    let home = rig.root.join("home");
    for dir in [".ssh", ".aws", ".config/gh", "Library/Keychains", ".gnupg", ".docker"] {
        std::fs::create_dir_all(home.join(dir)).unwrap();
    }
    rig.core.artifacts.set_home(Some(home.clone()));
    let elsewhere = rig.root.join("notes/outside.md");
    std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
    std::fs::write(&elsewhere, "# v1\n").unwrap();
    let args = |title: &str, key: &str| {
        json!({ "path": elsewhere.to_string_lossy(), "title": title, "note": "n",
                "idempotencyKey": key })
    };
    let first = publish(&rig, &rig.token, args("Outside", "k1")).await;
    assert_eq!(first["revision"], 1);
    let row = artifact(&rig, first["artifactId"].as_str().unwrap());
    assert!(row.stored_path.starts_with(item_folder(&rig).to_str().unwrap()));
    assert_eq!(std::fs::read_to_string(&row.stored_path).unwrap(), "# v1\n");
    std::fs::write(&elsewhere, "# v2\n").unwrap();
    let second = publish(&rig, &rig.token, args("Outside", "k2")).await;
    assert_eq!(second["artifactId"], first["artifactId"]);
    assert_eq!(second["revision"], 2);

    let denied = |path: &Path| {
        let args = json!({ "path": path.to_string_lossy(), "title": "X", "note": "n",
                           "idempotencyKey": "kd" });
        let rig = &rig;
        async move { tool(rig, &rig.token, "artifact_publish", args).await.unwrap_err() }
    };
    for (rel, body) in [
        (".ssh/id_ed25519", "key"),
        (".aws/credentials", "k"),
        (".config/gh/hosts.yml", "t"),
        ("Library/Keychains/login.keychain", "k"),
        (".gnupg/pubring.kbx", "k"),
        (".docker/config.json", "{}"),
        (".netrc", "machine"),
    ] {
        let file = home.join(rel);
        std::fs::write(&file, body).unwrap();
        let err = denied(&file).await;
        assert!(err.contains("artifact/denied"), "{rel}: {err}");
    }
    // A symlink from an innocent place into the deny-list is judged by its target.
    let link = rig.root.join("notes/innocent.md");
    std::os::unix::fs::symlink(home.join(".ssh/id_ed25519"), &link).unwrap();
    let err = denied(&link).await;
    assert!(err.contains("artifact/denied") && err.contains(".ssh"), "{err}");
    // .env files.
    for name in [".env", ".env.production"] {
        let file = rig.root.join("notes").join(name);
        std::fs::write(&file, "K=v").unwrap();
        let err = denied(&file).await;
        assert!(err.contains("artifact/denied"), "{name}: {err}");
    }
    // Oversize.
    let big = rig.root.join("notes/big.md");
    let handle = std::fs::File::create(&big).unwrap();
    handle.set_len(11 * 1024 * 1024).unwrap();
    let err = denied(&big).await;
    assert!(err.contains("artifact/denied"), "{err}");
    // Non-regular (fifo).
    let fifo = rig.root.join("notes/pipe.md");
    assert!(Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    let err = denied(&fifo).await;
    assert!(err.contains("artifact/denied") && err.contains("regular"), "{err}");
}
