//! Per-device GitHub section state: the whole section's fold and its two
//! collapsible lists (`GITHUB_PANEL_FILE`, next to the Linear panel's file).
//! Pure string-set transforms plus thin load/save wrappers over
//! `overview_positions`, so every rule is unit-testable.
use std::collections::BTreeSet;
use std::path::Path;

use crate::overview_positions;

/// The section's own state file.
pub(crate) const GITHUB_PANEL_FILE: &str = "overview-github-panel.json";
const SECTION_COLLAPSED_PREFIX: &str = "collapsed:";
/// The whole GitHub section is folded to its header.
const FOLDED_ENTRY: &str = "folded";

/// The section's two lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum GithubList {
    ReviewRequested,
    MyOpen,
}

impl GithubList {
    pub(crate) const ALL: [GithubList; 2] = [GithubList::ReviewRequested, GithubList::MyOpen];

    /// Stable persistence key and test-selector suffix.
    pub(crate) fn key(self) -> &'static str {
        match self {
            GithubList::ReviewRequested => "review-requested",
            GithubList::MyOpen => "my-open",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            GithubList::ReviewRequested => "Review requested",
            GithubList::MyOpen => "My open PRs",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|list| list.key() == key)
    }
}

/// What the section restores on creation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GithubPanelState {
    pub folded: bool,
    pub collapsed: BTreeSet<GithubList>,
}

pub(crate) fn state_from_set(set: &BTreeSet<String>) -> GithubPanelState {
    let mut state = GithubPanelState::default();
    for entry in set {
        if entry == FOLDED_ENTRY {
            state.folded = true;
        } else if let Some(key) = entry.strip_prefix(SECTION_COLLAPSED_PREFIX) {
            state.collapsed.extend(GithubList::from_key(key));
        }
    }
    state
}

pub(crate) fn state_to_set(state: &GithubPanelState) -> BTreeSet<String> {
    state
        .collapsed
        .iter()
        .map(|list| format!("{SECTION_COLLAPSED_PREFIX}{}", list.key()))
        .chain(state.folded.then(|| FOLDED_ENTRY.to_string()))
        .collect()
}

pub(crate) fn load_state(data_dir: &Path) -> GithubPanelState {
    state_from_set(&overview_positions::load_string_set(
        data_dir,
        GITHUB_PANEL_FILE,
    ))
}

pub(crate) fn save_state(data_dir: &Path, state: &GithubPanelState) {
    if let Err(err) =
        overview_positions::save_string_set(data_dir, GITHUB_PANEL_FILE, &state_to_set(state))
    {
        tracing::warn!(error = %err, "saving GitHub panel state failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_and_ignores_unknown_entries() {
        let state = GithubPanelState {
            folded: true,
            collapsed: BTreeSet::from([GithubList::MyOpen]),
        };
        let mut set = state_to_set(&state);
        set.insert("collapsed:nope".into());
        set.insert("future".into());
        assert_eq!(state_from_set(&set), state);
        assert_eq!(
            state_from_set(&BTreeSet::new()),
            GithubPanelState::default()
        );
    }
}
