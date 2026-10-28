//! Skills read straight from disk, the way Claude Code finds them: every
//! `.claude/skills/<dir>/SKILL.md` from a folder up to its nearest git root
//! (project skills, nearest folder first), plus `~/.claude/skills` (user
//! skills). Only the frontmatter's `name`, `description`, and
//! `user-invocable` are read; a skill marked `user-invocable: false` is not
//! offered.
//!
//! The instruction editors union this with the engine's discovery so their
//! `/` list never depends on which harness is selected or on the CLI's
//! handshake succeeding.
use std::path::{Path, PathBuf};

use mast_proto::SlashCommand;

/// The skills found for one folder.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct LocalSkills {
    /// `.claude/skills` from the folder up to its git root, nearest first.
    pub project: Vec<SlashCommand>,
    /// `~/.claude/skills`.
    pub user: Vec<SlashCommand>,
}

/// Read the skills for `root` (and `home`'s user skills). Missing folders
/// and unreadable files are skipped, never errors.
pub(crate) fn scan(root: &Path, home: Option<&Path>) -> LocalSkills {
    let project = project_dirs(root, home)
        .iter()
        .flat_map(|dir| read_skills_dir(&dir.join(".claude").join("skills")))
        .collect();
    let user = home
        .map(|home| read_skills_dir(&home.join(".claude").join("skills")))
        .unwrap_or_default();
    LocalSkills { project, user }
}

/// `root`, then each parent up to and including the nearest folder holding
/// `.git` (a directory, or a file in a submodule or worktree). With no git
/// root above, or once the walk reaches `home`, only the folders seen so
/// far: never the whole filesystem, never `home` itself (its
/// `.claude/skills` are the user skills).
fn project_dirs(root: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut current = Some(root);
    while let Some(dir) = current {
        if home.is_some_and(|home| dir == home) {
            break;
        }
        dirs.push(dir.to_path_buf());
        if dir.join(".git").exists() {
            return dirs;
        }
        current = dir.parent();
    }
    dirs.truncate(1);
    dirs
}

/// Every `<dir>/*/SKILL.md` that is user-invocable, by folder name.
fn read_skills_dir(dir: &Path) -> Vec<SlashCommand> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut folders: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    folders.sort();
    folders
        .iter()
        .filter_map(|folder| {
            let text = std::fs::read_to_string(folder.join("SKILL.md")).ok()?;
            let fallback = folder.file_name()?.to_str()?;
            parse_skill(&text, fallback)
        })
        .collect()
}

/// A skill from its `SKILL.md` text; `fallback` names it when the
/// frontmatter has no `name`. `None` for `user-invocable: false`.
pub(crate) fn parse_skill(text: &str, fallback: &str) -> Option<SlashCommand> {
    let frontmatter = frontmatter(text).unwrap_or_default();
    let mut name = None;
    let mut description = String::new();
    let mut user_invocable = true;
    let mut lines = frontmatter.lines().peekable();
    while let Some(line) = lines.next() {
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "name" => name = Some(unquote(value).to_owned()).filter(|name| !name.is_empty()),
            "description" => {
                description =
                    if value.is_empty() || value.starts_with('>') || value.starts_with('|') {
                        // A block scalar: the indented lines that follow.
                        let mut parts = Vec::new();
                        while let Some(next) = lines.peek() {
                            if !next.trim().is_empty() && !next.starts_with(char::is_whitespace) {
                                break;
                            }
                            parts.push(next.trim());
                            lines.next();
                        }
                        parts.retain(|part| !part.is_empty());
                        parts.join(" ")
                    } else {
                        unquote(value).to_owned()
                    }
            }
            "user-invocable" => user_invocable = !unquote(value).eq_ignore_ascii_case("false"),
            _ => {}
        }
    }
    if !user_invocable {
        return None;
    }
    let name = name.unwrap_or_else(|| fallback.to_owned());
    if name.is_empty() || name.contains(char::is_whitespace) {
        return None;
    }
    Some(SlashCommand {
        name,
        description,
        input_hint: None,
    })
}

