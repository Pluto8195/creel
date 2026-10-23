//! `StartPrReview` against a real offline engine, temp git repositories laid
//! out like the Cofactr workspace (a superproject Space with a `flagship`
//! submodule whose `origin` is `https://github.com/acme/flagship`, rewritten
//! to a local bare repo with `url.<bare>.insteadOf`), and a fake `gh`:
//! - the location-choice matrix of `launch.sh` (existing checkout reused;
//!   clean `main` → `gh pr checkout`; dirty/off-main → a new worktree at
//!   `<root>/flagship-pr-<n>`; both root overrides; occupied path refused);
//! - forks refused; repositories matched by remote URL, not folder name;
//! - the behind-origin warning;
//! - the seeded prompt is launch.sh's, verbatim, and the Run starts at the
//!   workspace root;
//! - the Chat is titled, PR-linked, optionally
//!   assigned to a work item, and a retry with the same `operationId` returns
//!   the same Chat without touching git again.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use zeron_engine::pr_review_launch::{PrReviewTools, WorktreesDirEnv};
use zeron_engine::pr_review_sync::PrReviewSyncConfig;
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::mock::InstructionMockHarness;
use zeron_proto::{
    ChatConfig, HarnessId, PrCheckoutKind, PrReviewAction, PrReviewError, PrReviewErrorCode,
    SandboxLevel, StartPrReviewParams, StartPrReviewReply,
};
use zeron_rpc::{RpcClient, methods};

const DEVICE: &str = "dev-a";
const SPACE: &str = "space-ws";
const BRANCH: &str = "feature-x";
const SLUG_URL: &str = "https://github.com/acme/flagship";

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
            "init.defaultBranch=main",
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn commit(dir: &Path, file: &str, text: &str) {
    std::fs::write(dir.join(file), text).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", file]);
}

struct Rig {
    _tmp: tempfile::TempDir,
    core: EngineCore,
    mock: InstructionMockHarness,
    client: RpcClient,
    root: PathBuf,
    /// The workspace superproject (the Space).
    ws: PathBuf,
    /// The flagship submodule's canonical checkout.
    flagship: PathBuf,
    /// A clone of the bare remote used to push new commits.
    upstream: PathBuf,
    gh_log: PathBuf,
    gh_dir: PathBuf,
}

/// `submodule`: the submodule's folder inside the workspace.
async fn rig_with(submodule: &str, env: WorktreesDirEnv) -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();

    // The GitHub remote: main + the PR branch.
    let bare = root.join("remotes/flagship.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare", "--initial-branch=main"]);
    let upstream = root.join("upstream");
    std::fs::create_dir_all(&upstream).unwrap();
    git(&upstream, &["init", "-q"]);
    commit(&upstream, "README", "flagship\n");
    git(
        &upstream,
        &["remote", "add", "origin", &bare.to_string_lossy()],
    );
    git(&upstream, &["push", "-q", "origin", "main"]);
    git(&upstream, &["checkout", "-q", "-b", BRANCH]);
    commit(&upstream, "feature.txt", "x\n");
    git(&upstream, &["push", "-q", "origin", BRANCH]);
    git(&upstream, &["checkout", "-q", "main"]);

    // The workspace superproject with flagship as a submodule.
    let ws = root.join("workspace");
    std::fs::create_dir_all(ws.join(".devcontainer")).unwrap();
    git(&ws, &["init", "-q"]);
    commit(&ws, "WORKSPACE", "ws\n");
    git(
        &ws,
        &["submodule", "add", "-q", &bare.to_string_lossy(), submodule],
    );
    git(&ws, &["commit", "-q", "-m", "add submodule"]);
    let flagship = ws.join(submodule);
    git(&flagship, &["config", "remote.origin.url", SLUG_URL]);
    git(
        &flagship,
        &[
            "config",
            &format!("url.{}.insteadOf", bare.to_string_lossy()),
            SLUG_URL,
        ],
    );
    assert_eq!(git(&flagship, &["branch", "--show-current"]), "main");

    // A fake `gh`: `pr view` prints a per-PR fixture, `pr checkout` does
    // what gh does for a same-repo PR. Every call is logged as `cwd|args`.
    let gh_dir = root.join("gh");
    std::fs::create_dir_all(&gh_dir).unwrap();
    let gh_log = gh_dir.join("calls.log");
    let script = format!(
        r#"#!/bin/sh
echo "$PWD|$*" >> '{log}'
case "$1 $2" in
  "pr view")
    if [ -f '{dir}'/view-"$3".json ]; then cat '{dir}'/view-"$3".json; else echo "no PR $3" >&2; exit 1; fi ;;
  "api user")
    if [ -f '{dir}'/user.json ]; then cat '{dir}'/user.json; else echo "no user" >&2; exit 1; fi ;;
  "pr checkout")
    branch=$(sed -n 's/.*"headRefName": *"\([^"]*\)".*/\1/p' '{dir}'/view-"$3".json)
    git checkout -q -b "$branch" --track "origin/$branch" ;;
  *) echo "unexpected gh call: $*" >&2; exit 2 ;;
