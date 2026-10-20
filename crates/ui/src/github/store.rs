//! GitHub state shared by every PR surface, held in `AppState::github`: a
//! per-PR status cache (chips, Details, the panel, ⇧⌘P) and the last
//! "Sync now" result per Space. Fetches go through
//! `AppState::work_outline_actions()`, so tests answer them with scripted
//! replies. Updates notify `AppState`, which redraws the observers.
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::{App, Entity};
use zeron_proto::{PrReviewSyncNowReply, PullRequestRef, PullRequestStatusSummary};

use super::client::GithubUiError;
use crate::state::AppState;

/// How long a fetched PR status is shown without asking the host again
/// (the host caches for about two minutes).
pub(crate) const STATUS_TTL: Duration = Duration::from_secs(2 * 60);
/// How long a failed fetch waits before the next automatic try.
pub(crate) const STATUS_RETRY: Duration = Duration::from_secs(60);

/// What a PR chip shows. `Loading` only stands for "nothing to show yet":
/// a refresh of an entry that already has data keeps showing that data
/// (`GithubStore::in_flight` tracks the fetch), so a pull that returns the
/// same data never changes the rendered text.
#[derive(Debug, Clone)]
pub(crate) enum PrEntry {
    Loading,
    Ready {
        status: Box<PullRequestStatusSummary>,
        at: Instant,
        /// From a list's search hit (no CI or review detail): shown until
        /// the real status arrives, never written over a full one.
        partial: bool,
    },
    Failed {
        error: GithubUiError,
        at: Instant,
    },
}

impl PrEntry {
    pub(crate) fn ready(&self) -> Option<&PullRequestStatusSummary> {
        match self {
            Self::Ready { status, .. } => Some(status),
            _ => None,
        }
    }
}

/// The outcome of the last Sync now of a Space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SyncResult {
    Running,
    Done(PrReviewSyncNowReply),
    Failed(GithubUiError),
}

