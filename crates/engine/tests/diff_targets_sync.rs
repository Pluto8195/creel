//! Chat diff targets through diff-sync, the turn snapshots and the RPCs, over
//! real temp git repositories: a superproject with a `flagship` submodule, a
//! linked worktree of the SUBMODULE inside the superproject's gitignored
//! `.worktrees/` folder (the cb4e2158 layout), and a linked worktree of the
//! root repository.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_proto::ChatDiffTargetSource;
use zeron_rpc::methods;

const CHAT: &str = "chat-1";

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Zeron Test",
            "-c",
            "user.email=test@zeron.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "protocol.file.allow=always",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn init_repo(path: &Path, file: &str) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q"]);
    std::fs::write(path.join(file), "x\n").unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-q", "-m", "init"]);
}

struct Fixture {
    _tmp: tempfile::TempDir,
    /// Superproject checkout (the chat's cwd).
    ws: PathBuf,
    /// `<ws>/.worktrees/flagship/eng-3012` (worktree of the submodule).
    sub_wt: PathBuf,
    /// `<ws>/.worktrees/ws/root-feature` (worktree of the root repository).
    root_wt: PathBuf,
    /// An unrelated repository.
    other: PathBuf,
    core: EngineCore,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    init_repo(&root.join("sources/flagship"), "flagship.txt");
    let ws = root.join("ws");
    init_repo(&ws, "README");
    let source = root.join("sources/flagship");
    git(
        &ws,
        &[
            "submodule",
            "add",
            "-q",
            &source.to_string_lossy(),
            "flagship",
        ],
    );
    std::fs::write(ws.join(".gitignore"), ".worktrees/\n").unwrap();
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "members"]);

    let sub_wt = ws.join(".worktrees/flagship/eng-3012");
    std::fs::create_dir_all(sub_wt.parent().unwrap()).unwrap();
    git(
        &ws.join("flagship"),
        &[
            "worktree",
            "add",
            "-q",
            "../.worktrees/flagship/eng-3012",
            "-b",
            "feat-eng-3012",
        ],
    );
    let root_wt = ws.join(".worktrees/ws/root-feature");
    std::fs::create_dir_all(root_wt.parent().unwrap()).unwrap();
    git(
        &ws,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "root-feature",
            &root_wt.to_string_lossy(),
        ],
    );
    let other = root.join("other");
    init_repo(&other, "other.txt");

    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("device-id"), "dev-a").unwrap();
    let core = EngineCore::assemble(
        &data,
        Arc::new(HarnessRegistry::new()),
        zeron_proto::HarnessId::Mock,
        None,
    )
    .expect("engine core assembles");
    core.workspace
        .create_chat(
            CHAT,
            None,
            Some("dev-a"),
            None,
            Some(ws.to_string_lossy().into_owned()),
        )
        .unwrap();
    Fixture {
        sub_wt: std::fs::canonicalize(&sub_wt).unwrap(),
        root_wt: std::fs::canonicalize(&root_wt).unwrap(),
        ws: std::fs::canonicalize(&ws).unwrap(),
        _tmp: tmp,
        other,
        core,
    }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn bind(f: &Fixture, path: &Path) {
    f.core
        .diff_targets
        .bind(CHAT, &text(path), ChatDiffTargetSource::Agent, None)
        .unwrap_or_else(|e| panic!("bind {}: {e}", path.display()));
}

async fn checkout_id(f: &Fixture, path: &Path) -> String {
    f.core.repos.checkout_identity(path).await.unwrap().id
}

