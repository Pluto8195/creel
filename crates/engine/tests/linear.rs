//! Linear integration v1 against a real offline engine and a local mock of
//! Linear's GraphQL endpoint (never the real API, never the real Keychain):
//! auth header shape, 401 key removal, capped 429 retries, response
//! parsing, cache TTL and force, request coalescing, per-Space gating,
//! createWorkItemFromLinear over RPC, the linked-item sync, agent context,
//! and that the key never appears in replies, agent payloads, or logs.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use zeron_engine::linear::{LinearConfig, MemoryKeyStore, client};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_proto::{
    HarnessId, LinearError, LinearErrorCode, WorkItemStatus, WorkOutlineSnapshot, derive_id, roles,
};
use zeron_rpc::{RpcClient, methods};

const KEY: &str = "lin_api_TESTSECRET0123456789abcdef";
const OTHER_KEY: &str = "lin_api_OTHERSECRET987654321";
const SPACE: &str = "space-on";
const OFF_SPACE: &str = "space-off";
const CHAT: &str = "chat-1";

// ── log capture (process-wide) ──────────────────────────────────────────────

#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn logs() -> &'static LogBuffer {
    static LOGS: OnceLock<LogBuffer> = OnceLock::new();
    LOGS.get_or_init(|| {
        let buffer = LogBuffer::default();
        let writer = buffer.clone();
        let _ = tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::TRACE)
                .with_writer(move || writer.clone())
                .finish(),
        );
        buffer
    })
}

// ── mock Linear ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Recorded {
    authorization: Option<String>,
    operation: String,
    variables: Value,
}

#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Value,
}

impl Reply {
    fn ok(data: Value) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: json!({ "data": data }),
        }
    }
}

#[derive(Default)]
struct MockState {
    recorded: Vec<Recorded>,
    /// Replies served before the default routing, in order.
    script: VecDeque<Reply>,
    issues: HashMap<String, Value>,
    my_issue_pages: Vec<Vec<Value>>,
    projects: Vec<Value>,
    project_issues: HashMap<String, Vec<Value>>,
    /// Attachment nodes per issue identifier.
    attachments: HashMap<String, Vec<Value>>,
    /// Answer attachment queries with a GraphQL validation error.
    attachments_error: bool,
    delay: Duration,
}

#[derive(Clone)]
struct MockLinear {
    url: String,
    state: Arc<Mutex<MockState>>,
}

impl MockLinear {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/graphql", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(MockState::default()));
        let shared = state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(serve(stream, shared.clone()));
            }
        });
        Self { url, state }
    }

    fn recorded(&self) -> Vec<Recorded> {
        self.state.lock().unwrap().recorded.clone()
    }

    fn count(&self, operation: &str) -> usize {
        self.recorded()
            .iter()
            .filter(|r| r.operation == operation)
            .count()
    }

    fn set_issue(&self, node: Value) {
        let id = node["identifier"].as_str().unwrap().to_owned();
        self.state.lock().unwrap().issues.insert(id, node);
    }

    fn set_attachments(&self, identifier: &str, nodes: Vec<Value>) {
        self.state
            .lock()
            .unwrap()
            .attachments
            .insert(identifier.to_owned(), nodes);
    }

    fn script(&self, reply: Reply) {
        self.state.lock().unwrap().script.push_back(reply);
    }
}

fn issue_node(identifier: &str, title: &str, description: Option<&str>, state: &str) -> Value {
    json!({
        "id": format!("uuid-{identifier}"),
        "identifier": identifier,
        "title": title,
        "description": description,
        "url": format!("https://linear.app/acme/issue/{identifier}"),
        "priority": 2,
        "priorityLabel": "High",
        "branchName": format!("ada/{}", identifier.to_lowercase()),
        "updatedAt": "2026-10-13T10:00:00.000Z",
        "state": { "id": "st", "name": "In Progress", "type": state, "color": "#f2c94c" },
        "assignee": { "id": "u-1", "name": "Ada", "displayName": "ada", "email": "ada@example.com" },
        "team": { "id": "t-1", "key": "ENG", "name": "Engineering" },
        "project": null,
        "labels": { "nodes": [{ "name": "bug", "color": "#eb5757" }] },
    })
}

fn page(nodes: Vec<Value>, next: Option<&str>) -> Value {
    json!({
        "nodes": nodes,
        "pageInfo": { "hasNextPage": next.is_some(), "endCursor": next },
    })
}

