//! Per-session on-disk event journal (port of zeron's `run-journal.ts`, JSONL-shaped).
//!
//! One append-only JSONL file per chat under `{data_dir}/journals/{chat_id}.jsonl`; each
//! line is `{"seq": n, "event": AgentEvent}` with a monotonically increasing `seq`. The
//! journal is the durable replay source for live streams (`Subscribe` = replay then tail
//! the broadcast hub) and the crash-recovery gauge: a journal whose LAST event is not
//! `Done` belongs to a run that died mid-stream — boot recovery stamps its doc entry
//! `aborted` and closes the journal with a synthetic `Done`.
//!
//! Bounded-window compaction is deferred (whole file kept for now, per M2 scope); a torn
//! trailing line from a crash mid-write is tolerated everywhere.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use mast_proto::{AgentEvent, InstructionRevisionPin};

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalLine {
    seq: u64,
    event: AgentEvent,
}

struct ChatJournal {
    file: File,
    next_seq: u64,
    /// True when the file ends without a newline (torn write) — the next append
    /// starts with one so the torn line stays isolated.
    needs_newline: bool,
}

/// Append-only JSONL journal store, one file per chat.
pub struct RunJournal {
    dir: PathBuf,
    open_files: Mutex<HashMap<String, ChatJournal>>,
    /// Serializes read-modify-write of the instruction sidecars.
    instruction_lock: Mutex<()>,
}

/// Turn pins kept per chat (oldest dropped first): enough for every Turn a
/// crash, retry, or queue promotion can still revive.
const INSTRUCTION_TURNS_KEPT: usize = 256;

/// Host-local Work Outline delivery record for one chat
/// (`<journals>/<chat>.instructions.json`, `m0-instruction-delivery.md` 7):
/// which revision each Turn was pinned to and which revision the chat's
/// native session actually received. Engine-built follow-up runs (crash
/// recovery, retries, steer fallbacks, queue promotion after a restart) take
/// their revision from here, never from the newest revision.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionSidecar {
    /// Native session the `applied` field describes.
    #[serde(default)]
    pub native_session_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    /// What that native session received (`None`: no instructions).
    #[serde(default)]
    pub applied: Option<InstructionRevisionPin>,
    /// Turn pins by user message id, oldest first.
    #[serde(default)]
    pub turns: Vec<InstructionTurnPin>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionTurnPin {
    pub message_id: String,
    pub pin: InstructionRevisionPin,
    /// The user confirmed that this Turn may start a fresh native session
    /// to apply `pin` (`FreshSession` harnesses). Kept until the Turn's
    /// session starts, so a crash or restart before `SessionStarted` revives
    /// the Turn without asking again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub context_reset_confirmed: bool,
}

impl InstructionSidecar {
    pub fn turn(&self, message_id: &str) -> Option<&InstructionRevisionPin> {
        self.turns
            .iter()
            .rev()
            .find(|t| t.message_id == message_id)
            .map(|t| &t.pin)
    }
}

