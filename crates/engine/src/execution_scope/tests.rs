//! Validator tests over real temporary git repositories (audit section 7.B):
//! plain repository, superproject with two submodule members and one exact
//! worktree each, the submodule `worktree list` quirk, re-cloned paths,
//! pruned / moved / missing worktrees, expected-ref drift, member
//! containment, structure, device and Space checks, and the read-only
//! guarantee.

use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use mast_proto::{
    ExecutionConfiguration, ExecutionHostCapabilities, ExecutionSourceScope,
    ExecutionValidationError, RepositoryBinding, ResolvedExecutionConfiguration, Space,
};

use super::*;

const DEVICE: &str = "dev-a";

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

fn init_repo(path: &Path, file: &str) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q"]);
    std::fs::write(path.join(file), format!("{file}\n")).unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-q", "-m", &format!("init {file}")]);
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    probe: ReposGitProbe,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let data = root.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let repos = Repos::with_worktrees_root(&data, DEVICE, root.join("worktrees"));
        Self {
            _temp: temp,
            root,
            probe: ReposGitProbe::new(repos, DEVICE),
        }
    }

    fn space(&self, path: &Path) -> Space {
        Space {
            id: "space-1".into(),
            device_id: DEVICE.into(),
            path: path.to_string_lossy().into_owned(),
            name: None,
            git_detected: true,
            git_checked_at: None,
            checkout_id: None,
            created_at: Utc::now(),
        }
    }

    async fn binding(&self, path: &Path, member: bool, primary: bool) -> RepositoryBinding {
        let identity = describe_repository(&self.probe, path).await.unwrap();
        RepositoryBinding {
            repo_id: identity.repo_id,
            repo_fingerprint: identity.repo_fingerprint,
            repo_path: identity.repo_path,
            member,
            primary,
            worktree_id: None,
            worktree_path: None,
            expected_ref: None,
        }
    }

    /// A binding pinned to the exact checkout at `checkout`.
    async fn pinned(&self, repo: &Path, checkout: &Path, primary: bool) -> RepositoryBinding {
        let mut binding = self.binding(repo, true, primary).await;
        let at = describe_repository(&self.probe, checkout).await.unwrap();
        binding.worktree_id = Some(at.checkout_id);
        binding.worktree_path = Some(checkout.to_string_lossy().into_owned());
        binding
    }

    async fn validate(
        &self,
        root: &Path,
        bindings: Vec<RepositoryBinding>,
        caps: ExecutionHostCapabilities,
    ) -> Result<ValidatedExecution, Vec<ExecutionValidationError>> {
        let space = self.space(root);
        validate(
            &resolved(root, bindings),
            DEVICE,
            Some(&space),
            caps,
            &self.probe,
        )
        .await
    }
}

fn resolved(root: &Path, bindings: Vec<RepositoryBinding>) -> ResolvedExecutionConfiguration {
    ResolvedExecutionConfiguration {
        space_id: "space-1".into(),
        device_id: DEVICE.into(),
        configuration: ExecutionConfiguration {
            execution_root: root.to_string_lossy().into_owned(),
            bindings,
            schema: 1,
        },
        source_scope: ExecutionSourceScope::SpaceDefault {
            space_id: "space-1".into(),
        },
        invalid_reason: None,
    }
}

fn caps() -> ExecutionHostCapabilities {
    ExecutionHostCapabilities::default()
}

/// `ws` superproject with initialized submodules `ui` and `flagship`, and an
/// exact worktree per member at `ws/.worktrees/<member>/t` (branch `t`).
fn superproject(root: &Path) -> PathBuf {
    let sources = root.join("sources");
    init_repo(&sources.join("ui"), "ui.txt");
    init_repo(&sources.join("flagship"), "flagship.txt");
    let ws = root.join("ws");
    init_repo(&ws, "README");
    for member in ["ui", "flagship"] {
        let source = sources.join(member);
        git(
            &ws,
            &["submodule", "add", "-q", &source.to_string_lossy(), member],
        );
    }
    git(&ws, &["commit", "-q", "-m", "members"]);
    for member in ["ui", "flagship"] {
        let target = ws.join(".worktrees").join(member).join("t");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        git(
            &ws.join(member),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "t",
                &target.to_string_lossy(),
            ],
        );
    }
    ws
}

