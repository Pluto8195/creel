//! The running-chats list behind the status lamp: what is mid-turn, waiting on
//! the user or failed, grouped "Needs you" first and then by Space, with child
//! chats indented under their coordinator.
//!
//! [`snapshot`] is the one pure aggregation (inputs gathered by
//! `shell/running_ui.rs`, nothing fetched here). [`Snapshot::entries`] is the
//! flat, keyboard-navigable order and [`step`] moves a selection through it by
//! stable key, so a refresh never jumps the highlight.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use mast_proto::{InterruptedOutcome, InterruptedRun};

/// Above this many chats the list folds: children collapse under their
/// coordinator and every Space after the second collapses to one line.
pub const COLLAPSE_ABOVE: usize = 8;
/// Space groups open by default in a folded list.
const OPEN_SPACES_WHEN_FOLDED: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// Waiting on the user.
    Awaiting,
    Errored,
    Working,
    /// Turn over, but background work (a shell, a subagent) is still going.
    IdleBackground,
}

impl RunState {
    pub fn needs_you(self) -> bool {
        matches!(self, Self::Awaiting | Self::Errored)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Awaiting => "needs you",
            Self::Errored => "errored",
            Self::Working => "working",
            Self::IdleBackground => "idle",
        }
    }
}

/// One chat as the aggregation sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub chat_id: String,
    pub parent_id: Option<String>,
    pub space_id: Option<String>,
    pub title: String,
    pub state: RunState,
    /// Turn start (working) or last change (idle/waiting); drives "elapsed".
    pub since: Option<DateTime<Utc>>,
    /// The last tool line or message preview; empty when nothing is known.
    pub activity: String,
    /// Work item breadcrumb, root first ("Mast / Features / Popover").
    pub item_path: Option<String>,
    pub account: Option<String>,
    pub model: Option<String>,
    /// Background processes still running for an idle chat.
    pub background: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub chat_id: String,
    pub title: String,
    pub state: RunState,
    pub elapsed: String,
    pub activity: String,
    pub item_path: Option<String>,
    pub account: Option<String>,
    pub model: Option<String>,
    pub background: u32,
    /// 0 for a top-level row, 1 for a child under its coordinator.
    pub depth: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Row(Row),
    /// "▸ N more" / "▾" line that folds or unfolds `key` (a coordinator's
    /// chat id or `space:<id>`).
    Fold { key: String, hidden: usize, open: bool },
}