esac
"#,
        log = gh_log.display(),
        dir = gh_dir.display()
    );
    let gh = gh_dir.join("gh");
    std::fs::write(&gh, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    write_view(&gh_dir, 7, BRANCH, false, "Add feature X");

    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("device-id"), DEVICE).unwrap();
    let mock = InstructionMockHarness::default();
    let harnesses = HarnessRegistry::new();
    harnesses.register(Arc::new(mock.clone()));
    let core = EngineCore::assemble(&data, Arc::new(harnesses), HarnessId::Mock, None)
        .expect("engine core assembles");
    // The viewer login comes from the engine's sync service; keep it on the
    // fake `gh` too.
    core.pr_review_sync
        .reconfigure(PrReviewSyncConfig::for_tests(&gh));
    let rpc = Arc::try_unwrap(core.rpc_service())
        .ok()
        .expect("sole owner")
        .with_pr_review_tools(PrReviewTools {
            gh: Some(gh),
            worktrees_dir_env: env,
        });
    let client = zeron_rpc::memory_client(Arc::new(rpc));
    client
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": SPACE, "deviceId": DEVICE,
                    "path": ws.to_string_lossy() }),
        )
        .await
        .unwrap();
    Rig {
        _tmp: tmp,
        core,
        mock,
        client,
        root,
        ws,
        flagship,
        upstream,
        gh_log,
        gh_dir,
    }
}

async fn rig() -> Rig {
    rig_with("flagship", WorktreesDirEnv::Fixed(None)).await
}

fn write_view(dir: &Path, number: u64, branch: &str, cross: bool, title: &str) {
    std::fs::write(
        dir.join(format!("view-{number}.json")),
        format!(
            r#"{{"headRefName": "{branch}", "isCrossRepository": {cross}, "title": "{title}"}}"#
        ),
    )
    .unwrap();
}