fn codes(errors: &[ExecutionValidationError]) -> Vec<&'static str> {
    errors.iter().map(ExecutionValidationError::code).collect()
}

#[tokio::test]
async fn plain_repository_main_checkout_validates() {
    let fx = Fixture::new();
    let repo = fx.root.join("plain");
    init_repo(&repo, "a.txt");
    let binding = fx.binding(&repo, true, true).await;
    let validated = fx
        .validate(&repo, vec![binding.clone()], caps())
        .await
        .unwrap();
    assert_eq!(validated.execution_root, repo.to_string_lossy());
    assert!(validated.root_checkout_id.is_some());
    let member = &validated.repositories[0];
    assert_eq!(member.checkout_path, repo.to_string_lossy());
    assert_eq!(member.branch.as_deref(), Some("main"));
    assert!(!member.worktree_configured);
    assert_eq!(
        Some(&member.checkout_id),
        validated.root_checkout_id.as_ref()
    );
    // The id is the topology repository id.
    let common = std::fs::canonicalize(repo.join(".git")).unwrap();
    assert_eq!(binding.repo_id, repository_id(DEVICE, &common));
    assert_eq!(binding.repo_fingerprint.len(), 40);
}

#[tokio::test]
async fn superproject_with_two_member_worktrees_validates() {
    let fx = Fixture::new();
    let ws = superproject(&fx.root);
    let ui_wt = ws.join(".worktrees/ui/t");
    let flagship_wt = ws.join(".worktrees/flagship/t");
    let ui = fx.pinned(&ws.join("ui"), &ui_wt, true).await;
    let mut flagship = fx.pinned(&ws.join("flagship"), &flagship_wt, false).await;
    flagship.expected_ref = Some("t".into());
    assert_ne!(ui.repo_id, flagship.repo_id);
    let validated = fx
        .validate(&ws, vec![ui.clone(), flagship.clone()], caps())
        .await
        .unwrap();
    let [v_ui, v_flagship] = validated.repositories.as_slice() else {
        panic!("two members");
    };
    assert!(v_ui.primary && !v_flagship.primary);
    assert_eq!(v_ui.checkout_path, ui_wt.to_string_lossy());
    assert_eq!(v_flagship.checkout_path, flagship_wt.to_string_lossy());
    assert_eq!(v_ui.branch.as_deref(), Some("t"));
    assert!(v_ui.worktree_configured);
    // Four distinct checkout identities: root, ui main, ui worktree,
    // flagship worktree.
    let ui_main = describe_repository(&fx.probe, &ws.join("ui"))
        .await
        .unwrap();
    let ids = [
        validated.root_checkout_id.clone().unwrap(),
        ui_main.checkout_id.clone(),
        v_ui.checkout_id.clone(),
        v_flagship.checkout_id.clone(),
    ];
    let unique: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), 4);
    // The repository path of a member bound through its worktree is still
    // the submodule's main checkout.
    assert_eq!(ui.repo_path, ws.join("ui").to_string_lossy());
}

#[tokio::test]
async fn submodule_main_checkout_is_identified_by_git_dir_not_list_index() {
    let fx = Fixture::new();
    let ws = superproject(&fx.root);
    let ui = ws.join("ui");
    // The quirk: inside a submodule git lists the main worktree as the
    // module git dir, so "first stanza = main checkout path" is wrong.
    let listed = fx.probe.worktrees(&ui).await;
    let first = canonical_lenient(&listed[0].path);
    assert_ne!(first, ui, "git reports the module dir, not ws/ui");
    assert!(first.to_string_lossy().contains(".git/modules/ui"));
    // Pinning the main checkout explicitly validates (git-dir == common-dir).
    let pinned = fx.pinned(&ui, &ui, true).await;
    let validated = fx.validate(&ws, vec![pinned], caps()).await.unwrap();
    assert_eq!(
        validated.repositories[0].checkout_path,
        ui.to_string_lossy()
    );
    // Describe reports the real main path and marks it main.
    let identity = describe_repository(&fx.probe, &ws.join(".worktrees/ui/t"))
        .await
        .unwrap();
    assert!(!identity.main_checkout);
    assert_eq!(identity.repo_path, ui.to_string_lossy());
    assert_eq!(identity.worktrees[0].path, ui.to_string_lossy());
    assert!(identity.worktrees[0].main);
    assert_eq!(identity.worktrees.len(), 2);
    // An uninitialized submodule directory is not a repository (it would
    // otherwise resolve to the superproject).
    git(&ws, &["submodule", "deinit", "-q", "-f", "flagship"]);
    let mut flagship = fx.binding(&ui, true, false).await;
    flagship.repo_path = ws.join("flagship").to_string_lossy().into_owned();
    let errors = fx.validate(&ws, vec![flagship], caps()).await.unwrap_err();
    assert_eq!(codes(&errors), ["repositoryMissing"]);
}