impl Entry {
    /// Stable identity for selection.
    pub fn key(&self) -> String {
        match self {
            Self::Row(row) => row.chat_id.clone(),
            Self::Fold { key, .. } => format!("fold:{key}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionKind {
    NeedsYou,
    Space(Option<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub kind: SectionKind,
    pub label: String,
    /// Chats in the section, folded or not.
    pub count: usize,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub sections: Vec<Section>,
    pub total: usize,
    pub needs_you: usize,
}

impl Snapshot {
    /// "Nothing running" / "9 running · 2 need you".
    pub fn headline(&self) -> String {
        match (self.total, self.needs_you) {
            (0, _) => "Nothing running".to_owned(),
            (total, 0) => format!("{total} running"),
            (total, n) => format!("{total} running \u{b7} {n} need you"),
        }
    }

    /// Every selectable entry in display order.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.sections.iter().flat_map(|section| section.entries.iter())
    }

    pub fn find(&self, key: &str) -> Option<&Entry> {
        self.entries().find(|entry| entry.key() == key)
    }
}

/// Folds the user toggled away from their default (see [`snapshot`]).
pub type Toggled = HashSet<String>;

pub fn space_key(space_id: Option<&str>) -> String {
    format!("space:{}", space_id.unwrap_or(""))
}

/// "8s", "5m", "1h 12m", "2d 3h".
pub fn format_elapsed(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => {
            let (h, m) = (secs / 3600, secs % 3600 / 60);
            if m == 0 { format!("{h}h") } else { format!("{h}h {m}m") }
        }
        _ => {
            let (d, h) = (secs / 86_400, secs % 86_400 / 3600);
            if h == 0 { format!("{d}d") } else { format!("{d}d {h}h") }
        }
    }
}

fn elapsed_secs(since: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Option<u64> {
    since.map(|since| now.signed_duration_since(since).num_seconds().max(0) as u64)
}

/// Group, order and fold `inputs`. `spaces` is the Space order and names.
pub fn snapshot(
    inputs: &[Input],
    spaces: &[(String, String)],
    toggled: &Toggled,
    now: DateTime<Utc>,
) -> Snapshot {
    let total = inputs.len();
    let folded = total > COLLAPSE_ABOVE;
    let row_of = |input: &Input, depth: u8| {
        let secs = elapsed_secs(input.since, now);
        let mut elapsed = secs.map(format_elapsed).unwrap_or_default();
        if input.state == RunState::IdleBackground && !elapsed.is_empty() {
            elapsed.push_str(" idle");
        }
        Row {
            chat_id: input.chat_id.clone(),
            title: input.title.clone(),
            state: input.state,
            elapsed,
            activity: input.activity.clone(),
            item_path: input.item_path.clone(),
            account: input.account.clone(),
            model: input.model.clone(),
            background: input.background,
            depth,
        }
    };
    // Oldest first, unknown start last, id as the tiebreak so order is stable.
    let order = |a: &&Input, b: &&Input| {
        let key = |i: &Input| (i.since.is_none(), i.since, i.chat_id.clone());
        key(a).cmp(&key(b))
    };

    let mut sections = Vec::new();
    let mut needs: Vec<&Input> = inputs.iter().filter(|i| i.state.needs_you()).collect();
    needs.sort_by(order);
    if !needs.is_empty() {
        sections.push(Section {
            kind: SectionKind::NeedsYou,
            label: "Needs you".to_owned(),
            count: needs.len(),
            entries: needs.iter().map(|i| Entry::Row(row_of(i, 0))).collect(),
        });
    }

    let running: Vec<&Input> = inputs.iter().filter(|i| !i.state.needs_you()).collect();
    let mut buckets: Vec<(Option<String>, String)> = spaces
        .iter()
        .map(|(id, name)| (Some(id.clone()), name.clone()))
        .collect();
    let known: HashSet<&str> = spaces.iter().map(|(id, _)| id.as_str()).collect();
    if running
        .iter()
        .any(|i| i.space_id.as_deref().is_none_or(|id| !known.contains(id)))
    {
        buckets.push((None, "No Space".to_owned()));
    }
    let mut opened = 0usize;
    for (space_id, name) in buckets {
        let in_space = |i: &&&Input| match &space_id {
            Some(id) => i.space_id.as_deref() == Some(id.as_str()),
            None => i.space_id.as_deref().is_none_or(|id| !known.contains(id)),
        };
        let members: Vec<&Input> = running.iter().filter(in_space).copied().collect();
        if members.is_empty() {
            continue;
        }
        let key = space_key(space_id.as_deref());
        let default_open = !folded || opened < OPEN_SPACES_WHEN_FOLDED;
        let open = default_open != toggled.contains(&key);
        opened += 1;
        let mut entries = Vec::new();
        if open {
            let member_ids: HashSet<&str> = members.iter().map(|i| i.chat_id.as_str()).collect();
            let mut children: HashMap<&str, Vec<&Input>> = HashMap::new();
            let mut top: Vec<&Input> = Vec::new();
            for input in &members {
                match input.parent_id.as_deref() {
                    Some(parent) if member_ids.contains(parent) && parent != input.chat_id => {
                        children.entry(parent).or_default().push(input)
                    }
                    _ => top.push(input),
                }
            }
            top.sort_by(order);
            for input in top {
                entries.push(Entry::Row(row_of(input, 0)));
                let Some(mut kids) = children.remove(input.chat_id.as_str()) else {
                    continue;
                };
                kids.sort_by(order);
                let child_open = !folded != toggled.contains(&input.chat_id);
                if child_open {
                    entries.extend(kids.iter().map(|k| Entry::Row(row_of(k, 1))));
                } else {
                    entries.push(Entry::Fold {
                        key: input.chat_id.clone(),
                        hidden: kids.len(),
                        open: false,
                    });
                }
            }
            // Grandchildren and parent cycles are not placed above; list them
            // flat rather than hide a running chat.
            let mut rest: Vec<&Input> = children.into_values().flatten().collect();
            rest.sort_by(order);
            entries.extend(rest.iter().map(|i| Entry::Row(row_of(i, 1))));
        } else {
            entries.push(Entry::Fold {
                key: key.clone(),
                hidden: members.len(),
                open: false,
            });
        }
        sections.push(Section {
            kind: SectionKind::Space(space_id),
            label: name,
            count: members.len(),
            entries,
        });
    }
    Snapshot {
        sections,
        total,
        needs_you: needs.len(),
    }
}

/// Move the selection `delta` entries through the flat order, wrapping. With
/// no (or a vanished) selection, down lands on the first entry and up on the
/// last. `None` for an empty list.
pub fn step(snapshot: &Snapshot, selected: Option<&str>, delta: isize) -> Option<String> {
    let keys: Vec<String> = snapshot.entries().map(Entry::key).collect();
    if keys.is_empty() {
        return None;
    }
    let len = keys.len() as isize;
    let next = match selected.and_then(|s| keys.iter().position(|k| k == s)) {
        Some(at) => (at as isize + delta).rem_euclid(len),
        None if delta >= 0 => 0,
        None => len - 1,
    };
    Some(keys[next as usize].clone())
}

/// The key to keep highlighted after a refresh: the same entry if it is still
/// listed, else the first.
pub fn reconcile(snapshot: &Snapshot, selected: Option<&str>) -> Option<String> {
    selected
        .filter(|key| snapshot.find(key).is_some())
        .map(str::to_owned)
        .or_else(|| snapshot.entries().next().map(Entry::key))
}

/// One chat in the post-restart "Interrupted" block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptedRow {
    pub chat_id: String,
    pub title: String,
    pub outcome: InterruptedOutcome,
}

/// "Restarted 09:41 · 4 interrupted": what the engine's ledger says happened
/// to turns a restart cut off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptedBlock {
    /// When the earliest listed interruption was recorded (epoch ms).
    pub restarted_at_ms: i64,
    /// Picked back up by boot recovery; clears when the turn completes.
    pub resumed: Vec<InterruptedRow>,
    /// Auto-resume exhausted or too old: waiting on a one-click decision.
    pub needs_decision: Vec<InterruptedRow>,
}

impl InterruptedBlock {
    pub fn total(&self) -> usize {
        self.resumed.len() + self.needs_decision.len()
    }
}

/// Group the ledger for display. `title_of` is `None` for a chat that no
/// longer exists or is archived; those entries are left out.
pub fn interrupted_block(
    runs: &[InterruptedRun],
    title_of: impl Fn(&str) -> Option<String>,
) -> Option<InterruptedBlock> {
    let mut block = InterruptedBlock {
        restarted_at_ms: i64::MAX,
        resumed: Vec::new(),
        needs_decision: Vec::new(),
    };
    for run in runs {
        let Some(title) = title_of(&run.chat_id) else {
            continue;
        };
        block.restarted_at_ms = block.restarted_at_ms.min(run.interrupted_at_ms);
        let row = InterruptedRow {
            chat_id: run.chat_id.clone(),
            title,
            outcome: run.outcome,
        };
        match run.outcome {
            InterruptedOutcome::Resumed => block.resumed.push(row),
            InterruptedOutcome::Exhausted | InterruptedOutcome::Stale => {
                block.needs_decision.push(row)
            }
        }
    }
    (block.total() > 0).then_some(block)
}

pub fn outcome_label(outcome: InterruptedOutcome) -> &'static str {
    match outcome {
        InterruptedOutcome::Resumed => "resumed automatically",
        InterruptedOutcome::Exhausted => "auto-resume exhausted",
        InterruptedOutcome::Stale => "too old to resume automatically",
    }
}

#[cfg(test)]
mod tests;