fn params(op: &str, number: u64, action: PrReviewAction) -> StartPrReviewParams {
    StartPrReviewParams {
        repo_owner: "acme".into(),
        repo_name: "flagship".into(),
        number,
        action,
        space_id: None,
        work_item_id: None,
        config: Some(ChatConfig {
            harness: HarnessId::Mock,
            model: None,
            reasoning: None,
            model_options: Default::default(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
        }),
        operation_id: op.into(),
        target_device_id: None,
    }
}

async fn start(rig: &Rig, p: &StartPrReviewParams) -> Result<StartPrReviewReply, PrReviewError> {
    rig.client
        .call(methods::START_PR_REVIEW, serde_json::to_value(p).unwrap())
        .await
        .map(|value| serde_json::from_value(value).expect("reply decodes"))
        .map_err(|e| {
            let text = e.to_string();
            PrReviewError::from_wire_message(&text)
                .unwrap_or_else(|| panic!("not a pr-review error: {text}"))
        })
}

fn gh_calls(rig: &Rig) -> Vec<String> {
    std::fs::read_to_string(&rig.gh_log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn review_prompt(branch: &str, location: &str) -> String {
    format!(
        "/code-review https://github.com/acme/flagship/pull/7 — someone else's PR; review it as a reviewer. Its branch `{branch}` is checked out at {location}; review the PR's commits against its base, not local changes. Report findings to me here and don't post anything to GitHub unless I ask."
    )
}

fn self_review_prompt(branch: &str, location: &str) -> String {
    format!(
        "/code-review https://github.com/acme/flagship/pull/7 — my own PR; review it as its author, before or while I wait for reviewers: look for bugs, missing tests, risky changes and unclear diffs. Its branch `{branch}` is checked out at {location}; review the PR's commits against its base, not local changes. Report findings to me here. Fix clear problems on the PR branch only if I ask, and don't post review comments or approve on GitHub."
    )
}

fn write_authored_view(dir: &Path, number: u64, author: &str) {
    std::fs::write(
        dir.join(format!("view-{number}.json")),
        format!(
            r#"{{"headRefName": "{BRANCH}", "isCrossRepository": false, "title": "Add feature X", "author": {{"login": "{author}"}}}}"#
        ),
    )
    .unwrap();
}

async fn wait_for(mut predicate: impl FnMut() -> bool, what: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !predicate() {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

// ---------------------------------------------------------------------------
// Location choice
// ---------------------------------------------------------------------------

#[tokio::test]
async fn clean_main_checks_out_with_gh_and_starts_the_review_at_the_workspace_root() {
    let rig = rig().await;
    let reply = start(&rig, &params("op-clean", 7, PrReviewAction::CodeReview))
        .await
        .expect("start succeeds");

    assert_eq!(reply.checkout_kind, PrCheckoutKind::CanonicalCheckout);
    assert_eq!(Path::new(&reply.checkout_path), rig.flagship);
    assert_eq!(reply.branch, BRANCH);
    assert_eq!(reply.behind_by, 0);
    assert_eq!(reply.space_id, SPACE);
    assert_eq!(Path::new(&reply.cwd), rig.ws);
    assert_eq!(reply.title, "Review flagship#7: Add feature X");
    assert_eq!(reply.pr_url, "https://github.com/acme/flagship/pull/7");
    assert_eq!(
        git(&rig.flagship, &["branch", "--show-current"]),
        BRANCH,
        "gh pr checkout ran in the canonical checkout"
    );
    let calls = gh_calls(&rig);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(
        calls[0].ends_with(
            "|pr view 7 --repo acme/flagship --json headRefName,isCrossRepository,title,author"
        ),
        "{calls:?}"
    );
    assert_eq!(
        calls[1],
        format!("{}|pr checkout 7", rig.flagship.display())
    );
    // launch.sh's prompt, verbatim.
    let expected = review_prompt(
        BRANCH,
        &format!("the canonical checkout `{}`", rig.flagship.display()),
    );
    assert_eq!(reply.prompt, expected);

    // The Chat: at the workspace root, titled, PR-linked.
    let chat = rig.core.workspace.chat(&reply.chat_id).unwrap().unwrap();
    assert_eq!(chat.cwd.as_deref(), Some(rig.ws.to_str().unwrap()));
    assert_eq!(chat.space_id.as_deref(), Some(SPACE));
    assert_eq!(
        chat.title.as_deref(),
        Some("Review flagship#7: Add feature X")
    );
    assert_eq!(
        chat.linked_pr_url.as_deref(),
        Some("https://github.com/acme/flagship/pull/7")
    );

    // The first Turn runs the seeded prompt at the workspace root.
    wait_for(|| !rig.mock.runs().is_empty(), "the review Run starts").await;
    let run = &rig.mock.runs()[0];
    assert_eq!(run.cwd, rig.ws.to_string_lossy());
    assert!(run.prompt.contains(&expected), "{}", run.prompt);
}

#[tokio::test]
async fn retry_with_the_same_operation_returns_the_same_chat_without_git() {
    let rig = rig().await;
    let p = params("op-retry", 7, PrReviewAction::CodeReview);
    let first = start(&rig, &p).await.expect("start");
    let calls = gh_calls(&rig);
    let second = start(&rig, &p).await.expect("retry");
    assert_eq!(first, second);
    assert_eq!(gh_calls(&rig), calls, "a retry never re-runs gh");
    let review_chats = rig
        .core
        .workspace
        .read_chats()
        .unwrap()
        .into_iter()
        .filter(|chat| chat.linked_pr_url.is_some())
        .count();
    assert_eq!(review_chats, 1);

    let mut other = p.clone();
    other.action = PrReviewAction::FixCi;
    let error = start(&rig, &other)
        .await
        .expect_err("other inputs conflict");
    assert_eq!(error.code, PrReviewErrorCode::OperationConflict);
}

#[tokio::test]
async fn an_existing_checkout_is_reused_and_a_stale_one_warns() {
    let rig = rig().await;
    // The branch is already checked out in some worktree of flagship…
    let existing = rig.root.join("elsewhere/fl-feature");
    std::fs::create_dir_all(existing.parent().unwrap()).unwrap();
    git(
        &rig.flagship,
        &["fetch", "-q", "origin", &format!("{BRANCH}:{BRANCH}")],
    );
    git(
        &rig.flagship,
        &["worktree", "add", "-q", &existing.to_string_lossy(), BRANCH],
    );
    // …and origin moved on since.
    git(&rig.upstream, &["checkout", "-q", BRANCH]);
    commit(&rig.upstream, "more.txt", "more\n");
    git(&rig.upstream, &["push", "-q", "origin", BRANCH]);

    let reply = start(&rig, &params("op-existing", 7, PrReviewAction::CodeReview))
        .await
        .expect("start");
    assert_eq!(reply.checkout_kind, PrCheckoutKind::Existing);
    assert_eq!(Path::new(&reply.checkout_path), existing);
    assert_eq!(reply.behind_by, 1);
    assert_eq!(
        reply.prompt,
        review_prompt(
            BRANCH,
            &format!(
                "`{}`, 1 commit(s) behind origin/{BRANCH} — pull before you start",
                existing.display()
            )
        )
    );
    assert!(
        reply
            .notes
            .iter()
            .any(|note| note.contains("1 commit(s) behind origin/feature-x")),
        "{:?}",
        reply.notes
    );
    assert!(
        !gh_calls(&rig)
            .iter()
            .any(|call| call.contains("pr checkout")),
        "an existing checkout is used as is"
    );
    assert_eq!(git(&rig.flagship, &["branch", "--show-current"]), "main");
}

#[tokio::test]
async fn dirty_main_gets_a_worktree_under_the_workspace() {
    let rig = rig().await;
    std::fs::write(rig.flagship.join("README"), "local edit\n").unwrap();
    let reply = start(&rig, &params("op-dirty", 7, PrReviewAction::CodeReview))
        .await
        .expect("start");
    let path = rig.ws.join(".worktrees/flagship-pr-7");
    assert_eq!(reply.checkout_kind, PrCheckoutKind::NewWorktree);
    assert_eq!(Path::new(&reply.checkout_path), path);
    assert_eq!(git(&path, &["branch", "--show-current"]), BRANCH);
    assert_eq!(
        git(&path, &["rev-parse", "--abbrev-ref", "@{upstream}"]),
        format!("origin/{BRANCH}")
    );
    assert_eq!(
        reply.prompt,
        review_prompt(
            BRANCH,
            &format!(
                "the worktree `{}` (`/workspace/.worktrees/flagship-pr-7` in the devcontainer)",
                path.display()
            )
        )
    );
    assert!(
        !gh_calls(&rig)
            .iter()
            .any(|call| call.contains("pr checkout"))
    );
    // The canonical checkout keeps its edit and branch.
    assert_eq!(git(&rig.flagship, &["branch", "--show-current"]), "main");
}

#[tokio::test]
async fn off_main_gets_a_worktree_and_an_occupied_path_is_refused() {
    let rig = rig().await;
    git(&rig.flagship, &["checkout", "-q", "-b", "my-work"]);
    let occupied = rig.ws.join(".worktrees/flagship-pr-7");
    std::fs::create_dir_all(&occupied).unwrap();
    let error = start(&rig, &params("op-occupied", 7, PrReviewAction::CodeReview))
        .await
        .expect_err("occupied path");
    assert_eq!(error.code, PrReviewErrorCode::WorktreePathExists);
    assert_eq!(
        error.message,
        format!(
            "{} exists but is not on {BRANCH}; remove it or check out {BRANCH} there.",
            occupied.display()
        )
    );
    std::fs::remove_dir(&occupied).unwrap();
    let reply = start(&rig, &params("op-off-main", 7, PrReviewAction::CodeReview))
        .await
        .expect("start");
    assert_eq!(reply.checkout_kind, PrCheckoutKind::NewWorktree);
    assert_eq!(Path::new(&reply.checkout_path), occupied);
}

#[tokio::test]
async fn worktrees_root_file_overrides_the_environment() {
    let rig = rig_with(
        "flagship",
        WorktreesDirEnv::Fixed(Some("/nonexistent/env-root".into())),
    )
    .await;
    let file_root = rig.root.join("file-root");
    std::fs::write(
        rig.ws.join(".devcontainer/.current-worktrees-root"),
        format!("{}\n", file_root.display()),
    )
    .unwrap();
    std::fs::write(rig.flagship.join("README"), "dirty\n").unwrap();
    let reply = start(&rig, &params("op-file-root", 7, PrReviewAction::CodeReview))
        .await
        .expect("start");
    let path = file_root.join("flagship-pr-7");
    assert_eq!(Path::new(&reply.checkout_path), path);
    // Under the overridden root: still the devcontainer worktree wording.
    assert!(
        reply.prompt.contains(&format!(
            "the worktree `{}` (`/workspace/.worktrees/flagship-pr-7` in the devcontainer)",
            path.display()
        )),
        "{}",
        reply.prompt
    );
}

#[tokio::test]
async fn workspace_worktrees_dir_applies_without_the_root_file() {
    let env_root = tempfile::tempdir().unwrap();
    let env_root_path = std::fs::canonicalize(env_root.path()).unwrap();
    let rig = rig_with(
        "flagship",
        WorktreesDirEnv::Fixed(Some(env_root_path.to_string_lossy().into_owned())),
    )
    .await;
    std::fs::write(rig.flagship.join("README"), "dirty\n").unwrap();
    let reply = start(&rig, &params("op-env-root", 7, PrReviewAction::CodeReview))
        .await
        .expect("start");
    assert_eq!(
        Path::new(&reply.checkout_path),
        env_root_path.join("flagship-pr-7")
    );
}

// ---------------------------------------------------------------------------
// Refusals and resolution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn forks_are_refused_before_anything_is_written() {
    let rig = rig().await;
    write_view(&rig.gh_dir, 9, "their-branch", true, "From a fork");
    let error = start(&rig, &params("op-fork", 9, PrReviewAction::CodeReview))
        .await
        .expect_err("fork");
    assert_eq!(error.code, PrReviewErrorCode::Fork);
    assert!(
        error.message.contains("comes from a fork"),
        "{}",
        error.message
    );
    assert!(
        rig.core
            .workspace
            .read_chats()
            .unwrap()
            .iter()
            .all(|chat| chat.linked_pr_url.is_none())
    );
    assert_eq!(gh_calls(&rig).len(), 1, "only gh pr view ran");
}

#[tokio::test]
async fn an_unknown_repository_is_not_found() {
    let rig = rig().await;
    let mut p = params("op-unknown", 7, PrReviewAction::CodeReview);
    p.repo_name = "kb".into();
    let error = start(&rig, &p).await.expect_err("not found");
    assert_eq!(error.code, PrReviewErrorCode::RepoNotFound);
    assert!(
        error.message.contains("github.com/acme/kb"),
        "{}",
        error.message
    );
    assert!(gh_calls(&rig).is_empty(), "resolution happens before gh");
}

#[tokio::test]
async fn repositories_match_by_remote_url_not_folder_name() {
    // The submodule lives at vendor/fs: only its origin says "flagship".
    let rig = rig_with("vendor/fs", WorktreesDirEnv::Fixed(None)).await;
    let reply = start(&rig, &params("op-renamed", 7, PrReviewAction::FixCi))
        .await
        .expect("start");
    assert_eq!(Path::new(&reply.checkout_path), rig.ws.join("vendor/fs"));
    assert_eq!(reply.title, "Fix CI flagship#7: Add feature X");
    assert_eq!(
        reply.prompt,
        format!(
            "/fix-ci https://github.com/acme/flagship/pull/7 — my PR. Its branch `{BRANCH}` is checked out at the canonical checkout `{}`; do all the work there.",
            rig.ws.join("vendor/fs").display()
        )
    );
}

#[tokio::test]
async fn the_outermost_space_wins_over_a_space_on_the_submodule() {
    let rig = rig().await;
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": "space-flagship", "deviceId": DEVICE,
                    "path": rig.flagship.to_string_lossy() }),
        )
        .await
        .unwrap();
    let reply = start(&rig, &params("op-outer", 7, PrReviewAction::CodeReview))
        .await
        .expect("start");
    assert_eq!(reply.space_id, SPACE);
    assert_eq!(Path::new(&reply.cwd), rig.ws);

    // Pinning the inner Space starts there instead.
    let mut pinned = params("op-pinned", 7, PrReviewAction::FixCi);
    pinned.space_id = Some("space-flagship".into());
    let reply = start(&rig, &pinned).await.expect("pinned start");
    assert_eq!(reply.space_id, "space-flagship");
    assert_eq!(reply.checkout_kind, PrCheckoutKind::Existing);
    assert_eq!(
        reply.prompt,
        format!(
            "/fix-ci https://github.com/acme/flagship/pull/7 — my PR. Its branch `{BRANCH}` is checked out at the canonical workspace checkout; do all the work there."
        )
    );
}

#[tokio::test]
async fn a_work_item_gets_the_review_chat() {
    let rig = rig().await;
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createWorkItem", "spaceId": SPACE, "title": "Reviews",
                    "operationId": "op-item" }),
        )
        .await
        .unwrap();
    let item = zeron_proto::derive_id("op-item", zeron_proto::roles::ITEM);
    let mut p = params("op-assigned", 7, PrReviewAction::CodeReview);
    p.work_item_id = Some(item.clone());
    let reply = start(&rig, &p).await.expect("start");
    let row = rig
        .core
        .workspace
        .registry_row(zeron_doc::KIND_WORK_ITEM_CHAT_ASSIGNMENTS, &reply.chat_id)
        .expect("assignment row");
    assert_eq!(row.fields["workItemId"], item.as_str());

    let mut missing = params("op-missing-item", 7, PrReviewAction::CodeReview);
    missing.work_item_id = Some("nope".into());
    let error = start(&rig, &missing).await.expect_err("unknown item");
    assert_eq!(error.code, PrReviewErrorCode::Invalid);
}