fn route(state: &MockState, operation: &str, auth: Option<&str>, variables: &Value) -> Reply {
    if auth != Some(KEY) {
        return Reply {
            status: 401,
            headers: Vec::new(),
            body: json!({ "errors": [{ "message": "Authentication required, not authenticated",
                "extensions": { "code": "AUTHENTICATION_ERROR" } }] }),
        };
    }
    match operation {
        "ZeronLinearViewer" => Reply::ok(json!({ "viewer": {
            "id": "u-1", "name": "Ada Lovelace", "displayName": "ada", "email": "ada@example.com"
        }})),
        "ZeronLinearIssue" => {
            let id = variables["id"].as_str().unwrap_or_default();
            match state.issues.get(id) {
                Some(node) => Reply::ok(json!({ "issue": node })),
                None => Reply {
                    status: 200,
                    headers: Vec::new(),
                    body: json!({ "data": null, "errors": [{ "message": "Entity not found: Issue",
                        "extensions": { "code": "INVALID_INPUT" } }] }),
                },
            }
        }
        "ZeronLinearMyIssues" => {
            let index = match variables["after"].as_str() {
                None => 0,
                Some(cursor) => cursor.trim_start_matches('c').parse().unwrap_or(0),
            };
            let nodes = state.my_issue_pages.get(index).cloned().unwrap_or_default();
            let next = (index + 1 < state.my_issue_pages.len()).then(|| format!("c{}", index + 1));
            Reply::ok(json!({ "viewer": { "assignedIssues": page(nodes, next.as_deref()) } }))
        }
        "ZeronLinearIssueAttachments" => {
            if state.attachments_error {
                return Reply {
                    status: 400,
                    headers: Vec::new(),
                    body: json!({ "errors": [{ "message": "Cannot query field \"sourceType\" on type \"Attachment\".",
                        "extensions": { "code": "GRAPHQL_VALIDATION_FAILED" } }] }),
                };
            }
            let id = variables["id"].as_str().unwrap_or_default();
            let nodes = state.attachments.get(id).cloned().unwrap_or_default();
            Reply::ok(json!({ "issue": { "identifier": id, "attachments": { "nodes": nodes } } }))
        }
        "ZeronLinearMyProjects" => {
            Reply::ok(json!({ "projects": page(state.projects.clone(), None) }))
        }
        "ZeronLinearProjectIssues" => {
            let id = variables["id"].as_str().unwrap_or_default();
            match state.project_issues.get(id) {
                Some(nodes) => {
                    Reply::ok(json!({ "project": { "issues": page(nodes.clone(), None) } }))
                }
                None => Reply::ok(json!({ "project": null })),
            }
        }
        other => panic!("unexpected operation {other}"),
    }
}

