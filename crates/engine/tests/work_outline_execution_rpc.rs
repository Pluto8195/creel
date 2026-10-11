//! Work Outline execution configuration (Milestone 3 backend) against a real
//! (offline) engine over the in-memory RPC transport: `configureExecution`
//! replay, `resolveExecutionConfiguration`, `DescribeExecutionRepository`,
//! `ValidateExecutionConfiguration` by scope and by captured binding over a
//! real superproject with two member worktrees, the dispatch helper, the
//! snapshot exception marker, remote-owner routing without the capability,
//! per-member diff entries, and Space deletion.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use zeron_engine::execution_scope::ExecutionScopeService;
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_proto::{
    ExecutionHostCapabilities, ExecutionRepositoryIdentity, ExecutionScope,
    ResolvedExecutionConfiguration, ValidateExecutionConfigurationReply, WorkOutlineActor,
    WorkOutlineActorKind, WorkOutlineError, WorkOutlineErrorCode, WorkOutlineQueryResponse,
    WorkOutlineSnapshot, derive_id, roles,
};
use zeron_rpc::methods;

fn assemble(dir: &Path) -> EngineCore {
    std::fs::create_dir_all(dir).expect("create data dir");
    std::fs::write(dir.join("device-id"), "dev-a").expect("write device id");
    EngineCore::assemble(
        dir,
        Arc::new(HarnessRegistry::new()),
        zeron_proto::HarnessId::Mock,
        None,
    )
    .expect("engine core assembles")
}

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