#[tokio::test]
async fn a_second_start_for_the_same_pr_and_action_reuses_the_live_review_chat() {
    let rig = rig().await;
    let first = start(&rig, &params("op-first", 7, PrReviewAction::CodeReview))
        .await
        .expect("start");
    assert!(!first.reused);
    let links = rig.core.workspace.chat_pr_links(&first.chat_id).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].source, zeron_proto::ChatLinkSource::ReviewLauncher);
    let calls = gh_calls(&rig);

    // Another window / after a restart: a different operation id.
    let second = start(&rig, &params("op-second", 7, PrReviewAction::CodeReview))
        .await
        .expect("reuse");
    assert!(second.reused);
    assert_eq!(second.chat_id, first.chat_id);
    assert_eq!(second.pr_url, first.pr_url);
    assert_eq!(second.branch, first.branch);
    assert_eq!(second.checkout_path, first.checkout_path);
    assert_eq!(second.cwd, first.cwd);
    assert_eq!(gh_calls(&rig), calls, "a reuse never runs gh or git");
    let review_chats = || {
        rig.core
            .workspace
            .read_chats()
            .unwrap()
            .into_iter()
            .filter(|chat| chat.linked_pr_url.is_some())
            .count()
    };
    assert_eq!(review_chats(), 1);

    // The second operation id is idempotent too, and stays claimed.
    let replay = start(&rig, &params("op-second", 7, PrReviewAction::CodeReview))
        .await
        .expect("replay");
    assert_eq!(replay, second);
    let mut conflicting = params("op-second", 7, PrReviewAction::CodeReview);
    conflicting.action = PrReviewAction::FixCi;
    assert_eq!(
        start(&rig, &conflicting).await.expect_err("conflict").code,
        PrReviewErrorCode::OperationConflict
    );

    // The same PR with the other action is a different review.
    let fix = start(&rig, &params("op-fix", 7, PrReviewAction::FixCi))
        .await
        .expect("other action");
    assert!(!fix.reused);
    assert_ne!(fix.chat_id, first.chat_id);
    assert_eq!(review_chats(), 2);

    // An archived review chat is not reused.
    rig.core
        .workspace
        .set_chat_archived(&first.chat_id, true)
        .unwrap();
    let fresh = start(&rig, &params("op-fresh", 7, PrReviewAction::CodeReview))
        .await
        .expect("fresh after archive");
    assert!(!fresh.reused);
    assert_ne!(fresh.chat_id, first.chat_id);
}

