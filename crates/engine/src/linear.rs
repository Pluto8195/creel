//! Linear integration v1 (read-only; contract
//! `docs/plans/work-outline/linear-contract.md`).
//!
//! - [`LinearService`]: this device's key ([`credentials`]), the GraphQL
//!   client ([`client`], [`queries`]), a ~5 minute per-identifier issue
//!   cache, and coalescing of identical in-flight requests. Any 401 deletes
//!   the stored key. The key never leaves this module: not in replies, logs,
//!   agent payloads, or harness environments.
//! - [`sync`]: the linked-item sync (title/description of issue-linked items
//!   follow Linear until the user edits them), run every 15 minutes, on
//!   `LinearRefreshLinkedItems`, and when a Space turns Linear on.

pub mod assigned;
pub mod client;
pub mod credentials;
pub mod queries;
pub mod sync;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};

use crate::swr::SwrStore;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use mast_proto::linear::{
    LinearConnectionStatus, LinearError, LinearErrorCode, LinearIssueSummary, LinearKeySource,
    LinearProjectDetail, LinearProjectSummary, MAX_LIST_ISSUES, MAX_PROJECT_UPDATES,
};

pub use client::{LINEAR_GRAPHQL_URL, LinearClient, Sleeper, tokio_sleeper};
pub use credentials::{
    EnvKeySource, KeychainKeyStore, LinearKeyStore, MemoryKeyStore, default_store, no_env_source,
    process_env_source,
};

/// How long a fetched issue is served from the cache.
pub const ISSUE_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
/// How long a fetched list (my issues, my projects, a project's issues) is
/// served without asking Linear again; an explicit refresh always asks.
pub const LIST_CACHE_TTL: Duration = Duration::from_secs(60);
/// Most projects `LinearMyProjects` returns.
const MAX_PROJECTS: usize = 250;

/// Everything a [`LinearService`] talks to (swappable for tests).
#[derive(Clone)]
pub struct LinearConfig {
    pub endpoint: String,
    pub store: Arc<dyn LinearKeyStore>,
    pub env: EnvKeySource,
    pub sleeper: Sleeper,
    pub issue_ttl: Duration,
}

impl LinearConfig {
    /// Linear's API, the Keychain (or `ZERON_LINEAR_KEY_STORE=memory`),
    /// and `LINEAR_API_KEY`.
    pub fn production() -> Self {
        Self {
            endpoint: LINEAR_GRAPHQL_URL.into(),
            store: default_store(),
            env: process_env_source(),
            sleeper: tokio_sleeper(),
            issue_ttl: ISSUE_CACHE_TTL,
        }
    }

    /// A mock endpoint and an injected store; the environment is ignored.
    pub fn for_tests(endpoint: impl Into<String>, store: Arc<dyn LinearKeyStore>) -> Self {
        Self {
            endpoint: endpoint.into(),
            store,
            env: no_env_source(),
            sleeper: tokio_sleeper(),
            issue_ttl: ISSUE_CACHE_TTL,
        }
    }
}

struct Backend {
    client: LinearClient,
    store: Arc<dyn LinearKeyStore>,
    env: EnvKeySource,
    ttl: Duration,
}

type SharedCall = Shared<BoxFuture<'static, Result<Value, LinearError>>>;

struct Inner {
    backend: RwLock<Arc<Backend>>,
    /// `None` = not resolved yet; `Some(None)` = no key anywhere.
    key: Mutex<Option<Option<(String, LinearKeySource)>>>,
    /// The last successful `viewer` check.
    verified: Mutex<Option<LinearConnectionStatus>>,
    issues: Mutex<HashMap<String, (Instant, LinearIssueSummary)>>,
    /// Per-identifier attachments, same TTL as `issues`.
    attachments: Mutex<HashMap<String, (Instant, Vec<queries::IssueAttachment>)>>,
    inflight: Mutex<HashMap<String, SharedCall>>,
    /// Stale-while-revalidate stores for the panel's lists (persisted when a
    /// cache directory is set). Keys: `my`, `project:<id>`.
    issue_lists: SwrStore<Vec<LinearIssueSummary>, LinearError>,
    project_lists: SwrStore<Vec<LinearProjectSummary>, LinearError>,
    /// One project's detail per id (the canonical plan panel).
    project_details: SwrStore<LinearProjectDetail, LinearError>,
    /// Serializes key changes (set/clear/forget).
    key_change: tokio::sync::Mutex<()>,
}