impl RunJournal {
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, JournalError> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            open_files: Mutex::new(HashMap::new()),
            instruction_lock: Mutex::new(()),
        })
    }

    fn instructions_path(&self, chat_id: &str) -> PathBuf {
        self.dir
            .join(format!("{}.instructions.json", sanitize_id(chat_id)))
    }

    /// The chat's instruction sidecar, if one was ever written.
    pub fn instruction_sidecar(&self, chat_id: &str) -> Option<InstructionSidecar> {
        let bytes = std::fs::read(self.instructions_path(chat_id)).ok()?;
        match serde_json::from_slice(&bytes) {
            Ok(sidecar) => Some(sidecar),
            Err(err) => {
                tracing::warn!(chat = %chat_id, error = %err, "unreadable instruction sidecar");
                None
            }
        }
    }

    /// The revision a Turn (by user message id) was pinned to.
    pub fn instruction_turn(
        &self,
        chat_id: &str,
        message_id: &str,
    ) -> Option<InstructionRevisionPin> {
        self.instruction_sidecar(chat_id)?.turn(message_id).cloned()
    }

    /// Record a Turn's pin BEFORE its run spawns (fsynced), so a crash at any
    /// later point revives the Turn with exactly this revision. A context
    /// reset confirmation for the same pin is sticky until
    /// [`Self::clear_instruction_turn_reset_confirmation`].
    pub fn note_instruction_turn(
        &self,
        chat_id: &str,
        message_id: &str,
        pin: &InstructionRevisionPin,
        context_reset_confirmed: bool,
    ) -> Result<(), JournalError> {
        self.update_instruction_sidecar(chat_id, |sidecar| {
            let existing = sidecar
                .turns
                .iter()
                .rev()
                .find(|t| t.message_id == message_id)
                .filter(|t| t.pin == *pin)
                .map(|t| t.context_reset_confirmed);
            if existing.is_some_and(|confirmed| confirmed || !context_reset_confirmed) {
                return false;
            }
            sidecar.turns.retain(|t| t.message_id != message_id);
            sidecar.turns.push(InstructionTurnPin {
                message_id: message_id.to_string(),
                pin: pin.clone(),
                context_reset_confirmed,
            });
            let excess = sidecar.turns.len().saturating_sub(INSTRUCTION_TURNS_KEPT);
            sidecar.turns.drain(..excess);
            true
        })
    }

    /// Whether the Turn's user confirmed a context reset for exactly `pin`.
    pub fn instruction_turn_reset_confirmed(
        &self,
        chat_id: &str,
        message_id: &str,
        pin: &InstructionRevisionPin,
    ) -> bool {
        self.instruction_sidecar(chat_id).is_some_and(|sidecar| {
            sidecar
                .turns
                .iter()
                .rev()
                .find(|t| t.message_id == message_id)
                .is_some_and(|t| t.pin == *pin && t.context_reset_confirmed)
        })
    }

    /// The Turn's session started: its context reset is spent.
    pub fn clear_instruction_turn_reset_confirmation(
        &self,
        chat_id: &str,
        message_id: &str,
    ) -> Result<(), JournalError> {
        if self.instruction_sidecar(chat_id).is_none() {
            return Ok(());
        }
        self.update_instruction_sidecar(chat_id, |sidecar| {
            let mut changed = false;
            for turn in sidecar
                .turns
                .iter_mut()
                .filter(|t| t.message_id == message_id && t.context_reset_confirmed)
            {
                turn.context_reset_confirmed = false;
                changed = true;
            }
            changed
        })
    }

    /// Record what a native session received (on `SessionStarted`).
    pub fn note_instruction_session(
        &self,
        chat_id: &str,
        native_session_id: &str,
        cwd: &str,
        applied: Option<&InstructionRevisionPin>,
    ) -> Result<(), JournalError> {
        self.update_instruction_sidecar(chat_id, |sidecar| {
            let next = (
                Some(native_session_id.to_string()),
                Some(cwd.to_string()),
                applied.cloned(),
            );
            if (
                sidecar.native_session_id.clone(),
                sidecar.cwd.clone(),
                sidecar.applied.clone(),
            ) == next
            {
                return false;
            }
            (sidecar.native_session_id, sidecar.cwd, sidecar.applied) = next;
            true
        })
    }

    fn update_instruction_sidecar(
        &self,
        chat_id: &str,
        update: impl FnOnce(&mut InstructionSidecar) -> bool,
    ) -> Result<(), JournalError> {
        let _guard = self
            .instruction_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut sidecar = self.instruction_sidecar(chat_id).unwrap_or_default();
        if !update(&mut sidecar) {
            return Ok(());
        }
        let path = self.instructions_path(chat_id);
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        {
            let mut file = File::create(&tmp)?;
            file.write_all(&serde_json::to_vec(&sidecar)?)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, &path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, ChatJournal>> {
        self.open_files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn path_for(&self, chat_id: &str) -> PathBuf {
        self.dir.join(format!("{}.jsonl", sanitize_id(chat_id)))
    }

    fn attempts_path(&self, chat_id: &str) -> PathBuf {
        self.dir.join(format!("{}.resume", sanitize_id(chat_id)))
    }

    fn model_checked_path(&self, chat_id: &str) -> PathBuf {
        self.dir
            .join(format!("{}.modelchecked", sanitize_id(chat_id)))
    }

    /// Journal size in bytes (append-only, so growth means new events).
    pub fn byte_len(&self, chat_id: &str) -> u64 {
        std::fs::metadata(self.path_for(chat_id)).map_or(0, |m| m.len())
    }

    /// Journal length at which the actual-model backfill last found no model.
    pub fn model_checked_len(&self, chat_id: &str) -> Option<u64> {
        std::fs::read_to_string(self.model_checked_path(chat_id))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// Record "checked at this length, no model" in a sidecar (never the journal).
    pub fn mark_model_checked(&self, chat_id: &str, len: u64) {
        if let Err(err) = std::fs::write(self.model_checked_path(chat_id), len.to_string()) {
            tracing::warn!(chat = %chat_id, error = %err, "model-checked marker write failed");
        }
    }

    fn graceful_restart_path(&self, chat_id: &str) -> PathBuf {
        self.dir.join(format!("{}.restart", sanitize_id(chat_id)))
    }

    /// Stable Mast-owned working directory for a project-less chat. Keeping
    /// this beside (rather than inside) `journals/` lets journal scans remain
    /// file-only and gives every chat its own sandbox root.
    pub(crate) fn projectless_cwd(&self, chat_id: &str) -> Result<PathBuf, JournalError> {
        let profile_root = self.dir.parent().unwrap_or(&self.dir);
        let cwd = profile_root.join("projectless").join(sanitize_id(chat_id));
        std::fs::create_dir_all(&cwd)?;
        Ok(cwd)
    }

    /// Mark a turn that the application interrupted only because it is
    /// shutting down. Unlike an explicit user Stop, this turn should be
    /// picked back up on the next boot even though graceful interruption
    /// leaves a terminal `Done { interrupted }` in the journal.
    pub fn mark_graceful_restart(&self, chat_id: &str) {
        let marked_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .to_string();
        if let Err(err) = std::fs::write(self.graceful_restart_path(chat_id), marked_at) {
            tracing::warn!(chat = %chat_id, error = %err, "restart-resume marker write failed");
        }
    }

    pub fn clear_graceful_restart(&self, chat_id: &str) {
        let _ = std::fs::remove_file(self.graceful_restart_path(chat_id));
    }

    pub fn graceful_restart_marked(&self, chat_id: &str) -> bool {
        self.graceful_restart_path(chat_id).is_file()
    }

    pub fn graceful_restart_marked_at(&self, chat_id: &str) -> Option<i64> {
        std::fs::read_to_string(self.graceful_restart_path(chat_id))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// Chats whose in-flight turn was interrupted by graceful application
    /// shutdown. Marker names use the same conservative chat-id encoding as
    /// journals; production chat ids are UUIDs, so the stem is the id.
    pub fn graceful_restart_sessions(&self) -> Result<Vec<String>, JournalError> {
        let mut marked = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("restart") {
                continue;
            }
            if let Some(chat_id) = path.file_stem().and_then(|s| s.to_str()) {
                marked.push(chat_id.to_string());
            }
        }
        marked.sort();
        Ok(marked)
    }

    /// Auto-resume revival budget (zeron `resumeAttempt`/`MAX_AUTO_RESUME`):
    /// persisted beside the journal so a run that CRASHES THE ENGINE cannot
    /// revive itself in an infinite boot loop.
    pub fn resume_attempts(&self, chat_id: &str) -> u32 {
        std::fs::read_to_string(self.attempts_path(chat_id))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    pub fn note_resume_attempt(&self, chat_id: &str) -> u32 {
        let next = self.resume_attempts(chat_id) + 1;
        if let Err(err) = std::fs::write(self.attempts_path(chat_id), next.to_string()) {
            tracing::warn!(chat = %chat_id, error = %err, "resume-attempt ledger write failed");
        }
        next
    }

    /// A cleanly completed turn resets the budget — only consecutive
    /// crash-revive-crash cycles exhaust it.
    pub fn clear_resume_attempts(&self, chat_id: &str) {
        let _ = std::fs::remove_file(self.attempts_path(chat_id));
    }

    /// Append one event; returns its journal seq.
    pub fn append(&self, chat_id: &str, event: &AgentEvent) -> Result<u64, JournalError> {
        let mut files = self.lock();
        if !files.contains_key(chat_id) {
            // Bound the open-fd set: entries were never removed, so every chat
            // ever run held a descriptor for the process lifetime. Dropping is
            // safe — the next append reopens and rescans the tail. The cap
            // comfortably exceeds concurrent runs, so eviction stays rare.
            const OPEN_FILE_CAP: usize = 16;
            if files.len() >= OPEN_FILE_CAP {
                files.clear();
            }
            let path = self.path_for(chat_id);
            let (next_seq, needs_newline) = scan_tail(&path)?;
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            files.insert(
                chat_id.to_string(),
                ChatJournal {
                    file,
                    next_seq,
                    needs_newline,
                },
            );
        }
        // Entry guaranteed present; avoid unwrap in a library path regardless.
        let Some(journal) = files.get_mut(chat_id) else {
            return Err(JournalError::Io(std::io::Error::other(
                "journal entry vanished under lock",
            )));
        };
        let seq = journal.next_seq;
        let line = serde_json::to_string(&JournalLine {
            seq,
            event: event.clone(),
        })?;
        let mut buf = Vec::with_capacity(line.len() + 2);
        if journal.needs_newline {
            buf.push(b'\n');
        }
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
        journal.file.write_all(&buf)?;
        journal.file.flush()?;
        journal.needs_newline = false;
        journal.next_seq = seq + 1;
        Ok(seq)
    }

    /// Events with `seq > after_seq`, in order. A cursor ahead of the last issued seq is
    /// from a previous era (file replaced) — falls back to a full replay, mirroring zeron.
    pub fn replay(
        &self,
        chat_id: &str,
        after_seq: u64,
    ) -> Result<Vec<(u64, AgentEvent)>, JournalError> {
        let path = self.path_for(chat_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let all = read_lines(&path)?;
        let last_seq = all.last().map(|(seq, _)| *seq).unwrap_or(0);
        let from = if after_seq > last_seq { 0 } else { after_seq };
        Ok(all.into_iter().filter(|(seq, _)| *seq > from).collect())
    }

    /// The last event in a chat's journal, if any (ignores a torn tail line).
    pub fn last_event(&self, chat_id: &str) -> Result<Option<(u64, AgentEvent)>, JournalError> {
        let path = self.path_for(chat_id);
        if !path.exists() {
            return Ok(None);
        }
        last_valid_line(&path)
    }

    /// The first `Some` of `f` over a chat's events, newest first, reading
    /// only as much of the file as the search needs.
    pub fn find_rev<R>(
        &self,
        chat_id: &str,
        f: impl FnMut(u64, AgentEvent) -> Option<R>,
    ) -> Result<Option<R>, JournalError> {
        find_rev_in(&self.path_for(chat_id), f)
    }

    /// The events of the journal's final turn, in order: everything after the
    /// last `Done` that is not the journal's final event (exclusive), through
    /// the end. A journal with no such `Done` yields every event, like
    /// [`Self::replay`] from 0.
    pub fn replay_last_turn(&self, chat_id: &str) -> Result<Vec<(u64, AgentEvent)>, JournalError> {
        let mut turn: Vec<(u64, AgentEvent)> = Vec::new();
        self.find_rev(chat_id, |seq, event| {
            // The final event may itself be the turn's `Done`; any other
            // `Done` ends the previous turn.
            if !turn.is_empty() && matches!(event, AgentEvent::Done { .. }) {
                return Some(());
            }
            turn.push((seq, event));
            None
        })?;
        turn.reverse();
        Ok(turn)
    }

    /// Last filesystem modification time for a chat journal, in epoch millis.
    /// Recovery uses this to distinguish a response lost before persistence
    /// from a newer user message that was intentionally queued after the last
    /// completed journaled turn.
    pub fn modified_at_ms(&self, chat_id: &str) -> Option<i64> {
        let modified = std::fs::metadata(self.path_for(chat_id))
            .ok()?
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        Some(modified.as_millis().min(i64::MAX as u128) as i64)
    }

    /// Crash-recovery scan: chat ids whose journal's last event is NOT a `Done` — their
    /// runs died mid-stream and need recovery (stamp `aborted`, close the journal).
    pub fn stale_sessions(&self) -> Result<Vec<String>, JournalError> {
        let mut stale = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(chat_id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let last = last_valid_line(&path)?;
            match last {
                Some((_, AgentEvent::Done { .. })) | None => {}
                Some(_) => stale.push(chat_id.to_string()),
            }
        }
        stale.sort();
        Ok(stale)
    }

    /// Remove a chat's journal file entirely (tests / future compaction).
    pub fn discard(&self, chat_id: &str) -> Result<(), JournalError> {
        self.lock().remove(chat_id);
        let path = self.path_for(chat_id);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        let _ = std::fs::remove_file(self.model_checked_path(chat_id));
        Ok(())
    }
}

/// Parse every valid line; malformed lines (torn tail writes) are skipped.
fn read_lines(path: &Path) -> Result<Vec<(u64, AgentEvent)>, JournalError> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<JournalLine>(&line) {
            Ok(parsed) => out.push((parsed.seq, parsed.event)),
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "journal: skipping malformed line");
            }
        }
    }
    Ok(out)
}

/// Visit the file's lines from the last to the first, reading backwards in
/// chunks; `visit` returns `true` to stop. Cost is proportional to what is
/// visited, not to the file (journals reach tens of MB).
fn rev_lines(path: &Path, mut visit: impl FnMut(&[u8]) -> bool) -> Result<(), JournalError> {
    use std::io::{Read, Seek, SeekFrom};
    const CHUNK: u64 = 64 * 1024;
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let mut end = file.metadata()?.len();
    // Bytes of the line cut by the previous chunk boundary (its start is in
    // the chunk still to be read).
    let mut carried: Vec<u8> = Vec::new();
    loop {
        let start = end.saturating_sub(CHUNK);
        let mut buf = vec![0u8; (end - start) as usize];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buf)?;
        buf.extend_from_slice(&carried);
        let mut region_end = buf.len();
        while let Some(newline) = buf[..region_end].iter().rposition(|b| *b == b'\n') {
            if visit(&buf[newline + 1..region_end]) {
                return Ok(());
            }
            region_end = newline;
        }
        if start == 0 {
            visit(&buf[..region_end]);
            return Ok(());
        }
        carried = buf[..region_end].to_vec();
        end = start;
    }
}

