//! Is a newer Mast build installed than the one running? And the restart
//! state machine that acts on it.
//!
//! Detection is layered, most robust first:
//! 1. **Identity.** At launch we record the executable's [`BuildStamp`]
//!    (inode, mtime, length). The coordinator's install is `cp` to a temp file
//!    then `mv -f` over the bundle executable, which swaps the inode and
//!    stamps a new mtime, so the same path later reads as a different stamp
//!    while this process keeps running the old image.
//! 2. **Build id** (best effort, only for "what changed"). `apps/mast` embeds
//!    `MAST-BUILD-ID:<sha>:<git dir>` in the binary; [`read_marker`] finds it
//!    in the installed file without running it, and `git log` over
//!    `running..installed` lists the merges in between.
//!
//! Everything here is pure or a read-only stat/scan. Relaunching lives in
//! `shell/running_ui.rs`; [`RestartPlan`] only decides when.

use std::path::Path;

/// Prefix of the build marker. Assembled at runtime so this crate's own
/// string table never contains the contiguous needle (only `apps/mast` does).
fn marker_prefix() -> String {
    ["MAST-BUILD", "-ID:"].concat()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildStamp {
    pub inode: u64,
    pub mtime_ms: i64,
    pub len: u64,
}

impl BuildStamp {
    pub fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime_ms = meta
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_millis() as i64;
        #[cfg(unix)]
        let inode = std::os::unix::fs::MetadataExt::ino(&meta);
        #[cfg(not(unix))]
        let inode = 0;
        Some(Self {
            inode,
            mtime_ms,
            len: meta.len(),
        })
    }
}

/// The installed executable differs from the one this process launched from.
/// `None` (unreadable, mid-install) is never "newer": wait for the next look.
pub fn is_newer_installed(running: &BuildStamp, installed: Option<&BuildStamp>) -> bool {
    installed.is_some_and(|installed| installed != running)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildId {
    pub sha: String,
    /// The repository's git dir at build time, for `git log`.
    pub git_dir: String,
}

/// Parse the text after the marker prefix: `<sha>:<git dir>`.
pub fn parse_marker(rest: &str) -> Option<BuildId> {
    let rest = rest.split('\0').next()?;
    let (sha, git_dir) = rest.split_once(':')?;
    let sha = sha.trim();
    (sha.len() >= 7 && sha.bytes().all(|b| b.is_ascii_hexdigit())).then(|| BuildId {
        sha: sha.to_owned(),
        git_dir: git_dir.to_owned(),
    })
}

/// Find the build marker in an executable by scanning its bytes in chunks.
/// Read-only; the caller runs it off the UI thread and only when the stamp
/// changed.
pub fn read_marker(path: &Path) -> Option<BuildId> {
    read_marker_from(std::fs::File::open(path).ok()?, 1 << 20)
}

fn read_marker_from(mut reader: impl std::io::Read, chunk: usize) -> Option<BuildId> {
    // The value is a sha and a path: well under TAIL bytes. A match is only
    // taken once TAIL bytes follow it (or the input ended), and the last TAIL
    // bytes are carried into the next read so none is split by a boundary.
    const TAIL: usize = 1024;
    let needle = marker_prefix().into_bytes();
    let mut buf: Vec<u8> = Vec::with_capacity(chunk + TAIL);
    let mut scratch = vec![0u8; chunk];
    loop {
        let read = reader.read(&mut scratch).ok()?;
        let eof = read == 0;
        buf.extend_from_slice(&scratch[..read]);
        if let Some(at) = find(&buf, &needle)
            && (eof || buf.len() - at >= TAIL)
        {
            let stop = (at + needle.len() + TAIL).min(buf.len());
            let text = String::from_utf8_lossy(&buf[at + needle.len()..stop]).into_owned();
            return parse_marker(&text);
        }
        if eof {
            return None;
        }
        if buf.len() > TAIL {
            buf.drain(..buf.len() - TAIL);
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

static RUNNING: std::sync::OnceLock<BuildId> = std::sync::OnceLock::new();

/// Called once at startup with the ids compiled into this binary.
pub fn set_running_build(sha: &str, git_dir: &str) {
    if let Some(id) = parse_marker(&format!("{sha}:{git_dir}")) {
        let _ = RUNNING.set(id);
    }
}

pub fn running_build() -> Option<&'static BuildId> {
    RUNNING.get()
}

/// The merges between this process's build and the installed executable's,
/// newest first. Blocking (file scan + `git log`): call off the UI thread.
/// `None` when either side has no build id, git fails, or they are unrelated.
pub fn changes_since(running: &BuildId, installed_exe: &Path) -> Option<Vec<Change>> {
    let installed = read_marker(installed_exe)?;
    if installed.sha == running.sha {
        return Some(Vec::new());
    }
    let output = mast_harness::git_command()
        .arg("--git-dir")
        .arg(&installed.git_dir)
        .args(["log", "--first-parent", "--format=%s"])
        .arg(format!("{}..{}", running.sha, installed.sha))
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| parse_changes(&String::from_utf8_lossy(&output.stdout)))
}

/// One merge between the running build and the installed one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The branch name for a merge commit, else the commit subject.
    pub label: String,
    pub subject: String,
}

/// Parse `git log --first-parent --format=%s` output (newest first).
pub fn parse_changes(log: &str) -> Vec<Change> {
    log.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|subject| Change {
            label: branch_of(subject).unwrap_or_else(|| subject.to_owned()),
            subject: subject.to_owned(),
        })
        .collect()
}

