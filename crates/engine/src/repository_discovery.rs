//! Repository discovery shared by the PR review sync and the execution-scope
//! validator: which repository folders a Space covers, `git worktree list`
//! parsing, and stable repository ids.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::EngineError;
use crate::repos::Repos;

const NESTED_REPO_SCAN_MAX_DIRS: usize = 20_000;
const NESTED_REPO_SCAN_MAX_DEPTH: usize = 12;
const NESTED_REPO_SCAN_MAX_REPOS: usize = 128;

/// execution-scope validator, `crate::execution_scope`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorktreeRecord {
    pub(crate) path: PathBuf,
    pub(crate) head_sha: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) locked: bool,
    pub(crate) prunable: bool,
}

/// Every repository folder of a Space: its own checkout (when the folder is
/// one), every nested repository the bounded scan finds, and every submodule
/// (recursively), canonicalized and deduplicated, outermost first. Uninitialized
/// submodules are included (their folders may not be repositories).
pub(crate) async fn repository_paths(
    repos: &Repos,
    workspace_root: &Path,
) -> Result<Vec<PathBuf>, EngineError> {
    let mut paths = Vec::<PathBuf>::new();
    let mut seen = HashSet::<PathBuf>::new();
    if repos.is_repo(workspace_root).await {
        let identity = repos.checkout_identity(workspace_root).await?;
        seen.insert(identity.root.clone());
        paths.push(identity.root);
    }

    // Product scope includes embedded repositories as well as submodules. The
    // walk is bounded, does not follow symlinks, respects common heavyweight
    // build-directory boundaries, and merely looks for .git markers.
    let scan_root = workspace_root.to_path_buf();
    let nested = tokio::task::spawn_blocking(move || scan_nested_repo_markers(&scan_root))
        .await
        .map_err(|err| EngineError::Other(format!("repository scan join failed: {err}")))?;
    for path in nested {
        if seen.insert(path.clone()) {
            paths.push(path);
        }
    }

    // Git config handles quoting/escaping in .gitmodules. Iterate because an
    // initialized submodule may itself declare more submodules.
    let mut ix = 0;
    while ix < paths.len() && paths.len() < NESTED_REPO_SCAN_MAX_REPOS {
        let parent = paths[ix].clone();
        ix += 1;
        if !repos.is_repo(&parent).await {
            continue;
        }
        for relative in submodule_paths(repos, &parent).await {
            let path = normalize_lexical(&parent.join(relative));
            let canonical = std::fs::canonicalize(&path).unwrap_or(path);
            if seen.insert(canonical.clone()) {
                paths.push(canonical);
            }
        }
    }

    paths.sort_by_key(|path| path.components().count());
    Ok(paths)
}

async fn submodule_paths(repos: &Repos, repo: &Path) -> Vec<PathBuf> {
    let file = repo.join(".gitmodules");
    if !file.is_file() {
        return Vec::new();
    }
    let file = file.to_string_lossy();
    repos
        .git(
            &[
                "config",
                "-z",
                "--file",
                &file,
                "--get-regexp",
                r"^submodule\..*\.path$",
            ],
            Some(repo),
        )
        .await
        .ok()
        .into_iter()
        .flat_map(|output| {
            output
                .split('\0')
                .filter_map(|record| record.split_once('\n').map(|(_, path)| PathBuf::from(path)))
                .collect::<Vec<_>>()
        })
        .collect()
}

pub(crate) fn parse_worktree_porcelain_z(output: &str) -> Vec<WorktreeRecord> {
    output
        .split("\0\0")
        .filter_map(|stanza| {
            let mut path = None;
            let mut head_sha = None;
            let mut branch = None;
            let mut locked = false;
            let mut prunable = false;
            for field in stanza.split('\0').filter(|field| !field.is_empty()) {
                if let Some(value) = field.strip_prefix("worktree ") {
                    path = Some(PathBuf::from(value));
                } else if let Some(value) = field.strip_prefix("HEAD ") {
                    head_sha = Some(value.to_string());
                } else if let Some(value) = field.strip_prefix("branch refs/heads/") {
                    branch = Some(value.to_string());
                } else if field == "locked" || field.starts_with("locked ") {
                    locked = true;
                } else if field == "prunable" || field.starts_with("prunable ") {
                    prunable = true;
                }
            }
            Some(WorktreeRecord {
                path: path?,
                head_sha,
                branch,
                locked,
                prunable,
            })
        })
        .collect()
}

fn scan_nested_repo_markers(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut queue = VecDeque::from([(root.to_path_buf(), 0usize)]);
    let mut visited = 0usize;
    while let Some((directory, depth)) = queue.pop_front() {
        if visited >= NESTED_REPO_SCAN_MAX_DIRS || found.len() >= NESTED_REPO_SCAN_MAX_REPOS {
            break;
        }
        visited += 1;
        if depth > 0 && std::fs::symlink_metadata(directory.join(".git")).is_ok() {
            found.push(std::fs::canonicalize(&directory).unwrap_or(directory.clone()));
        }
        if depth >= NESTED_REPO_SCAN_MAX_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if matches!(
                name.as_ref(),
                ".git" | ".worktrees" | "node_modules" | "target" | ".cache" | "build" | "dist"
            ) {
                continue;
            }
            if entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
            {
                queue.push_back((entry.path(), depth + 1));
            }
        }
    }
    found
}

fn normalize_lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// `{prefix}:` + sha256(device NUL path). With prefix `repo` and the
/// canonical git common dir this is the topology repository id that
/// execution configurations bind to.
pub(crate) fn stable_id(prefix: &str, device_id: &str, path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(device_id.as_bytes());
    hasher.update([0]);
    hasher.update(path.to_string_lossy().as_bytes());
    format!("{prefix}:{}", crate::repos::hex(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_detached_locked_and_prunable_worktrees() {
        let input = concat!(
            "worktree /repo\0HEAD abc\0branch refs/heads/main\0\0",
            "worktree /tmp/tree with spaces\0HEAD def\0detached\0locked reason\0prunable stale\0\0",
        );
        let records = parse_worktree_porcelain_z(input);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].branch.as_deref(), Some("main"));
        assert_eq!(records[1].path, PathBuf::from("/tmp/tree with spaces"));
        assert_eq!(records[1].branch, None);
        assert!(records[1].locked);
        assert!(records[1].prunable);
    }

    #[test]
    fn lexical_normalization_handles_uninitialized_submodule_parent_segments() {
        assert_eq!(
            normalize_lexical(Path::new("/repo/crates/../vendor/tool")),
            PathBuf::from("/repo/vendor/tool")
        );
    }
}