fn parse_journal_line(line: &[u8]) -> Option<(u64, AgentEvent)> {
    let line = line.trim_ascii();
    if line.is_empty() {
        return None;
    }
    serde_json::from_slice::<JournalLine>(line)
        .ok()
        .map(|parsed| (parsed.seq, parsed.event))
}

/// The first `Some` of `f` over the parseable events, newest first. A torn
/// tail line is skipped exactly as [`read_lines`] skips it.
fn find_rev_in<R>(
    path: &Path,
    mut f: impl FnMut(u64, AgentEvent) -> Option<R>,
) -> Result<Option<R>, JournalError> {
    let mut found = None;
    rev_lines(path, |line| {
        let Some((seq, event)) = parse_journal_line(line) else {
            return false;
        };
        found = f(seq, event);
        found.is_some()
    })?;
    Ok(found)
}

fn last_valid_line(path: &Path) -> Result<Option<(u64, AgentEvent)>, JournalError> {
    find_rev_in(path, |seq, event| Some((seq, event)))
}

/// Next seq (last valid seq + 1, starting at 1) and whether the file ends mid-line.
fn scan_tail(path: &Path) -> Result<(u64, bool), JournalError> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((1, false)),
        Err(e) => return Err(e.into()),
    };
    let len = file.metadata()?.len();
    let needs_newline = if len == 0 {
        false
    } else {
        let mut last = [0u8; 1];
        file.seek(SeekFrom::Start(len - 1))?;
        file.read_exact(&mut last)?;
        last[0] != b'\n'
    };
    drop(file);
    let next_seq = last_valid_line(path)?.map(|(seq, _)| seq + 1).unwrap_or(1);
    Ok((next_seq, needs_newline))
}

