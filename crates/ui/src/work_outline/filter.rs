//! Pure work-item facts for List/Canvas rows: the Chat → work item index
//! (path chip, User Action Required, instruction drift), and the work-item
//! filter (subtree
//! semantics, stale-filter reset, persistence through the Overview's
//! string-set files).
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use super::model::path_label;
use super::store::{WorkItemEffectiveStatus, WorkOutlineSnapshot};
use crate::overview_positions;

/// Filter entry meaning "Chats with no work item".
pub(crate) const UNASSOCIATED_KEY: &str = "~unassociated";

#[derive(Debug, Clone)]
struct IndexedItem {
    space_id: String,
    parent: Option<String>,
    title: String,
    archived: bool,
    needs_action: bool,
    unresolved: u32,
    depth: u32,
}

/// A work-item breadcrumb for a chip: the compact form shows the last two
/// segments, the tooltip the whole path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkItemPathLabel {
    pub short: String,
    pub full: String,
}

/// A filter choice for the toolbar dropdown, in outline order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FilterChoice {
    pub item_id: String,
    pub space_id: String,
    pub title: String,
    pub depth: u32,
}

/// Chat and item lookups across every loaded Space snapshot.
#[derive(Debug, Clone, Default)]
pub(crate) struct WorkItemIndex {
    items: HashMap<String, IndexedItem>,
    /// Outline (pre-)order of live items, for the filter dropdown.
    order: Vec<String>,
    chat_item: HashMap<String, String>,
    /// Associated Chats whose effective instructions changed since their
    /// applied revision (`ChatInstructionState::drift`).
    drifted_chats: HashSet<String>,
    /// Associated Chats whose captured execution configuration differs from
    /// their item's current one (`ChatExecutionBindingView::exception`).
    exception_chats: HashSet<String>,
}

impl WorkItemIndex {
    pub(crate) fn build<'a>(snapshots: impl IntoIterator<Item = &'a WorkOutlineSnapshot>) -> Self {
        let mut index = Self::default();
        for snapshot in snapshots {
            if !snapshot.available {
                continue;
            }
            for item in &snapshot.items {
                index.order.push(item.id.clone());
                index.items.insert(
                    item.id.clone(),
                    IndexedItem {
                        space_id: snapshot.space_id.clone(),
                        parent: item.parent_work_item_id.clone(),
                        title: if item.title.trim().is_empty() {
                            "Untitled".to_string()
                        } else {
                            item.title.clone()
                        },
                        archived: item.archived_at.is_some(),
                        needs_action: item.effective_status
                            == WorkItemEffectiveStatus::UserActionRequired,
                        unresolved: item.rollup.unresolved_user_action_count,
                        depth: item.depth,
                    },
                );
                for chat in &item.chat_ids {
                    index.chat_item.insert(chat.clone(), item.id.clone());
                }
            }
            let assigned = super::instructions::assigned_chats(snapshot);
            index.drifted_chats.extend(
                snapshot
                    .chat_instruction_states
                    .iter()
                    .filter(|state| state.drift && assigned.contains_key(&state.chat_id))
                    .map(|state| state.chat_id.clone()),
            );
            index.exception_chats.extend(
                snapshot
                    .chat_execution_bindings
                    .iter()
                    .filter(|binding| binding.exception)
                    .map(|binding| binding.chat_id.clone()),
            );
        }
        index
    }

    /// Whether the Chat's instructions drifted from its applied revision.
    pub(crate) fn instruction_drift_for_chat(&self, chat_id: &str) -> bool {
        self.drifted_chats.contains(chat_id)
    }

    /// Whether the Chat runs with a configuration other than its item's.
    pub(crate) fn execution_exception_for_chat(&self, chat_id: &str) -> bool {
        self.exception_chats.contains(chat_id)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(crate) fn contains(&self, item_id: &str) -> bool {
        self.items.contains_key(item_id)
    }

    pub(crate) fn item_for_chat(&self, chat_id: &str) -> Option<&str> {
        self.chat_item.get(chat_id).map(String::as_str)
    }

    pub(crate) fn space_of(&self, item_id: &str) -> Option<&str> {
        self.items.get(item_id).map(|item| item.space_id.as_str())
    }

    /// Titles root → item. Cycle-guarded.
    pub(crate) fn path_titles(&self, item_id: &str) -> Vec<&str> {
        let mut titles = Vec::new();
        let mut seen = HashSet::new();
        let mut cursor = Some(item_id);
        while let Some(id) = cursor {
            let Some(item) = self.items.get(id) else {
                break;
            };
            if !seen.insert(id) {
                break;
            }
            titles.push(item.title.as_str());
            cursor = item.parent.as_deref();
        }
        titles.reverse();
        titles
    }

    pub(crate) fn path_label(&self, item_id: &str) -> Option<WorkItemPathLabel> {
        let titles = self.path_titles(item_id);
        if titles.is_empty() {
            return None;
        }
        let full = path_label(titles.iter().copied());
        let short = if titles.len() > 2 {
            format!(
                "… › {}",
                path_label(titles[titles.len() - 2..].iter().copied())
            )
        } else {
            full.clone()
        };
        Some(WorkItemPathLabel { short, full })
    }

    /// `(needs user action, unresolved request count)` for a Chat's item.
    pub(crate) fn attention_for_chat(&self, chat_id: &str) -> (bool, u32) {
        self.item_for_chat(chat_id)
            .and_then(|id| self.items.get(id))
            .map_or((false, 0), |item| {
                (item.needs_action || item.unresolved > 0, item.unresolved)
            })
    }

    /// Whether `item_id` is `ancestor` or below it.
    pub(crate) fn is_in_subtree(&self, item_id: &str, ancestor: &str) -> bool {
        let mut seen = HashSet::new();
        let mut cursor = Some(item_id);
        while let Some(id) = cursor {
            if id == ancestor {
                return true;
            }
            if !seen.insert(id) {
                return false;
            }
            cursor = self.items.get(id).and_then(|item| item.parent.as_deref());
        }
        false
    }

    /// Non-archived items in outline order (the filter dropdown).
    pub(crate) fn choices(&self) -> Vec<FilterChoice> {
        self.order
            .iter()
            .filter_map(|id| {
                let item = self.items.get(id)?;
                (!item.archived).then(|| FilterChoice {
                    item_id: id.clone(),
                    space_id: item.space_id.clone(),
                    title: item.title.clone(),
                    depth: item.depth,
                })
            })
            .collect()
    }
}

