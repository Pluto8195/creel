//! PR review sync v1, client side (contract: `zeron_proto::pr_review_sync`,
//! user docs: `docs/pr-review.md` "PR review sync").
//!
//! - Per Space the user picks ONE work item as the sync destination
//!   (`setPrReviewSyncDestination`, Space-root details or the item's menu).
//!   The host then keeps one child per PR that requests the user's review,
//!   each linked with a `github` / `pullRequest` reference, plus one
//!   system-created Required Action per review round. Nothing here writes
//!   those items: the UI only shows them.
//! - Every item with a `github` / `pullRequest` reference shows a live PR
//!   chip (`PrStatus`, cached in [`store::GithubStore`]), a "Pull request"
//!   section in Details, and starts a code review (`StartPrReview` with its
//!   `workItemId`) on ⌘↩.
//! - The Overview's left column gets a GitHub section ([`panel`]): Review
//!   requested (`PrReviewRequested`) and My open PRs (`PrMyOpen`).
//! - A host that predates the RPCs answers `UnknownMethod`: every surface
//!   shows a calm "not available on this host" state.
//!
//! Files: [`client`] (typed RPCs + error mapping), [`store`] (status cache
//! and last sync results in `AppState`), [`chip`] (the PR chip), [`prefs`]
//! (panel section folds), [`panel`] (the left-column section), [`menu`]
//! (Outline right-click entries). Outline / details / Overview / Shell hooks
//! live in `work_outline/pr_sync_outline.rs`, `work_outline/details_pr_sync.rs`,
//! `overview/github_host.rs`, and `shell/github_ui.rs`.

pub(crate) mod chip;
pub(crate) mod client;
pub(crate) mod menu;
pub(crate) mod panel;
pub(crate) mod prefs;
pub(crate) mod store;

use std::collections::HashMap;

use zeron_proto::{
    GITHUB_PROVIDER, GITHUB_PULL_REQUEST_REFERENCE, PrReference, PullRequestRef,
    WorkOutlineSnapshot,
};

#[allow(unused_imports)]
pub(crate) use client::{GithubFailure, GithubUiError};

/// The origin label a system-created Required Action shows.
pub(crate) const SYSTEM_ORIGIN_LABEL: &str = "GitHub";

/// The PR a live item of `snapshot` links (`github` / `pullRequest`), if any
/// (the earliest linked, when it links several).
pub(crate) fn item_pull_request(
    snapshot: &WorkOutlineSnapshot,
    item_id: &str,
) -> Option<PullRequestRef> {
    let reference = snapshot.github_pull_request_reference(item_id)?;
    let mut pr = PullRequestRef::parse_external_id(&reference.external_id)?;
    if !reference.url.trim().is_empty() {
        pr.url = reference.url.clone();
    }
    Some(pr)
}

/// Every PR the item links, earliest linked first (an item can link
/// several: a ticket's UI and Flagship PRs, from Linear or its Chats).
pub(crate) fn item_pull_requests(
    snapshot: &WorkOutlineSnapshot,
    item_id: &str,
) -> Vec<PullRequestRef> {
    let mut references: Vec<_> = snapshot
        .external_references
        .iter()
        .filter(|r| {
            r.work_item_id == item_id
                && r.provider == GITHUB_PROVIDER
                && r.reference_type == GITHUB_PULL_REQUEST_REFERENCE
        })
        .collect();
    references.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
    let mut keys = std::collections::HashSet::new();
    references
        .into_iter()
        .filter_map(|reference| {
            let mut pr = PullRequestRef::parse_external_id(&reference.external_id)?;
            if !reference.url.trim().is_empty() {
                pr.url = reference.url.clone();
            }
            keys.insert(pr.key()).then_some(pr)
        })
        .collect()
}