impl SyncResult {
    /// The Space-root "last result" line.
    pub(crate) fn line(&self) -> String {
        match self {
            Self::Running => "Syncing\u{2026}".to_string(),
            Self::Done(reply) => {
                if reply.created == 0 && reply.updated == 0 && reply.resolved == 0 {
                    "Up to date".to_string()
                } else {
                    format!(
                        "Synced: {} created \u{00b7} {} updated \u{00b7} {} resolved",
                        reply.created, reply.updated, reply.resolved
                    )
                }
            }
            Self::Failed(error) => error.user_text(),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct GithubStore {
    statuses: HashMap<String, PrEntry>,
    /// Keys with a fetch under way.
    in_flight: HashSet<String>,
    /// The last Sync now per Space.
    pub sync_results: HashMap<String, SyncResult>,
}

impl GithubStore {
    pub(crate) fn status(&self, pr: &PullRequestRef) -> Option<&PrEntry> {
        self.statuses.get(&pr.key())
    }

    #[cfg(test)]
    pub(crate) fn ready(&self, pr: &PullRequestRef) -> Option<&PullRequestStatusSummary> {
        self.status(pr).and_then(PrEntry::ready)
    }

    /// Whether a fetch of `pr` is under way (the Details "Refreshing…").
    pub(crate) fn is_loading(&self, pr: &PullRequestRef) -> bool {
        self.in_flight.contains(&pr.key()) || matches!(self.status(pr), Some(PrEntry::Loading))
    }

    /// Remember statuses another call already fetched (panel lists). A list
    /// row can lack what a full fetch has (CI, review state), so it only
    /// fills a gap or replaces another partial row; the chip then fetches
    /// the full status once.
    pub(crate) fn remember<'a>(
        &mut self,
        statuses: impl IntoIterator<Item = &'a PullRequestStatusSummary>,
    ) {
        // Due for a full fetch right away (`needs_fetch`).
        let now = Instant::now();
        let due = now.checked_sub(STATUS_RETRY).unwrap_or(now);
        for status in statuses {
            let key = status.pr_ref.key();
            if matches!(
                self.statuses.get(&key),
                Some(PrEntry::Ready { partial: false, .. })
            ) {
                continue;
            }
            self.statuses.insert(
                key,
                PrEntry::Ready {
                    status: Box::new(status.clone()),
                    at: due,
                    partial: true,
                },
            );
        }
    }

    fn needs_fetch(&self, pr: &PullRequestRef, now: Instant) -> bool {
        match self.status(pr) {
            None => true,
            Some(PrEntry::Loading) => false,
            Some(PrEntry::Ready {
                at, partial: true, ..
            }) => now.duration_since(*at) >= STATUS_RETRY,
            Some(PrEntry::Ready { at, .. }) => now.duration_since(*at) >= STATUS_TTL,
            Some(PrEntry::Failed { at, .. }) => now.duration_since(*at) >= STATUS_RETRY,
        }
    }
}

/// Fetch `pr`'s status when it is not cached (or `force`: `forceRefresh`).
/// Marks it loading without notifying, so it is safe to call while
/// rendering. An entry that already has data keeps it during the fetch, and
/// keeps it (retrying after `STATUS_RETRY`) when the fetch fails.
pub(crate) fn request_status(
    state: &Entity<AppState>,
    pr: &PullRequestRef,
    force: bool,
    cx: &mut App,
) {
    let key = pr.key();
    let now = Instant::now();
    let Some(task) = state.update(cx, |state, _| {
        let store = &state.github;
        if store.is_loading(pr) || (!force && !store.needs_fetch(pr, now)) {
            return None;
        }
        let store = &mut state.github;
        store.in_flight.insert(key.clone());
        if store.status(pr).is_none() {
            store.statuses.insert(key.clone(), PrEntry::Loading);
        }
        Some(state.work_outline_actions().pr_status(pr, force))
    }) else {
        return;
    };
    let state = state.clone();
    cx.spawn(async move |cx| {
        let result = task.await;
        state.update(cx, |state, cx| {
            let at = Instant::now();
            let store = &mut state.github;
            store.in_flight.remove(&key);
            let entry = match result {
                Ok(status) => PrEntry::Ready {
                    status: Box::new(status),
                    at,
                    partial: false,
                },
                Err(error) => match store.statuses.remove(&key) {
                    // Keep showing the last good status; the next try waits
                    // `STATUS_RETRY`.
                    Some(PrEntry::Ready {
                        status, partial, ..
                    }) => PrEntry::Ready {
                        status,
                        at: if partial {
                            at
                        } else {
                            at.checked_sub(STATUS_TTL - STATUS_RETRY).unwrap_or(at)
                        },
                        partial,
                    },
                    _ => PrEntry::Failed { error, at },
                },
            };
            store.statuses.insert(key, entry);
            cx.notify();
        });
    })
    .detach();
}

/// `PrReviewSyncNow` for a Space; the result lands in
/// [`GithubStore::sync_results`]. A run already in flight is not repeated.
pub(crate) fn sync_now(state: &Entity<AppState>, space_id: &str, cx: &mut App) {
    let Some(task) = state.update(cx, |state, cx| {
        if matches!(
            state.github.sync_results.get(space_id),
            Some(SyncResult::Running)
        ) {
            return None;
        }
        state
            .github
            .sync_results
            .insert(space_id.to_string(), SyncResult::Running);
        cx.notify();
        Some(state.work_outline_actions().pr_review_sync_now(space_id))
    }) else {
        return;
    };
    let state = state.clone();
    let space_id = space_id.to_string();
    cx.spawn(async move |cx| {
        let result = task.await;
        state.update(cx, |state, cx| {
            let result = match result {
                Ok(reply) => SyncResult::Done(reply),
                Err(error) => SyncResult::Failed(error),
            };
            state.github.sync_results.insert(space_id, result);
            cx.notify();
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_lines_read_naturally() {
        assert_eq!(
            SyncResult::Done(PrReviewSyncNowReply::default()).line(),
            "Up to date"
        );
        assert_eq!(
            SyncResult::Done(PrReviewSyncNowReply {
                created: 2,
                updated: 1,
                resolved: 0
            })
            .line(),
            "Synced: 2 created \u{00b7} 1 updated \u{00b7} 0 resolved"
        );
    }

    #[test]
    fn remembered_statuses_are_keyed_case_insensitively() {
        let mut store = GithubStore::default();
        store.remember([&super::super::fixture::summary("App", 3, "Fix")]);
        assert!(
            store
                .ready(&PullRequestRef::new("ACME", "app", 3))
                .is_some()
        );
        // A list row is shown, but due for a full fetch.
        assert!(store.needs_fetch(&PullRequestRef::new("acme", "app", 3), Instant::now()));
        assert!(store.needs_fetch(&PullRequestRef::new("acme", "app", 4), Instant::now()));
    }
}