async fn serve(mut stream: tokio::net::TcpStream, state: Arc<Mutex<MockState>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        let Ok(n) = stream.read(&mut chunk).await else {
            return;
        };
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut headers = HashMap::new();
    for line in head.lines().skip(1) {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    let length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[header_end..].to_vec();
    while body.len() < length {
        let Ok(n) = stream.read(&mut chunk).await else {
            return;
        };
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let query = request["query"].as_str().unwrap_or_default();
    let operation = query
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .split('(')
        .next()
        .unwrap_or_default()
        .to_owned();
    let auth = headers.get("authorization").cloned();
    let (reply, delay) = {
        let mut state = state.lock().unwrap();
        state.recorded.push(Recorded {
            authorization: auth.clone(),
            operation: operation.clone(),
            variables: request["variables"].clone(),
        });
        let reply = match state.script.pop_front() {
            Some(reply) => reply,
            None => route(&state, &operation, auth.as_deref(), &request["variables"]),
        };
        (reply, state.delay)
    };
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let body = reply.body.to_string();
    let mut response = format!(
        "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        body.len()
    );
    for (k, v) in &reply.headers {
        response.push_str(&format!("{k}: {v}\r\n"));
    }
    response.push_str("\r\n");
    response.push_str(&body);
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

// ── engine rig ──────────────────────────────────────────────────────────────

struct Rig {
    _tmp: tempfile::TempDir,
    core: EngineCore,
    client: RpcClient,
    mock: MockLinear,
    store: Arc<MemoryKeyStore>,
    sleeps: Arc<Mutex<Vec<Duration>>>,
    /// Every RPC reply/error text this test saw (for the leak check).
    seen: Arc<Mutex<Vec<String>>>,
}

async fn rig_with(store: Arc<MemoryKeyStore>, ttl: Duration) -> Rig {
    logs();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("device-id"), "dev-a").unwrap();
    let core = EngineCore::assemble(&dir, Arc::new(HarnessRegistry::new()), HarnessId::Mock, None)
        .expect("engine core assembles");
    let mock = MockLinear::start().await;
    let sleeps: Arc<Mutex<Vec<Duration>>> = Arc::default();
    let recorder = sleeps.clone();
    let mut config = LinearConfig::for_tests(mock.url.clone(), store.clone());
    config.issue_ttl = ttl;
    config.sleeper = Arc::new(move |d| {
        recorder.lock().unwrap().push(d);
        Box::pin(async {})
    });
    core.linear.reconfigure(config);
    let client = zeron_rpc::memory_client(core.rpc_service());
    let rig = Rig {
        _tmp: tmp,
        core,
        client,
        mock,
        store,
        sleeps,
        seen: Arc::default(),
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
    rig
}

async fn rig() -> Rig {
    rig_with(MemoryKeyStore::with_key(KEY), Duration::from_secs(300)).await
}

impl Rig {
    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let result = self
            .client
            .call(method, params)
            .await
            .map_err(|e| e.to_string());
        self.seen.lock().unwrap().push(match &result {
            Ok(value) => value.to_string(),
            Err(text) => text.clone(),
        });
        result
    }

    async fn linear_err(&self, method: &str, params: Value) -> LinearError {
        let text = self.call(method, params).await.unwrap_err();
        let at = text
            .find("linear/")
            .unwrap_or_else(|| panic!("not a linear error: {text}"));
        LinearError::from_wire_message(&text[at..]).unwrap()
    }

    async fn mutate(&self, params: Value) -> Result<Value, String> {
        self.call(methods::WORK_OUTLINE_MUTATE, params).await
    }

    async fn enable(&self, space: &str, enabled: bool) {
        self.mutate(json!({ "op": "setSpaceLinearEnabled", "spaceId": space,
            "enabled": enabled, "operationId": uuid::Uuid::new_v4().to_string() }))
            .await
            .unwrap();
    }

    fn snapshot(&self, space: &str) -> WorkOutlineSnapshot {
        let inputs = self.core.workspace.work_outline_inputs(space);
        zeron_doc::materialize_work_outline(space, &inputs)
    }

    async fn from_linear(&self, op: &str, parent: Option<&str>, identifier: &str) -> String {
        let node = self.mock.state.lock().unwrap().issues[identifier].clone();
        let mut params = json!({
            "op": "createWorkItemFromLinear", "spaceId": SPACE,
            "placement": { "type": "last" }, "operationId": op,
            "issue": {
                "id": node["id"], "identifier": identifier, "title": node["title"],
                "description": node["description"], "url": node["url"],
            },
        });
        if let Some(parent) = parent {
            params["parentWorkItemId"] = json!(parent);
        }
        let reply = self.mutate(params).await.unwrap();
        reply["workItemId"].as_str().unwrap().to_owned()
    }
}

// ── tests ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn set_key_validates_with_viewer_and_sends_a_bare_authorization_header() {
    let rig = rig_with(MemoryKeyStore::new(), Duration::from_secs(300)).await;
    let info = rig.call(methods::ENGINE_INFO, json!({})).await.unwrap();
    assert!(info["capabilities"].as_array().unwrap().contains(&json!("linear-v1")));

    let status = rig.call(methods::LINEAR_STATUS, json!({})).await.unwrap();
    assert_eq!(status, json!({ "configured": false, "source": "none" }));
    let err = rig
        .linear_err(methods::LINEAR_ISSUE, json!({ "identifier": "ENG-1" }))
        .await;
    assert_eq!(err.code, LinearErrorCode::MissingKey);
    assert!(rig.mock.recorded().is_empty(), "no key, no request");

    // A rejected key is not stored.
    let err = rig
        .linear_err(methods::LINEAR_SET_API_KEY, json!({ "apiKey": OTHER_KEY }))
        .await;
    assert_eq!(err.code, LinearErrorCode::Unauthorized);
    assert_eq!(rig.store.stored(), None);

    let status = rig
        .call(methods::LINEAR_SET_API_KEY, json!({ "apiKey": format!("  {KEY}\n") }))
        .await
        .unwrap();
    assert_eq!(status["configured"], json!(true));
    assert_eq!(status["source"], json!("keychain"));
    assert_eq!(status["viewer"]["email"], json!("ada@example.com"));
    assert_eq!(rig.store.stored().as_deref(), Some(KEY));
    let (reads, writes, _) = rig.store.calls();
    assert!(writes == 1 && reads >= 1, "stored and read back");

    let recorded = rig.mock.recorded();
    let last = recorded.last().unwrap();
    assert_eq!(last.operation, "ZeronLinearViewer");
    assert_eq!(last.authorization.as_deref(), Some(KEY));
    assert!(recorded
        .iter()
        .all(|r| !r.authorization.clone().unwrap_or_default().starts_with("Bearer")));

    // Status is served from the verified cache; clear removes the key.
    let before = rig.mock.recorded().len();
    let again = rig.call(methods::LINEAR_STATUS, json!({})).await.unwrap();
    assert_eq!(again, status);
    assert_eq!(rig.mock.recorded().len(), before);
    let cleared = rig.call(methods::LINEAR_CLEAR_API_KEY, json!({})).await.unwrap();
    assert_eq!(cleared, json!({ "configured": false, "source": "none" }));
    assert_eq!(rig.store.stored(), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_401_deletes_the_stored_key() {
    let rig = rig().await;
    rig.mock.script(Reply {
        status: 401,
        headers: Vec::new(),
        body: json!({ "errors": [{ "message": "Authentication required" }] }),
    });
    let err = rig
        .linear_err(methods::LINEAR_ISSUE, json!({ "identifier": "ENG-1" }))
        .await;
    assert_eq!(err.code, LinearErrorCode::Unauthorized);
    assert_eq!(rig.store.stored(), None, "rejected key deleted");
    let status = rig.call(methods::LINEAR_STATUS, json!({})).await.unwrap();
    assert_eq!(status["configured"], json!(false));
    let err = rig
        .linear_err(methods::LINEAR_MY_ISSUES, json!({}))
        .await;
    assert_eq!(err.code, LinearErrorCode::MissingKey);
}

#[tokio::test(flavor = "multi_thread")]
async fn rate_limits_retry_with_a_capped_wait_then_give_up() {
    let rig = rig().await;
    rig.mock.set_issue(issue_node("ENG-1", "One", None, "started"));
    let limited = |retry_after: &str| Reply {
        status: 429,
        headers: vec![("Retry-After".into(), retry_after.into())],
        body: json!({ "errors": [{ "message": "Rate limit exceeded" }] }),
    };
    // One 429 (an hour) then success: waits the 30 s cap once.
    rig.mock.script(limited("3600"));
    let issue = rig
        .call(methods::LINEAR_ISSUE, json!({ "identifier": "eng-1" }))
        .await
        .unwrap();
    assert_eq!(issue["identifier"], json!("ENG-1"));
    assert_eq!(*rig.sleeps.lock().unwrap(), vec![Duration::from_secs(30)]);

    // Rate-limited on every attempt (also as a GraphQL RATELIMITED error).
    rig.sleeps.lock().unwrap().clear();
    rig.mock.script(limited("2"));
    rig.mock.script(Reply {
        status: 400,
        headers: Vec::new(),
        body: json!({ "errors": [{ "message": "Rate limit exceeded",
            "extensions": { "code": "RATELIMITED" } }] }),
    });
    rig.mock.script(limited("1"));
    let before = rig.mock.count("ZeronLinearIssue");
    let err = rig
        .linear_err(
            methods::LINEAR_ISSUE,
            json!({ "identifier": "ENG-1", "forceRefresh": true }),
        )
        .await;
    assert_eq!(err.code, LinearErrorCode::RateLimited);
    assert_eq!(
        rig.mock.count("ZeronLinearIssue") - before,
        client::MAX_ATTEMPTS
    );
    let sleeps = rig.sleeps.lock().unwrap().clone();
    assert_eq!(sleeps.len(), client::MAX_ATTEMPTS - 1);
    assert!(sleeps.iter().all(|d| *d <= client::MAX_RETRY_AFTER));
}

#[tokio::test(flavor = "multi_thread")]
async fn list_queries_paginate_and_parse() {
    let rig = rig().await;
    {
        let mut state = rig.mock.state.lock().unwrap();
        state.my_issue_pages = vec![
            vec![
                issue_node("ENG-1", "One", Some("Body"), "started"),
                issue_node("ENG-2", "Two", None, "unstarted"),
            ],
            vec![issue_node("ENG-3", "Three", None, "backlog")],
        ];
        state.projects = vec![
            json!({ "id": "p-1", "name": "Auth", "url": "https://linear.app/acme/project/auth",
                "state": "started", "updatedAt": "2026-10-01T00:00:00.000Z",
                "lead": { "id": "u-1", "name": "Ada" } }),
            json!({ "id": "p-2", "name": "Old", "url": "u", "state": "completed",
                "updatedAt": "2026-01-01T00:00:00.000Z", "lead": null }),
        ];
        state.project_issues.insert(
            "p-1".into(),
            vec![
                issue_node("ENG-5", "Five", None, "triage"),
                issue_node("ENG-6", "Six", None, "canceled"),
            ],
        );
    }
    let issues = rig.call(methods::LINEAR_MY_ISSUES, json!({})).await.unwrap();
    let ids: Vec<&str> = issues
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["identifier"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["ENG-1", "ENG-2", "ENG-3"]);
    assert_eq!(rig.mock.count("ZeronLinearMyIssues"), 2, "two pages");
    let first = &issues[0];
    assert_eq!(first["state"]["type"], json!("started"));
    assert_eq!(first["assignee"]["displayName"], json!("ada"));
    assert_eq!(first["labels"], json!([{ "name": "bug", "color": "#eb5757" }]));
    assert_eq!(first["priority"], json!(2));
    assert_eq!(first["team"]["key"], json!("ENG"));
    assert_eq!(first["description"], json!("Body"));
    let page_vars = &rig.mock.recorded()[1].variables;
    assert_eq!(page_vars["after"], json!("c1"));
    assert_eq!(page_vars["first"], json!(50));

    let projects = rig.call(methods::LINEAR_MY_PROJECTS, json!({})).await.unwrap();
    assert_eq!(projects.as_array().unwrap().len(), 1, "completed project hidden");
    assert_eq!(projects[0]["name"], json!("Auth"));
    assert_eq!(projects[0]["lead"]["name"], json!("Ada"));

    let open = rig
        .call(methods::LINEAR_PROJECT_ISSUES, json!({ "projectId": "p-1" }))
        .await
        .unwrap();
    assert_eq!(open.as_array().unwrap().len(), 1);
    assert_eq!(open[0]["identifier"], json!("ENG-5"));
    let err = rig
        .linear_err(methods::LINEAR_PROJECT_ISSUES, json!({ "projectId": "nope" }))
        .await;
    assert_eq!(err.code, LinearErrorCode::NotFound);
    let err = rig
        .linear_err(methods::LINEAR_ISSUE, json!({ "identifier": "ENG-404" }))
        .await;
    assert_eq!(err.code, LinearErrorCode::NotFound);
    let err = rig
        .linear_err(methods::LINEAR_ISSUE, json!({ "identifier": "not an id" }))
        .await;
    assert_eq!(err.code, LinearErrorCode::Invalid);
}

#[tokio::test(flavor = "multi_thread")]
async fn issues_are_cached_until_the_ttl_or_a_forced_refresh() {
    let rig = rig().await;
    rig.mock.set_issue(issue_node("ENG-1", "One", None, "started"));
    for _ in 0..3 {
        rig.call(methods::LINEAR_ISSUE, json!({ "identifier": "ENG-1" }))
            .await
            .unwrap();
    }
    assert_eq!(rig.mock.count("ZeronLinearIssue"), 1);
    rig.mock.set_issue(issue_node("ENG-1", "One v2", None, "started"));
    let cached = rig
        .call(methods::LINEAR_ISSUE, json!({ "identifier": "ENG-1" }))
        .await
        .unwrap();
    assert_eq!(cached["title"], json!("One"));
    let fresh = rig
        .call(methods::LINEAR_ISSUE, json!({ "identifier": "ENG-1", "forceRefresh": true }))
        .await
        .unwrap();
    assert_eq!(fresh["title"], json!("One v2"));
    assert_eq!(rig.mock.count("ZeronLinearIssue"), 2);

    // A zero TTL refetches every time.
    let short = rig_with(MemoryKeyStore::with_key(KEY), Duration::ZERO).await;
    short.mock.set_issue(issue_node("ENG-1", "One", None, "started"));
    for _ in 0..2 {
        short
            .call(methods::LINEAR_ISSUE, json!({ "identifier": "ENG-1" }))
            .await
            .unwrap();
    }
    assert_eq!(short.mock.count("ZeronLinearIssue"), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn identical_in_flight_requests_are_coalesced() {
    let rig = rig().await;
    rig.mock.state.lock().unwrap().delay = Duration::from_millis(300);
    rig.mock.state.lock().unwrap().my_issue_pages =
        vec![vec![issue_node("ENG-1", "One", None, "started")]];
    let calls = (0..4).map(|_| rig.call(methods::LINEAR_MY_ISSUES, json!({})));
    let results = futures::future::join_all(calls).await;
    assert!(results.iter().all(Result::is_ok));
    assert_eq!(rig.mock.count("ZeronLinearMyIssues"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn space_scoped_linear_operations_need_linear_on() {
    let rig = rig().await;
    rig.mock.set_issue(issue_node("ENG-1", "One", Some("Body"), "started"));
    let snapshot = rig.snapshot(SPACE);
    assert!(!snapshot.space_settings.linear_enabled, "off by default");

    let create = |space: &str, op: &str| {
        json!({ "op": "createWorkItemFromLinear", "spaceId": space,
            "placement": { "type": "last" }, "operationId": op,
            "issue": { "id": "uuid-ENG-1", "identifier": "ENG-1", "title": "One",
                "url": "https://linear.app/acme/issue/ENG-1" } })
    };
    let text = rig.mutate(create(SPACE, "op-off")).await.unwrap_err();
    assert!(text.contains("linear/disabledForSpace"), "{text}");
    let err = rig
        .linear_err(methods::LINEAR_REFRESH_LINKED_ITEMS, json!({ "spaceId": SPACE }))
        .await;
    assert_eq!(err.code, LinearErrorCode::DisabledForSpace);
    let item = rig
        .mutate(json!({ "op": "createWorkItem", "spaceId": SPACE, "title": "Plain",
            "placement": { "type": "last" }, "operationId": "plain" }))
        .await
        .unwrap()["workItemId"]
        .as_str()
        .unwrap()
        .to_owned();
    let link = json!({ "op": "linkLinearProject", "workItemId": item, "operationId": "link",
        "project": { "id": "p-1", "name": "Auth", "url": "https://linear.app/acme/project/auth" } });
    let text = rig.mutate(link.clone()).await.unwrap_err();
    assert!(text.contains("linear/disabledForSpace"), "{text}");

    // Account-wide reads work regardless.
    rig.call(methods::LINEAR_ISSUE, json!({ "identifier": "ENG-1" }))
        .await
        .unwrap();

    rig.enable(SPACE, true).await;
    let reply = rig.mutate(create(SPACE, "op-on")).await.unwrap();
    let id = derive_id("op-on", roles::ITEM);
    assert_eq!(reply["workItemId"], json!(id));
    assert!(reply["referenceId"].is_string());
    assert_eq!(rig.mutate(create(SPACE, "op-on")).await.unwrap(), reply, "idempotent");
    let snapshot = rig.snapshot(SPACE);
    assert_eq!(snapshot.items.len(), 2);
    assert_eq!(snapshot.item(&id).unwrap().title, "ENG-1 · One");
    assert_eq!(
        snapshot.linear_issue_reference(&id).unwrap().external_id,
        "ENG-1"
    );
    rig.mutate(link).await.unwrap();
    assert!(rig.snapshot(SPACE).linear_project_reference(&item).is_some());
    // Still off in the other Space.
    let text = rig.mutate(create(OFF_SPACE, "op-other")).await.unwrap_err();
    assert!(text.contains("linear/disabledForSpace"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_follows_linear_for_unmodified_fields_only() {
    let rig = rig().await;
    rig.mock.set_issue(issue_node("ENG-1", "One", Some("Body"), "started"));
    rig.mock.set_issue(issue_node("ENG-2", "Two", Some("Second"), "started"));
    rig.enable(SPACE, true).await;
    let parent = rig.from_linear("op-1", None, "ENG-1").await;
    let child = rig.from_linear("op-2", Some(&parent), "ENG-2").await;
    let plain = rig
        .mutate(json!({ "op": "createWorkItem", "spaceId": SPACE, "title": "Plain",
            "parentWorkItemId": parent, "placement": { "type": "last" }, "operationId": "plain" }))
        .await
        .unwrap()["workItemId"]
        .as_str()
        .unwrap()
        .to_owned();
    rig.mutate(json!({ "op": "updateWorkItem", "workItemId": parent,
        "changes": { "status": "active" }, "operationId": "activate" }))
        .await
        .unwrap();
    let before = rig.snapshot(SPACE);

    // Linear changes title and description of both issues.
    rig.mock.set_issue(issue_node("ENG-1", "One v2", Some("Body v2"), "completed"));
    rig.mock.set_issue(issue_node("ENG-2", "Two v2", None, "started"));
    let reply = rig
        .call(methods::LINEAR_REFRESH_LINKED_ITEMS, json!({ "spaceId": SPACE }))
        .await
        .unwrap();
    assert_eq!(reply, json!({ "updated": 2 }));
    let after = rig.snapshot(SPACE);
    let p = after.item(&parent).unwrap();
    assert_eq!(p.title, "ENG-1 · One v2");
    assert_eq!(p.description.as_deref(), Some("Body v2"));
    assert_eq!(p.status, WorkItemStatus::Active, "status never follows Linear");
    let c = after.item(&child).unwrap();
    assert_eq!(c.title, "ENG-2 · Two v2");
    assert_eq!(c.description, None);
    // Structure untouched.
    assert_eq!(after.items.len(), before.items.len());
    assert_eq!(after.root_work_item_ids, before.root_work_item_ids);
    assert_eq!(p.child_ids, before.item(&parent).unwrap().child_ids);
    assert_eq!(c.parent_work_item_id.as_deref(), Some(parent.as_str()));
    assert_eq!(after.item(&plain).unwrap().title, "Plain");
    assert!(after.items.iter().all(|i| i.archived_at.is_none()));

    // The user takes over the parent's title.
    rig.mutate(json!({ "op": "updateWorkItem", "workItemId": parent,
        "changes": { "title": "My parent" }, "operationId": "user-title" }))
        .await
        .unwrap();
    assert_eq!(
        rig.snapshot(SPACE).item(&parent).unwrap().external_sync_modified_fields,
        vec!["title".to_owned()]
    );
    rig.mock.set_issue(issue_node("ENG-1", "One v3", Some("Body v3"), "started"));
    let reply = rig
        .call(methods::LINEAR_REFRESH_LINKED_ITEMS, json!({ "spaceId": SPACE }))
        .await
        .unwrap();
    assert_eq!(reply, json!({ "updated": 1 }));
    let p = rig.snapshot(SPACE).item(&parent).cloned().unwrap();
    assert_eq!(p.title, "My parent");
    assert_eq!(p.description.as_deref(), Some("Body v3"));
    // Nothing left to do.
    let reply = rig
        .call(methods::LINEAR_REFRESH_LINKED_ITEMS, json!({ "spaceId": SPACE }))
        .await
        .unwrap();
    assert_eq!(reply, json!({ "updated": 0 }));
}

#[tokio::test(flavor = "multi_thread")]
async fn agents_get_the_ticket_only_in_linear_enabled_spaces_and_never_the_key() {
    let rig = rig().await;
    rig.mock.set_issue(issue_node("ENG-1", "One", Some("Full description"), "started"));
    rig.enable(SPACE, true).await;
    let parent = rig.from_linear("op-1", None, "ENG-1").await;
    let child = rig
        .mutate(json!({ "op": "createWorkItem", "spaceId": SPACE, "title": "Child",
            "parentWorkItemId": parent, "placement": { "type": "last" }, "operationId": "child" }))
        .await
        .unwrap()["workItemId"]
        .as_str()
        .unwrap()
        .to_owned();
    rig.mutate(json!({ "op": "linkLinearProject", "workItemId": parent, "operationId": "proj",
        "project": { "id": "p-1", "name": "Auth", "url": "https://linear.app/acme/project/auth" } }))
        .await
        .unwrap();
    rig.mutate(json!({ "op": "assignChatToWorkItem", "chatId": CHAT, "workItemId": child,
        "operationId": "assign" }))
        .await
        .unwrap();
    let lease = rig
        .core
        .sessions
        .agent_tokens()
        .mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    let read = || async {
        rig.call(
            zeron_proto::agent_tools::TOOL_CALL_METHOD,
            json!({ "token": lease.token(), "tool": "work_outline_read_current", "arguments": {} }),
        )
        .await
        .unwrap()
    };
    let current = read().await;
    assert_eq!(current["workItem"]["id"], json!(child));
    assert_eq!(current["linearIssue"]["identifier"], json!("ENG-1"));
    assert_eq!(current["linearIssue"]["description"], json!("Full description"));
    assert_eq!(current["linearIssue"]["state"]["name"], json!("In Progress"));
    assert_eq!(current["linearProject"]["id"], json!("p-1"));
    assert_eq!(current["linearProject"]["name"], json!("Auth"));

    rig.enable(SPACE, false).await;
    let current = read().await;
    assert!(current.get("linearIssue").is_none(), "{current}");
    assert!(current.get("linearProject").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_key_never_appears_in_replies_agent_payloads_or_logs() {
    let rig = rig().await;
    rig.mock.set_issue(issue_node("ENG-1", "One", Some("Body"), "started"));
    rig.enable(SPACE, true).await;
    let item = rig.from_linear("op-1", None, "ENG-1").await;
    rig.mutate(json!({ "op": "assignChatToWorkItem", "chatId": CHAT, "workItemId": item,
        "operationId": "assign" }))
        .await
        .unwrap();
    let lease = rig
        .core
        .sessions
        .agent_tokens()
        .mint(CHAT, "run-1", SPACE, &rig.core.device_id);
    rig.call(
        zeron_proto::agent_tools::TOOL_CALL_METHOD,
        json!({ "token": lease.token(), "tool": "work_outline_read_current", "arguments": {} }),
    )
    .await
    .unwrap();
    rig.call(methods::LINEAR_STATUS, json!({ "forceRefresh": true }))
        .await
        .unwrap();
    rig.call(methods::LINEAR_MY_ISSUES, json!({})).await.unwrap();
    rig.call(methods::LINEAR_REFRESH_LINKED_ITEMS, json!({ "spaceId": SPACE }))
        .await
        .unwrap();
    rig.call(methods::LINEAR_SET_API_KEY, json!({ "apiKey": KEY }))
        .await
        .unwrap();
    let _ = rig
        .call(methods::LINEAR_SET_API_KEY, json!({ "apiKey": OTHER_KEY }))
        .await;
    rig.mock.script(Reply {
        status: 401,
        headers: Vec::new(),
        body: json!({ "errors": [{ "message": "Authentication required" }] }),
    });
    let _ = rig
        .call(methods::LINEAR_ISSUE, json!({ "identifier": "ENG-1", "forceRefresh": true }))
        .await;
    let snapshot = serde_json::to_string(&rig.snapshot(SPACE)).unwrap();
    let seen = rig.seen.lock().unwrap().join("\n");
    let log_text = String::from_utf8_lossy(&logs().0.lock().unwrap()).into_owned();
    for secret in [KEY, OTHER_KEY] {
        assert!(!seen.contains(secret), "key in an RPC reply");
        assert!(!snapshot.contains(secret), "key in the outline");
        assert!(!log_text.contains(secret), "key in the logs");
    }
    assert!(log_text.contains("linear"), "logs were captured");
}

// ── PRs attached to linked issues ───────────────────────────────────────────

const GIT_SPACE: &str = "space-git";

/// A Linear-enabled Space whose folder is a git repository with origin
/// `acme/ui`, and an item there linked to `identifier`.
async fn git_space_item(rig: &Rig, identifier: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = std::fs::canonicalize(dir.path()).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["remote", "add", "origin", "git@github.com:acme/ui.git"],
    ] {
        let status = std::process::Command::new("git")
            .args(&args)
            .current_dir(&path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }
    rig.call(
        methods::MUTATE,
        json!({ "op": "createSpace", "spaceId": GIT_SPACE, "deviceId": "dev-a",
            "path": path.to_string_lossy() }),
    )
    .await
    .unwrap();
    rig.enable(GIT_SPACE, true).await;
    let node = rig.mock.state.lock().unwrap().issues[identifier].clone();
    let reply = rig
        .mutate(json!({
            "op": "createWorkItemFromLinear", "spaceId": GIT_SPACE,
            "placement": { "type": "last" }, "operationId": format!("from-{identifier}"),
            "issue": {
                "id": node["id"], "identifier": identifier, "title": node["title"],
                "description": node["description"], "url": node["url"],
            },
        }))
        .await
        .unwrap();
    (dir, reply["workItemId"].as_str().unwrap().to_owned())
}

/// `(externalId, url, cachedTitle, created by system)` of the item's PRs.
fn item_prs(rig: &Rig, item: &str) -> Vec<(String, String, Option<String>, bool)> {
    let mut prs: Vec<_> = rig
        .snapshot(GIT_SPACE)
        .external_references
        .into_iter()
        .filter(|r| r.work_item_id == item && r.provider == "github")
        .map(|r| {
            let system = r
                .created_by
                .is_some_and(|a| a.kind == zeron_proto::WorkOutlineActorKind::System);
            (r.external_id, r.url, r.cached_title, system)
        })
        .collect();
    prs.sort();
    prs
}

#[tokio::test(flavor = "multi_thread")]
async fn github_pr_attachments_become_references_on_the_linked_item() {
    let rig = rig().await;
    rig.mock.set_issue(issue_node("ENG-2690", "Receive flow", None, "started"));
    rig.mock.set_attachments(
        "ENG-2690",
        vec![
            json!({ "url": "https://github.com/acme/ui/pull/9", "title": "ENG-2690 Receive UI",
                "sourceType": "github", "subtitle": "Open", "metadata": { "state": "open" },
                "updatedAt": "2026-10-13T10:00:00.000Z" }),
            json!({ "url": "https://acme.slack.com/archives/C1/p1", "title": "Thread",
                "sourceType": "slack" }),
            json!({ "url": "https://github.com/acme/ui/issues/3", "sourceType": "github" }),
            json!({ "url": "https://github.com/other/thing/pull/1", "sourceType": "github" }),
        ],
    );
    let (_dir, item) = git_space_item(&rig, "ENG-2690").await;
    rig.call(methods::LINEAR_REFRESH_LINKED_ITEMS, json!({ "spaceId": GIT_SPACE }))
        .await
        .unwrap();
    assert_eq!(
        item_prs(&rig, &item),
        vec![(
            "acme/ui#9".to_owned(),
            "https://github.com/acme/ui/pull/9".to_owned(),
            Some("ENG-2690 Receive UI".to_owned()),
            true
        )],
        "only the GitHub PR of a repository in the Space"
    );
    let snapshot = rig.snapshot(GIT_SPACE);
    assert_eq!(snapshot.item(&item).unwrap().status, WorkItemStatus::Pending);
    assert!(snapshot.linear_issue_reference(&item).is_some());

    // Idempotent; a second PR attached later is added next to the first.
    let reply = rig
        .call(methods::LINEAR_REFRESH_LINKED_ITEMS, json!({ "spaceId": GIT_SPACE }))
        .await
        .unwrap();
    assert_eq!(reply, json!({ "updated": 0 }));
    rig.mock.set_attachments(
        "ENG-2690",
        vec![
            json!({ "url": "https://github.com/acme/ui/pull/9", "sourceType": "github" }),
            json!({ "url": "https://github.com/acme/ui/pull/10", "sourceType": "github" }),
        ],
    );
    let reply = rig
        .call(methods::LINEAR_REFRESH_LINKED_ITEMS, json!({ "spaceId": GIT_SPACE }))
        .await
        .unwrap();
    assert_eq!(reply, json!({ "updated": 1 }));
    assert_eq!(item_prs(&rig, &item).len(), 2);

    // The user removes one: it stays removed.
    let reference = rig
        .snapshot(GIT_SPACE)
        .external_references
        .iter()
        .find(|r| r.external_id == "acme/ui#10")
        .unwrap()
        .id
        .clone();
    rig.mutate(json!({ "op": "removeWorkItemExternalReference", "referenceId": reference,
        "operationId": "rm-10" }))
        .await
        .unwrap();
    let reply = rig
        .call(methods::LINEAR_REFRESH_LINKED_ITEMS, json!({ "spaceId": GIT_SPACE }))
        .await
        .unwrap();
    assert_eq!(reply, json!({ "updated": 0 }));
    assert_eq!(item_prs(&rig, &item).len(), 1);

    // Linear off: nothing is asked for and nothing is added.
    rig.enable(GIT_SPACE, false).await;
    rig.mock.set_attachments(
        "ENG-2690",
        vec![json!({ "url": "https://github.com/acme/ui/pull/11", "sourceType": "github" })],
    );
    let asked = rig.mock.count("ZeronLinearIssueAttachments");
    let error = rig
        .linear_err(methods::LINEAR_REFRESH_LINKED_ITEMS, json!({ "spaceId": GIT_SPACE }))
        .await;
    assert_eq!(error.code, LinearErrorCode::DisabledForSpace);
    assert_eq!(rig.mock.count("ZeronLinearIssueAttachments"), asked);
    assert_eq!(item_prs(&rig, &item).len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn unreadable_attachments_are_logged_and_the_title_sync_still_runs() {
    let rig = rig().await;
    rig.mock.set_issue(issue_node("ENG-5", "Five", None, "started"));
    let (_dir, item) = git_space_item(&rig, "ENG-5").await;
    rig.mock.state.lock().unwrap().attachments_error = true;
    rig.mock.set_issue(issue_node("ENG-5", "Five v2", None, "started"));
    let reply = rig
        .call(methods::LINEAR_REFRESH_LINKED_ITEMS, json!({ "spaceId": GIT_SPACE }))
        .await
        .unwrap();
    assert_eq!(reply, json!({ "updated": 1 }), "the title still follows Linear");
    assert_eq!(rig.snapshot(GIT_SPACE).item(&item).unwrap().title, "ENG-5 · Five v2");
    assert!(item_prs(&rig, &item).is_empty());
    let text = String::from_utf8_lossy(&logs().0.lock().unwrap()).into_owned();
    assert!(
        text.contains("issue attachments could not be read"),
        "logged for a fix from a real run"
    );
}
