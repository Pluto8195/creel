//! The linked Linear item against a real offline engine and a local mock of
//! Linear's GraphQL endpoint (never the real API or Keychain): matching
//! (assigned, state types, project exclusion), create, dedupe against existing
//! links, the stop-matching paths (touched vs. untouched), Spaces without the
//! feature or with Linear off making no calls, and that nothing is written to
//! Linear.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use mast_engine::linear::{LinearConfig, MemoryKeyStore};
use mast_engine::{EngineCore, HarnessRegistry};
use mast_proto::{HarnessId, WorkItemStatus, WorkOutlineSnapshot};
use mast_rpc::{RpcClient, methods};

const KEY: &str = "lin_api_TESTSECRET0123456789abcdef";
const SPACE: &str = "space-on";
const OFF_SPACE: &str = "space-off";
const CHAT: &str = "chat-1";
const ME: &str = "u-1";

// ── mock Linear ─────────────────────────────────────────────────────────────

#[derive(Default)]
struct MockState {
    /// (operation, full query text) of every request.
    recorded: Vec<(String, String)>,
    /// What the assigned-unstarted query answers (the server may be lenient).
    assigned: Vec<Value>,
    /// Answer the assigned-unstarted query with an error.
    assigned_error: bool,
    issues: HashMap<String, Value>,
}

#[derive(Clone)]
struct MockLinear {
    url: String,
    state: Arc<Mutex<MockState>>,
}

fn node(
    identifier: &str,
    title: &str,
    state: &str,
    assignee: &str,
    project: Option<&str>,
) -> Value {
    json!({
        "id": format!("uuid-{identifier}"),
        "identifier": identifier,
        "title": title,
        "description": null,
        "url": format!("https://linear.app/acme/issue/{identifier}"),
        "priority": 0,
        "priorityLabel": "No priority",
        "updatedAt": "2026-10-28T10:00:00.000Z",
        "state": { "id": "st", "name": state, "type": state, "color": "#aaaaaa" },
        "assignee": { "id": assignee, "name": "Someone" },
        "team": { "id": "t-1", "key": "ENG", "name": "Engineering" },
        "project": project.map(|p| json!({ "id": format!("p-{p}"), "name": p, "url": "" })),
        "labels": { "nodes": [] },
    })
}

impl MockLinear {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/graphql", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(MockState::default()));
        let shared = state.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, shared.clone()));
            }
        });
        Self { url, state }
    }

    /// Set the list and the per-identifier `issue` answers together.
    fn set(&self, nodes: Vec<Value>, listed: &[&str]) {
        let mut state = self.state.lock().unwrap();
        state.assigned = nodes
            .iter()
            .filter(|n| listed.contains(&n["identifier"].as_str().unwrap()))
            .cloned()
            .collect();
        for n in nodes {
            state
                .issues
                .insert(n["identifier"].as_str().unwrap().to_owned(), n);
        }
    }

    fn count(&self, operation: &str) -> usize {
        let state = self.state.lock().unwrap();
        state
            .recorded
            .iter()
            .filter(|(op, _)| op == operation)
            .count()
    }

    fn queries(&self) -> Vec<(String, String)> {
        self.state.lock().unwrap().recorded.clone()
    }
}

fn reply(status: u16, body: Value) -> (u16, Value) {
    (status, body)
}

fn route(state: &MockState, operation: &str, variables: &Value) -> (u16, Value) {
    match operation {
        "ZeronLinearViewer" => reply(
            200,
            json!({ "data": { "viewer": { "id": ME, "name": "Ada", "displayName": "ada" } } }),
        ),
        "ZeronLinearAssignedUnstarted" => {
            if state.assigned_error {
                return reply(
                    400,
                    json!({ "errors": [{ "message": "boom",
                        "extensions": { "code": "GRAPHQL_VALIDATION_FAILED" } }] }),
                );
            }
            reply(
                200,
                json!({ "data": { "viewer": { "assignedIssues": {
                    "nodes": state.assigned,
                    "pageInfo": { "hasNextPage": false, "endCursor": null },
                } } } }),
            )
        }
        "ZeronLinearIssue" => {
            let id = variables["id"].as_str().unwrap_or_default();
            match state.issues.get(id) {
                Some(n) => reply(200, json!({ "data": { "issue": n } })),
                None => reply(
                    200,
                    json!({ "data": null, "errors": [{ "message": "Entity not found: Issue",
                        "extensions": { "code": "INVALID_INPUT" } }] }),
                ),
            }
        }
        "ZeronLinearIssueAttachments" => reply(
            200,
            json!({ "data": { "issue": { "identifier": variables["id"],
                "attachments": { "nodes": [] } } } }),
        ),
        // Other engine background lookups (chat ticket chips) may ask too.
        _ => reply(200, json!({ "data": {} })),
    }
}