/// Whether a Chat passes the filter. Empty filter = everything. A selected
/// item keeps its whole subtree; [`UNASSOCIATED_KEY`] keeps Chats with no
/// work item.
pub(crate) fn chat_passes(filter: &BTreeSet<String>, index: &WorkItemIndex, chat_id: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    match index.item_for_chat(chat_id) {
        None => filter.contains(UNASSOCIATED_KEY),
        Some(item) => filter
            .iter()
            .filter(|key| key.as_str() != UNASSOCIATED_KEY)
            .any(|selected| index.is_in_subtree(item, selected)),
    }
}

/// Entries naming items no loaded snapshot knows (deleted or from a removed
/// Space). Only meaningful once every Space is loaded.
pub(crate) fn unknown_entries(
    filter: &BTreeSet<String>,
    index: &WorkItemIndex,
) -> BTreeSet<String> {
    filter
        .iter()
        .filter(|key| key.as_str() != UNASSOCIATED_KEY && !index.contains(key))
        .cloned()
        .collect()
}

/// The stale-filter safety net (same contract as the repo filter's): a
/// restored filter that hides every current Chat — typically one naming
/// items since archived or deleted — must not leave an invisibly empty view.
/// `chat_ids` empty says nothing (Chats not loaded yet).
pub(crate) fn restored_filter_hides_everything<'a>(
    filter: &BTreeSet<String>,
    index: &WorkItemIndex,
    chat_ids: impl IntoIterator<Item = &'a str>,
) -> bool {
    if filter.is_empty() {
        return false;
    }
    let mut any = false;
    for chat_id in chat_ids {
        any = true;
        if chat_passes(filter, index, chat_id) {
            return false;
        }
    }
    any
}

pub(crate) fn load_filter(data_dir: &Path) -> BTreeSet<String> {
    overview_positions::load_string_set(data_dir, overview_positions::WORK_ITEM_FILTER_FILE)
}

pub(crate) fn save_filter(data_dir: &Path, filter: &BTreeSet<String>) -> std::io::Result<()> {
    overview_positions::save_string_set(data_dir, overview_positions::WORK_ITEM_FILTER_FILE, filter)
}

