//! The repair tick must not re-run the full diff capture for a checkout whose
//! state has not moved, and a full capture must stay at five git processes.
//! Own binary: the spawn counter is process-wide.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use mast_engine::{CheckoutDiffSync, EngineCore, HarnessRegistry};
use mast_harness::git_spawn_count;

fn git(cwd: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .expect("git spawns");
    assert!(output.status.success(), "git {args:?} failed");
}

async fn settled_checksum(sync: &CheckoutDiffSync) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(diff) = sync.watch_diffs().borrow().first() {
            return diff.checksum.clone();
        }
        assert!(tokio::time::Instant::now() < deadline, "diff published");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Wait until the published checksum differs from `old`.
async fn next_checksum(sync: &CheckoutDiffSync, old: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let now = settled_checksum(sync).await;
        if now != old {
            return now;
        }
        assert!(tokio::time::Instant::now() < deadline, "diff changed");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repair_tick_skips_unchanged_checkouts_and_a_capture_is_five_processes() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(repo.join("old.txt"), "keep\nkeep\nkeep\nkeep\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "initial"]);
    std::fs::write(repo.join("a.txt"), "one\ntwo\nedited\n").unwrap();
    git(&repo, &["mv", "old.txt", "renamed.txt"]);
    std::fs::write(repo.join("new.txt"), "untracked\n").unwrap();

    std::fs::create_dir_all(tmp.path().join("data")).unwrap();
    let core = EngineCore::assemble(
        &tmp.path().join("data"),
        Arc::new(HarnessRegistry::new()),
        mast_proto::HarnessId::Mock,
        None,
    )
    .unwrap();

    // A full capture: five git processes (HEAD, branch, raw+numstat, patch,
    // status) where it used to be seven. Renames and numstat still parse.
    let before = git_spawn_count();
    let snapshot = mast_engine::capture_diff(&core.repos, &repo).await.unwrap();
    assert_eq!(git_spawn_count() - before, 5, "full capture spawn count");
    let by_path = |p: &str| snapshot.files.iter().find(|f| f.path == p).unwrap().clone();
    assert_eq!(by_path("a.txt").additions, 1);
    let renamed = by_path("renamed.txt");
    assert_eq!(renamed.old_path.as_deref(), Some("old.txt"));
    assert_eq!(renamed.status, "renamed");
    assert_eq!(by_path("new.txt").status, "added");

    core.workspace
        .create_space("space-1", &core.device_id, &repo.to_string_lossy(), None, true)
        .unwrap();
    core.workspace
        .create_chat("chat-1", Some("space-1"), None, None, None)
        .unwrap();
    core.diff_sync.reconcile_now().await;
    let first = settled_checksum(&core.diff_sync).await;
    tokio::time::sleep(Duration::from_secs(2)).await; // initial + attach kicks land

    // First repair pass records the probe (full capture); later ones skip.
    core.diff_sync.repair_now().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let before = git_spawn_count();
    for _ in 0..3 {
        core.diff_sync.repair_now().await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let spawns = git_spawn_count() - before;
    // One `git status` probe per pass per checkout (plus identity resolution
    // is memoized-free here: repair_now revalidates with fresh rev-parse).
    println!("BENCH diff-sync | 3 repair passes, 1 unchanged checkout | git_spawns={spawns}");
    assert!(spawns <= 3 * 4, "unchanged repair passes stay cheap: {spawns}");
    assert_eq!(settled_checksum(&core.diff_sync).await, first);

    // An edit to a file that is already modified (same size, new mtime) is
    // still seen by the next repair pass.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    std::fs::write(repo.join("a.txt"), "one\ntwo\nEDITED\n").unwrap();
    core.diff_sync.repair_now().await;
    let changed = next_checksum(&core.diff_sync, &first).await;
    assert_ne!(changed, first);
    core.shutdown().await;
}
