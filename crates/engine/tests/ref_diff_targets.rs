//! "Diff branch / PR…" (contract § "Diff a branch or PR without a checkout")
//! over real temp repositories and a local bare "origin" per repository: a
//! superproject `ws` with a `flagship` submodule whose PR branches exist only
//! as refs of the bare remote (never checked out, never fetched until the
//! bind). No GitHub call is made: `gh` is a fake script, and the remotes'
//! `https://github.com/cofactr/...` URLs are redirected to the bare repos with
//! `url.<bare>.insteadOf`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use mast_engine::pr_review_launch::PrReviewTools;
use mast_engine::{EngineCore, HarnessRegistry};
use mast_proto::{ChatDiffTargetSource, ChatLinkSource, RefDiffKind};
use mast_rpc::methods;

const CHAT: &str = "chat-1";
const FEATURE: &str = "michael/eng-3012-kit-line";
const PART_TWO: &str = "michael/eng-3012-part-2";
const PR_URL: &str = "https://github.com/cofactr/flagship/pull/6487";
const PR2_URL: &str = "https://github.com/cofactr/flagship/pull/6488";

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
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn git_ok(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn commit_file(dir: &Path, file: &str, body: &str, message: &str) {
    std::fs::write(dir.join(file), body).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", message]);
}

/// Point `https://github.com/<slug>.git` at `bare` for this repository.
fn redirect_origin(repo: &Path, slug: &str, bare: &Path) {
    let url = format!("https://github.com/{slug}.git");
    git(repo, &["remote", "add", "origin", &url]);
    git(
        repo,
        &["config", &format!("url.{}.insteadOf", text(bare)), &url],
    );
}

struct Fixture {
    _tmp: tempfile::TempDir,
    data: PathBuf,
    /// Superproject checkout (the chat's cwd).
    ws: PathBuf,
    /// The `flagship` submodule checkout.
    flagship: PathBuf,
    bare_flagship: PathBuf,
    /// `main` of the flagship remote when the feature branch forked.
    fork_point: String,
    tools: PrReviewTools,
    core: EngineCore,
}

fn fake_gh(dir: &Path) -> PrReviewTools {
    let script = dir.join("gh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             # gh pr view <n> --repo <slug> --json <fields>\n\
             [ \"$1 $2\" = \"pr view\" ] || exit 2\n\
             [ \"$4\" = \"--repo\" ] || exit 2\n\
             case \"$3\" in\n\
               6487) echo '{{\"headRefName\":\"{FEATURE}\",\"baseRefName\":\"main\",\"isCrossRepository\":false}}';;\n\
               6488) echo '{{\"headRefName\":\"{PART_TWO}\",\"baseRefName\":\"release\",\"isCrossRepository\":false}}';;\n\
               *) echo 'no such pull request' >&2; exit 1;;\n\
             esac\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    PrReviewTools {
        gh: Some(script),
        ..Default::default()
    }
}

fn assemble(data: &Path) -> EngineCore {
    EngineCore::assemble(
        data,
        Arc::new(HarnessRegistry::new()),
        mast_proto::HarnessId::Mock,
        None,
    )
    .expect("engine core assembles")
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();

    // Remotes.
    let bare_flagship = root.join("remotes/flagship.git");
    let bare_ws = root.join("remotes/workspace.git");
    for bare in [&bare_flagship, &bare_ws] {
        std::fs::create_dir_all(bare).unwrap();
        git(bare, &["init", "-q", "--bare"]);
    }

    // The flagship history: main, a feature branch forked from it, then main
    // moves on (so merge-base differs from main's tip), plus a `release` base
    // for the second PR.
    let seed = root.join("seed/flagship");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-q"]);
    std::fs::write(seed.join("flagship.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(seed.join("gone.txt"), "bye\n").unwrap();
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-q", "-m", "init"]);
    git(&seed, &["remote", "add", "origin", &text(&bare_flagship)]);
    git(&seed, &["push", "-q", "origin", "main"]);
    let fork_point = git(&seed, &["rev-parse", "HEAD"]);

    git(&seed, &["checkout", "-q", "-b", FEATURE]);
    commit_file(
        &seed,
        "flagship.txt",
        "one\nTWO\nthree\nfour\n",
        "feature edit",
    );
    commit_file(&seed, "feature.txt", "kit line\n", "feature file");
    git(&seed, &["rm", "-q", "gone.txt"]);
    git(&seed, &["commit", "-q", "-m", "drop gone"]);
    git(&seed, &["push", "-q", "origin", FEATURE]);
    let feature_sha = git(&seed, &["rev-parse", "HEAD"]);
    git(&seed, &["checkout", "-q", "main"]);
    commit_file(&seed, "main-only.txt", "main moved\n", "main moves on");
    git(&seed, &["push", "-q", "origin", "main"]);
    git(&seed, &["checkout", "-q", "-b", "release"]);
    commit_file(&seed, "release.txt", "release\n", "release base");
    git(&seed, &["push", "-q", "origin", "release"]);
    git(&seed, &["checkout", "-q", "-b", PART_TWO]);
    commit_file(&seed, "part2.txt", "part two\n", "part two");
    git(&seed, &["push", "-q", "origin", PART_TWO]);
    // GitHub's refs/pull/<n>/head, for a PR whose branch is not fetchable.
    git(
        &bare_flagship,
        &["update-ref", "refs/pull/9/head", &feature_sha],
    );
    git(&seed, &["checkout", "-q", "main"]);
    for branch in [FEATURE, "release", PART_TWO] {
        git(&seed, &["branch", "-q", "-D", branch]);
    }

    // The workspace superproject with the submodule; the submodule knows
    // only `main` of its remote (the PR branches are remote-only).
    let ws = root.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    git(&ws, &["init", "-q"]);
    std::fs::write(ws.join("README"), "ws\n").unwrap();
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "init"]);
    git(&ws, &["submodule", "add", "-q", &text(&seed), "flagship"]);
    std::fs::write(ws.join(".gitignore"), ".worktrees/\n").unwrap();
    git(&ws, &["add", "."]);
    git(&ws, &["commit", "-q", "-m", "members"]);
    let flagship = ws.join("flagship");
    git(
        &flagship,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/cofactr/flagship.git",
        ],
    );
    git(
        &flagship,
        &[
            "config",
            &format!("url.{}.insteadOf", text(&bare_flagship)),
            "https://github.com/cofactr/flagship.git",
        ],
    );
    git(
        &flagship,
        &[
            "fetch",
            "-q",
            "origin",
            "+refs/heads/main:refs/remotes/origin/main",
        ],
    );
    assert!(!git_ok(
        &flagship,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/origin/{FEATURE}")
        ]
    ));
    redirect_origin(&ws, "cofactr/workspace", &bare_ws);
    git(&ws, &["push", "-q", "origin", "main"]);
    // A branch that exists only in the workspace remote.
    git(&ws, &["checkout", "-q", "-b", "root-only-eng-3012"]);
    commit_file(&ws, "ROOT", "root change\n", "root only");
    git(&ws, &["push", "-q", "origin", "root-only-eng-3012"]);
    git(&ws, &["checkout", "-q", "main"]);
    git(&ws, &["branch", "-q", "-D", "root-only-eng-3012"]);

    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("device-id"), "dev-a").unwrap();
    let core = assemble(&data);
    core.workspace
        .create_chat(CHAT, None, Some("dev-a"), None, Some(text(&ws)))
        .unwrap();
    let tools = fake_gh(&root);
    Fixture {
        flagship: std::fs::canonicalize(&flagship).unwrap(),
        ws: std::fs::canonicalize(&ws).unwrap(),
        _tmp: tmp,
        data,
        bare_flagship,
        fork_point,
        tools,
        core,
    }
}

