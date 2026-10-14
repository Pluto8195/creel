//! Work item PR links from Chats (docs/pr-review.md "PRs on work items"),
//! against a real offline engine and temp git repositories laid out like the
//! Cofactr workspace (superproject `acme/workspace` with `ui` and `flagship`
//! submodules). No `gh` call is involved; the fake `gh` only fails loudly if
//! one is made.
//! - `gh pr create` output in a Chat's stored transcript links the PR to its
//!   item (and recovers the Chat's `created_in_chat` link);
//! - a manual Chat PR link links; a `mentioned` or branch-inferred PR does
//!   not; a PR outside the Space's repositories does not;
//! - passes are idempotent; a Chat without an item adds nothing; a moved
//!   Chat's old link stays (and does not follow it); a reference the user
//!   removed is not added back; one item can link several PRs.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use zeron_doc::{MessagePart, MessageRole, SessionMessageEntry};
use zeron_engine::pr_review_sync::PrReviewSyncConfig;
use zeron_engine::workspace_host::ChatLinkKind;
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_proto::{ChatLinkSource, HarnessId, ToolCall, WorkOutlineSnapshot};
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

type Transcripts = Arc<Mutex<HashMap<String, Vec<SessionMessageEntry>>>>;

struct Rig {
    _tmp: tempfile::TempDir,
    core: EngineCore,
    client: RpcClient,
    transcripts: Transcripts,
    gh_log: std::path::PathBuf,
}

async fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let ws = root.join("workspace");
    std::fs::create_dir_all(&ws).unwrap();
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
        let bare = root.join(format!("remotes/{name}.git"));
        std::fs::create_dir_all(&bare).unwrap();
        git(&bare, &["init", "-q", "--bare", "--initial-branch=main"]);
        let work = root.join(format!("upstream-{name}"));
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "-q"]);
        commit(&work, "README", &format!("{name}\n"));
        git(&work, &["remote", "add", "origin", &bare.to_string_lossy()]);
        git(&work, &["push", "-q", "origin", "main"]);
        git(
            &ws,
            &["submodule", "add", "-q", &bare.to_string_lossy(), name],
        );
        git(
            &ws.join(name),
            &[
                "config",
                "remote.origin.url",
                &format!("https://github.com/acme/{name}"),
            ],
        );
    }
    git(&ws, &["commit", "-q", "-m", "add submodules"]);

    // A gh that records and fails: the Chat pass must never call it.
    let gh_dir = root.join("gh");
    std::fs::create_dir_all(&gh_dir).unwrap();
    let gh_log = gh_dir.join("calls.log");
    let gh = gh_dir.join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\necho 'no gh here' >&2\nexit 2\n",
            gh_log.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("device-id"), DEVICE).unwrap();
    let core = EngineCore::assemble(
        &data,
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .expect("engine core assembles");
    core.pr_review_sync
        .reconfigure(PrReviewSyncConfig::for_tests(&gh));
    let transcripts: Transcripts = Arc::default();
    let reader = transcripts.clone();
    core.pr_review_sync
        .set_transcript_reader(Arc::new(move |chat_id: &str| {
            reader.lock().unwrap().get(chat_id).cloned()
        }));
    let client = zeron_rpc::memory_client(core.rpc_service());
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
        client,
        transcripts,
        gh_log,
    }
}

fn tool(command: &str, output: &str) -> MessagePart {
    MessagePart::Tool {
        id: format!("t-{}", command.len()),
        call: ToolCall::Exec {
            command: command.into(),
        },
        is_error: false,
        resolved: true,
        output: Some(output.into()),
        diff: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
        subagent_ref: None,
        subagent_status: None,
        subagent_tail: None,
    }
}

fn assistant(parts: Vec<MessagePart>) -> SessionMessageEntry {
    SessionMessageEntry {
        id: "m1".into(),
        role: MessageRole::Assistant,
        parts,
        created_at: 1,
        device_id: DEVICE.into(),
        status: None,
        continuation_of: None,
    }
}

