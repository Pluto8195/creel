//! Pure keyboard navigation over the flattened Outline rows.
//!
//! Selection is a row *key*, never an index, so a snapshot refresh or a
//! collapse keeps focus on the same row; [`reconcile_selection`] picks the
//! nearest survivor when the selected row itself goes away.
use super::model::OutlineModel;

/// One navigation step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavCommand {
    /// Next visible row.
    Next,
    /// Previous visible row.
    Previous,
    First,
    Last,
    Parent,
    /// First child, expanding a collapsed row first.
    Child,
    NextSibling,
    PreviousSibling,
    /// Expand a collapsed row; on an expanded row, move to its first child.
    Expand,
    /// Collapse an expanded row; on a leaf or collapsed row, move to the
    /// parent.
    Collapse,
}

/// What a step does: the row to select and/or a collapse toggle to apply
/// (`(key, collapsed)`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct NavResult {
    pub select: Option<String>,
    pub set_collapsed: Option<(String, bool)>,
}

impl NavResult {
    fn select(model: &OutlineModel, ix: usize) -> Self {
        Self {
            select: Some(model.rows[ix].key.clone()),
            set_collapsed: None,
        }
    }
}

pub(crate) fn next_sibling(model: &OutlineModel, ix: usize) -> Option<usize> {
    let row = model.rows.get(ix)?;
    for (j, other) in model.rows.iter().enumerate().skip(ix + 1) {
        if other.depth < row.depth {
            return None;
        }
        if other.depth == row.depth && other.parent == row.parent {
            return Some(j);
        }
    }
    None
}

pub(crate) fn previous_sibling(model: &OutlineModel, ix: usize) -> Option<usize> {
    let row = model.rows.get(ix)?;
    for j in (0..ix).rev() {
        let other = &model.rows[j];
        if other.depth < row.depth {
            return None;
        }
        if other.depth == row.depth && other.parent == row.parent {
            return Some(j);
        }
    }
    None
}

/// The first visible child (`None` when collapsed or a leaf).
pub(crate) fn first_child(model: &OutlineModel, ix: usize) -> Option<usize> {
    let row = model.rows.get(ix)?;
    (row.has_children && !row.collapsed)
        .then_some(ix + 1)
        .filter(|&child| model.rows.get(child).is_some_and(|c| c.parent == Some(ix)))
}

/// Apply `command` from the row keyed `selected` (none selected: the first
/// row for forward moves, the last for backward ones).
pub(crate) fn navigate(
    model: &OutlineModel,
    selected: Option<&str>,
    command: NavCommand,
) -> NavResult {
    if model.is_empty() {
        return NavResult::default();
    }
    let last = model.len() - 1;
    let Some(ix) = selected.and_then(|key| model.position(key)) else {
        return match command {
            NavCommand::Previous | NavCommand::Last => NavResult::select(model, last),
            _ => NavResult::select(model, 0),
        };
    };
    let row = &model.rows[ix];
    let target = match command {
        NavCommand::Next => Some((ix + 1).min(last)),
        NavCommand::Previous => Some(ix.saturating_sub(1)),
        NavCommand::First => Some(0),
        NavCommand::Last => Some(last),
        NavCommand::Parent => row.parent,
        NavCommand::NextSibling => next_sibling(model, ix),
        NavCommand::PreviousSibling => previous_sibling(model, ix),
        NavCommand::Child => {
            if row.has_children && row.collapsed {
                // Expand; the child is selected once it is on screen.
                return NavResult {
                    select: None,
                    set_collapsed: Some((row.key.clone(), false)),
                };
            }
            first_child(model, ix)
        }
        NavCommand::Expand => {
            if row.has_children && row.collapsed {
                return NavResult {
                    select: None,
                    set_collapsed: Some((row.key.clone(), false)),
                };
            }
            first_child(model, ix)
        }
        NavCommand::Collapse => {
            if row.has_children && !row.collapsed {
                return NavResult {
                    select: None,
                    set_collapsed: Some((row.key.clone(), true)),
                };
            }
            row.parent
        }
    };
    target.map_or_else(NavResult::default, |t| NavResult::select(model, t))
}