fn file_paths(diff: &Value) -> Vec<String> {
    let mut paths: Vec<String> = diff["files"]
        .as_array()
        .map(|files| {
            files
                .iter()
                .map(|file| file["path"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default();
    paths.sort();
    paths
}

type Stream = tokio::sync::mpsc::Receiver<Value>;

/// Next frame satisfying `predicate` (every frame before it is skipped).
async fn next_frame(stream: &mut Stream, predicate: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let frame = tokio::time::timeout_at(deadline, stream.recv())
            .await
            .expect("expected frame before timeout")
            .expect("stream alive");
        if predicate(&frame) {
            return frame;
        }
    }
}

async fn wait_for<T>(mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(tokio::time::Instant::now() < deadline, "condition timed out");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scoped_and_branch_diffs_cover_only_the_bound_worktree() {
    let f = fixture();
    let client = zeron_rpc::memory_client(f.core.rpc_service());
    // The chat's own checkout is dirty with unrelated docs.
    std::fs::write(f.ws.join("README"), "dirty root\n").unwrap();
    // The worktree has one committed change and one uncommitted edit.
    std::fs::write(f.sub_wt.join("feature.txt"), "feature\n").unwrap();
    git(&f.sub_wt, &["add", "."]);
    git(&f.sub_wt, &["commit", "-q", "-m", "feature"]);
    std::fs::write(f.sub_wt.join("flagship.txt"), "edited\n").unwrap();

    bind(&f, &f.sub_wt);
    f.core.diff_sync.reconcile_now().await;
    let id = checkout_id(&f, &f.sub_wt).await;

    let mut stream = client
        .subscribe(
            methods::WATCH_CHECKOUT_DIFFS,
            json!({ "cwd": text(&f.sub_wt), "checkoutId": id }),
        )
        .await
        .expect("scoped diff stream");
    let frame = next_frame(&mut stream, |frame| {
        frame.as_array().is_some_and(|d| {
            d.len() == 1 && d[0]["files"].as_array().is_some_and(|f| !f.is_empty())
        })
    })
    .await;
    let diff = &frame[0];
    assert_eq!(diff["checkoutId"], id);
    assert_eq!(diff["cwd"], text(&f.sub_wt));
    assert_eq!(file_paths(diff), vec!["flagship.txt"]);

    let branch = client
        .call(
            methods::GET_CHECKOUT_DIFF,
            json!({
                "cwd": text(&f.sub_wt),
                "checkoutId": id,
                "mode": "branch",
                "baseRef": "main",
            }),
        )
        .await
        .expect("branch diff");
    assert_eq!(file_paths(&branch), vec!["feature.txt", "flagship.txt"]);
    f.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn latest_turn_snapshots_cwd_and_targets_and_bases_mid_turn_binds() {
    let f = fixture();
    let client = zeron_rpc::memory_client(f.core.rpc_service());
    bind(&f, &f.sub_wt);
    f.core.diff_sync.reconcile_now().await;

    f.core.diff_sync.note_turn_start(CHAT, &text(&f.ws));
    let sync = f.core.diff_sync.clone();
    let (ws, sub) = (f.ws.clone(), f.sub_wt.clone());
    wait_for(|| {
        (sync.turn_snapshot(CHAT, &ws).is_some() && sync.turn_snapshot(CHAT, &sub).is_some())
            .then_some(())
    })
    .await;
    assert!(
        f.core.diff_sync.turn_snapshot(CHAT, &f.root_wt).is_none(),
        "an unbound worktree has no snapshot"
    );

    std::fs::write(f.sub_wt.join("flagship.txt"), "edited in turn\n").unwrap();
    std::fs::write(f.ws.join("README"), "root edit\n").unwrap();
    let id = checkout_id(&f, &f.sub_wt).await;
    let turn = client
        .call(
            methods::GET_CHECKOUT_DIFF,
            json!({
                "cwd": text(&f.sub_wt),
                "checkoutId": id,
                "mode": "turn",
                "chatId": CHAT,
            }),
        )
        .await
        .expect("turn diff of the target");
    assert_eq!(file_paths(&turn), vec!["flagship.txt"]);
    let root_id = checkout_id(&f, &f.ws).await;
    let turn = client
        .call(
            methods::GET_CHECKOUT_DIFF,
            json!({
                "cwd": text(&f.ws),
                "checkoutId": root_id,
                "mode": "turn",
                "chatId": CHAT,
            }),
        )
        .await
        .expect("turn diff of the chat checkout");
    assert!(file_paths(&turn).contains(&"README".to_string()));
    assert!(!file_paths(&turn).contains(&"flagship.txt".to_string()));

    // Binding mid-turn gives the worktree a base on its own (the changes
    // follower), and an explicit call never moves an existing base.
    bind(&f, &f.root_wt);
    let sync = f.core.diff_sync.clone();
    let root_wt = f.root_wt.clone();
    let base = wait_for(|| sync.turn_snapshot(CHAT, &root_wt)).await;
    std::fs::write(f.root_wt.join("README"), "worktree edit\n").unwrap();
    f.core
        .diff_sync
        .snapshot_target_now(CHAT, &text(&f.root_wt))
        .await;
    assert_eq!(
        f.core
            .diff_sync
            .turn_snapshot(CHAT, &f.root_wt)
            .unwrap()
            .tree,
        base.tree
    );
    let id = checkout_id(&f, &f.root_wt).await;
    let turn = client
        .call(
            methods::GET_CHECKOUT_DIFF,
            json!({
                "cwd": text(&f.root_wt),
                "checkoutId": id,
                "mode": "turn",
                "chatId": CHAT,
            }),
        )
        .await
        .expect("turn diff of the mid-turn bind");
    assert_eq!(file_paths(&turn), vec!["README"]);
    f.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chats_without_targets_keep_the_single_checkout_turn_behavior() {
    let f = fixture();
    let client = zeron_rpc::memory_client(f.core.rpc_service());
    let mut stream = client
        .subscribe(methods::WATCH_CHAT_DIFF_TARGETS, Value::Null)
        .await
        .expect("targets stream");
    let first = next_frame(&mut stream, |_| true).await;
    assert_eq!(first, json!([]));

    f.core.diff_sync.note_turn_start(CHAT, &text(&f.ws));
    let sync = f.core.diff_sync.clone();
    let ws = f.ws.clone();
    wait_for(|| sync.turn_snapshot(CHAT, &ws)).await;
    assert!(f.core.diff_sync.turn_snapshot(CHAT, &f.sub_wt).is_none());
    assert!(f.core.diff_sync.turn_snapshot("other-chat", &f.ws).is_none());
    f.core.diff_sync.reconcile_now().await;
    assert_eq!(*f.core.diff_sync.watch_chat_diff_targets().borrow(), vec![]);
    f.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_chat_diff_targets_follows_bind_select_unbind_and_removal() {
    let f = fixture();
    let client = zeron_rpc::memory_client(f.core.rpc_service());
    let mut stream = client
        .subscribe(methods::WATCH_CHAT_DIFF_TARGETS, Value::Null)
        .await
        .expect("targets stream");
    assert_eq!(next_frame(&mut stream, |_| true).await, json!([]));

    bind(&f, &f.sub_wt);
    let sub_id = checkout_id(&f, &f.sub_wt).await;
    let frame = next_frame(&mut stream, |frame| {
        frame
            .as_array()
            .is_some_and(|views| views.len() == 1 && views[0]["targets"][0]["branch"].is_string())
    })
    .await;
    assert_eq!(frame[0]["chatId"], CHAT);
    let target = &frame[0]["targets"][0];
    assert_eq!(target["path"], text(&f.sub_wt));
    assert_eq!(target["checkoutId"], sub_id);
    assert_eq!(target["source"], "agent");
    assert_eq!(target["selected"], true);
    assert_eq!(target["branch"], "feat-eng-3012");
    assert_eq!(target["repoName"], "flagship");
    assert_eq!(target["headSha"].as_str().map(str::len), Some(40));

    // A second bind keeps stored order and moves the selection.
    bind(&f, &f.root_wt);
    let frame = next_frame(&mut stream, |frame| {
        frame[0]["targets"].as_array().is_some_and(|t| t.len() == 2)
    })
    .await;
    let targets = frame[0]["targets"].as_array().unwrap();
    assert_eq!(targets[0]["path"], text(&f.sub_wt));
    assert_eq!(targets[0]["selected"], false);
    assert_eq!(targets[1]["path"], text(&f.root_wt));
    assert_eq!(targets[1]["selected"], true);
    assert_eq!(targets[1]["branch"], "root-feature");
    assert_eq!(targets[1]["repoName"], "ws");

    f.core.diff_targets.clear_selection(CHAT).unwrap();
    let frame = next_frame(&mut stream, |frame| {
        frame[0]["targets"]
            .as_array()
            .is_some_and(|t| t.iter().all(|t| t["selected"] == false))
    })
    .await;
    assert_eq!(frame[0]["targets"].as_array().unwrap().len(), 2);

    f.core
        .diff_targets
        .unbind(CHAT, &text(&f.sub_wt))
        .unwrap();
    let frame = next_frame(&mut stream, |frame| {
        frame[0]["targets"].as_array().is_some_and(|t| t.len() == 1)
    })
    .await;
    assert_eq!(frame[0]["targets"][0]["path"], text(&f.root_wt));

    // A branch switch inside the target republishes its branch.
    git(&f.root_wt, &["checkout", "-q", "-b", "renamed-branch"]);
    f.core.diff_sync.sync_all();
    next_frame(&mut stream, |frame| {
        frame[0]["targets"][0]["branch"] == "renamed-branch"
    })
    .await;

    // Removing the worktree drops its target after a repair pass.
    git(
        &f.ws,
        &["worktree", "remove", "--force", &text(&f.root_wt)],
    );
    f.core.diff_sync.repair_now().await;
    next_frame(&mut stream, |frame| frame == &json!([])).await;
    f.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_chat_diff_target_rpc_round_trip_and_refusals() {
    let f = fixture();
    let client = zeron_rpc::memory_client(f.core.rpc_service());
    f.core
        .workspace
        .create_chat(
            "remote-chat",
            None,
            Some("dev-b"),
            None,
            Some(text(&f.ws)),
        )
        .unwrap();

    let listed = client
        .call(methods::LIST_CHAT_DIFF_CHECKOUTS, json!({ "chatId": CHAT }))
        .await
        .expect("list");
    let paths: Vec<&str> = listed["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths[0], text(&f.ws));
    assert!(paths.contains(&text(&f.sub_wt).as_str()));
    assert!(paths.contains(&text(&f.root_wt).as_str()));

    let selected = client
        .call(
            methods::SET_CHAT_DIFF_TARGET,
            json!({ "chatId": CHAT, "action": "select", "path": text(&f.sub_wt) }),
        )
        .await
        .expect("select");
    assert_eq!(
        selected,
        json!({ "targets": [{ "path": text(&f.sub_wt), "source": "manual", "selected": true }] })
    );
    // The select gave the worktree a "Latest turn" base.
    assert!(f.core.diff_sync.turn_snapshot(CHAT, &f.sub_wt).is_some());

    let cleared = client
        .call(
            methods::SET_CHAT_DIFF_TARGET,
            json!({ "chatId": CHAT, "action": "clearSelection" }),
        )
        .await
        .expect("clear");
    assert_eq!(cleared["targets"][0]["selected"], false);

    let unbound = client
        .call(
            methods::SET_CHAT_DIFF_TARGET,
            json!({ "chatId": CHAT, "action": "unbind", "path": text(&f.sub_wt) }),
        )
        .await
        .expect("unbind");
    assert_eq!(unbound, json!({ "targets": [] }));

    // Refusals carry the DiffTargetError message.
    let expected = f
        .core
        .diff_targets
        .bind(CHAT, &text(&f.other), ChatDiffTargetSource::Manual, None)
        .expect_err("outside path")
        .to_string();
    let error = client
        .call(
            methods::SET_CHAT_DIFF_TARGET,
            json!({ "chatId": CHAT, "action": "select", "path": text(&f.other) }),
        )
        .await
        .expect_err("outside path is refused");
    assert!(error.to_string().contains(&expected), "{error}");

    let error = client
        .call(
            methods::SET_CHAT_DIFF_TARGET,
            json!({ "chatId": CHAT, "action": "select" }),
        )
        .await
        .expect_err("select needs a path");
    assert!(error.to_string().contains("path is required"), "{error}");

    // A chat of another device is never relayed: it fails as not local.
    let error = client
        .call(
            methods::SET_CHAT_DIFF_TARGET,
            json!({
                "chatId": "remote-chat",
                "action": "select",
                "path": text(&f.sub_wt),
                "targetDeviceId": "dev-b",
            }),
        )
        .await
        .expect_err("remote chat");
    assert!(error.to_string().contains("another device"), "{error}");
    let error = client
        .call(
            methods::LIST_CHAT_DIFF_CHECKOUTS,
            json!({ "chatId": "remote-chat" }),
        )
        .await
        .expect_err("remote chat list");
    assert!(error.to_string().contains("another device"), "{error}");
    f.core.shutdown().await;
}