/// Journal (`.jsonl`) and resume-budget (`.resume`) paths for `chat_id` under an
/// arbitrary journals directory — profile import copies these files between
/// profiles without opening a `RunJournal`.
pub fn journal_paths(dir: &Path, chat_id: &str) -> (PathBuf, PathBuf) {
    let stem = sanitize_id(chat_id);
    (
        dir.join(format!("{stem}.jsonl")),
        dir.join(format!("{stem}.resume")),
    )
}

/// Chat ids become file names; anything outside a conservative set is replaced so a
/// hostile id cannot traverse paths. (Ids are uuids in practice.)
fn sanitize_id(chat_id: &str) -> String {
    chat_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::DoneStatus;

    fn journal_line(seq: u64, text: &str) -> String {
        serde_json::to_string(&JournalLine {
            seq,
            event: AgentEvent::TextDelta { text: text.into() },
        })
        .unwrap()
    }

    #[test]
    fn replay_last_turn_and_find_rev_read_from_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let journal = RunJournal::open(dir.path()).unwrap();
        let done = || AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        };
        let text = |t: &str| AgentEvent::TextDelta { text: t.into() };
        // Two completed turns and a third still running.
        for event in [
            text("a1"),
            text("a2"),
            done(),
            text("b1"),
            done(),
            text("c1"),
            text("c2"),
        ] {
            journal.append("chat", &event).unwrap();
        }
        let seqs =
            |events: Vec<(u64, AgentEvent)>| events.into_iter().map(|(s, _)| s).collect::<Vec<_>>();
        // The third turn is still open.
        assert_eq!(seqs(journal.replay_last_turn("chat").unwrap()), vec![6, 7]);
        journal.append("chat", &done()).unwrap();
        assert_eq!(
            seqs(journal.replay_last_turn("chat").unwrap()),
            vec![6, 7, 8]
        );
        // A journal with a single turn yields everything.
        journal.append("solo", &text("x")).unwrap();
        journal.append("solo", &done()).unwrap();
        assert_eq!(seqs(journal.replay_last_turn("solo").unwrap()), vec![1, 2]);
        assert!(journal.replay_last_turn("missing").unwrap().is_empty());
        // find_rev walks newest first and stops at the first hit.
        let mut visited = 0;
        let hit = journal
            .find_rev("chat", |seq, event| {
                visited += 1;
                matches!(event, AgentEvent::TextDelta { ref text } if text == "b1").then_some(seq)
            })
            .unwrap();
        assert_eq!(hit, Some(4));
        assert_eq!(visited, 5);
    }

    /// The backwards tail scan must agree with a full parse for every shape of
    /// file: empty, torn tail, blank lines, garbage in the middle, and lines
    /// longer than the scan's chunk size.
    #[test]
    fn last_valid_line_matches_a_full_parse() {
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(200_000);
        let cases: Vec<(&str, Vec<String>)> = vec![
            ("empty", vec![]),
            ("one", vec![journal_line(1, "a")]),
            (
                "many",
                (1..=500).map(|i| journal_line(i, "hello")).collect(),
            ),
            (
                "torn tail",
                vec![
                    journal_line(1, "a"),
                    journal_line(2, "b"),
                    "{\"seq\":3,\"ev".into(),
                ],
            ),
            (
                "garbage tail",
                vec![
                    journal_line(1, "a"),
                    "not json".into(),
                    "".into(),
                    "  ".into(),
                ],
            ),
            (
                "huge lines",
                vec![
                    journal_line(1, "a"),
                    journal_line(2, &big),
                    journal_line(3, &big),
                ],
            ),
            (
                "huge torn tail",
                vec![
                    journal_line(1, "a"),
                    journal_line(2, &big),
                    journal_line(3, &big)[..150_000].to_string(),
                ],
            ),
            ("only garbage", vec!["nope".into(), "also nope".into()]),
        ];
        for (name, lines) in cases {
            for trailing_newline in [true, false] {
                let path = dir.path().join(format!("{name}-{trailing_newline}.jsonl"));
                let mut body = lines.join("\n");
                if trailing_newline && !lines.is_empty() {
                    body.push('\n');
                }
                std::fs::write(&path, &body).unwrap();
                let expected = read_lines(&path).unwrap().into_iter().next_back();
                let got = last_valid_line(&path).unwrap();
                assert_eq!(
                    got.as_ref().map(|(seq, _)| *seq),
                    expected.as_ref().map(|(seq, _)| *seq),
                    "{name} trailing_newline={trailing_newline}"
                );
                let (next, needs_newline) = scan_tail(&path).unwrap();
                assert_eq!(next, expected.map_or(1, |(seq, _)| seq + 1), "{name}");
                assert_eq!(
                    needs_newline,
                    !body.is_empty() && !body.ends_with('\n'),
                    "{name}"
                );
            }
        }
        assert!(
            last_valid_line(&dir.path().join("missing.jsonl"))
                .unwrap()
                .is_none()
        );
    }

    fn pin(id: &str) -> InstructionRevisionPin {
        InstructionRevisionPin {
            revision_id: id.into(),
            revision_hash: format!("{id}-hash"),
        }
    }

    #[test]
    fn instruction_sidecar_records_turn_pins_and_the_applied_revision() {
        let dir = tempfile::tempdir().unwrap();
        let journal = RunJournal::open(dir.path()).unwrap();
        assert!(journal.instruction_sidecar("chat").is_none());
        journal
            .note_instruction_turn("chat", "m1", &pin("a"), false)
            .unwrap();
        journal
            .note_instruction_session("chat", "native-1", "/w", Some(&pin("a")))
            .unwrap();
        journal
            .note_instruction_turn("chat", "m2", &pin("b"), false)
            .unwrap();
        // Survives a reopen (a restart).
        let journal = RunJournal::open(dir.path()).unwrap();
        assert_eq!(journal.instruction_turn("chat", "m1"), Some(pin("a")));
        assert_eq!(journal.instruction_turn("chat", "m2"), Some(pin("b")));
        assert_eq!(journal.instruction_turn("chat", "m3"), None);
        let sidecar = journal.instruction_sidecar("chat").unwrap();
        assert_eq!(sidecar.native_session_id.as_deref(), Some("native-1"));
        assert_eq!(sidecar.applied, Some(pin("a")));
        // Not a journal: scans for stale runs ignore it.
        assert!(journal.stale_sessions().unwrap().is_empty());
    }

    #[test]
    fn a_turn_context_reset_confirmation_is_sticky_until_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let journal = RunJournal::open(dir.path()).unwrap();
        journal
            .note_instruction_turn("chat", "m1", &pin("b"), true)
            .unwrap();
        // A later unconfirmed write of the same pin (a retry) keeps it.
        journal
            .note_instruction_turn("chat", "m1", &pin("b"), false)
            .unwrap();
        let journal = RunJournal::open(dir.path()).unwrap();
        assert!(journal.instruction_turn_reset_confirmed("chat", "m1", &pin("b")));
        assert!(!journal.instruction_turn_reset_confirmed("chat", "m1", &pin("c")));
        assert!(!journal.instruction_turn_reset_confirmed("chat", "m2", &pin("b")));
        journal
            .clear_instruction_turn_reset_confirmation("chat", "m1")
            .unwrap();
        assert!(!journal.instruction_turn_reset_confirmed("chat", "m1", &pin("b")));
        assert_eq!(journal.instruction_turn("chat", "m1"), Some(pin("b")));
        // A different pin replaces the entry with its own confirmation.
        journal
            .note_instruction_turn("chat", "m1", &pin("c"), true)
            .unwrap();
        assert!(journal.instruction_turn_reset_confirmed("chat", "m1", &pin("c")));
    }

    #[test]
    fn instruction_turn_pins_are_bounded_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let journal = RunJournal::open(dir.path()).unwrap();
        for i in 0..(INSTRUCTION_TURNS_KEPT + 3) {
            journal
                .note_instruction_turn("chat", &format!("m{i}"), &pin("a"), false)
                .unwrap();
        }
        let sidecar = journal.instruction_sidecar("chat").unwrap();
        assert_eq!(sidecar.turns.len(), INSTRUCTION_TURNS_KEPT);
        assert!(sidecar.turn("m0").is_none());
        assert!(
            sidecar
                .turn(&format!("m{}", INSTRUCTION_TURNS_KEPT + 2))
                .is_some()
        );
    }

    fn text(s: &str) -> AgentEvent {
        AgentEvent::TextDelta { text: s.into() }
    }

    fn done() -> AgentEvent {
        AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        }
    }

    #[test]
    fn appends_are_monotonic_and_replayable() {
        let dir = tempfile::tempdir().unwrap();
        let journal = RunJournal::open(dir.path()).unwrap();
        assert_eq!(journal.append("chat-1", &text("a")).unwrap(), 1);
        assert_eq!(journal.append("chat-1", &text("b")).unwrap(), 2);
        assert_eq!(journal.append("chat-1", &done()).unwrap(), 3);

        let all = journal.replay("chat-1", 0).unwrap();
        assert_eq!(all.len(), 3);
        let after = journal.replay("chat-1", 2).unwrap();
        assert_eq!(after.len(), 1);
        assert!(matches!(after[0].1, AgentEvent::Done { .. }));
        // Era fallback: cursor ahead of last seq replays everything.
        assert_eq!(journal.replay("chat-1", 99).unwrap().len(), 3);
    }

    #[test]
    fn seq_continues_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let journal = RunJournal::open(dir.path()).unwrap();
            journal.append("chat-1", &text("a")).unwrap();
        }
        let journal = RunJournal::open(dir.path()).unwrap();
        assert_eq!(journal.append("chat-1", &text("b")).unwrap(), 2);
    }

    #[test]
    fn stale_scan_flags_journals_without_terminal_done() {
        let dir = tempfile::tempdir().unwrap();
        let journal = RunJournal::open(dir.path()).unwrap();
        journal.append("dead", &text("partial")).unwrap();
        journal.append("clean", &text("full")).unwrap();
        journal.append("clean", &done()).unwrap();
        assert_eq!(journal.stale_sessions().unwrap(), vec!["dead".to_string()]);
        // Closing the stale journal with a Done clears the flag.
        journal.append("dead", &done()).unwrap();
        assert!(journal.stale_sessions().unwrap().is_empty());
    }

    #[test]
    fn graceful_restart_markers_round_trip_and_enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let journal = RunJournal::open(dir.path()).unwrap();
        journal.mark_graceful_restart("chat-b");
        journal.mark_graceful_restart("chat-a");
        assert!(journal.graceful_restart_marked("chat-a"));
        assert!(journal.graceful_restart_marked_at("chat-a").is_some());
        assert_eq!(
            journal.graceful_restart_sessions().unwrap(),
            vec!["chat-a".to_string(), "chat-b".to_string()]
        );
        journal.clear_graceful_restart("chat-a");
        assert!(!journal.graceful_restart_marked("chat-a"));
        assert_eq!(
            journal.graceful_restart_sessions().unwrap(),
            vec!["chat-b".to_string()]
        );
    }

    #[test]
    fn torn_tail_line_is_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        {
            let journal = RunJournal::open(dir.path()).unwrap();
            journal.append("chat-1", &text("a")).unwrap();
        }
        // Simulate a crash mid-write: garbage with no trailing newline.
        let path = dir.path().join("chat-1.jsonl");
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"seq\":2,\"event\":{\"type\":\"textD")
            .unwrap();
        drop(f);

        let journal = RunJournal::open(dir.path()).unwrap();
        assert_eq!(journal.replay("chat-1", 0).unwrap().len(), 1);
        assert_eq!(journal.append("chat-1", &text("b")).unwrap(), 2);
        let all = journal.replay("chat-1", 0).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].0, 2);
    }
}
