//! Work Outline launch (Milestone 3 stage 2) against a real offline engine
//! with the instruction-aware mock harness and temp git repositories:
//! - `CreateChatForWorkItem` creates one Chat, assignment, binding, revision,
//!   application, command, and Run, and the queued pin is the delivered one
//!   (acceptance 10, 16);
//! - a crash after EACH workflow step, then a retry, still yields exactly one
//!   of each (acceptance 16);
//! - a bound Chat with a pruned worktree, a moved root, a re-cloned repository,
//!   or a resume from another folder blocks launch AND resume with an
//!   actionable error and never falls back (acceptance 14);
//! - child Chats inherit assignment, binding, and revision (acceptance 19);
//!   harness-native subagents never become Chats (acceptance 20);
//! - bound Chats cannot be retargeted (SetChatCwd, CreateChatWorktree, a Run
//!   carrying another cwd) and skip branch auto-rename; unbound Chats keep
//!   today's behavior.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use zeron_doc::{
    KIND_INSTRUCTION_REVISION_APPLICATIONS, KIND_INSTRUCTION_REVISIONS,
    KIND_WORK_ITEM_CHAT_ASSIGNMENTS, MessageRole, SessionCommandEntry, SessionCommandPayload,
    SessionCommandStatus,
};
use zeron_engine::execution_scope::ExecutionScopeService;
use zeron_engine::work_outline_launch::{LaunchStep, WorkItemLaunchService};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::mock::InstructionMockHarness;
use zeron_proto::{
    CreateChatForWorkItemParams, CreateChatForWorkItemReply, ExecutionRepositoryIdentity,
    HarnessId, RunRequest, SandboxLevel, WorkItemLaunchState, WorkItemPrompt, WorkOutlineError,
    WorkOutlineErrorCode, WorktreeChoice, derive_id, roles,
};
use zeron_rpc::{RpcClient, methods};

const DEVICE: &str = "dev-a";
const SPACE: &str = "space-1";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

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
    std::fs::write(path.join(file), format!("{file}\n")).unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-q", "-m", file]);
}

struct Rig {
    _tmp: tempfile::TempDir,
    core: EngineCore,
    mock: InstructionMockHarness,
    client: RpcClient,
    root: PathBuf,
    repo: PathBuf,
    item: String,
}

fn outline_error(text: String) -> WorkOutlineError {
    text.find("work-outline/")
        .and_then(|at| WorkOutlineError::from_wire_message(&text[at..]))
        .unwrap_or_else(|| panic!("not a work outline error: {text}"))
}

async fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let repo = root.join("repo");
    init_repo(&repo, "README");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("device-id"), DEVICE).unwrap();
    let mock = InstructionMockHarness::default();
    let harnesses = HarnessRegistry::new();
    harnesses.register(Arc::new(mock.clone()));
    let core = EngineCore::assemble(&data, Arc::new(harnesses), HarnessId::Mock, None)
        .expect("engine core assembles");
    let client = zeron_rpc::memory_client(core.rpc_service());
    client
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": SPACE, "deviceId": DEVICE,
                    "path": repo.to_string_lossy() }),
        )
        .await
        .unwrap();
    client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createWorkItem", "spaceId": SPACE, "title": "Launch me",
                    "operationId": "op-item" }),
        )
        .await
        .unwrap();
    client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createWorkItemInstruction", "spaceId": SPACE,
                    "scope": { "type": "spaceRoot" }, "text": "Run the tests first.",
                    "operationId": "op-instruction" }),
        )
        .await
        .unwrap();
    Rig {
        _tmp: tmp,
        core,
        mock,
        client,
        root,
        repo,
        item: derive_id("op-item", roles::ITEM),
    }
}