async fn serve(mut stream: tokio::net::TcpStream, state: Arc<Mutex<MockState>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        match stream.read(&mut chunk).await {
            Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
            _ => return,
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
    let length: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    let mut body = buf[header_end..].to_vec();
    while body.len() < length {
        match stream.read(&mut chunk).await {
            Ok(n) if n > 0 => body.extend_from_slice(&chunk[..n]),
            _ => break,
        }
    }
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let query = request["query"].as_str().unwrap_or_default().to_owned();
    let operation = query
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .split('(')
        .next()
        .unwrap_or_default()
        .to_owned();
    let (status, payload) = {
        let mut state = state.lock().unwrap();
        state.recorded.push((operation.clone(), query));
        route(&state, &operation, &request["variables"])
    };
    let text = payload.to_string();
    let response = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

// ── engine rig ──────────────────────────────────────────────────────────────

struct Rig {
    _tmp: tempfile::TempDir,
    core: EngineCore,
    client: RpcClient,
    mock: MockLinear,
}

async fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("device-id"), "dev-a").unwrap();
    let core = EngineCore::assemble(
        &dir,
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .expect("engine core assembles");
    let mock = MockLinear::start().await;
    core.linear.reconfigure(LinearConfig::for_tests(
        mock.url.clone(),
        MemoryKeyStore::with_key(KEY),
    ));
    let client = mast_rpc::memory_client(core.rpc_service());
    let rig = Rig {
        _tmp: tmp,
        core,
        client,
        mock,
    };
    for (space, path) in [(SPACE, "/tmp/on"), (OFF_SPACE, "/tmp/off")] {
        rig.call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": space, "deviceId": "dev-a", "path": path }),
        )
        .await
        .unwrap();
    }
    rig.call(
        methods::MUTATE,
        json!({ "op": "createChat", "chatId": CHAT, "spaceId": SPACE, "deviceId": "dev-a" }),
    )
    .await
    .unwrap();
    rig.enable(SPACE, true).await;
    rig
}

fn op() -> String {
    uuid::Uuid::new_v4().to_string()
}

impl Rig {
    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        self.client
            .call(method, params)
            .await
            .map_err(|e| e.to_string())
    }

    async fn mutate(&self, mut params: Value) -> Result<Value, String> {
        params["operationId"] = json!(op());
        self.call(methods::WORK_OUTLINE_MUTATE, params).await
    }

    async fn enable(&self, space: &str, enabled: bool) {
        self.mutate(json!({ "op": "setSpaceLinearEnabled", "spaceId": space, "enabled": enabled }))
            .await
            .unwrap();
    }

    fn snapshot(&self) -> WorkOutlineSnapshot {
        let inputs = self.core.workspace.work_outline_inputs(SPACE);
        mast_doc::materialize_work_outline(SPACE, &inputs)
    }

    async fn item(&self, title: &str) -> String {
        let reply = self
            .mutate(
                json!({ "op": "createWorkItem", "spaceId": SPACE, "title": title,
                "placement": { "type": "last" } }),
            )
            .await
            .unwrap();
        reply["workItemId"].as_str().unwrap().to_owned()
    }

    async fn set_destination(&self, space: &str, item: Option<&str>) -> Result<Value, String> {
        self.mutate(
            json!({ "op": "setLinearAssignedDestination", "spaceId": space,
            "workItemId": item }),
        )
        .await
    }

    /// Manual refresh of the Space (bypasses the caches).
    async fn refresh(&self, space: &str) -> Result<Value, String> {
        self.call(
            methods::LINEAR_REFRESH_LINKED_ITEMS,
            json!({ "spaceId": space }),
        )
        .await
    }

    /// The live and archived children of `parent`, by title.
    fn children(&self, parent: &str) -> Vec<mast_proto::WorkItemView> {
        self.snapshot()
            .items
            .into_iter()
            .filter(|i| i.parent_work_item_id.as_deref() == Some(parent))
            .collect()
    }

    fn child(&self, parent: &str, identifier: &str) -> mast_proto::WorkItemView {
        self.children(parent)
            .into_iter()
            .find(|i| i.title.starts_with(&format!("{identifier} ")))
            .unwrap_or_else(|| panic!("no child {identifier}"))
    }
}