/// `ws` with submodules `ui` and `flagship` and a worktree per member at
/// `ws/.worktrees/<member>/t`.
fn superproject(root: &Path) -> PathBuf {
    for member in ["ui", "flagship"] {
        init_repo(&root.join("sources").join(member), &format!("{member}.txt"));
    }
    let ws = root.join("ws");
    init_repo(&ws, "README");
    for member in ["ui", "flagship"] {
        let source = root.join("sources").join(member);
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

fn outline_error(text: String) -> WorkOutlineError {
    text.find("work-outline/")
        .and_then(|at| WorkOutlineError::from_wire_message(&text[at..]))
        .unwrap_or_else(|| panic!("not a work outline error: {text}"))
}

async fn mutate(client: &zeron_rpc::RpcClient, params: Value) -> Result<Value, WorkOutlineError> {
    client
        .call(methods::WORK_OUTLINE_MUTATE, params)
        .await
        .map_err(|e| outline_error(e.to_string()))
}

async fn validate(
    client: &zeron_rpc::RpcClient,
    params: Value,
) -> ValidateExecutionConfigurationReply {
    let value: Value = client
        .call(methods::VALIDATE_EXECUTION_CONFIGURATION, params)
        .await
        .expect("validation reply");
    serde_json::from_value(value).expect("reply decodes")
}

async fn describe(client: &zeron_rpc::RpcClient, path: &Path) -> ExecutionRepositoryIdentity {
    let value: Value = client
        .call(
            methods::DESCRIBE_EXECUTION_REPOSITORY,
            json!({ "path": path.to_string_lossy() }),
        )
        .await
        .expect("describe");
    serde_json::from_value(value).unwrap()
}

async fn binding_json(
    client: &zeron_rpc::RpcClient,
    repo: &Path,
    worktree: &Path,
    primary: bool,
) -> Value {
    let repo_identity = describe(client, repo).await;
    let at = describe(client, worktree).await;
    json!({
        "repoId": repo_identity.repo_id,
        "repoFingerprint": repo_identity.repo_fingerprint,
        "repoPath": repo_identity.repo_path,
        "member": true,
        "primary": primary,
        "worktreeId": at.checkout_id,
        "worktreePath": worktree.to_string_lossy(),
    })
}

async fn frame_until(
    stream: &mut tokio::sync::mpsc::Receiver<Value>,
    mut predicate: impl FnMut(&WorkOutlineSnapshot) -> bool,
) -> WorkOutlineSnapshot {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let value = tokio::time::timeout_at(deadline, stream.recv())
            .await
            .expect("frame before timeout")
            .expect("stream alive");
        let snapshot: WorkOutlineSnapshot = serde_json::from_value(value).expect("snapshot");
        if predicate(&snapshot) {
            return snapshot;
        }
    }
}

fn actor() -> WorkOutlineActor {
    WorkOutlineActor {
        kind: WorkOutlineActorKind::User,
        device_id: "dev-a".into(),
        chat_id: None,
        message_id: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn configure_resolve_validate_capture_and_dispatch_over_rpc() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let ws = superproject(&root);
    let core = assemble(&root.join("data"));
    let client = zeron_rpc::memory_client(core.rpc_service());
    client
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": "space-1", "deviceId": "dev-a",
                    "path": ws.to_string_lossy() }),
        )
        .await
        .unwrap();
    client
        .call(
            methods::MUTATE,
            json!({ "op": "createChat", "chatId": "chat-1", "deviceId": "dev-a",
                    "spaceId": "space-1", "cwd": ws.to_string_lossy() }),
        )
        .await
        .unwrap();
    let item = derive_id("op-item", roles::ITEM);
    mutate(
        &client,
        json!({ "op": "createWorkItem", "spaceId": "space-1", "title": "Multi-repo",
                "operationId": "op-item" }),
    )
    .await
    .unwrap();
    mutate(
        &client,
        json!({ "op": "assignChatToWorkItem", "chatId": "chat-1", "workItemId": item,
                "operationId": "op-assign" }),
    )
    .await
    .unwrap();
    let mut stream = client
        .subscribe(methods::WATCH_WORK_OUTLINE, json!({ "spaceId": "space-1" }))
        .await
        .unwrap();

    // Configure the item: root = superproject, ui primary, flagship member,
    // each pinned to its exact worktree.
    let ui_wt = ws.join(".worktrees/ui/t");
    let flagship_wt = ws.join(".worktrees/flagship/t");
    let configuration = json!({
        "executionRoot": ws.to_string_lossy(),
        "bindings": [
            binding_json(&client, &ws.join("ui"), &ui_wt, true).await,
            binding_json(&client, &ws.join("flagship"), &flagship_wt, false).await,
        ],
        "schema": 1,
    });
    let configure = json!({
        "op": "configureExecution",
        "scope": { "type": "workItem", "workItemId": item },
        "configuration": configuration,
        "operationId": "op-configure",
    });
    assert_eq!(mutate(&client, configure.clone()).await.unwrap(), json!({}));
    // Replay: same id and inputs answer the same reply without writing.
    let pending = core.workspace.registry_pending_len();
    assert_eq!(mutate(&client, configure.clone()).await.unwrap(), json!({}));
    assert_eq!(core.workspace.registry_pending_len(), pending);
    // Same id, other inputs: conflict.
    let mut other = configure.clone();
    other["configuration"]["executionRoot"] = json!("/elsewhere");
    assert_eq!(
        mutate(&client, other).await.unwrap_err().code,
        WorkOutlineErrorCode::OperationConflict
    );

    // Resolve over the query surface.
    let value: Value = client
        .call(
            methods::WORK_OUTLINE_QUERY,
            json!({ "query": "resolveExecutionConfiguration",
                    "scope": { "type": "workItem", "workItemId": item } }),
        )
        .await
        .unwrap();
    let WorkOutlineQueryResponse::ResolveExecutionConfiguration(resolved) =
        serde_json::from_value(value).unwrap()
    else {
        panic!("resolve response");
    };
    assert_eq!(resolved.device_id, "dev-a");
    assert_eq!(resolved.configuration.bindings.len(), 2);

    // Validate by scope on the owning (this) device.
    let reply = validate(
        &client,
        json!({ "scope": { "type": "workItem", "workItemId": item } }),
    )
    .await;
    assert!(reply.ok, "{:?}", reply.errors);
    let validated = reply.validated.unwrap();
    assert_eq!(validated.repositories.len(), 2);
    assert_eq!(
        validated.repositories[0].checkout_path,
        ui_wt.to_string_lossy()
    );

    // Capture the binding the way the start workflow will; replay is a no-op.
    assert!(
        core.workspace
            .capture_chat_execution_binding("chat-1", &resolved, "op-launch", &actor())
            .unwrap()
    );
    assert!(
        !core
            .workspace
            .capture_chat_execution_binding("chat-1", &resolved, "op-launch", &actor())
            .unwrap()
    );

    // Per-member diff entries: one checkout entry per member worktree, each
    // reachable through the scoped watch.
    core.diff_sync.reconcile_now().await;
    let wanted: Vec<String> = validated
        .repositories
        .iter()
        .map(|r| r.checkout_id.clone())
        .collect();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let diffs = core.diff_sync.watch_diffs().borrow().clone();
        if wanted
            .iter()
            .all(|id| diffs.iter().any(|d| &d.checkout_id == id))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "member diff entries never appeared"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let member = &validated.repositories[1];
    let mut scoped = client
        .subscribe(
            methods::WATCH_CHECKOUT_DIFFS,
            json!({ "cwd": member.checkout_path, "checkoutId": member.checkout_id }),
        )
        .await
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(10), scoped.recv())
        .await
        .expect("scoped diff frame")
        .expect("stream alive");
    assert!(first.to_string().contains(&member.checkout_id));

    // The item changes after launch: the Chat keeps its binding and shows as
    // an exception on the item.
    let mut changed = configure.clone();
    changed["operationId"] = json!("op-configure-2");
    changed["configuration"]["bindings"][1]["primary"] = json!(false);
    changed["configuration"]["bindings"][1]["member"] = json!(false);
    mutate(&client, changed).await.unwrap();
    let snapshot = frame_until(&mut stream, |s| !s.execution_exceptions.is_empty()).await;
    assert_eq!(
        snapshot.execution_exception_chats(&item),
        ["chat-1".to_owned()]
    );
    assert!(snapshot.chat_execution_binding("chat-1").unwrap().exception);

    // Validation by captured binding, and the dispatch helper.
    let reply = validate(&client, json!({ "chatId": "chat-1" })).await;
    assert!(reply.ok, "{:?}", reply.errors);
    let service = ExecutionScopeService::new(core.workspace.clone(), core.repos.clone());
    let caps = ExecutionHostCapabilities::default();
    assert!(
        service
            .validate_for_dispatch("chat-1", caps)
            .await
            .unwrap()
            .is_some()
    );
    // An unbound Chat keeps today's behavior.
    assert!(
        service
            .validate_for_dispatch("no-binding", caps)
            .await
            .unwrap()
            .is_none()
    );

    // The flagship worktree disappears: validation blocks with an actionable
    // error and nothing else is chosen.
    std::fs::remove_dir_all(&flagship_wt).unwrap();
    let reply = validate(&client, json!({ "chatId": "chat-1" })).await;
    assert!(!reply.ok && reply.validated.is_none());
    assert_eq!(reply.errors.len(), 1);
    assert_eq!(reply.errors[0].error.code(), "worktreePruned");
    assert!(!reply.errors[0].remediation.is_empty());
    let errors = service
        .validate_for_dispatch("chat-1", caps)
        .await
        .unwrap_err();
    let message = zeron_engine::execution_scope::rejection_message(&errors);
    assert!(message.starts_with("execution-scope:worktreePruned: "));
    // The Chat row was never retargeted.
    let chat = core.workspace.chat("chat-1").unwrap().unwrap();
    assert_eq!(chat.cwd.as_deref(), Some(ws.to_string_lossy().as_ref()));

    // Exactly one target.
    let err = client
        .call(
            methods::VALIDATE_EXECUTION_CONFIGURATION,
            json!({ "chatId": "chat-1", "scope": { "type": "space", "spaceId": "space-1" } }),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("exactly one"));

    // Space deletion tombstones the execution rows with the Space.
    client
        .call(
            methods::MUTATE,
            json!({ "op": "deleteSpace", "spaceId": "space-1" }),
        )
        .await
        .unwrap();
    assert!(core.workspace.chat_execution_binding("chat-1").is_none());
    assert_eq!(
        core.workspace
            .resolve_execution(&ExecutionScope::WorkItem {
                work_item_id: item.clone()
            })
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::NotFound
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_owner_without_reachability_is_reported_not_validated_locally() {
    let tmp = tempfile::tempdir().unwrap();
    let core = assemble(&tmp.path().join("data"));
    let client = zeron_rpc::memory_client(core.rpc_service());
    client
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": "remote", "deviceId": "dev-b",
                    "path": "/remote/ws" }),
        )
        .await
        .unwrap();
    // Configuring a remote Space is shape-only: it never touches paths.
    mutate(
        &client,
        json!({
            "op": "configureExecution",
            "scope": { "type": "space", "spaceId": "remote" },
            "configuration": { "executionRoot": "/remote/ws", "bindings": [], "schema": 1 },
            "operationId": "op-remote",
        }),
    )
    .await
    .unwrap();
    let reply = validate(
        &client,
        json!({ "scope": { "type": "space", "spaceId": "remote" } }),
    )
    .await;
    assert!(!reply.ok);
    let code = reply.errors[0].error.code();
    assert!(
        code == "deviceOffline" || code == "hostUnsupported",
        "unexpected {code}"
    );
    // A snapshot carried for another device is never validated here either.
    let resolved: ResolvedExecutionConfiguration = reply.resolved.unwrap();
    assert_eq!(resolved.device_id, "dev-b");
    let service = ExecutionScopeService::new(core.workspace.clone(), core.repos.clone());
    let errors = service
        .validate_resolved(&resolved, ExecutionHostCapabilities::default())
        .await
        .unwrap_err();
    assert_eq!(errors[0].code(), "deviceMismatch");
}

#[test]
fn engine_advertises_the_multi_repo_capability() {
    assert!(
        zeron_proto::capabilities::current()
            .iter()
            .any(|c| c == zeron_proto::capabilities::WORK_OUTLINE_MULTI_REPO_V1)
    );
}
