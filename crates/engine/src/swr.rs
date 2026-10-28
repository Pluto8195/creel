//! A small stale-while-revalidate store: the last good value per key, kept in
//! memory and (when a file is attached) on disk, with one shared in-flight
//! fetch per key. The PR page and the Linear lists use it so a reopened view
//! or a cold start has data to show while a refresh runs.
//!
//! Freshness is wall-clock (`fetched_at`), so it survives a restart. A fresh
//! value (younger than the TTL) answers without fetching unless `force`;
//! callers that want the stale value only use [`SwrStore::peek`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::future::{BoxFuture, FutureExt, Shared};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// A stored value and when it was fetched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry<V> {
    pub fetched_at: DateTime<Utc>,
    pub value: V,
}

type Flight<V, E> = Shared<BoxFuture<'static, Result<Entry<V>, E>>>;

struct State<V, E> {
    entries: HashMap<String, Entry<V>>,
    inflight: HashMap<String, (u64, Flight<V, E>)>,
    next_id: u64,
    path: Option<PathBuf>,
}

/// Cheap to clone; clones share state.
pub struct SwrStore<V, E> {
    ttl: Duration,
    state: Arc<Mutex<State<V, E>>>,
}

impl<V, E> Clone for SwrStore<V, E> {
    fn clone(&self) -> Self {
        Self {
            ttl: self.ttl,
            state: self.state.clone(),
        }
    }
}

impl<V, E> SwrStore<V, E>
where
    V: Clone + Serialize + DeserializeOwned + Send + Sync + 'static,
    E: Clone + Send + Sync + 'static,
{
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            state: Arc::new(Mutex::new(State {
                entries: HashMap::new(),
                inflight: HashMap::new(),
                next_id: 0,
                path: None,
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State<V, E>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Persist to `path` from now on, first loading what it holds (entries
    /// already in memory win). A missing or unreadable file is ignored.
    pub fn attach_file(&self, path: PathBuf) {
        let loaded: HashMap<String, Entry<V>> = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let mut state = self.lock();
        for (key, entry) in loaded {
            state.entries.entry(key).or_insert(entry);
        }
        state.path = Some(path);
    }

    /// The stored entry, however old.
    pub fn peek(&self, key: &str) -> Option<Entry<V>> {
        self.lock().entries.get(key).cloned()
    }

    fn is_fresh(&self, entry: &Entry<V>) -> bool {
        let age = Utc::now().signed_duration_since(entry.fetched_at);
        age >= chrono::Duration::zero() && age.to_std().is_ok_and(|age| age < self.ttl)
    }

    /// Drop every entry (memory and file).
    pub fn clear(&self) {
        let snapshot = {
            let mut state = self.lock();
            state.entries.clear();
            persist_snapshot(&state)
        };
        write_snapshot(snapshot);
    }

    /// The entry for `key`: stored if fresh (unless `force`), else the result
    /// of `fetch`, shared with any concurrent caller for the same key. A
    /// failed fetch stores nothing.
    pub async fn get<F>(&self, key: &str, force: bool, fetch: F) -> Result<Entry<V>, E>
    where
        F: FnOnce() -> BoxFuture<'static, Result<V, E>>,
    {
        let flight = {
            let mut state = self.lock();
            if !force {
                if let Some(entry) = state.entries.get(key) {
                    if self.is_fresh(entry) {
                        return Ok(entry.clone());
                    }
                }
            }
            match state.inflight.get(key) {
                Some((_, flight)) => flight.clone(),
                None => {
                    state.next_id += 1;
                    let id = state.next_id;
                    let store = self.clone();
                    let key_owned = key.to_owned();
                    let work = fetch();
                    let flight: Flight<V, E> = async move {
                        let result = work.await;
                        store.finish(&key_owned, id, result)
                    }
                    .boxed()
                    .shared();
                    state.inflight.insert(key.to_owned(), (id, flight.clone()));
                    flight
                }
            }
        };
        flight.await
    }

    fn finish(&self, key: &str, id: u64, result: Result<V, E>) -> Result<Entry<V>, E> {
        let mut state = self.lock();
        if state.inflight.get(key).is_some_and(|(i, _)| *i == id) {
            state.inflight.remove(key);
        }
        let value = result?;
        let entry = Entry {
            fetched_at: Utc::now(),
            value,
        };
        state.entries.insert(key.to_owned(), entry.clone());
        let snapshot = persist_snapshot(&state);
        drop(state);
        write_snapshot(snapshot);
        Ok(entry)
    }
}

type Snapshot = Option<(PathBuf, Result<Vec<u8>, String>)>;

fn persist_snapshot<V: Serialize, E>(state: &State<V, E>) -> Snapshot {
    let path = state.path.clone()?;
    let bytes = serde_json::to_vec(&state.entries).map_err(|e| e.to_string());
    Some((path, bytes))
}

/// Write off the async worker when a runtime is present; atomic via rename.
fn write_snapshot(snapshot: Snapshot) {
    let Some((path, bytes)) = snapshot else {
        return;
    };
    let write = move || {
        let Ok(bytes) = bytes else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn_blocking(write);
        }
        Err(_) => write(),
    }
}

#[cfg(test)]
mod tests;
