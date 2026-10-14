//! PR review sync against a real offline engine, temp git repositories laid
//! out like the Cofactr workspace (a superproject Space whose `origin` is
//! `acme/workspace`, with `ui` and `flagship` submodules and a nested `kb`
//! repository, each with a GitHub `origin`), and a fake `gh` that serves
//! fixture files (never the real GitHub):
//! - the repository set covers the superproject, submodules, and nested
//!   repositories; PRs of other repositories are ignored;
//! - review requests become items under the destination with the right
//!   titles, references, records, and system Required Actions; a second pass
//!   is a no-op;
//! - submitted review / merged / closed / withdrawn resolve the round's
//!   action and complete the item; a re-request starts a new round;
//! - a merged or closed PR archives its synced item once (chats stay), a
//!   user restore sticks until a new round, reviewed-then-merged archives;
//! - a moved item is not moved back, an archived one is untouched, and a
//!   user status stands until GitHub's state changes;
//! - no destination → no sync; `ghMissing` / `unauthorized` → errors and no
//!   writes;
//! - `PrStatus` caching, `PrMyOpen` / `PrReviewRequested` filtering, and
//!   `StartPrReview` assigning the review Chat to a synced item.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use zeron_engine::pr_review_launch::{PrReviewTools, WorktreesDirEnv};
use zeron_engine::pr_review_sync::{PrReviewSyncConfig, synced_item_id, synced_request_id};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::mock::InstructionMockHarness;
use zeron_proto::{
    GithubError, GithubErrorCode, HarnessId, PrReviewRequestState, UserActionOriginKind,
    WorkItemEffectiveStatus, WorkItemStatus, WorkItemView, WorkOutlineSnapshot,
};
use zeron_rpc::{RpcClient, methods};

const DEVICE: &str = "dev-a";
const SPACE: &str = "space-ws";

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

/// A bare remote with `main` and the PR branch `feature-x`.
fn bare_remote(root: &Path, name: &str) -> PathBuf {
    let bare = root.join(format!("remotes/{name}.git"));
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare", "--initial-branch=main"]);
    let work = root.join(format!("upstream-{name}"));
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q"]);
    commit(&work, "README", &format!("{name}\n"));
    git(&work, &["remote", "add", "origin", &bare.to_string_lossy()]);
    git(&work, &["push", "-q", "origin", "main"]);
    git(&work, &["checkout", "-q", "-b", "feature-x"]);
    commit(&work, "feature.txt", "x\n");
    git(&work, &["push", "-q", "origin", "feature-x"]);
    bare
}

struct Rig {
    _tmp: tempfile::TempDir,
    core: EngineCore,
    _mock: InstructionMockHarness,
    client: RpcClient,
    gh: PathBuf,
    gh_dir: PathBuf,
    gh_log: PathBuf,
}

async fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();

    // The superproject with ui + flagship submodules and a nested kb repo.
    let ws = root.join("workspace");
    std::fs::create_dir_all(ws.join(".devcontainer")).unwrap();
    git(&ws, &["init", "-q"]);
    commit(&ws, "WORKSPACE", "ws\n");
    git(
        &ws,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:acme/workspace.git",
        ],
    );
    for name in ["ui", "flagship"] {
        let bare = bare_remote(&root, name);
        git(
            &ws,
            &["submodule", "add", "-q", &bare.to_string_lossy(), name],
        );
        let sub = ws.join(name);
        let url = format!("https://github.com/acme/{name}");
        git(&sub, &["config", "remote.origin.url", &url]);
        git(
            &sub,
            &[
                "config",
                &format!("url.{}.insteadOf", bare.to_string_lossy()),
                &url,
            ],
        );
    }
    git(&ws, &["commit", "-q", "-m", "add submodules"]);
    let kb = ws.join("tools/kb");
    std::fs::create_dir_all(&kb).unwrap();
    git(&kb, &["init", "-q"]);
    commit(&kb, "KB", "kb\n");
    git(
        &kb,
        &["remote", "add", "origin", "ssh://git@github.com/acme/kb"],
    );

    // The fake gh: fixtures from files, every call logged.
    let gh_dir = root.join("gh");
    std::fs::create_dir_all(&gh_dir).unwrap();
    let gh_log = gh_dir.join("calls.log");
    let script = format!(
        r#"#!/bin/sh
echo "$*" >> '{log}'
dir='{dir}'
if [ -f "$dir/fail" ]; then cat "$dir/fail" >&2; exit 4; fi
case "$1 $2" in
  "api user") cat "$dir/user.json" ;;
  "search prs")
    case "$3" in
      --review-requested=@me) cat "$dir/requested.json" ;;
      --author=@me) cat "$dir/authored.json" ;;
      *) echo "unexpected search $3" >&2; exit 2 ;;
    esac ;;
  "pr view")
    file="$dir/view-$(echo "$5" | tr '/' '-')-$3.json"
    if [ -f "$file" ]; then cat "$file"; else echo "GraphQL: Could not resolve to a PullRequest with the number of $3." >&2; exit 1; fi ;;
  "pr checkout")
    git checkout -q -b feature-x --track origin/feature-x ;;
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
    std::fs::write(gh_dir.join("user.json"), r#"{"login":"me"}"#).unwrap();
    std::fs::write(gh_dir.join("requested.json"), "[]").unwrap();
    std::fs::write(gh_dir.join("authored.json"), "[]").unwrap();

    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("device-id"), DEVICE).unwrap();
    let mock = InstructionMockHarness::default();
    let harnesses = HarnessRegistry::new();
    harnesses.register(Arc::new(mock.clone()));
    let core = EngineCore::assemble(&data, Arc::new(harnesses), HarnessId::Mock, None)
        .expect("engine core assembles");
    core.pr_review_sync
        .reconfigure(PrReviewSyncConfig::for_tests(&gh));
    let rpc = Arc::try_unwrap(core.rpc_service())
        .ok()
        .expect("sole owner")
        .with_pr_review_tools(PrReviewTools {
            gh: Some(gh.clone()),
            worktrees_dir_env: WorktreesDirEnv::Fixed(None),
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
        _mock: mock,
        client,
        gh,
        gh_dir,
        gh_log,
    }
}

