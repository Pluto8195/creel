//! Work Outline agent tools (Milestone 4) against a real offline engine:
//! - a Run of a Chat in a Space gets the Mast MCP injection with a token
//!   bound to that Chat and Run, revoked when the Run ends; a projectless
//!   Chat's Run gets plain `run()` and no token;
//! - `AgentToolCall` authenticates by token only (forged / absent / revoked
//!   tokens are refused) and enforces the agent policy per tool;
//! - mutations run as `Actor { kind: agent, chatId }` and dedupe by
//!   `(token Chat, idempotencyKey)` across Runs.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};
use tokio::sync::Notify;

use zeron_doc::SessionCommandPayload;
use zeron_engine::agent_tools::AgentToolsLaunch;
use zeron_engine::diff_targets::DiffTargets;
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::{AgentToolsInjection, Harness, HarnessError, RunControls};
use zeron_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SteeringMode, WorkOutlineActorKind, WorkOutlineSnapshot, derive_id, roles,
};
use zeron_rpc::{RpcClient, methods};

const SPACE: &str = "space-1";
const CHAT: &str = "chat-agent";
const OTHER_CHAT: &str = "chat-other";
const PROJECTLESS: &str = "chat-projectless";

#[derive(Debug, Clone)]
enum Spawn {
    Plain {
        prompt: String,
    },
    WithTools {
        prompt: String,
        tools: AgentToolsInjection,
    },
}

/// Records how each Run was spawned and holds the Run open until released.
#[derive(Clone, Default)]
struct GatedHarness {
    spawns: Arc<Mutex<Vec<Spawn>>>,
    /// Every Run request as the harness received it.
    requests: Arc<Mutex<Vec<RunRequest>>>,
    release: Arc<Notify>,
}

impl GatedHarness {
    fn stream(&self, request: &RunRequest) -> BoxStream<'static, Result<AgentEvent, HarnessError>> {
        let release = self.release.clone();
        let cwd = request.cwd.clone();
        let started = futures::stream::iter(vec![Ok(AgentEvent::SessionStarted {
            harness: HarnessId::Mock,
            model: "mock".into(),
            tools: vec![],
            cwd,
            session_id: "sess-agent".into(),
            assistant_message_id: "a-1".into(),
        })]);
        let done = futures::stream::once(async move {
            release.notified().await;
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("sess-agent".into()),
            })
        });
        started.chain(done).boxed()
    }
}

#[async_trait]
impl Harness for GatedHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Gated"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    fn supports_agent_tools(&self) -> bool {
        true
    }
    fn instruction_support(&self) -> zeron_harness::InstructionSupport {
        zeron_harness::InstructionSupport::Supported {
            channel: zeron_harness::InstructionChannel::Test,
            replace_strategy: zeron_harness::InstructionReplaceStrategy::RespawnResume,
            max_bytes: 64 * 1024,
        }
    }
    async fn run(
        &self,
        request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.spawns.lock().unwrap().push(Spawn::Plain {
            prompt: request.prompt.clone(),
        });
        self.requests.lock().unwrap().push(request.clone());
        Ok(self.stream(&request))
    }
    async fn run_with_agent_tools(
        &self,
        request: RunRequest,
        _controls: RunControls,
        tools: AgentToolsInjection,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.spawns.lock().unwrap().push(Spawn::WithTools {
            prompt: request.prompt.clone(),
            tools,
        });
        self.requests.lock().unwrap().push(request.clone());
        Ok(self.stream(&request))
    }
}

struct Rig {
    _tmp: tempfile::TempDir,
    core: EngineCore,
    client: RpcClient,
    harness: GatedHarness,
    space_path: String,
}

async fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let space_path = root.join("project");
    std::fs::create_dir_all(&space_path).unwrap();
    let harness = GatedHarness::default();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(harness.clone()));
    let core = EngineCore::assemble(
        &root.join("data"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
    )
    .expect("engine core assembles");
    // No IPC port is served here: name one explicitly (the default dials the
    // port the engine bound, and injects nothing without one).
    core.sessions.set_agent_tools_launch(Some(AgentToolsLaunch {
        command: "/opt/zeron/bin/zeron".into(),
        args: vec!["mcp".into()],
        ipc_port: Some(27999),
    }));
    let client = zeron_rpc::memory_client(core.rpc_service());
    let space_path = space_path.to_string_lossy().into_owned();
    client
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": SPACE, "deviceId": core.device_id,
                    "path": space_path }),
        )
        .await
        .unwrap();
    for chat in [CHAT, OTHER_CHAT] {
        client
            .call(
                methods::MUTATE,
                json!({ "op": "createChat", "chatId": chat, "spaceId": SPACE,
                        "deviceId": core.device_id }),
            )
            .await
            .unwrap();
    }
    client
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": PROJECTLESS, "deviceId": core.device_id }),
        )
        .await
        .unwrap();
    Rig {
        _tmp: tmp,
        core,
        client,
        harness,
        space_path,
    }
}