/// Toolbar label for the trigger.
pub(crate) fn filter_label(filter: &BTreeSet<String>) -> String {
    match filter.len() {
        0 => "Work items".to_string(),
        n => format!("Work items ({n})"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::store::fixture_ids::*;
    use super::super::store::fixture_snapshot;
    use super::*;

    fn set(keys: &[&str]) -> BTreeSet<String> {
        keys.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn subtree_filter_and_unassociated_sentinel() {
        let snapshot = fixture_snapshot("s1");
        let index = WorkItemIndex::build([&snapshot]);
        // Empty passes everything.
        assert!(chat_passes(&set(&[]), &index, CHAT_LOOSE_A));
        // Selecting the root keeps the whole subtree.
        let launch = set(&[LAUNCH]);
        assert!(chat_passes(&launch, &index, CHAT_API));
        assert!(chat_passes(&launch, &index, CHAT_AUTH));
        assert!(chat_passes(&launch, &index, CHAT_UI));
        assert!(!chat_passes(&launch, &index, CHAT_LOOSE_A));
        // A leaf keeps only itself.
        let auth = set(&[API_AUTH]);
        assert!(chat_passes(&auth, &index, CHAT_AUTH));
        assert!(!chat_passes(&auth, &index, CHAT_API));
        // Sentinel keeps unassociated (including dangling-work-item) chats.
        let loose = set(&[UNASSOCIATED_KEY]);
        assert!(chat_passes(&loose, &index, CHAT_LOOSE_A));
        assert!(chat_passes(&loose, &index, CHAT_ORPHAN));
        assert!(!chat_passes(&loose, &index, CHAT_API));
    }

    #[test]
    fn stale_filters_are_detected() {
        let snapshot = fixture_snapshot("s1");
        let index = WorkItemIndex::build([&snapshot]);
        let gone = set(&["w-gone"]);
        assert_eq!(unknown_entries(&gone, &index), gone);
        assert!(restored_filter_hides_everything(
            &gone,
            &index,
            [CHAT_API, CHAT_LOOSE_A]
        ));
        // Chats not loaded yet says nothing.
        assert!(!restored_filter_hides_everything(&gone, &index, []));
        // A filter that still matches something is kept.
        assert!(!restored_filter_hides_everything(
            &set(&["w-gone", API]),
            &index,
            [CHAT_API, CHAT_LOOSE_A]
        ));
        assert!(unknown_entries(&set(&[UNASSOCIATED_KEY, API]), &index).is_empty());
    }

    #[test]
    fn path_labels_and_attention() {
        let snapshot = fixture_snapshot("s1");
        let index = WorkItemIndex::build([&snapshot]);
        assert_eq!(
            index.path_label(API_AUTH),
            Some(WorkItemPathLabel {
                short: "… › API › Auth endpoints".into(),
                full: "Launch v2 › API › Auth endpoints".into(),
            })
        );
        assert_eq!(index.path_label(API).unwrap().short, "Launch v2 › API");
        assert_eq!(index.attention_for_chat(CHAT_UI), (true, 1));
        assert_eq!(index.attention_for_chat(CHAT_API), (false, 0));
        assert_eq!(index.attention_for_chat(CHAT_LOOSE_A), (false, 0));
        let choices: Vec<String> = index.choices().into_iter().map(|c| c.item_id).collect();
        assert_eq!(choices, [LAUNCH, API, API_AUTH, UI, HOUSEKEEPING]);
    }

    #[test]
    fn drift_is_indexed_only_for_drifted_assigned_chats() {
        let snapshot = super::super::instructions::fixture::snapshot("s1");
        let index = WorkItemIndex::build([&snapshot]);
        assert!(index.instruction_drift_for_chat(CHAT_API));
        assert!(!index.instruction_drift_for_chat(CHAT_AUTH));
        assert!(!index.instruction_drift_for_chat(CHAT_UI));
        assert!(!index.instruction_drift_for_chat(CHAT_LOOSE_A));
    }

    #[test]
    fn filter_round_trips_through_its_own_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_filter(dir.path()).is_empty());
        let filter = set(&[API, UNASSOCIATED_KEY]);
        save_filter(dir.path(), &filter).unwrap();
        assert_eq!(load_filter(dir.path()), filter);
        assert!(
            overview_positions::load_string_set(dir.path(), overview_positions::UI_FLAGS_FILE)
                .is_empty()
        );
    }

    /// Bound Chats are ordinary associated Chats to the work-item filter:
    /// the subtree filter keeps them with their item, and only exception
    /// Chats are flagged.
    #[test]
    fn the_work_item_filter_composes_with_bound_chats() {
        let snapshot = super::super::execution::fixture::snapshot("s1");
        let index = WorkItemIndex::build([&snapshot]);
        // chat-ui (bound, current), chat-auth (bound, current), chat-api
        // (bound, exception) all follow their items.
        let launch = set(&[LAUNCH]);
        for chat in [CHAT_UI, CHAT_AUTH, CHAT_API] {
            assert!(chat_passes(&launch, &index, chat), "{chat}");
        }
        assert!(chat_passes(&set(&[API]), &index, CHAT_AUTH));
        assert!(!chat_passes(&set(&[UI]), &index, CHAT_API));
        assert!(!chat_passes(&set(&[UNASSOCIATED_KEY]), &index, CHAT_UI));
        assert!(index.execution_exception_for_chat(CHAT_API));
        assert!(!index.execution_exception_for_chat(CHAT_UI));
        assert!(!index.execution_exception_for_chat(CHAT_LOOSE_A));
    }
}