impl Rig {
    async fn call(&self, method: &str, params: Value) -> Value {
        self.client
            .call(method, params)
            .await
            .unwrap_or_else(|e| panic!("{method}: {e}"))
    }

    async fn mutate(&self, params: Value) -> Value {
        self.call(methods::WORK_OUTLINE_MUTATE, params).await
    }

    fn snapshot(&self) -> WorkOutlineSnapshot {
        let inputs = self.core.workspace.work_outline_inputs(SPACE);
        zeron_doc::materialize_work_outline(SPACE, &inputs)
    }

    async fn item(&self, op: &str, title: &str) -> String {
        self.mutate(
            json!({ "op": "createWorkItem", "spaceId": SPACE, "title": title,
            "operationId": op }),
        )
        .await["workItemId"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn chat(&self, chat_id: &str, item: Option<&str>) {
        self.call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": chat_id, "spaceId": SPACE, "deviceId": DEVICE }),
        )
        .await;
        if let Some(item) = item {
            self.assign(chat_id, item).await;
        }
    }

    async fn assign(&self, chat_id: &str, item: &str) {
        self.mutate(json!({ "op": "assignChatToWorkItem", "chatId": chat_id,
            "workItemId": item, "operationId": uuid::Uuid::new_v4().to_string() }))
            .await;
    }

    fn link(&self, chat_id: &str, url: &str, source: ChatLinkSource) {
        assert!(
            self.core
                .workspace
                .set_chat_link(chat_id, ChatLinkKind::Pr, Some(url), source)
                .unwrap()
        );
    }

    async fn pass(&self) -> zeron_engine::work_outline_pr_links::ChatLinkPass {
        self.core
            .pr_review_sync
            .link_chat_pull_requests(SPACE)
            .await
            .expect("pass runs")
    }

    /// `(externalId, url, created by system)` of every PR the item links.
    fn prs(&self, item: &str) -> Vec<(String, String, bool)> {
        let mut prs: Vec<_> = self
            .snapshot()
            .external_references
            .into_iter()
            .filter(|r| {
                r.work_item_id == item
                    && r.provider == "github"
                    && r.reference_type == "pullRequest"
            })
            .map(|r| {
                let system = r
                    .created_by
                    .is_some_and(|a| a.kind == zeron_proto::WorkOutlineActorKind::System);
                (r.external_id, r.url, system)
            })
            .collect();
        prs.sort();
        prs
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn gh_pr_create_output_in_a_transcript_links_the_pr_to_the_chats_item() {
    let rig = rig().await;
    let ticket = rig.item("t", "ENG-2690 · Receive flow").await;
    rig.chat("c1", Some(&ticket)).await;
    // The live tap only saw the narration (`mentioned`); the transcript has
    // the tool call and its output.
    rig.link(
        "c1",
        "https://github.com/acme/ui/pull/77",
        ChatLinkSource::Mentioned,
    );
    rig.transcripts.lock().unwrap().insert(
        "c1".into(),
        vec![assistant(vec![
            tool("gh pr view 5", "https://github.com/acme/ui/pull/5"),
            tool(
                "gh pr create --fill",
                "Creating pull request for eng-2690 into main\n\nhttps://github.com/acme/ui/pull/77\n",
            ),
        ])],
    );

    let pass = rig.pass().await;
    assert_eq!(pass.linked, 1);
    assert_eq!(pass.scanned, 1);
    assert_eq!(pass.recovered, 1);
    assert_eq!(
        rig.prs(&ticket),
        vec![(
            "acme/ui#77".to_owned(),
            "https://github.com/acme/ui/pull/77".to_owned(),
            true
        )]
    );
    let links = rig.core.workspace.chat_pr_links("c1").unwrap();
    assert_eq!(links[0].source, ChatLinkSource::CreatedInChat);
    // Status is untouched; a second pass adds nothing and re-reads nothing.
    let item = rig.snapshot().item(&ticket).unwrap().clone();
    assert_eq!(item.status, zeron_proto::WorkItemStatus::Pending);
    let again = rig.pass().await;
    assert_eq!(again.linked + again.scanned, 0);
    assert_eq!(rig.prs(&ticket).len(), 1);
    assert!(!rig.gh_log.exists(), "no gh call");
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_links_count_and_mentioned_branch_or_foreign_prs_do_not() {
    let rig = rig().await;
    let ticket = rig.item("t", "ENG-1 · Two repos").await;
    let other = rig.item("o", "ENG-2 · Nothing durable").await;
    rig.chat("c1", Some(&ticket)).await;
    rig.chat("c2", Some(&other)).await;
    rig.chat("c3", None).await;
    // Manual through the RPC, plus a created one in the second repository.
    rig.call(
        methods::SET_CHAT_LINK,
        json!({ "chatId": "c1", "kind": "pr", "value": "https://github.com/acme/ui/pull/9",
            "operation": "add" }),
    )
    .await;
    rig.link(
        "c1",
        "https://github.com/acme/flagship/pull/12",
        ChatLinkSource::CreatedInChat,
    );
    // Not in the Space.
    rig.link(
        "c1",
        "https://github.com/other/thing/pull/1",
        ChatLinkSource::Manual,
    );
    // Mentioned only, and a branch that GitHub would resolve to a PR.
    rig.link(
        "c2",
        "https://github.com/acme/ui/pull/10",
        ChatLinkSource::Mentioned,
    );
    rig.core
        .workspace
        .set_chat_branch("c2", "feature-x")
        .unwrap();
    // No item.
    rig.link(
        "c3",
        "https://github.com/acme/ui/pull/11",
        ChatLinkSource::Manual,
    );

    assert_eq!(rig.pass().await.linked, 2);
    assert_eq!(
        rig.prs(&ticket)
            .into_iter()
            .map(|(id, _, _)| id)
            .collect::<Vec<_>>(),
        vec!["acme/flagship#12".to_owned(), "acme/ui#9".to_owned()]
    );
    assert!(rig.prs(&other).is_empty());
    let total = rig
        .snapshot()
        .external_references
        .iter()
        .filter(|r| r.provider == "github")
        .count();
    assert_eq!(total, 2, "nothing for c2, c3, or other/thing");
    // The Chat's own links are untouched.
    let sources: Vec<_> = rig
        .core
        .workspace
        .chat_pr_links("c1")
        .unwrap()
        .into_iter()
        .map(|l| (l.url, l.source))
        .collect();
    assert!(sources.contains(&(
        "https://github.com/acme/ui/pull/9".to_owned(),
        ChatLinkSource::Manual
    )));
    assert_eq!(rig.pass().await.linked, 0, "idempotent");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_moved_chat_keeps_its_old_link_and_a_removed_one_stays_removed() {
    let rig = rig().await;
    let first = rig.item("a", "ENG-1 · First").await;
    let second = rig.item("b", "ENG-2 · Second").await;
    rig.chat("c1", Some(&first)).await;
    rig.link(
        "c1",
        "https://github.com/acme/ui/pull/1",
        ChatLinkSource::Manual,
    );
    assert_eq!(rig.pass().await.linked, 1);

    // Moved: the old link stays where it was, the new item does not get it,
    // and a PR created after the move goes to the new item.
    rig.assign("c1", &second).await;
    rig.link(
        "c1",
        "https://github.com/acme/flagship/pull/2",
        ChatLinkSource::CreatedInChat,
    );
    assert_eq!(rig.pass().await.linked, 1);
    assert_eq!(rig.prs(&first).len(), 1);
    assert_eq!(rig.prs(&first)[0].0, "acme/ui#1");
    assert_eq!(rig.prs(&second).len(), 1);
    assert_eq!(rig.prs(&second)[0].0, "acme/flagship#2");

    // The user removes a link: it is not added back.
    let reference = rig
        .snapshot()
        .external_references
        .iter()
        .find(|r| r.work_item_id == second)
        .unwrap()
        .id
        .clone();
    rig.mutate(
        json!({ "op": "removeWorkItemExternalReference", "referenceId": reference,
        "operationId": "rm" }),
    )
    .await;
    assert_eq!(rig.pass().await.linked, 0);
    assert!(rig.prs(&second).is_empty());
}