/// Cheap to clone; one per engine.
#[derive(Clone)]
pub struct LinearService {
    inner: Arc<Inner>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn backend_of(config: LinearConfig) -> Arc<Backend> {
    Arc::new(Backend {
        client: LinearClient::new(config.endpoint, config.sleeper),
        store: config.store,
        env: config.env,
        ttl: config.issue_ttl,
    })
}

/// `ENG-123` (trimmed, uppercased) or `invalid`.
pub fn normalize_identifier(identifier: &str) -> Result<String, LinearError> {
    let id = identifier.trim().to_ascii_uppercase();
    let valid = !id.is_empty()
        && id.len() <= 64
        && id.contains('-')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(id)
    } else {
        Err(LinearError::new(
            LinearErrorCode::Invalid,
            "Not a Linear issue identifier",
        ))
    }
}

impl LinearService {
    pub fn new(config: LinearConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                backend: RwLock::new(backend_of(config)),
                key: Mutex::new(None),
                verified: Mutex::new(None),
                issues: Mutex::new(HashMap::new()),
                attachments: Mutex::new(HashMap::new()),
                inflight: Mutex::new(HashMap::new()),
                issue_lists: SwrStore::new(LIST_CACHE_TTL),
                project_lists: SwrStore::new(LIST_CACHE_TTL),
                project_details: SwrStore::new(LIST_CACHE_TTL),
                key_change: tokio::sync::Mutex::new(()),
            }),
        }
    }

    /// Swap endpoint/store/env (tests) and drop every cache.
    pub fn reconfigure(&self, config: LinearConfig) {
        *self
            .inner
            .backend
            .write()
            .unwrap_or_else(|e| e.into_inner()) = backend_of(config);
        self.forget_caches();
    }

    fn backend(&self) -> Arc<Backend> {
        self.inner
            .backend
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn forget_caches(&self) {
        *lock(&self.inner.key) = None;
        *lock(&self.inner.verified) = None;
        lock(&self.inner.issues).clear();
        lock(&self.inner.attachments).clear();
        self.inner.issue_lists.clear();
        self.inner.project_lists.clear();
        self.inner.project_details.clear();
    }

    /// Keep the list caches under `dir` so a cold start can show the last
    /// lists while a refresh runs.
    pub fn set_cache_dir(&self, dir: &std::path::Path) {
        self.inner
            .issue_lists
            .attach_file(dir.join("linear-issue-lists.json"));
        self.inner
            .project_lists
            .attach_file(dir.join("linear-project-lists.json"));
        self.inner
            .project_details
            .attach_file(dir.join("linear-project-details.json"));
    }

    /// The key and its source, reading the store once (off the async
    /// workers: the Keychain is a `security` process).
    async fn resolve_key(&self) -> Result<Option<(String, LinearKeySource)>, LinearError> {
        if let Some(known) = lock(&self.inner.key).clone() {
            return Ok(known);
        }
        let backend = self.backend();
        let resolved = tokio::task::spawn_blocking(move || {
            let stored = backend.store.read()?;
            Ok::<_, String>(match stored {
                Some(key) => Some((key, LinearKeySource::Keychain)),
                None => (backend.env)().map(|key| (key, LinearKeySource::Environment)),
            })
        })
        .await
        .map_err(|e| LinearError::new(LinearErrorCode::Network, e.to_string()))?
        .map_err(|message| LinearError::new(LinearErrorCode::MissingKey, message))?;
        *lock(&self.inner.key) = Some(resolved.clone());
        Ok(resolved)
    }

    /// Whether a key is available (reads the store at most once).
    pub async fn has_key(&self) -> bool {
        matches!(self.resolve_key().await, Ok(Some(_)))
    }

    /// One GraphQL call with the device key; identical in-flight calls share
    /// one request. A 401 deletes the stored key.
    async fn call(&self, query: &str, variables: Value) -> Result<Value, LinearError> {
        let (key, source) = self
            .resolve_key()
            .await?
            .ok_or_else(LinearError::missing_key)?;
        let mut hasher = Sha256::new();
        hasher.update(key.as_bytes());
        hasher.update([0]);
        hasher.update(query.as_bytes());
        hasher.update([0]);
        hasher.update(variables.to_string().as_bytes());
        let slot = format!("{:x}", hasher.finalize());
        let shared = {
            let mut inflight = lock(&self.inner.inflight);
            inflight
                .entry(slot.clone())
                .or_insert_with(|| {
                    let client = self.backend().client.clone();
                    let query = query.to_owned();
                    async move { client.graphql(&key, &query, variables).await }
                        .boxed()
                        .shared()
                })
                .clone()
        };
        let result = shared.await;
        lock(&self.inner.inflight).remove(&slot);
        if let Err(error) = &result
            && error.code == LinearErrorCode::Unauthorized
        {
            self.forget_key(source).await;
        }
        result
    }

    /// After a 401: delete a stored key (an environment key cannot be
    /// deleted, it is only reported) and drop the caches.
    async fn forget_key(&self, source: LinearKeySource) {
        let _guard = self.inner.key_change.lock().await;
        if source == LinearKeySource::Keychain {
            let backend = self.backend();
            let deleted = tokio::task::spawn_blocking(move || backend.store.delete()).await;
            if !matches!(deleted, Ok(Ok(()))) {
                tracing::warn!("linear: could not delete the rejected API key");
            }
        }
        self.forget_caches();
        tracing::info!("linear: API key rejected (401); stored key removed");
    }

    fn status_without_viewer(
        &self,
        key: Option<&(String, LinearKeySource)>,
    ) -> LinearConnectionStatus {
        LinearConnectionStatus {
            configured: key.is_some(),
            source: key.map_or(LinearKeySource::None, |(_, source)| *source),
            viewer: None,
            error: None,
        }
    }

    /// `LinearStatus`: the key's source and its verified user (the `viewer`
    /// check runs once per key unless `force`).
    pub async fn status(&self, force: bool) -> Result<LinearConnectionStatus, LinearError> {
        if !force && let Some(status) = lock(&self.inner.verified).clone() {
            return Ok(status);
        }
        let key = self.resolve_key().await?;
        let Some(key) = key else {
            return Ok(self.status_without_viewer(None));
        };
        match self.call(&queries::viewer(), json!({})).await {
            Ok(data) => {
                let status = LinearConnectionStatus {
                    viewer: queries::parse_viewer(&data),
                    ..self.status_without_viewer(Some(&key))
                };
                *lock(&self.inner.verified) = Some(status.clone());
                Ok(status)
            }
            Err(error) if error.code == LinearErrorCode::Unauthorized => {
                // The rejected key is gone; report what remains.
                let remaining = self.resolve_key().await.ok().flatten();
                Ok(LinearConnectionStatus {
                    error: Some(error.message),
                    ..self.status_without_viewer(remaining.as_ref())
                })
            }
            Err(error) => Ok(LinearConnectionStatus {
                error: Some(error.message),
                ..self.status_without_viewer(Some(&key))
            }),
        }
    }

    /// `LinearSetApiKey`: verify with `viewer`, store, read back.
    pub async fn set_api_key(&self, api_key: &str) -> Result<LinearConnectionStatus, LinearError> {
        let api_key = api_key.trim().to_owned();
        if api_key.is_empty() || api_key.chars().any(char::is_whitespace) {
            return Err(LinearError::new(
                LinearErrorCode::Invalid,
                "Enter a Linear personal API key",
            ));
        }
        let backend = self.backend();
        let data = backend
            .client
            .graphql(&api_key, &queries::viewer(), json!({}))
            .await?;
        let viewer = queries::parse_viewer(&data).ok_or_else(|| {
            LinearError::new(
                LinearErrorCode::Unauthorized,
                "Linear did not accept the key",
            )
        })?;
        let _guard = self.inner.key_change.lock().await;
        let stored_ok = {
            let backend = backend.clone();
            let key = api_key.clone();
            tokio::task::spawn_blocking(move || {
                backend.store.write(&key)?;
                Ok::<bool, String>(backend.store.read()?.as_deref() == Some(key.as_str()))
            })
            .await
            .map_err(|e| LinearError::new(LinearErrorCode::Network, e.to_string()))?
            .map_err(|message| LinearError::new(LinearErrorCode::Invalid, message))?
        };
        if !stored_ok {
            return Err(LinearError::new(
                LinearErrorCode::Invalid,
                "The Keychain did not keep the key",
            ));
        }
        self.forget_caches();
        let key = (api_key, LinearKeySource::Keychain);
        *lock(&self.inner.key) = Some(Some(key.clone()));
        let status = LinearConnectionStatus {
            viewer: Some(viewer),
            ..self.status_without_viewer(Some(&key))
        };
        *lock(&self.inner.verified) = Some(status.clone());
        Ok(status)
    }

    /// `LinearClearApiKey`: delete the stored key; an environment key stays.
    pub async fn clear_api_key(&self) -> Result<LinearConnectionStatus, LinearError> {
        let _guard = self.inner.key_change.lock().await;
        let backend = self.backend();
        tokio::task::spawn_blocking(move || backend.store.delete())
            .await
            .map_err(|e| LinearError::new(LinearErrorCode::Network, e.to_string()))?
            .map_err(|message| LinearError::new(LinearErrorCode::Invalid, message))?;
        self.forget_caches();
        drop(_guard);
        let key = self.resolve_key().await?;
        Ok(self.status_without_viewer(key.as_ref()))
    }

    /// A cached issue, if fresh (no network).
    pub fn cached_issue(&self, identifier: &str) -> Option<LinearIssueSummary> {
        let id = normalize_identifier(identifier).ok()?;
        let ttl = self.backend().ttl;
        lock(&self.inner.issues)
            .get(&id)
            .filter(|(at, _)| at.elapsed() < ttl)
            .map(|(_, issue)| issue.clone())
    }

    /// `LinearIssue`: cached per identifier unless `force`.
    pub async fn issue(
        &self,
        identifier: &str,
        force: bool,
    ) -> Result<LinearIssueSummary, LinearError> {
        let id = normalize_identifier(identifier)?;
        if !force && let Some(issue) = self.cached_issue(&id) {
            return Ok(issue);
        }
        let data = self.call(&queries::issue(), json!({ "id": id })).await?;
        let issue = data
            .get("issue")
            .filter(|v| !v.is_null())
            .and_then(queries::issue_summary)
            .ok_or_else(|| {
                LinearError::new(LinearErrorCode::NotFound, format!("{id} was not found"))
            })?;
        let mut cache = lock(&self.inner.issues);
        cache.insert(id.clone(), (Instant::now(), issue.clone()));
        if issue.identifier != id {
            // A moved issue answers under its new identifier.
            cache.insert(issue.identifier.clone(), (Instant::now(), issue.clone()));
        }
        Ok(issue)
    }

    /// The issue's attachments (GitHub PRs among them), cached like issues
    /// unless `force`. A response without the expected shape is `invalid`
    /// (the query is isolated from [`Self::issue`] for exactly that case).
    pub async fn issue_attachments(
        &self,
        identifier: &str,
        force: bool,
    ) -> Result<Vec<queries::IssueAttachment>, LinearError> {
        let id = normalize_identifier(identifier)?;
        let ttl = self.backend().ttl;
        if !force
            && let Some((_, cached)) = lock(&self.inner.attachments)
                .get(&id)
                .filter(|(at, _)| at.elapsed() < ttl)
        {
            return Ok(cached.clone());
        }
        let data = self
            .call(&queries::issue_attachments(), json!({ "id": id }))
            .await?;
        let attachments = queries::parse_issue_attachments(&data).ok_or_else(|| {
            LinearError::new(
                LinearErrorCode::Invalid,
                "Linear answered issue attachments in an unexpected shape",
            )
        })?;
        lock(&self.inner.attachments).insert(id, (Instant::now(), attachments.clone()));
        Ok(attachments)
    }

    /// Follow a connection at `path` (JSON pointer into `data`) page by page
    /// until it ends or `cap` nodes were read.
    async fn paginate(
        &self,
        query: &str,
        extra: Value,
        path: &str,
        cap: usize,
    ) -> Result<(Vec<Value>, bool), LinearError> {
        let mut nodes = Vec::new();
        let mut after: Option<String> = None;
        let mut found = false;
        loop {
            let data = self
                .call(
                    query,
                    queries::page_variables(extra.clone(), after.as_deref()),
                )
                .await?;
            let connection = data.pointer(path);
            found |= connection.is_some_and(|c| !c.is_null());
            let (page, next) = queries::connection_page(connection);
            nodes.extend(page);
            match next {
                Some(cursor) if nodes.len() < cap => after = Some(cursor),
                _ => break,
            }
        }
        nodes.truncate(cap);
        Ok((nodes, found))
    }

    fn cache_issues(&self, issues: &[LinearIssueSummary]) {
        let now = Instant::now();
        let mut cache = lock(&self.inner.issues);
        for issue in issues {
            cache.insert(issue.identifier.clone(), (now, issue.clone()));
        }
    }

    /// `LinearMyIssues`: served from the list cache inside [`LIST_CACHE_TTL`]
    /// unless `force`; `cached_only` answers from whatever is stored.
    pub async fn my_issues(
        &self,
        force: bool,
        cached_only: bool,
    ) -> Result<Vec<LinearIssueSummary>, LinearError> {
        let this = self.clone();
        self.issue_list("my", force, cached_only, move || async move {
            let (nodes, _) = this
                .paginate(
                    &queries::my_issues(),
                    json!({}),
                    "/viewer/assignedIssues",
                    MAX_LIST_ISSUES,
                )
                .await?;
            Ok(open_issues(&nodes))
        })
        .await
    }

    /// The viewer's not-started, project-less issues (the linked Linear
    /// item). Same SWR store as [`Self::my_issues`]; the response is also
    /// filtered locally so a lenient server cannot widen the set.
    pub async fn assigned_unstarted_issues(
        &self,
        force: bool,
    ) -> Result<Vec<LinearIssueSummary>, LinearError> {
        let this = self.clone();
        self.issue_list("assigned-unstarted", force, false, move || async move {
            let (nodes, _) = this
                .paginate(
                    &queries::assigned_unstarted_issues(),
                    json!({}),
                    "/viewer/assignedIssues",
                    MAX_LIST_ISSUES,
                )
                .await?;
            Ok(nodes
                .iter()
                .filter_map(queries::issue_summary)
                .filter(|i| i.state.state_type.is_unstarted() && i.project.is_none())
                .collect())
        })
        .await
    }

    /// `LinearMyProjects`; same caching as [`Self::my_issues`].
    pub async fn my_projects(
        &self,
        force: bool,
        cached_only: bool,
    ) -> Result<Vec<LinearProjectSummary>, LinearError> {
        let store = &self.inner.project_lists;
        if cached_only {
            return store
                .peek("my")
                .map(|entry| entry.value)
                .ok_or_else(not_cached);
        }
        let this = self.clone();
        let entry = store
            .get("my", force, move || {
                async move {
                    let (nodes, _) = this
                        .paginate(
                            &queries::my_projects(),
                            json!({}),
                            "/projects",
                            MAX_PROJECTS,
                        )
                        .await?;
                    Ok(nodes
                        .iter()
                        .filter_map(queries::project_summary)
                        .filter(|p| !queries::project_is_closed(p.state.as_deref()))
                        .collect())
                }
                .boxed()
            })
            .await?;
        Ok(entry.value)
    }

    /// `LinearProjectDetail`; same caching as [`Self::my_issues`], per project.
    pub async fn project_detail(
        &self,
        project_id: &str,
        force: bool,
        cached_only: bool,
    ) -> Result<LinearProjectDetail, LinearError> {
        let project_id = project_id.trim().to_owned();
        if project_id.is_empty() {
            return Err(LinearError::new(
                LinearErrorCode::Invalid,
                "projectId is required",
            ));
        }
        let store = &self.inner.project_details;
        if cached_only {
            return store
                .peek(&project_id)
                .map(|entry| entry.value)
                .ok_or_else(not_cached);
        }
        let this = self.clone();
        let id = project_id.clone();
        let entry = store
            .get(&project_id, force, move || {
                async move {
                    let data = this
                        .call(
                            &queries::project_detail(),
                            json!({ "id": id, "updates": MAX_PROJECT_UPDATES }),
                        )
                        .await?;
                    queries::parse_project_detail(&data).ok_or_else(|| {
                        LinearError::new(
                            LinearErrorCode::NotFound,
                            "That Linear project was not found",
                        )
                    })
                }
                .boxed()
            })
            .await?;
        Ok(entry.value)
    }

    /// `LinearProjectIssues`; same caching as [`Self::my_issues`].
    pub async fn project_issues(
        &self,
        project_id: &str,
        force: bool,
        cached_only: bool,
    ) -> Result<Vec<LinearIssueSummary>, LinearError> {
        let project_id = project_id.trim().to_owned();
        if project_id.is_empty() {
            return Err(LinearError::new(
                LinearErrorCode::Invalid,
                "projectId is required",
            ));
        }
        let this = self.clone();
        let id = project_id.clone();
        self.issue_list(
            &format!("project:{project_id}"),
            force,
            cached_only,
            move || async move {
                let (nodes, found) = this
                    .paginate(
                        &queries::project_issues(),
                        json!({ "id": id }),
                        "/project/issues",
                        MAX_LIST_ISSUES,
                    )
                    .await?;
                if !found {
                    return Err(LinearError::new(
                        LinearErrorCode::NotFound,
                        "That Linear project was not found",
                    ));
                }
                Ok(open_issues(&nodes))
            },
        )
        .await
    }

    async fn issue_list<F, Fut>(
        &self,
        key: &str,
        force: bool,
        cached_only: bool,
        fetch: F,
    ) -> Result<Vec<LinearIssueSummary>, LinearError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Vec<LinearIssueSummary>, LinearError>>
            + Send
            + 'static,
    {
        let store = &self.inner.issue_lists;
        if cached_only {
            return store
                .peek(key)
                .map(|entry| entry.value)
                .ok_or_else(not_cached);
        }
        let entry = store.get(key, force, || fetch().boxed()).await?;
        // Remember the issues so chips and lookups see them, cached or not.
        self.cache_issues(&entry.value);
        Ok(entry.value)
    }
}

fn not_cached() -> LinearError {
    LinearError::new(LinearErrorCode::NotFound, "nothing cached yet")
}

fn open_issues(nodes: &[Value]) -> Vec<LinearIssueSummary> {
    nodes
        .iter()
        .filter_map(queries::issue_summary)
        .filter(|i| !i.state.state_type.is_closed())
        .collect()
}

impl Default for LinearService {
    fn default() -> Self {
        Self::new(LinearConfig::production())
    }
}
