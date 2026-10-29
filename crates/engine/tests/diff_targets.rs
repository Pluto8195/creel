//! Chat diff targets (engine core) over real temp git repositories: a
//! superproject with a `flagship` submodule, a linked worktree of the
//! SUBMODULE inside the superproject's gitignored `.worktrees/` folder (the
//! cb4e2158 layout), and a linked worktree of the root repository.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use mast_engine::diff_targets::{DiffTargetError, DiffTargets};
use mast_engine::{EngineCore, HarnessRegistry};
use mast_proto::{ChatDiffTargetSource, DiffCheckoutKind, MAX_CHAT_DIFF_TARGETS};

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
    root: PathBuf,
    /// Superproject checkout (the chat's cwd).
    ws: PathBuf,
    /// `<ws>/.worktrees/flagship/eng-3012` (worktree of the submodule).
    sub_wt: PathBuf,
    /// `<ws>/.worktrees/ws/root-feature` (worktree of the root repository).
    root_wt: PathBuf,
    /// An unrelated repository.
    other: PathBuf,
    core: EngineCore,
    targets: DiffTargets,
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
        mast_proto::HarnessId::Mock,
        None,
    )
    .expect("engine core assembles");
    let targets = DiffTargets::new(core.repos.clone(), core.workspace.clone(), "dev-a");
    core.workspace
        .create_chat(
            "chat-1",
            None,
            Some("dev-a"),
            None,
            Some(ws.to_string_lossy().into_owned()),
        )
        .unwrap();
    Fixture {
        sub_wt: std::fs::canonicalize(&sub_wt).unwrap(),
        root_wt: std::fs::canonicalize(&root_wt).unwrap(),
        _tmp: tmp,
        root,
        ws,
        other,
        core,
        targets,
    }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn code<T: std::fmt::Debug>(result: Result<T, DiffTargetError>) -> &'static str {
    result.expect_err("expected a refusal").code()
}

fn bind(f: &Fixture, path: &Path, op: &str) -> mast_engine::diff_targets::BindOutcome {
    f.targets
        .bind("chat-1", &text(path), ChatDiffTargetSource::Agent, Some(op))
        .unwrap_or_else(|e| panic!("bind {}: {e}", path.display()))
}

#[tokio::test(flavor = "multi_thread")]
async fn listing_orders_own_submodule_and_worktrees_with_flags() {
    let f = fixture();
    let listing = f.targets.list_checkouts("chat-1").unwrap().candidates;
    let summary: Vec<_> = listing
        .iter()
        .map(|c| {
            (
                c.path.clone(),
                c.kind,
                c.repo_name.clone(),
                c.branch.clone(),
                c.bound,
                c.selected,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                text(&f.ws),
                DiffCheckoutKind::Own,
                "ws".to_string(),
                Some("main".to_string()),
                false,
                true
            ),
            (
                text(&f.ws.join("flagship")),
                DiffCheckoutKind::Submodule,
                "flagship".to_string(),
                Some("main".to_string()),
                false,
                false
            ),
            (
                text(&f.root_wt),
                DiffCheckoutKind::Worktree,
                "ws".to_string(),
                Some("root-feature".to_string()),
                false,
                false
            ),
            (
                text(&f.sub_wt),
                DiffCheckoutKind::Worktree,
                "flagship".to_string(),
                Some("feat-eng-3012".to_string()),
                false,
                false
            ),
        ]
    );
    let head = git(&f.sub_wt, &["rev-parse", "HEAD"]);
    assert_eq!(listing[3].head_sha.as_deref(), Some(head.as_str()));
    assert!(listing[0].head_sha.is_some());

    // A worktree deleted behind git's back (prunable, missing) disappears.
    std::fs::remove_dir_all(&f.root_wt).unwrap();
    let after = f.targets.list_checkouts("chat-1").unwrap().candidates;
    assert_eq!(after.len(), 3);
    assert!(after.iter().all(|c| c.path != text(&f.root_wt)));

    // The main checkout of a chat that lives in a linked worktree is listed
    // as a worktree of that repository; the own checkout is not repeated.
    f.core
        .workspace
        .create_chat(
            "chat-in-wt",
            None,
            Some("dev-a"),
            None,
            Some(text(&f.sub_wt.join("."))),
        )
        .unwrap();
    let from_wt = f.targets.list_checkouts("chat-in-wt").unwrap().candidates;
    assert_eq!(from_wt[0].path, text(&f.sub_wt));
    assert_eq!(from_wt[0].kind, DiffCheckoutKind::Own);
    assert_eq!(from_wt[0].repo_name, "flagship");
    assert_eq!(
        from_wt.iter().filter(|c| c.path == text(&f.sub_wt)).count(),
        1
    );
    assert!(from_wt.iter().any(|c| c.path == text(&f.ws.join("flagship"))
        && c.kind == DiffCheckoutKind::Worktree));
}

