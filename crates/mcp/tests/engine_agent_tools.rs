//! The MCP tools against a real offline engine, authenticated by an
//! engine-minted agent token (Work Outline Milestone 4):
//! - `create_chat` from an agent in a Space creates ONE child Chat per
//!   idempotency key (retries return it, with one queued first Turn), and
//!   the child inherits the parent's work item (acceptance 19);
//! - the Work Outline tools reach the engine's agent surface through MCP;
//! - once the token is revoked, every tool stops working.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::{Harness, HarnessError, RunControls};
use zeron_mcp::{Tools, Zeron};
use zeron_proto::{AgentEvent, DoneStatus, Model, ReasoningLevel, RunRequest, SteeringMode};
use zeron_proto::{HarnessId, WorkItemChatAssignmentState, derive_id, roles};
use zeron_rpc::methods;

const SPACE: &str = "space-1";
const PARENT: &str = "chat-parent";

/// Stands in for Claude Code (so the MCP tools see an available harness)
/// and completes every Run at once without spawning anything.
struct FakeClaude;

#[async_trait]
impl Harness for FakeClaude {
    fn id(&self) -> HarnessId {
        HarnessId::ClaudeCode
    }
    fn display_name(&self) -> &str {
        "Fake Claude"
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
    async fn run(
        &self,
        request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        Ok(futures::stream::iter(vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::ClaudeCode,
                model: "fake".into(),
                tools: vec![],
                cwd: request.cwd,
                session_id: "sess-fake".into(),
                assistant_message_id: "a-1".into(),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("sess-fake".into()),
            }),
        ])
        .boxed())
    }
}

async fn wait_for<F: FnMut() -> bool>(mut predicate: F, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_created_child_chats_are_idempotent_and_inherit_the_work_item() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(FakeClaude));
    let core = EngineCore::assemble(
        &root.join("data"),
        Arc::new(registry),
        HarnessId::ClaudeCode,
        None,
    )
    .expect("engine core assembles");
    let admin = zeron_rpc::memory_client(core.rpc_service());
    admin
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": SPACE, "deviceId": core.device_id,
                    "path": project.to_string_lossy() }),
        )
        .await
        .unwrap();
    admin
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": PARENT, "spaceId": SPACE,
                    "deviceId": core.device_id,
                    "config": { "harness": "claude-code", "model": null, "reasoning": null,
                                "sandbox": "workspace-write" } }),
        )
        .await
        .unwrap();
    admin
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createWorkItem", "spaceId": SPACE, "title": "Feature",
                    "operationId": "op-item" }),
        )
        .await
        .unwrap();
    let item = derive_id("op-item", roles::ITEM);
    admin
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "assignChatToWorkItem", "chatId": PARENT, "workItemId": item,
                    "operationId": "op-assign" }),
        )
        .await
        .unwrap();

    let lease = core
        .sessions
        .agent_tokens()
        .mint(PARENT, "run-1", SPACE, &core.device_id);
    let tools = Tools::new(Arc::new(Zeron::with_client(
        zeron_rpc::memory_client(core.rpc_service()),
        Some(lease.token().to_owned()),
    )));

    // The Work Outline tools are listed and reach the engine.
    assert!(tools.has("work_outline_read_current"));
    let current = tools
        .call("work_outline_read_current", json!({}))
        .await
        .unwrap();
    assert_eq!(current["workItem"]["id"], item);

    let args = json!({ "project": SPACE, "harness": "claude-code", "title": "Reviewer",
                       "prompt": "review the diff", "idempotency_key": "spawn-reviewer" });
    let first = tools.call("create_chat", args.clone()).await.unwrap();
    let child = first["chatId"].as_str().unwrap().to_owned();
    assert_eq!(first["parentChatId"], PARENT);
    let second = tools.call("create_chat", args.clone()).await.unwrap();
    assert_eq!(second["chatId"], child);
    assert_eq!(second["replayed"], true);
    // The same key with other inputs: a conflict, and still one child.
    let mut changed = args.clone();
    changed["prompt"] = json!("rewrite everything");
    let err = tools.call("create_chat", changed).await.unwrap_err();
    assert!(err.contains("work-outline/operationConflict"), "{err}");
    // A wider sandbox than the parent's (workspace-write) is refused.
    let err = tools
        .call(
            "create_chat",
            json!({ "project": SPACE, "sandbox": "danger-full-access",
                    "idempotency_key": "wide" }),
        )
        .await
        .unwrap_err();
    assert!(err.contains("more permissive"), "{err}");

    // One child Chat, parented to the caller, holding exactly one first
    // Turn (the key-derived command id), whatever the retries.
    let chats = core.workspace.read_chats().unwrap();
    let children: Vec<_> = chats
        .iter()
        .filter(|c| c.parent_chat_id.as_deref() == Some(PARENT))
        .collect();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, child);
    let operation = zeron_proto::agent_tools::operation_id(PARENT, "spawn-reviewer");
    let command_id = derive_id(&operation, roles::COMMAND);
    let commands = core
        .doc_host
        .open(&child)
        .unwrap()
        .doc()
        .read_commands()
        .unwrap();
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert_eq!(commands[0].id, command_id);

    // Inheritance (acceptance 19): the child joins the parent's work item.
    wait_for(
        || {
            let inputs = core.workspace.work_outline_inputs(SPACE);
            let snapshot = zeron_doc::materialize_work_outline(SPACE, &inputs);
            snapshot.assignments.iter().any(|a| {
                a.chat_id == child
                    && a.work_item_id.as_deref() == Some(item.as_str())
                    && a.state == WorkItemChatAssignmentState::Associated
            })
        },
        "child assignment",
    )
    .await;

    // The Run ends: the token dies and every tool with it.
    drop(lease);
    for (name, arguments) in [
        ("list_chats", json!({})),
        ("work_outline_read_current", json!({})),
        (
            "create_chat",
            json!({ "project": SPACE, "idempotency_key": "late" }),
        ),
    ] {
        let err = tools.call(name, arguments).await.unwrap_err();
        assert!(err.contains("unauthenticated"), "{name}: {err}");
    }
    let _: Value = current;
}