/// The PR of a PR review item: a live item linking a PR that the sync
/// manages (`externalSyncState` from `github`), that sits directly under
/// the sync destination, or whose title starts with `<repo>#<number>` (the
/// panel's manual Create). ⌘↩ on such an item starts a code review of the
/// PR instead of the generic Start Chat; any other PR-linked item (a
/// feature item linking its own PR) keeps the generic Start Chat.
pub(crate) fn pr_review_item(
    snapshot: &WorkOutlineSnapshot,
    item_id: &str,
) -> Option<PullRequestRef> {
    let item = snapshot
        .item(item_id)
        .filter(|item| item.archived_at.is_none())?;
    let pr = item_pull_request(snapshot, item_id)?;
    let synced = item
        .external_sync_state
        .as_ref()
        .is_some_and(|state| state.provider == GITHUB_PROVIDER);
    let under_destination = live_destination(snapshot)
        .is_some_and(|destination| item.parent_work_item_id.as_deref() == Some(destination));
    let titled = item
        .title
        .to_lowercase()
        .starts_with(&format!("{}#{}", pr.repo, pr.number).to_lowercase());
    (synced || under_destination || titled).then_some(pr)
}

/// The `StartPrReview` reference of `pr`.
pub(crate) fn launch_reference(pr: &PullRequestRef) -> PrReference {
    PrReference {
        owner: pr.owner.clone(),
        name: pr.repo.clone(),
        number: pr.number,
    }
}

/// The Space's sync destination when it names a live item.
pub(crate) fn live_destination(snapshot: &WorkOutlineSnapshot) -> Option<&str> {
    let id = snapshot.pr_review_sync_destination()?;
    snapshot
        .item(id)
        .filter(|item| item.archived_at.is_none())
        .map(|item| item.id.as_str())
}

/// Whether the GitHub surfaces (left-column section, sync destination)
/// apply to the Space: its host advertises the PR review sync, or a
/// destination is already set.
pub(crate) fn space_available(
    state: &crate::state::AppState,
    space_id: &str,
    snapshot: &WorkOutlineSnapshot,
) -> bool {
    if snapshot.pr_review_sync_destination().is_some() {
        return true;
    }
    state.space_row(space_id).is_some_and(|space| {
        state.device_supports(
            &space.device_id,
            zeron_proto::capabilities::PR_REVIEW_SYNC_V1,
        )
    })
}

/// PR key (`owner/repo#n`, lowercased) → the earliest live item that links
/// it, preferring an item under the sync destination (the synced item).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct PrIndex {
    items: HashMap<String, String>,
}

impl PrIndex {
    pub(crate) fn from_snapshot(snapshot: &WorkOutlineSnapshot) -> Self {
        let destination = live_destination(snapshot);
        let mut refs: Vec<_> = snapshot
            .external_references
            .iter()
            .filter(|r| {
                r.provider == GITHUB_PROVIDER && r.reference_type == GITHUB_PULL_REQUEST_REFERENCE
            })
            .filter(|r| {
                snapshot
                    .item(&r.work_item_id)
                    .is_some_and(|item| item.archived_at.is_none())
            })
            .collect();
        // Synced children of the destination first, then creation order.
        refs.sort_by_key(|r| {
            let synced = destination.is_some_and(|dest| {
                snapshot
                    .item(&r.work_item_id)
                    .is_some_and(|item| item.parent_work_item_id.as_deref() == Some(dest))
            });
            (!synced, r.created_at, r.id.clone())
        });
        let mut items = HashMap::new();
        for r in refs {
            if let Some(pr) = PullRequestRef::parse_external_id(&r.external_id) {
                items
                    .entry(pr.key())
                    .or_insert_with(|| r.work_item_id.clone());
            }
        }
        Self { items }
    }

    /// The item linking `pr`, if any.
    pub(crate) fn item(&self, pr: &PullRequestRef) -> Option<&str> {
        self.items.get(&pr.key()).map(String::as_str)
    }
}

