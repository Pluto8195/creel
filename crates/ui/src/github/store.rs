//! GitHub state shared by every PR surface, held in `AppState::github`: a
//! per-PR status cache (chips, Details, the panel, ⇧⌘P) and the last
//! "Sync now" result per Space. Fetches go through
//! `AppState::work_outline_actions()`, so tests answer them with scripted
//! replies. Updates notify `AppState`, which redraws the observers.
use std::collections::HashMap;
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

#[derive(Debug, Clone)]
pub(crate) enum PrEntry {
    Loading,
    Ready {
        status: Box<PullRequestStatusSummary>,
        at: Instant,
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
    /// The last Sync now per Space.
    pub sync_results: HashMap<String, SyncResult>,
}

impl GithubStore {
    pub(crate) fn status(&self, pr: &PullRequestRef) -> Option<&PrEntry> {
        self.statuses.get(&pr.key())
    }

    pub(crate) fn ready(&self, pr: &PullRequestRef) -> Option<&PullRequestStatusSummary> {
        self.status(pr).and_then(PrEntry::ready)
    }

    /// Remember statuses another call already fetched (panel lists).
    pub(crate) fn remember<'a>(
        &mut self,
        statuses: impl IntoIterator<Item = &'a PullRequestStatusSummary>,
    ) {
        let now = Instant::now();
        for status in statuses {
            self.statuses.insert(
                status.pr_ref.key(),
                PrEntry::Ready {
                    status: Box::new(status.clone()),
                    at: now,
                },
            );
        }
    }

    fn needs_fetch(&self, pr: &PullRequestRef, now: Instant) -> bool {
        match self.status(pr) {
            None => true,
            Some(PrEntry::Loading) => false,
            Some(PrEntry::Ready { at, .. }) => now.duration_since(*at) >= STATUS_TTL,
            Some(PrEntry::Failed { at, .. }) => now.duration_since(*at) >= STATUS_RETRY,
        }
    }
}

/// Fetch `pr`'s status when it is not cached (or `force`: `forceRefresh`).
/// Marks it loading without notifying, so it is safe to call while
/// rendering.
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
        let loading = matches!(store.status(pr), Some(PrEntry::Loading));
        if loading || (!force && !store.needs_fetch(pr, now)) {
            return None;
        }
        state.github.statuses.insert(key.clone(), PrEntry::Loading);
        Some(state.work_outline_actions().pr_status(pr, force))
    }) else {
        return;
    };
    let state = state.clone();
    cx.spawn(async move |cx| {
        let result = task.await;
        state.update(cx, |state, cx| {
            let at = Instant::now();
            let entry = match result {
                Ok(status) => PrEntry::Ready {
                    status: Box::new(status),
                    at,
                },
                Err(error) => PrEntry::Failed { error, at },
            };
            state.github.statuses.insert(key, entry);
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
        assert!(!store.needs_fetch(&PullRequestRef::new("acme", "app", 3), Instant::now()));
        assert!(store.needs_fetch(&PullRequestRef::new("acme", "app", 4), Instant::now()));
    }
}