#[tokio::test]
async fn recloned_path_fails_on_fingerprint() {
    let fx = Fixture::new();
    let repo = fx.root.join("repo");
    init_repo(&repo, "original.txt");
    let binding = fx.binding(&repo, true, true).await;
    std::fs::remove_dir_all(&repo).unwrap();
    init_repo(&repo, "different.txt");
    let again = fx.binding(&repo, true, true).await;
    assert_eq!(again.repo_id, binding.repo_id, "path-derived id survives");
    assert_ne!(again.repo_fingerprint, binding.repo_fingerprint);
    let errors = fx
        .validate(&repo, vec![binding.clone()], caps())
        .await
        .unwrap_err();
    match errors.as_slice() {
        [
            ExecutionValidationError::RepositoryIdentityMismatch {
                found_repo_id: None,
                expected_fingerprint,
                found_fingerprint: Some(found),
                ..
            },
        ] => {
            assert_eq!(*expected_fingerprint, binding.repo_fingerprint);
            assert_eq!(*found, again.repo_fingerprint);
        }
        other => panic!("{other:?}"),
    }
    // A missing repository.
    std::fs::remove_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let errors = fx.validate(&repo, vec![binding], caps()).await.unwrap_err();
    assert_eq!(codes(&errors), ["repositoryMissing"]);
}

#[tokio::test]
async fn worktree_pruned_then_missing_never_falls_back() {
    let fx = Fixture::new();
    let ws = superproject(&fx.root);
    let wt = ws.join(".worktrees/ui/t");
    let pinned = fx.pinned(&ws.join("ui"), &wt, true).await;
    std::fs::remove_dir_all(&wt).unwrap();
    let errors = fx
        .validate(&ws, vec![pinned.clone()], caps())
        .await
        .unwrap_err();
    assert_eq!(codes(&errors), ["worktreePruned"]);
    git(&ws.join("ui"), &["worktree", "prune"]);
    let errors = fx
        .validate(&ws, vec![pinned.clone()], caps())
        .await
        .unwrap_err();
    assert_eq!(codes(&errors), ["worktreeMissing"]);
    // Residual risk (audit 7.7): a worktree recreated at the same path with
    // the same basename gets the same path-derived checkout id, so identity
    // alone accepts it; `expectedRef` is what pins it (next test).
    git(
        &ws.join("ui"),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "other",
            &wt.to_string_lossy(),
        ],
    );
    let validated = fx.validate(&ws, vec![pinned], caps()).await.unwrap();
    assert_eq!(validated.repositories[0].branch.as_deref(), Some("other"));
}

#[tokio::test]
async fn recreated_worktree_with_same_basename_is_caught_by_expected_ref() {
    let fx = Fixture::new();
    let ws = superproject(&fx.root);
    let wt = ws.join(".worktrees/ui/t");
    let mut pinned = fx.pinned(&ws.join("ui"), &wt, true).await;
    pinned.expected_ref = Some("t".into());
    git(
        &ws.join("ui"),
        &["worktree", "remove", "--force", &wt.to_string_lossy()],
    );
    git(
        &ws.join("ui"),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "other",
            &wt.to_string_lossy(),
        ],
    );
    let errors = fx.validate(&ws, vec![pinned], caps()).await.unwrap_err();
    assert_eq!(codes(&errors), ["branchMismatch"]);
}

