//! Per-account CLI homes: spawns for a managed account carry
//! `CLAUDE_CONFIG_DIR` / `CODEX_HOME`; Default spawns carry neither. Fake
//! CLIs record the env they were started with; no real login is involved.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::mpsc;

use zeron_harness::{CancellationToken, ClaudeHarness, CodexHarness, Harness, RunControls};
use zeron_proto::{RunRequest, SandboxLevel};

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn request(prompt: &str, cwd: &Path) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        harness: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: cwd.to_string_lossy().into_owned(),
        sandbox: SandboxLevel::DangerFullAccess,
        auto_approve: true,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
        instructions: None,
    }
}

fn controls() -> RunControls {
    let (_tx, rx) = mpsc::channel(1);
    RunControls {
        request_input: Box::new(|_| tokio::sync::oneshot::channel().1),
        steering: rx,
        interrupt: CancellationToken::new(),
    }
}

/// A fake claude that records `CLAUDE_CONFIG_DIR` (or `<unset>`) and finishes.
fn fake_claude(dir: &Path, record: &Path) -> PathBuf {
    script(
        dir,
        "claude",
        &format!(
            "read -r first\n\
             printf '%s\\n' \"${{CLAUDE_CONFIG_DIR-<unset>}}\" >> {}\n\
             printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"ok\",\"usage\":{{\"input_tokens\":1,\"output_tokens\":1}},\"session_id\":\"s1\"}}'\n",
            record.display()
        ),
    )
}

async fn run_once(harness: &dyn Harness, cwd: &Path) {
    let mut stream = harness.run(request("hi", cwd), controls()).await.unwrap();
    while stream.next().await.is_some() {}
}

fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn claude_default_sets_nothing_and_managed_sets_home() {
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("record");
    let exe = fake_claude(dir.path(), &record);
    // Make sure the test process itself doesn't leak a home into the child.
    // SAFETY: tests in this file never read this variable concurrently.
    unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") };

    let default = ClaudeHarness::new().with_executable(&exe);
    run_once(&default, dir.path()).await;

    let home_a = dir.path().join("homes/a");
    let home_b = dir.path().join("homes/b");
    let a: Arc<dyn Harness> = default.scoped_to_account_home(&home_a).unwrap();
    let b: Arc<dyn Harness> = default.scoped_to_account_home(&home_b).unwrap();
    // Two accounts at the same time.
    tokio::join!(
        run_once(a.as_ref(), dir.path()),
        run_once(b.as_ref(), dir.path())
    );
    // And the Default instance is still unaffected afterwards.
    run_once(&default, dir.path()).await;

    let seen = lines(&record);
    assert_eq!(seen.len(), 4, "{seen:?}");
    assert_eq!(seen[0], "<unset>");
    assert_eq!(seen[3], "<unset>");
    let mut middle = vec![seen[1].clone(), seen[2].clone()];
    middle.sort();
    assert_eq!(
        middle,
        vec![
            home_a.to_string_lossy().into_owned(),
            home_b.to_string_lossy().into_owned()
        ]
    );
}

#[tokio::test]
async fn codex_managed_home_reaches_the_app_server_env() {
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("record");
    let exe = script(
        dir.path(),
        "codex",
        &format!(
            "printf '%s\\n' \"${{CODEX_HOME-<unset>}}\" >> {}\nexit 1\n",
            record.display()
        ),
    );
    unsafe { std::env::remove_var("CODEX_HOME") };
    let default = CodexHarness::new().with_executable(&exe);
    let home = dir.path().join("homes/work");
    let scoped = default.scoped_to_account_home(&home).unwrap();
    for harness in [&default as &dyn Harness, scoped.as_ref()] {
        // The fake exits at once; the run fails, we only want its env.
        if let Ok(mut stream) = harness.run(request("hi", dir.path()), controls()).await {
            while stream.next().await.is_some() {}
        }
    }
    assert_eq!(
        lines(&record),
        vec!["<unset>".to_string(), home.to_string_lossy().into_owned()]
    );
}