impl Fixture {
    fn targets(&self) -> mast_engine::diff_targets::DiffTargets {
        self.core
            .diff_targets
            .clone()
            .with_pr_tools(self.tools.clone())
    }

    fn no_gh(&self) -> mast_engine::diff_targets::DiffTargets {
        self.core.diff_targets.clone().with_pr_tools(PrReviewTools {
            gh: Some(self._tmp.path().join("no-such-gh")),
            ..Default::default()
        })
    }
}

fn refs_of(bare: &Path) -> String {
    git(bare, &["for-each-ref", "--format=%(objectname) %(refname)"])
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

async fn range_diff(f: &Fixture, client: &mast_rpc::RpcClient, view: &Value) -> Value {
    client
        .call(
            methods::GET_CHECKOUT_DIFF,
            json!({
                "cwd": view["path"],
                "checkoutId": view["checkoutId"],
                "mode": "range",
                "baseRef": view["baseRef"],
                "headRef": view["headRef"],
            }),
        )
        .await
        .unwrap_or_else(|e| panic!("range diff in {}: {e}", f.ws.display()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pr_url_diffs_merge_base_to_head_from_a_remote_only_branch() {
    let f = fixture();
    let client = mast_rpc::memory_client(f.core.rpc_service());
    let remote_before = refs_of(&f.bare_flagship);

    let outcome = f
        .targets()
        .bind_ref(CHAT, PR_URL, None, None)
        .expect("bind PR");
    let target = &outcome.target;
    assert_eq!(target.kind, RefDiffKind::Pr);
    assert_eq!(target.pr_number, Some(6487));
    assert_eq!(target.pr_url.as_deref(), Some(PR_URL));
    // Right repository: the submodule, picked from the URL's owner/repo.
    assert_eq!(target.path, text(&f.flagship));
    assert_eq!(target.head_branch, FEATURE);
    assert_eq!(target.head_ref, format!("refs/remotes/origin/{FEATURE}"));
    // Base defaults to the PR's base branch.
    assert_eq!(target.base_branch, "main");
    assert_eq!(target.base_ref, "refs/remotes/origin/main");
    // The branch was fetched into remote-tracking refs, and nothing moved on
    // the remote.
    assert!(git_ok(
        &f.flagship,
        &["rev-parse", "--verify", "--quiet", &target.head_ref]
    ));
    assert_eq!(refs_of(&f.bare_flagship), remote_before);
    // No branch checkout anywhere.
    assert!(!git(&f.flagship, &["branch", "--list", FEATURE]).contains(FEATURE));
    assert!(outcome.reply.ref_targets[0].selected);
    assert_eq!(outcome.reply.ref_targets[0].label, "PR #6487");

    f.core.diff_sync.reconcile_now().await;
    let mut stream = client
        .subscribe(methods::WATCH_CHAT_DIFF_TARGETS, Value::Null)
        .await
        .unwrap();
    let frame = next_frame(&mut stream, |frame| {
        frame
            .as_array()
            .is_some_and(|views| views.len() == 1 && views[0]["refTargets"].is_array())
    })
    .await;
    let view = frame[0]["refTargets"][0].clone();
    assert_eq!(view["label"], "PR #6487");
    assert_eq!(view["kind"], "pr");
    assert_eq!(view["selected"], true);
    assert_eq!(view["path"], text(&f.flagship));
    assert_eq!(view["headBranch"], FEATURE);
    assert_eq!(view["baseBranch"], "main");
    assert_eq!(view["prNumber"], 6487);
    assert_eq!(view["repoName"], "flagship");
    let head_sha = git(&f.flagship, &["rev-parse", &target.head_ref]);
    assert_eq!(view["headSha"], head_sha);
    // The chat's own checkout stays the default; no checkout target.
    assert_eq!(frame[0]["targets"], json!([]));

    // The diff equals `git diff merge-base...head`.
    let diff = range_diff(&f, &client, &view).await;
    let merge_base = git(
        &f.flagship,
        &["merge-base", &target.base_ref, &target.head_ref],
    );
    assert_eq!(merge_base, f.fork_point);
    let expected: Vec<String> = {
        let mut names: Vec<String> = git(
            &f.flagship,
            &[
                "diff",
                "--name-only",
                &format!("{}...{}", target.base_ref, target.head_ref),
            ],
        )
        .lines()
        .map(str::to_owned)
        .collect();
        names.sort();
        names
    };
    assert_eq!(file_paths(&diff), expected);
    assert_eq!(expected, vec!["feature.txt", "flagship.txt", "gone.txt"]);
    // Main's own later commit is not part of the PR.
    assert!(!file_paths(&diff).contains(&"main-only.txt".to_owned()));
    let patch = diff["patch"].as_str().unwrap();
    assert!(patch.contains("+kit line"), "{patch}");
    assert_eq!(
        patch,
        git(
            &f.flagship,
            &[
                "diff",
                "--no-ext-diff",
                "--no-color",
                "--find-renames",
                "--unified=3",
                &merge_base,
                &target.head_ref,
            ],
        )
        .to_owned()
            + "\n"
    );
    let by_path = |name: &str| {
        diff["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(by_path("feature.txt")["status"], "added");
    assert_eq!(by_path("gone.txt")["status"], "deleted");

    // Both sides of one file come from immutable blobs.
    let text_reply = client
        .call(
            methods::GET_CHECKOUT_FILE_DIFF_TEXT,
            json!({
                "cwd": view["path"],
                "checkoutId": view["checkoutId"],
                "mode": "range",
                "baseRef": view["baseRef"],
                "headRef": view["headRef"],
                "path": "flagship.txt",
                "diffChecksum": diff["checksum"],
            }),
        )
        .await
        .expect("file text");
    assert_eq!(text_reply["stale"], false);
    assert_eq!(text_reply["oldText"], "one\ntwo\nthree\n");
    assert_eq!(text_reply["newText"], "one\nTWO\nthree\nfour\n");
    f.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_base_defaults_to_the_prs_base_then_origin_main_and_can_be_overridden() {
    let f = fixture();

    // gh says PR 6488 targets `release`.
    let second = f
        .targets()
        .bind_ref(CHAT, PR2_URL, None, None)
        .unwrap()
        .target;
    assert_eq!(second.base_branch, "release");
    assert_eq!(second.base_ref, "refs/remotes/origin/release");
    assert_eq!(second.head_branch, PART_TWO);

    // gh does not know PR 9: the head comes from refs/pull/9/head and the
    // base falls back to origin/main.
    let ninth = f
        .targets()
        .bind_ref(
            CHAT,
            "https://github.com/cofactr/flagship/pull/9",
            None,
            None,
        )
        .unwrap()
        .target;
    assert_eq!(ninth.head_ref, "refs/zeron/pr/9/head");
    assert_eq!(ninth.base_ref, "refs/remotes/origin/main");
    assert_eq!(ninth.head_branch, "pr-9");

    // No gh at all: same fallback.
    let without_gh = f
        .no_gh()
        .bind_ref(
            CHAT,
            "https://github.com/cofactr/flagship/pull/9",
            None,
            None,
        )
        .unwrap()
        .target;
    assert_eq!(without_gh.id, ninth.id);
    assert_eq!(without_gh.base_branch, "main");

    // An explicit base wins over the PR's base.
    let overridden = f
        .targets()
        .bind_ref(CHAT, PR2_URL, Some("origin/main"), None)
        .unwrap()
        .target;
    assert_eq!(overridden.id, second.id, "same PR, same target");
    assert_eq!(overridden.base_branch, "main");

    // A branch input defaults to origin/main.
    let branch = f
        .targets()
        .bind_ref(CHAT, FEATURE, None, None)
        .unwrap()
        .target;
    assert_eq!(branch.base_ref, "refs/remotes/origin/main");

    // A base that does not exist is refused with the name.
    let error = f
        .targets()
        .bind_ref(CHAT, FEATURE, Some("no-such-base"), None)
        .expect_err("unknown base");
    assert!(error.to_string().contains("no-such-base"), "{error}");

    // Three PR/branch targets so far were stored once each (same id replaced).
    let stored = f.targets().stored(CHAT);
    let labels: Vec<&str> = stored
        .ref_targets
        .iter()
        .map(|t| t.label.as_str())
        .collect();
    assert_eq!(
        labels,
        vec!["PR #6488", "PR #9", format!("branch {FEATURE}").as_str()]
    );
    f.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn free_text_branch_number_and_url_pick_the_right_repository() {
    let f = fixture();
    let targets = f.targets();

    // A branch that exists only in the submodule's remote.
    let sub = targets.bind_ref(CHAT, FEATURE, None, None).unwrap().target;
    assert_eq!(sub.kind, RefDiffKind::Branch);
    assert_eq!(sub.path, text(&f.flagship));
    assert_eq!(sub.head_ref, format!("refs/remotes/origin/{FEATURE}"));
    assert_eq!(
        f.targets().stored(CHAT).ref_targets.last().unwrap().label,
        format!("branch {FEATURE}")
    );

    // `origin/<name>` and `refs/heads/<name>` mean the same branch.
    let again = targets
        .bind_ref(CHAT, &format!("origin/{FEATURE}"), None, None)
        .unwrap()
        .target;
    assert_eq!(again.id, sub.id);

    // A branch that exists only in the root repository's remote.
    let root = targets
        .bind_ref(CHAT, "root-only-eng-3012", None, None)
        .unwrap()
        .target;
    assert_eq!(root.path, text(&f.ws));
    assert_eq!(root.repo_id == sub.repo_id, false);

    // A bare PR number needs a linked PR (or a single GitHub repository).
    let ambiguous = targets
        .bind_ref(CHAT, "6487", None, None)
        .expect_err("ambiguous");
    assert!(
        ambiguous.to_string().contains("Paste the PR URL"),
        "{ambiguous}"
    );
    f.core
        .workspace
        .add_chat_pr_link(CHAT, PR_URL, ChatLinkSource::CreatedInChat)
        .unwrap();
    let by_number = targets.bind_ref(CHAT, "#6487", None, None).unwrap().target;
    assert_eq!(by_number.pr_number, Some(6487));
    assert_eq!(by_number.path, text(&f.flagship));
    let by_url = targets.bind_ref(CHAT, PR_URL, None, None).unwrap().target;
    assert_eq!(by_url.id, by_number.id);

    // Refusals say what is wrong.
    let unknown_repo = targets
        .bind_ref(CHAT, "https://github.com/someone/else/pull/3", None, None)
        .expect_err("unknown repository");
    assert!(
        unknown_repo.to_string().contains("someone/else"),
        "{unknown_repo}"
    );
    let missing = targets
        .bind_ref(CHAT, "no-such-branch", None, None)
        .expect_err("missing branch");
    assert!(missing.to_string().contains("no-such-branch"), "{missing}");
    let nonsense = targets
        .bind_ref(CHAT, "bad..name", None, None)
        .expect_err("bad name");
    assert!(nonsense.to_string().contains("bad..name"), "{nonsense}");
    assert!(targets.bind_ref(CHAT, "  ", None, None).is_err());
    assert!(targets.bind_ref(CHAT, "-rf", None, None).is_err());

    // The picker's repository hint disambiguates and is validated.
    let hinted = targets
        .bind_ref(CHAT, FEATURE, None, Some(&text(&f.flagship)))
        .unwrap()
        .target;
    assert_eq!(hinted.id, sub.id);
    let outside = targets
        .bind_ref(CHAT, FEATURE, None, Some("/tmp"))
        .expect_err("hint outside the family");
    assert!(outside.to_string().contains("/tmp"), "{outside}");
    f.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suggestions_put_linked_prs_first_then_ticket_branches() {
    let f = fixture();
    f.core
        .workspace
        .set_chat_link(
            CHAT,
            mast_engine::workspace_host::ChatLinkKind::Ticket,
            Some("ENG-3012"),
            ChatLinkSource::Mentioned,
        )
        .unwrap();
    for url in [PR_URL, PR2_URL, "https://github.com/someone/else/pull/3"] {
        f.core
            .workspace
            .add_chat_pr_link(CHAT, url, ChatLinkSource::Mentioned)
            .unwrap();
    }
    // Local refs that match the ticket (listing never fetches): a fetched
    // remote branch, a local-only branch, a trunk, and an unrelated branch.
    git(
        &f.flagship,
        &[
            "fetch",
            "-q",
            "origin",
            &format!("+refs/heads/{PART_TWO}:refs/remotes/origin/{PART_TWO}"),
        ],
    );
    git(&f.flagship, &["branch", "eng-3012-wip", "origin/main"]);
    git(&f.flagship, &["branch", "unrelated-eng-77", "origin/main"]);
    git(&f.ws, &["branch", "ws-eng-3012-notes"]);

    let reply = f.targets().list_ref_candidates(CHAT).unwrap();
    let rows: Vec<(String, bool, bool)> = reply
        .candidates
        .iter()
        .map(|c| (c.label.clone(), c.linked, c.suggested))
        .collect();
    // Linked PRs first, in link order, one per PR; the PR of a repository the
    // chat doesn't have is left out.
    assert_eq!(rows[0], ("PR #6487".into(), true, false));
    assert_eq!(rows[1], ("PR #6488".into(), true, false));
    // Then ticket branches (the submodule's, then the root repository's).
    let branches: Vec<&str> = rows[2..]
        .iter()
        .map(|(label, _, _)| label.as_str())
        .collect();
    assert!(
        rows[2..]
            .iter()
            .all(|(_, linked, suggested)| !linked && *suggested)
    );
    assert!(
        branches.contains(&format!("branch {PART_TWO}").as_str()),
        "{branches:?}"
    );
    assert!(branches.contains(&"branch eng-3012-wip"), "{branches:?}");
    assert!(
        branches.contains(&"branch ws-eng-3012-notes"),
        "{branches:?}"
    );
    // `git push` left a remote-tracking ref for the root-only branch.
    assert!(
        branches.contains(&"branch root-only-eng-3012"),
        "{branches:?}"
    );
    assert!(
        !branches
            .iter()
            .any(|b| b.contains("unrelated") || b.ends_with(" main")),
        "{branches:?}"
    );
    assert_eq!(reply.candidates.len(), 6);
    let pr = &reply.candidates[0];
    assert_eq!(pr.input, PR_URL);
    assert_eq!(pr.path, text(&f.flagship));
    assert_eq!(pr.repo_name, "flagship");
    let ws_branch = reply
        .candidates
        .iter()
        .find(|c| c.input == "ws-eng-3012-notes")
        .unwrap();
    assert_eq!(ws_branch.path, text(&f.ws));

    // Binding marks the candidate as bound.
    f.targets().bind_ref(CHAT, PR_URL, None, None).unwrap();
    let after = f.targets().list_ref_candidates(CHAT).unwrap();
    assert!(after.candidates[0].bound);
    assert!(!after.candidates[1].bound);

    // Without a ticket there are only linked PRs.
    f.core
        .workspace
        .create_chat("chat-2", None, Some("dev-a"), None, Some(text(&f.ws)))
        .unwrap();
    assert!(
        f.targets()
            .list_ref_candidates("chat-2")
            .unwrap()
            .candidates
            .is_empty()
    );
    f.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ref_targets_persist_across_a_restart_and_follow_selection_rules() {
    let f = fixture();
    let sub_wt_free = f.flagship.clone();
    let targets = f.targets();
    let pr = targets.bind_ref(CHAT, PR_URL, None, None).unwrap().target;
    let branch = targets
        .bind_ref(CHAT, "root-only-eng-3012", None, None)
        .unwrap()
        .target;
    // The latest bind is the selected one.
    let stored = targets.stored(CHAT);
    assert_eq!(
        stored
            .ref_targets
            .iter()
            .map(|t| t.selected)
            .collect::<Vec<_>>(),
        vec![false, true]
    );

    // Selecting a checkout clears the ref selection; selecting a ref clears
    // the checkout selection.
    targets
        .bind(
            CHAT,
            &text(&sub_wt_free),
            ChatDiffTargetSource::Manual,
            None,
        )
        .unwrap();
    let stored = targets.stored(CHAT);
    assert!(stored.ref_targets.iter().all(|t| !t.selected));
    assert!(stored.targets.iter().any(|t| t.selected));
    let stored = targets.select_ref(CHAT, &pr.id).unwrap();
    assert!(stored.targets.iter().all(|t| !t.selected));
    assert_eq!(
        stored
            .ref_targets
            .iter()
            .map(|t| (t.id.clone(), t.selected))
            .collect::<Vec<_>>(),
        vec![(pr.id.clone(), true), (branch.id.clone(), false)]
    );
    assert!(targets.select_ref(CHAT, "pr-unknown").is_err());
    // Clearing the selection shows the chat's own checkout again.
    let cleared = targets.clear_selection(CHAT).unwrap();
    assert!(cleared.ref_targets.iter().all(|t| !t.selected));
    targets.select_ref(CHAT, &pr.id).unwrap();

    f.core.shutdown().await;
    // The engine holds a lock on its data directory until it is dropped.
    drop(f.core);

    // Restart on the same data directory.
    let core = assemble(&f.data);
    let reply = core.diff_targets.stored(CHAT);
    assert_eq!(reply.ref_targets.len(), 2);
    assert_eq!(reply.ref_targets[0].label, "PR #6487");
    assert!(reply.ref_targets[0].selected);
    assert_eq!(reply.ref_targets[1].label, "branch root-only-eng-3012");
    let rows = core.diff_targets.rows();
    assert_eq!(rows[0].selected_ref.as_deref(), Some(pr.id.as_str()));
    assert_eq!(rows[0].ref_targets[0].head_ref, pr.head_ref);

    // The restarted engine resolves them again and still diffs.
    let client = mast_rpc::memory_client(core.rpc_service());
    core.diff_sync.reconcile_now().await;
    let mut stream = client
        .subscribe(methods::WATCH_CHAT_DIFF_TARGETS, Value::Null)
        .await
        .unwrap();
    let frame = next_frame(&mut stream, |frame| {
        frame.as_array().is_some_and(|views| {
            views.len() == 1
                && views[0]["refTargets"]
                    .as_array()
                    .is_some_and(|r| r.len() == 2)
        })
    })
    .await;
    let view = &frame[0]["refTargets"][0];
    assert_eq!(view["label"], "PR #6487");
    assert_eq!(view["selected"], true);
    let diff = client
        .call(
            methods::GET_CHECKOUT_DIFF,
            json!({
                "cwd": view["path"], "checkoutId": view["checkoutId"],
                "mode": "range", "baseRef": view["baseRef"], "headRef": view["headRef"],
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        file_paths(&diff),
        vec!["feature.txt", "flagship.txt", "gone.txt"]
    );
    assert_eq!(
        frame[0]["refTargets"][1]["label"],
        "branch root-only-eng-3012"
    );
    assert_eq!(frame[0]["refTargets"][1]["path"], text(&f.ws));

    // Unbinding drops it; a ref that vanished disappears from the view.
    let reply = core.diff_targets.unbind_ref(CHAT, &pr.id).unwrap();
    assert_eq!(reply.ref_targets.len(), 1);
    assert!(reply.ref_targets.iter().all(|t| !t.selected));
    git(
        &f.ws,
        &["update-ref", "-d", "refs/remotes/origin/root-only-eng-3012"],
    );
    core.diff_sync.reconcile_now().await;
    // The checkout target bound earlier stays; no ref target is left.
    let frame = next_frame(&mut stream, |frame| {
        frame
            .as_array()
            .is_some_and(|views| views.len() == 1 && views[0].get("refTargets").is_none())
    })
    .await;
    assert_eq!(frame[0]["targets"].as_array().map(Vec::len), Some(1));
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_rpc_binds_selects_and_unbinds_and_unbound_chats_are_unchanged() {
    let f = fixture();
    let client = mast_rpc::memory_client(f.core.rpc_service());

    // A chat that never bound anything: no ref fields on the wire.
    let mut stream = client
        .subscribe(methods::WATCH_CHAT_DIFF_TARGETS, Value::Null)
        .await
        .unwrap();
    assert_eq!(next_frame(&mut stream, |_| true).await, json!([]));
    assert_eq!(f.targets().stored(CHAT), Default::default());
    let none = client
        .call(
            methods::LIST_CHAT_REF_DIFF_CANDIDATES,
            json!({ "chatId": CHAT }),
        )
        .await
        .unwrap();
    assert_eq!(none, json!({ "candidates": [] }));

    // bindRef by branch name (no gh involved).
    let bound = client
        .call(
            methods::SET_CHAT_DIFF_TARGET,
            json!({ "chatId": CHAT, "action": "bindRef", "input": FEATURE }),
        )
        .await
        .expect("bindRef");
    let id = bound["refTargets"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        bound,
        json!({
            "targets": [],
            "refTargets": [{
                "id": id, "kind": "branch",
                "label": format!("branch {FEATURE}"), "selected": true,
            }]
        })
    );
    let unselected = client
        .call(
            methods::SET_CHAT_DIFF_TARGET,
            json!({ "chatId": CHAT, "action": "clearSelection" }),
        )
        .await
        .unwrap();
    assert_eq!(unselected["refTargets"][0]["selected"], false);
    let selected = client
        .call(
            methods::SET_CHAT_DIFF_TARGET,
            json!({ "chatId": CHAT, "action": "selectRef", "refId": id }),
        )
        .await
        .unwrap();
    assert_eq!(selected["refTargets"][0]["selected"], true);

    // Refusals carry the message.
    let error = client
        .call(
            methods::SET_CHAT_DIFF_TARGET,
            json!({ "chatId": CHAT, "action": "bindRef", "input": "no-such-branch" }),
        )
        .await
        .expect_err("unknown branch");
    assert!(error.to_string().contains("no-such-branch"), "{error}");
    let error = client
        .call(
            methods::SET_CHAT_DIFF_TARGET,
            json!({ "chatId": CHAT, "action": "selectRef" }),
        )
        .await
        .expect_err("refId required");
    assert!(error.to_string().contains("refId"), "{error}");
    // A range without refs is refused, not guessed.
    let error = client
        .call(
            methods::GET_CHECKOUT_DIFF,
            json!({ "cwd": text(&f.flagship), "mode": "range", "baseRef": "refs/remotes/origin/main" }),
        )
        .await
        .expect_err("headRef required");
    assert!(error.to_string().contains("headRef"), "{error}");

    let removed = client
        .call(
            methods::SET_CHAT_DIFF_TARGET,
            json!({ "chatId": CHAT, "action": "unbindRef", "refId": id }),
        )
        .await
        .unwrap();
    assert_eq!(removed, json!({ "targets": [] }));

    // The chat's own working-tree diff is untouched by all of this.
    std::fs::write(f.ws.join("README"), "dirty\n").unwrap();
    f.core.diff_sync.reconcile_now().await;
    let id = f.core.repos.checkout_identity(&f.ws).await.unwrap().id;
    let diff = client
        .call(
            methods::GET_CHECKOUT_DIFF,
            json!({ "cwd": text(&f.ws), "checkoutId": id }),
        )
        .await
        .unwrap();
    assert_eq!(file_paths(&diff), vec!["README"]);
    f.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ref_targets_are_refused_for_chats_on_other_devices() {
    let f = fixture();
    f.core
        .workspace
        .create_chat("remote-chat", None, Some("dev-b"), None, Some(text(&f.ws)))
        .unwrap();
    let error = f
        .targets()
        .bind_ref("remote-chat", FEATURE, None, None)
        .expect_err("not local");
    assert!(error.to_string().contains("another device"), "{error}");
    assert!(f.targets().list_ref_candidates("remote-chat").is_err());
    f.core.shutdown().await;
}
