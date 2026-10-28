//! Environment variables under their Mast names, with the old `ZERON_*`
//! spelling still honoured so existing shells, launchd/systemd units, `~/.zeron/env`
//! files and scripts keep working.
//!
//! Callers pass the new name (`MAST_DATA_DIR`). The new name wins when both are
//! set. Values written for other programs (child process environments) that
//! user scripts read are exported under both names by the code that spawns them.

use std::env::VarError;
use std::ffi::OsString;

const NEW_PREFIX: &str = "MAST_";
const OLD_PREFIX: &str = "ZERON_";

/// The pre-rename spelling of a `MAST_*` variable (`MAST_X` -> `ZERON_X`).
/// `None` when `name` is not a `MAST_*` name.
pub fn legacy_name(name: &str) -> Option<String> {
    name.strip_prefix(NEW_PREFIX)
        .map(|rest| format!("{OLD_PREFIX}{rest}"))
}

/// `std::env::var`, falling back to the old `ZERON_*` name when the `MAST_*`
/// variable is not set.
pub fn var(name: &str) -> Result<String, VarError> {
    match std::env::var(name) {
        Err(VarError::NotPresent) => match legacy_name(name) {
            Some(old) => std::env::var(old),
            None => Err(VarError::NotPresent),
        },
        found => found,
    }
}

/// `std::env::var_os`, falling back to the old `ZERON_*` name.
pub fn var_os(name: &str) -> Option<OsString> {
    std::env::var_os(name).or_else(|| legacy_name(name).and_then(std::env::var_os))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Each test uses its own variable pair so parallel tests never collide.
    #[test]
    fn the_new_name_wins_over_the_old_one() {
        unsafe {
            std::env::set_var("MAST_ENV_TEST_BOTH", "new");
            std::env::set_var("ZERON_ENV_TEST_BOTH", "old");
        }
        assert_eq!(var("MAST_ENV_TEST_BOTH").unwrap(), "new");
        assert_eq!(var_os("MAST_ENV_TEST_BOTH").unwrap(), "new");
    }

    #[test]
    fn the_old_name_is_still_read() {
        unsafe { std::env::set_var("ZERON_ENV_TEST_OLD_ONLY", "old") };
        assert_eq!(var("MAST_ENV_TEST_OLD_ONLY").unwrap(), "old");
        assert_eq!(var_os("MAST_ENV_TEST_OLD_ONLY").unwrap(), "old");
    }

    #[test]
    fn neither_name_is_not_present() {
        assert_eq!(var("MAST_ENV_TEST_NEITHER"), Err(VarError::NotPresent));
        assert!(var_os("MAST_ENV_TEST_NEITHER").is_none());
    }

    #[test]
    fn legacy_name_maps_only_mast_names() {
        assert_eq!(
            legacy_name("MAST_DATA_DIR").as_deref(),
            Some("ZERON_DATA_DIR")
        );
        assert_eq!(legacy_name("HOME"), None);
        assert_eq!(var("HOME_NOT_SET_ANYWHERE_X"), Err(VarError::NotPresent));
    }
}