/// The live children of the destination that link a PR, in Outline order
/// (⇧⌘P's destination list).
pub(crate) fn destination_pr_items(
    snapshot: &WorkOutlineSnapshot,
) -> Vec<(zeron_proto::WorkItemView, PullRequestRef)> {
    let Some(destination) = live_destination(snapshot) else {
        return Vec::new();
    };
    let Some(parent) = snapshot.item(destination) else {
        return Vec::new();
    };
    parent
        .child_ids
        .iter()
        .filter_map(|id| snapshot.item(id))
        .filter(|item| item.archived_at.is_none())
        .filter_map(|item| item_pull_request(snapshot, &item.id).map(|pr| (item.clone(), pr)))
        .collect()
}

#[cfg(test)]
pub(crate) mod fixture {
    use zeron_proto::{
        PullRequestCiStatus, PullRequestMyReviewState, PullRequestRef, PullRequestReviewDecision,
        PullRequestState, PullRequestStatusSummary, WorkItemExternalReferenceView,
    };

    pub(crate) fn summary(repo: &str, number: u64, title: &str) -> PullRequestStatusSummary {
        PullRequestStatusSummary {
            pr_ref: PullRequestRef::new("acme", repo, number),
            title: title.into(),
            author: "octo".into(),
            state: PullRequestState::Open,
            ci_status: PullRequestCiStatus::Passing,
            review_decision: Some(PullRequestReviewDecision::ReviewRequired),
            my_review_state: PullRequestMyReviewState::Requested,
            requested_reviewers: vec!["mikey".into()],
            updated_at: "2026-10-14T00:00:00Z".into(),
            head_ref_name: Some(format!("feature-{number}")),
            base_ref_name: Some("main".into()),
        }
    }

    /// A `github` / `pullRequest` reference view.
    pub(crate) fn reference(
        id: &str,
        space_id: &str,
        item_id: &str,
        repo: &str,
        number: u64,
    ) -> WorkItemExternalReferenceView {
        let pr = PullRequestRef::new("acme", repo, number);
        WorkItemExternalReferenceView {
            id: id.into(),
            space_id: space_id.into(),
            work_item_id: item_id.into(),
            provider: "github".into(),
            reference_type: "pullRequest".into(),
            external_id: pr.external_id(),
            url: pr.url.clone(),
            cached_title: Some(format!("PR {number}")),
            created_by: None,
            created_at: 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_outline::store::fixture_ids::*;

    #[test]
    fn item_pull_request_reads_the_reference_and_prefers_its_url() {
        let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
        snapshot.external_references = vec![fixture::reference("r1", "s1", API, "app", 7)];
        let pr = item_pull_request(&snapshot, API).expect("linked");
        assert_eq!(
            (pr.owner.as_str(), pr.repo.as_str(), pr.number),
            ("acme", "app", 7)
        );
        assert_eq!(pr.url, "https://github.com/acme/app/pull/7");
        assert!(item_pull_request(&snapshot, UI).is_none());
        let reference = launch_reference(&pr);
        assert_eq!(reference.url(), "https://github.com/acme/app/pull/7");
    }

    #[test]
    fn index_prefers_the_synced_child_and_skips_archived_items() {
        let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
        snapshot.space_settings.pr_review_sync_work_item_id = Some(API.into());
        let mut early = fixture::reference("r0", "s1", UI, "app", 7);
        early.created_at = 0;
        snapshot.external_references = vec![
            early,
            fixture::reference("r1", "s1", API_AUTH, "app", 7),
            fixture::reference("r2", "s1", RETIRED, "app", 9),
        ];
        let index = PrIndex::from_snapshot(&snapshot);
        assert_eq!(
            index.item(&PullRequestRef::new("ACME", "App", 7)),
            Some(API_AUTH)
        );
        assert_eq!(index.item(&PullRequestRef::new("acme", "app", 9)), None);
        snapshot.space_settings.pr_review_sync_work_item_id = None;
        let index = PrIndex::from_snapshot(&snapshot);
        assert_eq!(index.item(&PullRequestRef::new("acme", "app", 7)), Some(UI));
        let items = destination_pr_items(&snapshot);
        assert!(items.is_empty(), "no destination");
    }
}
