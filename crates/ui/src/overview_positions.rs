//! Small per-device overview UI-state files under the engine's local data
//! dir — the acknowledged-"done" set, the hidden-repos filter set, the UI
//! flags and the Outline's own sets. Purely local UI state, not synced
//! workspace data, so it lives alongside `ui-settings.json` rather than in the
//! doc store — same convention `SettingsStore` already uses (`settings.rs`).
//!
//! Everything goes through the generic [`load_string_set`]/[`save_string_set`]
//! (and list) pairs: atomic write, corrupt degrades to empty. (The Canvas
//! view's `overview-positions.json` is no longer read or written; an old copy
//! is simply left on disk.)

use std::collections::BTreeSet;
use std::io;
use std::path::Path;

/// Chat ids whose "done" status the user has already acknowledged (expanded
/// the row or opened the chat) — see `overview.rs`'s unread-done ribbon.
pub const ACKNOWLEDGED_DONE_FILE: &str = "overview-acknowledged-done.json";
/// Repo keys (cwd last path segment) the repo filter currently hides.
pub const HIDDEN_REPOS_FILE: &str = "overview-hidden-repos.json";
/// Named on/off overview UI toggles that persist (a string is present = on),
/// e.g. the My PRs sidebar. Unknown entries are ignored by readers.
pub const UI_FLAGS_FILE: &str = "overview-ui-flags.json";
/// Work-item ids (plus the `~unassociated` sentinel) the List
/// work-item filter keeps; empty = no filter. See `work_outline::filter`.
pub const WORK_ITEM_FILTER_FILE: &str = "overview-work-item-filter.json";
/// Outline row keys the user collapsed (Space roots, work items, groups).
/// Its own file rather than `UI_FLAGS_FILE`, which is read-modify-written on
/// every view toggle.
pub const OUTLINE_COLLAPSED_FILE: &str = "overview-outline-collapsed.json";
/// Space ids in the order the user arranged the Outline's Space roots
/// (an ordered list; Spaces not in it follow alphabetically).
pub const OUTLINE_SPACE_ORDER_FILE: &str = "overview-outline-space-order.json";
/// Space ids the user hid from the Outline (List and the sidebar are
/// unaffected).
pub const OUTLINE_HIDDEN_SPACES_FILE: &str = "overview-outline-hidden-spaces.json";

/// Load a JSON string array from `{data_dir}/{file_name}`. Absent or corrupt
/// degrades to empty.
pub fn load_string_set(data_dir: &Path, file_name: &str) -> BTreeSet<String> {
    match std::fs::read_to_string(data_dir.join(file_name)) {
        Ok(text) => match serde_json::from_str::<BTreeSet<String>>(&text) {
            Ok(set) => set,
            Err(err) => {
                tracing::warn!(error = %err, file = file_name, "overview state file corrupt; starting empty");
                BTreeSet::new()
            }
        },
        Err(_) => BTreeSet::new(),
    }
}

/// Atomic write of a string set as a sorted JSON array.
pub fn save_string_set(data_dir: &Path, file_name: &str, set: &BTreeSet<String>) -> io::Result<()> {
    write_json_atomic(data_dir, &data_dir.join(file_name), set)
}

/// Load an ordered JSON string array from `{data_dir}/{file_name}`.
/// Absent or corrupt degrades to empty, like [`load_string_set`].
pub fn load_string_list(data_dir: &Path, file_name: &str) -> Vec<String> {
    match std::fs::read_to_string(data_dir.join(file_name)) {
        Ok(text) => match serde_json::from_str::<Vec<String>>(&text) {
            Ok(list) => list,
            Err(err) => {
                tracing::warn!(error = %err, file = file_name, "overview state file corrupt; starting empty");
                Vec::new()
            }
        },
        Err(_) => Vec::new(),
    }
}

/// Atomic write of an ordered string list as a JSON array.
pub fn save_string_list(data_dir: &Path, file_name: &str, list: &[String]) -> io::Result<()> {
    write_json_atomic(data_dir, &data_dir.join(file_name), &list)
}

/// Temp file + rename so a crash mid-write never corrupts the file, matching
/// `SettingsStore::save`'s idiom.
fn write_json_atomic<T: serde::Serialize>(
    data_dir: &Path,
    dest: &Path,
    value: &T,
) -> io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let tmp = dest.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_set_round_trips_and_missing_or_corrupt_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(load_string_set(dir.path(), HIDDEN_REPOS_FILE).is_empty());

        let set: BTreeSet<String> = ["zeron".to_string(), "ui".to_string()].into();
        save_string_set(dir.path(), HIDDEN_REPOS_FILE, &set).expect("save");
        assert_eq!(load_string_set(dir.path(), HIDDEN_REPOS_FILE), set);
        // Separate files don't bleed into each other.
        assert!(load_string_set(dir.path(), ACKNOWLEDGED_DONE_FILE).is_empty());

        std::fs::write(dir.path().join(ACKNOWLEDGED_DONE_FILE), b"[nope").expect("write");
        assert!(load_string_set(dir.path(), ACKNOWLEDGED_DONE_FILE).is_empty());
    }

    #[test]
    fn string_list_keeps_its_order_and_missing_or_corrupt_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(load_string_list(dir.path(), OUTLINE_SPACE_ORDER_FILE).is_empty());
        let list = vec!["zeta".to_string(), "alpha".to_string()];
        save_string_list(dir.path(), OUTLINE_SPACE_ORDER_FILE, &list).expect("save");
        assert_eq!(load_string_list(dir.path(), OUTLINE_SPACE_ORDER_FILE), list);
        std::fs::write(dir.path().join(OUTLINE_SPACE_ORDER_FILE), b"{").expect("write");
        assert!(load_string_list(dir.path(), OUTLINE_SPACE_ORDER_FILE).is_empty());
    }
}