#[tokio::test]
async fn moved_worktree_is_reported_not_followed() {
    let fx = Fixture::new();
    let ws = superproject(&fx.root);
    let wt = ws.join(".worktrees/ui/t");
    let pinned = fx.pinned(&ws.join("ui"), &wt, true).await;
    let moved = ws.join(".worktrees/ui/moved");
    git(
        &ws.join("ui"),
        &[
            "worktree",
            "move",
            &wt.to_string_lossy(),
            &moved.to_string_lossy(),
        ],
    );
    // Same identity at the new path (the admin dir keeps its name).
    let at_new = describe_repository(&fx.probe, &moved).await.unwrap();
    assert_eq!(Some(&at_new.checkout_id), pinned.worktree_id.as_ref());
    let errors = fx.validate(&ws, vec![pinned], caps()).await.unwrap_err();
    match errors.as_slice() {
        [
            ExecutionValidationError::WorktreeMoved {
                expected_path,
                found_path,
                ..
            },
        ] => {
            assert_eq!(*expected_path, wt.to_string_lossy());
            assert_eq!(*found_path, moved.to_string_lossy());
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn expected_ref_mismatch_blocks_and_sha_matches() {
    let fx = Fixture::new();
    let repo = fx.root.join("repo");
    init_repo(&repo, "a.txt");
    let mut binding = fx.binding(&repo, true, true).await;
    binding.expected_ref = Some("refs/heads/main".into());
    fx.validate(&repo, vec![binding.clone()], caps())
        .await
        .unwrap();
    git(&repo, &["switch", "-q", "-c", "feature"]);
    let errors = fx
        .validate(&repo, vec![binding.clone()], caps())
        .await
        .unwrap_err();
    match errors.as_slice() {
        [
            ExecutionValidationError::BranchMismatch {
                expected_ref,
                found_ref,
                ..
            },
        ] => {
            assert_eq!(expected_ref, "refs/heads/main");
            assert_eq!(found_ref, "feature");
        }
        other => panic!("{other:?}"),
    }
    let head = git(&repo, &["rev-parse", "HEAD"]);
    binding.expected_ref = Some(head[..12].to_owned());
    fx.validate(&repo, vec![binding], caps()).await.unwrap();
}

#[tokio::test]
async fn member_outside_root_blocks_unless_additional_directories() {
    let fx = Fixture::new();
    let root = fx.root.join("root");
    init_repo(&root, "root.txt");
    let outside = fx.root.join("outside");
    init_repo(&outside, "o.txt");
    let binding = fx.binding(&outside, true, false).await;
    let errors = fx
        .validate(&root, vec![binding.clone()], caps())
        .await
        .unwrap_err();
    assert_eq!(codes(&errors), ["memberOutsideRoot"]);
    let allowed = ExecutionHostCapabilities {
        additional_directories: true,
    };
    fx.validate(&root, vec![binding], allowed).await.unwrap();
}

#[tokio::test]
async fn structure_errors_are_collected_with_path_errors() {
    let fx = Fixture::new();
    let ws = superproject(&fx.root);
    let ui = fx.binding(&ws.join("ui"), true, true).await;
    let flagship = fx.binding(&ws.join("flagship"), true, true).await;
    let errors = fx
        .validate(&ws, vec![ui, flagship], caps())
        .await
        .unwrap_err();
    assert_eq!(codes(&errors), ["multiplePrimary"]);

    // Root missing and not a directory; every error is reported.
    let ghost = fx.root.join("ghost");
    let mut gone = fx.binding(&ws.join("ui"), true, false).await;
    gone.repo_path = fx.root.join("nope").to_string_lossy().into_owned();
    let errors = fx.validate(&ghost, vec![gone], caps()).await.unwrap_err();
    assert_eq!(codes(&errors), ["rootMissing", "repositoryMissing"]);
    let file = fx.root.join("file.txt");
    std::fs::write(&file, "x").unwrap();
    let errors = fx.validate(&file, vec![], caps()).await.unwrap_err();
    assert_eq!(codes(&errors), ["rootNotDirectory"]);
    // Relative roots are never probed.
    let mut relative = resolved(Path::new("relative/root"), vec![]);
    relative.configuration.execution_root = "relative/root".into();
    let space = fx.space(&ws);
    let errors = validate(&relative, DEVICE, Some(&space), caps(), &fx.probe)
        .await
        .unwrap_err();
    assert_eq!(codes(&errors), ["configInvalid"]);
}

#[tokio::test]
async fn home_directory_is_never_an_execution_root() {
    let fx = Fixture::new();
    let home = crate::repos::home_dir();
    let errors = fx.validate(&home, vec![], caps()).await.unwrap_err();
    assert_eq!(codes(&errors), ["homeDirectoryRoot"]);
    assert_eq!(
        errors[0].remediation(),
        "Choose a project folder inside your home directory."
    );
    // A symlink alias of HOME is HOME.
    #[cfg(unix)]
    {
        let alias = fx.root.join("home-alias");
        std::os::unix::fs::symlink(&home, &alias).unwrap();
        let errors = fx.validate(&alias, vec![], caps()).await.unwrap_err();
        assert_eq!(codes(&errors), ["homeDirectoryRoot"]);
    }
    // An ordinary folder is not.
    assert!(!is_home_directory(&fx.root));
}

#[tokio::test]
async fn device_and_space_checks_stop_before_touching_paths() {
    let fx = Fixture::new();
    let repo = fx.root.join("repo");
    init_repo(&repo, "a.txt");
    let config = resolved(&repo, vec![]);
    let space = fx.space(&repo);
    let errors = validate(&config, "dev-b", Some(&space), caps(), &fx.probe)
        .await
        .unwrap_err();
    assert_eq!(codes(&errors), ["deviceMismatch"]);
    let errors = validate(&config, DEVICE, None, caps(), &fx.probe)
        .await
        .unwrap_err();
    assert_eq!(codes(&errors), ["spaceMissing"]);
    let mut foreign = space.clone();
    foreign.device_id = "dev-c".into();
    let errors = validate(&config, DEVICE, Some(&foreign), caps(), &fx.probe)
        .await
        .unwrap_err();
    assert_eq!(codes(&errors), ["deviceMismatch"]);
    // An undecodable row reports only its reason.
    let mut broken = config.clone();
    broken.configuration = ExecutionConfiguration {
        execution_root: String::new(),
        bindings: vec![],
        schema: 0,
    };
    broken.invalid_reason = Some("the stored configuration could not be read".into());
    let errors = validate(&broken, DEVICE, Some(&space), caps(), &fx.probe)
        .await
        .unwrap_err();
    assert_eq!(codes(&errors), ["configInvalid"]);
}

#[tokio::test]
async fn validation_is_read_only() {
    let fx = Fixture::new();
    let ws = superproject(&fx.root);
    let ui = ws.join("ui");
    let wt = ws.join(".worktrees/ui/t");
    let mut pinned = fx.pinned(&ui, &wt, true).await;
    pinned.expected_ref = Some("does-not-exist".into());
    let state = |dir: &Path| {
        (
            git(dir, &["rev-parse", "HEAD"]),
            git(dir, &["for-each-ref"]),
            git(dir, &["worktree", "list", "--porcelain"]),
            git(dir, &["status", "--porcelain=v1", "--ignored"]),
        )
    };
    let before = (state(&ws), state(&ui), state(&wt));
    let errors = fx.validate(&ws, vec![pinned], caps()).await.unwrap_err();
    assert_eq!(codes(&errors), ["branchMismatch"]);
    assert_eq!((state(&ws), state(&ui), state(&wt)), before);
}

#[test]
fn rejection_message_carries_the_code_and_remediation() {
    let message = rejection_message(&[
        ExecutionValidationError::WorktreeMissing {
            repo_id: "r".into(),
            worktree_id: "w".into(),
            path: "/p".into(),
        },
        ExecutionValidationError::RootMissing { path: "/r".into() },
    ]);
    assert!(message.starts_with("execution-scope:worktreeMissing: "));
    assert!(message.contains("Recreate the worktree"));
    assert!(message.ends_with("(+1 more)"));
}

#[test]
fn ref_matching_rules() {
    assert!(ref_matches("main", Some("main"), None));
    assert!(ref_matches("refs/heads/main", Some("main"), None));
    assert!(!ref_matches("main", None, Some("abc1234")));
    assert!(ref_matches("abc1234", None, Some("abc1234ffff")));
    assert!(
        !ref_matches("abc", None, Some("abc1234ffff")),
        "too short for a sha"
    );
    assert_eq!(
        found_ref(None, Some("0123456789abcdef")),
        "HEAD (0123456789ab)"
    );
}
