//! GitHub entries of the Outline's item menu (Spaces where the PR review
//! sync applies only): "Auto-sync PR Review Requests Here" (or "Stop Syncing PR
//! Reviews Here" on the destination), and on an item linking a PR: Review,
//! Fix CI, Open on GitHub, Refresh PR Status. The Shell runs them
//! (`shell/github_ui.rs`).
use mast_proto::{PrReviewAction, PullRequestRef, WorkOutlineSnapshot};

use crate::icons;
use crate::shell::work_outline_ui::OutlineNavKey;
use crate::work_outline::hints::HintKey;
use crate::work_outline::outline_view::context_menu::{MenuAction, MenuEntry, OutlineContextMenu};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GithubMenuAction {
    /// `setPrReviewSyncDestination` to this item (`None` clears it).
    SetDestination {
        space_id: String,
        item_id: Option<String>,
    },
    /// `StartPrReview` for the item's PR, filed under the item.
    Review {
        space_id: String,
        item_id: String,
        action: PrReviewAction,
    },
    /// Open the PR URL (web-link preference).
    OpenOnGithub(String),
    /// Open the PR URL in the external browser, whatever the preference.
    OpenOnGithubExternal(String),
    /// `PrStatus` with `forceRefresh`.
    Refresh(PullRequestRef),
}

impl GithubMenuAction {
    pub(crate) fn hint(&self, review_item: bool) -> HintKey {
        match self {
            // ⌘↩ starts the code review on a PR review item.
            Self::Review {
                action: PrReviewAction::CodeReview,
                ..
            } if review_item => HintKey::Outline(OutlineNavKey::StartChat),
            _ => HintKey::None,
        }
    }
}

/// The entries an item's menu adds (none where the sync does not apply).
pub(crate) fn item_entries(snapshot: &WorkOutlineSnapshot, item_id: &str) -> Vec<MenuEntry> {
    let Some(item) = snapshot
        .item(item_id)
        .filter(|item| item.archived_at.is_none())
    else {
        return Vec::new();
    };
    let space_id = snapshot.space_id.clone();
    let mut entries = Vec::new();
    if let Some(pr) = super::item_pull_request(snapshot, item_id) {
        let review_item = super::pr_review_item(snapshot, item_id).is_some();
        let review = |id: &'static str, label: &'static str, icon, action| {
            let action = GithubMenuAction::Review {
                space_id: space_id.clone(),
                item_id: item.id.clone(),
                action,
            };
            let hint = action.hint(review_item);
            let mut entry = MenuEntry::new(id, label, icon, MenuAction::Github(action));
            entry.hint = hint;
            entry
        };
        entries.extend([
            review(
                "github-review",
                "Review",
                icons::PULL_REQUEST,
                PrReviewAction::CodeReview,
            ),
            review(
                "github-fix-ci",
                "Fix CI",
                icons::ACTION_TEST,
                PrReviewAction::FixCi,
            ),
            MenuEntry::new(
                "github-open",
                "Open on GitHub",
                icons::ARROW_UP_RIGHT,
                MenuAction::Github(GithubMenuAction::OpenOnGithub(pr.url.clone())),
            ),
            MenuEntry::new(
                "github-open-external",
                "Open in External Browser",
                icons::ARROW_UP_RIGHT,
                MenuAction::Github(GithubMenuAction::OpenOnGithubExternal(pr.url.clone())),
            ),
            MenuEntry::new(
                "github-refresh",
                "Refresh PR Status",
                icons::REFRESH,
                MenuAction::Github(GithubMenuAction::Refresh(pr)),
            ),
        ]);
    }
    let destination = snapshot.pr_review_sync_destination() == Some(item_id);
    entries.push(if destination {
        MenuEntry::new(
            "github-clear-destination",
            "Stop Syncing PR Reviews Here",
            icons::CLOSE_CIRCLE,
            MenuAction::Github(GithubMenuAction::SetDestination {
                space_id: space_id.clone(),
                item_id: None,
            }),
        )
    } else {
        MenuEntry::new(
            "github-use-destination",
            "Auto-sync PR Review Requests Here",
            icons::PULL_REQUEST,
            MenuAction::Github(GithubMenuAction::SetDestination {
                space_id: space_id.clone(),
                item_id: Some(item_id.to_string()),
            }),
        )
    });
    if let Some(first) = entries.first_mut() {
        first.separator_before = true;
    }
    entries
}

/// Put [`item_entries`] before Archive (the menu's destructive tail).
pub(crate) fn insert_item_entries(
    menu: &mut OutlineContextMenu,
    snapshot: &WorkOutlineSnapshot,
    item_id: &str,
) {
    let entries = item_entries(snapshot, item_id);
    let at = menu
        .entries
        .iter()
        .position(|entry| entry.id == "archive")
        .unwrap_or(menu.entries.len());
    menu.entries.splice(at..at, entries);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_outline::store::fixture_ids::*;

    #[test]
    fn pr_items_get_review_entries_and_the_destination_toggles() {
        let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
        // The fixture links acme/launch#42 on API.
        let ids = |snapshot: &WorkOutlineSnapshot, id: &str| -> Vec<&'static str> {
            item_entries(snapshot, id).iter().map(|e| e.id).collect()
        };
        assert_eq!(
            ids(&snapshot, API),
            [
                "github-review",
                "github-fix-ci",
                "github-open",
                "github-open-external",
                "github-refresh",
                "github-use-destination"
            ]
        );
        assert_eq!(ids(&snapshot, UI), ["github-use-destination"]);
        assert!(ids(&snapshot, RETIRED).is_empty(), "archived");
        snapshot.space_settings.pr_review_sync_work_item_id = Some(UI.into());
        assert_eq!(ids(&snapshot, UI), ["github-clear-destination"]);
        // API is a feature item linking its PR: Review shows no ⌘↩ hint.
        let review = &item_entries(&snapshot, API)[0];
        assert_eq!(review.hint, HintKey::None);
        assert!(review.separator_before);
    }
}
