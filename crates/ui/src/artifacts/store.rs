//! The read side of planning artifacts: the latest `ArtifactsSnapshot` per
//! Space plus its load state. `AppState` owns one [`ArtifactStore`] and feeds
//! it from `WatchArtifacts` (`state.rs` `reconcile_artifact_watches`), which
//! watches exactly the Spaces the Work Outline reconciler watches. Frames
//! replace a Space's snapshot wholesale. Views join artifacts to the outline
//! by `workItemId` / `chatId`.
use std::collections::HashMap;
use std::sync::Arc;

use zeron_proto::{ArtifactView, ArtifactsSnapshot};

/// Per-Space load state. `Unsupported` is a host that answers `unknown
/// method` to `WatchArtifacts` (the engine branch has not landed there yet).
#[derive(Debug, Clone)]
pub(crate) enum ArtifactsLoad {
    Loading,
    Ready(Arc<ArtifactsSnapshot>),
    Unsupported,
    Failed(String),
}

impl ArtifactsLoad {
    pub(crate) fn snapshot(&self) -> Option<&Arc<ArtifactsSnapshot>> {
        match self {
            Self::Ready(snapshot) => Some(snapshot),
            _ => None,
        }
    }

    /// The load state a failed watch maps to: `unknown method` is an
    /// unsupported host; everything else is a retryable failure.
    pub(crate) fn from_watch_error(message: &str) -> Self {
        if message.to_ascii_lowercase().contains("unknown method") {
            Self::Unsupported
        } else {
            Self::Failed(message.to_owned())
        }
    }

    fn same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Loading, Self::Loading) | (Self::Unsupported, Self::Unsupported) => true,
            // Frames differ only by their per-stream counter: not a change.
            (Self::Ready(a), Self::Ready(b)) => {
                Arc::ptr_eq(a, b) || (a.space_id == b.space_id && a.artifacts == b.artifacts)
            }
            (Self::Failed(a), Self::Failed(b)) => a == b,
            _ => false,
        }
    }
}

/// Shown wherever an artifact RPC answers `unknown method`.
pub(crate) const UNSUPPORTED_TEXT: &str = "Artifacts aren\u{2019}t available on this host yet";

/// Latest snapshot and load state per Space id.
#[derive(Debug, Default)]
pub(crate) struct ArtifactStore {
    loads: HashMap<String, ArtifactsLoad>,
}

impl ArtifactStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn load(&self, space_id: &str) -> ArtifactsLoad {
        self.loads
            .get(space_id)
            .cloned()
            .unwrap_or(ArtifactsLoad::Loading)
    }

    pub(crate) fn snapshot(&self, space_id: &str) -> Option<&Arc<ArtifactsSnapshot>> {
        self.loads.get(space_id).and_then(ArtifactsLoad::snapshot)
    }

    /// Replace a Space's state; whether anything changed. A transient
    /// failure keeps the last good snapshot (the UI stays stale, not blank).
    pub(crate) fn set(&mut self, space_id: &str, load: ArtifactsLoad) -> bool {
        if matches!(load, ArtifactsLoad::Failed(_)) && self.snapshot(space_id).is_some() {
            return false;
        }
        if self
            .loads
            .get(space_id)
            .is_some_and(|old| old.same_as(&load))
        {
            return false;
        }
        self.loads.insert(space_id.to_owned(), load);
        true
    }

    /// Drop Spaces no longer watched; whether anything was dropped.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) -> bool {
        let before = self.loads.len();
        self.loads.retain(|space_id, _| keep(space_id));
        before != self.loads.len()
    }

    pub(crate) fn space_ids(&self) -> impl Iterator<Item = &str> {
        self.loads.keys().map(String::as_str)
    }

    fn all(&self) -> impl Iterator<Item = &ArtifactView> {
        self.loads
            .values()
            .filter_map(ArtifactsLoad::snapshot)
            .flat_map(|snapshot| snapshot.artifacts.iter())
    }

    /// An artifact by id, in any watched Space.
    pub(crate) fn artifact(&self, artifact_id: &str) -> Option<&ArtifactView> {
        self.all().find(|artifact| artifact.id == artifact_id)
    }

    /// A live artifact whose stored, origin, or preview path is `path`
    /// (absolute, or relative to `cwd` — the linking chat's checkout).
    /// Artifacts of `space_id` win over other Spaces'.
    pub(crate) fn artifact_by_path(
        &self,
        path: &str,
        cwd: Option<&str>,
        space_id: Option<&str>,
    ) -> Option<&ArtifactView> {
        let candidates = candidate_paths(path, cwd);
        let matches = |artifact: &&ArtifactView| {
            artifact.archived_at.is_none()
                && [
                    Some(artifact.stored_path.as_str()),
                    artifact.origin_path.as_deref(),
                    artifact.preview_path.as_deref(),
                ]
                .into_iter()
                .flatten()
                .any(|known| candidates.iter().any(|c| same_path(known, c)))
        };
        space_id
            .and_then(|space| self.snapshot(space))
            .and_then(|snapshot| snapshot.artifacts.iter().find(matches))
            .or_else(|| self.all().find(matches))
    }

    /// The Details list of one work item: the pinned plan first, then the
    /// rest newest first (`updatedAt`), archived artifacts excluded.
    pub(crate) fn for_work_item(&self, space_id: &str, work_item_id: &str) -> Vec<&ArtifactView> {
        let Some(snapshot) = self.snapshot(space_id) else {
            return Vec::new();
        };
        details_order(snapshot.for_work_item(work_item_id))
    }

    /// Live artifacts of a work item (the Outline row's count).
    pub(crate) fn count_for_work_item(&self, space_id: &str, work_item_id: &str) -> usize {
        self.snapshot(space_id)
            .map(|snapshot| snapshot.for_work_item(work_item_id).len())
            .unwrap_or(0)
    }

    /// The pinned plan of a work item.
    pub(crate) fn plan_of(&self, space_id: &str, work_item_id: &str) -> Option<&ArtifactView> {
        self.snapshot(space_id)?.plan_of(work_item_id)
    }
}

/// Pinned plan first, then `updatedAt` descending, then title for a stable
/// order. Pure.
pub(crate) fn details_order(mut artifacts: Vec<&ArtifactView>) -> Vec<&ArtifactView> {
    artifacts.sort_by(|a, b| {
        b.pinned_as_plan
            .cmp(&a.pinned_as_plan)
            .then(b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.id.cmp(&b.id))
    });
    artifacts
}

fn candidate_paths(path: &str, cwd: Option<&str>) -> Vec<String> {
    let path = path.trim();
    let mut out = vec![path.to_owned()];
    if !path.starts_with('/')
        && let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty())
    {
        out.push(format!("{}/{}", cwd.trim_end_matches('/'), path));
    }
    out
}

/// Lexical path equality: `.` segments and repeated or trailing slashes do
/// not matter; `..` pops a segment.
fn same_path(a: &str, b: &str) -> bool {
    normalize(a) == normalize(b)
}

fn normalize(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}