/// "Merge running-chats-popover: ..." / "Merge branch 'x' into y" → the branch.
fn branch_of(subject: &str) -> Option<String> {
    let rest = subject.strip_prefix("Merge ")?;
    let rest = rest.strip_prefix("branch ").unwrap_or(rest);
    let rest = rest.trim_start_matches('\'');
    let end = rest.find(['\'', ':', ' ']).unwrap_or(rest.len());
    let name = &rest[..end];
    (!name.is_empty()).then(|| name.to_owned())
}

/// What the lamp and popover know about a waiting build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub installed: BuildStamp,
    /// `None` until (or unless) the marker scan and `git log` produce a list.
    pub changes: Option<Vec<Change>>,
}

// ---------------------------------------------------------------------------
// Restart state machine
// ---------------------------------------------------------------------------

/// Consecutive idle observations before "restart when idle" fires, so the
/// instant between one turn ending and a queued one starting is not "idle".
pub const IDLE_TICKS_TO_FIRE: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RestartState {
    #[default]
    Idle,
    /// Armed: relaunch once nothing is mid-turn.
    WhenIdle { idle_ticks: u8 },
    /// "Restart now…" asked and chats are mid-turn: waiting on the sheet.
    Confirming,
    Relaunching,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartAction {
    None,
    Relaunch,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RestartPlan {
    pub state: RestartState,
}

impl RestartPlan {
    pub fn is_armed(&self) -> bool {
        matches!(self.state, RestartState::WhenIdle { .. })
    }

    pub fn arm(&mut self) {
        if self.state != RestartState::Relaunching {
            self.state = RestartState::WhenIdle { idle_ticks: 0 };
        }
    }

    /// "Restart now…": immediate with nothing mid-turn, else the sheet.
    pub fn request_now(&mut self, mid_turn: usize) -> RestartAction {
        if self.state == RestartState::Relaunching {
            return RestartAction::None;
        }
        if mid_turn == 0 {
            self.state = RestartState::Relaunching;
            RestartAction::Relaunch
        } else {
            self.state = RestartState::Confirming;
            RestartAction::None
        }
    }

    /// "Restart anyway" on the sheet.
    pub fn confirm(&mut self) -> RestartAction {
        if self.state == RestartState::Confirming {
            self.state = RestartState::Relaunching;
            RestartAction::Relaunch
        } else {
            RestartAction::None
        }
    }

    /// "Wait" / "Cancel": back to nothing planned.
    pub fn cancel(&mut self) {
        if self.state != RestartState::Relaunching {
            self.state = RestartState::Idle;
        }
    }

    /// The 1 s heartbeat: `mid_turn` is the number of chats mid-turn now.
    pub fn tick(&mut self, mid_turn: usize) -> RestartAction {
        let RestartState::WhenIdle { idle_ticks } = self.state else {
            return RestartAction::None;
        };
        if mid_turn > 0 {
            self.state = RestartState::WhenIdle { idle_ticks: 0 };
            return RestartAction::None;
        }
        if idle_ticks + 1 >= IDLE_TICKS_TO_FIRE {
            self.state = RestartState::Relaunching;
            RestartAction::Relaunch
        } else {
            self.state = RestartState::WhenIdle {
                idle_ticks: idle_ticks + 1,
            };
            RestartAction::None
        }
    }

    /// The pending build went away (rolled back, or this process is the new
    /// build): drop any plan that was waiting on it.
    pub fn build_gone(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests;