async fn wait_for<F: FnMut() -> bool>(mut predicate: F, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

fn run_command(prompt: &str, cwd: &str) -> SessionCommandPayload {
    SessionCommandPayload::Run {
        request: RunRequest {
            prompt: prompt.into(),
            harness: Some(HarnessId::Mock),
            model: None,
            reasoning: None,
            model_options: Default::default(),
            cwd: cwd.into(),
            sandbox: SandboxLevel::WorkspaceWrite,
            auto_approve: true,
            resume: None,
            attachments: Vec::new(),
            worktree: None,
            instructions: None,
        },
        message_id: uuid::Uuid::new_v4().to_string(),
    }
}

async fn create_item(rig: &Rig, op: &str, title: &str, parent: Option<&str>) -> String {
    let mut params = json!({ "op": "createWorkItem", "spaceId": SPACE, "title": title,
                             "operationId": op });
    if let Some(parent) = parent {
        params["parentWorkItemId"] = json!(parent);
    }
    rig.client
        .call(methods::WORK_OUTLINE_MUTATE, params)
        .await
        .unwrap();
    derive_id(op, roles::ITEM)
}

async fn tool(rig: &Rig, token: &str, name: &str, arguments: Value) -> Result<Value, String> {
    rig.client
        .call(
            zeron_proto::agent_tools::TOOL_CALL_METHOD,
            json!({ "token": token, "tool": name, "arguments": arguments }),
        )
        .await
        .map_err(|e| e.to_string())
}

fn snapshot(rig: &Rig) -> WorkOutlineSnapshot {
    let inputs = rig.core.workspace.work_outline_inputs(SPACE);
    zeron_doc::materialize_work_outline(SPACE, &inputs)
}

#[tokio::test(flavor = "multi_thread")]
async fn space_chat_runs_get_a_run_scoped_token_and_projectless_runs_get_none() {
    let rig = rig().await;
    let tokens = rig.core.sessions.agent_tokens();

    rig.core
        .doc_host
        .queue_command(CHAT, run_command("hello", &rig.space_path))
        .unwrap();
    wait_for(
        || !rig.harness.spawns.lock().unwrap().is_empty(),
        "space run",
    )
    .await;
    let spawn = rig.harness.spawns.lock().unwrap()[0].clone();
    let Spawn::WithTools { prompt, tools } = spawn else {
        panic!("a Chat in a Space must get agent tools: {spawn:?}");
    };
    assert_eq!(prompt, "hello");
    assert_eq!(tools.server_name, "zeron-agent");
    assert_eq!(tools.args, vec!["mcp".to_owned()]);
    let token = tools.env["ZERON_AGENT_TOKEN"].clone();
    assert_eq!(tools.env["ZERON_IPC_PORT"], "27999");
    assert_eq!(tokens.live_count(), 1);

    let origin = rig
        .client
        .call(
            zeron_proto::agent_tools::AUTHENTICATE_METHOD,
            json!({ "token": token }),
        )
        .await
        .expect("a live Run's token authenticates");
    assert_eq!(origin["chatId"], CHAT);
    assert_eq!(origin["spaceId"], SPACE);
    assert_eq!(origin["deviceId"], rig.core.device_id);
    assert!(!origin["runId"].as_str().unwrap().is_empty());

    // The Run ends: its token is revoked with it.
    rig.harness.release.notify_one();
    wait_for(|| tokens.live_count() == 0, "token revocation").await;
    let err = rig
        .client
        .call(
            zeron_proto::agent_tools::AUTHENTICATE_METHOD,
            json!({ "token": token }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("agent-tools/unauthenticated"), "{err}");
    let err = tool(&rig, &token, "work_outline_read_current", json!({}))
        .await
        .unwrap_err();
    assert!(err.contains("agent-tools/unauthenticated"), "{err}");

    // A projectless Chat keeps today's behavior: plain run, no token.
    rig.core
        .doc_host
        .queue_command(PROJECTLESS, run_command("solo", "~"))
        .unwrap();
    wait_for(
        || rig.harness.spawns.lock().unwrap().len() == 2,
        "projectless run",
    )
    .await;
    let spawn = rig.harness.spawns.lock().unwrap()[1].clone();
    assert!(
        matches!(&spawn, Spawn::Plain { prompt } if prompt == "solo"),
        "{spawn:?}"
    );
    assert_eq!(tokens.live_count(), 0);
    rig.harness.release.notify_one();
}

#[tokio::test(flavor = "multi_thread")]
async fn forged_or_absent_tokens_reach_no_tool() {
    let rig = rig().await;
    let lease = rig
        .core
        .sessions
        .agent_tokens()
        .mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    for token in ["", "zat_forged", &format!("{}0", lease.token())] {
        let err = tool(&rig, token, "work_outline_read_current", json!({}))
            .await
            .unwrap_err();
        assert!(
            err.contains("agent-tools/unauthenticated"),
            "{token:?}: {err}"
        );
    }
    // A missing token field is a malformed call, never an anonymous one.
    assert!(
        rig.client
            .call(
                zeron_proto::agent_tools::TOOL_CALL_METHOD,
                json!({ "tool": "work_outline_read_current" }),
            )
            .await
            .is_err()
    );
    // Unknown tools are refused even with a valid token.
    let err = tool(&rig, lease.token(), "delete_everything", json!({}))
        .await
        .unwrap_err();
    assert!(err.contains("agent-tools/unsupported"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn each_tool_is_allowed_or_denied_per_the_agent_policy() {
    let rig = rig().await;
    let root_a = create_item(&rig, "op-a", "Feature A", None).await;
    let root_b = create_item(&rig, "op-b", "Feature B", None).await;
    let a_child = create_item(&rig, "op-a1", "A step", Some(&root_a)).await;
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();

    // Unassociated: reads show no item; subtree tools are refused.
    let read = tool(&rig, &token, "work_outline_read_current", json!({}))
        .await
        .unwrap();
    assert_eq!(read["workItem"], Value::Null);
    assert_eq!(read["chatId"], CHAT);
    let err = tool(
        &rig,
        &token,
        "work_outline_create_child",
        json!({ "title": "x", "parentWorkItemId": root_a, "idempotencyKey": "k0" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");

    // Associate ONLY the calling Chat.
    let associated = tool(
        &rig,
        &token,
        "work_outline_associate_current_chat",
        json!({ "workItemId": root_a, "idempotencyKey": "assoc" }),
    )
    .await
    .unwrap();
    assert_eq!(associated["chatId"], CHAT);
    let snap = snapshot(&rig);
    let assignment = snap.assignments.iter().find(|a| a.chat_id == CHAT).unwrap();
    assert_eq!(assignment.work_item_id.as_deref(), Some(root_a.as_str()));
    let actor = assignment.assigned_by.as_ref().unwrap();
    assert_eq!(actor.kind, WorkOutlineActorKind::Agent);
    assert_eq!(actor.chat_id.as_deref(), Some(CHAT));
    assert!(snap.assignments.iter().all(|a| a.chat_id != OTHER_CHAT));
    let service = rig.core.rpc_service();
    let err = service
        .work_outline()
        .authorize_agent(
            CHAT,
            &serde_json::from_value(json!({ "op": "assignChatToWorkItem",
                "chatId": OTHER_CHAT, "workItemId": root_a, "operationId": "x" }))
            .unwrap(),
        )
        .unwrap_err();
    assert_eq!(
        err.code,
        zeron_proto::WorkOutlineErrorCode::Unauthorized,
        "{err:?}"
    );

    // Read: the item, its path, and effective instructions.
    let read = tool(&rig, &token, "work_outline_read_current", json!({}))
        .await
        .unwrap();
    assert_eq!(read["workItem"]["id"], root_a);
    assert_eq!(read["workItem"]["children"][0]["id"], a_child);
    assert_eq!(read["effectiveInstructions"]["workItemId"], root_a);

    // Create a child under the Chat's item (default parent) and under a
    // descendant; never under another subtree or at the Space root.
    let created = tool(
        &rig,
        &token,
        "work_outline_create_child",
        json!({ "title": "Agent step", "idempotencyKey": "child-1" }),
    )
    .await
    .unwrap();
    let child = created["workItemId"].as_str().unwrap().to_owned();
    assert_eq!(created["parentWorkItemId"], root_a);
    let view = snapshot(&rig).item(&child).cloned().unwrap();
    let creator = view.created_by.unwrap();
    assert_eq!(creator.kind, WorkOutlineActorKind::Agent);
    assert_eq!(creator.chat_id.as_deref(), Some(CHAT));
    tool(
        &rig,
        &token,
        "work_outline_create_child",
        json!({ "title": "Deeper", "parentWorkItemId": a_child, "idempotencyKey": "child-2",
                "status": "active" }),
    )
    .await
    .unwrap();
    let err = tool(
        &rig,
        &token,
        "work_outline_create_child",
        json!({ "title": "Elsewhere", "parentWorkItemId": root_b, "idempotencyKey": "child-3" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    let err = service
        .work_outline()
        .authorize_agent(
            CHAT,
            &serde_json::from_value(json!({ "op": "createWorkItem", "spaceId": SPACE,
                "title": "Root", "operationId": "x" }))
            .unwrap(),
        )
        .unwrap_err();
    assert_eq!(
        err.code,
        zeron_proto::WorkOutlineErrorCode::Unauthorized,
        "{err:?}"
    );
    let err = tool(
        &rig,
        &token,
        "work_outline_create_child",
        json!({ "title": "Done already", "status": "completed", "idempotencyKey": "child-4" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("agent-tools/invalid"), "{err}");

    // Status: active/completed inside the subtree only.
    tool(
        &rig,
        &token,
        "work_outline_set_status",
        json!({ "status": "active", "idempotencyKey": "s1" }),
    )
    .await
    .unwrap();
    tool(
        &rig,
        &token,
        "work_outline_set_status",
        json!({ "workItemId": child, "status": "completed", "idempotencyKey": "s2" }),
    )
    .await
    .unwrap();
    let snap = snapshot(&rig);
    assert_eq!(
        snap.item(&root_a).unwrap().status,
        zeron_proto::WorkItemStatus::Active
    );
    assert_eq!(
        snap.item(&child).unwrap().status,
        zeron_proto::WorkItemStatus::Completed
    );
    let err = tool(
        &rig,
        &token,
        "work_outline_set_status",
        json!({ "workItemId": root_b, "status": "active", "idempotencyKey": "s3" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    let err = tool(
        &rig,
        &token,
        "work_outline_set_status",
        json!({ "status": "pending", "idempotencyKey": "s4" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("agent-tools/invalid"), "{err}");

    // Everything outside the allow-list is refused by the policy.
    for forbidden in [
        json!({ "op": "updateWorkItem", "workItemId": root_a,
                "changes": { "title": "Renamed" }, "operationId": "x" }),
        json!({ "op": "moveWorkItem", "workItemId": a_child, "operationId": "x" }),
        json!({ "op": "archiveWorkItem", "workItemId": a_child,
                "descendantArchivePolicy": { "type": "archiveDescendants" }, "operationId": "x" }),
        json!({ "op": "removeChatFromWorkItem", "chatId": CHAT, "operationId": "x" }),
        json!({ "op": "createWorkItemInstruction", "spaceId": SPACE,
                "scope": { "type": "spaceRoot" }, "text": "x", "operationId": "x" }),
        json!({ "op": "configureExecution", "scope": { "type": "workItem", "workItemId": root_a },
                "configuration": null, "operationId": "x" }),
    ] {
        let mutation = serde_json::from_value(forbidden.clone()).unwrap();
        let err = service
            .work_outline()
            .authorize_agent(CHAT, &mutation)
            .unwrap_err();
        assert_eq!(
            err.code,
            zeron_proto::WorkOutlineErrorCode::Unauthorized,
            "{forbidden}"
        );
    }

    // Requests: created on the Chat's item with the Chat as origin; an
    // item with an open request cannot be completed (never auto-resolved);
    // only the originating Chat resolves it.
    let request = tool(
        &rig,
        &token,
        "work_outline_create_user_action_request",
        json!({ "request": "Approve the plan?", "detail": "Two options", "idempotencyKey": "r1" }),
    )
    .await
    .unwrap();
    let request_id = request["userActionRequestId"].as_str().unwrap().to_owned();
    assert_eq!(request["workItemId"], root_a);
    let read = tool(&rig, &token, "work_outline_read_current", json!({}))
        .await
        .unwrap();
    assert_eq!(read["workItem"]["effectiveStatus"], "userActionRequired");
    assert_eq!(read["openUserActionRequests"][0]["id"], request_id);
    let err = tool(
        &rig,
        &token,
        "work_outline_set_status",
        json!({ "status": "completed", "idempotencyKey": "s5" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/openUserActionRequests"), "{err}");
    let snap = snapshot(&rig);
    assert_eq!(
        snap.item(&root_a).unwrap().status,
        zeron_proto::WorkItemStatus::Active
    );
    let other =
        rig.core
            .sessions
            .agent_tokens()
            .mint(OTHER_CHAT, "run-9", SPACE, &rig.core.device_id);
    let err = tool(
        &rig,
        other.token(),
        "work_outline_resolve_user_action_request",
        json!({ "userActionRequestId": request_id, "idempotencyKey": "r2" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    tool(
        &rig,
        &token,
        "work_outline_resolve_user_action_request",
        json!({ "userActionRequestId": request_id, "resolution": "User approved in chat",
                "idempotencyKey": "r2" }),
    )
    .await
    .unwrap();
    tool(
        &rig,
        &token,
        "work_outline_set_status",
        json!({ "status": "completed", "idempotencyKey": "s6" }),
    )
    .await
    .unwrap();

    // Proposals: for the item, its parent, or the Space root; pending until
    // the user reviews them (no instruction is written).
    let proposal = tool(
        &rig,
        &token,
        "work_outline_propose_instruction",
        json!({ "target": "spaceRoot", "text": "Prefer small PRs", "idempotencyKey": "r3" }),
    )
    .await
    .unwrap();
    assert!(proposal["proposalId"].is_string(), "{proposal}");
    assert!(snapshot(&rig).instructions.is_empty());
    let err = tool(
        &rig,
        &token,
        "work_outline_propose_instruction",
        json!({ "target": "parent", "text": "x", "idempotencyKey": "r4" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("no parent"), "{err}");
    for name in [
        "work_outline_create_user_action_request",
        "work_outline_resolve_user_action_request",
        "work_outline_propose_instruction",
    ] {
        let err = tool(&rig, &token, name, json!({})).await.unwrap_err();
        assert!(err.contains("agent-tools/invalid"), "{name}: {err}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_with_one_key_dedupe_across_runs_and_conflict_on_new_inputs() {
    let rig = rig().await;
    let root = create_item(&rig, "op-root", "Root", None).await;
    let tokens = rig.core.sessions.agent_tokens();
    let first = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    tool(
        &rig,
        first.token(),
        "work_outline_associate_current_chat",
        json!({ "workItemId": root, "idempotencyKey": "assoc" }),
    )
    .await
    .unwrap();

    let err = tool(
        &rig,
        first.token(),
        "work_outline_create_child",
        json!({ "title": "Step" }),
    )
    .await
    .unwrap_err();
    assert!(
        err.contains("agent-tools/invalid") && err.contains("idempotencyKey"),
        "{err}"
    );

    let args = json!({ "title": "Step", "idempotencyKey": "step-1" });
    let a = tool(
        &rig,
        first.token(),
        "work_outline_create_child",
        args.clone(),
    )
    .await
    .unwrap();
    let b = tool(
        &rig,
        first.token(),
        "work_outline_create_child",
        args.clone(),
    )
    .await
    .unwrap();
    assert_eq!(a["workItemId"], b["workItemId"]);
    let count = |rig: &Rig| snapshot(rig).items.len();
    assert_eq!(count(&rig), 2);

    // A later Run of the same Chat retrying the same key: same item.
    drop(first);
    let second = tokens.mint(CHAT, "run-2", SPACE, &rig.core.device_id);
    let c = tool(&rig, second.token(), "work_outline_create_child", args)
        .await
        .unwrap();
    assert_eq!(a["workItemId"], c["workItemId"]);
    assert_eq!(count(&rig), 2);

    // Same key, different inputs: a conflict, never a second item.
    let err = tool(
        &rig,
        second.token(),
        "work_outline_create_child",
        json!({ "title": "Other", "idempotencyKey": "step-1" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/operationConflict"), "{err}");
    assert_eq!(count(&rig), 2);

    // Another Chat using the same key gets its own operation.
    let other = tokens.mint(OTHER_CHAT, "run-3", SPACE, &rig.core.device_id);
    tool(
        &rig,
        other.token(),
        "work_outline_associate_current_chat",
        json!({ "workItemId": root, "idempotencyKey": "assoc" }),
    )
    .await
    .unwrap();
    let d = tool(
        &rig,
        other.token(),
        "work_outline_create_child",
        json!({ "title": "Step", "idempotencyKey": "step-1" }),
    )
    .await
    .unwrap();
    assert_ne!(a["workItemId"], d["workItemId"]);
    assert_eq!(count(&rig), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_first_turn_runs_with_the_child_chat_config_not_the_callers() {
    let rig = rig().await;
    let lease = rig
        .core
        .sessions
        .agent_tokens()
        .mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let key = "spawn-reviewer";
    let child = derive_id(
        &zeron_proto::agent_tools::operation_id(CHAT, key),
        roles::CHAT,
    );
    rig.client
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": child, "spaceId": SPACE,
                    "deviceId": rig.core.device_id, "parentChatId": CHAT,
                    "config": { "harness": "mock", "model": null, "reasoning": null,
                                "sandbox": "read-only", "autoApprove": false } }),
        )
        .await
        .unwrap();
    // A caller trying to smuggle a wider Run: only the prompt survives.
    let queued = tool(
        &rig,
        lease.token(),
        "chat_queue_child_turn",
        json!({
            "chatId": child,
            "idempotencyKey": key,
            "prompt": "review the diff",
            "command": { "type": "run", "messageId": "",
                "request": { "prompt": "x", "cwd": "/", "sandbox": "danger-full-access",
                             "autoApprove": true } },
            "sandbox": "danger-full-access",
            "autoApprove": true,
            "cwd": "/",
        }),
    )
    .await
    .unwrap();
    assert_eq!(queued["chatId"], child);
    wait_for(
        || !rig.harness.requests.lock().unwrap().is_empty(),
        "child run",
    )
    .await;
    let request = rig.harness.requests.lock().unwrap()[0].clone();
    assert_eq!(request.prompt, "review the diff");
    assert_eq!(request.sandbox, SandboxLevel::ReadOnly);
    assert!(!request.auto_approve);
    assert_eq!(request.cwd, rig.space_path);
    assert!(request.worktree.is_none());

    // Not a child of the caller, or the wrong key: refused.
    let err = tool(
        &rig,
        lease.token(),
        "chat_queue_child_turn",
        json!({ "chatId": OTHER_CHAT, "idempotencyKey": key, "prompt": "hi" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("agent-tools/forbidden"), "{err}");
    let err = tool(
        &rig,
        lease.token(),
        "chat_queue_child_turn",
        json!({ "chatId": child, "idempotencyKey": key, "prompt": "   " }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("agent-tools/invalid"), "{err}");
    rig.harness.release.notify_one();
}

async fn add_instruction(rig: &Rig, op: &str, scope: Value, text: &str) {
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createWorkItemInstruction", "spaceId": SPACE, "scope": scope,
                    "text": text, "operationId": op }),
        )
        .await
        .unwrap();
}

async fn assign_chat(rig: &Rig, op: &str, chat: &str, item: &str) {
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "assignChatToWorkItem", "chatId": chat, "workItemId": item,
                    "operationId": op }),
        )
        .await
        .unwrap();
}

fn launch_args(key: &str, item: &str, sandbox: &str) -> Value {
    json!({
        "idempotencyKey": key, "workItemId": item, "prompt": "go",
        "config": { "harness": "mock", "model": null, "reasoning": null,
                    "modelOptions": {}, "sandbox": sandbox, "autoApprove": true },
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_started_for_a_descendant_item_gets_that_items_instructions() {
    let rig = rig().await;
    let root_a = create_item(&rig, "op-a", "Coordinator", None).await;
    let root_b = create_item(&rig, "op-b", "Elsewhere", None).await;
    let a_child = create_item(&rig, "op-a1", "Step", Some(&root_a)).await;
    let a_grand = create_item(&rig, "op-a11", "Leaf", Some(&a_child)).await;
    let b_child = create_item(&rig, "op-b1", "Other step", Some(&root_b)).await;
    add_instruction(
        &rig,
        "ins-root",
        json!({ "type": "workItem", "workItemId": root_a }),
        "Coordinator rule.",
    )
    .await;
    add_instruction(
        &rig,
        "ins-leaf",
        json!({ "type": "workItem", "workItemId": a_grand }),
        "Leaf rule.",
    )
    .await;
    add_instruction(
        &rig,
        "ins-other",
        json!({ "type": "workItem", "workItemId": b_child }),
        "Other subtree rule.",
    )
    .await;
    assign_chat(&rig, "assign-caller", CHAT, &root_a).await;
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();

    // The child is started FOR the descendant: attached to it, parented to
    // the caller, with the leaf's effective instructions (its own plus the
    // inherited ones), never the caller's item's alone.
    let key = "leaf-worker";
    let reply = tool(
        &rig,
        &token,
        "chat_launch_child_for_item",
        launch_args(key, &a_grand, "workspace-write"),
    )
    .await
    .unwrap();
    let child = derive_id(
        &zeron_proto::agent_tools::operation_id(CHAT, key),
        roles::CHAT,
    );
    assert_eq!(reply["chatId"], child);
    let snap = snapshot(&rig);
    let assignment = snap
        .assignments
        .iter()
        .find(|a| a.chat_id == child)
        .unwrap();
    assert_eq!(assignment.work_item_id.as_deref(), Some(a_grand.as_str()));
    let row = rig.core.workspace.chat(&child).unwrap().unwrap();
    assert_eq!(row.parent_chat_id.as_deref(), Some(CHAT));
    // Approval is never automatic for an agent-started child.
    assert!(!row.config.unwrap().auto_approve);
    let pin = rig
        .core
        .workspace
        .selected_instruction_revision(&child)
        .expect("the launch selected an instruction revision");
    let revision = rig
        .core
        .workspace
        .instruction_revision(&pin.revision_id)
        .unwrap();
    assert_eq!(revision.work_item_id, a_grand);
    let texts: Vec<&str> = revision
        .body
        .entries
        .iter()
        .map(|e| e.text.as_str())
        .collect();
    assert_eq!(texts, ["Coordinator rule.", "Leaf rule."]);
    wait_for(
        || !rig.harness.requests.lock().unwrap().is_empty(),
        "child run",
    )
    .await;
    let request = rig.harness.requests.lock().unwrap()[0].clone();
    assert_eq!(request.prompt, "go");
    let rendered = request.instructions.unwrap().envelope.rendered;
    assert!(rendered.contains("Leaf rule."), "{rendered}");
    assert!(!rendered.contains("Other subtree rule."), "{rendered}");

    // A retry with the same key and inputs is the same Chat, started once.
    let again = tool(
        &rig,
        &token,
        "chat_launch_child_for_item",
        launch_args(key, &a_grand, "workspace-write"),
    )
    .await
    .unwrap();
    assert_eq!(again["chatId"], child);
    assert_eq!(again["commandId"], reply["commandId"]);
    assert_eq!(
        snapshot(&rig)
            .assignments
            .iter()
            .filter(|a| a.chat_id == child)
            .count(),
        1
    );
    // The caller's own item is a valid target too.
    let own = tool(
        &rig,
        &token,
        "chat_launch_child_for_item",
        launch_args("own-item", &root_a, "read-only"),
    )
    .await
    .unwrap();
    let own_chat = own["chatId"].as_str().unwrap().to_owned();
    assert_eq!(
        snapshot(&rig)
            .assignments
            .iter()
            .find(|a| a.chat_id == own_chat)
            .and_then(|a| a.work_item_id.clone()),
        Some(root_a.clone())
    );

    // Outside the caller's subtree (another root, a sibling subtree's
    // descendant, an unknown id): unauthorized, nothing created.
    let chats_before = rig.core.workspace.read_chats().unwrap().len();
    for target in [root_b.as_str(), b_child.as_str(), "no-such-item"] {
        let err = tool(
            &rig,
            &token,
            "chat_launch_child_for_item",
            launch_args(&format!("out-{target}"), target, "workspace-write"),
        )
        .await
        .unwrap_err();
        assert!(err.contains("work-outline/unauthorized"), "{target}: {err}");
    }
    // Never a wider sandbox than the caller's, and a prompt is required.
    let err = tool(
        &rig,
        &token,
        "chat_launch_child_for_item",
        launch_args("wide", &a_grand, "danger-full-access"),
    )
    .await
    .unwrap_err();
    assert!(err.contains("agent-tools/forbidden"), "{err}");
    let mut empty = launch_args("empty", &a_grand, "workspace-write");
    empty["prompt"] = json!("  ");
    let err = tool(&rig, &token, "chat_launch_child_for_item", empty)
        .await
        .unwrap_err();
    assert!(err.contains("agent-tools/invalid"), "{err}");
    assert_eq!(rig.core.workspace.read_chats().unwrap().len(), chats_before);

    // The re-association rule is NOT relaxed: the started child keeps the
    // item it was started for.
    let child_lease = tokens.mint(&child, "run-2", SPACE, &rig.core.device_id);
    let err = tool(
        &rig,
        child_lease.token(),
        "work_outline_associate_current_chat",
        json!({ "workItemId": root_a, "idempotencyKey": "reassoc" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    assert!(err.contains("re-associate"), "{err}");

    // The coordinator closes what its own child raised; an unrelated Chat
    // still cannot.
    let ready = tool(
        &rig,
        child_lease.token(),
        "work_outline_create_user_action_request",
        json!({ "request": "Ready to merge: leaf-1", "idempotencyKey": "ready" }),
    )
    .await
    .unwrap();
    let ready_id = ready["userActionRequestId"].as_str().unwrap().to_owned();
    let other = tokens.mint(OTHER_CHAT, "run-3", SPACE, &rig.core.device_id);
    let err = tool(
        &rig,
        other.token(),
        "work_outline_resolve_user_action_request",
        json!({ "userActionRequestId": ready_id, "idempotencyKey": "close-other" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    tool(
        &rig,
        &token,
        "work_outline_resolve_user_action_request",
        json!({ "userActionRequestId": ready_id, "resolution": "Merged",
                "idempotencyKey": "close-parent" }),
    )
    .await
    .unwrap();
    assert!(
        snapshot(&rig)
            .user_action_requests
            .iter()
            .find(|r| r.id == ready_id)
            .is_some_and(|r| r.state == zeron_proto::UserActionRequestState::Resolved),
        "the coordinator resolved its child's request"
    );

    // An unassociated caller has no subtree to start chats in.
    let err = tool(
        &rig,
        other.token(),
        "chat_launch_child_for_item",
        launch_args("nobody", &a_grand, "workspace-write"),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    rig.harness.release.notify_waiters();
}

#[tokio::test(flavor = "multi_thread")]
async fn read_subtree_lists_descendants_chats_and_requests_within_the_callers_subtree() {
    let rig = rig().await;
    let root_a = create_item(&rig, "op-a", "Coordinator", None).await;
    let root_b = create_item(&rig, "op-b", "Elsewhere", None).await;
    let a_child = create_item(&rig, "op-a1", "Step", Some(&root_a)).await;
    let a_grand = create_item(&rig, "op-a11", "Leaf", Some(&a_child)).await;
    let a_deep = create_item(&rig, "op-a111", "Deep", Some(&a_grand)).await;
    let a_archived = create_item(&rig, "op-a2", "Old", Some(&root_a)).await;
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "archiveWorkItem", "workItemId": a_archived,
                    "descendantArchivePolicy": { "type": "archiveDescendants" },
                    "operationId": "arch" }),
        )
        .await
        .unwrap();
    assign_chat(&rig, "assign-caller", CHAT, &root_a).await;
    assign_chat(&rig, "assign-worker", OTHER_CHAT, &a_grand).await;
    rig.client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createUserActionRequest", "workItemId": a_grand,
                    "origin": { "chatId": OTHER_CHAT }, "text": "Ready to merge",
                    "info": "branch leaf-1", "operationId": "req-1" }),
        )
        .await
        .unwrap();
    let tokens = rig.core.sessions.agent_tokens();
    let lease = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();

    // Default: the caller's item, depth 4 (so the grandchild is in), archived
    // items left out.
    let read = tool(&rig, &token, "work_outline_read_subtree", json!({}))
        .await
        .unwrap();
    assert_eq!(read["rootWorkItemId"], root_a);
    assert_eq!(read["truncated"], false);
    assert_eq!(read["itemCount"], 4);
    let tree = &read["tree"];
    assert_eq!(tree["id"], root_a);
    assert_eq!(tree["children"].as_array().unwrap().len(), 1);
    let step = &tree["children"][0];
    assert_eq!(step["id"], a_child);
    assert_eq!(
        step["effectiveStatus"],
        snapshot(&rig)
            .item(&a_child)
            .map(|i| json!(i.effective_status))
            .unwrap()
    );
    let leaf = &step["children"][0];
    assert_eq!(leaf["id"], a_grand);
    assert_eq!(leaf["chats"][0]["id"], OTHER_CHAT);
    assert_eq!(leaf["chats"][0]["status"], "idle");
    assert!(leaf["chats"][0].get("lastMessageAt").is_some());
    let request = &leaf["openUserActionRequests"][0];
    assert_eq!(request["text"], "Ready to merge");
    assert_eq!(request["detail"], "branch leaf-1");
    assert_eq!(request["originChatId"], OTHER_CHAT);
    assert!(request["id"].as_str().is_some() && request["createdAt"].is_number());
    assert_eq!(leaf["children"][0]["id"], a_deep);
    // The caller's own chat is listed on its own item.
    assert_eq!(tree["chats"][0]["id"], CHAT);

    // maxDepth limits how far down it goes and says what it left out.
    let shallow = tool(
        &rig,
        &token,
        "work_outline_read_subtree",
        json!({ "maxDepth": 1 }),
    )
    .await
    .unwrap();
    assert_eq!(shallow["itemCount"], 2);
    assert_eq!(shallow["tree"]["children"][0]["id"], a_child);
    assert_eq!(shallow["tree"]["children"][0]["children"], json!([]));
    assert_eq!(shallow["tree"]["children"][0]["childrenOmitted"], 1);
    // A descendant as the root of the read; maxDepth is capped at 8.
    let from_leaf = tool(
        &rig,
        &token,
        "work_outline_read_subtree",
        json!({ "workItemId": a_grand, "maxDepth": 99 }),
    )
    .await
    .unwrap();
    assert_eq!(from_leaf["maxDepth"], 8);
    assert_eq!(from_leaf["tree"]["id"], a_grand);
    assert_eq!(from_leaf["itemCount"], 2);
    // includeArchived adds the archived item.
    let with_archived = tool(
        &rig,
        &token,
        "work_outline_read_subtree",
        json!({ "includeArchived": true }),
    )
    .await
    .unwrap();
    assert_eq!(with_archived["itemCount"], 5);
    let archived = with_archived["tree"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == a_archived)
        .unwrap();
    assert_eq!(archived["archived"], true);

    // Outside the subtree (another root, an ancestor-side item, unknown) and
    // unassociated callers are refused.
    for target in [root_b.as_str(), "no-such-item"] {
        let err = tool(
            &rig,
            &token,
            "work_outline_read_subtree",
            json!({ "workItemId": target }),
        )
        .await
        .unwrap_err();
        assert!(err.contains("work-outline/unauthorized"), "{target}: {err}");
    }
    let worker = tokens.mint(OTHER_CHAT, "run-2", SPACE, &rig.core.device_id);
    let err = tool(
        &rig,
        worker.token(),
        "work_outline_read_subtree",
        json!({ "workItemId": root_a }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    // A leaf's chat reads its own subtree only, never upward.
    let err = tool(
        &rig,
        worker.token(),
        "work_outline_read_subtree",
        json!({ "workItemId": a_child }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    let own = tool(&rig, worker.token(), "work_outline_read_subtree", json!({}))
        .await
        .unwrap();
    assert_eq!(own["tree"]["id"], a_grand);
}

#[tokio::test(flavor = "multi_thread")]
async fn read_subtree_caps_its_output_and_says_so() {
    let rig = rig().await;
    let root = create_item(&rig, "op-root", "Big", None).await;
    for i in 0..310 {
        create_item(
            &rig,
            &format!("op-leaf-{i}"),
            &format!("Leaf {i}"),
            Some(&root),
        )
        .await;
    }
    assign_chat(&rig, "assign-caller", CHAT, &root).await;
    let lease = rig
        .core
        .sessions
        .agent_tokens()
        .mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let read = tool(&rig, lease.token(), "work_outline_read_subtree", json!({}))
        .await
        .unwrap();
    assert_eq!(read["truncated"], true);
    assert_eq!(read["itemCount"], 300);
    assert_eq!(read["tree"]["children"].as_array().unwrap().len(), 299);
    assert_eq!(read["tree"]["childrenOmitted"], 11);
}

// ---- bind_chat_worktree -----------------------------------------------------
//
// Real temp git repositories: a superproject with a `flagship` submodule, a
// linked worktree of the SUBMODULE in the superproject's gitignored
// `.worktrees/` folder (the cb4e2158 layout), and one of the root repository.

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

struct BindRig {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
    sub_wt: PathBuf,
    root_wt: PathBuf,
    other: PathBuf,
    core: EngineCore,
    rpc: Arc<zeron_engine::rpc::EngineRpc>,
    targets: DiffTargets,
    token: String,
    /// Keeps the token's Run alive.
    _lease: zeron_engine::agent_tools::AgentTokenLease,
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn bind_rig(attach: bool) -> BindRig {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    init_repo(&root.join("sources/flagship"), "flagship.txt");
    let ws = root.join("ws");
    init_repo(&ws, "README");
    let source = root.join("sources/flagship");
    git(&ws, &["submodule", "add", "-q", &text(&source), "flagship"]);
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
            &text(&root_wt),
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
        HarnessId::Mock,
        None,
    )
    .expect("engine core assembles");
    core.workspace
        .create_space(SPACE, "dev-a", &text(&ws), None, true)
        .unwrap();
    core.workspace
        .create_chat(CHAT, Some(SPACE), Some("dev-a"), None, Some(text(&ws)))
        .unwrap();
    let rpc = core.rpc_service();
    let targets = DiffTargets::new(core.repos.clone(), core.workspace.clone(), "dev-a");
    if attach {
        rpc.agent_tools().set_diff_targets(targets.clone());
    }
    let lease = core
        .sessions
        .agent_tokens()
        .mint(CHAT, "run-1", SPACE, "dev-a");
    BindRig {
        token: lease.token().to_owned(),
        _lease: lease,
        sub_wt: std::fs::canonicalize(&sub_wt).unwrap(),
        root_wt: std::fs::canonicalize(&root_wt).unwrap(),
        _tmp: tmp,
        ws,
        other,
        core,
        rpc,
        targets,
    }
}

fn bind_call(
    rig: &BindRig,
    token: &str,
    arguments: Value,
) -> Result<Value, zeron_engine::agent_tools::AgentToolError> {
    rig.rpc
        .agent_tools()
        .call(&zeron_proto::agent_tools::AgentToolCallParams {
            token: token.to_owned(),
            tool: zeron_proto::agent_tools::tools::BIND_CHAT_WORKTREE.to_owned(),
            arguments,
        })
}

fn bind_ok(rig: &BindRig, path: &Path, key: &str) -> Value {
    bind_call(
        rig,
        &rig.token,
        json!({ "path": text(path), "idempotencyKey": key }),
    )
    .unwrap_or_else(|e| panic!("bind {}: {e}", path.display()))
}

#[tokio::test(flavor = "multi_thread")]
async fn bind_chat_worktree_records_and_selects_a_submodule_worktree() {
    let rig = bind_rig(true);
    let reply = bind_ok(&rig, &rig.sub_wt, "bind-flagship");
    assert_eq!(reply["chatId"], CHAT);
    assert_eq!(reply["path"], text(&rig.sub_wt));
    assert_eq!(reply["branch"], "feat-eng-3012");
    assert_eq!(
        reply["headSha"],
        git(&rig.sub_wt, &["rev-parse", "HEAD"]).as_str()
    );
    assert_eq!(reply["repoName"], "flagship");
    assert_eq!(reply["selected"], true);
    assert_eq!(reply["replayed"], false);
    assert_eq!(
        reply["targets"],
        json!([{ "path": text(&rig.sub_wt), "source": "agent", "selected": true }])
    );
    // The row is persisted, agent-sourced, with the derived operation id.
    let stored = rig.targets.stored(CHAT);
    assert_eq!(stored.targets.len(), 1);
    assert!(stored.targets[0].selected);
    let row = rig.targets.rows().into_iter().next().unwrap();
    assert_eq!(row.chat_id, CHAT);
    assert_eq!(
        row.ops,
        vec![zeron_proto::agent_tools::operation_id(
            CHAT,
            "bind-flagship"
        )]
    );
    assert!(row.ops[0].len() <= 64);
}

#[tokio::test(flavor = "multi_thread")]
async fn bind_chat_worktree_retries_replay_and_new_keys_do_not_duplicate() {
    let rig = bind_rig(true);
    bind_ok(&rig, &rig.sub_wt, "k1");
    let before = *rig.targets.changes().borrow();
    let retry = bind_ok(&rig, &rig.sub_wt, "k1");
    assert_eq!(retry["replayed"], true);
    assert_eq!(retry["targets"].as_array().unwrap().len(), 1);
    assert_eq!(*rig.targets.changes().borrow(), before, "no second write");

    // Same key, different path: the key is already spent.
    let err = bind_call(
        &rig,
        &rig.token,
        json!({ "path": text(&rig.root_wt), "idempotencyKey": "k1" }),
    )
    .unwrap_err();
    assert_eq!(err.code(), "invalid");
    assert_eq!(rig.targets.stored(CHAT).targets.len(), 1);

    // A new key for the same path refreshes the selection, no duplicate.
    let again = bind_ok(&rig, &rig.sub_wt, "k2");
    assert_eq!(again["replayed"], false);
    assert_eq!(again["targets"].as_array().unwrap().len(), 1);
    assert_eq!(rig.targets.stored(CHAT).targets.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn bind_chat_worktree_binds_each_repository_family_and_selects_the_last() {
    let rig = bind_rig(true);
    bind_ok(&rig, &rig.sub_wt, "a");
    let reply = bind_ok(&rig, &rig.root_wt, "b");
    assert_eq!(reply["repoName"], "ws");
    assert_eq!(reply["branch"], "root-feature");
    assert_eq!(
        reply["targets"],
        json!([
            { "path": text(&rig.sub_wt), "source": "agent", "selected": false },
            { "path": text(&rig.root_wt), "source": "agent", "selected": true },
        ])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn bind_chat_worktree_refuses_paths_outside_the_chats_repositories() {
    let rig = bind_rig(true);
    for (key, path) in [
        ("outside", text(rig.ws.parent().unwrap())),
        ("other", text(&rig.other)),
        ("relative", "flagship".to_owned()),
        ("missing", text(&rig.ws.join("nope"))),
    ] {
        let err = bind_call(
            &rig,
            &rig.token,
            json!({ "path": path, "idempotencyKey": key }),
        )
        .unwrap_err();
        assert_eq!(err.code(), "invalid", "{key}: {err}");
    }
    let subdir = rig.sub_wt.join("src");
    std::fs::create_dir_all(&subdir).unwrap();
    let err = bind_call(
        &rig,
        &rig.token,
        json!({ "path": text(&subdir), "idempotencyKey": "subdir" }),
    )
    .unwrap_err();
    assert_eq!(err.code(), "invalid", "{err}");
    // A chatId argument is ignored: the token's chat is the only one bound.
    core_chat_other(&rig);
    let reply = bind_call(
        &rig,
        &rig.token,
        json!({ "path": text(&rig.sub_wt), "idempotencyKey": "forge", "chatId": OTHER_CHAT }),
    )
    .unwrap();
    assert_eq!(reply["chatId"], CHAT);
    assert!(rig.targets.stored(OTHER_CHAT).targets.is_empty());
    assert_eq!(rig.targets.stored(CHAT).targets.len(), 1);
}

fn core_chat_other(rig: &BindRig) {
    rig.core
        .workspace
        .create_chat(
            OTHER_CHAT,
            Some(SPACE),
            Some("dev-a"),
            None,
            Some(text(&rig.ws)),
        )
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn bind_chat_worktree_validates_arguments_and_authentication() {
    let rig = bind_rig(true);
    let path = text(&rig.sub_wt);
    for (arguments, what) in [
        (json!({ "idempotencyKey": "k" }), "missing path"),
        (json!({ "path": "", "idempotencyKey": "k" }), "empty path"),
        (json!({ "path": 7, "idempotencyKey": "k" }), "bad path type"),
        (json!(null), "no arguments"),
    ] {
        let err = bind_call(&rig, &rig.token, arguments).unwrap_err();
        assert_eq!(err.code(), "invalid", "{what}: {err}");
    }
    assert!(rig.targets.stored(CHAT).targets.is_empty());

    // A key is optional for a bind (blank counts as omitted).
    for key in [None, Some(""), Some("  ")] {
        let mut arguments = json!({ "path": path });
        if let Some(key) = key {
            arguments["idempotencyKey"] = json!(key);
        }
        bind_call(&rig, &rig.token, arguments)
            .unwrap_or_else(|e| panic!("bind with key {key:?}: {e}"));
    }

    let good = json!({ "path": path, "idempotencyKey": "k" });
    for token in ["", "zat_forged", &format!("{}0", rig.token)] {
        let err = bind_call(&rig, token, good.clone()).unwrap_err();
        assert_eq!(err.code(), "unauthenticated", "{token:?}");
    }
    assert_eq!(rig.targets.stored(CHAT).targets.len(), 1);
}

/// Replay of today's failure (Mikey's ENG-1655 review chat and the
/// pr-page-parity child): `bind_chat_worktree` without an `idempotencyKey`
/// binds, and rebinding a path later re-selects it even after another bind.
#[tokio::test(flavor = "multi_thread")]
async fn bind_chat_worktree_without_a_key_binds_and_rebinding_reselects() {
    let rig = bind_rig(true);
    let call = |path: &Path| {
        bind_call(&rig, &rig.token, json!({ "path": text(path) })).unwrap_or_else(|e| panic!("{e}"))
    };
    let selected = || -> Option<String> {
        rig.targets
            .stored(CHAT)
            .targets
            .into_iter()
            .find(|t| t.selected)
            .map(|t| t.path)
    };
    let first = call(&rig.sub_wt);
    assert_eq!(first["selected"], true);
    assert_eq!(first["replayed"], false);
    // The same call again is harmless: still one target.
    let again = call(&rig.sub_wt);
    assert_eq!(again["targets"].as_array().unwrap().len(), 1);
    assert_eq!(again["replayed"], false);
    // Bind another path, then the first again: the first is selected, not
    // replayed as a stale no-op.
    call(&rig.root_wt);
    assert_eq!(selected().as_deref(), Some(text(&rig.root_wt).as_str()));
    call(&rig.sub_wt);
    assert_eq!(selected().as_deref(), Some(text(&rig.sub_wt).as_str()));
    assert_eq!(rig.targets.stored(CHAT).targets.len(), 2);
    // No replay ledger entries were written for the keyless calls.
    assert!(rig.targets.rows().into_iter().all(|row| row.ops.is_empty()));

    // `snake_case` keys are accepted too.
    let reply = bind_call(
        &rig,
        &rig.token,
        json!({ "path": text(&rig.sub_wt), "idempotency_key": "snake" }),
    )
    .unwrap();
    assert_eq!(reply["replayed"], false);
    let row = rig.targets.rows().into_iter().next().unwrap();
    assert_eq!(
        row.ops,
        vec![zeron_proto::agent_tools::operation_id(CHAT, "snake")]
    );

    // A missing path lists the parameters and an example.
    let err = bind_call(&rig, &rig.token, json!({}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("path is required"), "{err}");
    assert!(err.contains("Usage of bind_chat_worktree"), "{err}");
    assert!(err.contains("example:"), "{err}");
}

/// Engine assembly attaches the shared `DiffTargets` itself: a bind works
/// without any test-side `set_diff_targets`.
#[tokio::test(flavor = "multi_thread")]
async fn bind_chat_worktree_is_wired_on_the_assembled_engine() {
    let rig = bind_rig(false);
    let reply = bind_ok(&rig, &rig.sub_wt, "k");
    assert_eq!(reply["selected"], true);
    assert_eq!(reply["path"], text(&rig.sub_wt));
}

#[tokio::test(flavor = "multi_thread")]
async fn bind_chat_worktree_refuses_a_chat_hosted_on_another_device() {
    let rig = bind_rig(true);
    // A Space on another device: its chats are hosted there.
    rig.core
        .workspace
        .create_space("space-b", "dev-b", &text(&rig.ws), None, true)
        .unwrap();
    rig.core
        .workspace
        .create_chat(
            "chat-remote",
            Some("space-b"),
            Some("dev-b"),
            None,
            Some(text(&rig.ws)),
        )
        .unwrap();
    let lease = rig
        .core
        .sessions
        .agent_tokens()
        .mint("chat-remote", "run-2", "space-b", "dev-b");
    let err = bind_call(
        &rig,
        lease.token(),
        json!({ "path": text(&rig.sub_wt), "idempotencyKey": "k" }),
    )
    .unwrap_err();
    assert_eq!(err.code(), "forbidden", "{err}");
}

/// What a coordinator's resolve of its child's request must leave behind.
fn request_is_resolved(snapshot: &WorkOutlineSnapshot, id: &str) -> bool {
    snapshot
        .user_action_requests
        .iter()
        .find(|r| r.id == id)
        .is_some_and(|r| r.state == zeron_proto::UserActionRequestState::Resolved)
}

fn open_request_ids(read: &Value) -> Vec<String> {
    fn walk(node: &Value, out: &mut Vec<String>) {
        for request in node["openUserActionRequests"]
            .as_array()
            .into_iter()
            .flatten()
        {
            out.push(request["id"].as_str().unwrap_or_default().to_owned());
        }
        for child in node["children"].as_array().into_iter().flatten() {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(&read["tree"], &mut out);
    out
}

/// A child's request is raised with a `dedupeKey` (the tool guidance tells
/// agents to pass one per topic). The coordinator's resolve used to select the
/// dedupe group by the CALLER's Chat id, so the child's request fell out of
/// its own group, nothing was written, and the reply still said `resolved`.
#[tokio::test(flavor = "multi_thread")]
async fn a_coordinator_resolving_its_childs_deduped_request_persists_across_reads_and_restart() {
    let rig = rig().await;
    let root = create_item(&rig, "op-root", "Coordinator", None).await;
    let leaf = create_item(&rig, "op-leaf", "Leaf", Some(&root)).await;
    assign_chat(&rig, "assign-caller", CHAT, &root).await;
    let tokens = rig.core.sessions.agent_tokens();
    let coordinator = tokens.mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = coordinator.token().to_owned();

    let launched = tool(
        &rig,
        &token,
        "chat_launch_child_for_item",
        launch_args("leaf-worker", &leaf, "workspace-write"),
    )
    .await
    .unwrap();
    let child = launched["chatId"].as_str().unwrap().to_owned();
    let child_lease = tokens.mint(&child, "run-2", SPACE, &rig.core.device_id);
    let raised = tool(
        &rig,
        child_lease.token(),
        "work_outline_create_user_action_request",
        json!({ "request": "Ready to merge: leaf", "dedupeKey": "ready-leaf",
                "idempotencyKey": "ready" }),
    )
    .await
    .unwrap();
    let request_id = raised["userActionRequestId"].as_str().unwrap().to_owned();
    let open = |read: &Value| open_request_ids(read).contains(&request_id);
    let read = tool(&rig, &token, "work_outline_read_subtree", json!({}))
        .await
        .unwrap();
    assert!(open(&read), "the child's request starts open: {read}");

    // An unrelated Chat is refused loudly and the request stays open.
    let stranger = tokens.mint(OTHER_CHAT, "run-3", SPACE, &rig.core.device_id);
    let err = tool(
        &rig,
        stranger.token(),
        "work_outline_resolve_user_action_request",
        json!({ "userActionRequestId": request_id, "idempotencyKey": "close-other" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("work-outline/unauthorized"), "{err}");
    assert!(!request_is_resolved(&snapshot(&rig), &request_id));

    let reply = tool(
        &rig,
        &token,
        "work_outline_resolve_user_action_request",
        json!({ "userActionRequestId": request_id, "resolution": "Merged",
                "idempotencyKey": "close-parent" }),
    )
    .await
    .unwrap();
    assert_eq!(reply["resolved"], true);

    // The reply is backed by the row, by the subtree read, and by the row's
    // own fields.
    assert!(request_is_resolved(&snapshot(&rig), &request_id));
    let read = tool(&rig, &token, "work_outline_read_subtree", json!({}))
        .await
        .unwrap();
    assert!(!open(&read), "still open after the resolve: {read}");
    let row = rig
        .core
        .workspace
        .registry_row("userActionRequests", &request_id)
        .expect("the request row exists");
    assert_eq!(row.fields.get("state"), Some(&json!("resolved")), "{row:?}");
    assert_eq!(rig.core.workspace.registry_pending_len(), 0);

    // Re-assembling the engine from the same profile keeps it resolved.
    rig.harness.release.notify_waiters();
    let Rig {
        _tmp, core, client, ..
    } = rig;
    drop(client);
    // A graceful restart flushes the debounced registry snapshot first.
    core.workspace.shutdown();
    drop(core);
    let root_dir = std::fs::canonicalize(_tmp.path()).unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(GatedHarness::default()));
    let again = EngineCore::assemble(
        &root_dir.join("data"),
        Arc::new(registry),
        HarnessId::Mock,
        None,
    )
    .expect("engine core re-assembles from the same profile");
    let inputs = again.workspace.work_outline_inputs(SPACE);
    let after = zeron_doc::materialize_work_outline(SPACE, &inputs);
    assert!(
        request_is_resolved(&after, &request_id),
        "the resolution did not survive a restart"
    );
}

// ---- forgiving calls ----------------------------------------------------------

/// Replays of the calls agents got wrong on their first try (Claude Code loads
/// MCP tools lazily, so they guess parameters): missing keys on naturally
/// idempotent tools and guessed field names now succeed; the create-style
/// tools name the missing field and show an example.
#[tokio::test(flavor = "multi_thread")]
async fn first_guess_calls_succeed_or_say_exactly_what_to_pass() {
    let rig = rig().await;
    let root = create_item(&rig, "op-root", "Feature", None).await;
    let lease = rig
        .core
        .sessions
        .agent_tokens()
        .mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let token = lease.token().to_owned();
    let call = |name: &'static str, arguments: Value| {
        let (rig, token) = (&rig, token.clone());
        async move { tool(rig, &token, name, arguments).await }
    };

    // associate_current_chat: no key, `workItem` for `workItemId`.
    call(
        "work_outline_associate_current_chat",
        json!({ "workItem": root }),
    )
    .await
    .unwrap();
    assert_eq!(
        snapshot(&rig)
            .assignments
            .iter()
            .find(|a| a.chat_id == CHAT)
            .and_then(|a| a.work_item_id.clone())
            .as_deref(),
        Some(root.as_str())
    );
    // The same call again is fine.
    call(
        "work_outline_associate_current_chat",
        json!({ "itemId": root }),
    )
    .await
    .unwrap();

    // work_outline_set_status: no key; completing, reopening and completing
    // again each take effect (a derived key must not replay a stale result).
    let status = |rig: &Rig| snapshot(rig).item(&root).unwrap().status;
    for (wanted, expected) in [
        ("completed", zeron_proto::WorkItemStatus::Completed),
        ("active", zeron_proto::WorkItemStatus::Active),
        ("completed", zeron_proto::WorkItemStatus::Completed),
        ("completed", zeron_proto::WorkItemStatus::Completed),
    ] {
        // Updates are stamped with the clock; keep the stamps apart.
        tokio::time::sleep(Duration::from_millis(3)).await;
        call("work_outline_set_status", json!({ "status": wanted }))
            .await
            .unwrap();
        assert_eq!(status(&rig), expected, "after {wanted}");
    }
    // Also by an explicit item and a snake_case key.
    call(
        "work_outline_set_status",
        json!({ "status": "active", "work_item_id": root, "idempotency_key": "reopen" }),
    )
    .await
    .unwrap();
    assert_eq!(status(&rig), zeron_proto::WorkItemStatus::Active);

    // create_user_action_request: `text` for `request`, a dedupeKey as the key.
    let created = call(
        "work_outline_create_user_action_request",
        json!({ "text": "Which branch?", "detail": "main or release", "dedupeKey": "branch" }),
    )
    .await
    .unwrap();
    let request_id = created["userActionRequestId"].as_str().unwrap().to_owned();
    // Repeating the call returns the same open request, not a duplicate.
    let repeated = call(
        "work_outline_create_user_action_request",
        json!({ "title": "Which branch?", "detail": "main or release", "dedupeKey": "branch" }),
    )
    .await
    .unwrap();
    assert_eq!(repeated["userActionRequestId"], request_id.as_str());
    assert_eq!(
        snapshot(&rig)
            .user_action_requests
            .iter()
            .filter(|r| r.is_open() && !r.deduped)
            .count(),
        1
    );
    // `question` works as well, with an explicit key.
    let other = call(
        "work_outline_create_user_action_request",
        json!({ "question": "Ship it?", "idempotencyKey": "ship" }),
    )
    .await
    .unwrap();
    assert_ne!(other["userActionRequestId"], request_id.as_str());

    // resolve: `id` for `userActionRequestId`, no key; a retry is fine.
    for _ in 0..2 {
        call(
            "work_outline_resolve_user_action_request",
            json!({ "id": request_id, "resolution": "main" }),
        )
        .await
        .unwrap();
    }
    assert!(request_is_resolved(&snapshot(&rig), &request_id));
    call(
        "work_outline_resolve_user_action_request",
        json!({ "requestId": other["userActionRequestId"] }),
    )
    .await
    .unwrap();

    // The same dedupeKey after a resolve is a new request, not a replay.
    let reasked = call(
        "work_outline_create_user_action_request",
        json!({ "request": "Which branch?", "detail": "main or release", "dedupeKey": "branch" }),
    )
    .await
    .unwrap();
    assert_ne!(reasked["userActionRequestId"], request_id.as_str());
    assert!(!request_is_resolved(
        &snapshot(&rig),
        reasked["userActionRequestId"].as_str().unwrap()
    ));

    // Create-style tools keep their key but say exactly what is missing.
    let err = call(
        "work_outline_create_user_action_request",
        json!({ "request": "No key, no dedupeKey" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("idempotencyKey is required"), "{err}");
    assert!(err.contains("dedupeKey"), "{err}");
    assert!(
        err.contains("Usage of work_outline_create_user_action_request"),
        "{err}"
    );
    assert!(err.contains("example:"), "{err}");
    let err = call("work_outline_create_user_action_request", json!({}))
        .await
        .unwrap_err();
    assert!(err.contains("missing field `request`"), "{err}");
    assert!(err.contains("required: request"), "{err}");
    assert!(err.contains("optional: detail"), "{err}");
    assert!(err.contains("example: {\"request\""), "{err}");
    let err = call("work_outline_create_child", json!({ "title": "x" }))
        .await
        .unwrap_err();
    assert!(err.contains("idempotencyKey is required"), "{err}");
    assert!(err.contains("Usage of work_outline_create_child"), "{err}");
    let err = call(
        "work_outline_propose_instruction",
        json!({ "idempotencyKey": "k" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("missing field `text`"), "{err}");
    assert!(
        err.contains("Usage of work_outline_propose_instruction"),
        "{err}"
    );
    let err = call("work_outline_resolve_user_action_request", json!({}))
        .await
        .unwrap_err();
    assert!(err.contains("missing field `userActionRequestId`"), "{err}");
    assert!(err.contains("example:"), "{err}");
    let err = call("work_outline_set_status", json!({ "status": "done" }))
        .await
        .unwrap_err();
    assert!(err.contains("status must be active or completed"), "{err}");
    assert!(err.contains("Usage of work_outline_set_status"), "{err}");

    // `parentId` stands for `parentWorkItemId` on create_child; the key stays
    // required, and `idempotency_key` is accepted for it.
    let child = call(
        "work_outline_create_child",
        json!({ "title": "Child", "parentId": root, "idempotency_key": "child-1" }),
    )
    .await
    .unwrap();
    assert_eq!(child["parentWorkItemId"], root.as_str());
    // A retry with the snake_case key replays the same child.
    let retry = call(
        "work_outline_create_child",
        json!({ "title": "Child", "parent": root, "idempotencyKey": "child-1" }),
    )
    .await
    .unwrap();
    assert_eq!(retry["workItemId"], child["workItemId"]);
}