#[tokio::test]
async fn a_review_of_the_viewers_own_pr_is_seeded_as_a_self_review() {
    let rig = rig().await;
    std::fs::write(rig.gh_dir.join("user.json"), r#"{"login":"Octo"}"#).unwrap();
    write_authored_view(&rig.gh_dir, 7, "octo");
    write_authored_view(&rig.gh_dir, 8, "someone-else");
    let location = format!("the canonical checkout `{}`", rig.flagship.display());

    // The viewer's login matches the author's, ignoring case.
    let own = start(&rig, &params("op-own", 7, PrReviewAction::CodeReview))
        .await
        .expect("own PR");
    assert_eq!(own.prompt, self_review_prompt(BRANCH, &location));
    assert!(own.prompt.starts_with("/code-review https://"));

    // Someone else's PR keeps launch.sh's wording.
    let other = start(&rig, &params("op-other", 8, PrReviewAction::CodeReview))
        .await
        .expect("other PR");
    assert_eq!(
        other.prompt,
        review_prompt(BRANCH, &location).replace("pull/7", "pull/8")
    );

    // Fix CI is the same on your own PR.
    let fix = start(&rig, &params("op-fix-own", 7, PrReviewAction::FixCi))
        .await
        .expect("fix ci");
    assert_eq!(
        fix.prompt,
        format!(
            "/fix-ci https://github.com/acme/flagship/pull/7 — my PR. Its branch `{BRANCH}` is checked out at {location}; do all the work there."
        )
    );
}

#[tokio::test]
async fn an_unknown_viewer_keeps_the_reviewer_prompt() {
    // No `user.json`: the fake `gh api user` fails.
    let rig = rig().await;
    write_authored_view(&rig.gh_dir, 7, "octo");
    let location = format!("the canonical checkout `{}`", rig.flagship.display());
    let reply = start(&rig, &params("op-unknown", 7, PrReviewAction::CodeReview))
        .await
        .expect("start");
    assert_eq!(reply.prompt, review_prompt(BRANCH, &location));
}