/// The text between a leading `---` line and the next `---` line.
fn frontmatter(text: &str) -> Option<&str> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let end = rest.match_indices("\n---").map(|(at, _)| at).find(|&at| {
        let after = &rest[at + 4..];
        after.is_empty() || after.starts_with('\n') || after.starts_with("\r\n")
    })?;
    Some(&rest[..end])
}

fn unquote(value: &str) -> &str {
    let value = value.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|value| value.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(dir: &Path, folder: &str, body: &str) {
        let path = dir.join(".claude").join("skills").join(folder);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("SKILL.md"), body).unwrap();
    }

    fn names(commands: &[SlashCommand]) -> Vec<&str> {
        commands
            .iter()
            .map(|command| command.name.as_str())
            .collect()
    }

    #[test]
    fn frontmatter_names_describes_and_hides_non_invocable_skills() {
        let skill = parse_skill(
            "---\nname: design\ndescription: Storybook design loop\nuser-invocable: true\n---\n# Body\n",
            "folder",
        )
        .unwrap();
        assert_eq!(skill.name, "design");
        assert_eq!(skill.description, "Storybook design loop");
        // Quoted values, a folded description, the folder as the name.
        let skill = parse_skill(
            "---\ndescription: >\n  Design-system allowlist\n  and rubric.\nother: 'x'\n---\n",
            "design-system",
        )
        .unwrap();
        assert_eq!(skill.name, "design-system");
        assert_eq!(skill.description, "Design-system allowlist and rubric.");
        assert_eq!(
            parse_skill("---\nname: \"quoted\"\n---\n", "f")
                .unwrap()
                .name,
            "quoted"
        );
        assert!(parse_skill("---\nname: hidden\nuser-invocable: false\n---\n", "f").is_none());
        // No frontmatter: still the folder's skill.
        assert_eq!(parse_skill("# Just a body", "plain").unwrap().name, "plain");
    }

    /// Project skills come from the folder up to its git root (a submodule's
    /// `.git` file stops the walk), nearest first; user skills from
    /// `~/.claude/skills`.
    #[test]
    fn scan_walks_up_to_the_nearest_git_root() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let space = tmp.path().join("workspace");
        let ui = space.join("ui");
        let nested = ui.join("packages").join("app");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(space.join(".git")).unwrap();
        std::fs::write(ui.join(".git"), "gitdir: ../.git/modules/ui\n").unwrap();
        write_skill(&space, "db-query", "---\nname: db-query\n---\n");
        write_skill(
            &ui,
            "design",
            "---\nname: design\ndescription: UI loop\n---\n",
        );
        write_skill(&ui, "design-system", "---\nname: design-system\n---\n");
        write_skill(&nested, "app-only", "---\nname: app-only\n---\n");
        write_skill(&home, "my-voice", "---\nname: my-voice\n---\n");

        let found = scan(&ui, Some(&home));
        assert_eq!(names(&found.project), ["design", "design-system"]);
        assert_eq!(names(&found.user), ["my-voice"]);
        let found = scan(&nested, Some(&home));
        assert_eq!(
            names(&found.project),
            ["app-only", "design", "design-system"]
        );
        // The Space folder is its own git root.
        assert_eq!(names(&scan(&space, Some(&home)).project), ["db-query"]);
        // No git root above: only the folder itself.
        let loose = tmp.path().join("loose").join("deep");
        std::fs::create_dir_all(&loose).unwrap();
        write_skill(
            &tmp.path().join("loose"),
            "parent",
            "---\nname: parent\n---\n",
        );
        assert!(scan(&loose, Some(&home)).project.is_empty());
        // A missing folder is empty, not an error.
        assert!(scan(&tmp.path().join("missing"), None).project.is_empty());
    }
}