fn marker(item: &mast_proto::WorkItemView) -> Option<String> {
    item.external_sync_state
        .as_ref()
        .and_then(|s| s.marker.clone())
}

fn all() -> Vec<Value> {
    vec![
        node("ENG-1", "Backlog ticket", "backlog", ME, None),
        node("ENG-2", "Unstarted ticket", "unstarted", ME, None),
        node("ENG-3", "Triage ticket", "triage", ME, None),
        node("ENG-4", "Started ticket", "started", ME, None),
        node("ENG-5", "Done ticket", "completed", ME, None),
        node("ENG-6", "Canceled ticket", "canceled", ME, None),
        node("ENG-7", "Someone else's", "backlog", "u-2", None),
        node("ENG-8", "In a project", "backlog", ME, Some("Auth")),
    ]
}

// ── tests ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn only_assigned_unstarted_projectless_tickets_get_children() {
    let rig = rig().await;
    // A lenient server returns every ticket; the sync still filters.
    rig.mock.set(
        all(),
        &[
            "ENG-1", "ENG-2", "ENG-3", "ENG-4", "ENG-5", "ENG-6", "ENG-7", "ENG-8",
        ],
    );
    let dest = rig.item("Linear").await;
    rig.set_destination(SPACE, Some(&dest)).await.unwrap();
    rig.refresh(SPACE).await.unwrap();

    let titles: Vec<String> = rig.children(&dest).into_iter().map(|i| i.title).collect();
    assert_eq!(titles.len(), 3, "{titles:?}");
    for (id, title) in [
        ("ENG-1", "ENG-1 · Backlog ticket"),
        ("ENG-2", "ENG-2 · Unstarted ticket"),
        ("ENG-3", "ENG-3 · Triage ticket"),
    ] {
        let item = rig.child(&dest, id);
        assert_eq!(item.title, title);
        assert_eq!(item.status, WorkItemStatus::Pending);
        let reference = rig
            .snapshot()
            .linear_issue_reference(&item.id)
            .cloned()
            .expect("linear reference");
        assert_eq!(reference.external_id, id);
        let record = item.external_sync_state.unwrap();
        assert_eq!(record.provider, "linear");
        assert_eq!(record.external_id, id);
        assert!(record.marker.is_none());
    }

    // The query asks Linear for exactly this set.
    let queries = rig.mock.queries();
    let list = queries
        .iter()
        .find(|(op, _)| op == "ZeronLinearAssignedUnstarted")
        .unwrap();
    assert!(
        list.1
            .contains("in: [\"backlog\", \"unstarted\", \"triage\"]")
    );
    assert!(list.1.contains("project: { null: true }"));

    // A second pass creates nothing more.
    rig.refresh(SPACE).await.unwrap();
    assert_eq!(rig.children(&dest).len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_ever_written_to_linear() {
    let rig = rig().await;
    rig.mock.set(all(), &["ENG-1", "ENG-2"]);
    let dest = rig.item("Linear").await;
    rig.set_destination(SPACE, Some(&dest)).await.unwrap();
    rig.refresh(SPACE).await.unwrap();
    // ENG-1 starts, ENG-2 stays: a second pass archives one.
    rig.mock.set(all(), &["ENG-2"]);
    rig.refresh(SPACE).await.unwrap();
    for (operation, query) in rig.mock.queries() {
        assert!(
            !query.to_lowercase().contains("mutation"),
            "{operation}: {query}"
        );
        if !query.is_empty() {
            assert!(
                query.trim_start().starts_with("query "),
                "{operation}: {query}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ticket_already_linked_elsewhere_is_not_duplicated() {
    let rig = rig().await;
    rig.mock.set(all(), &["ENG-1", "ENG-2", "ENG-3"]);
    let dest = rig.item("Linear").await;
    let feature = rig.item("My feature").await;
    // ENG-1 is linked by a feature item, ENG-2 by an item the user archived.
    for (id, parent) in [("ENG-1", &feature), ("ENG-2", &feature)] {
        rig.mutate(
            json!({ "op": "createWorkItemExternalReference", "workItemId": parent,
            "provider": "linear", "referenceType": "issue", "externalId": id,
            "url": format!("https://linear.app/acme/issue/{id}") }),
        )
        .await
        .unwrap();
    }
    let archived = rig.item("Old ENG-3 work").await;
    rig.mutate(
        json!({ "op": "createWorkItemExternalReference", "workItemId": archived,
        "provider": "linear", "referenceType": "issue", "externalId": "ENG-3",
        "url": "https://linear.app/acme/issue/ENG-3" }),
    )
    .await
    .unwrap();
    rig.mutate(json!({ "op": "archiveWorkItem", "workItemId": archived,
        "descendantArchivePolicy": { "type": "archiveDescendants" } }))
        .await
        .unwrap();
    rig.set_destination(SPACE, Some(&dest)).await.unwrap();
    rig.refresh(SPACE).await.unwrap();
    assert!(rig.children(&dest).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_untouched_child_is_archived_when_its_ticket_leaves() {
    let cases = [
        (
            "started",
            "Started in Linear",
            node("ENG-1", "T", "started", ME, None),
        ),
        (
            "completed",
            "Completed in Linear",
            node("ENG-1", "T", "completed", ME, None),
        ),
        (
            "canceled",
            "Canceled in Linear",
            node("ENG-1", "T", "canceled", ME, None),
        ),
        (
            "reassigned",
            "Reassigned in Linear",
            node("ENG-1", "T", "backlog", "u-2", None),
        ),
        (
            "project",
            "Moved to project Auth",
            node("ENG-1", "T", "backlog", ME, Some("Auth")),
        ),
    ];
    for (name, reason, leaving) in cases {
        let rig = rig().await;
        rig.mock
            .set(vec![node("ENG-1", "T", "backlog", ME, None)], &["ENG-1"]);
        let dest = rig.item("Linear").await;
        rig.set_destination(SPACE, Some(&dest)).await.unwrap();
        rig.refresh(SPACE).await.unwrap();
        assert!(rig.child(&dest, "ENG-1").archived_at.is_none(), "{name}");

        rig.mock.set(vec![leaving], &[]);
        rig.refresh(SPACE).await.unwrap();
        let item = rig.child(&dest, "ENG-1");
        assert!(item.archived_at.is_some(), "{name}: archived");
        assert_eq!(marker(&item).as_deref(), Some(reason), "{name}");
        let record = item.external_sync_state.unwrap();
        assert!(record.archived_by_linear_sync, "{name}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_touched_child_is_kept_and_marked() {
    for touch in ["chat", "child", "title", "status", "instruction"] {
        let rig = rig().await;
        rig.mock
            .set(vec![node("ENG-1", "T", "backlog", ME, None)], &["ENG-1"]);
        let dest = rig.item("Linear").await;
        rig.set_destination(SPACE, Some(&dest)).await.unwrap();
        rig.refresh(SPACE).await.unwrap();
        let id = rig.child(&dest, "ENG-1").id;
        match touch {
            "chat" => {
                rig.mutate(json!({ "op": "assignChatToWorkItem", "chatId": CHAT,
                    "workItemId": id }))
                    .await
                    .unwrap();
            }
            "child" => {
                rig.mutate(
                    json!({ "op": "createWorkItem", "spaceId": SPACE, "title": "sub",
                    "parentWorkItemId": id, "placement": { "type": "last" } }),
                )
                .await
                .unwrap();
            }
            "title" => {
                rig.mutate(json!({ "op": "updateWorkItem", "workItemId": id,
                    "changes": { "title": "My own title" } }))
                    .await
                    .unwrap();
            }
            "status" => {
                rig.mutate(json!({ "op": "updateWorkItem", "workItemId": id,
                    "changes": { "status": "active" } }))
                    .await
                    .unwrap();
            }
            _ => {
                rig.mutate(json!({ "op": "createWorkItemInstruction", "spaceId": SPACE,
                    "scope": { "type": "workItem", "workItemId": id }, "text": "Do it" }))
                    .await
                    .unwrap();
            }
        }
        rig.mock
            .set(vec![node("ENG-1", "T", "started", ME, None)], &[]);
        rig.refresh(SPACE).await.unwrap();
        let item = rig.snapshot().item(&id).cloned().unwrap();
        assert!(item.archived_at.is_none(), "{touch}: never archived");
        assert_eq!(
            marker(&item).as_deref(),
            Some("Started in Linear"),
            "{touch}"
        );

        // It moves on to completed: the marker follows; nothing is deleted.
        rig.mock
            .set(vec![node("ENG-1", "T", "completed", ME, None)], &[]);
        rig.refresh(SPACE).await.unwrap();
        let item = rig.snapshot().item(&id).cloned().unwrap();
        assert_eq!(
            marker(&item).as_deref(),
            Some("Completed in Linear"),
            "{touch}"
        );

        // It matches again: the marker clears.
        rig.mock
            .set(vec![node("ENG-1", "T", "backlog", ME, None)], &["ENG-1"]);
        rig.refresh(SPACE).await.unwrap();
        let item = rig.snapshot().item(&id).cloned().unwrap();
        assert!(marker(&item).is_none(), "{touch}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_archived_child_comes_back_when_its_ticket_matches_again() {
    let rig = rig().await;
    rig.mock
        .set(vec![node("ENG-1", "T", "backlog", ME, None)], &["ENG-1"]);
    let dest = rig.item("Linear").await;
    rig.set_destination(SPACE, Some(&dest)).await.unwrap();
    rig.refresh(SPACE).await.unwrap();
    rig.mock
        .set(vec![node("ENG-1", "T", "started", ME, None)], &[]);
    rig.refresh(SPACE).await.unwrap();
    assert!(rig.child(&dest, "ENG-1").archived_at.is_some());
    rig.mock
        .set(vec![node("ENG-1", "T", "backlog", ME, None)], &["ENG-1"]);
    rig.refresh(SPACE).await.unwrap();
    let item = rig.child(&dest, "ENG-1");
    assert!(item.archived_at.is_none());
    assert!(marker(&item).is_none());
    assert_eq!(rig.children(&dest).len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_the_user_archived_is_left_alone() {
    let rig = rig().await;
    rig.mock
        .set(vec![node("ENG-1", "T", "backlog", ME, None)], &["ENG-1"]);
    let dest = rig.item("Linear").await;
    rig.set_destination(SPACE, Some(&dest)).await.unwrap();
    rig.refresh(SPACE).await.unwrap();
    let id = rig.child(&dest, "ENG-1").id;
    rig.mutate(json!({ "op": "archiveWorkItem", "workItemId": id,
        "descendantArchivePolicy": { "type": "archiveDescendants" } }))
        .await
        .unwrap();
    rig.refresh(SPACE).await.unwrap();
    let item = rig.snapshot().item(&id).cloned().unwrap();
    assert!(item.archived_at.is_some());
    assert_eq!(rig.children(&dest).len(), 1, "not recreated");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_list_fetch_changes_nothing() {
    let rig = rig().await;
    rig.mock
        .set(vec![node("ENG-1", "T", "backlog", ME, None)], &["ENG-1"]);
    let dest = rig.item("Linear").await;
    rig.set_destination(SPACE, Some(&dest)).await.unwrap();
    rig.refresh(SPACE).await.unwrap();
    rig.mock.state.lock().unwrap().assigned_error = true;
    rig.refresh(SPACE).await.unwrap();
    assert!(rig.child(&dest, "ENG-1").archived_at.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn spaces_without_the_item_or_with_linear_off_make_no_assigned_calls() {
    let rig = rig().await;
    rig.mock.set(all(), &["ENG-1"]);
    // Linear on, no linked item: the pass asks for nothing.
    rig.refresh(SPACE).await.unwrap();
    assert_eq!(rig.mock.count("ZeronLinearAssignedUnstarted"), 0);
    assert_eq!(rig.mock.count("ZeronLinearViewer"), 0);

    // Setting one needs Linear on.
    let other = rig
        .mutate(
            json!({ "op": "createWorkItem", "spaceId": OFF_SPACE, "title": "L",
            "placement": { "type": "last" } }),
        )
        .await
        .unwrap();
    let other = other["workItemId"].as_str().unwrap().to_owned();
    assert!(rig.set_destination(OFF_SPACE, Some(&other)).await.is_err());

    // Turning Linear off after setting it: refresh refuses, no calls.
    let dest = rig.item("Linear").await;
    rig.mock.set(Vec::new(), &[]);
    rig.set_destination(SPACE, Some(&dest)).await.unwrap();
    // Setting it syncs the Space in the background; let that pass finish.
    while rig.mock.count("ZeronLinearAssignedUnstarted") == 0 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    rig.enable(SPACE, false).await;
    let before = rig.mock.queries().len();
    let err = rig.refresh(SPACE).await.unwrap_err();
    assert!(err.contains("disabledForSpace"), "{err}");
    assert_eq!(rig.mock.queries().len(), before, "Linear off: no calls");
    assert!(rig.children(&dest).is_empty());
}