#[tokio::test(flavor = "multi_thread")]
async fn binding_a_submodule_worktree_persists_and_selects_it() {
    let f = fixture();
    let before = *f.targets.changes().borrow();
    let outcome = bind(&f, &f.sub_wt, "op-1");
    assert!(!outcome.replayed);
    assert!(outcome.selected);
    assert_eq!(outcome.path, text(&f.sub_wt));
    assert_eq!(outcome.branch.as_deref(), Some("feat-eng-3012"));
    assert_eq!(outcome.repo_name, "flagship");
    assert_eq!(
        outcome.head_sha.as_deref(),
        Some(git(&f.sub_wt, &["rev-parse", "HEAD"]).as_str())
    );
    assert_eq!(outcome.reply.targets.len(), 1);
    assert!(outcome.reply.targets[0].selected);
    assert_eq!(*f.targets.changes().borrow(), before + 1);

    // A fresh service over the same workspace reads the persisted row.
    let reopened = DiffTargets::new(f.core.repos.clone(), f.core.workspace.clone(), "dev-a");
    let row = reopened.rows().into_iter().next().expect("row stored");
    assert_eq!(row.chat_id, "chat-1");
    assert_eq!(row.selected.as_deref(), Some(text(&f.sub_wt).as_str()));
    assert_eq!(row.ops, vec!["op-1".to_string()]);
    let stored = &row.targets[0];
    assert_eq!(stored.source, ChatDiffTargetSource::Agent);
    assert!(stored.repo_id.starts_with("repo:"));
    assert_eq!(stored.worktree_id.len(), 64);
    assert!(stored.bound_at > 0);
    assert!(row.updated_at > 0);
    assert_eq!(reopened.stored("chat-1"), outcome.reply);

    let listing = reopened.list_checkouts("chat-1").unwrap().candidates;
    let bound = listing.iter().find(|c| c.path == text(&f.sub_wt)).unwrap();
    assert!(bound.bound && bound.selected);
    let own = listing
        .iter()
        .find(|c| c.kind == DiffCheckoutKind::Own)
        .unwrap();
    assert!(!own.selected);

    // Deleting the chat deletes the row.
    f.core.workspace.delete_chat("chat-1").unwrap();
    assert!(reopened.rows().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn selection_follows_the_latest_bind_clear_and_unbind() {
    let f = fixture();
    bind(&f, &f.sub_wt, "op-a");
    let second = bind(&f, &f.root_wt, "op-b");
    assert_eq!(second.reply.targets.len(), 2);
    let selected: Vec<_> = second
        .reply
        .targets
        .iter()
        .filter(|t| t.selected)
        .map(|t| t.path.clone())
        .collect();
    assert_eq!(selected, vec![text(&f.root_wt)]);

    // Re-binding the first one (manual, new op) selects it without a
    // duplicate, and keeps its original source.
    let again = f
        .targets
        .bind(
            "chat-1",
            &text(&f.sub_wt),
            ChatDiffTargetSource::Manual,
            Some("op-c"),
        )
        .unwrap();
    assert_eq!(again.reply.targets.len(), 2);
    let sub = again
        .reply
        .targets
        .iter()
        .find(|t| t.path == text(&f.sub_wt))
        .unwrap();
    assert!(sub.selected);
    assert_eq!(sub.source, ChatDiffTargetSource::Agent);

    let cleared = f.targets.clear_selection("chat-1").unwrap();
    assert_eq!(cleared.targets.len(), 2);
    assert!(cleared.targets.iter().all(|t| !t.selected));
    let listing = f.targets.list_checkouts("chat-1").unwrap().candidates;
    assert!(listing[0].selected, "own checkout selected again");
    assert_eq!(listing.iter().filter(|c| c.bound).count(), 2);
    // Clearing again changes nothing.
    let counter = *f.targets.changes().borrow();
    f.targets.clear_selection("chat-1").unwrap();
    assert_eq!(*f.targets.changes().borrow(), counter);

    bind(&f, &f.root_wt, "op-d");
    let unbound = f.targets.unbind("chat-1", &text(&f.root_wt)).unwrap();
    assert_eq!(unbound.targets.len(), 1);
    assert!(unbound.targets.iter().all(|t| !t.selected));
    let counter = *f.targets.changes().borrow();
    f.targets.unbind("chat-1", &text(&f.root_wt)).unwrap();
    assert_eq!(
        *f.targets.changes().borrow(),
        counter,
        "unknown path is a no-op"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_with_the_same_operation_id_write_nothing() {
    let f = fixture();
    let first = bind(&f, &f.sub_wt, "op-1");
    let row_before = f
        .core
        .workspace
        .chat_diff_targets("chat-1")
        .unwrap()
        .unwrap();
    let counter = *f.targets.changes().borrow();

    let replay = bind(&f, &f.sub_wt, "op-1");
    assert!(replay.replayed);
    assert_eq!(replay.reply, first.reply);
    assert_eq!(replay.path, first.path);
    assert_eq!(*f.targets.changes().borrow(), counter);
    assert_eq!(
        f.core
            .workspace
            .chat_diff_targets("chat-1")
            .unwrap()
            .unwrap(),
        row_before
    );

    // Same path under a new operation id: no duplicate target.
    let retry = bind(&f, &f.sub_wt, "op-2");
    assert!(!retry.replayed);
    assert_eq!(retry.reply.targets.len(), 1);
    let row = f
        .core
        .workspace
        .chat_diff_targets("chat-1")
        .unwrap()
        .unwrap();
    assert_eq!(row.ops, vec!["op-1".to_string(), "op-2".to_string()]);
    assert_eq!(row.targets, row_before.targets);

    // An operation id cannot be reused for another path.
    let conflict = f.targets.bind(
        "chat-1",
        &text(&f.root_wt),
        ChatDiffTargetSource::Agent,
        Some("op-1"),
    );
    assert_eq!(code(conflict), "operation-conflict");

    // Binding the chat's own checkout clears the selection and stores nothing.
    let own = bind(&f, &f.ws, "op-3");
    assert!(!own.selected);
    assert_eq!(own.reply.targets.len(), 1);
    assert!(own.reply.targets.iter().all(|t| !t.selected));
    // No operation id: still fine, and a no-op write is skipped.
    let counter = *f.targets.changes().borrow();
    f.targets
        .bind("chat-1", &text(&f.ws), ChatDiffTargetSource::Manual, None)
        .unwrap();
    assert_eq!(*f.targets.changes().borrow(), counter);
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_everything_outside_the_chat_checkout_family() {
    let f = fixture();
    let t = &f.targets;
    let bind_path = |path: &str| t.bind("chat-1", path, ChatDiffTargetSource::Agent, None);

    // Another repository, and a worktree of it.
    assert_eq!(code(bind_path(&text(&f.other))), "outside-repository");
    let other_wt = f.root.join("other-wt");
    git(
        &f.other,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "o",
            &other_wt.to_string_lossy(),
        ],
    );
    assert_eq!(code(bind_path(&text(&other_wt))), "outside-repository");
    // A plain directory next to the repositories.
    assert_eq!(code(bind_path(&text(&f.root))), "outside-repository");

    // Folders inside allowed checkouts are not checkouts.
    std::fs::create_dir_all(f.sub_wt.join("src/deep")).unwrap();
    assert_eq!(
        code(bind_path(&text(&f.sub_wt.join("src")))),
        "not-a-checkout"
    );
    assert_eq!(
        code(bind_path(&text(&f.sub_wt.join("src/deep")))),
        "not-a-checkout"
    );
    assert_eq!(
        code(bind_path(&text(&f.ws.join(".worktrees/flagship")))),
        "not-a-checkout"
    );
    assert_eq!(
        code(bind_path(&text(&f.ws.join(".worktrees")))),
        "not-a-checkout"
    );

    // The submodule's absorbed module git dir and the root's git dir.
    let module_dir = f.ws.join(".git/modules/flagship");
    assert!(module_dir.is_dir());
    assert_eq!(code(bind_path(&text(&module_dir))), "not-a-checkout");
    assert_eq!(code(bind_path(&text(&f.ws.join(".git")))), "not-a-checkout");

    // Syntax and existence.
    assert_eq!(code(bind_path("")), "invalid-path");
    assert_eq!(code(bind_path("   ")), "invalid-path");
    assert_eq!(code(bind_path("relative/worktree")), "invalid-path");
    assert_eq!(code(bind_path("../ws")), "invalid-path");
    assert_eq!(
        code(bind_path(&text(&f.root.join("missing")))),
        "invalid-path"
    );
    assert_eq!(code(bind_path(&text(&f.ws.join("README")))), "invalid-path");
    assert_eq!(
        code(bind_path(&format!("/{}", "a".repeat(5000)))),
        "invalid-path"
    );

    // A symlink out to an unrelated repository is judged by where it lands.
    let out_link = f.root.join("link-out");
    std::os::unix::fs::symlink(&f.other, &out_link).unwrap();
    assert_eq!(code(bind_path(&text(&out_link))), "outside-repository");
    // A symlink inside the family that points out is refused the same way.
    let sneaky = f.sub_wt.join("sneaky");
    std::os::unix::fs::symlink(&f.other, &sneaky).unwrap();
    assert_eq!(code(bind_path(&text(&sneaky))), "outside-repository");

    // Chat-level refusals.
    f.core
        .workspace
        .create_chat("chat-remote", None, Some("dev-b"), None, Some(text(&f.ws)))
        .unwrap();
    f.core
        .workspace
        .create_chat("chat-nocwd", None, Some("dev-a"), None, None)
        .unwrap();
    let plain = f.root.join("plain-dir");
    std::fs::create_dir_all(&plain).unwrap();
    f.core
        .workspace
        .create_chat("chat-nogit", None, Some("dev-a"), None, Some(text(&plain)))
        .unwrap();
    let sub = text(&f.sub_wt);
    let on = |chat: &str| t.bind(chat, &sub, ChatDiffTargetSource::Agent, None);
    assert_eq!(code(on("chat-remote")), "not-local");
    assert_eq!(code(on("chat-nocwd")), "no-checkout");
    assert_eq!(code(on("chat-nogit")), "no-checkout");
    assert_eq!(code(on("no-such-chat")), "chat-not-found");
    assert_eq!(code(t.list_checkouts("chat-remote")), "not-local");
    assert_eq!(code(t.unbind("chat-remote", &sub)), "not-local");

    // Nothing above wrote a row, and refusals name what is allowed.
    assert!(t.rows().is_empty());
    let message = bind_path(&text(&f.other)).unwrap_err().to_string();
    assert!(message.contains("Allowed:"), "{message}");
    assert!(message.contains(&text(&f.sub_wt)), "{message}");
    let message = bind_path("relative/worktree").unwrap_err().to_string();
    assert!(message.contains("absolute"), "{message}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_symlink_into_an_allowed_worktree_is_accepted_canonicalized() {
    let f = fixture();
    let alias = f.root.join("alias-to-submodule-worktree");
    std::os::unix::fs::symlink(&f.sub_wt, &alias).unwrap();
    let outcome = f
        .targets
        .bind("chat-1", &text(&alias), ChatDiffTargetSource::Manual, None)
        .unwrap();
    assert_eq!(outcome.path, text(&f.sub_wt));
    assert_eq!(outcome.reply.targets.len(), 1);
    assert_eq!(outcome.reply.targets[0].path, text(&f.sub_wt));
    assert_eq!(
        outcome.reply.targets[0].source,
        ChatDiffTargetSource::Manual
    );
    // The real path afterwards is the same target.
    let real = bind(&f, &f.sub_wt, "op-real");
    assert_eq!(real.reply.targets.len(), 1);
    // `..` segments resolve too.
    let dotted = f.sub_wt.join("../eng-3012");
    let dotted = f
        .targets
        .bind("chat-1", &text(&dotted), ChatDiffTargetSource::Manual, None)
        .unwrap();
    assert_eq!(dotted.path, text(&f.sub_wt));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_seventeenth_target_is_refused_until_a_stale_one_is_gone() {
    let f = fixture();
    let mut worktrees = Vec::new();
    for i in 0..=MAX_CHAT_DIFF_TARGETS {
        let path = f.ws.join(format!(".worktrees/ws/many-{i}"));
        git(
            &f.ws,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &format!("many-{i}"),
                &path.to_string_lossy(),
            ],
        );
        worktrees.push(std::fs::canonicalize(&path).unwrap());
    }
    for (i, path) in worktrees.iter().take(MAX_CHAT_DIFF_TARGETS).enumerate() {
        bind(&f, path, &format!("op-{i}"));
    }
    let last = &worktrees[MAX_CHAT_DIFF_TARGETS];
    let refused = f.targets.bind(
        "chat-1",
        &text(last),
        ChatDiffTargetSource::Agent,
        Some("op-last"),
    );
    let message = refused.as_ref().unwrap_err().to_string();
    assert_eq!(code(refused), "too-many-targets");
    assert!(
        message.contains(&MAX_CHAT_DIFF_TARGETS.to_string()),
        "{message}"
    );
    let row = f
        .core
        .workspace
        .chat_diff_targets("chat-1")
        .unwrap()
        .unwrap();
    assert_eq!(row.targets.len(), MAX_CHAT_DIFF_TARGETS);
    assert!(!row.ops.iter().any(|op| op == "op-last"));

    // Re-binding an existing target still works at the limit.
    assert_eq!(
        bind(&f, &worktrees[3], "op-again").reply.targets.len(),
        MAX_CHAT_DIFF_TARGETS
    );

    // A target whose checkout was removed no longer counts: it is pruned.
    git(
        &f.ws,
        &[
            "worktree",
            "remove",
            "--force",
            &worktrees[0].to_string_lossy(),
        ],
    );
    let accepted = bind(&f, last, "op-last-2");
    assert_eq!(accepted.reply.targets.len(), MAX_CHAT_DIFF_TARGETS);
    assert!(
        accepted
            .reply
            .targets
            .iter()
            .all(|t| t.path != text(&worktrees[0]))
    );
    assert!(
        accepted
            .reply
            .targets
            .iter()
            .any(|t| t.path == text(last) && t.selected)
    );
}