/// `(nameWithOwner, number, title, author)`.
type Hit<'a> = (&'a str, u64, &'a str, &'a str);

struct View<'a> {
    state: &'a str,
    /// `(login, state, submittedAt)`.
    reviews: Vec<(&'a str, &'a str, &'a str)>,
    requested: Vec<&'a str>,
}

impl Rig {
    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        self.client
            .call(method, params)
            .await
            .map_err(|e| e.to_string())
    }

    async fn mutate(&self, params: Value) -> Result<Value, String> {
        self.call(methods::WORK_OUTLINE_MUTATE, params).await
    }

    fn snapshot(&self) -> WorkOutlineSnapshot {
        let inputs = self.core.workspace.work_outline_inputs(SPACE);
        zeron_doc::materialize_work_outline(SPACE, &inputs)
    }

    /// A root item "PR reviews" set as the destination.
    async fn destination(&self) -> String {
        let item = self
            .mutate(
                json!({ "op": "createWorkItem", "spaceId": SPACE, "title": "PR reviews",
                "operationId": "dest" }),
            )
            .await
            .unwrap()["workItemId"]
            .as_str()
            .unwrap()
            .to_owned();
        self.mutate(
            json!({ "op": "setPrReviewSyncDestination", "spaceId": SPACE,
            "workItemId": item, "operationId": "set-dest" }),
        )
        .await
        .unwrap();
        item
    }

    fn requested(&self, hits: &[Hit<'_>]) {
        self.write_hits("requested.json", hits);
    }

    fn write_hits(&self, file: &str, hits: &[Hit<'_>]) {
        let list: Vec<Value> = hits
            .iter()
            .map(|(repo, number, title, author)| {
                json!({
                    "number": number, "title": title,
                    "url": format!("https://github.com/{repo}/pull/{number}"),
                    "repository": {"name": repo.split('/').nth(1), "nameWithOwner": repo},
                    "isDraft": false, "updatedAt": "2026-10-14T09:00:00Z",
                    "author": {"login": author},
                })
            })
            .collect();
        std::fs::write(self.gh_dir.join(file), Value::Array(list).to_string()).unwrap();
    }

    fn view(&self, repo: &str, number: u64, title: &str, view: View<'_>) {
        let body = json!({
            "number": number, "title": title,
            "url": format!("https://github.com/{repo}/pull/{number}"),
            "state": view.state, "isDraft": false, "author": {"login": "ada"},
            "reviewDecision": "REVIEW_REQUIRED",
            "reviews": view.reviews.iter().map(|(login, state, at)| json!({
                "author": {"login": login}, "state": state, "submittedAt": at
            })).collect::<Vec<_>>(),
            "reviewRequests": view.requested.iter().map(|login| json!({
                "__typename": "User", "login": login
            })).collect::<Vec<_>>(),
            "statusCheckRollup": [{"status": "COMPLETED", "conclusion": "SUCCESS"}],
            "headRefName": "feature-x", "isCrossRepository": false,
            "baseRefName": "main", "updatedAt": "2026-10-14T09:00:00Z",
        });
        let file = format!("view-{}-{number}.json", repo.replace('/', "-"));
        std::fs::write(self.gh_dir.join(file), body.to_string()).unwrap();
    }

    fn open_requested(&self, repo: &str, number: u64, title: &str) {
        self.view(
            repo,
            number,
            title,
            View {
                state: "OPEN",
                reviews: vec![],
                requested: vec!["me"],
            },
        );
    }

    async fn sync(&self) -> Value {
        self.call(methods::PR_REVIEW_SYNC_NOW, json!({ "spaceId": SPACE }))
            .await
            .unwrap()
    }

    async fn github_err(&self, method: &str, params: Value) -> GithubError {
        let text = self.call(method, params).await.unwrap_err();
        GithubError::from_wire_message(&text).unwrap_or_else(|| panic!("not github: {text}"))
    }

    fn gh_calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.gh_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn item_for(&self, key: &str) -> WorkItemView {
        let snapshot = self.snapshot();
        snapshot
            .item(&synced_item_id(SPACE, key))
            .cloned()
            .unwrap_or_else(|| panic!("no item for {key}"))
    }
}

fn synced_children(snapshot: &WorkOutlineSnapshot, destination: &str) -> Vec<String> {
    snapshot
        .item(destination)
        .unwrap()
        .child_ids
        .iter()
        .map(|id| snapshot.item(id).unwrap().title.clone())
        .collect()
}

// ── tests ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn review_requests_across_the_space_repos_become_items_once() {
    let rig = rig().await;
    let info = rig.call(methods::ENGINE_INFO, json!({})).await.unwrap();
    assert!(
        info["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("pr-review-sync-v1"))
    );
    let dest = rig.destination().await;
    rig.requested(&[
        ("acme/ui", 9, "Fix the  button", "ada"),
        ("acme/flagship", 12, "Speed up search", "bob"),
        ("acme/kb", 3, "Docs", "cy"),
        ("acme/workspace", 5, "Bump submodules", "dee"),
        ("other/thing", 1, "Not ours", "eve"),
    ]);
    rig.open_requested("acme/ui", 9, "Fix the  button");

    let reply = rig.sync().await;
    assert_eq!(reply, json!({ "created": 4, "updated": 0, "resolved": 0 }));
    let snapshot = rig.snapshot();
    let mut titles = synced_children(&snapshot, &dest);
    titles.sort();
    assert_eq!(
        titles,
        vec![
            "flagship#12 · Speed up search",
            "kb#3 · Docs",
            "ui#9 · Fix the button",
            "workspace#5 · Bump submodules",
        ]
    );
    let ui = rig.item_for("acme/ui#9");
    assert_eq!(ui.parent_work_item_id.as_deref(), Some(dest.as_str()));
    assert_eq!(ui.status, WorkItemStatus::Active);
    assert_eq!(
        ui.effective_status,
        WorkItemEffectiveStatus::UserActionRequired
    );
    let reference = snapshot.github_pull_request_reference(&ui.id).unwrap();
    assert_eq!(reference.provider, "github");
    assert_eq!(reference.reference_type, "pullRequest");
    assert_eq!(reference.external_id, "acme/ui#9");
    assert_eq!(reference.url, "https://github.com/acme/ui/pull/9");
    let state = ui.external_sync_state.clone().unwrap();
    assert_eq!(state.round, 1);
    assert_eq!(state.request_state, PrReviewRequestState::Requested);
    assert_eq!(state.external_id, "acme/ui#9");
    let request = snapshot
        .user_action_request(&synced_request_id(SPACE, "acme/ui#9", 1))
        .unwrap();
    assert_eq!(request.text, "Review requested: ui#9 Fix the button");
    assert_eq!(request.dedupe_key.as_deref(), Some("pr-review:acme/ui#9:1"));
    assert_eq!(request.origin_kind, UserActionOriginKind::System);
    assert!(!request.origin_chat_available);
    assert_eq!(
        request.created_by.as_ref().unwrap().kind,
        zeron_proto::WorkOutlineActorKind::System
    );
    let detail = request.info.as_deref().unwrap();
    assert!(detail.contains("Author: @ada"), "{detail}");
    assert!(detail.contains("CI: passing"), "{detail}");
    assert!(
        detail.contains("https://github.com/acme/ui/pull/9"),
        "{detail}"
    );
    assert_eq!(snapshot.user_action_requests.len(), 4);

    // A second pass changes nothing and calls only the search.
    let before = rig.gh_calls().len();
    let reply = rig.sync().await;
    assert_eq!(reply, json!({ "created": 0, "updated": 0, "resolved": 0 }));
    let after = rig.snapshot();
    assert_eq!(after.items.len(), snapshot.items.len());
    assert_eq!(after.user_action_requests.len(), 4);
    assert_eq!(after.external_references.len(), 4);
    let calls = rig.gh_calls()[before..].to_vec();
    assert!(
        calls
            .iter()
            .all(|c| c.starts_with("search prs") || c == "api user"),
        "{calls:?}"
    );

    // The Space-scoped list is filtered to the Space's repositories.
    let listed = rig
        .call(methods::PR_REVIEW_REQUESTED, json!({ "spaceId": SPACE }))
        .await
        .unwrap();
    let mut refs: Vec<String> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            format!(
                "{}/{}#{}",
                s["ref"]["owner"].as_str().unwrap(),
                s["ref"]["repo"].as_str().unwrap(),
                s["ref"]["number"]
            )
        })
        .collect();
    refs.sort();
    assert_eq!(
        refs,
        vec![
            "acme/flagship#12",
            "acme/kb#3",
            "acme/ui#9",
            "acme/workspace#5"
        ]
    );
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["myReviewState"] == "requested")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn review_rounds_resolve_and_reopen() {
    let rig = rig().await;
    rig.destination().await;
    rig.requested(&[
        ("acme/ui", 9, "Fix", "ada"),
        ("acme/flagship", 12, "Search", "bob"),
        ("acme/kb", 3, "Docs", "cy"),
    ]);
    for (repo, n, title) in [
        ("acme/ui", 9, "Fix"),
        ("acme/flagship", 12, "Search"),
        ("acme/kb", 3, "Docs"),
    ] {
        rig.open_requested(repo, n, title);
    }
    assert_eq!(rig.sync().await["created"], 3);

    // ui#9: reviewed; flagship#12: closed; kb#3: request withdrawn.
    rig.requested(&[]);
    let now = chrono::Utc::now().to_rfc3339();
    rig.view(
        "acme/ui",
        9,
        "Fix",
        View {
            state: "OPEN",
            reviews: vec![("me", "COMMENTED", &now)],
            requested: vec![],
        },
    );
    rig.view(
        "acme/flagship",
        12,
        "Search",
        View {
            state: "CLOSED",
            reviews: vec![],
            requested: vec!["me"],
        },
    );
    rig.view(
        "acme/kb",
        3,
        "Docs",
        View {
            state: "OPEN",
            reviews: vec![("bob", "APPROVED", &now)],
            requested: vec![],
        },
    );
    let reply = rig.sync().await;
    assert_eq!(reply, json!({ "created": 0, "updated": 3, "resolved": 3 }));
    let snapshot = rig.snapshot();
    for (key, state, note) in [
        (
            "acme/ui#9",
            PrReviewRequestState::Submitted,
            "Review submitted",
        ),
        (
            "acme/flagship#12",
            PrReviewRequestState::Closed,
            "PR closed",
        ),
        (
            "acme/kb#3",
            PrReviewRequestState::Withdrawn,
            "Review request withdrawn",
        ),
    ] {
        let item = rig.item_for(key);
        assert_eq!(item.status, WorkItemStatus::Completed, "{key}");
        assert_eq!(
            item.effective_status,
            WorkItemEffectiveStatus::Completed,
            "{key}"
        );
        assert_eq!(
            item.external_sync_state.unwrap().request_state,
            state,
            "{key}"
        );
        let request = snapshot
            .user_action_request(&synced_request_id(SPACE, key, 1))
            .unwrap();
        assert!(!request.is_open(), "{key}");
        assert_eq!(
            request.resolution.as_ref().unwrap().note.as_deref(),
            Some(note)
        );
    }
    // Nothing more to do.
    assert_eq!(
        rig.sync().await,
        json!({ "created": 0, "updated": 0, "resolved": 0 })
    );

    // Re-review requested on ui#9: round 2, active again, a new action.
    rig.requested(&[("acme/ui", 9, "Fix", "ada")]);
    rig.view(
        "acme/ui",
        9,
        "Fix",
        View {
            state: "OPEN",
            reviews: vec![("me", "COMMENTED", &now)],
            requested: vec!["me"],
        },
    );
    let reply = rig.sync().await;
    assert_eq!(reply, json!({ "created": 0, "updated": 1, "resolved": 0 }));
    let item = rig.item_for("acme/ui#9");
    assert_eq!(item.status, WorkItemStatus::Active);
    assert_eq!(
        item.effective_status,
        WorkItemEffectiveStatus::UserActionRequired
    );
    let state = item.external_sync_state.unwrap();
    assert_eq!(state.round, 2);
    assert_eq!(state.request_state, PrReviewRequestState::Requested);
    let snapshot = rig.snapshot();
    let request = snapshot
        .user_action_request(&synced_request_id(SPACE, "acme/ui#9", 2))
        .unwrap();
    assert!(request.is_unresolved());
    assert_eq!(request.text, "Re-review requested: ui#9 Fix");
    assert_eq!(request.dedupe_key.as_deref(), Some("pr-review:acme/ui#9:2"));
    assert_eq!(
        rig.sync().await,
        json!({ "created": 0, "updated": 0, "resolved": 0 })
    );

    // Then it merges.
    rig.requested(&[]);
    rig.view(
        "acme/ui",
        9,
        "Fix",
        View {
            state: "MERGED",
            reviews: vec![("me", "COMMENTED", &now)],
            requested: vec![],
        },
    );
    let reply = rig.sync().await;
    assert_eq!(reply, json!({ "created": 0, "updated": 1, "resolved": 1 }));
    let item = rig.item_for("acme/ui#9");
    assert_eq!(item.status, WorkItemStatus::Completed);
    assert_eq!(
        item.external_sync_state.unwrap().request_state,
        PrReviewRequestState::Merged
    );
    let snapshot = rig.snapshot();
    let request = snapshot
        .user_action_request(&synced_request_id(SPACE, "acme/ui#9", 2))
        .unwrap();
    assert_eq!(
        request.resolution.as_ref().unwrap().note.as_deref(),
        Some("PR merged")
    );
    // Items are never deleted.
    assert_eq!(snapshot.items.len(), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn user_moves_archives_and_status_changes_are_respected() {
    let rig = rig().await;
    let dest = rig.destination().await;
    rig.requested(&[
        ("acme/ui", 9, "Fix", "ada"),
        ("acme/flagship", 12, "Search", "bob"),
    ]);
    rig.open_requested("acme/ui", 9, "Fix");
    rig.open_requested("acme/flagship", 12, "Search");
    rig.sync().await;
    let ui = synced_item_id(SPACE, "acme/ui#9");
    let flagship = synced_item_id(SPACE, "acme/flagship#12");

    // The user moves ui#9 to the root and sets it pending; archives flagship.
    rig.mutate(json!({ "op": "moveWorkItem", "workItemId": ui,
        "placement": { "type": "last" }, "operationId": "move" }))
        .await
        .unwrap();
    rig.mutate(json!({ "op": "updateWorkItem", "workItemId": ui,
        "changes": { "status": "pending" }, "operationId": "pending" }))
        .await
        .unwrap();
    rig.mutate(json!({ "op": "archiveWorkItem", "workItemId": flagship,
        "descendantArchivePolicy": { "type": "archiveDescendants" }, "operationId": "archive" }))
        .await
        .unwrap();
    assert_eq!(
        rig.sync().await,
        json!({ "created": 0, "updated": 0, "resolved": 0 })
    );
    let item = rig.item_for("acme/ui#9");
    assert_eq!(item.parent_work_item_id, None, "never moved back");
    assert_eq!(item.status, WorkItemStatus::Pending, "user status stands");
    assert_eq!(
        rig.snapshot().item(&dest).unwrap().child_ids,
        vec![flagship.clone()]
    );

    // GitHub's state changes: the review is submitted and flagship merges.
    rig.requested(&[]);
    let now = chrono::Utc::now().to_rfc3339();
    rig.view(
        "acme/ui",
        9,
        "Fix",
        View {
            state: "OPEN",
            reviews: vec![("me", "APPROVED", &now)],
            requested: vec![],
        },
    );
    rig.view(
        "acme/flagship",
        12,
        "Search",
        View {
            state: "MERGED",
            reviews: vec![],
            requested: vec![],
        },
    );
    let reply = rig.sync().await;
    assert_eq!(reply, json!({ "created": 0, "updated": 1, "resolved": 1 }));
    let item = rig.item_for("acme/ui#9");
    assert_eq!(item.status, WorkItemStatus::Completed, "state change wins");
    assert_eq!(item.parent_work_item_id, None);
    // The archived item is untouched: still archived, record unchanged.
    let archived = rig.item_for("acme/flagship#12");
    assert!(archived.archived_at.is_some());
    assert_eq!(
        archived.external_sync_state.unwrap().request_state,
        PrReviewRequestState::Requested
    );
    // A re-request of the archived PR creates nothing new.
    rig.requested(&[("acme/flagship", 12, "Search", "bob")]);
    assert_eq!(rig.sync().await["created"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_destination_means_no_sync() {
    let rig = rig().await;
    rig.requested(&[("acme/ui", 9, "Fix", "ada")]);
    let error = rig
        .github_err(methods::PR_REVIEW_SYNC_NOW, json!({ "spaceId": SPACE }))
        .await;
    assert_eq!(error.code, GithubErrorCode::Invalid);
    rig.core.pr_review_sync.sync_all().await;
    assert!(rig.gh_calls().is_empty(), "{:?}", rig.gh_calls());
    assert!(rig.snapshot().items.is_empty());

    // Set, then cleared: no sync again.
    rig.destination().await;
    rig.mutate(
        json!({ "op": "setPrReviewSyncDestination", "spaceId": SPACE,
        "workItemId": null, "operationId": "clear" }),
    )
    .await
    .unwrap();
    assert_eq!(rig.snapshot().pr_review_sync_destination(), None);
    let error = rig
        .github_err(methods::PR_REVIEW_SYNC_NOW, json!({ "spaceId": SPACE }))
        .await;
    assert_eq!(error.code, GithubErrorCode::Invalid);
    rig.core.pr_review_sync.sync_all().await;
    assert!(rig.gh_calls().is_empty());
    assert_eq!(rig.snapshot().items.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn setting_the_destination_syncs_the_space() {
    let rig = rig().await;
    let mut config = PrReviewSyncConfig::for_tests(&rig.gh);
    config.background = true;
    rig.core.pr_review_sync.reconfigure(config);
    rig.requested(&[("acme/ui", 9, "Fix", "ada")]);
    rig.open_requested("acme/ui", 9, "Fix");
    rig.destination().await;
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if rig
            .snapshot()
            .item(&synced_item_id(SPACE, "acme/ui#9"))
            .is_some()
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no sync after setting the destination"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn gh_missing_and_signed_out_fail_without_writes() {
    let rig = rig().await;
    rig.destination().await;
    rig.requested(&[("acme/ui", 9, "Fix", "ada")]);
    let items = rig.snapshot().items.len();

    rig.core
        .pr_review_sync
        .reconfigure(PrReviewSyncConfig::for_tests(rig.gh_dir.join("no-such-gh")));
    let error = rig
        .github_err(methods::PR_REVIEW_SYNC_NOW, json!({ "spaceId": SPACE }))
        .await;
    assert_eq!(error.code, GithubErrorCode::GhMissing);
    let error = rig
        .github_err(
            methods::PR_STATUS,
            json!({ "owner": "acme", "repo": "ui", "number": 9 }),
        )
        .await;
    assert_eq!(error.code, GithubErrorCode::GhMissing);

    rig.core
        .pr_review_sync
        .reconfigure(PrReviewSyncConfig::for_tests(&rig.gh));
    std::fs::write(
        rig.gh_dir.join("fail"),
        "To get started with GitHub CLI, please run:  gh auth login",
    )
    .unwrap();
    let error = rig
        .github_err(methods::PR_REVIEW_SYNC_NOW, json!({ "spaceId": SPACE }))
        .await;
    assert_eq!(error.code, GithubErrorCode::Unauthorized);
    assert!(error.message.contains("gh auth login"), "{}", error.message);
    let error = rig
        .github_err(methods::PR_MY_OPEN, json!({ "spaceId": SPACE }))
        .await;
    assert_eq!(error.code, GithubErrorCode::Unauthorized);

    let snapshot = rig.snapshot();
    assert_eq!(snapshot.items.len(), items);
    assert!(snapshot.user_action_requests.is_empty());
    assert!(snapshot.external_references.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn pr_status_is_cached_and_lists_are_filtered() {
    let rig = rig().await;
    rig.view(
        "acme/ui",
        9,
        "Fix",
        View {
            state: "OPEN",
            reviews: vec![("me", "CHANGES_REQUESTED", "2026-10-01T00:00:00Z")],
            requested: vec!["me", "bob"],
        },
    );
    let params = json!({ "owner": "acme", "repo": "ui", "number": 9 });
    let status = rig.call(methods::PR_STATUS, params.clone()).await.unwrap();
    assert_eq!(status["ref"]["url"], "https://github.com/acme/ui/pull/9");
    assert_eq!(status["state"], "open");
    assert_eq!(status["ciStatus"], "passing");
    assert_eq!(status["reviewDecision"], "reviewRequired");
    assert_eq!(status["myReviewState"], "reRequested");
    assert_eq!(status["requestedReviewers"], json!(["me", "bob"]));
    assert_eq!(status["headRefName"], "feature-x");
    assert_eq!(status["baseRefName"], "main");
    assert_eq!(status["author"], "ada");
    let views = |rig: &Rig| {
        rig.gh_calls()
            .iter()
            .filter(|c| c.starts_with("pr view"))
            .count()
    };
    rig.call(methods::PR_STATUS, params.clone()).await.unwrap();
    assert_eq!(views(&rig), 1, "served from the cache");
    let mut forced = params.clone();
    forced["forceRefresh"] = json!(true);
    rig.call(methods::PR_STATUS, forced).await.unwrap();
    assert_eq!(views(&rig), 2);
    let error = rig
        .github_err(
            methods::PR_STATUS,
            json!({ "owner": "acme", "repo": "ui", "number": 404 }),
        )
        .await;
    assert_eq!(error.code, GithubErrorCode::NotFound);
    let error = rig
        .github_err(
            methods::PR_STATUS,
            json!({ "owner": "a b", "repo": "ui", "number": 1 }),
        )
        .await;
    assert_eq!(error.code, GithubErrorCode::Invalid);

    rig.write_hits(
        "authored.json",
        &[
            ("acme/ui", 9, "Fix", "me"),
            ("other/thing", 2, "Elsewhere", "me"),
        ],
    );
    let mine = rig
        .call(methods::PR_MY_OPEN, json!({ "spaceId": SPACE }))
        .await
        .unwrap();
    let mine = mine.as_array().unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0]["ref"]["repo"], "ui");
    // Filled from the status cache.
    assert_eq!(mine[0]["ciStatus"], "passing");

    let error = rig
        .github_err(methods::PR_MY_OPEN, json!({ "spaceId": "nope" }))
        .await;
    assert_eq!(error.code, GithubErrorCode::Invalid);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_review_started_from_a_synced_item_is_assigned_to_it() {
    let rig = rig().await;
    rig.destination().await;
    rig.requested(&[("acme/ui", 9, "Fix", "ada")]);
    rig.open_requested("acme/ui", 9, "Fix");
    rig.sync().await;
    let item = synced_item_id(SPACE, "acme/ui#9");
    let reply = rig
        .call(
            methods::START_PR_REVIEW,
            json!({
                "repoOwner": "acme", "repoName": "ui", "number": 9, "action": "codeReview",
                "spaceId": SPACE, "workItemId": item, "operationId": "review-ui-9",
                "config": serde_json::to_value(zeron_proto::ChatConfig {
                    harness: HarnessId::Mock,
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    sandbox: zeron_proto::SandboxLevel::WorkspaceWrite,
                    auto_approve: true,
                }).unwrap(),
            }),
        )
        .await
        .unwrap();
    let chat = reply["chatId"].as_str().unwrap();
    let row = rig
        .core
        .workspace
        .registry_row(zeron_doc::KIND_WORK_ITEM_CHAT_ASSIGNMENTS, chat)
        .expect("assignment row");
    assert_eq!(row.fields["workItemId"], item.as_str());
    let snapshot = rig.snapshot();
    assert!(
        snapshot
            .item(&item)
            .unwrap()
            .chat_ids
            .iter()
            .any(|c| c == chat)
    );
    // The synced item id is the derived one.
    assert_eq!(
        item,
        zeron_engine::pr_review_sync::synced_item_id(SPACE, "acme/ui#9")
    );
}

fn view_state(rig: &Rig, repo: &str, number: u64, title: &str, state: &str, requested: bool) {
    rig.view(
        repo,
        number,
        title,
        View {
            state,
            reviews: vec![],
            requested: if requested { vec!["me"] } else { vec![] },
        },
    );
}

const ZERO: fn() -> Value = || json!({ "created": 0, "updated": 0, "resolved": 0 });

#[tokio::test(flavor = "multi_thread")]
async fn a_merged_pr_archives_its_item_and_leaves_chats_alone() {
    let rig = rig().await;
    rig.destination().await;
    rig.requested(&[("acme/ui", 9, "Fix", "ada")]);
    rig.open_requested("acme/ui", 9, "Fix");
    rig.sync().await;
    let item = synced_item_id(SPACE, "acme/ui#9");
    let reply = rig
        .call(
            methods::START_PR_REVIEW,
            json!({
                "repoOwner": "acme", "repoName": "ui", "number": 9, "action": "codeReview",
                "spaceId": SPACE, "workItemId": item, "operationId": "review-ui-9",
                "config": serde_json::to_value(zeron_proto::ChatConfig {
                    harness: HarnessId::Mock,
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    sandbox: zeron_proto::SandboxLevel::WorkspaceWrite,
                    auto_approve: true,
                }).unwrap(),
            }),
        )
        .await
        .unwrap();
    let chat = reply["chatId"].as_str().unwrap().to_owned();

    rig.requested(&[]);
    view_state(&rig, "acme/ui", 9, "Fix", "MERGED", false);
    assert_eq!(
        rig.sync().await,
        json!({ "created": 0, "updated": 1, "resolved": 1 })
    );
    let archived = rig.item_for("acme/ui#9");
    assert!(archived.archived_at.is_some());
    assert_eq!(
        archived.archived_by.as_ref().unwrap().kind,
        zeron_proto::WorkOutlineActorKind::System
    );
    assert_eq!(archived.status, WorkItemStatus::Completed);
    let state = archived.external_sync_state.clone().unwrap();
    assert_eq!(state.request_state, PrReviewRequestState::Merged);
    assert_eq!(state.archived_by_sync, Some(PrReviewRequestState::Merged));
    let snapshot = rig.snapshot();
    let request = snapshot
        .user_action_request(&synced_request_id(SPACE, "acme/ui#9", 1))
        .unwrap();
    assert_eq!(
        request.resolution.as_ref().unwrap().note.as_deref(),
        Some("PR merged")
    );
    // Chats stay as they are: still assigned, assignment row untouched.
    assert!(archived.chat_ids.iter().any(|c| *c == chat));
    assert!(
        rig.core
            .workspace
            .registry_row(zeron_doc::KIND_WORK_ITEM_CHAT_ASSIGNMENTS, &chat)
            .is_some()
    );

    // Idempotent: nothing is written or archived again.
    assert_eq!(rig.sync().await, ZERO());
    assert_eq!(rig.item_for("acme/ui#9"), archived);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_pr_archives_and_a_reopened_one_comes_back() {
    let rig = rig().await;
    rig.destination().await;
    rig.requested(&[("acme/flagship", 12, "Search", "bob")]);
    rig.open_requested("acme/flagship", 12, "Search");
    rig.sync().await;

    rig.requested(&[]);
    view_state(&rig, "acme/flagship", 12, "Search", "CLOSED", true);
    assert_eq!(
        rig.sync().await,
        json!({ "created": 0, "updated": 1, "resolved": 1 })
    );
    let item = rig.item_for("acme/flagship#12");
    assert!(item.archived_at.is_some());
    let state = item.external_sync_state.unwrap();
    assert_eq!(state.request_state, PrReviewRequestState::Closed);
    assert_eq!(state.archived_by_sync, Some(PrReviewRequestState::Closed));
    assert_eq!(rig.sync().await, ZERO());

    // The PR is reopened and the review requested again: the item the sync
    // archived returns for a new round (and is not archived while open).
    rig.requested(&[("acme/flagship", 12, "Search", "bob")]);
    view_state(&rig, "acme/flagship", 12, "Search", "OPEN", true);
    assert_eq!(
        rig.sync().await,
        json!({ "created": 0, "updated": 1, "resolved": 0 })
    );
    let item = rig.item_for("acme/flagship#12");
    assert!(item.archived_at.is_none());
    assert_eq!(item.status, WorkItemStatus::Active);
    let state = item.external_sync_state.unwrap();
    assert_eq!(state.round, 2);
    assert_eq!(state.archived_by_sync, None);
    assert_eq!(rig.sync().await, ZERO());

    // Merged this time: archived again.
    rig.requested(&[]);
    view_state(&rig, "acme/flagship", 12, "Search", "MERGED", false);
    assert_eq!(
        rig.sync().await,
        json!({ "created": 0, "updated": 1, "resolved": 1 })
    );
    let item = rig.item_for("acme/flagship#12");
    assert!(item.archived_at.is_some());
    assert_eq!(
        item.external_sync_state.unwrap().archived_by_sync,
        Some(PrReviewRequestState::Merged)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_user_restore_is_not_archived_again_until_the_state_changes() {
    let rig = rig().await;
    rig.destination().await;
    rig.requested(&[("acme/ui", 9, "Fix", "ada")]);
    rig.open_requested("acme/ui", 9, "Fix");
    rig.sync().await;
    let item = synced_item_id(SPACE, "acme/ui#9");

    rig.requested(&[]);
    view_state(&rig, "acme/ui", 9, "Fix", "MERGED", false);
    rig.sync().await;
    assert!(rig.item_for("acme/ui#9").archived_at.is_some());

    rig.mutate(json!({ "op": "restoreWorkItem", "workItemId": item, "operationId": "restore" }))
        .await
        .unwrap();
    for _ in 0..3 {
        assert_eq!(rig.sync().await, ZERO());
        let restored = rig.item_for("acme/ui#9");
        assert!(restored.archived_at.is_none(), "stays restored");
        assert_eq!(restored.status, WorkItemStatus::Completed);
    }

    // A new state change: reopened and requested again (round 2), then
    // merged again. Only now may the sync archive it again.
    rig.requested(&[("acme/ui", 9, "Fix", "ada")]);
    rig.open_requested("acme/ui", 9, "Fix");
    assert_eq!(
        rig.sync().await,
        json!({ "created": 0, "updated": 1, "resolved": 0 })
    );
    assert!(rig.item_for("acme/ui#9").archived_at.is_none());
    rig.requested(&[]);
    view_state(&rig, "acme/ui", 9, "Fix", "MERGED", false);
    assert_eq!(
        rig.sync().await,
        json!({ "created": 0, "updated": 1, "resolved": 1 })
    );
    let item = rig.item_for("acme/ui#9");
    assert!(item.archived_at.is_some());
    assert_eq!(item.external_sync_state.unwrap().round, 2);
    assert_eq!(rig.sync().await, ZERO());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reviewed_item_is_archived_when_its_pr_merges() {
    let rig = rig().await;
    rig.destination().await;
    rig.requested(&[("acme/ui", 9, "Fix", "ada"), ("acme/kb", 3, "Docs", "cy")]);
    rig.open_requested("acme/ui", 9, "Fix");
    rig.open_requested("acme/kb", 3, "Docs");
    rig.sync().await;

    // ui#9 reviewed, kb#3 withdrawn: completed, not archived while open.
    rig.requested(&[]);
    let now = chrono::Utc::now().to_rfc3339();
    rig.view(
        "acme/ui",
        9,
        "Fix",
        View {
            state: "OPEN",
            reviews: vec![("me", "APPROVED", &now)],
            requested: vec![],
        },
    );
    view_state(&rig, "acme/kb", 3, "Docs", "OPEN", false);
    assert_eq!(
        rig.sync().await,
        json!({ "created": 0, "updated": 2, "resolved": 2 })
    );
    for key in ["acme/ui#9", "acme/kb#3"] {
        let item = rig.item_for(key);
        assert_eq!(item.status, WorkItemStatus::Completed, "{key}");
        assert!(item.archived_at.is_none(), "{key}");
    }
    assert_eq!(rig.sync().await, ZERO());

    // Both merge later: archived at the merge.
    rig.view(
        "acme/ui",
        9,
        "Fix",
        View {
            state: "MERGED",
            reviews: vec![("me", "APPROVED", &now)],
            requested: vec![],
        },
    );
    view_state(&rig, "acme/kb", 3, "Docs", "MERGED", false);
    assert_eq!(
        rig.sync().await,
        json!({ "created": 0, "updated": 2, "resolved": 0 })
    );
    for key in ["acme/ui#9", "acme/kb#3"] {
        let item = rig.item_for(key);
        assert!(item.archived_at.is_some(), "{key}");
        assert_eq!(item.status, WorkItemStatus::Completed, "{key}");
        let state = item.external_sync_state.unwrap();
        assert_eq!(state.request_state, PrReviewRequestState::Merged, "{key}");
        assert_eq!(state.archived_by_sync, Some(PrReviewRequestState::Merged));
    }
    assert_eq!(rig.sync().await, ZERO());
}

/// A ticket item that links a PR (a Chat created it, or Linear attached it)
/// is not a PR review item: a review request for that PR gets its own synced
/// item, and the ticket item keeps its status, record, and place.
#[tokio::test(flavor = "multi_thread")]
async fn a_ticket_item_linking_a_requested_pr_is_not_adopted() {
    let rig = rig().await;
    let dest = rig.destination().await;
    let ticket = rig
        .mutate(json!({ "op": "createWorkItem", "spaceId": SPACE,
            "title": "ENG-2690 · Receive flow", "operationId": "ticket" }))
        .await
        .unwrap()["workItemId"]
        .as_str()
        .unwrap()
        .to_owned();
    rig.mutate(json!({ "op": "createWorkItemExternalReference", "workItemId": ticket,
        "provider": "github", "referenceType": "pullRequest", "externalId": "acme/ui#9",
        "url": "https://github.com/acme/ui/pull/9", "operationId": "ticket-pr" }))
        .await
        .unwrap();
    rig.requested(&[("acme/ui", 9, "Fix the button", "ada")]);
    rig.open_requested("acme/ui", 9, "Fix the button");

    let reply = rig.sync().await;
    assert_eq!(reply["created"], 1);
    let synced = rig.item_for("acme/ui#9");
    assert_eq!(synced.parent_work_item_id.as_deref(), Some(dest.as_str()));
    let snapshot = rig.snapshot();
    let item = snapshot.item(&ticket).unwrap();
    assert!(item.external_sync_state.is_none());
    assert_eq!(item.status, WorkItemStatus::Pending);
    assert!(item.parent_work_item_id.is_none());
    assert_eq!(snapshot.unresolved_user_action_requests(&ticket).count(), 0);
}
