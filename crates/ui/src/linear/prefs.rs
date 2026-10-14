//! Per-device Linear panel state: the open flag and column width (entries
//! of the Overview's `UI_FLAGS_FILE`, next to the My PRs sidebar flag), and
//! the panel's own section collapse / expanded projects
//! (`LINEAR_PANEL_FILE`). Pure string-set transforms plus thin load/save
//! wrappers over `overview_positions`, so every rule is unit-testable.
use std::collections::BTreeSet;
use std::path::Path;

use crate::overview_positions;

/// UI-flag entry: the Linear panel column is open.
pub(crate) const PANEL_OPEN_FLAG: &str = "linearPanel";
/// UI-flag prefix holding the column width in whole px
/// (`linearPanelWidth:340`). Exactly one such entry at a time.
const PANEL_WIDTH_PREFIX: &str = "linearPanelWidth:";

/// The panel's own state file: collapsed sections and expanded projects.
pub(crate) const LINEAR_PANEL_FILE: &str = "overview-linear-panel.json";
const SECTION_COLLAPSED_PREFIX: &str = "collapsed:";
const PROJECT_EXPANDED_PREFIX: &str = "project:";

/// Column width bounds (px). Below the minimum the column overlays the
/// body instead of squeezing it (`overview::linear_host::linear_column`).
pub(crate) const PANEL_MIN_W: f32 = 260.0;
pub(crate) const PANEL_MAX_W: f32 = 560.0;
pub(crate) const PANEL_DEFAULT_W: f32 = 340.0;

pub(crate) fn clamp_width(width: f32) -> f32 {
    if width.is_finite() {
        width.clamp(PANEL_MIN_W, PANEL_MAX_W)
    } else {
        PANEL_DEFAULT_W
    }
}

/// The persisted width, clamped; `None` when absent or unparseable.
pub(crate) fn width_from_flags(flags: &BTreeSet<String>) -> Option<f32> {
    flags
        .iter()
        .filter_map(|flag| flag.strip_prefix(PANEL_WIDTH_PREFIX))
        .find_map(|value| value.parse::<f32>().ok())
        .map(clamp_width)
}

/// Replace the width entry (every older one is purged).
pub(crate) fn set_width_flag(flags: &mut BTreeSet<String>, width: f32) {
    flags.retain(|flag| !flag.starts_with(PANEL_WIDTH_PREFIX));
    flags.insert(format!(
        "{PANEL_WIDTH_PREFIX}{}",
        clamp_width(width).round()
    ));
}

/// The panel's two collapsible sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum PanelSection {
    MyIssues,
    MyProjects,
}

impl PanelSection {
    pub(crate) const ALL: [PanelSection; 2] = [PanelSection::MyIssues, PanelSection::MyProjects];

    /// Stable persistence key and test-selector suffix.
    pub(crate) fn key(self) -> &'static str {
        match self {
            PanelSection::MyIssues => "my-issues",
            PanelSection::MyProjects => "my-projects",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            PanelSection::MyIssues => "My issues",
            PanelSection::MyProjects => "My projects",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|section| section.key() == key)
    }
}

/// What the panel restores on creation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PanelState {
    pub collapsed: BTreeSet<PanelSection>,
    pub expanded_projects: BTreeSet<String>,
}

pub(crate) fn panel_state_from_set(set: &BTreeSet<String>) -> PanelState {
    let mut state = PanelState::default();
    for entry in set {
        if let Some(key) = entry.strip_prefix(SECTION_COLLAPSED_PREFIX) {
            state.collapsed.extend(PanelSection::from_key(key));
        } else if let Some(id) = entry.strip_prefix(PROJECT_EXPANDED_PREFIX)
            && !id.is_empty()
        {
            state.expanded_projects.insert(id.to_string());
        }
    }
    state
}

pub(crate) fn panel_state_to_set(state: &PanelState) -> BTreeSet<String> {
    state
        .collapsed
        .iter()
        .map(|section| format!("{SECTION_COLLAPSED_PREFIX}{}", section.key()))
        .chain(
            state
                .expanded_projects
                .iter()
                .map(|id| format!("{PROJECT_EXPANDED_PREFIX}{id}")),
        )
        .collect()
}

pub(crate) fn load_panel_state(data_dir: &Path) -> PanelState {
    panel_state_from_set(&overview_positions::load_string_set(
        data_dir,
        LINEAR_PANEL_FILE,
    ))
}

pub(crate) fn save_panel_state(data_dir: &Path, state: &PanelState) {
    if let Err(err) =
        overview_positions::save_string_set(data_dir, LINEAR_PANEL_FILE, &panel_state_to_set(state))
    {
        tracing::warn!(error = %err, "saving Linear panel state failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_round_trips_clamped_and_replaces_older_entries() {
        let mut flags = BTreeSet::from(["linearPanel".to_string(), "other".to_string()]);
        assert_eq!(width_from_flags(&flags), None);
        set_width_flag(&mut flags, 401.4);
        assert_eq!(width_from_flags(&flags), Some(401.0));
        set_width_flag(&mut flags, 9_000.0);
        assert_eq!(width_from_flags(&flags), Some(PANEL_MAX_W));
        assert_eq!(
            flags
                .iter()
                .filter(|f| f.starts_with(PANEL_WIDTH_PREFIX))
                .count(),
            1
        );
        assert!(flags.contains("other") && flags.contains(PANEL_OPEN_FLAG));
        let junk = BTreeSet::from([format!("{PANEL_WIDTH_PREFIX}junk")]);
        assert_eq!(width_from_flags(&junk), None);
        assert_eq!(clamp_width(10.0), PANEL_MIN_W);
        assert_eq!(clamp_width(f32::NAN), PANEL_DEFAULT_W);
    }

    #[test]
    fn panel_state_round_trips_and_ignores_unknown_entries() {
        let state = PanelState {
            collapsed: BTreeSet::from([PanelSection::MyProjects]),
            expanded_projects: BTreeSet::from(["p-launch".to_string(), "p-2".to_string()]),
        };
        let mut set = panel_state_to_set(&state);
        set.insert("collapsed:nope".into());
        set.insert("project:".into());
        set.insert("future".into());
        assert_eq!(panel_state_from_set(&set), state);
    }
}