/// Keep focus stable across a re-flatten: the same key if it survives,
/// otherwise its nearest surviving ancestor (from `previous`), otherwise the
/// row now at its old index (clamped), otherwise nothing.
pub(crate) fn reconcile_selection(
    previous: &OutlineModel,
    next: &OutlineModel,
    selected: &str,
) -> Option<String> {
    if next.position(selected).is_some() {
        return Some(selected.to_string());
    }
    if let Some(old_ix) = previous.position(selected) {
        if let Some(ancestor) = previous
            .ancestor_keys(old_ix)
            .into_iter()
            .find(|key| next.position(key).is_some())
        {
            return Some(ancestor.to_string());
        }
        if !next.is_empty() {
            return Some(next.rows[old_ix.min(next.len() - 1)].key.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    use super::super::model::tests::{fixture_chats, large_snapshot, ready};
    use super::super::model::{OutlineOptions, flatten, row_key};
    use super::super::store::fixture_ids::*;
    use super::super::store::fixture_snapshot;
    use super::*;

    fn model(collapsed: &HashSet<String>) -> OutlineModel {
        flatten(
            &[ready("s1", "Repo", fixture_snapshot("s1"))],
            &fixture_chats("s1"),
            collapsed,
            &OutlineOptions {
                all_spaces: true,
                ..Default::default()
            },
        )
    }

    fn item(id: &str) -> String {
        row_key::item("s1", id)
    }

    fn sel(result: NavResult) -> String {
        result.select.expect("selects a row")
    }

    #[test]
    fn parent_child_sibling_first_last() {
        let m = model(&HashSet::new());
        let launch = item(LAUNCH);
        assert_eq!(
            sel(navigate(&m, Some(&launch), NavCommand::Child)),
            item(API)
        );
        assert_eq!(
            sel(navigate(&m, Some(&item(API)), NavCommand::NextSibling)),
            item(UI)
        );
        assert_eq!(
            sel(navigate(&m, Some(&item(UI)), NavCommand::PreviousSibling)),
            item(API)
        );
        assert_eq!(
            navigate(&m, Some(&item(UI)), NavCommand::NextSibling),
            NavResult::default()
        );
        assert_eq!(
            sel(navigate(&m, Some(&item(UI)), NavCommand::Parent)),
            launch
        );
        assert_eq!(
            sel(navigate(&m, Some(&launch), NavCommand::NextSibling)),
            item(HOUSEKEEPING)
        );
        assert_eq!(
            sel(navigate(
                &m,
                Some(&item(HOUSEKEEPING)),
                NavCommand::NextSibling
            )),
            row_key::unassociated(Some("s1"))
        );
        assert_eq!(
            navigate(&m, Some(&launch), NavCommand::Parent),
            NavResult::default()
        );
        assert_eq!(
            sel(navigate(&m, Some(&item(API)), NavCommand::First)),
            launch
        );
        assert_eq!(
            sel(navigate(&m, Some(&launch), NavCommand::Last)),
            m.rows.last().unwrap().key
        );
        assert_eq!(
            sel(navigate(&m, Some(&launch), NavCommand::Next)),
            item(API)
        );
        assert_eq!(
            sel(navigate(&m, Some(&launch), NavCommand::Previous)),
            launch
        );
        // Nothing selected: forward picks the first row, backward the last.
        assert_eq!(sel(navigate(&m, None, NavCommand::Next)), launch);
        assert_eq!(
            sel(navigate(&m, None, NavCommand::Previous)),
            m.rows.last().unwrap().key
        );
    }

    #[test]
    fn expand_and_collapse_toggle_then_move() {
        let m = model(&HashSet::new());
        // Expanded parent: Collapse folds it, does not move.
        assert_eq!(
            navigate(&m, Some(&item(API)), NavCommand::Collapse).set_collapsed,
            Some((item(API), true))
        );
        // Leaf: Collapse goes to the parent; Expand does nothing.
        assert_eq!(
            navigate(&m, Some(&item(HOUSEKEEPING)), NavCommand::Expand),
            NavResult::default()
        );
        let leaf = row_key::chat(CHAT_UI);
        assert_eq!(
            sel(navigate(&m, Some(&leaf), NavCommand::Collapse)),
            item(UI)
        );
        assert_eq!(
            navigate(&m, Some(&leaf), NavCommand::Expand),
            NavResult::default()
        );
        // Collapsed: Expand/Child unfold, Collapse moves to the parent.
        let folded = model(&[item(API)].into());
        assert_eq!(
            navigate(&folded, Some(&item(API)), NavCommand::Expand).set_collapsed,
            Some((item(API), false))
        );
        assert_eq!(
            navigate(&folded, Some(&item(API)), NavCommand::Child).set_collapsed,
            Some((item(API), false))
        );
        assert_eq!(
            sel(navigate(&folded, Some(&item(API)), NavCommand::Collapse)),
            item(LAUNCH)
        );
        // Sibling of the collapsed row skips its hidden subtree.
        assert_eq!(
            sel(navigate(&folded, Some(&item(API)), NavCommand::Next)),
            item(UI)
        );
    }

    #[test]
    fn focus_is_stable_across_refresh_and_falls_back_to_an_ancestor() {
        let before = model(&HashSet::new());
        // The same key survives a refresh that reorders nothing.
        let after = model(&HashSet::new());
        assert_eq!(
            reconcile_selection(&before, &after, &item(UI)),
            Some(item(UI))
        );
        // Collapsing the parent hides the selected chat: focus goes to the
        // nearest visible ancestor.
        let folded = model(&[item(API)].into());
        assert_eq!(
            reconcile_selection(&before, &folded, &row_key::chat(CHAT_AUTH)),
            Some(item(API))
        );
        // A key unknown to both: nothing.
        assert_eq!(reconcile_selection(&before, &after, "chat:nope"), None);
    }

    /// Acceptance 30: a 20k-row outline flattens and navigates well within an
    /// interactive budget (debug build, so the budget is generous).
    #[test]
    fn twenty_thousand_rows_flatten_and_navigate_within_budget() {
        let (snapshot, chats) = large_snapshot("s1", 50, 200);
        let spaces = [ready("s1", "Big", snapshot)];
        let started = Instant::now();
        let m = flatten(&spaces, &chats, &HashSet::new(), &OutlineOptions::default());
        let flattened = started.elapsed();
        assert_eq!(m.len(), 50 + 50 * 200 * 2);
        let started = Instant::now();
        let mut selected = m.rows[0].key.clone();
        for _ in 0..m.len() {
            if let Some(next) = navigate(&m, Some(&selected), NavCommand::Next).select {
                selected = next;
            }
        }
        assert_eq!(selected, m.rows.last().unwrap().key);
        for _ in 0..200 {
            if let Some(prev) = navigate(&m, Some(&selected), NavCommand::PreviousSibling).select {
                selected = prev;
            }
        }
        let navigated = started.elapsed();
        let budget = Duration::from_secs(3);
        assert!(flattened < budget, "flatten took {flattened:?}");
        assert!(navigated < budget, "navigation took {navigated:?}");
    }
}
