//! The app is called Mast. Internal identifiers keep the old name, but no
//! user-facing string may bring "Zeron" or "Creel" back. This scans the Rust
//! sources that feed the UI, engine messages, and agent instructions for a
//! whole-word "Zeron" or "Creel" outside comments, and checks the macOS bundle
//! templates.

use std::fs;
use std::path::{Path, PathBuf};

/// Substrings that mark a legitimate internal use on the same line: type and
/// variant names, filesystem paths, bundle/tarball directory names, fixtures,
/// and strings that are pinned by a golden hash or read by other programs.
const ALLOWED: &[&str] = &[
    "Zeron::",
    "Self::Zeron",
    "AccentColor::Zeron",
    "AccentPreset::Zeron",
    "Zeron,",
    "struct Zeron",
    "impl Zeron",
    "Arc<Zeron>",
    "use ",
    ".join(\"Zeron\")",
    "Zeron.app",
    "Zeron\\",
    "\\Zeron",
    "Zeron Test",
    "Zeron #42",
    "Zeron @ MacBook",
    "Zeron Missing SVG",
    "Zeron-launched",
    "fork of Zeron",
    "name: Some(\"Zeron\"",
    "repo_name: \"Zeron\"",
    // The guard assertions in app_menus.rs.
    ".contains(\"Zeron\")",
    // The guard assertions in app_menus.rs.
    ".contains(\"Zeron\")",
    // Panic messages for built-in invariants.
    "expect(\"",
    "contains both Zeron",
    // Env value other programs key on (TERM_PROGRAM).
    "TERM_PROGRAM",
    // Part of the golden-hashed Work Outline envelope preamble.
    "the user in Zeron. They supplement",
];

/// Names the app used to have; none may appear in a user-facing string.
const OLD_NAMES: &[&str] = &["Zeron", "Creel"];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn has_whole_word(line: &str, word: &str) -> bool {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    line.match_indices(word).any(|(start, _)| {
        let before = line[..start].chars().next_back();
        let after = line[start + word.len()..].chars().next();
        !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
    })
}

#[test]
fn user_facing_strings_do_not_use_the_old_app_name() {
    let root = workspace_root();
    let this_file = Path::new(file!()).file_name().unwrap().to_owned();
    let mut files = Vec::new();
    for dir in [
        "crates/ui/src",
        "crates/engine/src",
        "crates/mcp/src",
        "crates/proto/src",
        "crates/update/src",
        "crates/theme/src",
        "crates/harness/src",
        "apps/zeron/src",
    ] {
        rust_files(&root.join(dir), &mut files);
    }
    assert!(files.len() > 100, "scan found only {} files", files.len());

    let mut offenders = Vec::new();
    for path in files {
        if path.file_name() == Some(&this_file) {
            continue;
        }
        let text = fs::read_to_string(&path).expect("read source");
        for (index, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with('*') {
                continue;
            }
            let old_name = OLD_NAMES.iter().any(|word| has_whole_word(line, word));
            if !old_name || ALLOWED.iter().any(|ok| line.contains(ok)) {
                continue;
            }
            offenders.push(format!(
                "{}:{}: {}",
                path.strip_prefix(&root).unwrap().display(),
                index + 1,
                line.trim()
            ));
        }
    }
    assert!(
        offenders.is_empty(),
        "user-facing \"Zeron\" or \"Creel\" found (use \"Mast\"; add a deliberate internal use to ALLOWED):\n{}",
        offenders.join("\n")
    );
}

#[test]
fn macos_bundle_templates_are_named_mast() {
    let root = workspace_root();
    for name in ["Info.plist", "Info-dev.plist"] {
        let plist = fs::read_to_string(root.join("dist/macos").join(name)).expect("read plist");
        for key in ["CFBundleName", "CFBundleDisplayName"] {
            let after = plist.split(&format!("<key>{key}</key>")).nth(1).expect(key);
            let value = after
                .split("<string>")
                .nth(1)
                .and_then(|s| s.split("</string>").next());
            assert_eq!(value, Some("Mast"), "{name} {key}");
        }
        assert!(plist.contains("<string>mast.icns</string>"), "{name} icon");
        // Internal identifiers stay put.
        assert!(plist.contains("<string>sh.zeron.app"), "{name} bundle id");
        assert!(plist.contains("<key>CFBundleExecutable</key>\n    <string>zeron</string>"));
    }
}