fn params(rig: &Rig, op: &str, prompt: &str) -> CreateChatForWorkItemParams {
    CreateChatForWorkItemParams {
        work_item_id: rig.item.clone(),
        initial_prompt: WorkItemPrompt {
            text: prompt.into(),
            attachments: Vec::new(),
        },
        config: Some(zeron_proto::ChatConfig {
            harness: HarnessId::Mock,
            model: None,
            reasoning: None,
            model_options: Default::default(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
        }),
        execution_override: None,
        worktree_choice: None,
        operation_id: op.into(),
        target_device_id: None,
    }
}

async fn launch(
    rig: &Rig,
    params: &CreateChatForWorkItemParams,
) -> Result<CreateChatForWorkItemReply, WorkOutlineError> {
    rig.client
        .call(
            methods::CREATE_CHAT_FOR_WORK_ITEM,
            serde_json::to_value(params).unwrap(),
        )
        .await
        .map(|value| serde_json::from_value(value).expect("launch reply decodes"))
        .map_err(|e| outline_error(e.to_string()))
}

async fn continue_chat(rig: &Rig, chat: &str, op: &str, prompt: &str) -> Result<Value, String> {
    rig.client
        .call(
            methods::CONTINUE_CHAT_FOR_WORK_ITEM,
            json!({ "chatId": chat, "prompt": { "text": prompt }, "operationId": op }),
        )
        .await
        .map_err(|e| e.to_string())
}

async fn describe(rig: &Rig, path: &Path) -> ExecutionRepositoryIdentity {
    let value = rig
        .client
        .call(
            methods::DESCRIBE_EXECUTION_REPOSITORY,
            json!({ "path": path.to_string_lossy() }),
        )
        .await
        .expect("describe");
    serde_json::from_value(value).unwrap()
}

fn service(rig: &Rig) -> WorkItemLaunchService {
    WorkItemLaunchService::new(
        rig.core.workspace.clone(),
        rig.core.doc_host.clone(),
        rig.core.sessions.clone(),
        ExecutionScopeService::new(rig.core.workspace.clone(), rig.core.repos.clone()),
    )
}

async fn wait_for(mut predicate: impl FnMut() -> bool, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Turns whose prompt starts with `prefix` (auto-titling runs use other
/// prompts on the same mock).
fn turns_with(rig: &Rig, prefix: &str) -> Vec<zeron_harness::mock::RecordedInstructionTurn> {
    rig.mock
        .turns()
        .into_iter()
        .filter(|t| t.prompt.starts_with(prefix))
        .collect()
}

fn run_cwd(rig: &Rig, turn: &zeron_harness::mock::RecordedInstructionTurn) -> String {
    rig.mock.runs()[turn.run_index].cwd.clone()
}

async fn settle(rig: &Rig, chat: &str) {
    wait_for(
        || !rig.core.sessions.turn_in_flight(chat),
        "the turn to settle",
    )
    .await;
}

fn commands(rig: &Rig, chat: &str) -> Vec<SessionCommandEntry> {
    rig.core
        .doc_host
        .open(chat)
        .unwrap()
        .doc()
        .read_commands()
        .unwrap()
}

fn user_messages(rig: &Rig, chat: &str) -> usize {
    rig.core
        .doc_host
        .open(chat)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap()
        .iter()
        .filter(|e| e.role == MessageRole::User)
        .count()
}

async fn wait_command(rig: &Rig, chat: &str, id: &str) -> SessionCommandEntry {
    wait_for(
        || {
            commands(rig, chat)
                .iter()
                .any(|c| c.id == id && c.status != SessionCommandStatus::Pending)
        },
        "the command to resolve",
    )
    .await;
    commands(rig, chat)
        .into_iter()
        .find(|c| c.id == id)
        .unwrap()
}

fn row_field(rig: &Rig, kind: &str, id: &str, field: &str) -> Option<Value> {
    rig.core
        .workspace
        .registry_row(kind, id)
        .filter(|row| !row.deleted)
        .and_then(|row| row.fields.get(field).cloned())
}

fn applications_of(rig: &Rig, chat: &str) -> Vec<zeron_doc::RegistryRow> {
    rig.core
        .workspace
        .work_outline_inputs(SPACE)
        .applications
        .into_iter()
        .filter(|row| row.fields.get("chatId").and_then(Value::as_str) == Some(chat))
        .collect()
}

fn run_request(prompt: &str, cwd: &str) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        harness: Some(HarnessId::Mock),
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: cwd.into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
        instructions: None,
    }
}

/// Queue a plain Run the way any device (or the Zeron MCP `send`) does.
fn queue_run(rig: &Rig, chat: &str, prompt: &str, cwd: &str) -> String {
    rig.core
        .doc_host
        .queue_command(
            chat,
            SessionCommandPayload::Run {
                request: run_request(prompt, cwd),
                message_id: format!("msg-{prompt}"),
            },
        )
        .expect("queue run")
}

fn assert_rejected_with(entry: &SessionCommandEntry, code: &str) {
    assert_eq!(entry.status, SessionCommandStatus::Rejected, "{entry:?}");
    let resolution = entry.resolution.clone().unwrap_or_default();
    assert!(
        resolution.contains(&format!("execution-scope:{code}")),
        "expected {code}: {resolution}"
    );
}

// ---------------------------------------------------------------------------
// Acceptance 10 + 16: one of everything, pinned revision delivered
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn launch_creates_each_record_once_and_delivers_the_pinned_revision() {
    let rig = rig().await;
    let p = params(&rig, "op-launch", "launch one");
    let reply = launch(&rig, &p).await.expect("launch");
    assert_eq!(reply.chat_id, derive_id("op-launch", roles::CHAT));
    assert_eq!(reply.message_id, derive_id("op-launch", roles::MESSAGE));
    assert_eq!(reply.command_id, derive_id("op-launch", roles::COMMAND));
    assert_eq!(reply.revision_id, derive_id("op-launch", roles::REVISION));
    assert_eq!(
        reply.application_id,
        derive_id("op-launch", roles::APPLICATION)
    );
    assert_eq!(reply.execution_root, rig.repo.to_string_lossy());
    let chat = reply.chat_id.clone();

    wait_for(
        || turns_with(&rig, "launch one").len() == 1,
        "the first turn",
    )
    .await;
    settle(&rig, &chat).await;
    let turn = &turns_with(&rig, "launch one")[0];
    // Acceptance 10: the revision captured at queue time is the delivered one.
    assert_eq!(
        turn.revision_id.as_deref(),
        Some(reply.revision_id.as_str())
    );
    assert_eq!(run_cwd(&rig, turn), rig.repo.to_string_lossy());

    // Records.
    let row = rig.core.workspace.chat(&chat).unwrap().expect("chat row");
    assert_eq!(row.space_id.as_deref(), Some(SPACE));
    assert_eq!(row.cwd.as_deref(), Some(&*rig.repo.to_string_lossy()));
    assert_eq!(
        row_field(&rig, KIND_WORK_ITEM_CHAT_ASSIGNMENTS, &chat, "workItemId"),
        Some(json!(rig.item))
    );
    let binding = rig
        .core
        .workspace
        .chat_execution_binding(&chat)
        .expect("binding");
    assert_eq!(binding.operation_id.as_deref(), Some("op-launch"));
    assert_eq!(
        binding.resolved.configuration.execution_root,
        rig.repo.to_string_lossy()
    );
    assert!(
        rig.core
            .workspace
            .registry_row(KIND_INSTRUCTION_REVISIONS, &reply.revision_id)
            .is_some()
    );
    let applications = applications_of(&rig, &chat);
    assert_eq!(applications.len(), 1);
    assert_eq!(applications[0].fields["reason"], json!("launch"));
    assert_eq!(
        applications[0].fields["turnBoundary"],
        json!({ "messageId": reply.message_id })
    );
    let cmds = commands(&rig, &chat);
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].id, reply.command_id);
    assert!(matches!(
        &cmds[0].payload,
        SessionCommandPayload::RunWithInstructions { instructions, .. }
            if instructions.revision_id == reply.revision_id
    ));
    assert_eq!(user_messages(&rig, &chat), 1);

    // Replay: same reply, nothing new.
    let again = launch(&rig, &p).await.expect("replay");
    assert_eq!(again.chat_id, reply.chat_id);
    assert_eq!(again.revision_id, reply.revision_id);
    assert_eq!(again.state, WorkItemLaunchState::Started);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(turns_with(&rig, "launch one").len(), 1);
    assert_eq!(commands(&rig, &chat).len(), 1);
    assert_eq!(applications_of(&rig, &chat).len(), 1);

    // Same operation id, other inputs.
    let other = launch(&rig, &params(&rig, "op-launch", "launch other"))
        .await
        .expect_err("conflict");
    assert_eq!(other.code, WorkOutlineErrorCode::OperationConflict);

    // Continue: derived ids, retry-safe, pinned to the selected revision.
    let first = continue_chat(&rig, &chat, "op-continue", "launch next")
        .await
        .expect("continue");
    assert_eq!(
        first["messageId"],
        json!(derive_id("op-continue", roles::MESSAGE))
    );
    wait_for(
        || turns_with(&rig, "launch next").len() == 1,
        "the second turn",
    )
    .await;
    settle(&rig, &chat).await;
    let replay = continue_chat(&rig, &chat, "op-continue", "launch next")
        .await
        .expect("continue replay");
    assert_eq!(replay["commandId"], first["commandId"]);
    let conflict = continue_chat(&rig, &chat, "op-continue", "launch different")
        .await
        .expect_err("continue conflict");
    assert!(conflict.contains("operationConflict"), "{conflict}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let next = turns_with(&rig, "launch next");
    assert_eq!(next.len(), 1);
    assert_eq!(
        next[0].revision_id.as_deref(),
        Some(reply.revision_id.as_str())
    );
    assert_eq!(commands(&rig, &chat).len(), 2);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn crash_after_each_step_then_retry_yields_exactly_one_of_each() {
    let rig = rig().await;
    let launcher = service(&rig);
    let steps = [
        LaunchStep::Validated,
        LaunchStep::ChatCreated,
        LaunchStep::Assigned,
        LaunchStep::BindingCaptured,
        LaunchStep::RevisionCreated,
        LaunchStep::RevisionApplied,
        LaunchStep::TurnQueued,
    ];
    for (index, step) in steps.iter().enumerate() {
        let op = format!("op-crash-{index}");
        let prompt = format!("launch crash {index}");
        let p = params(&rig, &op, &prompt);
        launcher.inject_fault(Some(*step));
        let failed = launcher.create_chat_for_work_item(&p).await;
        assert!(failed.is_err(), "{step:?} must fail");
        let reply = launcher
            .create_chat_for_work_item(&p)
            .await
            .unwrap_or_else(|e| panic!("retry after {step:?}: {e}"));
        let chat = reply.chat_id.clone();
        wait_for(|| turns_with(&rig, &prompt).len() == 1, "the launched turn").await;
        settle(&rig, &chat).await;
        // A third call (another retry) changes nothing either.
        launcher.create_chat_for_work_item(&p).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert_eq!(turns_with(&rig, &prompt).len(), 1, "one Run after {step:?}");
        assert_eq!(commands(&rig, &chat).len(), 1, "one command after {step:?}");
        assert_eq!(user_messages(&rig, &chat), 1, "one message after {step:?}");
        assert_eq!(applications_of(&rig, &chat).len(), 1);
        let inputs = rig.core.workspace.work_outline_inputs(SPACE);
        assert_eq!(
            inputs.chats.iter().filter(|c| c.id == chat).count(),
            1,
            "one chat after {step:?}"
        );
        assert_eq!(
            inputs
                .chat_execution_bindings
                .iter()
                .filter(|row| row.id == chat)
                .count(),
            1
        );
        assert_eq!(
            row_field(&rig, KIND_WORK_ITEM_CHAT_ASSIGNMENTS, &chat, "workItemId"),
            Some(json!(rig.item))
        );
        assert_eq!(
            turns_with(&rig, &prompt)[0].revision_id.as_deref(),
            Some(reply.revision_id.as_str())
        );
    }
    // Totals: one per launch.
    let inputs = rig.core.workspace.work_outline_inputs(SPACE);
    assert_eq!(inputs.applications.len(), steps.len());
    assert_eq!(inputs.chat_execution_bindings.len(), steps.len());
    assert_eq!(
        inputs.assignments.iter().filter(|row| !row.deleted).count(),
        steps.len()
    );
    assert!(
        rig.core
            .workspace
            .registry_row(
                KIND_INSTRUCTION_REVISION_APPLICATIONS,
                &derive_id("op-crash-0", roles::APPLICATION)
            )
            .is_some()
    );
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Acceptance 14: strict validation, no fallback
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn pruned_worktree_blocks_launch_and_resume() {
    let rig = rig().await;
    let mut p = params(&rig, "op-prune", "launch prune");
    p.worktree_choice = Some(WorktreeChoice::CreateNew {
        name: "prune me".into(),
    });
    let reply = launch(&rig, &p).await.expect("launch into a new worktree");
    let worktree = rig.repo.join(".worktrees/workspace/prune-me");
    assert_eq!(reply.execution_root, worktree.to_string_lossy());
    let chat = reply.chat_id.clone();
    wait_for(
        || turns_with(&rig, "launch prune").len() == 1,
        "the first turn",
    )
    .await;
    settle(&rig, &chat).await;
    assert_eq!(
        run_cwd(&rig, &turns_with(&rig, "launch prune")[0]),
        worktree.to_string_lossy()
    );
    // A retry reuses the worktree this operation created.
    launch(&rig, &p).await.expect("retry reuses the worktree");
    let identity = describe(&rig, &worktree).await;

    std::fs::remove_dir_all(&worktree).unwrap();

    // Resume through the work-item path: blocked up front.
    let error = continue_chat(&rig, &chat, "op-prune-next", "launch prune next")
        .await
        .expect_err("pruned worktree blocks continue");
    assert!(error.contains("execution-scope:worktreePruned"), "{error}");
    // Resume through a plain Run (any device, MCP send): rejected at drain,
    // no user message, no Run, no fallback.
    let id = queue_run(&rig, &chat, "launch prune raw", &rig.repo.to_string_lossy());
    assert_rejected_with(&wait_command(&rig, &chat, &id).await, "worktreePruned");
    assert_eq!(user_messages(&rig, &chat), 1);
    assert!(turns_with(&rig, "launch prune raw").is_empty());

    // A new launch selecting the pruned worktree is blocked before any write.
    let mut q = params(&rig, "op-prune-2", "launch prune two");
    q.worktree_choice = Some(WorktreeChoice::UseExisting {
        worktree_id: identity.checkout_id.clone(),
        worktree_path: worktree.to_string_lossy().into_owned(),
    });
    let error = launch(&rig, &q).await.expect_err("blocked");
    assert!(
        error.message.contains("execution-scope:worktreePruned"),
        "{error:?}"
    );
    assert!(
        rig.core
            .workspace
            .chat(&derive_id("op-prune-2", roles::CHAT))
            .unwrap()
            .is_none()
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn moved_root_blocks_resume_and_never_falls_back() {
    let rig = rig().await;
    let reply = launch(&rig, &params(&rig, "op-move", "launch move"))
        .await
        .expect("launch");
    let chat = reply.chat_id.clone();
    wait_for(
        || turns_with(&rig, "launch move").len() == 1,
        "the first turn",
    )
    .await;
    settle(&rig, &chat).await;
    std::fs::rename(&rig.repo, rig.root.join("repo-moved")).unwrap();

    let error = continue_chat(&rig, &chat, "op-move-next", "launch move next")
        .await
        .expect_err("moved root blocks continue");
    assert!(error.contains("execution-scope:rootMissing"), "{error}");
    let id = queue_run(&rig, &chat, "launch move raw", "/tmp");
    assert_rejected_with(&wait_command(&rig, &chat, &id).await, "rootMissing");
    assert!(turns_with(&rig, "launch move raw").is_empty());
    // The Chat is never retargeted.
    let row = rig.core.workspace.chat(&chat).unwrap().unwrap();
    assert_eq!(row.cwd.as_deref(), Some(&*rig.repo.to_string_lossy()));

    // A fresh launch is blocked too, before any Chat exists.
    let error = launch(&rig, &params(&rig, "op-move-2", "launch move two"))
        .await
        .expect_err("blocked");
    assert!(
        error.message.contains("execution-scope:rootMissing"),
        "{error:?}"
    );
    assert!(
        rig.core
            .workspace
            .chat(&derive_id("op-move-2", roles::CHAT))
            .unwrap()
            .is_none()
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn recloned_repository_fingerprint_mismatch_blocks_launch_and_resume() {
    let rig = rig().await;
    let other = rig.root.join("other");
    init_repo(&other, "first");
    let identity = describe(&rig, &other).await;
    let mut p = params(&rig, "op-clone", "launch clone");
    p.execution_override = Some(
        serde_json::from_value(json!({
            "configuration": {
                "executionRoot": other.to_string_lossy(),
                "bindings": [{
                    "repoId": identity.repo_id, "repoFingerprint": identity.repo_fingerprint,
                    "repoPath": identity.repo_path, "member": true, "primary": true,
                }],
                "schema": 1,
            }
        }))
        .unwrap(),
    );
    let reply = launch(&rig, &p).await.expect("launch with override");
    let chat = reply.chat_id.clone();
    wait_for(
        || turns_with(&rig, "launch clone").len() == 1,
        "the first turn",
    )
    .await;
    settle(&rig, &chat).await;

    std::fs::remove_dir_all(&other).unwrap();
    init_repo(&other, "second-history");

    let error = continue_chat(&rig, &chat, "op-clone-next", "launch clone next")
        .await
        .expect_err("re-cloned repository blocks continue");
    assert!(
        error.contains("execution-scope:repositoryIdentityMismatch"),
        "{error}"
    );
    let id = queue_run(&rig, &chat, "launch clone raw", &other.to_string_lossy());
    assert_rejected_with(
        &wait_command(&rig, &chat, &id).await,
        "repositoryIdentityMismatch",
    );
    assert!(turns_with(&rig, "launch clone raw").is_empty());

    let mut q = p.clone();
    q.operation_id = "op-clone-2".into();
    let error = launch(&rig, &q).await.expect_err("blocked");
    assert!(
        error
            .message
            .contains("execution-scope:repositoryIdentityMismatch"),
        "{error:?}"
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bound_resume_from_another_folder_is_an_error_not_a_fresh_session() {
    let rig = rig().await;
    let launcher = service(&rig);
    let p = params(&rig, "op-resume", "launch resume");
    // Everything but the Turn exists.
    launcher.inject_fault(Some(LaunchStep::RevisionApplied));
    assert!(launcher.create_chat_for_work_item(&p).await.is_err());
    let chat = derive_id("op-resume", roles::CHAT);
    // The Chat's stored native session ran somewhere else.
    rig.core
        .workspace
        .set_chat_harness_session(&chat, "native-elsewhere", "/somewhere/else");
    let reply = launcher
        .create_chat_for_work_item(&p)
        .await
        .expect("queued");
    let entry = wait_command(&rig, &chat, &reply.command_id).await;
    assert_rejected_with(&entry, "resumeRootMismatch");
    assert!(turns_with(&rig, "launch resume").is_empty());
    assert_eq!(user_messages(&rig, &chat), 0);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_owner_blocks_a_fresh_launch() {
    let rig = rig().await;
    let remote_repo = rig.root.join("remote");
    init_repo(&remote_repo, "r");
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": "space-remote", "deviceId": "dev-b",
                    "path": remote_repo.to_string_lossy() }),
        )
        .await
        .unwrap();
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createWorkItem", "spaceId": "space-remote", "title": "Remote",
                    "operationId": "op-remote-item" }),
        )
        .await
        .unwrap();
    let mut p = params(&rig, "op-remote", "launch remote");
    p.work_item_id = derive_id("op-remote-item", roles::ITEM);
    let error = launch(&rig, &p).await.expect_err("offline owner");
    assert!(
        error.message.contains("execution-scope:deviceOffline"),
        "{error:?}"
    );
    assert!(
        rig.core
            .workspace
            .chat(&derive_id("op-remote", roles::CHAT))
            .unwrap()
            .is_none()
    );
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Retargeting, branch rename, unbound behavior
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn bound_chats_cannot_be_retargeted() {
    let rig = rig().await;
    let reply = launch(&rig, &params(&rig, "op-pin", "launch pin"))
        .await
        .expect("launch");
    let chat = reply.chat_id.clone();
    wait_for(
        || turns_with(&rig, "launch pin").len() == 1,
        "the first turn",
    )
    .await;
    settle(&rig, &chat).await;
    let elsewhere = rig.root.join("elsewhere");
    init_repo(&elsewhere, "e");

    let error = rig
        .client
        .call(
            methods::MUTATE,
            json!({ "op": "setChatCwd", "chatId": chat, "cwd": elsewhere.to_string_lossy() }),
        )
        .await
        .expect_err("SetChatCwd refuses");
    assert!(
        error
            .to_string()
            .contains("execution-scope:boundChatRetarget"),
        "{error}"
    );
    // Setting the root itself is harmless.
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "setChatCwd", "chatId": chat, "cwd": rig.repo.to_string_lossy() }),
        )
        .await
        .expect("same root is allowed");
    let error = rig
        .client
        .call(
            methods::CREATE_CHAT_WORKTREE,
            json!({ "chatId": chat, "repoPath": rig.repo.to_string_lossy(), "name": "nope" }),
        )
        .await
        .expect_err("CreateChatWorktree refuses");
    assert!(
        error
            .to_string()
            .contains("execution-scope:boundChatRetarget"),
        "{error}"
    );
    assert!(!rig.repo.join(".worktrees/workspace/nope").exists());
    // A createChat replay with another cwd never moves an existing Chat.
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": chat, "spaceId": SPACE, "deviceId": DEVICE,
                    "cwd": elsewhere.to_string_lossy() }),
        )
        .await
        .unwrap();
    // The Zeron MCP `send` shape: a Run built from another cwd still runs in
    // the bound root.
    let id = queue_run(&rig, &chat, "launch pin mcp", &elsewhere.to_string_lossy());
    let entry = wait_command(&rig, &chat, &id).await;
    assert_eq!(entry.status, SessionCommandStatus::Applied, "{entry:?}");
    wait_for(
        || turns_with(&rig, "launch pin mcp").len() == 1,
        "the MCP turn",
    )
    .await;
    assert_eq!(
        run_cwd(&rig, &turns_with(&rig, "launch pin mcp")[0]),
        rig.repo.to_string_lossy()
    );
    let row = rig.core.workspace.chat(&chat).unwrap().unwrap();
    assert_eq!(row.cwd.as_deref(), Some(&*rig.repo.to_string_lossy()));
    // A Run carrying a worktree directive is refused.
    let mut request = run_request("launch pin worktree", &rig.repo.to_string_lossy());
    request.worktree = Some(zeron_proto::WorktreeSpec {
        repo_path: rig.repo.to_string_lossy().into_owned(),
        base: "main".into(),
        space_id: None,
    });
    let id = rig
        .core
        .doc_host
        .queue_command(
            &chat,
            SessionCommandPayload::Run {
                request,
                message_id: "msg-worktree".into(),
            },
        )
        .unwrap();
    assert_rejected_with(&wait_command(&rig, &chat, &id).await, "boundChatRetarget");
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn branch_auto_rename_is_skipped_for_bound_chats() {
    let rig = rig().await;
    let repos = zeron_engine::repos::Repos::with_worktrees_root(
        &rig.root.join("data"),
        DEVICE,
        rig.repo.join(".worktrees"),
    );
    let worktree = repos.create_worktree(&rig.repo, "main").await.expect("wt");
    assert!(worktree.branch.starts_with("zeron/"));
    let wt_path = std::fs::canonicalize(&worktree.path).unwrap();
    let identity = describe(&rig, &wt_path).await;
    let mut p = params(&rig, "op-title", "launch title [hold]");
    p.worktree_choice = Some(WorktreeChoice::UseExisting {
        worktree_id: identity.checkout_id,
        worktree_path: wt_path.to_string_lossy().into_owned(),
    });
    let reply = launch(&rig, &p).await.expect("launch");
    let chat = reply.chat_id.clone();
    wait_for(
        || turns_with(&rig, "launch title").len() == 1,
        "the held turn",
    )
    .await;
    rig.core
        .workspace
        .set_chat_branch(&chat, &worktree.branch)
        .unwrap();
    rig.mock.release();
    wait_for(
        || {
            rig.core
                .workspace
                .chat(&chat)
                .ok()
                .flatten()
                .is_some_and(|c| c.title.as_deref().is_some_and(|t| !t.is_empty()))
        },
        "the auto title",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let row = rig.core.workspace.chat(&chat).unwrap().unwrap();
    assert_eq!(row.branch.as_deref(), Some(worktree.branch.as_str()));
    assert_eq!(
        git(&wt_path, &["branch", "--show-current"]),
        worktree.branch
    );
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unbound_chats_keep_todays_cwd_behavior() {
    let rig = rig().await;
    let elsewhere = rig.root.join("elsewhere");
    init_repo(&elsewhere, "e");
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": "plain", "spaceId": SPACE, "deviceId": DEVICE }),
        )
        .await
        .unwrap();
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "setChatCwd", "chatId": "plain", "cwd": elsewhere.to_string_lossy() }),
        )
        .await
        .expect("unbound chats retarget freely");
    let id = queue_run(&rig, "plain", "plain run", &elsewhere.to_string_lossy());
    let entry = wait_command(&rig, "plain", &id).await;
    assert_eq!(entry.status, SessionCommandStatus::Applied, "{entry:?}");
    wait_for(
        || turns_with(&rig, "plain run").len() == 1,
        "the plain turn",
    )
    .await;
    assert_eq!(
        run_cwd(&rig, &turns_with(&rig, "plain run")[0]),
        elsewhere.to_string_lossy()
    );
    // An unassociated Chat is not a work-item Chat.
    let error = continue_chat(&rig, "plain", "op-plain", "plain next")
        .await
        .expect_err("not a work-item chat");
    assert!(error.contains("work-outline/invalid"), "{error}");
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Acceptance 19 (child Chats) and 20 (subagents)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn child_chats_inherit_assignment_binding_and_revision() {
    let rig = rig().await;
    let reply = launch(&rig, &params(&rig, "op-parent", "launch parent"))
        .await
        .expect("launch");
    let parent = reply.chat_id.clone();
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": "child-1", "spaceId": SPACE,
                    "deviceId": DEVICE, "parentChatId": parent }),
        )
        .await
        .unwrap();
    let child = rig.core.workspace.chat("child-1").unwrap().unwrap();
    assert_eq!(child.parent_chat_id.as_deref(), Some(parent.as_str()));
    assert_eq!(child.cwd.as_deref(), Some(&*rig.repo.to_string_lossy()));
    assert_eq!(
        row_field(
            &rig,
            KIND_WORK_ITEM_CHAT_ASSIGNMENTS,
            "child-1",
            "workItemId"
        ),
        Some(json!(rig.item))
    );
    let binding = rig
        .core
        .workspace
        .chat_execution_binding("child-1")
        .expect("inherited binding");
    assert_eq!(
        binding.resolved,
        rig.core
            .workspace
            .chat_execution_binding(&parent)
            .unwrap()
            .resolved
    );
    let applications = applications_of(&rig, "child-1");
    assert_eq!(applications.len(), 1);
    assert_eq!(applications[0].fields["reason"], json!("forkInherit"));
    assert_eq!(
        applications[0].fields["revisionId"],
        json!(reply.revision_id)
    );
    assert_eq!(
        rig.core
            .workspace
            .selected_instruction_revision("child-1")
            .map(|pin| pin.revision_id),
        Some(reply.revision_id.clone())
    );
    // A retried create is a no-op.
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": "child-1", "spaceId": SPACE,
                    "deviceId": DEVICE, "parentChatId": parent }),
        )
        .await
        .unwrap();
    assert_eq!(applications_of(&rig, "child-1").len(), 1);

    // A child explicitly created in another folder keeps the assignment and
    // revision but not the binding.
    let elsewhere = rig.root.join("elsewhere");
    init_repo(&elsewhere, "e");
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": "child-2", "spaceId": SPACE,
                    "deviceId": DEVICE, "parentChatId": parent,
                    "cwd": elsewhere.to_string_lossy() }),
        )
        .await
        .unwrap();
    assert!(
        rig.core
            .workspace
            .chat_execution_binding("child-2")
            .is_none()
    );
    assert_eq!(
        row_field(
            &rig,
            KIND_WORK_ITEM_CHAT_ASSIGNMENTS,
            "child-2",
            "workItemId"
        ),
        Some(json!(rig.item))
    );
    assert_eq!(applications_of(&rig, "child-2").len(), 1);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn harness_native_subagents_are_not_promoted_to_chats() {
    let rig = rig().await;
    let before = rig.core.workspace.read_chats().unwrap().len();
    let reply = launch(&rig, &params(&rig, "op-sub", "launch sub [subagent]"))
        .await
        .expect("launch");
    let chat = reply.chat_id.clone();
    wait_for(|| turns_with(&rig, "launch sub").len() == 1, "the turn").await;
    settle(&rig, &chat).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let chats = rig.core.workspace.read_chats().unwrap();
    assert_eq!(chats.len(), before + 1, "only the launched Chat exists");
    assert!(
        chats
            .iter()
            .all(|c| c.parent_chat_id.as_deref() != Some(chat.as_str()))
    );
    let inputs = rig.core.workspace.work_outline_inputs(SPACE);
    assert_eq!(inputs.assignments.len(), 1);
    rig.core.shutdown().await;
}
