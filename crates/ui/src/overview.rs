//! Multi-chat overview (ticket 002 phase 1):
//! every current chat, at once, in one view — the thing
//! `session_canvas.html`/`session_canvas_server.py` (the `agent-mode-tools`
//! stopgap tool) already does, without leaving for the single-chat route.
//! "View full chat →" opens a side panel that embeds the Shell's OWN
//! `Transcript`/`Composer` entities (live transcript + working composer) by
//! selecting the chat without changing the route — see
//! [`Overview::chat_panel_open`]. The same right-side slot has a second,
//! read-only mode: clicking one of a chat's compact subagent rows "peeks"
//! into that subagent's transcript (`READ_SUBAGENT_TRANSCRIPT`; no composer
//! — a subagent can't be continued) — see [`Overview::subagent_peek`].
//!
//! Two rendering modes over the SAME `OverviewRow` data:
//! - List: the plain row list, optionally grouped.
//! - Outline: the Work Outline tree of work items and their Chats.
//!
//! The acknowledged-"done" set, the repo filter's hidden set and the work-item
//! filter persist per device
//! (`overview_positions`, JSON under the engine data dir; the view mode, the
//! toggles and the grouping persist in `UI_FLAGS_FILE`). Ticket 002 puts
//! cross-device layout sync out of scope for v1.
//!
//! Freshness: the TTL-gated `ensure_*` fetches only get a chance to re-run
//! when something calls them, so a mounted, visible overview runs a light
//! [`IDLE_REFRESH`] tick (`refresh_tick`) that re-checks every visible row's
//! TTLs and repaints relative times without forcing a row rebuild unless a fetch
//! actually changed something.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

use chrono::Utc;
use gpui::{
    Context, Entity, MouseButton, ScrollHandle, SharedString, Subscription, Window, div,
    prelude::*, px,
};

use mast_engine::pr_ticket_cache::{ChecksStatus, PrAction, PrStatus, TicketStatus};
use mast_proto::{Chat, ChatIndicator, ChatLinkSource, Space};
use mast_rpc::methods;

use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::overview_grouping::{self, GroupDimension, GroupKeyed, Partition};
use crate::overview_positions;
use crate::popover;
use crate::shell::Shell;
use crate::state::{AppState, format_time_ago};
use crate::theme::Theme;
use crate::transcript::Transcript;
use crate::work_outline::details_panel::{DetailsEvent, WorkItemDetails};
use crate::work_outline::filter::{self as work_item_filter, WorkItemIndex, WorkItemPathLabel};
use crate::work_outline::hints::{HintKey, WithHint};
use crate::work_outline::model::{OutlineChat, OutlineOptions, OutlineSpace, row_key};
use crate::work_outline::outline_view::{OutlineEvent, OutlineInputs, OutlineView};
use crate::work_outline::store::WorkOutlineSnapshot;
use crate::work_outline::{WorkOutlineCommand, WorkOutlineLoad};

/// The Linear panel host (`overview/linear_host.rs`).
mod details_width;
mod github_host;
mod linear_host;

/// A chat with no activity for longer than this, and not currently
/// working/awaiting input, reads as stale. Adapted from
/// `session_canvas_server.py`'s `CURSOR_STALE_SECONDS` (3600s) — that value
/// times a continuously-polled cursor-liveness hook, a much noisier signal
/// than Mast's per-message timestamps, so a day is the closer match in
/// spirit ("this chat has gone quiet") rather than a literal port of the
/// number.
const STALE_AFTER: chrono::Duration = chrono::Duration::days(7);

/// How long a fetched `ChatLinkStatus` stays good before this view refetches
/// it — independent of the server's own cache TTL, just how often the UI
/// bothers to poll for a fresher answer.
const LINK_STATUS_REFRESH: Duration = Duration::from_secs(45);

/// Subagent scans are settled-ish once a transcript exists, but an imported
/// chat that keeps running elsewhere (or gets resumed here) grows new
/// subagents — a generous TTL instead of fetch-once-forever.
const SUBAGENTS_REFRESH: Duration = Duration::from_secs(120);
/// Subagent refetch TTL while any of a chat's subagents is still `running`
/// — a running subagent flips to `done` on its own, and the row's status
/// dot should follow within an idle tick or so, not two minutes later. Just
/// under [`IDLE_REFRESH`], same reasoning as [`CONTEXT_USAGE_REFRESH`].
const SUBAGENTS_RUNNING_REFRESH: Duration = Duration::from_secs(12);

/// One subagent, as `SCAN_CHAT_SUBAGENTS` reports it — the UI's own decode
/// of the wire item (a subset of `mast_engine::subagent_scan::SubagentSummary`
/// plus the `status` field), so this view depends on the RPC contract rather
/// than the engine struct's exact shape. Unknown extra keys
/// (`transcriptPath`) are ignored.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubagentItem {
    agent_id: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    agent_type: Option<String>,
    #[serde(default)]
    status: SubagentStatus,
}

impl SubagentItem {
    /// The web's `sub.description || sub.agent_type || sub.id`.
    fn display_name(&self) -> String {
        self.description
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.agent_type.clone().filter(|s| !s.trim().is_empty()))
            .unwrap_or_else(|| self.agent_id.clone())
    }
}

/// `SCAN_CHAT_SUBAGENTS`'s `status`. Missing (an engine that predates the
/// field) or unrecognized reads as `Done` — the settled, non-alarming
/// default; a subagent is never shown "running" without the engine saying so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum SubagentStatus {
    Running,
    #[default]
    #[serde(other)]
    Done,
}

impl SubagentStatus {
    /// Reuses the reference status palette: running = `--accent` (the
    /// working color), done = `--status-good`.
    fn color(self) -> gpui::Hsla {
        match self {
            SubagentStatus::Running => reference_status_color(ChatIndicator::Working),
            SubagentStatus::Done => reference_status_color(ChatIndicator::Completed),
        }
    }

    fn label(self) -> &'static str {
        match self {
            SubagentStatus::Running => "running",
            SubagentStatus::Done => "done",
        }
    }
}

/// `READ_SUBAGENT_TRANSCRIPT` reply: `{turns, model}`.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
struct SubagentTranscript {
    #[serde(default)]
    turns: Vec<TranscriptTurn>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
struct TranscriptTurn {
    /// `"user" | "assistant"`; anything else renders under its raw name.
    role: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    tools: Vec<TranscriptTool>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TranscriptTool {
    name: String,
    #[serde(default)]
    input_preview: String,
    #[serde(default)]
    result_preview: Option<String>,
}

/// The web's `turn-role` label: "You" / "Assistant".
fn turn_role_label(role: &str) -> String {
    match role {
        "user" => "You".to_string(),
        "assistant" => "Assistant".to_string(),
        other => other.to_string(),
    }
}

/// Split message text on ``` fences into `(is_code, chunk)` segments — the
/// web `renderMarkdownLite`'s code-block treatment, minus everything else
/// (plain text keeps its newlines as-is). A fence's language tag line is
/// dropped; an unterminated fence runs to the end. Empty prose chunks
/// (e.g. between back-to-back fences) are skipped.
fn split_fenced_code(text: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    loop {
        let Some(open) = rest.find("```") else {
            let prose = rest.trim_matches('\n');
            if !prose.trim().is_empty() {
                out.push((false, prose.to_string()));
            }
            return out;
        };
        let prose = rest[..open].trim_matches('\n');
        if !prose.trim().is_empty() {
            out.push((false, prose.to_string()));
        }
        let after = &rest[open + 3..];
        // Drop the language tag (everything up to the first newline).
        let body_start = after.find('\n').map_or(after.len(), |nl| nl + 1);
        let body = &after[body_start..];
        let (code, next) = match body.find("```") {
            Some(close) => (&body[..close], &body[close + 3..]),
            None => (body, ""),
        };
        out.push((true, code.trim_end_matches('\n').to_string()));
        rest = next;
    }
}

/// Web `formatTurnTime`: a short local "Mon D, HH:MM", empty when absent or
/// unparseable.
fn format_turn_time(timestamp: Option<&str>) -> String {
    timestamp
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%b %-d, %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// Where a subagent-transcript peek's load stands.
#[derive(Debug, Clone, PartialEq)]
enum PeekLoad {
    Loading,
    Loaded(SubagentTranscript),
    Failed,
}

/// The read-only subagent-transcript mode of the right-side panel (web
/// `openTranscriptPanel(sessionId, subagentId, ...)`): distinct from the
/// live chat panel (`Overview::chat_panel_open`) — a subagent can't be
/// continued, so there's no composer, only its turns.
#[derive(Debug, Clone)]
struct SubagentPeek {
    chat_id: String,
    agent_id: String,
    name: String,
    agent_type: Option<String>,
    /// The `Overview::peek_request_seq` value this peek's fetch was issued
    /// under — web `transcriptRequestId`. See [`peek_reply_is_current`].
    request_id: u64,
    load: PeekLoad,
    /// `(turn index, tool index)` pairs whose result preview is disclosed
    /// (web `<details class="tool-detail">`).
    open_tools: HashSet<(usize, usize)>,
}

/// Stale-response guard (web `if (requestId !== transcriptRequestId)
/// return`): a landed `READ_SUBAGENT_TRANSCRIPT` reply only applies if the
/// panel is still showing the peek that issued it — not closed since, and
/// not superseded by a quicker click on another subagent (or the same one
/// again, which issues a fresh id).
fn peek_reply_is_current(peek: Option<&SubagentPeek>, request_id: u64) -> bool {
    peek.is_some_and(|p| p.request_id == request_id)
}

/// Context-window % moves constantly for a live chat, so it gets the
/// shortest TTL of any per-row fetch. Set just under [`IDLE_REFRESH`] so
/// every idle tick refetches it; a TTL of exactly 15s would lose the race
/// against the tick's own timer jitter and only refresh every other tick.
const CONTEXT_USAGE_REFRESH: Duration = Duration::from_secs(12);

/// Idle-refresh cadence while the overview is the visible route — see
/// `refresh_tick`. Only re-checks TTLs (each fetch keeps its own, longer
/// TTL) and repaints; it never forces a row rebuild by itself.
const IDLE_REFRESH: Duration = Duration::from_secs(15);

/// How long the Outline selection must rest on a Chat row before the chat
/// panel switches to it, so holding an arrow key does not reload a
/// transcript per row.
pub(crate) const OUTLINE_PREVIEW_DEBOUNCE: Duration = Duration::from_millis(100);

/// Every Nth idle tick also re-derives rows (staleness is time-based, so a
/// chat can cross `STALE_AFTER` while nothing else changes). ~60s — cheap
/// relative to the per-frame rebuild `rows_dirty` exists to avoid.
const STALENESS_REDERIVE_EVERY_TICKS: u32 = 4;

/// Work counters for the perf regression test: what the Overview did in
/// response to `AppState` changes. Plain increments, always on.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OverviewPerf {
    /// `rows()` calls that rebuilt the row list.
    rows_rebuilds: u64,
    /// `AppState` observer callbacks delivered to the Overview.
    observer_calls: u64,
    /// Of those, the ones that went on to `cx.notify()`.
    observer_notifies: u64,
    /// Per-chat `ensure_row_fetches` evaluations.
    fetch_checks: u64,
    /// Engine requests actually spawned by the per-row fetches.
    fetch_requests: u64,
}

/// One row's worth of what this pass can show.
#[derive(Clone)]
struct OverviewRow {
    status: ChatIndicator,
    chat: Chat,
    stale: bool,
    /// Linked ticket identifier, if `link_status` has it cached by the time
    /// `rows()` builds this row — copied in at construction (rather than
    /// read from `Overview.link_status` inside `group_key`) because
    /// `GroupKeyed::group_key` only has `&self` on the row itself, matching
    /// how `overview_grouping` is deliberately UI-crate-agnostic.
    ticket: Option<String>,
    /// [`repo_key_for_chat`] — the Repo grouping key and the repo filter's
    /// key, computed once so both agree by construction.
    repo: String,
    /// Linked PR number/title, copied in like `ticket`: the number is the
    /// Ticket dimension's `PR #<n>` fallback (`ticketGroupKey`), the title is
    /// a search field (`sessionMatchesSearch` includes `pr.title`).
    pr_number: Option<u64>,
    pr_title: Option<String>,
    /// Search-only values for every linked PR (number and title). The two
    /// singular fields above remain the stable grouping fallback.
    pr_search_values: Vec<String>,
    /// `CHAT_CONTEXT_USAGE` fraction (`0.0..=1.0`), copied in like `ticket`
    /// so the cached rows carry it. `None` until the fetch lands, or when the
    /// engine has no usage for this chat.
    context_pct: Option<f32>,
    /// How many compact subagent rows exist for this chat — copied in from
    /// `Overview.subagents` like `ticket`. `0` while the fetch is pending or
    /// the chat genuinely has none — both render no rows, so they're
    /// indistinguishable here on purpose.
    subagent_count: usize,
    /// `chat.cwd` cheaply looks like a linked worktree — the expanded detail
    /// offers "Close out worktree…" and the engine plan decides actual
    /// eligibility. Computed in the cached `rows()` pass so the fs check never
    /// runs per frame.
    closeable: bool,
    /// The associated work item's breadcrumb (path chip), copied in from the
    /// Work Outline index in `rows()` like `ticket`. The chip shares an
    /// existing line of the row.
    work_item_path: Option<WorkItemPathLabel>,
    /// The associated work item is in User Action Required.
    user_action_required: bool,
    /// Its unresolved user-action request count (0 until Milestone 4).
    user_action_count: u32,
    /// The Chat's effective instructions changed since its applied revision
    /// (Work Outline drift). The mark shares the title line.
    instruction_drift: bool,
    /// The Chat captured an execution configuration that differs from its
    /// item's current one (Work Outline exception). Shares the title line.
    execution_exception: bool,
}

impl GroupKeyed for OverviewRow {
    /// Repo/Ticket are proxies (cwd basename, linked-ticket identifier).
    fn group_key(&self, dim: GroupDimension) -> String {
        match dim {
            GroupDimension::Repo => self.repo.clone(),
            GroupDimension::Ticket => ticket_group_key(self.ticket.as_deref(), self.pr_number),
        }
    }
}

impl GroupKeyed for &OverviewRow {
    fn group_key(&self, dim: GroupDimension) -> String {
        (*self).group_key(dim)
    }
}

/// Port of `repoGroupKey` (`session_canvas.html:968-972`): the cwd's last
/// path segment after stripping trailing slashes, `NONE_KEY` for no cwd (or
/// a cwd that is nothing but slashes).
///
/// Deliberate divergence from the reference: agent worktrees carry a
/// `.workspace-root` file at their root whose first line is the absolute path
/// of the superproject checkout. When `<cwd>/.workspace-root` exists and that
/// trimmed first line is non-empty, the key is the last segment of THAT path
/// instead, so worktree chats group under their real repo rather than each
/// worktree name becoming its own one-chat "repo". A missing, unreadable, or
/// empty file falls back to the reference behavior above.
///
/// Does a small fs read per call; only invoked from the cached `rows()`
/// recompute (not per frame), so it is not memoized.
fn repo_key(cwd: Option<&str>) -> String {
    let Some(cwd) = cwd else {
        return overview_grouping::NONE_KEY.to_string();
    };
    if let Some(root) = workspace_root_of(cwd) {
        if let Some(key) = last_segment(&root) {
            return key;
        }
    }
    last_segment(cwd).unwrap_or_else(|| overview_grouping::NONE_KEY.to_string())
}

/// Resolve a chat's repo label without mistaking an isolated checkout's
/// folder name for the owning project. The persisted Space is authoritative
/// when the chat runs somewhere other than the Space root (normally a linked
/// worktree). Same-root and legacy chats retain the cwd-based behavior above.
///
/// This is deliberately a lexical comparison: grouping must not canonicalize
/// arbitrary chat paths merely to render the overview.
fn repo_key_for_chat(chat: &Chat, space: Option<&Space>) -> String {
    if let (Some(cwd), Some(space)) = (chat.cwd.as_deref(), space)
        && lexical_path(cwd) != lexical_path(&space.path)
    {
        return space.display_name().to_string();
    }
    repo_key(chat.cwd.as_deref())
}

fn lexical_path(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
}

fn last_segment(path: &str) -> Option<String> {
    match path.trim_end_matches('/').rsplit('/').next() {
        Some(last) if !last.is_empty() => Some(last.to_string()),
        _ => None,
    }
}

/// First line (trimmed) of `<cwd>/.workspace-root`, if present and non-empty.
fn workspace_root_of(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    if mast_engine::repos::is_automatic_access_blocked(std::path::Path::new(cwd)) {
        return None;
    }
    let contents =
        std::fs::read_to_string(std::path::Path::new(cwd).join(".workspace-root")).ok()?;
    let line = contents.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// Port of `ticketGroupKey` (`session_canvas.html:955-957`): ticket id, else
/// `PR #<n>` when a PR is linked, else `NONE_KEY`.
fn ticket_group_key(ticket: Option<&str>, pr_number: Option<u64>) -> String {
    match (ticket, pr_number) {
        (Some(t), _) => t.to_string(),
        (None, Some(n)) => format!("PR #{n}"),
        (None, None) => overview_grouping::NONE_KEY.to_string(),
    }
}

/// Display label for a raw group key (`PartitionGroup::key`) on `dim` —
/// the reference's per-dimension `getMeta().label`: "No repo"/"No ticket /
/// PR" for the `NONE_KEY` bucket (`repoGroupMeta`/`ticketGroupMeta`), the raw
/// key (a folder name, a ticket id) otherwise.
fn group_label(dim: GroupDimension, key: &str) -> String {
    match dim {
        GroupDimension::Repo if key == overview_grouping::NONE_KEY => "No repo".into(),
        GroupDimension::Ticket if key == overview_grouping::NONE_KEY => "No ticket / PR".into(),
        GroupDimension::Repo | GroupDimension::Ticket => key.to_string(),
    }
}

/// `session_canvas.html`'s `HASH_COLOR_PALETTE` (`~888`) — Repo/Ticket group
/// colors have no fixed lookup table, so the same key always lands on the
/// same hue via a deterministic hash instead.
const HASH_COLOR_PALETTE: [u32; 8] = [
    0x3987e5, 0xd95926, 0x199e70, 0xc98500, 0xd55181, 0x008300, 0x9085e9, 0xe66767,
];

/// Exact port of `session_canvas.html`'s `hashColor` (`~889-893`):
/// `hash = (hash * 31 + charCode) >>> 0` per char, then `PALETTE[hash %
/// PALETTE.length]`. `wrapping_mul`/`wrapping_add` on `u32` reproduces JS's
/// `>>> 0` unsigned-32-bit truncation exactly — this must NOT be `i32` math,
/// which would diverge on overflow.
fn hash_color(key: &str) -> gpui::Hsla {
    let mut hash: u32 = 0;
    for ch in key.chars() {
        hash = hash.wrapping_mul(31).wrapping_add(ch as u32);
    }
    let idx = (hash as usize) % HASH_COLOR_PALETTE.len();
    gpui::rgb(HASH_COLOR_PALETTE[idx]).into()
}

/// Exact `--status-*` hex values from `session_canvas.html`'s `:root` CSS
/// (`~20-22`) and `STATUS_STYLES` (`~837-846`) — replaces the app theme's
/// own approximation so status colors genuinely match the
/// reference instead of merely resembling it.
fn reference_status_color(status: ChatIndicator) -> gpui::Hsla {
    match status {
        ChatIndicator::Working => gpui::rgb(0x3987e5).into(), // --accent (working/running)
        // Reference's `needs_input` is --status-critical (red), not a
        // distinct blue — a real semantic difference from this app's prior
        // theme-based choice, ported faithfully rather than kept.
        ChatIndicator::AwaitingInput => gpui::rgb(0xd03b3b).into(),
        ChatIndicator::Errored => gpui::rgb(0xd03b3b).into(), // --status-critical (blocked)
        ChatIndicator::Completed => gpui::rgb(0x0ca30c).into(), // --status-good (done)
        ChatIndicator::Idle => gpui::rgb(0x898781).into(),    // --text-muted
    }
}

/// `STATUS_STYLES` icon + label (`session_canvas.html:892-905`) mapped from
/// Mast's real `ChatIndicator` vocabulary. What Mast can and can't produce:
/// - `pending_approval` (#fab219 "⚡"): no equivalent. `SessionStatus` has no
///   permission-wait state — an `InputRequested` event (the only "blocked on
///   the human" signal) maps to `AwaitingInput`, which is the web's
///   `needs_input`. Not invented here.
/// - `blocked` ("✕"): Mast's `Errored` (an unseen failed run) — labeled
///   "error", which is what it actually is.
/// - `not_running` ("○"): no equivalent. It marked an externally imported
///   session with no live run, which needed its import classification;
///   without that every `Idle` chat is "idle" ("–").
/// - `running`/`unknown`: aliases/fallbacks with no distinct Mast source.
fn status_style(status: ChatIndicator) -> (&'static str, &'static str) {
    match status {
        ChatIndicator::Working => ("●", "working"),
        ChatIndicator::AwaitingInput => ("●", "needs input"),
        ChatIndicator::Errored => ("✕", "error"),
        ChatIndicator::Completed => ("✓", "done"),
        ChatIndicator::Idle => ("–", "idle"),
    }
}

/// `CHECKS_META` (`session_canvas.html:912-916`): color + icon per checks
/// rollup.
fn checks_style(checks: ChecksStatus) -> (gpui::Hsla, &'static str, &'static str) {
    match checks {
        ChecksStatus::Passing => (gpui::rgb(0x0ca30c).into(), "✓", "passing"),
        ChecksStatus::Pending => (gpui::rgb(0xfab219).into(), "…", "pending"),
        ChecksStatus::Failing => (gpui::rgb(0xd03b3b).into(), "✕", "failing"),
    }
}

/// Web `isUnreadDone` reconciliation (`session_canvas.html:1289-1298`): a
/// chat that is no longer `done` drops out of the acknowledged set so its
/// NEXT completion starts unread again. Returns whether the set changed (so
/// the caller persists it).
fn reconcile_acknowledged(
    acknowledged: &mut BTreeSet<String>,
    chat_id: &str,
    is_done: bool,
) -> bool {
    !is_done && acknowledged.remove(chat_id)
}

/// Whether a row should carry the unread-done ribbon.
fn is_unread_done(acknowledged: &BTreeSet<String>, chat_id: &str, is_done: bool) -> bool {
    is_done && !acknowledged.contains(chat_id)
}

/// `sessionMatchesSearch` (`session_canvas.html:1464-1470`): case-insensitive
/// substring over whichever fields the row carries. `query_lower` must
/// already be trimmed + lowercased; empty matches everything.
fn matches_search(query_lower: &str, fields: &[Option<&str>]) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    fields
        .iter()
        .flatten()
        .any(|field| field.to_lowercase().contains(query_lower))
}

/// Legacy UI-flag entry from when a Canvas view existed. Read-only: a saved
/// `viewMode:canvas` loads as Outline ([`OverviewMode::from_flags`]) and is
/// never written back (the writer purges every `viewMode:` flag).
const LEGACY_VIEW_CANVAS_FLAG: &str = "viewMode:canvas";
/// UI-flag entry: the Work Outline is the main column's mode. Older builds
/// read it as an unknown flag, i.e. List (downgrade-safe).
const VIEW_OUTLINE_FLAG: &str = "viewMode:outline";
/// Prefix shared by every overview-mode flag; the writer purges all of them
/// before inserting the one for the active mode.
const VIEW_MODE_PREFIX: &str = "viewMode:";
/// UI-flag entry: the "Hide stale" toggle is on.
const HIDE_STALE_FLAG: &str = "hideStale";
/// UI-flag entry: the "Show archived" toggle is on.
const SHOW_ARCHIVED_FLAG: &str = "showArchived";
/// Prefix of the UI-flag entry holding the active grouping dimensions in
/// activation (= nesting) order, comma-joined:
/// `groupBy:ticket,repo`. Exactly one such entry exists at a time (a string
/// set has no order of its own, so the order lives inside the one string).
const GROUP_BY_PREFIX: &str = "groupBy:";

/// The overview's persisted view state other than the repo filter (which has
/// its own `HIDDEN_REPOS_FILE`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ViewPrefs {
    mode: OverviewMode,
    hide_stale: bool,
    show_archived: bool,
    groups: Vec<GroupDimension>,
}

/// Restore [`ViewPrefs`] from the persisted UI-flag set. Unknown flags and
/// unknown/duplicate grouping dimensions are dropped rather than failing the
/// load.
fn view_prefs_from_flags(flags: &BTreeSet<String>) -> ViewPrefs {
    let mut groups: Vec<GroupDimension> = Vec::new();
    if let Some(list) = flags
        .iter()
        .find_map(|flag| flag.strip_prefix(GROUP_BY_PREFIX))
    {
        for dim in list
            .split(',')
            .filter_map(|key| GroupDimension::from_persist_key(key.trim()))
        {
            if !groups.contains(&dim) {
                groups.push(dim);
            }
        }
    }
    ViewPrefs {
        mode: OverviewMode::from_flags(flags),
        hide_stale: flags.contains(HIDE_STALE_FLAG),
        show_archived: flags.contains(SHOW_ARCHIVED_FLAG),
        groups,
    }
}

/// Write [`ViewPrefs`] into the UI-flag set, replacing any previous view-pref
/// entries and leaving every other flag (including retired ones such as the
/// old PR-sidebar flags, and future flags) untouched.
fn apply_view_prefs_to_flags(flags: &mut BTreeSet<String>, prefs: &ViewPrefs) {
    flags.retain(|flag| !flag.starts_with(GROUP_BY_PREFIX) && !flag.starts_with(VIEW_MODE_PREFIX));
    if let Some(flag) = prefs.mode.flag() {
        flags.insert(flag.to_string());
    }
    for (flag, on) in [
        (HIDE_STALE_FLAG, prefs.hide_stale),
        (SHOW_ARCHIVED_FLAG, prefs.show_archived),
    ] {
        if on {
            flags.insert(flag.to_string());
        } else {
            flags.remove(flag);
        }
    }
    if !prefs.groups.is_empty() {
        let list: Vec<&str> = prefs.groups.iter().map(|dim| dim.persist_key()).collect();
        flags.insert(format!("{GROUP_BY_PREFIX}{}", list.join(",")));
    }
}

/// Whether a just-restored repo filter hides every repo that currently has
/// chats — an invisible "everything is gone" state (typically a stale filter
/// naming repos that were since renamed/cleaned up). `known` empty means the
/// chat list hasn't arrived yet, which says nothing.
fn restored_repo_filter_hides_everything(
    hidden: &BTreeSet<String>,
    known: &BTreeSet<String>,
) -> bool {
    !known.is_empty() && known.iter().all(|repo| hidden.contains(repo))
}

/// Drop hover state when its element is no longer mounted. Hover-leave is
/// not guaranteed when filtering or changing view modes removes the element
/// under the pointer, so renderers reconcile against the
/// ids they actually built this frame.
fn reconcile_hovered_row(hovered: &mut Option<String>, mounted_ids: &[String]) -> bool {
    let stale = hovered
        .as_ref()
        .is_some_and(|id| !mounted_ids.iter().any(|mounted| mounted == id));
    if stale {
        *hovered = None;
    }
    stale
}

/// Repo-filter button label (`updateRepoFilterBtnState`,
/// `session_canvas.html:2101-2105`).
fn repo_filter_label(hidden: usize) -> String {
    if hidden > 0 {
        format!("Repos ({hidden} hidden)")
    } else {
        "Repos".to_string()
    }
}

/// Toggle one repo's visibility in the hidden set (a checkbox click).
fn toggle_repo_hidden(hidden: &mut BTreeSet<String>, repo: &str) {
    if !hidden.remove(repo) {
        hidden.insert(repo.to_string());
    }
}

/// Repo-filter list order (`renderRepoFilterList`): alphabetical, `NONE_KEY`
/// last.
fn sorted_repo_keys(known: &BTreeSet<String>) -> Vec<String> {
    let mut keys: Vec<String> = known
        .iter()
        .filter(|k| k.as_str() != overview_grouping::NONE_KEY)
        .cloned()
        .collect();
    if known.contains(overview_grouping::NONE_KEY) {
        keys.push(overview_grouping::NONE_KEY.to_string());
    }
    keys
}

/// Web's `.worktree` badge rule (`is_worktree`, `session_canvas_server.py:
/// 300-304`): cwd under `/.worktrees/` or `/.agent-worktrees/`. UI-side
/// fallback until/unless the engine's link status carries its own flag.
fn cwd_is_worktree(cwd: Option<&str>) -> bool {
    cwd.is_some_and(|c| c.contains("/.worktrees/") || c.contains("/.agent-worktrees/"))
}

/// Compact, always-visible model metadata for chat cards: the model that
/// actually ran when the harness reported it, else the configured one, else
/// an honest "default model" (the harness decides) — never the picker default.
fn chat_model_label(chat: &Chat) -> String {
    crate::chat_metadata::chat_model_summary(chat)
        .0
        .unwrap_or_else(|| "default model".to_string())
}

fn model_badge(chat: &Chat, theme: &Theme) -> gpui::AnyElement {
    let label: SharedString = chat_model_label(chat).into();
    let tooltip: SharedString = crate::chat_metadata::chat_model_summary(chat).1.into();
    div()
        .id(SharedString::from(format!("overview-model-{}", chat.id)))
        .debug_selector(|| "overview-card-model".into())
        .flex_none()
        .max_w(px(112.0))
        .px(px(5.0))
        .py(px(1.0))
        .rounded(px(4.0))
        .bg(theme.element_hover.opacity(0.7))
        .truncate()
        .text_size(crate::typography::ui_rems(9.0))
        .text_color(theme.text_muted.opacity(0.85))
        .tooltip(move |_, cx| cx.new(|_| LinkSourceTooltip(tooltip.clone())).into())
        .child(label)
        .into_any_element()
}

/// A group's display color for `dim`/`key` — `NONE_KEY` is always
/// `--text-muted` (matches `ticketGroupMeta`/`repoGroupMeta`'s explicit
/// special-case, `session_canvas.html:~899,~914`); Repo/Ticket hash the key
/// since they have no bounded value set.
fn group_color(dim: GroupDimension, key: &str) -> gpui::Hsla {
    if key == overview_grouping::NONE_KEY {
        return gpui::rgb(0x898781).into(); // --text-muted
    }
    match dim {
        GroupDimension::Repo | GroupDimension::Ticket => hash_color(key),
    }
}

#[cfg(test)]
mod color_tests {
    use super::*;

    /// Pins `hash_color` against `session_canvas.html`'s `hashColor`
    /// (`~889-893`) computed independently in Python with the same
    /// `(hash * 31 + charCode) & 0xFFFFFFFF` truncation JS's `>>> 0`
    /// performs, to catch a signed/unsigned overflow divergence rather than
    /// just eyeballing the Rust output.
    #[test]
    fn hash_color_matches_reference_js_algorithm() {
        let cases: &[(&str, u32)] = &[
            ("zeron", 6),
            ("ui", 4),
            ("ENG-1234", 1),
            ("agent-mode-tools", 1),
            ("a", 1),
        ];
        for (key, expected_idx) in cases {
            let expected: gpui::Hsla = gpui::rgb(HASH_COLOR_PALETTE[*expected_idx as usize]).into();
            assert_eq!(
                hash_color(key),
                expected,
                "hash_color({key:?}) should land on palette index {expected_idx}"
            );
        }
    }

    #[test]
    fn hash_color_empty_key_is_index_zero() {
        let expected: gpui::Hsla = gpui::rgb(HASH_COLOR_PALETTE[0]).into();
        assert_eq!(hash_color(""), expected);
    }

    #[test]
    fn none_key_is_always_text_muted_regardless_of_dimension() {
        let expected: gpui::Hsla = gpui::rgb(0x898781).into();
        for dim in [GroupDimension::Repo, GroupDimension::Ticket] {
            assert_eq!(group_color(dim, overview_grouping::NONE_KEY), expected);
        }
    }
}

/// Total leaf-row count under a `Partition` node, for a group header's
/// `(N)` count — a `Groups` node's own count is the sum of its children's,
/// recursed all the way down to the `Leaves` at the bottom.
fn count_rows<T>(partition: &Partition<T>) -> usize {
    match partition {
        Partition::Leaves(rows) => rows.len(),
        Partition::Groups { groups, .. } => groups.iter().map(|g| count_rows(&g.items)).sum(),
    }
}

/// One flattened item for List mode's indented grouped rendering — a group
/// header (label/nesting-depth/leaf-count) or a row, in the depth-first order
/// [`flatten_partition`] visits the tree.
enum ListItem<'a> {
    Header {
        label: String,
        color: gpui::Hsla,
        depth: usize,
        count: usize,
    },
    Row(&'a OverviewRow),
}

/// Depth-first flatten of a `Partition` tree into `ListItem`s: a header
/// immediately followed by everything in its subtree (nested headers and
/// rows alike), each nested level one `depth` deeper — what List mode
/// indents by. The flat/no-groups case never reaches this function (see
/// `render_list_body`'s `active_groups.is_empty()` branch).
fn flatten_partition<'a>(
    partition: &Partition<&'a OverviewRow>,
    depth: usize,
    out: &mut Vec<ListItem<'a>>,
) {
    match partition {
        Partition::Leaves(rows) => {
            for &row in rows {
                out.push(ListItem::Row(row));
            }
        }
        Partition::Groups { dimension, groups } => {
            for g in groups {
                out.push(ListItem::Header {
                    label: group_label(*dimension, &g.key),
                    color: group_color(*dimension, &g.key),
                    depth,
                    count: count_rows(&g.items),
                });
                flatten_partition(&g.items, depth + 1, out);
            }
        }
    }
}

struct LinkStatusEntry {
    status: OverviewLinkStatus,
    /// Uncommitted diff stat, if the engine's `CHAT_LINK_STATUS` response
    /// carries a `diffStat` object (`{filesChanged, linesAdded,
    /// linesRemoved}`, the reference's `diff_stat`). Read off the raw JSON
    /// rather than a `ChatLinkStatus` field so this lights up the moment the
    /// engine adds it, with no UI change — absent today.
    diff_stat: Option<DiffStat>,
    /// Engine-provided `isWorktree`, when present; otherwise the detail pane
    /// falls back to [`cwd_is_worktree`].
    is_worktree: Option<bool>,
    fetched_at: Instant,
}

/// UI-owned decode of `CHAT_LINK_STATUS`. Keeping the wire shape local lets
/// the overview accept the additive `prLinks` field while remaining
/// compatible with engines that only return the legacy singular `pr` /
/// `prSource` pair.
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OverviewLinkStatus {
    #[serde(default)]
    pr: Option<PrStatus>,
    #[serde(default)]
    ticket: Option<TicketStatus>,
    #[serde(default)]
    pr_source: Option<ChatLinkSource>,
    #[serde(default)]
    ticket_source: Option<ChatLinkSource>,
    #[serde(default)]
    pr_links: Vec<OverviewPrLink>,
}

/// One PR associated with a chat. `detail` can be absent while the engine's
/// background GitHub lookup is still pending; `url` remains enough to show a
/// stable placeholder and to remove this exact link.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OverviewPrLink {
    url: String,
    #[serde(default)]
    source: Option<ChatLinkSource>,
    #[serde(default)]
    detail: Option<PrStatus>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiffStat {
    files_changed: u64,
    lines_added: u64,
    lines_removed: u64,
}

/// What of a landed link status feeds `OverviewRow`/grouping/search — a
/// refetch whose fingerprint is unchanged must not dirty rows (badges/detail
/// read `link_status` live at render).
fn link_fingerprint(
    status: &OverviewLinkStatus,
) -> (Option<String>, Vec<(String, u64, Option<String>)>) {
    let prs = effective_pr_links(status)
        .into_iter()
        .map(|link| {
            let number = link
                .detail
                .as_ref()
                .map_or_else(|| pr_number_from_url(&link.url), |pr| pr.number);
            let title = link.detail.as_ref().and_then(|pr| pr.title.clone());
            (link.url, number, title)
        })
        .collect();
    (status.ticket.as_ref().map(|t| t.identifier.clone()), prs)
}

/// Whether `badges_for` would render a PR/ticket badge row for this link
/// status: a linked ticket, or at least one linked PR.
fn link_has_badge_row(status: &OverviewLinkStatus) -> bool {
    status.ticket.is_some() || !effective_pr_links(status).is_empty()
}

/// Which durable link slot a `SET_CHAT_LINK` write targets — the RPC's
/// `kind: "pr" | "ticket"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkKind {
    Pr,
    Ticket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkOperation {
    Add,
    Remove,
    Clear,
}

impl LinkOperation {
    fn wire(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Remove => "remove",
            Self::Clear => "clear",
        }
    }
}

impl LinkKind {
    fn wire(self) -> &'static str {
        match self {
            LinkKind::Pr => "pr",
            LinkKind::Ticket => "ticket",
        }
    }
}

/// What the "Add PR / ticket…" input parsed to: the RPC `kind` plus the
/// normalized value to store.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedLink {
    kind: LinkKind,
    value: String,
}

/// Error text shown under the link input when nothing parses.
const LINK_PARSE_ERROR: &str =
    "Paste a GitHub PR URL (github.com/owner/repo/pull/123) or a ticket id like ENG-1234.";

/// The link input's one-box smart parse:
/// - a `github.com/<owner>/<repo>/pull/<n>` URL (scheme and `www.`
///   optional, any trailing `/files`, `?query` or `#fragment` ignored) →
///   `pr`, normalized to `https://github.com/<owner>/<repo>/pull/<n>` —
///   the exact shape `gh` reports, which PR-link matching relies on. A
///   bare `#123` is NOT accepted: it doesn't say which repo.
/// - an `XX-1234`-shaped id (2-6 letters, `-`, digits — the engine's own
///   `extract_ticket_id` shape) → `ticket`, uppercased. A Linear issue URL
///   (`linear.app/<ws>/issue/ENG-1234/...`) is accepted too and reduced to
///   its id.
/// - anything else → [`LINK_PARSE_ERROR`].
fn parse_link_input(raw: &str) -> Result<ParsedLink, &'static str> {
    let input = raw.trim();
    if input.is_empty() {
        return Err(LINK_PARSE_ERROR);
    }
    let without_scheme = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))
        .unwrap_or(input);
    let without_www = without_scheme
        .strip_prefix("www.")
        .unwrap_or(without_scheme);
    // Drop `?query` / `#fragment` before splitting into path segments.
    let path = without_www.split(['?', '#']).next().unwrap_or_default();
    let segments: Vec<&str> = path.split('/').collect();
    match segments
        .first()
        .map(|host| host.to_ascii_lowercase())
        .as_deref()
    {
        Some("github.com") => {
            if let [_, owner, repo, "pull", number, ..] = segments.as_slice()
                && !owner.is_empty()
                && !repo.is_empty()
                && !number.is_empty()
                && number.bytes().all(|b| b.is_ascii_digit())
            {
                return Ok(ParsedLink {
                    kind: LinkKind::Pr,
                    value: format!("https://github.com/{owner}/{repo}/pull/{number}"),
                });
            }
            Err(LINK_PARSE_ERROR)
        }
        Some("linear.app") => match segments.as_slice() {
            [_, _, "issue", id, ..] => ticket_id_shape(id)
                .map(|value| ParsedLink {
                    kind: LinkKind::Ticket,
                    value,
                })
                .ok_or(LINK_PARSE_ERROR),
            _ => Err(LINK_PARSE_ERROR),
        },
        _ => ticket_id_shape(input)
            .map(|value| ParsedLink {
                kind: LinkKind::Ticket,
                value,
            })
            .ok_or(LINK_PARSE_ERROR),
    }
}

/// `Some(uppercased)` when the WHOLE of `s` is a ticket id: 2-6 ASCII
/// letters, `-`, one or more digits (the engine's `extract_ticket_id`
/// shape, anchored instead of searched).
fn ticket_id_shape(s: &str) -> Option<String> {
    let (letters, digits) = s.split_once('-')?;
    let ok = (2..=6).contains(&letters.len())
        && letters.bytes().all(|b| b.is_ascii_alphabetic())
        && !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit());
    ok.then(|| s.to_ascii_uppercase())
}

/// Provenance tooltip for a PR/ticket line. `None` source = no durable link
/// stored; the shown PR/ticket came from branch/title inference.
fn link_source_label(kind: LinkKind, source: Option<ChatLinkSource>) -> &'static str {
    match (kind, source) {
        (_, Some(ChatLinkSource::Manual)) => "Linked manually",
        (LinkKind::Pr, Some(ChatLinkSource::CreatedInChat)) => "PR created in this chat",
        (LinkKind::Ticket, Some(ChatLinkSource::CreatedInChat)) => "Created in this chat",
        (_, Some(ChatLinkSource::Mentioned)) => "Mentioned in conversation",
        (_, Some(inferred)) => inferred.inferred_label().unwrap_or("Linked"),
        (_, None) => "Inferred from branch",
    }
}

/// `SET_CHAT_LINK`'s reply: the chat row's durable link fields after the
/// write.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetChatLinkReply {
    #[serde(default)]
    linked_pr_url: Option<String>,
    #[serde(default)]
    linked_pr_source: Option<ChatLinkSource>,
    #[serde(default)]
    linked_pr_links: Option<Vec<StoredPrLink>>,
    #[serde(default)]
    linked_ticket_id: Option<String>,
    #[serde(default)]
    linked_ticket_source: Option<ChatLinkSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredPrLink {
    url: String,
    source: ChatLinkSource,
}

/// PR number off a `.../pull/<n>` URL (`0` if it has none).
fn pr_number_from_url(url: &str) -> u64 {
    url.rsplit_once("/pull/")
        .and_then(|(_, rest)| rest.split(['/', '?', '#']).next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Stand-in `PrStatus` for a just-linked URL whose detail the engine's
/// cache hasn't fetched yet (`CHAT_LINK_STATUS` is a pure cache read — a
/// miss comes back `pr: null` until the background sweep fills it). No
/// action reasons (`has_reviewer_requested: true`), so it never paints a
/// false "Needs reviewer" badge.
fn placeholder_pr(url: &str) -> PrStatus {
    PrStatus {
        number: pr_number_from_url(url),
        url: Some(url.to_string()),
        state: "loading…".to_string(),
        is_draft: false,
        review_decision: None,
        reviewers: Vec::new(),
        checks: None,
        title: None,
        branch: None,
        mergeable: String::new(),
        has_reviewer_requested: true,
    }
}

/// Optimistic local update after a successful `SET_CHAT_LINK` — the badge/
/// detail change the same frame instead of waiting on the refetch (and on
/// the engine's sweep to cache the new PR/ticket's detail). A set whose
/// value already matches what's shown keeps the real detail; a different
/// value gets a placeholder; a clear drops the slot (the follow-up refetch
/// brings back whatever inference finds).
fn effective_pr_links(status: &OverviewLinkStatus) -> Vec<OverviewPrLink> {
    if !status.pr_links.is_empty() {
        return status.pr_links.clone();
    }
    status
        .pr
        .as_ref()
        .map(|pr| OverviewPrLink {
            url: pr.url.clone().unwrap_or_default(),
            source: status.pr_source,
            detail: Some(pr.clone()),
        })
        .into_iter()
        .collect()
}

fn header_links(status: &OverviewLinkStatus) -> crate::chat_metadata::HeaderLinks {
    crate::chat_metadata::HeaderLinks {
        prs: effective_pr_links(status)
            .into_iter()
            .map(|link| crate::chat_metadata::HeaderPr {
                number: link
                    .detail
                    .as_ref()
                    .map_or_else(|| pr_number_from_url(&link.url), |pr| pr.number),
                title: link.detail.as_ref().and_then(|pr| pr.title.clone()),
                url: link.url,
                source: link.source,
            })
            .collect(),
        ticket: status
            .ticket
            .as_ref()
            .map(|ticket| crate::chat_metadata::HeaderTicket {
                identifier: ticket.identifier.clone(),
                title: ticket.title.clone(),
                url: ticket.url.clone(),
            }),
    }
}

fn apply_link_reply(status: &mut OverviewLinkStatus, kind: LinkKind, reply: &SetChatLinkReply) {
    match kind {
        LinkKind::Pr => {
            if let Some(links) = &reply.linked_pr_links {
                let previous = effective_pr_links(status);
                status.pr_links = links
                    .iter()
                    .map(|link| {
                        let detail = previous
                            .iter()
                            .find(|old| old.url == link.url)
                            .and_then(|old| old.detail.clone())
                            .or_else(|| Some(placeholder_pr(&link.url)));
                        OverviewPrLink {
                            url: link.url.clone(),
                            source: Some(link.source),
                            detail,
                        }
                    })
                    .collect();
                status.pr = status.pr_links.first().and_then(|link| link.detail.clone());
                status.pr_source = status.pr_links.first().and_then(|link| link.source);
            } else {
                match reply.linked_pr_url.as_deref() {
                    Some(url) => {
                        if status.pr.as_ref().and_then(|p| p.url.as_deref()) != Some(url) {
                            status.pr = Some(placeholder_pr(url));
                        }
                        status.pr_source = reply.linked_pr_source;
                    }
                    None => {
                        status.pr = None;
                        status.pr_source = None;
                    }
                }
            }
        }
        LinkKind::Ticket => match reply.linked_ticket_id.as_deref() {
            Some(id) => {
                if status.ticket.as_ref().map(|t| t.identifier.as_str()) != Some(id) {
                    status.ticket = Some(TicketStatus {
                        identifier: id.to_string(),
                        title: None,
                        url: None,
                        status: None,
                    });
                }
                status.ticket_source = reply.linked_ticket_source;
            }
            None => {
                status.ticket = None;
                status.ticket_source = None;
            }
        },
    }
}

/// A landed `CHAT_LINK_STATUS` refetch, merged over what was shown: when a
/// durable link exists (`*_source` set) but the engine's cache hasn't got
/// its detail yet (`pr`/`ticket` null), keep the previous value (typically
/// [`apply_link_reply`]'s placeholder) instead of blanking the badge until
/// the sweep catches up.
fn merge_refetched_link(
    previous: Option<&OverviewLinkStatus>,
    mut next: OverviewLinkStatus,
) -> OverviewLinkStatus {
    if let Some(prev) = previous {
        for link in &mut next.pr_links {
            if link.detail.is_none() {
                link.detail = effective_pr_links(prev)
                    .into_iter()
                    .find(|old| old.url == link.url)
                    .and_then(|old| old.detail);
            }
        }
        if next.pr.is_none() && next.pr_source.is_some() && prev.pr_source.is_some() {
            next.pr = prev.pr.clone();
        }
        if next.ticket.is_none() && next.ticket_source.is_some() && prev.ticket_source.is_some() {
            next.ticket = prev.ticket.clone();
        }
    }
    next
}

/// Tiny hover tooltip carrying one short text (link provenance, a compact
/// subagent row's full name). Wraps past `max_w` (web `.name-tooltip`'s
/// `max-width` + `white-space: normal`).
struct LinkSourceTooltip(SharedString);

impl Render for LinkSourceTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .px(px(8.0))
            .py(px(5.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.surface_raised)
            .shadow_md()
            .max_w(px(280.0))
            .text_size(px(11.0))
            .text_color(theme.text)
            .child(self.0.clone())
    }
}

/// Attach the provenance tooltip ([`link_source_label`]) to a PR/ticket
/// element.
fn link_source_hover(
    el: gpui::Stateful<gpui::Div>,
    kind: LinkKind,
    source: Option<ChatLinkSource>,
) -> gpui::Stateful<gpui::Div> {
    let label = SharedString::from(link_source_label(kind, source));
    el.tooltip(move |_, cx| cx.new(|_| LinkSourceTooltip(label.clone())).into())
}

/// The tiny pin glyph marking a manually linked PR/ticket.
fn manual_link_pin(color: gpui::Hsla) -> gpui::Svg {
    crate::icons::icon(crate::icons::PIN)
        .size(px(9.0))
        .text_color(color.opacity(0.8))
}

/// Port of `session_canvas_server.py`'s staleness spirit ("quiet AND not
/// currently doing anything"), not its literal cursor-liveness mechanism —
/// see `STALE_AFTER`'s doc comment.
fn is_stale(chat: &Chat, status: ChatIndicator, now: chrono::DateTime<Utc>) -> bool {
    if matches!(
        status,
        ChatIndicator::Working | ChatIndicator::AwaitingInput
    ) {
        return false;
    }
    let last_activity = chat.last_message_at.unwrap_or(chat.created_at);
    now.signed_duration_since(last_activity) > STALE_AFTER
}

/// Label + severity color for a PR action reason — exact `ACTION_META`
/// (`session_canvas.html:2206-2212`) labels and `--status-*` hex values.
fn pr_action_badge(action: PrAction) -> (&'static str, gpui::Hsla) {
    let critical: gpui::Hsla = gpui::rgb(0xd03b3b).into();
    match action {
        PrAction::CiFailing => ("CI failing", critical),
        PrAction::MergeConflict => ("Merge conflict", critical),
        PrAction::ChangesRequested => ("Changes requested", critical),
        PrAction::NeedsReviewer => ("Needs reviewer", gpui::rgb(0xfab219).into()),
        PrAction::ReadyToMerge => ("Ready to merge", gpui::rgb(0x0ca30c).into()),
    }
}

/// The main column's rendering mode.
///
/// Persisted as at most one `viewMode:` flag in `UI_FLAGS_FILE`: no flag =
/// List, [`VIEW_OUTLINE_FLAG`] = Outline, any other `viewMode:` value = List.
/// The legacy [`LEGACY_VIEW_CANVAS_FLAG`] (the removed Canvas view) also reads
/// as Outline, alone or alongside the outline flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OverviewMode {
    #[default]
    List,
    Outline,
}

impl OverviewMode {
    fn from_flags(flags: &BTreeSet<String>) -> Self {
        if flags.contains(VIEW_OUTLINE_FLAG) || flags.contains(LEGACY_VIEW_CANVAS_FLAG) {
            Self::Outline
        } else {
            Self::List
        }
    }

    /// The flag written for this mode (`None` = List, written as absence).
    fn flag(self) -> Option<&'static str> {
        match self {
            Self::List => None,
            Self::Outline => Some(VIEW_OUTLINE_FLAG),
        }
    }
}

/// Height of the PR/ticket badge row `badges_for` renders when present.
const BADGE_ROW_H: f32 = 18.0;
/// Height of one compact nested subagent row (`.tile.compact`).
const SUBAGENT_TILE_H: f32 = 24.0;
/// Gap between adjacent compact subagent tiles.
const SUBAGENT_TILE_GAP: f32 = 4.0;
/// Keep rows scannable at rest. A fourth compact row communicates the
/// remainder; hovering the row swaps the preview for the complete stack.
const SUBAGENT_PREVIEW_LIMIT: usize = 3;

/// The fill bar's filled fraction: `Math.round((pct || 0) * 100)%`.
fn context_fill_fraction(pct: Option<f32>) -> f32 {
    context_pct_percent(pct).unwrap_or(0) as f32 / 100.0
}

/// Rounded whole percent, `None` for null. Everything this view renders
/// from the pct (width, fill bar, label) is determined by this value, so
/// it's also the refetch no-op key (see [`context_pct_changed`]).
fn context_pct_percent(pct: Option<f32>) -> Option<i32> {
    pct.filter(|p| p.is_finite())
        .map(|p| (p.clamp(0.0, 1.0) * 100.0).round() as i32)
}

/// `.pct-label`: `${pctLabel} context`, `pctLabel` = `N%` or `—` for null.
fn context_pct_label(pct: Option<f32>) -> String {
    match context_pct_percent(pct) {
        Some(n) => format!("{n}% context"),
        None => "— context".to_string(),
    }
}

/// Whether a landed context fetch changes anything visible — only then may
/// it dirty rows. A first fetch that comes back
/// null is no change (the row already rendered as null), and sub-percent
/// drift on a live chat is no change either (see [`context_pct_percent`]).
fn context_pct_changed(previous: Option<Option<f32>>, next: Option<f32>) -> bool {
    context_pct_percent(previous.flatten()) != context_pct_percent(next)
}

/// The context bar + `N% context` label from `renderTile`'s tile face
/// (`.fill-bar` 4px/radius 2 on `--gridline` with an `--accent` fill,
/// `.pct-label` 10px muted tabular), as shown on each list row.
fn context_meter(pct: Option<f32>, theme: &Theme) -> gpui::Div {
    let accent: gpui::Hsla = gpui::rgb(0x3987e5).into(); // --accent
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .w_full()
                .h(px(4.0))
                .rounded(px(2.0))
                .overflow_hidden()
                .bg(theme.border)
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(context_fill_fraction(pct)))
                        .bg(accent),
                ),
        )
        .child(
            div()
                .text_size(crate::typography::ui_rems(10.0))
                .text_color(theme.text_muted)
                .child(SharedString::from(context_pct_label(pct))),
        )
}

/// Width of the interactive chat panel. Wide enough that the embedded
/// composer's footer (pickers, send) isn't crushed — the composer itself
/// caps at `COMPOSER_MAX_WIDTH` (768).
const CHAT_PANEL_W: f32 = 540.0;
/// Horizontal inset of the composer inside the chat panel.
const CHAT_PANEL_COMPOSER_PAD: f32 = 10.0;
/// The embedded transcript's top inset (`Transcript::set_top_inset`): the
/// panel's own header sits ABOVE the transcript rather than overlaying it,
/// so only a little breathing room replaces the chat route's titlebar inset.
pub(crate) const CHAT_PANEL_TRANSCRIPT_TOP_INSET: f32 = 8.0;

/// The chat the interactive panel shows: always the current selection, and
/// only while the panel is open. `None` while open means the selection went
/// away (deleted/archived/new-session) and the panel should close itself.
fn panel_chat_id(panel_open: bool, selected_chat: Option<&str>) -> Option<String> {
    if !panel_open {
        return None;
    }
    selected_chat
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

pub struct Overview {
    state: Entity<AppState>,
    shell: gpui::WeakEntity<Shell>,
    scroll: ScrollHandle,
    /// The focus target for the List surface: Esc, Enter and Down
    /// from the search field land here, and a click on the surface takes it.
    body_focus: gpui::FocusHandle,
    /// The keyboard-selected List row (chat id).
    list_selected: Option<String>,
    /// Rendered List rows in on-screen order: `(chat id, child index in the
    /// scroll container)`, headers counted. Rebuilt every List render.
    list_order: Vec<(String, usize)>,
    view_mode: OverviewMode,
    /// Rows currently showing their expanded detail (toggled by a click,
    /// matching the reference's `expandedIds`/`toggleExpanded`). Never causes
    /// navigation; only the explicit "Open chat" affordance does that.
    expanded: HashSet<String>,
    link_status: HashMap<String, LinkStatusEntry>,
    link_status_pending: HashSet<String>,
    /// Subagents per chat id. Fetched once per chat (not refreshed on an
    /// interval like `link_status` — a session's own subagent set is
    /// settled once its transcript exists, unlike a PR's live checks/review
    /// state) and only for chats that came through the external-import flow
    /// (anything else has no transcript path to scan, per
    /// `SCAN_CHAT_SUBAGENTS`'s contract).
    subagents: HashMap<String, Vec<SubagentItem>>,
    subagent_pending: HashSet<String>,
    /// The list row with subagent overflow currently hovered.
    /// Kept separate from `expanded`: hover must not acknowledge a completed
    /// chat or open its detail panel.
    hovered_row: Option<String>,
    /// Last attempt time per chat (success OR failure), so a chat with
    /// nothing to scan retries on `SUBAGENTS_REFRESH` instead of on every
    /// row rebuild.
    subagents_fetched_at: HashMap<String, Instant>,
    /// Per chat: whether it had a harness session when last checked, and the
    /// earliest instant any of its row fetches (subagents, context usage,
    /// link status) can be due again. `rows()` and the idle tick skip the
    /// chat until then. Absent while a fetch is in flight or never landed.
    row_fetch_due: HashMap<String, (bool, Instant)>,
    /// `CHAT_CONTEXT_USAGE` per chat id (`None` = engine had no usage).
    /// Refetched on `CONTEXT_USAGE_REFRESH`.
    context_usage: HashMap<String, Option<f32>>,
    context_usage_pending: HashSet<String>,
    context_usage_fetched_at: HashMap<String, Instant>,
    /// Chat ids whose "done" has been acknowledged (row expanded / chat
    /// opened) — the web's `acknowledgedDone`, persisted via
    /// `overview_positions::ACKNOWLEDGED_DONE_FILE`.
    acknowledged_done: BTreeSet<String>,
    /// Repo keys the repo filter hides (web `hiddenRepos`), persisted via
    /// `overview_positions::HIDDEN_REPOS_FILE`.
    hidden_repos: BTreeSet<String>,
    /// Every repo key seen across rows before the repo/search/stale filters
    /// — the repo filter lists these so unchecking a repo never makes its
    /// own checkbox vanish (web: built from `lastSessions`).
    known_repos: BTreeSet<String>,
    /// Whether `acknowledged_done`/`hidden_repos` have been read from disk
    /// (lazily, once `AppState.data_dir` exists).
    prefs_loaded: bool,
    /// A view pref (mode / toggles / grouping) was changed by the user before
    /// `prefs_loaded` — the older file must not then overwrite it.
    view_prefs_touched: bool,
    /// The restored repo filter still has to be sanity-checked against the
    /// first non-empty chat list (see [`restored_repo_filter_hides_everything`]).
    /// One-shot so a deliberate "None" in the dropdown isn't undone later.
    repo_filter_needs_validation: bool,
    /// The Repos dropdown — the app's standard `Popup` lifecycle (exit
    /// animation + the trigger-press note that keeps a trigger click from
    /// close-then-reopening).
    repo_filter: popover::Popup<()>,
    legend_open: bool,
    /// Rows before any filter (archived-inclusion aside) — lets the empty
    /// state tell "no chats at all" from "everything filtered out".
    unfiltered_count: usize,
    refresh_ticks: u32,
    _refresh_task: gpui::Task<()>,
    /// Composable grouping dimensions, in activation (click) order — the
    /// order they appear here IS the nesting order, matching the reference's
    /// `activeGroups` insertion-order semantics (`overview_grouping::partition`'s
    /// doc comment). Empty = flat/ungrouped, the default.
    active_groups: Vec<GroupDimension>,
    /// "Hide stale" toggle — default off, matches the reference tool's own
    /// `hideStale` default (`session_canvas.html`).
    hide_stale: bool,
    /// "Show archived" toggle — default off, matches the reference's own
    /// `showArchived` default. Archived rows render heavily dimmed when on.
    show_archived: bool,
    /// The chat the List should scroll into view on the next render (opening
    /// the link editor) — consumed by the List body
    /// and cleared after that render either way.
    focus_due: Option<String>,
    /// Set whenever something that could change `rows()`'s OUTPUT happens
    /// (the underlying chat list, `hide_stale`/`show_archived`, or a
    /// link-status/subagent/context-usage fetch landing with
    /// changed data) — `false` the rest of the time, including every
    /// hover/scroll frame. `rows()` skips its expensive rebuild (walking
    /// `AppState`, cloning every `Chat`, re-deriving staleness) on a `false`
    /// read and returns the cache instead.
    rows_dirty: bool,
    perf: OverviewPerf,
    /// `AppState::overview_fingerprint` as of the last observed change.
    state_fingerprint: Option<u64>,
    /// The snapshots (in space order) `work_item_index` was built from; the
    /// index is rebuilt only when one of them is replaced.
    work_item_index_key: Option<Vec<std::sync::Arc<WorkOutlineSnapshot>>>,
    rows_cache: std::rc::Rc<Vec<OverviewRow>>,
    /// Filters `rows()` by title/cwd/preview substring (case-insensitive) —
    /// there was no way at all to search/filter across a real 227+-chat
    /// set before this. Same `ComposerInput` pattern `pickers.rs` already
    /// uses for its own search boxes.
    search: Entity<ComposerInput>,
    /// "View full chat →" opens an interactive chat panel alongside the
    /// overview instead of navigating away to `Route::Chat` — the reference
    /// tool's own `openTranscriptPanel` shape (an in-page panel, never
    /// leaving the overview), but live: it SELECTS the chat
    /// (`Shell::preview_chat_in_overview`, route untouched) and renders the
    /// Shell's own `transcript`/`composer` entities, so sending, queueing,
    /// the question wizard, interrupt, attachments and resume all work
    /// exactly as on the chat route (they key off `AppState.selected_chat`,
    /// not the route). The chat shown is always `selected_chat` — no id is
    /// stored here, so a sidebar click while the panel is open retargets it.
    ///
    /// Invariant: those entities must never render in two places in one
    /// frame. This view only renders on `Route::Overview`, and the Shell's
    /// chat stack only renders on `Route::Chat` (`render_main`'s early
    /// return), so the routes keep them exclusive.
    chat_panel_open: bool,
    /// Chat whose overview-panel identity control is showing "Copied".
    copied_panel_identity: Option<String>,
    /// Invalidates an older feedback timer when the user copies again.
    copied_panel_identity_seq: u64,
    /// The Shell's own conversation transcript (see `chat_panel_open`).
    transcript: Entity<Transcript>,
    /// The Shell's own composer (see `chat_panel_open`).
    composer: Entity<crate::composer::Composer>,
    /// The chat the panel shows this frame (`panel_chat_id`), snapshotted at
    /// the top of `render` so `render_list_row` can outline it without a
    /// `cx`.
    panel_highlight: Option<String>,
    /// The chat whose expanded detail shows the inline "Add PR / ticket…"
    /// input (at most one at a time — `link_input` is a single entity and
    /// must render in one place per frame).
    link_editor_chat: Option<String>,
    /// The link editor's own input (`PALETTE_SEARCH_CONTEXT`, so editing
    /// keys work and Enter/Escape bubble to the wrapper's key handler).
    link_input: Entity<ComposerInput>,
    /// A `SET_CHAT_LINK` from the editor is in flight (ignore repeat Enter).
    link_submitting: bool,
    /// `(chat_id, message)`: parse or RPC error to show in that chat's
    /// expanded detail (editor or unlink).
    link_error: Option<(String, String)>,
    /// The right-side panel's read-only subagent-transcript mode (see
    /// [`SubagentPeek`]). Mutually exclusive with `chat_panel_open`: opening
    /// either replaces the other (`open_subagent_peek`/`open_chat_panel`),
    /// and closing a peek returns to the bare list, never to a chat
    /// panel it replaced.
    subagent_peek: Option<SubagentPeek>,
    /// Monotonic id for peek fetches — web `transcriptRequestId`.
    peek_request_seq: u64,
    /// The peek body's scroll, pinned to the bottom when its turns land.
    peek_scroll: ScrollHandle,
    /// Focused when a peek opens, so Escape reaches the panel's key handler
    /// wherever the pointer is.
    peek_focus: gpui::FocusHandle,
    /// The Outline presentation (`OverviewMode::Outline`).
    outline: Entity<OutlineView>,
    /// Where Toggle Outline Focus returns from the tree: the last Chat opened
    /// from the Outline (Enter, Details, Required Actions) or the Chat the
    /// toggle left, with the layout it was shown in.
    outline_chat_return: Option<OutlineChatReturn>,
    /// Focus the tree on the next render: arriving from another route, its
    /// element is not mounted yet when the toggle asks for focus, and the
    /// Shell's unmounted-focus recovery would take it back.
    outline_focus_pending: bool,
    /// The pending preview-on-select (see `schedule_outline_preview`);
    /// replacing or dropping it cancels the switch.
    outline_preview: Option<gpui::Task<()>>,
    /// The Outline's inputs must be re-pushed (set whenever `rows()`
    /// rebuilds, or an Outline-only option changes).
    outline_dirty: bool,
    /// Every Chat as the Outline sees it (child and archived Chats included,
    /// with the Overview's filters folded into `visible`). Built by `rows()`.
    outline_chats: std::sync::Arc<Vec<OutlineChat>>,
    /// The explicit "reveal completed items" control (they hide while
    /// "Hide stale" is on).
    outline_show_completed: bool,
    /// Test/fixture override for the per-Space Work Outline loads (instead
    /// of `AppState::work_outline`).
    outline_fixture: Option<HashMap<String, WorkOutlineLoad>>,
    /// The Space set last declared through `AppState::set_work_outline_visible`.
    declared_outline_spaces: Option<Vec<String>>,
    /// Chat → work item lookups across every loaded Space (path chips,
    /// attention, the work-item filter). Rebuilt by `rows()`.
    work_item_index: WorkItemIndex,
    /// List work-item filter (item ids + `UNASSOCIATED_KEY`),
    /// persisted via `overview_positions::WORK_ITEM_FILTER_FILE`.
    work_item_filter: BTreeSet<String>,
    /// One-shot stale-filter check once every Space's outline has loaded.
    work_item_filter_needs_validation: bool,
    work_item_filter_popup: popover::Popup<()>,
    /// Work Item Details (Overview-owned right surface). While `Some` it
    /// renders in the right slot instead of the chat panel / peek, leaving
    /// `chat_panel_open`, `subagent_peek`, and the selection untouched, so
    /// closing it restores the previous layout.
    work_item_details: Option<Entity<WorkItemDetails>>,
    /// Expand Effective Instructions once details for this `(space, item)`
    /// open (palette "Open Effective Instructions" across a discard prompt).
    pending_effective: Option<(String, String)>,
    /// Expand Proposed instructions once details for this `(space, item)`
    /// open (Outline / chat-header proposal indicators).
    pending_proposals: Option<(String, String)>,
    /// The global Required Actions surface (Milestone 4): renders in the
    /// right slot under Work Item Details (details opened from it stack on
    /// top; closing them shows it again) and above the chat panel / peek.
    required_actions_panel:
        Option<Entity<crate::work_outline::required_actions_view::RequiredActionsList>>,
    _required_actions_events: Option<Subscription>,
    /// The Linear panel (Linear-enabled Spaces only): its own left column,
    /// independent of the right slot. `Some` while it shows.
    linear_panel: Option<Entity<crate::linear::panel::LinearPanel>>,
    _linear_panel_events: Option<Subscription>,
    /// The Linear column's persisted open state and width, its open/close
    /// tween, and an in-flight edge drag (`overview/linear_host.rs`).
    linear_column: linear_host::LinearColumnState,
    /// Width of the right-slot Details / Required Actions panel.
    details_width: details_width::DetailsWidthState,
    /// The GitHub section of the same left column (PR review sync;
    /// `overview/github_host.rs`). `Some` while it shows.
    github_panel: Option<Entity<crate::github::panel::GithubPanel>>,
    _github_panel_events: Option<Subscription>,
    /// Tests run without a Shell: PR review starts land here.
    #[cfg(test)]
    pub(crate) captured_pr_review: Option<(
        mast_proto::PullRequestRef,
        Option<(String, String)>,
        mast_proto::PrReviewAction,
    )>,
    /// "Add issues from <project>…" on a project-linked item: a picker over
    /// the Overview, independent of the Linear panel (`linear_host.rs`).
    project_issue_picker: Option<Entity<crate::linear::project_picker::ProjectIssuePicker>>,
    _project_issue_picker_events: Option<Subscription>,
    /// Tests run without a Shell: Start Chat requests land here.
    #[cfg(test)]
    pub(crate) captured_start_chat: Option<(String, String)>,
    _details_events: Option<Subscription>,
    _outline_events: Subscription,
    _observe: Subscription,
    _search_events: Subscription,
    _link_input_events: Subscription,
}

impl Overview {
    pub fn new(
        state: Entity<AppState>,
        shell: gpui::WeakEntity<Shell>,
        transcript: Entity<Transcript>,
        composer: Entity<crate::composer::Composer>,
        cx: &mut Context<Self>,
    ) -> Self {
        let observe = cx.observe(&state, |this, state, cx| {
            this.perf.observer_calls += 1;
            // Only a change to something the rows or render read marks them
            // dirty and repaints; streaming transcript frames and the like
            // leave the fingerprint alone.
            let fingerprint = state.read(cx).overview_fingerprint(Utc::now());
            if this.state_fingerprint == Some(fingerprint) {
                return;
            }
            this.state_fingerprint = Some(fingerprint);
            this.perf.observer_notifies += 1;
            this.rows_dirty = true;
            cx.notify();
        });
        let initial_fingerprint = Some(state.read(cx).overview_fingerprint(Utc::now()));
        let search = cx.new(|cx| {
            // Must be `PALETTE_SEARCH_CONTEXT`: editing keys (backspace, ⌥⌫, ⌘⌫,
            // ⌘A/C/V/Z…) are bound only to the contexts `input_bindings`
            // registers; a bespoke context got typing but no deletion.
            ComposerInput::with_context(
                "Search chats…",
                crate::composer::PALETTE_SEARCH_CONTEXT,
                cx,
            )
            .with_single_line()
            .with_accessibility_role(gpui::Role::SearchInput)
        });
        let search_events = cx.subscribe(&search, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                this.rows_dirty = true;
                cx.notify();
            }
        });
        let link_input = cx.new(|cx| {
            ComposerInput::with_context(
                "GitHub PR URL or ticket id (ENG-1234)",
                crate::composer::PALETTE_SEARCH_CONTEXT,
                cx,
            )
            .with_single_line()
        });
        let link_input_events = cx.subscribe(&link_input, |this: &mut Self, _, event, cx| {
            // A fresh edit clears a stale parse/RPC error for that editor.
            if matches!(event, ComposerInputEvent::Edited)
                && this.link_error.is_some()
                && this.link_error.as_ref().map(|(id, _)| id) == this.link_editor_chat.as_ref()
            {
                this.link_error = None;
                cx.notify();
            }
        });
        let outline = cx.new(|cx| OutlineView::new(state.clone(), cx).with_shell(shell.clone()));
        let outline_events = cx.subscribe(&outline, |this: &mut Self, _, event, cx| match event {
            OutlineEvent::OpenChat(chat_id) => {
                this.outline_preview = None;
                this.leave_work_item_details(DetailsExit::OpenChat(chat_id.clone()), cx);
            }
            OutlineEvent::SelectionMoved => this.schedule_outline_preview(cx),
            OutlineEvent::RenameChat(chat_id) => {
                if let Some(shell) = this.shell.upgrade() {
                    let chat_id = chat_id.clone();
                    shell.update(cx, |shell, cx| shell.open_rename_chat(chat_id, cx));
                }
            }
            OutlineEvent::OpenDetails { space_id, item_id } => {
                this.open_work_item_details(space_id.clone(), item_id.clone(), cx);
            }
            OutlineEvent::OpenSpaceRoot { space_id } => {
                this.open_work_item_details(
                    space_id.clone(),
                    crate::work_outline::details_panel::SPACE_ROOT_ITEM_ID.into(),
                    cx,
                );
            }
            OutlineEvent::StartChat { space_id, item_id } => {
                if let Err(notice) = this.start_chat_for_item(space_id.clone(), item_id.clone(), cx)
                {
                    this.outline
                        .update(cx, |outline, cx| outline.show_error(notice, cx));
                }
            }
            OutlineEvent::OpenProposals { space_id, item_id } => {
                this.open_proposals(space_id.clone(), item_id.clone(), cx);
            }
            OutlineEvent::OpenRequiredActions => this.open_required_actions(cx),
            OutlineEvent::LinearIssueDropped {
                space_id,
                item_id,
                issue,
            } => {
                this.linear_issue_dropped(space_id.clone(), item_id.clone(), (**issue).clone(), cx)
            }
            OutlineEvent::PullRequestDropped {
                space_id,
                item_id,
                pr,
            } => this.pull_request_dropped(space_id.clone(), item_id.clone(), (**pr).clone(), cx),
            // The toolbar's Hidden projects count.
            OutlineEvent::SpacesArranged => cx.notify(),
        });
        // Idle refresh — same `cx.spawn` + `background_executor().timer`
        // loop idiom as the composer's queue-lease renewal. Ends on its own
        // once the entity is dropped (`update` fails).
        let refresh_task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(IDLE_REFRESH).await;
                if this.update(cx, |this, cx| this.refresh_tick(cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            state,
            shell,
            scroll: ScrollHandle::new(),
            body_focus: cx.focus_handle(),
            list_selected: None,
            list_order: Vec::new(),
            view_mode: OverviewMode::List,
            expanded: HashSet::new(),
            link_status: HashMap::new(),
            link_status_pending: HashSet::new(),
            subagents: HashMap::new(),
            subagent_pending: HashSet::new(),
            hovered_row: None,
            subagents_fetched_at: HashMap::new(),
            row_fetch_due: HashMap::new(),
            context_usage: HashMap::new(),
            context_usage_pending: HashSet::new(),
            context_usage_fetched_at: HashMap::new(),
            acknowledged_done: BTreeSet::new(),
            hidden_repos: BTreeSet::new(),
            known_repos: BTreeSet::new(),
            prefs_loaded: false,
            view_prefs_touched: false,
            repo_filter_needs_validation: false,
            repo_filter: popover::Popup::default(),
            legend_open: false,
            unfiltered_count: 0,
            refresh_ticks: 0,
            _refresh_task: refresh_task,
            active_groups: Vec::new(),
            hide_stale: false,
            show_archived: false,
            focus_due: None,
            rows_dirty: true,
            perf: OverviewPerf::default(),
            state_fingerprint: initial_fingerprint,
            work_item_index_key: None,
            rows_cache: std::rc::Rc::default(),
            search,
            chat_panel_open: false,
            outline_chat_return: None,
            outline_focus_pending: false,
            outline_preview: None,
            copied_panel_identity: None,
            copied_panel_identity_seq: 0,
            transcript,
            composer,
            panel_highlight: None,
            link_editor_chat: None,
            link_input,
            link_submitting: false,
            link_error: None,
            subagent_peek: None,
            peek_request_seq: 0,
            peek_scroll: ScrollHandle::new(),
            peek_focus: cx.focus_handle(),
            outline,
            outline_dirty: true,
            outline_chats: std::sync::Arc::default(),
            outline_show_completed: false,
            outline_fixture: None,
            declared_outline_spaces: None,
            work_item_index: WorkItemIndex::default(),
            work_item_filter: BTreeSet::new(),
            work_item_filter_needs_validation: false,
            work_item_filter_popup: popover::Popup::default(),
            work_item_details: None,
            pending_effective: None,
            pending_proposals: None,
            required_actions_panel: None,
            _required_actions_events: None,
            linear_panel: None,
            _linear_panel_events: None,
            linear_column: Default::default(),
            details_width: Default::default(),
            github_panel: None,
            _github_panel_events: None,
            #[cfg(test)]
            captured_pr_review: None,
            project_issue_picker: None,
            _project_issue_picker_events: None,
            #[cfg(test)]
            captured_start_chat: None,
            _details_events: None,
            _outline_events: outline_events,
            _observe: observe,
            _search_events: search_events,
            _link_input_events: link_input_events,
        }
    }

    /// Cmd/Ctrl+F follows the platform Find convention in every Overview
    /// mode (List and Outline share the toolbar search, which also
    /// filters the Outline). Selecting the existing query makes a repeated
    /// search immediately replaceable.
    pub(crate) fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let focus = gpui::Focusable::focus_handle(self.search.read(cx), cx);
        window.focus(&focus, cx);
        window.dispatch_action(Box::new(crate::composer::SelectAll), cx);
        true
    }

    /// Whether the toolbar search field holds focus.
    #[cfg(test)]
    pub(crate) fn search_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        gpui::Focusable::focus_handle(self.search.read(cx), cx).is_focused(window)
    }

    /// Hand focus from the search field to the body of the current mode:
    /// the Outline tree, or the List surface. With `first_row` (Enter /
    /// Down) the first matching row is selected as well, so the arrow keys
    /// work at once.
    fn leave_search(&mut self, first_row: bool, window: &mut Window, cx: &mut Context<Self>) {
        match self.view_mode {
            OverviewMode::Outline => {
                self.outline.update(cx, |outline, cx| {
                    if first_row {
                        outline.focus_first_row(window, cx);
                    } else {
                        outline.focus(window, cx);
                    }
                });
            }
            OverviewMode::List => {
                window.focus(&self.body_focus, cx);
                if first_row {
                    self.select_list_row(0, cx);
                }
            }
        }
        cx.notify();
    }

    /// Select the `ix`-th rendered List row and scroll it into view.
    fn select_list_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some((chat_id, child)) = self.list_order.get(ix).cloned() else {
            return;
        };
        self.scroll.scroll_to_item(child);
        self.list_selected = Some(chat_id);
        cx.notify();
    }

    /// Keys on the search field. Esc clears a non-empty query, else returns
    /// focus to the mode's body; Enter / Down move to the first matching
    /// row. Anything else (typing, editing) is left to the field. Esc with
    /// an empty query while a side panel is open falls through to the
    /// Overview's own handler, which closes the panel.
    fn search_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.modifiers.modified() {
            return;
        }
        match event.keystroke.key.as_str() {
            "escape" => {
                if !self.search.read(cx).text().is_empty() {
                    self.search.update(cx, |search, cx| search.set_text("", cx));
                } else if self.work_item_details.is_some()
                    || self.required_actions_panel.is_some()
                    || self.subagent_peek.is_some()
                {
                    return;
                } else {
                    self.leave_search(false, window, cx);
                }
            }
            "enter" | "down" => self.leave_search(true, window, cx),
            _ => return,
        }
        cx.stop_propagation();
        window.prevent_default();
    }

    /// Keys on the focused List surface (not on an input inside a row):
    /// Up / Down move the selection (Up from the first row returns to the
    /// search), Enter opens the selected chat in the side panel.
    fn list_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.view_mode != OverviewMode::List
            || !self.body_focus.is_focused(window)
            || event.keystroke.modifiers.modified()
        {
            return;
        }
        let current = self
            .list_selected
            .as_ref()
            .and_then(|id| self.list_order.iter().position(|(row, _)| row == id));
        match event.keystroke.key.as_str() {
            "down" => match current {
                Some(ix) if ix + 1 < self.list_order.len() => self.select_list_row(ix + 1, cx),
                Some(_) => {}
                None => self.select_list_row(0, cx),
            },
            "up" => match current {
                Some(ix) if ix > 0 => self.select_list_row(ix - 1, cx),
                _ => {
                    self.focus_search(window, cx);
                }
            },
            "enter" => match self.list_selected.clone() {
                Some(chat_id) if current.is_some() => self.open_chat_panel(chat_id, cx),
                _ => self.select_list_row(0, cx),
            },
            _ => return,
        }
        cx.stop_propagation();
        window.prevent_default();
    }

    /// Click-to-expand (list row). Expanding also acknowledges an
    /// unread done — web `toggleExpanded` → `acknowledgeDone`.
    fn toggle_expanded(&mut self, chat_id: &str, cx: &Context<Self>) {
        if !self.expanded.remove(chat_id) {
            self.expanded.insert(chat_id.to_string());
            self.acknowledge_done(chat_id, cx);
        }
    }

    /// Web `acknowledgeDone`: only meaningful while the chat is actually
    /// done — acknowledging a non-done chat would be reconciled straight
    /// back out on the next row rebuild anyway, so it's skipped up front.
    fn acknowledge_done(&mut self, chat_id: &str, cx: &Context<Self>) {
        let is_done = self
            .rows_cache
            .iter()
            .any(|r| r.chat.id == chat_id && r.status == ChatIndicator::Completed);
        if is_done && self.acknowledged_done.insert(chat_id.to_string()) {
            self.save_string_set(
                overview_positions::ACKNOWLEDGED_DONE_FILE,
                &self.acknowledged_done,
                cx,
            );
        }
    }

    /// Read the persisted acknowledged/hidden-repo sets once `data_dir`
    /// exists (it's `None` until engine bootstrap finishes — retried each
    /// render until then, unlike positions' one-shot flag).
    fn ensure_prefs_loaded(&mut self, cx: &mut Context<Self>) {
        if self.prefs_loaded {
            return;
        }
        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
            return;
        };
        self.prefs_loaded = true;
        let flags =
            overview_positions::load_string_set(&data_dir, overview_positions::UI_FLAGS_FILE);
        self.restore_linear_column(&flags);
        self.restore_details_width(&flags);
        self.acknowledged_done
            .extend(overview_positions::load_string_set(
                &data_dir,
                overview_positions::ACKNOWLEDGED_DONE_FILE,
            ));
        self.hidden_repos
            .extend(overview_positions::load_string_set(
                &data_dir,
                overview_positions::HIDDEN_REPOS_FILE,
            ));
        self.work_item_filter
            .extend(work_item_filter::load_filter(&data_dir));
        if !self.work_item_filter.is_empty() {
            self.work_item_filter_needs_validation = true;
            self.rows_dirty = true;
        }
        if !self.hidden_repos.is_empty() {
            self.repo_filter_needs_validation = true;
            self.rows_dirty = true;
        }
        // A toggle made before `data_dir` existed wins over the older file.
        if !self.view_prefs_touched {
            let prefs = view_prefs_from_flags(&flags);
            self.view_mode = prefs.mode;
            self.hide_stale = prefs.hide_stale;
            self.show_archived = prefs.show_archived;
            self.active_groups = prefs.groups;
            self.rows_dirty = true;
        }
    }

    /// The current view state as [`ViewPrefs`].
    fn view_prefs(&self) -> ViewPrefs {
        ViewPrefs {
            mode: self.view_mode,
            hide_stale: self.hide_stale,
            show_archived: self.show_archived,
            groups: self.active_groups.clone(),
        }
    }

    /// Persist the view state (mode, Hide stale / Show archived, grouping
    /// dimensions + order) into `UI_FLAGS_FILE`, read-modify-write like
    /// [`Overview::persist_ui_flag`]. Call after every change to any of them.
    fn view_prefs_changed(&mut self, cx: &Context<Self>) {
        if !self.prefs_loaded {
            self.view_prefs_touched = true;
        }
        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
            return;
        };
        let mut flags =
            overview_positions::load_string_set(&data_dir, overview_positions::UI_FLAGS_FILE);
        apply_view_prefs_to_flags(&mut flags, &self.view_prefs());
        self.save_string_set(overview_positions::UI_FLAGS_FILE, &flags, cx);
    }

    fn save_string_set(&self, file_name: &str, set: &BTreeSet<String>, cx: &Context<Self>) {
        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
            return;
        };
        if let Err(err) = overview_positions::save_string_set(&data_dir, file_name, set) {
            tracing::warn!(error = %err, file = file_name, "saving overview state failed");
        }
    }

    /// Set/clear one entry of `UI_FLAGS_FILE`. Read-modify-write so every
    /// other flag in the file survives.
    fn persist_ui_flag(&self, flag: String, on: bool, cx: &Context<Self>) {
        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
            return;
        };
        let mut flags =
            overview_positions::load_string_set(&data_dir, overview_positions::UI_FLAGS_FILE);
        if on {
            flags.insert(flag);
        } else {
            flags.remove(&flag);
        }
        self.save_string_set(overview_positions::UI_FLAGS_FILE, &flags, cx);
    }

    /// Any repo-filter change: persist, and re-derive rows.
    fn repo_filter_changed(&mut self, cx: &mut Context<Self>) {
        self.save_string_set(
            overview_positions::HIDDEN_REPOS_FILE,
            &self.hidden_repos,
            cx,
        );
        self.rows_dirty = true;
        cx.notify();
    }

    /// One idle-refresh tick (see [`IDLE_REFRESH`]). Skipped entirely while
    /// the overview entity is alive but another route is on screen. Re-runs
    /// every visible row's TTL-gated fetches (no-ops when fresh), then
    /// repaints for relative times. Does NOT mark rows dirty (except the ~60s
    /// staleness re-derive) — a fetch that lands with changed data does that
    /// itself.
    fn refresh_tick(&mut self, cx: &mut Context<Self>) {
        let visible = self
            .shell
            .upgrade()
            .is_some_and(|shell| shell.read(cx).is_overview_route());
        if !visible {
            return;
        }
        self.refresh_ticks = self.refresh_ticks.wrapping_add(1);
        let targets: Vec<(String, bool)> = self
            .rows_cache
            .iter()
            .map(|r| (r.chat.id.clone(), r.chat.harness_session_id.is_some()))
            .collect();
        for (chat_id, imported) in targets {
            self.ensure_row_fetches(chat_id, imported, cx);
        }
        if self
            .refresh_ticks
            .is_multiple_of(STALENESS_REDERIVE_EVERY_TICKS)
        {
            self.rows_dirty = true;
        }
        cx.notify();
    }

    /// Every per-row TTL-gated lookup, in one place so `rows()` and
    /// `refresh_tick` can't drift. Subagents only apply to chats with a
    /// harness session (anything else has no transcript to scan, per
    /// `SCAN_CHAT_SUBAGENTS`'s contract).
    /// Context usage applies to every chat: its live source is the engine's
    /// own doc store, not an import cursor.
    fn ensure_row_fetches(
        &mut self,
        chat_id: String,
        has_harness_session: bool,
        cx: &mut Context<Self>,
    ) {
        if self
            .row_fetch_due
            .get(&chat_id)
            .is_some_and(|(had_session, due)| {
                *had_session == has_harness_session && Instant::now() < *due
            })
        {
            return;
        }
        self.perf.fetch_checks += 1;
        if has_harness_session {
            self.ensure_subagents(chat_id.clone(), cx);
        }
        self.ensure_context_usage(chat_id.clone(), cx);
        self.ensure_link_status(chat_id.clone(), cx);
        match self.next_row_fetch_due(&chat_id, has_harness_session) {
            Some(due) => {
                self.row_fetch_due
                    .insert(chat_id, (has_harness_session, due));
            }
            None => {
                self.row_fetch_due.remove(&chat_id);
            }
        }
    }

    /// When the soonest of a chat's row fetches goes stale. `None` when any
    /// is in flight or has no recorded result yet (so it must be re-checked
    /// next time rather than skipped).
    fn next_row_fetch_due(&self, chat_id: &str, has_harness_session: bool) -> Option<Instant> {
        if self.link_status_pending.contains(chat_id)
            || self.context_usage_pending.contains(chat_id)
            || self.subagent_pending.contains(chat_id)
        {
            return None;
        }
        let link = self.link_status.get(chat_id)?.fetched_at + LINK_STATUS_REFRESH;
        let context = *self.context_usage_fetched_at.get(chat_id)? + CONTEXT_USAGE_REFRESH;
        let mut due = link.min(context);
        if has_harness_session {
            let ttl = if self
                .subagents
                .get(chat_id)
                .is_some_and(|subs| subs.iter().any(|s| s.status == SubagentStatus::Running))
            {
                SUBAGENTS_RUNNING_REFRESH
            } else {
                SUBAGENTS_REFRESH
            };
            due = due.min(*self.subagents_fetched_at.get(chat_id)? + ttl);
        }
        Some(due)
    }

    fn toggle_group(&mut self, dim: GroupDimension, cx: &Context<Self>) {
        if let Some(ix) = self.active_groups.iter().position(|&d| d == dim) {
            self.active_groups.remove(ix);
        } else {
            self.active_groups.push(dim);
        }
        // Cache key includes `active_groups`, so this alone would naturally
        // miss on the next read anyway — dropped explicitly for clarity.
        self.view_prefs_changed(cx);
    }

    /// The current rows AND, as a side effect, kicks off any missing/stale
    /// PR-ticket-link and subagent lookups for what's visible right now.
    /// Combined into one pass rather than a separate "ensure" sweep so a
    /// freshly-appeared chat (new import, new sidebar row) gets its lookups
    /// queued the same render it first appears in, not one frame later.
    fn rows(&mut self, cx: &mut Context<Self>) -> std::rc::Rc<Vec<OverviewRow>> {
        // See `rows_dirty`'s doc comment: hover and scroll call `cx.notify()`
        // (hence `render()`, hence this) on every pointer-move, and without
        // this short-circuit every one of those frames re-walked `AppState`,
        // cloned every `Chat`, and re-derived staleness for all 227+ rows. A stale
        // cache is bounded by whatever next marks `rows_dirty` (any real
        // `AppState` change already does, via the `cx.observe` in `new`, so
        // this isn't "never refreshes" — it's "doesn't redundantly rebuild
        // on frames where nothing relevant changed").
        if !self.rows_dirty {
            return std::rc::Rc::clone(&self.rows_cache);
        }

        self.perf.rows_rebuilds += 1;
        let now = Utc::now();
        // Work Outline facts for path chips, attention, and the work-item
        // filter.
        let outline_spaces = self.outline_spaces(cx);
        let snapshots: Vec<std::sync::Arc<WorkOutlineSnapshot>> = outline_spaces
            .iter()
            .filter_map(|space| space.load.snapshot().cloned())
            .collect();
        let index_is_current = self.work_item_index_key.as_ref().is_some_and(|key| {
            key.len() == snapshots.len()
                && key
                    .iter()
                    .zip(&snapshots)
                    .all(|(built_from, now)| std::sync::Arc::ptr_eq(built_from, now))
        });
        let work_item_index = if index_is_current {
            std::mem::take(&mut self.work_item_index)
        } else {
            WorkItemIndex::build(snapshots.iter().map(|s| s.as_ref()))
        };
        // Holding the Arcs keeps their addresses from being reused, so
        // `ptr_eq` can't match a different snapshot.
        self.work_item_index_key = Some(snapshots.clone());
        let outlines_loaded = outline_spaces
            .iter()
            .all(|space| !matches!(space.load, WorkOutlineLoad::Loading));
        let (raw, spaces): (Vec<(ChatIndicator, Chat)>, HashMap<String, Space>) = {
            let state = self.state.read(cx);
            (
                state
                    .overview_chats_with_archived(now, self.show_archived)
                    .into_iter()
                    .map(|(status, chat)| (status, chat.clone()))
                    .collect(),
                state
                    .spaces
                    .iter()
                    .map(|space| (space.id.clone(), space.clone()))
                    .collect(),
            )
        };
        self.unfiltered_count = raw.len();
        let hide_stale = self.hide_stale;
        let query = self.search.read(cx).text().trim().to_lowercase();
        let mut known_repos: BTreeSet<String> = BTreeSet::new();
        let mut acknowledged_changed = false;
        let mut all: Vec<OverviewRow> = Vec::with_capacity(raw.len());
        for (status, chat) in raw {
            let stale = is_stale(&chat, status, now);
            self.ensure_row_fetches(chat.id.clone(), chat.harness_session_id.is_some(), cx);
            let link = self.link_status.get(&chat.id).map(|e| &e.status);
            let ticket = link
                .and_then(|s| s.ticket.as_ref())
                .map(|t| t.identifier.clone());
            let prs = link.map(effective_pr_links).unwrap_or_default();
            let pr_number = prs.first().map(|link| {
                link.detail
                    .as_ref()
                    .map_or_else(|| pr_number_from_url(&link.url), |pr| pr.number)
            });
            let pr_title = prs
                .first()
                .and_then(|link| link.detail.as_ref()?.title.clone());
            let pr_search_values = prs
                .iter()
                .flat_map(|link| {
                    let number = link
                        .detail
                        .as_ref()
                        .map_or_else(|| pr_number_from_url(&link.url), |pr| pr.number);
                    let mut values =
                        vec![number.to_string(), format!("#{number}"), link.url.clone()];
                    if let Some(title) = link.detail.as_ref().and_then(|pr| pr.title.clone()) {
                        values.push(title);
                    }
                    values
                })
                .collect();
            let subagent_count = self.subagents.get(&chat.id).map_or(0, Vec::len);
            let context_pct = self.context_usage.get(&chat.id).copied().flatten();
            acknowledged_changed |= reconcile_acknowledged(
                &mut self.acknowledged_done,
                &chat.id,
                status == ChatIndicator::Completed,
            );
            let space = chat
                .space_id
                .as_deref()
                .and_then(|space_id| spaces.get(space_id));
            let repo = repo_key_for_chat(&chat, space);
            let closeable = crate::chat_closeout::is_closeout_candidate(chat.cwd.as_deref());
            known_repos.insert(repo.clone());
            let work_item_path = work_item_index
                .item_for_chat(&chat.id)
                .and_then(|item| work_item_index.path_label(item));
            let (user_action_required, user_action_count) =
                work_item_index.attention_for_chat(&chat.id);
            let instruction_drift = work_item_index.instruction_drift_for_chat(&chat.id);
            let execution_exception = work_item_index.execution_exception_for_chat(&chat.id);
            all.push(OverviewRow {
                status,
                chat,
                stale,
                ticket,
                repo,
                pr_number,
                pr_title,
                pr_search_values,
                context_pct,
                subagent_count,
                closeable,
                work_item_path,
                user_action_required,
                user_action_count,
                instruction_drift,
                execution_exception,
            });
        }
        self.known_repos = known_repos;
        if self.repo_filter_needs_validation && !self.known_repos.is_empty() {
            self.repo_filter_needs_validation = false;
            if restored_repo_filter_hides_everything(&self.hidden_repos, &self.known_repos) {
                tracing::info!("restored repo filter hid every chat; resetting it");
                self.hidden_repos.clear();
                self.save_string_set(
                    overview_positions::HIDDEN_REPOS_FILE,
                    &self.hidden_repos,
                    cx,
                );
            }
        }
        if self.work_item_filter_needs_validation && outlines_loaded && !all.is_empty() {
            self.work_item_filter_needs_validation = false;
            if work_item_filter::restored_filter_hides_everything(
                &self.work_item_filter,
                &work_item_index,
                all.iter().map(|row| row.chat.id.as_str()),
            ) {
                tracing::info!("restored work-item filter hid every chat; resetting it");
                self.work_item_filter.clear();
                self.save_string_set(
                    overview_positions::WORK_ITEM_FILTER_FILE,
                    &self.work_item_filter,
                    cx,
                );
            }
        }
        self.outline_chats = std::sync::Arc::new(self.build_outline_chats(&all, now, cx));
        self.outline_dirty = true;
        if acknowledged_changed {
            self.save_string_set(
                overview_positions::ACKNOWLEDGED_DONE_FILE,
                &self.acknowledged_done,
                cx,
            );
        }
        let hidden_repos = &self.hidden_repos;
        let work_filter = &self.work_item_filter;
        let rows: Vec<OverviewRow> = all
            .into_iter()
            .filter(|row| !(hide_stale && row.stale))
            .filter(|row| !hidden_repos.contains(&row.repo))
            .filter(|row| {
                work_item_filter::chat_passes(work_filter, &work_item_index, &row.chat.id)
            })
            .filter(|row| {
                // Web `sessionMatchesSearch` fields that exist here: name
                // (title), branch, last message (preview), cwd, ticket id,
                // PR title. Mast has no separate `summary` field.
                let mut fields = vec![
                    row.chat.title.as_deref(),
                    row.chat.branch.as_deref(),
                    row.chat.last_message_preview.as_deref(),
                    row.chat.cwd.as_deref(),
                    row.ticket.as_deref(),
                    row.pr_title.as_deref(),
                ];
                fields.extend(row.pr_search_values.iter().map(String::as_str).map(Some));
                matches_search(&query, &fields)
            })
            .collect();
        self.work_item_index = work_item_index;
        let rows = std::rc::Rc::new(rows);
        self.rows_cache = std::rc::Rc::clone(&rows);
        self.rows_dirty = false;
        rows
    }

    /// Every Chat as the Outline sees it: the Overview's rows (pre-filter)
    /// plus child Chats and hidden (archived, stale, repo-filtered) Chats,
    /// with those filters folded into `visible` so an associated Chat that
    /// is merely filtered is never mistaken for a missing one.
    fn build_outline_chats(
        &self,
        rows: &[OverviewRow],
        now: chrono::DateTime<Utc>,
        cx: &gpui::App,
    ) -> Vec<OutlineChat> {
        let by_id: HashMap<&str, &OverviewRow> =
            rows.iter().map(|row| (row.chat.id.as_str(), row)).collect();
        let state = self.state.read(cx);
        state
            .chats
            .iter()
            .filter(|chat| {
                chat.space_id
                    .as_deref()
                    .is_none_or(|space_id| state.space_row(space_id).is_some())
            })
            .map(|chat| {
                let row = by_id.get(chat.id.as_str());
                let status = row.map_or_else(|| state.display_status_for(chat, now), |r| r.status);
                let stale = row.map_or_else(|| is_stale(chat, status, now), |r| r.stale);
                let shown_by_toggles =
                    (self.show_archived || !chat.archived) && !(self.hide_stale && stale);
                let visible = shown_by_toggles && {
                    let repo = match row {
                        Some(row) => row.repo.clone(),
                        None => repo_key_for_chat(
                            chat,
                            chat.space_id.as_deref().and_then(|id| state.space_row(id)),
                        ),
                    };
                    !self.hidden_repos.contains(&repo)
                };
                let pr_number = row.and_then(|r| r.pr_number).or_else(|| {
                    self.link_status.get(&chat.id).and_then(|entry| {
                        effective_pr_links(&entry.status).first().map(|link| {
                            link.detail
                                .as_ref()
                                .map_or_else(|| pr_number_from_url(&link.url), |pr| pr.number)
                        })
                    })
                });
                OutlineChat {
                    id: chat.id.clone(),
                    space_id: chat.space_id.clone(),
                    parent_chat_id: chat.parent_chat_id.clone(),
                    title: chat
                        .title
                        .clone()
                        .unwrap_or_else(|| "New session".to_string()),
                    status,
                    archived: chat.archived,
                    stale,
                    branch: chat.branch.clone(),
                    harness: Some(chat_model_label(chat)),
                    pr_number,
                    visible,
                    activity: chat
                        .last_message_at
                        .unwrap_or(chat.created_at)
                        .timestamp_millis(),
                }
            })
            .collect()
    }

    /// Every Space the Outline shows (all of them: the Overview is the
    /// All-spaces view), in display order, with its current load.
    fn outline_spaces(&self, cx: &gpui::App) -> Vec<OutlineSpace> {
        let state = self.state.read(cx);
        let mut spaces: Vec<&Space> = state.spaces.iter().collect();
        spaces.sort_by_cached_key(|space| (space.display_name().to_lowercase(), space.id.clone()));
        spaces
            .into_iter()
            .map(|space| OutlineSpace {
                id: space.id.clone(),
                label: space.display_name().to_string(),
                load: match &self.outline_fixture {
                    Some(map) => map
                        .get(&space.id)
                        .cloned()
                        .unwrap_or(WorkOutlineLoad::Loading),
                    None => state.work_outline(&space.id),
                },
                // The Outline applies its own hidden set (`OutlineView`).
                hidden: false,
            })
            .collect()
    }

    fn outline_snapshot(
        &self,
        space_id: &str,
        cx: &gpui::App,
    ) -> Option<std::sync::Arc<WorkOutlineSnapshot>> {
        match &self.outline_fixture {
            Some(map) => map.get(space_id).and_then(|load| load.snapshot().cloned()),
            None => self
                .state
                .read(cx)
                .work_outline(space_id)
                .snapshot()
                .cloned(),
        }
    }

    fn open(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.acknowledge_done(&chat_id, cx);
        let _ = self.shell.update(cx, |shell, cx| {
            shell.open_chat(chat_id, cx);
        });
    }

    /// Open the interactive chat panel for `chat_id` alongside the overview,
    /// instead of navigating away — see `chat_panel_open`'s doc comment.
    /// Selecting the chat is what wires the Shell's transcript/composer to
    /// it; re-clicking the already-shown chat just refocuses the composer.
    fn open_chat_panel(&mut self, chat_id: String, cx: &mut Context<Self>) {
        // Web `openTranscriptPanel` → `acknowledgeDone`.
        self.acknowledge_done(&chat_id, cx);
        let _ = self.shell.update(cx, |shell, cx| {
            shell.preview_chat_in_overview(chat_id, cx);
        });
        // The live panel replaces a subagent peek (one right-side panel).
        self.subagent_peek = None;
        self.chat_panel_open = true;
        cx.notify();
    }

    /// Clicking a compact subagent row: open the right-side panel in its
    /// read-only transcript mode (web `openTranscriptPanel(session.id,
    /// sub.id, title)`), replacing whatever the panel showed — including the
    /// live chat panel, which is closed, not stacked underneath. Issues a
    /// fresh `READ_SUBAGENT_TRANSCRIPT` under a new request id every time
    /// (re-clicking the same subagent reloads it), so a slower earlier reply
    /// can never overwrite a newer peek ([`peek_reply_is_current`]).
    fn open_subagent_peek(
        &mut self,
        chat_id: String,
        sub: &SubagentItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.peek_request_seq += 1;
        let request_id = self.peek_request_seq;
        self.chat_panel_open = false;
        self.subagent_peek = Some(SubagentPeek {
            chat_id: chat_id.clone(),
            agent_id: sub.agent_id.clone(),
            name: sub.display_name(),
            agent_type: sub.agent_type.clone(),
            request_id,
            load: PeekLoad::Loading,
            open_tools: HashSet::new(),
        });
        window.focus(&self.peek_focus, cx);
        cx.notify();
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.apply_peek_reply(request_id, None, cx);
            return;
        };
        let agent_id = sub.agent_id.clone();
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::READ_SUBAGENT_TRANSCRIPT,
                    serde_json::json!({ "chatId": chat_id, "agentId": agent_id }),
                )
                .await;
            let decoded = result
                .ok()
                .and_then(|value| serde_json::from_value::<SubagentTranscript>(value).ok());
            this.update(cx, |this, cx| {
                this.apply_peek_reply(request_id, decoded, cx)
            })
            .ok();
        })
        .detach();
    }

    /// Land a peek fetch (`None` = RPC or decode failure) — dropped unless
    /// it's still the current peek's request.
    fn apply_peek_reply(
        &mut self,
        request_id: u64,
        reply: Option<SubagentTranscript>,
        cx: &mut Context<Self>,
    ) {
        if !peek_reply_is_current(self.subagent_peek.as_ref(), request_id) {
            return;
        }
        let Some(peek) = self.subagent_peek.as_mut() else {
            return;
        };
        peek.load = match reply {
            Some(transcript) => PeekLoad::Loaded(transcript),
            None => PeekLoad::Failed,
        };
        // Web `renderTurns` → `scrollTop = scrollHeight`: open at the end.
        self.peek_scroll.scroll_to_bottom();
        cx.notify();
    }

    /// ✕ / Escape on a peek: back to the bare list.
    fn close_subagent_peek(&mut self, cx: &mut Context<Self>) {
        if self.subagent_peek.take().is_some() {
            cx.notify();
        }
    }

    /// Hide the panel. The chat stays selected (like any sidebar pick) —
    /// deselecting would drop the user back onto the blank new-session
    /// canvas the next time they visit the chat route.
    fn close_chat_panel(&mut self, cx: &mut Context<Self>) {
        if self.chat_panel_open {
            self.chat_panel_open = false;
            cx.notify();
        }
    }

    /// Copy the same debugging identity exposed by the full-chat header and
    /// sidebar menu, but from the overview's embedded live-chat panel.
    fn copy_panel_chat_identity(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let Some(text) = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .map(crate::shell::chat_identity_for)
        else {
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.copied_panel_identity = Some(chat_id.clone());
        self.copied_panel_identity_seq = self.copied_panel_identity_seq.wrapping_add(1);
        let seq = self.copied_panel_identity_seq;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1200))
                .await;
            this.update(cx, |this, cx| {
                if this.copied_panel_identity_seq == seq {
                    this.copied_panel_identity = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The chat the panel currently shows — `Some` only while it's open and
    /// something is selected (see [`panel_chat_id`]).
    fn panel_chat_id(&self, cx: &Context<Self>) -> Option<String> {
        panel_chat_id(
            self.chat_panel_open,
            self.state.read(cx).selected_chat.as_deref(),
        )
    }

    /// "Close out worktree…": the confirmation dialog is Shell-owned (it's a
    /// window modal, shared with the sidebar chat menu's entry point).
    fn open_closeout(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let _ = self.shell.update(cx, |shell, cx| {
            shell.open_chat_closeout(chat_id, cx);
        });
    }

    fn set_archived(&mut self, chat_id: String, archived: bool, cx: &mut Context<Self>) {
        let _ = self.shell.update(cx, |shell, cx| {
            shell.set_chat_archived(chat_id, archived, cx);
        });
    }

    fn ensure_link_status(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let fresh_enough = self
            .link_status
            .get(&chat_id)
            .is_some_and(|e| e.fetched_at.elapsed() < LINK_STATUS_REFRESH);
        if fresh_enough || self.link_status_pending.contains(&chat_id) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.link_status_pending.insert(chat_id.clone());
        self.perf.fetch_requests += 1;
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::CHAT_LINK_STATUS,
                    serde_json::json!({ "chatId": chat_id }),
                )
                .await;
            this.update(cx, |this, cx| {
                this.link_status_pending.remove(&chat_id);
                if let Ok(value) = result
                    && let Ok(status) = serde_json::from_value::<OverviewLinkStatus>(value.clone())
                {
                    let links: Vec<_> = status
                        .pr_links
                        .iter()
                        .filter_map(|link| Some((link.url.clone(), link.source?)))
                        .collect();
                    crate::review_chats::note_chat_links(cx, &chat_id, &links);
                    let diff_stat = value
                        .get("diffStat")
                        .and_then(|v| serde_json::from_value::<DiffStat>(v.clone()).ok());
                    let is_worktree = value.get("isWorktree").and_then(|v| v.as_bool());
                    // `prSource`/`ticketSource` ride the `ChatLinkStatus`
                    // decode itself (both `Option`, so an older engine that
                    // omits them reads as "inferred"). Merged over the
                    // previous entry so a just-set manual link's placeholder
                    // survives the engine's detail-cache miss.
                    let status = merge_refetched_link(
                        this.link_status.get(&chat_id).map(|e| &e.status),
                        status,
                    );
                    let fingerprint = link_fingerprint(&status);
                    let previous = this
                        .link_status
                        .insert(
                            chat_id,
                            LinkStatusEntry {
                                status,
                                diff_stat,
                                is_worktree,
                                fetched_at: Instant::now(),
                            },
                        )
                        .map(|old| link_fingerprint(&old.status));
                    // `rows()`'s cache bakes ticket/PR number/PR title in
                    // (grouping key + search fields) — only a CHANGE to
                    // those invalidates rows; a
                    // same-data TTL refetch just repaints (badges/detail
                    // read `link_status` live).
                    if previous.as_ref() != Some(&fingerprint) {
                        this.rows_dirty = true;
                    }
                }
                // Fetch failure: the stale entry (if any) keeps rendering;
                // its expired `fetched_at` retries on the next tick.
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Open the inline "Add PR / ticket…" editor in `chat_id`'s expanded
    /// detail (expanding it if needed), focused and empty. Also the entry
    /// point for the sidebar chat menu's "Add PR / ticket…", which switches
    /// to the overview first; `reveal` then brings the row into view
    /// the same way a PR-sidebar jump does.
    pub(crate) fn open_link_editor(
        &mut self,
        chat_id: String,
        reveal: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.expanded.insert(chat_id.clone()) {
            self.acknowledge_done(&chat_id, cx);
        }
        self.link_editor_chat = Some(chat_id.clone());
        self.link_submitting = false;
        self.link_error = None;
        self.link_input
            .update(cx, |input, cx| input.set_text("", cx));
        window.focus(
            &gpui::Focusable::focus_handle(self.link_input.read(cx), cx),
            cx,
        );
        if reveal {
            self.focus_due = Some(chat_id);
        }
        cx.notify();
    }

    fn close_link_editor(&mut self, cx: &mut Context<Self>) {
        if let Some(chat_id) = self.link_editor_chat.take() {
            if self
                .link_error
                .as_ref()
                .is_some_and(|(id, _)| *id == chat_id)
            {
                self.link_error = None;
            }
            self.link_submitting = false;
            cx.notify();
        }
    }

    /// Enter in the link editor: parse ([`parse_link_input`]) and, if it's a
    /// PR URL or ticket id, write it via `SET_CHAT_LINK`; otherwise show the
    /// parse error inline and keep the editor open.
    fn submit_link_editor(&mut self, cx: &mut Context<Self>) {
        let Some(chat_id) = self.link_editor_chat.clone() else {
            return;
        };
        if self.link_submitting {
            return;
        }
        match parse_link_input(self.link_input.read(cx).text()) {
            Err(message) => {
                self.link_error = Some((chat_id, message.to_string()));
                cx.notify();
            }
            Ok(parsed) => {
                self.link_submitting = true;
                self.link_error = None;
                self.set_chat_link(
                    chat_id,
                    parsed.kind,
                    Some(parsed.value),
                    LinkOperation::Add,
                    cx,
                );
            }
        }
    }

    /// `SET_CHAT_LINK` (always a manual write; `value: None` unlinks). On
    /// success: optimistic local update ([`apply_link_reply`]), rows
    /// invalidated (ticket/PR feed grouping, search and the badge row), the
    /// entry's TTL expired and an immediate `CHAT_LINK_STATUS` refetch
    /// kicked (which [`merge_refetched_link`] keeps from blanking the
    /// placeholder). Closes the editor if it was this chat's. On failure:
    /// inline error in the chat's detail.
    fn set_chat_link(
        &mut self,
        chat_id: String,
        kind: LinkKind,
        value: Option<String>,
        operation: LinkOperation,
        cx: &mut Context<Self>,
    ) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.link_submitting = false;
            self.link_error = Some((chat_id, "Engine not connected".to_string()));
            cx.notify();
            return;
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::SET_CHAT_LINK,
                    serde_json::json!({
                        "chatId": chat_id,
                        "kind": kind.wire(),
                        "value": value,
                        "operation": operation.wire(),
                    }),
                )
                .await;
            this.update(cx, |this, cx| {
                let editor_was_this_chat =
                    this.link_editor_chat.as_deref() == Some(chat_id.as_str());
                if editor_was_this_chat {
                    this.link_submitting = false;
                }
                let reply = result.map_err(|err| err.to_string()).and_then(|value| {
                    serde_json::from_value::<SetChatLinkReply>(value).map_err(|err| err.to_string())
                });
                match reply {
                    Ok(reply) => {
                        let _ = this.shell.update(cx, |shell, _| {
                            shell.invalidate_chat_header_links(&chat_id);
                        });
                        let expired = Instant::now()
                            .checked_sub(LINK_STATUS_REFRESH)
                            .unwrap_or_else(Instant::now);
                        let entry = this.link_status.entry(chat_id.clone()).or_insert_with(|| {
                            LinkStatusEntry {
                                status: OverviewLinkStatus::default(),
                                diff_stat: None,
                                is_worktree: None,
                                fetched_at: expired,
                            }
                        });
                        apply_link_reply(&mut entry.status, kind, &reply);
                        entry.fetched_at = expired;
                        this.row_fetch_due.remove(&chat_id);
                        this.rows_dirty = true;
                        if this
                            .link_error
                            .as_ref()
                            .is_some_and(|(id, _)| *id == chat_id)
                        {
                            this.link_error = None;
                        }
                        if editor_was_this_chat && operation == LinkOperation::Add {
                            if kind == LinkKind::Pr {
                                this.link_input
                                    .update(cx, |input, cx| input.set_text("", cx));
                            } else {
                                this.link_editor_chat = None;
                            }
                        }
                        this.ensure_link_status(chat_id, cx);
                    }
                    Err(err) => {
                        tracing::warn!(chat = %chat_id, error = %err, "SetChatLink failed");
                        this.link_error = Some((chat_id, format!("Couldn't save link: {err}")));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn ensure_subagents(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let ttl = if self
            .subagents
            .get(&chat_id)
            .is_some_and(|subs| subs.iter().any(|s| s.status == SubagentStatus::Running))
        {
            SUBAGENTS_RUNNING_REFRESH
        } else {
            SUBAGENTS_REFRESH
        };
        let fresh_enough = self
            .subagents_fetched_at
            .get(&chat_id)
            .is_some_and(|at| at.elapsed() < ttl);
        if fresh_enough || self.subagent_pending.contains(&chat_id) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.subagent_pending.insert(chat_id.clone());
        self.perf.fetch_requests += 1;
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::SCAN_CHAT_SUBAGENTS,
                    serde_json::json!({ "chatId": chat_id }),
                )
                .await;
            this.update(cx, |this, cx| {
                this.subagent_pending.remove(&chat_id);
                this.subagents_fetched_at
                    .insert(chat_id.clone(), Instant::now());
                if let Ok(value) = result
                    && let Ok(subagents) = serde_json::from_value::<Vec<SubagentItem>>(value)
                {
                    // `subagent_count` is baked onto `OverviewRow`, so only a
                    // COUNT change rebuilds rows; a status flip (running →
                    // done) or a renamed description just repaints, matching
                    // how `ensure_context_usage` only dirties rows on a
                    // visible change.
                    let previous_count = this.subagents.get(&chat_id).map_or(0, Vec::len);
                    let next_count = subagents.len();
                    this.subagents.insert(chat_id, subagents);
                    if previous_count != next_count {
                        this.rows_dirty = true;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Fetch/refresh one chat's context-window fraction on
    /// `CONTEXT_USAGE_REFRESH`. The attempt time is recorded on success and
    /// failure alike, so an engine without the
    /// method just retries on the TTL. A landed value that rounds to the same
    /// whole percent is a no-op: nothing visible changes, so rows stay
    /// cached (see [`context_pct_changed`]). A real change dirties them.
    fn ensure_context_usage(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let fresh_enough = self
            .context_usage_fetched_at
            .get(&chat_id)
            .is_some_and(|at| at.elapsed() < CONTEXT_USAGE_REFRESH);
        if fresh_enough || self.context_usage_pending.contains(&chat_id) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.context_usage_pending.insert(chat_id.clone());
        self.perf.fetch_requests += 1;
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::CHAT_CONTEXT_USAGE,
                    serde_json::json!({ "chatId": chat_id }),
                )
                .await;
            this.update(cx, |this, cx| {
                this.context_usage_pending.remove(&chat_id);
                this.context_usage_fetched_at
                    .insert(chat_id.clone(), Instant::now());
                if let Ok(value) = result {
                    let pct = value
                        .get("contextPct")
                        .and_then(|v| v.as_f64())
                        .map(|f| f as f32);
                    let previous = this.context_usage.insert(chat_id, pct);
                    if context_pct_changed(previous, pct) {
                        this.rows_dirty = true;
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Compact badge row for one chat: PR action (if any, highest-severity
    /// reason only), ticket identifier (if any). `None` when there's nothing
    /// to show yet (no data fetched, or fetched and genuinely nothing
    /// linked) — callers skip rendering the row. Subagents are not a badge
    /// here — see `subagent_tiles_for` for the nested compact rows.
    fn badges_for(&self, chat_id: &str, theme: &Theme) -> Option<gpui::AnyElement> {
        let link = self.link_status.get(chat_id).map(|e| &e.status);
        let ticket_label = link
            .and_then(|s| s.ticket.as_ref())
            .map(|t| t.identifier.clone());
        if !link.is_some_and(link_has_badge_row) {
            return None;
        }
        let ticket_source = link.and_then(|s| s.ticket_source);
        // Keep the complete badge set in one horizontal line and clip at the
        // row edge instead of wrapping. The aggregate tooltip exposes the
        // full clipped sequence; each reachable badge retains its provenance
        // tooltip too.
        let mut summary = Vec::new();
        let mut row = div()
            .debug_selector(|| "overview-row-badges".into())
            .flex_none()
            .w_full()
            .h(px(BADGE_ROW_H))
            .flex()
            .items_center()
            .gap(px(6.0))
            .overflow_hidden()
            .whitespace_nowrap();
        for (index, pr_link) in link.into_iter().flat_map(effective_pr_links).enumerate() {
            let pr = pr_link
                .detail
                .as_ref()
                .cloned()
                .unwrap_or_else(|| placeholder_pr(&pr_link.url));
            let (action, color) = pr
                .action_reasons()
                .into_iter()
                .next()
                .map(pr_action_badge)
                .map(|(label, color)| (label.to_string(), color))
                .unwrap_or_else(|| (pr.state.clone(), theme.text_muted));
            let label = format!("#{} {action}", pr.number);
            summary.push(label.clone());
            let pr_source = pr_link.source;
            let debug_id = format!("overview-badge-pr-{chat_id}-{index}");
            let badge_id = SharedString::from(debug_id.clone());
            row = row.child(
                link_source_hover(
                    div()
                        .id(badge_id)
                        .debug_selector(move || debug_id.clone())
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(3.0))
                        .px(px(6.0))
                        .py(px(1.0))
                        .rounded(px(3.0))
                        .bg(color.opacity(0.15))
                        .text_size(crate::typography::ui_rems(9.5))
                        .text_color(color),
                    LinkKind::Pr,
                    pr_source,
                )
                .when(pr_source == Some(ChatLinkSource::Manual), |el| {
                    el.child(manual_link_pin(color))
                })
                .child(SharedString::from(label)),
            );
        }
        if let Some(identifier) = ticket_label {
            summary.push(identifier.clone());
            let debug_id = format!("overview-badge-ticket-{chat_id}");
            let badge_id = SharedString::from(debug_id.clone());
            row = row.child(
                link_source_hover(
                    div()
                        .id(badge_id)
                        .debug_selector(move || debug_id.clone())
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(3.0))
                        .px(px(6.0))
                        .py(px(1.0))
                        .rounded(px(3.0))
                        .bg(theme.element_hover)
                        .text_size(crate::typography::ui_rems(9.5))
                        .text_color(theme.text_muted),
                    LinkKind::Ticket,
                    ticket_source,
                )
                .when(ticket_source == Some(ChatLinkSource::Manual), |el| {
                    el.child(manual_link_pin(theme.text_muted))
                })
                .child(SharedString::from(identifier)),
            );
        }
        let tooltip: SharedString = summary.join(" · ").into();
        Some(
            row.id(SharedString::from(format!("overview-badges-{chat_id}")))
                .tooltip(move |_, cx| cx.new(|_| LinkSourceTooltip(tooltip.clone())).into())
                .into_any_element(),
        )
    }

    /// Compact nested rows for a chat's subagents — `session_canvas.html`'s
    /// `.tile.compact` sub-agent tiles (`~441-517`, `renderTile`'s compact
    /// branch `~1150-1160`, the stack itself `~1864-1906`). At rest this
    /// returns three agents plus a "+N more" row; `show_all` returns the full
    /// stack while the parent row is hovered. Each agent is a single
    /// fixed-height row — status dot (running = accent, done = green),
    /// truncated name, muted agent type, and a "⋯" affordance — with the full
    /// name in a hover tooltip (gpui's deferred tooltip layer: floats above
    /// everything, never reflows or gets clipped by the parent row's
    /// `overflow_hidden`). Spans the parent's width; never sized by its own
    /// context (the web's rule).
    ///
    /// Clicking one opens its read-only transcript peek
    /// (`open_subagent_peek`). The press stops propagation at mouse-DOWN —
    /// like the web's `subTile.addEventListener("pointerdown", e =>
    /// e.stopPropagation())` — so the parent row never toggles its expand,
    /// and the click itself stops again for good measure.
    ///
    /// Empty when the chat has no subagents (or they haven't been fetched
    /// yet) — callers skip rendering.
    fn subagent_tiles_for(
        &self,
        chat_id: &str,
        theme: &Theme,
        show_all: bool,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let Some(subagents) = self.subagents.get(chat_id) else {
            return Vec::new();
        };
        let visible_count = if show_all {
            subagents.len()
        } else {
            subagents.len().min(SUBAGENT_PREVIEW_LIMIT)
        };
        let mut tiles: Vec<gpui::AnyElement> = subagents
            .iter()
            .take(visible_count)
            .map(|sub| {
                let color = sub.status.color();
                let name = sub.display_name();
                let peeking = self
                    .subagent_peek
                    .as_ref()
                    .is_some_and(|p| p.chat_id == chat_id && p.agent_id == sub.agent_id);
                let tooltip: SharedString = match sub.agent_type.as_deref() {
                    Some(t) if !t.is_empty() => format!("{name} · {t} · {}", sub.status.label()),
                    _ => format!("{name} · {}", sub.status.label()),
                }
                .into();
                let click_chat = chat_id.to_string();
                let click_sub = sub.clone();
                div()
                    .id(SharedString::from(format!(
                        "overview-subagent-{chat_id}-{}",
                        sub.agent_id
                    )))
                    .debug_selector(|| "overview-subagent-tile".into())
                    .flex_none()
                    .w_full()
                    .h(px(SUBAGENT_TILE_H))
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .px(px(7.0))
                    .rounded(px(8.0))
                    .border_1()
                    // Same status tint as the parent card (`renderTile`'s
                    // 14% background / 30% border color-mix).
                    .border_color(theme.border.blend(color.opacity(0.30)))
                    .bg(theme.surface_raised.blend(color.opacity(0.14)))
                    .cursor_pointer()
                    .hover(|el| el.border_color(color.opacity(0.6)))
                    .when(peeking, |el| el.border_color(theme.accent))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_subagent_peek(click_chat.clone(), &click_sub, window, cx);
                    }))
                    .tooltip(move |_, cx| cx.new(|_| LinkSourceTooltip(tooltip.clone())).into())
                    .child(div().flex_none().size(px(8.0)).rounded_full().bg(color))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(20.0))
                            .truncate()
                            .text_size(crate::typography::ui_rems(10.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(SharedString::from(name)),
                    )
                    .when_some(
                        sub.agent_type.clone().filter(|t| !t.is_empty()),
                        |el, agent_type| {
                            el.child(
                                div()
                                    .flex_shrink(1.0)
                                    .min_w_0()
                                    .max_w(px(120.0))
                                    .truncate()
                                    .text_size(crate::typography::ui_rems(9.0))
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(agent_type)),
                            )
                        },
                    )
                    .child(
                        div()
                            .flex_none()
                            .size(px(15.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.0))
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from("⋯")),
                    )
                    .into_any_element()
            })
            .collect();

        if !show_all && subagents.len() > SUBAGENT_PREVIEW_LIMIT {
            let remaining = subagents.len() - SUBAGENT_PREVIEW_LIMIT;
            tiles.push(
                div()
                    .debug_selector(|| "overview-subagent-more".into())
                    .flex_none()
                    .w_full()
                    .h(px(SUBAGENT_TILE_H))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(theme.border.opacity(0.65))
                    .bg(theme.surface_raised.opacity(0.6))
                    .text_size(crate::typography::ui_rems(9.5))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text_muted)
                    .child(SharedString::from(format!(
                        "+{remaining} more · hover to expand"
                    )))
                    .into_any_element(),
            );
        }
        tiles
    }

    /// Small archive affordance on the list row. Always visible rather than
    /// hover-gated: simpler, and a stray click only archives (reversible via
    /// the sidebar's own archived-chats view), not a destructive delete.
    fn archive_button(
        &self,
        chat_id: String,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .id(SharedString::from(format!("overview-archive-{chat_id}")))
            .flex_none()
            .size(px(16.0))
            .rounded(px(3.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text_muted.opacity(0.5))
            .hover(|el| el.bg(theme.element_hover).text_color(theme.text_muted))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_archived(chat_id.clone(), true, cx);
            }))
            .child(SharedString::from("×"))
            .into_any_element()
    }

    /// Explicit "View full chat" affordance — opens the interactive chat
    /// panel (`session_canvas.html`'s separate "View chat" button,
    /// `~1137-1143`: its own `pointerdown` stop-propagation keeps it from
    /// also triggering the row's click-to-expand gesture). Only the panel's
    /// "Open full chat →" actually leaves the overview. Row body clicks
    /// toggle expand in place instead — see `toggle_expanded`.
    #[cfg(test)]
    pub(crate) fn expanded_is_empty(&self) -> bool {
        self.expanded.is_empty()
    }

    /// The Work Outline drift mark for a drifted Chat; clicking opens the
    /// Shell's Compare dialog (where Apply lives).
    fn instruction_drift_mark(
        &self,
        id: String,
        chat_id: &str,
        with_label: bool,
        theme: &Theme,
    ) -> gpui::AnyElement {
        let shell = self.shell.clone();
        let chat_id = chat_id.to_owned();
        crate::work_outline::chips::instruction_drift_mark(
            id,
            with_label,
            theme,
            move |_, _, cx| {
                shell
                    .update(cx, |shell, cx| shell.open_instruction_compare(&chat_id, cx))
                    .ok();
            },
        )
    }

    fn open_chat_button(
        &self,
        chat_id: String,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .id(SharedString::from(format!("overview-open-{chat_id}")))
            .flex_none()
            .px(px(8.0))
            .py(px(2.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(10.0))
            .text_color(theme.accent)
            .bg(theme.accent.opacity(0.1))
            .hover(|el| el.bg(theme.accent.opacity(0.2)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_chat_panel(chat_id.clone(), cx);
            }))
            .child(SharedString::from("View full chat →"))
            .into_any_element()
    }

    /// The interactive chat panel — `Some` only while `chat_panel_open` is
    /// set and a chat is selected. Header ("Open full chat →" / ×), the
    /// Shell's live transcript, and the Shell's composer docked BELOW it in
    /// a flex column (not overlaid, unlike the chat route — so the
    /// transcript's bottom clearance is zero here; `Shell::render` sets that
    /// per frame on this route).
    fn render_chat_panel(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let Some(chat_id) = self.panel_chat_id(cx) else {
            // Selection cleared under the open panel (chat deleted/archived,
            // new-session): nothing to show, so the panel closes itself.
            self.chat_panel_open = false;
            return None;
        };
        self.ensure_link_status(chat_id.clone(), cx);
        let (chat, space) = {
            let state = self.state.read(cx);
            let chat = state.chats.iter().find(|chat| chat.id == chat_id)?.clone();
            let space = chat
                .space_id
                .as_deref()
                .and_then(|id| state.space_row(id))
                .cloned();
            (chat, space)
        };
        let title: SharedString = chat
            .title
            .clone()
            .unwrap_or_else(|| "Chat".to_string())
            .into();
        let links = self
            .link_status
            .get(&chat_id)
            .map(|entry| header_links(&entry.status));
        let mut metadata =
            crate::chat_metadata::metadata_items(&chat, space.as_ref(), links.as_ref());
        crate::chat_metadata::insert_account_item(
            &mut metadata,
            self.state.read(cx).chat_account_badge(&chat_id),
        );
        if let Some(path) = self
            .work_item_index
            .item_for_chat(&chat_id)
            .and_then(|item| self.work_item_index.path_label(item))
        {
            metadata.push(crate::chat_metadata::work_item_item(
                &path.short,
                &path.full,
            ));
        }
        let drift_mark = self
            .work_item_index
            .instruction_drift_for_chat(&chat_id)
            .then(|| {
                self.instruction_drift_mark(
                    "overview-chat-panel-instruction-drift".into(),
                    &chat_id,
                    true,
                    theme,
                )
            });
        // The chat route drives these per frame (`render_main`); it doesn't
        // run on this route, so the panel does. Without the settled-docked
        // frame, arriving from the blank new-session screen would leave the
        // composer in its big centered hero layout.
        self.composer.update(cx, |composer, cx| {
            composer.set_dock_frame(crate::composer_dock::DockFrame::settled(true), cx);
            composer.set_available_width(CHAT_PANEL_W - 2.0 * CHAT_PANEL_COMPOSER_PAD, cx);
        });
        let jump_pill = self.transcript.read(cx).jump_button_shown().then(|| {
            let transcript = self.transcript.clone();
            div()
                .absolute()
                .bottom(px(10.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("overview-chat-panel-jump")
                        .h(px(26.0))
                        .px(px(11.0))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .rounded_full()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.surface_raised)
                        .shadow_md()
                        .cursor_pointer()
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text)
                        .hover(|el| el.bg(theme.element_hover))
                        .on_click(move |_, _, cx| {
                            transcript.update(cx, |transcript, cx| transcript.jump_to_bottom(cx));
                        })
                        .child(SharedString::from("↓ Scroll to bottom")),
                )
        });
        let full_chat_id = chat_id.clone();
        let copy_chat_id = chat_id.clone();
        let copied = self.copied_panel_identity.as_deref() == Some(chat_id.as_str());
        Some(
            div()
                .id("overview-chat-panel")
                .debug_selector(|| "overview-chat-panel".into())
                .flex_none()
                .w(px(CHAT_PANEL_W))
                .h_full()
                .flex()
                .flex_col()
                .border_l_1()
                .border_color(theme.border)
                .bg(theme.surface_raised)
                // Same attachment path as the chat route's dropzone.
                .on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, _, cx| {
                    let paths = paths.paths().to_vec();
                    this.composer
                        .update(cx, |composer, cx| composer.add_paths(paths, cx));
                    cx.notify();
                }))
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .px(px(12.0))
                        .py(px(8.0))
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(crate::typography::ui_rems(12.0))
                                        .text_color(theme.text)
                                        .child(title),
                                )
                                .children(drift_mark)
                                .child(
                                    div()
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .gap(px(10.0))
                                        .child(
                                            div()
                                                .id("overview-chat-panel-copy")
                                                .flex()
                                                .items_center()
                                                .gap(px(4.0))
                                                .px(px(6.0))
                                                .py(px(2.0))
                                                .rounded(px(4.0))
                                                .cursor_pointer()
                                                .text_size(crate::typography::ui_rems(10.5))
                                                .text_color(if copied {
                                                    theme.success
                                                } else {
                                                    theme.text_muted
                                                })
                                                .hover(|el| {
                                                    el.bg(theme.element_hover)
                                                        .text_color(theme.text)
                                                })
                                                .child(
                                                    crate::icons::icon(if copied {
                                                        crate::icons::CHECK
                                                    } else {
                                                        crate::icons::COPY
                                                    })
                                                    .size(px(11.0)),
                                                )
                                                .child(SharedString::from(if copied {
                                                    "Copied"
                                                } else {
                                                    "Copy name + IDs"
                                                }))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.copy_panel_chat_identity(
                                                        copy_chat_id.clone(),
                                                        cx,
                                                    );
                                                })),
                                        )
                                        .child(
                                            div()
                                                .id("overview-chat-panel-outline")
                                                .debug_selector(|| {
                                                    "overview-chat-panel-outline".into()
                                                })
                                                .cursor_pointer()
                                                .text_size(crate::typography::ui_rems(10.5))
                                                .text_color(theme.text_muted)
                                                .hover(|el| el.text_color(theme.text))
                                                .child(SharedString::from("Outline"))
                                                .hint(
                                                    "Show this Chat in the Outline",
                                                    HintKey::Command(
                                                        WorkOutlineCommand::ToggleOutlineFocus,
                                                    ),
                                                )
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    let Some(shell) = this.shell.upgrade() else {
                                                        return;
                                                    };
                                                    // The Shell updates this Overview.
                                                    window.defer(cx, move |window, cx| {
                                                        shell.update(cx, |shell, cx| {
                                                            shell.toggle_outline_focus(window, cx)
                                                        });
                                                    });
                                                })),
                                        )
                                        .child(
                                            div()
                                                .id("overview-chat-panel-open-full")
                                                .cursor_pointer()
                                                .text_size(crate::typography::ui_rems(10.5))
                                                .text_color(theme.accent)
                                                .child(SharedString::from("Open full chat →"))
                                                .hint(
                                                    "Open this Chat as the full chat",
                                                    HintKey::Command(
                                                        WorkOutlineCommand::OpenFullChat,
                                                    ),
                                                )
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    // `open` acknowledges the
                                                    // unread done; the key path
                                                    // is `prepare_full_chat`.
                                                    this.close_chat_panel(cx);
                                                    this.open(full_chat_id.clone(), cx);
                                                })),
                                        )
                                        .child(
                                            div()
                                                .id("overview-chat-panel-close")
                                                .cursor_pointer()
                                                .text_color(theme.text_muted)
                                                .child(SharedString::from("×"))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.close_chat_panel(cx);
                                                })),
                                        ),
                                ),
                        )
                        .child(crate::chat_metadata::metadata_strip(
                            "overview-chat-panel",
                            metadata,
                            theme,
                        )),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .relative()
                        .child(self.transcript.clone())
                        .children(jump_pill),
                )
                .child(
                    div()
                        .flex_none()
                        .w_full()
                        .px(px(CHAT_PANEL_COMPOSER_PAD))
                        .pb(px(8.0))
                        .child(self.composer.clone()),
                )
                .into_any_element(),
        )
    }

    /// The right-side panel's read-only subagent-transcript mode — `Some`
    /// only while `subagent_peek` is set. Web `#transcript-panel` opened for
    /// a sub-agent: header = the subagent's name + agent type (+ model once
    /// loaded) and a ✕; body = its turns (role label + time, text, tool
    /// chips), scrolled to the end on load. No composer: a subagent can't be
    /// continued. Same width/chrome as the live chat panel so switching
    /// between the two modes doesn't jump the layout.
    fn render_subagent_peek(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let peek = self.subagent_peek.as_ref()?;
        let code_family = crate::typography::code_effective_family_name(cx);
        let mut subtitle = String::from("sub-agent transcript");
        if let PeekLoad::Loaded(t) = &peek.load
            && let Some(model) = t.model.as_deref().filter(|m| !m.is_empty())
        {
            subtitle.push_str(" · ");
            subtitle.push_str(model);
        }
        let hint = |text: &'static str| {
            div()
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_muted.opacity(0.7))
                .child(SharedString::from(text))
                .into_any_element()
        };
        let body: Vec<gpui::AnyElement> = match &peek.load {
            PeekLoad::Loading => vec![hint("Loading…")],
            PeekLoad::Failed => vec![hint("Failed to load transcript.")],
            PeekLoad::Loaded(t) if t.turns.is_empty() => {
                vec![hint("No turns found in this transcript.")]
            }
            PeekLoad::Loaded(t) => t
                .turns
                .iter()
                .enumerate()
                .map(|(ti, turn)| {
                    self.render_peek_turn(ti, turn, &peek.open_tools, theme, &code_family, cx)
                })
                .collect(),
        };
        let name: SharedString = peek.name.clone().into();
        let agent_type = peek.agent_type.clone().filter(|t| !t.is_empty());
        Some(
            div()
                .id("overview-subagent-peek")
                .debug_selector(|| "overview-subagent-peek".into())
                .track_focus(&self.peek_focus)
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        this.close_subagent_peek(cx);
                        cx.stop_propagation();
                    }
                }))
                .flex_none()
                .w(px(CHAT_PANEL_W))
                .h_full()
                .flex()
                .flex_col()
                .border_l_1()
                .border_color(theme.border)
                .bg(theme.surface_raised)
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .px(px(12.0))
                        .py(px(10.0))
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.0))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .text_size(crate::typography::ui_rems(12.0))
                                                .text_color(theme.text)
                                                .child(name),
                                        )
                                        .when_some(agent_type, |el, agent_type| {
                                            el.child(
                                                div()
                                                    .flex_none()
                                                    .max_w(px(160.0))
                                                    .truncate()
                                                    .text_size(crate::typography::ui_rems(10.5))
                                                    .text_color(theme.text_muted)
                                                    .child(SharedString::from(agent_type)),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(crate::typography::ui_rems(10.0))
                                        .text_color(theme.text_muted.opacity(0.7))
                                        .child(SharedString::from(subtitle)),
                                ),
                        )
                        .child(
                            div()
                                .id("overview-subagent-peek-close")
                                .flex_none()
                                .cursor_pointer()
                                .text_color(theme.text_muted)
                                .hover(|el| el.text_color(theme.text))
                                .child(SharedString::from("✕"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.close_subagent_peek(cx);
                                })),
                        ),
                )
                .child(
                    div()
                        .id("overview-subagent-peek-body")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.peek_scroll)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(14.0))
                                .px(px(14.0))
                                .py(px(12.0))
                                .children(body),
                        ),
                )
                .into_any_element(),
        )
    }

    /// One transcript turn (web `renderTurns`' `.turn`): role label + time,
    /// the text (newlines preserved; ``` fences as monospace blocks — see
    /// [`split_fenced_code`]), then its tool chips. A chip with a result
    /// preview is a disclosure (web `<details class="tool-detail">`):
    /// clicking toggles the result open underneath.
    fn render_peek_turn(
        &self,
        ti: usize,
        turn: &TranscriptTurn,
        open_tools: &HashSet<(usize, usize)>,
        theme: &Theme,
        code_family: &SharedString,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let is_user = turn.role == "user";
        let time = format_turn_time(turn.timestamp.as_deref());
        let text_blocks: Vec<gpui::AnyElement> = turn
            .text
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .map(split_fenced_code)
            .unwrap_or_default()
            .into_iter()
            .map(|(is_code, chunk)| {
                if is_code {
                    div()
                        .w_full()
                        .px(px(8.0))
                        .py(px(6.0))
                        .rounded(px(5.0))
                        .bg(theme.element_hover.opacity(0.6))
                        .font_family(code_family.clone())
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text)
                        .child(SharedString::from(chunk))
                        .into_any_element()
                } else {
                    div()
                        .w_full()
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text)
                        .child(SharedString::from(chunk))
                        .into_any_element()
                }
            })
            .collect();
        let tools: Vec<gpui::AnyElement> = turn
            .tools
            .iter()
            .enumerate()
            .map(|(tj, tool)| {
                let open = open_tools.contains(&(ti, tj));
                let has_result = tool
                    .result_preview
                    .as_deref()
                    .is_some_and(|r| !r.is_empty());
                let summary = if tool.input_preview.is_empty() {
                    tool.name.clone()
                } else {
                    format!("{} {}", tool.name, tool.input_preview)
                };
                let chip = div()
                    .id(SharedString::from(format!("overview-peek-tool-{ti}-{tj}")))
                    .max_w_full()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .px(px(7.0))
                    .py(px(2.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.element_hover.opacity(0.4))
                    .font_family(code_family.clone())
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_muted)
                    .when(has_result, |el| {
                        el.cursor_pointer()
                            .hover(|el| el.text_color(theme.text))
                            .child(SharedString::from(if open { "▾" } else { "▸" }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(peek) = this.subagent_peek.as_mut()
                                    && !peek.open_tools.remove(&(ti, tj))
                                {
                                    peek.open_tools.insert((ti, tj));
                                }
                                cx.notify();
                            }))
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(summary)),
                    );
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(chip)
                    .when(open, |el| {
                        el.when_some(tool.result_preview.clone(), |el, result| {
                            el.child(
                                div()
                                    .ml(px(10.0))
                                    .px(px(8.0))
                                    .py(px(5.0))
                                    .rounded(px(4.0))
                                    .border_l_2()
                                    .border_color(theme.border)
                                    .font_family(code_family.clone())
                                    .text_size(crate::typography::ui_rems(10.5))
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(result)),
                            )
                        })
                    })
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .gap(px(5.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(crate::typography::ui_rems(10.5))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(if is_user {
                                theme.accent
                            } else {
                                theme.text_muted
                            })
                            .child(SharedString::from(turn_role_label(&turn.role))),
                    )
                    .when(!time.is_empty(), |el| {
                        el.child(
                            div()
                                .text_color(theme.text_muted.opacity(0.6))
                                .child(SharedString::from(time)),
                        )
                    }),
            )
            .children(text_blocks)
            .when(!tools.is_empty(), |el| {
                el.child(div().flex().flex_col().gap(px(4.0)).children(tools))
            })
            .into_any_element()
    }

    /// Inline expanded detail — toggled by a row click, never navigates. Ports `renderDetails`
    /// (`session_canvas.html:1092-1113`) field by field, from data actually
    /// available here:
    /// - branch + worktree badge (engine `isWorktree` when the link status
    ///   carries it, else the reference's own cwd rule, [`cwd_is_worktree`])
    /// - changes (diff stat) — only when the engine's link status carries a
    ///   `diffStat` (see `LinkStatusEntry::diff_stat`); absent today
    /// - pull request (title/state/draft,
    ///   review decision, checks, reviewer chips — click opens GitHub),
    ///   linear ticket (identifier/title/status — click opens Linear)
    /// - last message (Mast has no separate `summary` field)
    /// - "Open chat →" + Archive/Unarchive actions. The web's "Open in
    ///   Terminal/Cursor" has no Mast equivalent (the chat IS here) —
    ///   "Open full chat →" in the preview panel is the analog.
    fn expanded_detail_for(
        &self,
        row: &OverviewRow,
        theme: &Theme,
        chat_id: String,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let entry = self.link_status.get(&row.chat.id);
        let link = entry.map(|e| &e.status);
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        let label_el = |label: &'static str| {
            div()
                .flex_none()
                .w(px(78.0))
                .text_size(crate::typography::ui_rems(10.0))
                .text_color(theme.text_muted.opacity(0.6))
                .child(SharedString::from(label))
        };
        let detail_row = |label: &'static str, value: gpui::AnyElement| {
            div()
                .flex()
                .gap(px(6.0))
                .text_size(crate::typography::ui_rems(10.0))
                .child(label_el(label))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(theme.text_muted)
                        .child(value),
                )
                .into_any_element()
        };
        let text = |value: String| SharedString::from(value).into_any_element();
        let chip = |text: String, color: gpui::Hsla, bg: gpui::Hsla| {
            div()
                .px(px(6.0))
                .py(px(1.0))
                .rounded(px(3.0))
                .bg(bg)
                .text_size(crate::typography::ui_rems(9.5))
                .text_color(color)
                .child(SharedString::from(text))
        };
        // `renderChips`/`renderToolChips`: an empty list renders a single
        // "none" chip rather than hiding the row.
        let chip_row = |chips: Vec<String>| {
            let chips = if chips.is_empty() {
                vec!["none".to_string()]
            } else {
                chips
            };
            div()
                .flex()
                .flex_wrap()
                .gap(px(4.0))
                .children(
                    chips
                        .into_iter()
                        .map(|t| chip(t, theme.text_muted, theme.element_hover)),
                )
                .into_any_element()
        };

        // branch
        let is_worktree = entry
            .and_then(|e| e.is_worktree)
            .unwrap_or_else(|| cwd_is_worktree(row.chat.cwd.as_deref()));
        let branch_value = match &row.chat.branch {
            Some(branch) => div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(5.0))
                .child(SharedString::from(format!("⎇ {branch}")))
                .when(is_worktree, |el| {
                    el.child(chip(
                        "worktree".into(),
                        theme.text_muted,
                        theme.element_hover,
                    ))
                })
                .into_any_element(),
            None => text("—".into()),
        };
        rows.push(detail_row("branch", branch_value));

        // changes
        if let Some(diff) = entry.and_then(|e| e.diff_stat) {
            let files = if diff.files_changed == 1 {
                "file"
            } else {
                "files"
            };
            rows.push(detail_row(
                "changes",
                div()
                    .flex()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_color(gpui::rgb(0x0ca30c))
                            .child(SharedString::from(format!("+{}", diff.lines_added))),
                    )
                    .child(
                        div()
                            .text_color(gpui::rgb(0xd03b3b))
                            .child(SharedString::from(format!("-{}", diff.lines_removed))),
                    )
                    .child(SharedString::from(format!(
                        "· {} {files}",
                        diff.files_changed
                    )))
                    .into_any_element(),
            ));
        }

        if let Some(link) = link {
            let prs = effective_pr_links(link);
            if !prs.is_empty() {
                let values = prs.into_iter().enumerate().map(|(index, pr_link)| {
                    let pr = pr_link
                        .detail
                        .clone()
                        .unwrap_or_else(|| placeholder_pr(&pr_link.url));
                    let mut summary =
                        format!("#{} {}", pr.number, pr.title.clone().unwrap_or_default());
                    summary.push_str(" — ");
                    summary.push_str(if pr.is_draft { "draft" } else { &pr.state });
                    if let Some(decision) = &pr.review_decision {
                        summary.push_str(", ");
                        summary.push_str(&decision.to_lowercase().replace('_', " "));
                    }
                    let checks = pr.checks.map(checks_style);
                    if let Some((_, _, checks_label)) = checks {
                        summary.push_str(&format!(", checks {checks_label}"));
                    }
                    let url = pr_link.url.clone();
                    let open_url = url.clone();
                    let open_shell = self.shell.clone();
                    let pr_source = pr_link.source;
                    div()
                        .min_w_0()
                        .max_w_full()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.0))
                                .when(pr_source == Some(ChatLinkSource::Manual), |el| {
                                    el.child(manual_link_pin(theme.text_muted))
                                })
                                .child(
                                    link_source_hover(
                                        div()
                                            .id(SharedString::from(format!(
                                                "overview-detail-pr-{chat_id}-{index}"
                                            )))
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .gap(px(4.0)),
                                        LinkKind::Pr,
                                        pr_source,
                                    )
                                    .overflow_hidden()
                                    .cursor_pointer()
                                    .text_color(theme.accent)
                                    .hover(|el| el.underline())
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .max_w_full()
                                            .max_h(px(32.0))
                                            .line_height(px(16.0))
                                            .whitespace_normal()
                                            .overflow_hidden()
                                            .child(SharedString::from(summary)),
                                    )
                                    .when_some(checks, |el, (color, icon, _)| {
                                        el.child(
                                            div()
                                                .flex_none()
                                                .text_color(color)
                                                .child(SharedString::from(icon)),
                                        )
                                    })
                                    .on_click(
                                        crate::github::chip::open_pr(Some(open_shell), open_url),
                                    ),
                                )
                                .when(pr_source.is_some(), |el| {
                                    el.child(self.unlink_button(
                                        chat_id.clone(),
                                        LinkKind::Pr,
                                        Some(url.clone()),
                                        theme,
                                        cx,
                                    ))
                                }),
                        )
                        .when(!pr.reviewers.is_empty(), |el| {
                            el.child(chip_row(pr.reviewers.clone()))
                        })
                });
                rows.push(detail_row(
                    if link.pr_links.len() > 1 {
                        "pull requests"
                    } else {
                        "pull request"
                    },
                    div()
                        .min_w_0()
                        .max_w_full()
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .children(values)
                        .into_any_element(),
                ));
            }
        }

        if let Some(ticket) = link.and_then(|s| s.ticket.as_ref()) {
            let mut summary = ticket.identifier.clone();
            if let Some(title) = &ticket.title {
                summary.push_str(&format!(" — {title}"));
            }
            if let Some(status) = &ticket.status {
                summary.push_str(&format!(" ({status})"));
            }
            let url = ticket.url.clone();
            let ticket_shell = self.shell.clone();
            let ticket_source = link.and_then(|s| s.ticket_source);
            rows.push(detail_row(
                "linear ticket",
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .when(ticket_source == Some(ChatLinkSource::Manual), |el| {
                        el.child(manual_link_pin(theme.text_muted))
                    })
                    .child(
                        link_source_hover(
                            div()
                                .id(SharedString::from(format!(
                                    "overview-detail-ticket-{chat_id}"
                                )))
                                .flex_1()
                                .min_w_0(),
                            LinkKind::Ticket,
                            ticket_source,
                        )
                        .when(url.is_some(), |el| {
                            el.cursor_pointer()
                                .text_color(theme.accent)
                                .hover(|el| el.underline())
                        })
                        .child(SharedString::from(summary))
                        .when_some(url, |el, url| {
                            el.on_click(crate::github::chip::open_pr(Some(ticket_shell), url))
                        }),
                    )
                    .when(ticket_source.is_some(), |el| {
                        el.child(self.unlink_button(
                            chat_id.clone(),
                            LinkKind::Ticket,
                            None,
                            theme,
                            cx,
                        ))
                    })
                    .into_any_element(),
            ));
        }

        if let Some(preview) = &row.chat.last_message_preview {
            rows.push(detail_row("last message", text(preview.clone())));
        }

        let archived = row.chat.archived;
        let archive_id = chat_id.clone();
        let archive_toggle = div()
            .id(SharedString::from(format!(
                "overview-detail-archive-{chat_id}"
            )))
            .flex_none()
            .px(px(8.0))
            .py(px(2.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(10.0))
            .text_color(theme.text_muted)
            .border_1()
            .border_color(theme.border)
            .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_archived(archive_id.clone(), !archived, cx);
            }))
            .child(SharedString::from(if archived {
                "Unarchive"
            } else {
                "Archive"
            }));

        let closeout_id = chat_id.clone();
        let closeout = row.closeable.then(|| {
            div()
                .id(SharedString::from(format!(
                    "overview-detail-closeout-{chat_id}"
                )))
                .flex_none()
                .px(px(8.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(10.0))
                .text_color(theme.text_muted)
                .border_1()
                .border_color(theme.border)
                .hover(|el| el.bg(theme.element_hover).text_color(theme.danger))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_closeout(closeout_id.clone(), cx);
                }))
                .child(SharedString::from("Close out worktree…"))
        });

        let editor_open = self.link_editor_chat.as_deref() == Some(chat_id.as_str());
        let link_toggle_id = chat_id.clone();
        let link_toggle = div()
            .id(SharedString::from(format!(
                "overview-detail-link-{chat_id}"
            )))
            .flex_none()
            .px(px(8.0))
            .py(px(2.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(10.0))
            .text_color(theme.text_muted)
            .border_1()
            .border_color(theme.border)
            .when(editor_open, |el| {
                el.bg(theme.element_active).text_color(theme.text)
            })
            .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.link_editor_chat.as_deref() == Some(link_toggle_id.as_str()) {
                    this.close_link_editor(cx);
                } else {
                    this.open_link_editor(link_toggle_id.clone(), false, window, cx);
                }
            }))
            .child(SharedString::from(if editor_open {
                "Cancel"
            } else {
                "Add PR / ticket…"
            }));

        let link_error = self
            .link_error
            .as_ref()
            .filter(|(id, _)| *id == chat_id)
            .map(|(_, message)| message.clone());
        let editor = editor_open.then(|| {
            let hint = if self.link_submitting {
                "Linking…".to_string()
            } else {
                "Enter to add · Esc to cancel".to_string()
            };
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                            match event.keystroke.key.as_str() {
                                "enter" => this.submit_link_editor(cx),
                                "escape" => this.close_link_editor(cx),
                                _ => return,
                            }
                            cx.stop_propagation();
                            window.prevent_default();
                        }))
                        .child(popover::search_input_frame(
                            theme,
                            self.link_input.clone().into_any_element(),
                        )),
                )
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(9.5))
                        .text_color(theme.text_muted.opacity(0.6))
                        .child(SharedString::from(hint)),
                )
                .into_any_element()
        });
        let critical: gpui::Hsla = gpui::rgb(0xd03b3b).into();
        let error_line = link_error.map(|message| {
            div()
                .text_size(crate::typography::ui_rems(9.5))
                .text_color(critical)
                .child(SharedString::from(message))
                .into_any_element()
        });

        div()
            .w_full()
            .max_w_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .gap(px(4.0))
            .p(px(8.0))
            .mt(px(4.0))
            .rounded(px(6.0))
            // Was `theme.surface.opacity(0.6)` — see-through enough to hurt
            // contrast against whatever else is underneath. Solid background,
            // same as the row itself.
            .bg(theme.surface_raised)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .children(rows)
            .child(
                div()
                    .pt(px(4.0))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.0))
                    .child(self.open_chat_button(chat_id, theme, cx))
                    .child(archive_toggle)
                    .child(link_toggle)
                    .children(closeout),
            )
            .children(editor)
            .children(error_line)
            .into_any_element()
    }

    /// Small "✕" that clears a stored (any-source) PR/ticket link —
    /// `SET_CHAT_LINK` with `value: null`. Only rendered when the slot has a
    /// stored source; an inference-derived link has nothing to clear.
    fn unlink_button(
        &self,
        chat_id: String,
        kind: LinkKind,
        value: Option<String>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let tooltip = SharedString::from(match kind {
            LinkKind::Pr => "Unlink this PR",
            LinkKind::Ticket => "Unlink this ticket",
        });
        div()
            .id(SharedString::from(format!(
                "overview-unlink-{}-{chat_id}",
                kind.wire()
            )))
            .flex_none()
            .px(px(4.0))
            .rounded(px(3.0))
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(10.0))
            .text_color(theme.text_muted.opacity(0.5))
            .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                let operation = if kind == LinkKind::Pr {
                    LinkOperation::Remove
                } else {
                    LinkOperation::Clear
                };
                this.set_chat_link(chat_id.clone(), kind, value.clone(), operation, cx);
            }))
            .tooltip(move |_, cx| cx.new(|_| LinkSourceTooltip(tooltip.clone())).into())
            .child(SharedString::from("✕"))
            .into_any_element()
    }
}

impl Render for Overview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        self.ensure_prefs_loaded(cx);
        if std::mem::take(&mut self.outline_focus_pending)
            && self.view_mode == OverviewMode::Outline
        {
            let handle = gpui::Focusable::focus_handle(self.outline.read(cx), cx);
            window.focus(&handle, cx);
        }
        // The chat open in the side panel — or, in peek mode, the parent of
        // the subagent being peeked.
        self.panel_highlight = self
            .subagent_peek
            .as_ref()
            .map(|p| p.chat_id.clone())
            .or_else(|| self.panel_chat_id(cx));
        let rows = self.rows(cx);
        self.declare_outline_spaces(cx);
        if self.view_mode == OverviewMode::Outline {
            self.sync_outline(cx);
        }

        let toggle = |mode: OverviewMode, label: String, current: OverviewMode| {
            let active = mode == current;
            div()
                .id(SharedString::from(format!("overview-mode-{mode:?}")))
                .px(px(8.0))
                .py(px(3.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(11.0))
                .when(active, |el| {
                    el.bg(theme.element_active).text_color(theme.text)
                })
                .when(!active, |el| el.text_color(theme.text_muted))
                .child(SharedString::from(label))
        };

        let new_chat_button = self.render_new_chat_button(&theme, cx);

        let title_row = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(14.0))
                            .text_color(theme.text)
                            .child(SharedString::from("Overview")),
                    )
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(format!("{} chats", rows.len()))),
                    )
                    .child(
                        div()
                            .id("overview-search")
                            .w(px(260.0))
                            .on_key_down(cx.listener(
                                |this, event: &gpui::KeyDownEvent, window, cx| {
                                    this.search_key_down(event, window, cx)
                                },
                            ))
                            .hint("Search", HintKey::Find)
                            .child(popover::search_input_frame(
                                &theme,
                                self.search.clone().into_any_element(),
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    // Leftmost of the header's action cluster — the overview's
                    // one clearly-primary button (see `render_new_chat_button`).
                    .child(new_chat_button)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(2.0))
                            .p(px(2.0))
                            .rounded(px(6.0))
                            .bg(theme.element_hover.opacity(0.5))
                            .child(
                                toggle(OverviewMode::List, "List".into(), self.view_mode)
                                    .hint("List view", HintKey::None)
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.set_mode(OverviewMode::List, cx);
                                        }),
                                    ),
                            )
                            .child(
                                toggle(OverviewMode::Outline, "Outline".into(), self.view_mode)
                                    .hint(
                                        "Outline: work items and their Chats",
                                        HintKey::Command(WorkOutlineCommand::OpenOutline),
                                    )
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|this, _, window, cx| {
                                            this.set_mode(OverviewMode::Outline, cx);
                                            let handle = gpui::Focusable::focus_handle(
                                                this.outline.read(cx),
                                                cx,
                                            );
                                            window.focus(&handle, cx);
                                        }),
                                    ),
                            ),
                    ),
            );

        let pill = |id: &'static str, label: String, active: bool| {
            div()
                .id(id)
                .px(px(8.0))
                .py(px(3.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(11.0))
                .when(active, |el| {
                    el.bg(theme.element_active).text_color(theme.text)
                })
                .when(!active, |el| el.text_color(theme.text_muted))
                .hover(|el| el.text_color(theme.text))
                .child(SharedString::from(label))
        };

        // "Hide stale" / "Show archived" — the reference's own default-off
        // toggles (`hideStale`/`showArchived`).
        let filters = div()
            .flex()
            .items_center()
            .gap(px(2.0))
            .p(px(2.0))
            .rounded(px(6.0))
            .bg(theme.element_hover.opacity(0.5))
            .child(
                pill("overview-hide-stale", "Hide stale".into(), self.hide_stale)
                    .hint("Hide stale chats and completed work items", HintKey::None)
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.hide_stale = !this.hide_stale;
                            this.view_prefs_changed(cx);
                            this.rows_dirty = true;
                            cx.notify();
                        }),
                    ),
            )
            .child(
                pill(
                    "overview-show-archived",
                    "Show archived".into(),
                    self.show_archived,
                )
                .hint("Show archived chats and work items, dimmed", HintKey::None)
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.show_archived = !this.show_archived;
                        this.view_prefs_changed(cx);
                        this.rows_dirty = true;
                        cx.notify();
                    }),
                ),
            );

        let repo_filter = self.render_repo_filter(&theme, cx);
        let key_toggle = pill("overview-legend-toggle", "Key".into(), self.legend_open)
            .hint("Explain the colors and marks", HintKey::None)
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.legend_open = !this.legend_open;
                    cx.notify();
                }),
            );

        let outline_mode = self.view_mode == OverviewMode::Outline;
        let work_item_filter = (!outline_mode).then(|| self.render_work_item_filter(&theme, cx));
        let mut toolbar = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.0))
            .child(filters)
            .child(repo_filter)
            .when_some(work_item_filter, |el, filter| el.child(filter))
            .child(key_toggle);
        if outline_mode {
            toolbar = toolbar.children(self.render_outline_toolbar(&theme, cx));
        } else {
            // Composable dimension chips: click toggles membership, order
            // clicked = nesting order (`self.active_groups`). A chip shows
            // its position in that order (1-based) while active, so the
            // nesting is legible without opening a menu.
            let dims: [(GroupDimension, &'static str); 2] = [
                (GroupDimension::Repo, "Repo"),
                (GroupDimension::Ticket, "PR / ticket"),
            ];
            let chips = dims.into_iter().map(|(dim, label)| {
                let order = self.active_groups.iter().position(|&d| d == dim);
                let active = order.is_some();
                let text = match order {
                    Some(ix) => format!("{label} {}", ix + 1),
                    None => label.to_string(),
                };
                div()
                    .id(SharedString::from(format!("overview-group-{label}")))
                    .px(px(7.0))
                    .py(px(2.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(10.5))
                    .when(active, |el| {
                        el.bg(theme.element_active).text_color(theme.text)
                    })
                    .when(!active, |el| el.text_color(theme.text_muted))
                    .child(SharedString::from(text))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.toggle_group(dim, cx);
                            cx.notify();
                        }),
                    )
            });
            toolbar = toolbar.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .p(px(2.0))
                    .rounded(px(6.0))
                    .bg(theme.element_hover.opacity(0.3))
                    .child(
                        div()
                            .px(px(5.0))
                            .text_size(crate::typography::ui_rems(10.5))
                            .text_color(theme.text_muted.opacity(0.7))
                            .child(SharedString::from("Group by")),
                    )
                    .children(chips),
            );
        }

        let header = div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .px(px(16.0))
            .py(px(12.0))
            .child(title_row)
            .child(toolbar);

        let legend = self.legend_open.then(|| self.render_legend(&theme));

        let body = match self.view_mode {
            OverviewMode::List => {
                let body = self.render_list_body(&rows, &theme, cx);
                self.surface_focus_wrapper(body, cx)
            }
            OverviewMode::Outline => self.render_outline_body(cx),
        };
        // A focus the body couldn't use (target filtered out / not a row)
        // is dropped, never replayed on a later frame.
        self.focus_due = None;

        let main = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(header)
            .when_some(legend, |el, legend| el.child(legend))
            .child(body);

        // One right-side panel. Work Item Details, while open, renders in the
        // slot without touching the chat panel / peek state underneath, so
        // closing it brings back exactly what was there.
        // The Linear panel is NOT in this slot: it has its own left column
        // (below), so it shows alongside whichever of these is up.
        let (panel, right_w) = if let Some(details) = self.work_item_details.clone() {
            let space_id = details.read(cx).space_id.clone();
            let snapshot = self.outline_snapshot(&space_id, cx);
            let chats = self.outline_chats.clone();
            details.update(cx, |details, _| details.set_data(snapshot, chats));
            let width = self.shown_details_width();
            let panel = self.details_slot(details.into_any_element(), width, &theme, cx);
            (Some(panel), width)
        } else if let Some(list) = self.required_actions_panel.clone() {
            let spaces = self.required_action_spaces(cx);
            let chats = self.outline_chats.clone();
            list.update(cx, |list, _| list.set_data(spaces, chats));
            let width = self.shown_details_width();
            let panel = self.details_slot(list.into_any_element(), width, &theme, cx);
            (Some(panel), width)
        } else {
            let panel = if self.subagent_peek.is_some() {
                self.render_subagent_peek(&theme, cx)
            } else {
                self.render_chat_panel(&theme, cx)
            };
            let width = if panel.is_some() { CHAT_PANEL_W } else { 0.0 };
            (panel, width)
        };
        let linear = self.render_linear_column(right_w, window, cx);
        let (linear_inline, linear_overlay) = match linear {
            Some(linear_host::LinearSlot::Inline(el)) => (Some(el), None),
            Some(linear_host::LinearSlot::Overlay(el)) => (None, Some(el)),
            None => (None, None),
        };

        // `[Linear | main | right panel]`: the side panels are
        // `flex_none` widths and `main` is `flex_1().min_w_0()`, so the
        // list/Outline is what shrinks. The Linear column yields
        // first (down to its minimum, then it overlays `main`) so the body
        // keeps `linear_host::OVERVIEW_BODY_MIN_W`.
        div()
            .size_full()
            .relative()
            .flex()
            .child(self.linear_width_capture())
            .on_drag_move(cx.listener(
                |this, event: &gpui::DragMoveEvent<details_width::DetailsPanelResize>, _, cx| {
                    this.details_resize_drag(f32::from(event.event.position.x), cx)
                },
            ))
            .on_drag_move(cx.listener(
                |this, event: &gpui::DragMoveEvent<linear_host::LinearPanelResize>, _, cx| {
                    this.linear_resize_drag(f32::from(event.event.position.x), cx)
                },
            ))
            // Escape closes a subagent peek from anywhere in the overview
            // (the peek panel's own handler covers focus inside it; this
            // covers focus elsewhere here, e.g. the search box).
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key != "escape" {
                    return;
                }
                if let Some(details) = this.work_item_details.clone() {
                    details.update(cx, |details, cx| details.request_close(cx));
                    cx.stop_propagation();
                } else if this.required_actions_panel.is_some() {
                    this.close_required_actions(cx);
                    cx.stop_propagation();
                } else if this.subagent_peek.is_some() {
                    this.close_subagent_peek(cx);
                    cx.stop_propagation();
                }
            }))
            .when_some(linear_inline, |el, linear| el.child(linear))
            .child(main)
            .when_some(panel, |el, panel| el.child(panel))
            .when_some(linear_overlay, |el, linear| {
                el.child(div().absolute().top_0().bottom_0().left_0().child(linear))
            })
            .children(self.render_project_issue_picker(cx))
    }
}

impl Overview {
    /// Wrap the List body in its keyboard focus target (Esc / Enter /
    /// Down from the search land on it; List arrows and Enter run here).
    fn surface_focus_wrapper(
        &self,
        body: gpui::AnyElement,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .id("overview-surface")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .track_focus(&self.body_focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                this.list_key_down(event, window, cx)
            }))
            .child(body)
            .into_any_element()
    }

    /// The overview's one clearly-primary action: reuses the exact same
    /// new-session flow as the titlebar `+` / `mod-n` (`Shell::open_new_session`,
    /// via the `open_new_session_from_overview` wrapper — `open_new_session`
    /// itself is private to `crate::shell`). Clicking it leaves the overview
    /// for the blank new-chat composer screen; that's intentional, since the
    /// project/device pickers and the worktree chip live there, not here.
    ///
    /// As the overview becomes home base, THIS is the intended anchor point
    /// for future overview-level new-chat UX (e.g. surfacing the new-chat
    /// screen's worktree chip or pickers without leaving the overview) —
    /// per the user's stated intent. A later pass may grow this into
    /// something richer in place; it should not need a second "new chat"
    /// entrypoint added elsewhere.
    fn render_new_chat_button(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let shortcut = self
            .shell
            .upgrade()
            .map(|shell| shell.read(cx).new_session_shortcut_label());
        let tooltip: SharedString = match shortcut {
            Some(combo) => format!("Start a new chat ({combo})").into(),
            None => "Start a new chat".into(),
        };
        popover::btn_primary(theme, "+ New chat")
            .id("overview-new-chat")
            .on_click(cx.listener(|this, _, _, cx| {
                let _ = this.shell.update(cx, |shell, cx| {
                    shell.open_new_session_from_overview(cx);
                });
            }))
            .tooltip(move |_, cx| cx.new(|_| LinkSourceTooltip(tooltip.clone())).into())
            .into_any_element()
    }

    /// "Repos" toolbar control + its dropdown (web `#repo-filter-btn`/
    /// `#repo-filter-panel`, `session_canvas.html:2082-2148`): every repo
    /// seen across rows (pre-filter, so unchecking one never hides its own
    /// checkbox), a checkbox + hash-color swatch per repo, All/None bulk
    /// actions. Closes on any press outside the card (the web's
    /// document-level click listener).
    fn render_repo_filter(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let hidden = self.hidden_repos.len();
        let active = hidden > 0 || self.repo_filter.is_open();
        let trigger = div()
            .id("overview-repo-filter")
            .relative()
            .px(px(8.0))
            .py(px(3.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(11.0))
            .bg(theme.element_hover.opacity(0.5))
            .when(active, |el| {
                el.bg(theme.element_active).text_color(theme.text)
            })
            .when(!active, |el| el.text_color(theme.text_muted))
            .hover(|el| el.text_color(theme.text))
            .child(SharedString::from(format!(
                "{} ▾",
                repo_filter_label(hidden)
            )))
            .hint("Filter by repository", HintKey::None)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.repo_filter.note_trigger_press()),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if this.repo_filter.take_press_was_open() {
                    this.close_repo_filter(cx);
                } else {
                    this.repo_filter.open(());
                    cx.notify();
                }
            }));
        if self.repo_filter.get().is_none() {
            return trigger.into_any_element();
        }
        let closing = self.repo_filter.closing_since();

        let popup = theme.for_popup();
        let bulk = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .px(px(8.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(10.5))
                .text_color(popup.text_muted)
                .border_1()
                .border_color(popup.border)
                .hover(|el| el.bg(popup.element_hover).text_color(popup.text))
                .child(SharedString::from(label))
        };
        let keys = sorted_repo_keys(&self.known_repos);
        let list: Vec<gpui::AnyElement> = if keys.is_empty() {
            vec![
                div()
                    .px(px(6.0))
                    .py(px(4.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(popup.text_muted)
                    .child(SharedString::from("No chats yet"))
                    .into_any_element(),
            ]
        } else {
            keys.into_iter()
                .enumerate()
                .map(|(ix, key)| {
                    let checked = !self.hidden_repos.contains(&key);
                    let color = group_color(GroupDimension::Repo, &key);
                    let label = group_label(GroupDimension::Repo, &key);
                    div()
                        .id(("overview-repo-filter-row", ix))
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .px(px(6.0))
                        .py(px(3.0))
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .hover(|el| el.bg(popup.element_hover))
                        .child(
                            div()
                                .flex_none()
                                .size(px(12.0))
                                .rounded(px(3.0))
                                .border_1()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(crate::typography::ui_rems(9.0))
                                .when(checked, |el| {
                                    el.bg(popup.accent)
                                        .border_color(popup.accent)
                                        .text_color(gpui::white())
                                        .child("✓")
                                })
                                .when(!checked, |el| el.border_color(popup.border_strong)),
                        )
                        .child(div().flex_none().size(px(8.0)).rounded(px(2.0)).bg(color))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(if checked {
                                    popup.text
                                } else {
                                    popup.text_muted
                                })
                                .child(SharedString::from(label)),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            toggle_repo_hidden(&mut this.hidden_repos, &key);
                            this.repo_filter_changed(cx);
                        }))
                        .into_any_element()
                })
                .collect()
        };
        let card = popover::popover_card(&popup)
            .w(px(240.0))
            .max_h(px(360.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_repo_filter(cx)))
            .child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .pb(px(4.0))
                    .child(
                        bulk("overview-repo-filter-all", "All").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.hidden_repos.clear();
                                this.repo_filter_changed(cx);
                            },
                        )),
                    )
                    .child(
                        bulk("overview-repo-filter-none", "None").on_click(cx.listener(
                            |this, _, _, cx| {
                                let known = this.known_repos.clone();
                                this.hidden_repos.extend(known);
                                this.repo_filter_changed(cx);
                            },
                        )),
                    ),
            )
            .child(
                div()
                    .id("overview-repo-filter-list")
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .overflow_y_scroll()
                    .children(list),
            );
        trigger
            .child(popover::anchored_menu_below(
                "overview-repo-filter-menu",
                card.into_any_element(),
                closing,
            ))
            .into_any_element()
    }

    fn close_repo_filter(&mut self, cx: &mut Context<Self>) {
        if self.repo_filter.begin_close() {
            popover::reap_popup(cx, |this: &mut Self| &mut this.repo_filter);
        }
        cx.notify();
    }

    /// The "Key" legend (web `renderLegend`, `session_canvas.html:997-1038`)
    /// — inline panel under the toolbar, adapted to what this overview
    /// actually shows (no context-% sizing, no Cursor sessions, no Open in
    /// Terminal). The web animates it open via `max-height`; this just
    /// appears (no layout-height tween primitive worth the cost here).
    fn render_legend(&self, theme: &Theme) -> gpui::AnyElement {
        let swatch_row = |color: gpui::Hsla, text: String| {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(crate::typography::ui_rems(10.5))
                .text_color(theme.text_muted)
                .child(div().flex_none().size(px(8.0)).rounded_full().bg(color))
                .child(SharedString::from(text))
                .into_any_element()
        };
        let group = |title: &'static str, rows: Vec<gpui::AnyElement>| {
            div()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .min_w(px(130.0))
                .child(
                    div()
                        .pb(px(2.0))
                        .text_size(crate::typography::ui_rems(10.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(SharedString::from(title.to_uppercase())),
                )
                .children(rows)
        };
        let statuses = [
            ChatIndicator::AwaitingInput,
            ChatIndicator::Working,
            ChatIndicator::Completed,
            ChatIndicator::Errored,
            ChatIndicator::Idle,
        ];
        let mut status_rows: Vec<gpui::AnyElement> = statuses
            .iter()
            .map(|&status| {
                let (icon, label) = status_style(status);
                swatch_row(reference_status_color(status), format!("{icon} {label}"))
            })
            .collect();
        status_rows.push(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(crate::typography::ui_rems(10.5))
                .text_color(theme.text_muted)
                .child(
                    div()
                        .flex_none()
                        .relative()
                        .w(px(14.0))
                        .h(px(8.0))
                        .rounded(px(2.0))
                        .overflow_hidden()
                        .bg(theme.surface_raised)
                        .child(unread_done_ribbon(0.5)),
                )
                .child(SharedString::from(
                    "white left-edge ribbon: done, not looked at yet",
                ))
                .into_any_element(),
        );
        let notes = [
            "White left-edge ribbon = the chat finished and you haven't looked yet. Expanding the row or opening its chat clears it; the next completion re-adds the ribbon.",
            "Dimmed row = stale: no activity for 7 days and not working / waiting on you. \"Hide stale\" filters them out.",
            "Badge row = linked Linear ticket and/or GitHub PR; a PR badge shows its most urgent action (CI failing, merge conflict, changes requested, needs reviewer, ready to merge).",
            "Links come from the branch (inferred), PRs created or mentioned in the chat, or you: \"Add PR / ticket…\" in the expanded detail (or the sidebar chat menu) takes a GitHub PR URL or a ticket id like ENG-1234. Add as many PRs as the chat needs. Hover a PR/ticket for its source; a pin marks manual links, and ✕ in the detail unlinks one stored link.",
            "\"Group by\" toggles combine — the first becomes the top-level headers, each further one nests beneath it. Turn all off for the flat list.",
            "Click a row = expand for branch/worktree, PR (reviewers, checks, review decision), ticket, last message, archive.",
            "\"View full chat →\" = live chat side panel: read and reply without leaving the overview (the chat becomes the selected one). \"Open full chat →\" there jumps to the chat itself.",
            "Small rows under a chat = its subagents (dot: blue running, green done). Rows show three plus a remainder count; hover the row to reveal the full stack. Hover a row for its full name; click one to peek at its transcript in a read-only side panel (a subagent can't be continued). ✕ or Escape closes it.",
            "Archive hides a chat from the default view (same as the sidebar's archive). \"Show archived\" brings archived chats back, dimmed.",
            "\"Repos\" hides whole repos everywhere (list, grouping, search). Search matches title, branch, last message, folder, ticket id and PR title.",
        ];
        div()
            .flex_none()
            .mx(px(16.0))
            .mb(px(10.0))
            .p(px(12.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_raised)
            .flex()
            .flex_wrap()
            .gap(px(24.0))
            .child(group("Status", status_rows))
            .child(
                div()
                    .flex_1()
                    .min_w(px(280.0))
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(
                        div()
                            .pb(px(2.0))
                            .text_size(crate::typography::ui_rems(10.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(SharedString::from("READING A ROW")),
                    )
                    .children(notes.iter().map(|note| {
                        div()
                            .text_size(crate::typography::ui_rems(10.5))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(*note))
                    })),
            )
            .into_any_element()
    }

    /// Web `#empty-message`, adapted: distinguishes "no chats at all" from
    /// "everything is filtered out" and says which filters are active.
    fn render_empty_state(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        // No chats at all (as opposed to "everything's filtered out") is
        // exactly when a prominent way to start one belongs right here —
        // same button, same action as the header's.
        let truly_empty = self.unfiltered_count == 0;
        let (headline, detail) = if truly_empty {
            (
                if self.show_archived {
                    "No chats yet."
                } else {
                    "No active chats."
                },
                None,
            )
        } else {
            let mut active: Vec<String> = Vec::new();
            if !self.search.read(cx).text().trim().is_empty() {
                active.push("search".into());
            }
            if !self.hidden_repos.is_empty() {
                active.push(repo_filter_label(self.hidden_repos.len()).to_lowercase());
            }
            if self.hide_stale {
                active.push("hide stale".into());
            }
            let detail =
                (!active.is_empty()).then(|| format!("Active filters: {}.", active.join(", ")));
            ("Every chat is hidden by the current filters.", detail)
        };
        let new_chat_button = truly_empty.then(|| self.render_new_chat_button(theme, cx));
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(4.0))
            .p(px(24.0))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(13.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(headline)),
            )
            .when_some(detail, |el, detail| {
                el.child(
                    div()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(detail)),
                )
            })
            .when_some(new_chat_button, |el, button| {
                el.child(div().mt(px(4.0)).child(button))
            })
            .into_any_element()
    }
}

/// How a card/row is dimmed and whether it carries the unread-done ribbon:
/// `(opacity, ribbon)`.
///
/// The ribbon replaced a white box-shadow halo: gpui's `opacity` multiplies
/// the alpha of EVERYTHING the element paints — its box-shadow included —
/// and a gpui shadow is a blurred filled rect painted *under* the card, so
/// on a translucent (dimmed) card the white halo both faded AND bled through
/// the card's own background as a milky wash. That doesn't apply to the
/// ribbon (it only ever shows at full opacity — see below), but the
/// precedence it sits under is unchanged. Dimmed + unread was also the
/// common case, not an edge case: the unread set starts empty, so every
/// never-opened done chat quiet for >`STALE_AFTER` was stale and unread at
/// once. Hence:
/// - unread-done beats stale: it's the newest, most attention-worthy
///   signal, so the card renders at full opacity with the ribbon showing;
///   once acknowledged, normal stale dimming returns.
/// - archived beats unread-done: the user explicitly put it away, so it
///   dims (0.4) and shows no ribbon at all (a faded, bleeding ribbon is
///   exactly the look being avoided).
fn card_dimming(archived: bool, stale: bool, unread_done: bool) -> (Option<f32>, bool) {
    if archived {
        return (Some(0.4), false);
    }
    if unread_done {
        return (None, true);
    }
    (stale.then_some(0.55), false)
}

/// Width (px) of the unread-done left-edge ribbon, replacing the old white
/// halo (`unread_done_glow`, a two-layer box-shadow — removed: it bled
/// through a dimmed card's translucent background as a milky wash, per
/// `card_dimming`'s doc comment). A solid edge bar has no such bleed
/// concern (the ribbon only ever renders at full opacity), reads cleanly
/// against any of the status-tinted card backgrounds (blue/green/red/
/// yellow), and against the green tint every done card already carries —
/// plain white is the safest choice against all of them, so that's the
/// color used at the call sites, at full opacity.
///
/// `scale` is the swatch scale (`1.0` for list rows), floored so the
/// ribbon stays visible zoomed out — same floor `unread_done_glow` used for
/// its blur/spread.
const UNREAD_DONE_RIBBON_WIDTH: f32 = 3.0;
const UNREAD_DONE_RIBBON_MIN_SCALE: f32 = 0.5;

fn unread_done_ribbon_width(scale: f32) -> f32 {
    UNREAD_DONE_RIBBON_WIDTH * scale.max(UNREAD_DONE_RIBBON_MIN_SCALE)
}

/// The ribbon itself: an absolutely-positioned bar flush along the left
/// edge, `top`/`bottom` pinned to `0` so it spans the card's full height.
/// Relies on the caller already being `overflow_hidden()` with rounded
/// corners (every caller here is) to clip it to the corner radius, rather
/// than rounding the bar's own corners — one clip mask instead of matching
/// radii by hand at every zoom level.
fn unread_done_ribbon(scale: f32) -> gpui::Div {
    div()
        .absolute()
        .top(px(0.0))
        .bottom(px(0.0))
        .left(px(0.0))
        .w(px(unread_done_ribbon_width(scale)))
        .bg(gpui::white())
}

/// `.lane-header` / `.lane-header.nested` (`session_canvas.html:299-315`):
/// text in the group's color, underlined in the same color; top level is
/// 13px bold uppercase with a 2px rule, nested is 10px semibold mixed-case
/// with a 1px rule.
fn lane_header(text: String, color: gpui::Hsla, depth: usize) -> gpui::Div {
    let top = depth == 0;
    div()
        .flex_none()
        .text_color(color)
        .border_color(color)
        .when(top, |el| {
            el.text_size(crate::typography::ui_rems(13.0))
                .font_weight(gpui::FontWeight::BOLD)
                .pb(px(6.0))
                .border_b_2()
        })
        .when(!top, |el| {
            el.text_size(crate::typography::ui_rems(10.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .pb(px(3.0))
                .border_b_1()
        })
        .child(SharedString::from(if top {
            text.to_uppercase()
        } else {
            text
        }))
}

impl Overview {
    fn render_list_body(
        &mut self,
        rows: &[OverviewRow],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let mounted_ids: Vec<String> = rows.iter().map(|row| row.chat.id.clone()).collect();
        reconcile_hovered_row(&mut self.hovered_row, &mounted_ids);
        self.list_order.clear();
        if rows.is_empty() {
            self.list_selected = None;
            return self.render_empty_state(theme, cx);
        }

        // PR-row jump target: the index among the scroll container's
        // DIRECT children (group headers included), which is what
        // `ScrollHandle::scroll_to_item` counts.
        let focus_id = self.focus_due.take();
        let mut focus_child: Option<usize> = None;
        let children: Vec<gpui::AnyElement> = if self.active_groups.is_empty() {
            focus_child = focus_id
                .as_deref()
                .and_then(|id| rows.iter().position(|r| r.chat.id == id));
            self.list_order = rows
                .iter()
                .enumerate()
                .map(|(ix, row)| (row.chat.id.clone(), ix))
                .collect();
            rows.iter()
                .enumerate()
                .map(|(ix, row)| self.render_list_row(ix, row, theme, cx))
                .collect()
        } else {
            let mut items: Vec<ListItem<'_>> = Vec::new();
            let refs: Vec<&OverviewRow> = rows.iter().collect();
            flatten_partition(
                &overview_grouping::partition(refs, &self.active_groups),
                0,
                &mut items,
            );
            let mut out = Vec::new();
            let mut ix = 0usize;
            for item in items {
                match item {
                    // Same header treatment as the web's lane headers
                    // (`.lane-header` / `.lane-header.nested`): colored,
                    // underlined in its own color, uppercase+bold at the top
                    // level, smaller/thinner nested.
                    ListItem::Header {
                        label,
                        color,
                        depth,
                        count,
                    } => out.push(
                        div()
                            .flex()
                            .pl(px(10.0 + depth as f32 * 14.0))
                            .pr(px(10.0))
                            .pt(px(if depth == 0 { 14.0 } else { 8.0 }))
                            .pb(px(2.0))
                            .child(lane_header(format!("{label} ({count})"), color, depth))
                            .into_any_element(),
                    ),
                    ListItem::Row(row) => {
                        if focus_id.as_deref() == Some(row.chat.id.as_str()) {
                            focus_child = Some(out.len());
                        }
                        self.list_order.push((row.chat.id.clone(), out.len()));
                        out.push(self.render_list_row(ix, row, theme, cx));
                        ix += 1;
                    }
                }
            }
            out
        };
        if let Some(child) = focus_child {
            self.scroll.scroll_to_item(child);
        }
        // A filtered-out row loses the keyboard selection.
        if self
            .list_selected
            .as_ref()
            .is_some_and(|id| !self.list_order.iter().any(|(row, _)| row == id))
        {
            self.list_selected = None;
        }

        div()
            .id("overview-list")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(1.0))
            .px(px(8.0))
            .pb(px(8.0))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .children(children)
            .into_any_element()
    }

    /// One List-mode row. Same click split as the web's `renderTile`: clicking the row toggles its expanded detail in place
    /// (and acknowledges an unread done); the explicit "View full chat →"
    /// control opens the interactive chat panel. Neither navigates away
    /// — "Open full chat →" in that panel does.
    fn render_list_row(
        &mut self,
        ix: usize,
        row: &OverviewRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let now = Utc::now();
        let chat_id = row.chat.id.clone();
        let title: SharedString = row
            .chat
            .title
            .clone()
            .unwrap_or_else(|| "New session".to_string())
            .into();
        let cwd: SharedString = row
            .chat
            .cwd
            .clone()
            .unwrap_or_else(|| "No folder".to_string())
            .into();
        let when: SharedString = row
            .chat
            .last_message_at
            .map(|at| format_time_ago(at, now))
            .unwrap_or_else(|| format_time_ago(row.chat.created_at, now))
            .into();
        let dot = reference_status_color(row.status);
        let (icon, label) = status_style(row.status);
        let unread = is_unread_done(
            &self.acknowledged_done,
            &chat_id,
            row.status == ChatIndicator::Completed,
        );
        let (dim, ribbon) = card_dimming(row.chat.archived, row.stale, unread);
        // A chat whose work item needs the user stays at full opacity.
        let dim = dim.filter(|_| !(row.user_action_required && !row.chat.archived));
        let attention = crate::work_outline::chips::attention_indicator(
            format!("overview-row-attention-{chat_id}"),
            row.user_action_required,
            row.user_action_count,
            theme,
        );
        let exception_mark = row.execution_exception.then(|| {
            crate::work_outline::chips::execution_exception_mark(
                format!("overview-row-execution-exception-{chat_id}"),
                theme,
            )
        });
        let drift_mark = row.instruction_drift.then(|| {
            self.instruction_drift_mark(
                format!("overview-row-instruction-drift-{chat_id}"),
                &chat_id,
                false,
                theme,
            )
        });
        let path_chip = row.work_item_path.as_ref().map(|path| {
            crate::work_outline::chips::path_chip(
                format!("overview-row-work-item-{chat_id}"),
                path,
                theme,
            )
        });
        let is_expanded = self.expanded.contains(&chat_id);
        let model = model_badge(&row.chat, theme);
        let badges = self.badges_for(&chat_id, theme);
        let archive = (!row.chat.archived).then(|| self.archive_button(chat_id.clone(), theme, cx));
        let open_button = self.open_chat_button(chat_id.clone(), theme, cx);
        let has_subagent_overflow = row.subagent_count > SUBAGENT_PREVIEW_LIMIT;
        let show_all_subagents =
            has_subagent_overflow && self.hovered_row.as_deref() == Some(chat_id.as_str());
        let subagent_tiles = self.subagent_tiles_for(&chat_id, theme, show_all_subagents, cx);
        let detail = is_expanded.then(|| self.expanded_detail_for(row, theme, chat_id.clone(), cx));
        let toggle_id = chat_id.clone();
        let hover_id = chat_id.clone();
        div()
            .id(("overview-row", ix))
            .debug_selector(|| "overview-list-row".into())
            .relative()
            // Never shrink inside the scrolling list column. The row is
            // `overflow_hidden` (below, for the ribbon), which zeroes its
            // automatic flex min-height — so without this, once the list
            // outgrows the viewport every row got squeezed toward 0 and the
            // squeezed part (the subagent rows at the bottom first) was
            // clipped away instead of the list scrolling.
            .flex_none()
            .flex()
            .flex_col()
            .rounded(px(6.0))
            // Clips the unread-done ribbon (an absolutely-positioned child
            // below) to the row's own rounded corner instead of squaring it
            // off; see `unread_done_ribbon`. A list row is normally
            // background-less and no longer needs an opaque plate now that
            // there's no halo to ring — the ribbon just paints on top.
            .overflow_hidden()
            .border_1()
            .border_color(gpui::transparent_black())
            // The chat open in the side panel.
            .when(
                self.panel_highlight.as_deref() == Some(chat_id.as_str()),
                |el| el.border_color(theme.accent),
            )
            .when(is_expanded, |el| el.bg(theme.element_hover.opacity(0.5)))
            .when(
                self.list_selected.as_deref() == Some(chat_id.as_str()),
                |el| el.bg(theme.element_active),
            )
            .when_some(dim, |el, opacity| el.opacity(opacity))
            .hover(|el| el.bg(theme.element_hover))
            .when(has_subagent_overflow, |el| {
                el.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if *hovered {
                        if this.hovered_row.as_deref() != Some(hover_id.as_str()) {
                            this.hovered_row = Some(hover_id.clone());
                            cx.notify();
                        }
                    } else if this.hovered_row.as_deref() == Some(hover_id.as_str()) {
                        this.hovered_row = None;
                        cx.notify();
                    }
                }))
            })
            .when(ribbon, |el| el.child(unread_done_ribbon(1.0)))
            .child(
                div()
                    .id(("overview-row-click", ix))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(10.0))
                    .py(px(8.0))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_expanded(&toggle_id, cx);
                        cx.notify();
                    }))
                    .child(div().flex_none().size(px(7.0)).rounded_full().bg(dot))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(theme.text)
                                            .child(title),
                                    )
                                    .children(drift_mark)
                                    .children(exception_mark)
                                    .when_some(attention, |el, attention| el.child(attention)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(crate::typography::ui_rems(11.0))
                                            .text_color(theme.text_muted.opacity(0.7))
                                            .child(cwd),
                                    )
                                    .child(model)
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_size(crate::typography::ui_rems(10.0))
                                            .text_color(dot)
                                            .child(SharedString::from(format!("{icon} {label}"))),
                                    )
                                    .when_some(path_chip, |el, chip| el.child(chip)),
                            )
                            .when_some(badges, |el, badges| el.child(badges)),
                    )
                    // Same bar + label as the web's tile face, at a fixed width so
                    // the column lines up down the list.
                    .child(
                        div()
                            .flex_none()
                            .w(px(76.0))
                            .child(context_meter(row.context_pct, theme)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(crate::typography::ui_rems(10.0))
                            .text_color(theme.text_muted.opacity(0.6))
                            .child(when),
                    )
                    .child(open_button)
                    .when_some(archive, |el, archive| el.child(archive)),
            )
            .when(!subagent_tiles.is_empty(), |el| {
                el.child(
                    div()
                        .debug_selector(|| "overview-list-subagents".into())
                        .flex()
                        .flex_col()
                        .gap(px(SUBAGENT_TILE_GAP))
                        .pb(px(8.0))
                        .pl(px(27.0))
                        .pr(px(10.0))
                        .children(subagent_tiles),
                )
            })
            .when_some(detail, |el, detail| {
                el.child(div().px(px(10.0)).pb(px(8.0)).child(detail))
            })
            .into_any_element()
    }
}

impl Overview {
    /// Switch the main column's presentation and persist it.
    fn set_mode(&mut self, mode: OverviewMode, cx: &mut Context<Self>) {
        if self.view_mode == mode {
            return;
        }
        self.view_mode = mode;
        self.hovered_row = None;
        self.outline_dirty = true;
        self.view_prefs_changed(cx);
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn mode(&self) -> OverviewMode {
        self.view_mode
    }

    #[cfg(test)]
    /// The search field's focus handle (tests of context-scoped keys).
    #[cfg(test)]
    pub(crate) fn search_focus_handle(&self, cx: &gpui::App) -> gpui::FocusHandle {
        gpui::Focusable::focus_handle(self.search.read(cx), cx)
    }

    pub(crate) fn outline_entity(&self) -> Entity<OutlineView> {
        self.outline.clone()
    }

    #[cfg(test)]
    pub(crate) fn has_work_item_details(&self) -> bool {
        self.work_item_details.is_some()
    }

    /// Tell the watch reconciler which Spaces' outlines the Overview needs:
    /// all of them (path chips and attention on the List need them too).
    /// Only on change, so it never loops with the `AppState` observer.
    fn declare_outline_spaces(&mut self, cx: &mut Context<Self>) {
        if self.outline_fixture.is_some() {
            return;
        }
        let ids: Vec<String> = self
            .state
            .read(cx)
            .spaces
            .iter()
            .map(|space| space.id.clone())
            .collect();
        if self.declared_outline_spaces.as_ref() == Some(&ids) {
            return;
        }
        self.declared_outline_spaces = Some(ids.clone());
        self.state
            .update(cx, |state, cx| state.declare_work_outline_visible(ids, cx));
    }

    /// Push fresh inputs into the Outline when `rows()` rebuilt or an
    /// Outline-only option changed.
    fn sync_outline(&mut self, cx: &mut Context<Self>) {
        if !self.outline_dirty {
            return;
        }
        self.outline_dirty = false;
        let inputs = OutlineInputs {
            spaces: self.outline_spaces(cx),
            chats: (*self.outline_chats).clone(),
            options: OutlineOptions {
                hide_completed: self.hide_stale && !self.outline_show_completed,
                show_archived: self.show_archived,
                query: self.search.read(cx).text().trim().to_lowercase(),
                all_spaces: true,
                // The Outline owns the reveal (`OutlineView`).
                show_hidden_spaces: false,
                // Likewise: the view compacts single-chat items itself.
                compact_single_chat: false,
            },
        };
        self.outline
            .update(cx, |outline, cx| outline.set_inputs(inputs, cx));
    }

    fn render_outline_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let details_key = self.work_item_details.as_ref().map(|details| {
            let details = details.read(cx);
            row_key::item(&details.space_id, &details.item_id)
        });
        let panel_chat = self.panel_highlight.clone();
        self.outline.update(cx, |outline, _| {
            outline.set_highlights(details_key, panel_chat)
        });
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.outline.clone())
            .into_any_element()
    }

    /// Outline-only toolbar controls: the inline "New item" entry point, the
    /// reveal for projects hidden from the Outline, and the explicit reveal
    /// for completed items hidden by "Hide stale".
    fn render_outline_toolbar(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let pill = |id: &'static str, label: String, active: bool| {
            div()
                .id(id)
                .px(px(8.0))
                .py(px(3.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(11.0))
                .bg(theme.element_hover.opacity(0.5))
                .when(active, |el| {
                    el.bg(theme.element_active).text_color(theme.text)
                })
                .when(!active, |el| el.text_color(theme.text_muted))
                .hover(|el| el.text_color(theme.text))
                .child(SharedString::from(label))
        };
        let mut out = vec![
            pill("outline-new-item", "New item".into(), false)
                .debug_selector(|| "outline-new-item".into())
                .hint(
                    "Create a work item",
                    HintKey::Command(WorkOutlineCommand::CreateWorkItem),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.run_work_outline_command(WorkOutlineCommand::CreateWorkItem, window, cx);
                }))
                .into_any_element(),
        ];
        // Required Actions: always reachable; the count uses the attention token.
        let open = crate::work_outline::required_actions::open_request_count(
            &self.required_action_spaces(cx),
        );
        out.push(
            pill(
                "outline-required-actions",
                if open > 0 {
                    format!("Required actions ({open})")
                } else {
                    "Required actions".to_string()
                },
                self.required_actions_panel.is_some(),
            )
            .debug_selector(|| "outline-required-actions".into())
            .hint(
                "Requests waiting on you",
                HintKey::Command(WorkOutlineCommand::OpenRequiredActions),
            )
            .when(open > 0, |el| {
                el.text_color(crate::work_outline::chips::attention_color(theme))
            })
            .on_click(cx.listener(|this, _, _, cx| {
                if this.required_actions_panel.is_some() {
                    this.close_required_actions(cx);
                } else {
                    this.open_required_actions(cx);
                }
            }))
            .into_any_element(),
        );
        // Linear (only while some Space has Linear on).
        out.extend(self.linear_toolbar_button(theme, cx));
        // The shortcut reference, on demand (also `?` in the Outline and the
        // palette's Show Outline Shortcuts).
        out.push(
            div()
                .id("outline-shortcuts")
                .debug_selector(|| "outline-shortcuts".into())
                .aria_label("Shortcuts")
                .flex()
                .items_center()
                .px(px(6.0))
                .py(px(4.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .bg(theme.element_hover.opacity(0.5))
                .text_color(theme.text_muted)
                .hover(|el| el.text_color(theme.text))
                .child(crate::icons::icon(crate::icons::KEYBOARD).size(px(12.0)))
                .hint(
                    "Shortcuts",
                    HintKey::Outline(crate::shell::work_outline_ui::OutlineNavKey::ShowShortcuts),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(shell) = this.shell.upgrade() {
                        shell.update(cx, |shell, cx| shell.open_outline_shortcuts(cx));
                    }
                }))
                .into_any_element(),
        );
        // Projects hidden from the Outline (multi-Space only): reveal them as
        // dimmed rows with Unhide.
        let (hidden_spaces, showing_hidden) = {
            let outline = self.outline.read(cx);
            (
                outline.hidden_space_count(),
                outline.showing_hidden_spaces(),
            )
        };
        if hidden_spaces > 0 {
            out.push(
                pill(
                    "outline-hidden-spaces",
                    format!("Hidden projects ({hidden_spaces})"),
                    showing_hidden,
                )
                .debug_selector(|| "outline-hidden-spaces".into())
                .hint(
                    if showing_hidden {
                        "Stop showing the projects hidden from the Outline"
                    } else {
                        "Show the projects hidden from the Outline, to unhide them"
                    },
                    HintKey::None,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.outline.update(cx, |outline, cx| {
                        outline.set_show_hidden_spaces(!showing_hidden, cx)
                    });
                }))
                .into_any_element(),
            );
        }
        if self.hide_stale {
            let hidden = self.outline.read(cx).hidden_completed();
            let label = if self.outline_show_completed {
                "Completed shown".to_string()
            } else {
                format!("Completed hidden ({hidden})")
            };
            out.push(
                pill("outline-show-completed", label, self.outline_show_completed)
                    .hint(
                        if self.outline_show_completed {
                            "Hide completed work items again"
                        } else {
                            "Show the completed work items Hide stale hides"
                        },
                        HintKey::None,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.outline_show_completed = !this.outline_show_completed;
                        this.outline_dirty = true;
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        out
    }

    /// The List work-item filter dropdown (same shape as "Repos").
    fn render_work_item_filter(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let active = !self.work_item_filter.is_empty() || self.work_item_filter_popup.is_open();
        let trigger = div()
            .id("overview-work-item-filter")
            .relative()
            .px(px(8.0))
            .py(px(3.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(11.0))
            .bg(theme.element_hover.opacity(0.5))
            .when(active, |el| {
                el.bg(theme.element_active).text_color(theme.text)
            })
            .when(!active, |el| el.text_color(theme.text_muted))
            .hover(|el| el.text_color(theme.text))
            .child(SharedString::from(format!(
                "{} ▾",
                work_item_filter::filter_label(&self.work_item_filter)
            )))
            .hint("Filter by work item", HintKey::None)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.work_item_filter_popup.note_trigger_press()),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if this.work_item_filter_popup.take_press_was_open() {
                    this.close_work_item_filter(cx);
                } else {
                    this.work_item_filter_popup.open(());
                    cx.notify();
                }
            }));
        if self.work_item_filter_popup.get().is_none() {
            return trigger.into_any_element();
        }
        let closing = self.work_item_filter_popup.closing_since();
        let popup = theme.for_popup();
        let check_row = |id: gpui::ElementId, label: String, depth: u32, checked: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(7.0))
                .pl(px(6.0 + depth as f32 * 12.0))
                .pr(px(6.0))
                .py(px(3.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .hover(|el| el.bg(popup.element_hover))
                .child(
                    div()
                        .flex_none()
                        .size(px(12.0))
                        .rounded(px(3.0))
                        .border_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(crate::typography::ui_rems(9.0))
                        .when(checked, |el| {
                            el.bg(popup.accent)
                                .border_color(popup.accent)
                                .text_color(gpui::white())
                                .child("✓")
                        })
                        .when(!checked, |el| el.border_color(popup.border_strong)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(if checked {
                            popup.text
                        } else {
                            popup.text_muted
                        })
                        .child(SharedString::from(label)),
                )
        };
        let choices = self.work_item_index.choices();
        let multi_space = self.state.read(cx).spaces.len() > 1;
        let mut list: Vec<gpui::AnyElement> = Vec::new();
        let unassociated = work_item_filter::UNASSOCIATED_KEY.to_string();
        list.push(
            check_row(
                "overview-work-item-filter-unassociated".into(),
                "Unassociated chats".into(),
                0,
                self.work_item_filter.contains(&unassociated),
            )
            .on_click(
                cx.listener(move |this, _, _, cx| this.toggle_work_item_filter(&unassociated, cx)),
            )
            .into_any_element(),
        );
        let mut last_space: Option<String> = None;
        for (ix, choice) in choices.into_iter().enumerate() {
            if multi_space && last_space.as_deref() != Some(choice.space_id.as_str()) {
                last_space = Some(choice.space_id.clone());
                let label = self
                    .state
                    .read(cx)
                    .space_row(&choice.space_id)
                    .map(|space| space.display_name().to_string())
                    .unwrap_or_default();
                list.push(
                    div()
                        .px(px(6.0))
                        .pt(px(6.0))
                        .text_size(crate::typography::ui_rems(10.0))
                        .text_color(popup.text_muted)
                        .child(SharedString::from(label))
                        .into_any_element(),
                );
            }
            let key = choice.item_id.clone();
            list.push(
                check_row(
                    ("overview-work-item-filter-row", ix).into(),
                    choice.title,
                    choice.depth,
                    self.work_item_filter.contains(&key),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_work_item_filter(&key, cx)))
                .into_any_element(),
            );
        }
        let card = popover::popover_card(&popup)
            .w(px(260.0))
            .max_h(px(380.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_work_item_filter(cx)))
            .child(
                div().flex().gap(px(6.0)).pb(px(4.0)).child(
                    div()
                        .id("overview-work-item-filter-clear")
                        .px(px(8.0))
                        .py(px(2.0))
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .text_size(crate::typography::ui_rems(10.5))
                        .text_color(popup.text_muted)
                        .border_1()
                        .border_color(popup.border)
                        .hover(|el| el.bg(popup.element_hover).text_color(popup.text))
                        .child("All chats")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.work_item_filter.clear();
                            this.work_item_filter_changed(cx);
                        })),
                ),
            )
            .child(
                div()
                    .id("overview-work-item-filter-list")
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .overflow_y_scroll()
                    .children(list),
            );
        trigger
            .child(popover::anchored_menu_below(
                "overview-work-item-filter-menu",
                card.into_any_element(),
                closing,
            ))
            .into_any_element()
    }

    fn close_work_item_filter(&mut self, cx: &mut Context<Self>) {
        if self.work_item_filter_popup.begin_close() {
            popover::reap_popup(cx, |this: &mut Self| &mut this.work_item_filter_popup);
        }
        cx.notify();
    }

    fn toggle_work_item_filter(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.work_item_filter.remove(key) {
            self.work_item_filter.insert(key.to_string());
        }
        self.work_item_filter_changed(cx);
    }

    /// Persist and re-derive rows. Filtering only changes which rows exist.
    fn work_item_filter_changed(&mut self, cx: &mut Context<Self>) {
        self.work_item_filter_needs_validation = false;
        self.save_string_set(
            overview_positions::WORK_ITEM_FILTER_FILE,
            &self.work_item_filter,
            cx,
        );
        self.rows_dirty = true;
        cx.notify();
    }

    /// Open Work Item Details in the right slot (replacing a details surface
    /// for another item; the chat panel / peek state is left as is).
    pub(crate) fn open_work_item_details(
        &mut self,
        space_id: String,
        item_id: String,
        cx: &mut Context<Self>,
    ) {
        if self.work_item_details.as_ref().is_some_and(|details| {
            let details = details.read(cx);
            details.space_id == space_id && details.item_id == item_id
        }) {
            return;
        }
        self.leave_work_item_details(DetailsExit::OpenItem { space_id, item_id }, cx);
    }

    /// Open Work Item Details for `item_id` (`""` = the Space root), then run
    /// `then` on them. The Outline context menu reaches the details' own
    /// actions (status, archive, restore, instructions, execution) this way,
    /// so they keep their confirmations, in-flight guards, and error line.
    /// With an unsaved draft in other details, `then` runs only after
    /// Discard; Keep Editing drops it.
    pub(crate) fn with_work_item_details(
        &mut self,
        space_id: String,
        item_id: String,
        window: &Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut WorkItemDetails, &mut Window, &mut Context<WorkItemDetails>) + 'static,
    ) {
        let handle = window.window_handle();
        // Deferred: callers are mid-update of the Shell and this Overview.
        let run = move |this: &Self, cx: &mut Context<Self>| {
            let Some(details) = this.work_item_details.clone() else {
                return;
            };
            let space_id = details.read(cx).space_id.clone();
            let snapshot = this.outline_snapshot(&space_id, cx);
            let chats = this.outline_chats.clone();
            details.update(cx, |details, _| details.set_data(snapshot, chats));
            cx.defer(move |cx| {
                handle
                    .update(cx, |_, window, cx| {
                        details.update(cx, |details, cx| then(details, window, cx))
                    })
                    .ok();
            });
        };
        let current = self.work_item_details.clone();
        if current.as_ref().is_some_and(|details| {
            let details = details.read(cx);
            details.space_id == space_id && details.item_id == item_id
        }) {
            run(self, cx);
            return;
        }
        let Some(current) = current else {
            self.replace_work_item_details(space_id, item_id, cx);
            run(self, cx);
            return;
        };
        let overview = cx.entity().downgrade();
        current.update(cx, |current, cx| {
            current.confirm_discard(cx, move |cx| {
                overview
                    .update(cx, |this, cx| {
                        this.replace_work_item_details(space_id, item_id, cx);
                        run(this, cx);
                    })
                    .ok();
            })
        });
    }

    /// Leave the current Work Item Details (switch item, open a Chat, or
    /// close) without silently dropping a draft: with unsaved edits this asks
    /// "Keep Editing / Discard" and acts only after Discard.
    fn leave_work_item_details(&mut self, exit: DetailsExit, cx: &mut Context<Self>) {
        let Some(details) = self.work_item_details.clone() else {
            self.apply_details_exit(exit, cx);
            return;
        };
        let overview = cx.entity().downgrade();
        details.update(cx, |details, cx| {
            details.confirm_discard(cx, move |cx| {
                overview
                    .update(cx, |this, cx| this.apply_details_exit(exit, cx))
                    .ok();
            })
        });
    }

    fn apply_details_exit(&mut self, exit: DetailsExit, cx: &mut Context<Self>) {
        match exit {
            DetailsExit::Close => self.close_work_item_details(cx),
            DetailsExit::OpenChat(chat_id) => {
                self.close_work_item_details(cx);
                self.close_required_actions(cx);
                self.outline_chat_return = Some(OutlineChatReturn {
                    chat_id: chat_id.clone(),
                    full_route: false,
                });
                self.open_chat_panel(chat_id, cx);
            }
            DetailsExit::OpenItem { space_id, item_id } => {
                self.replace_work_item_details(space_id, item_id, cx)
            }
        }
    }

    fn replace_work_item_details(
        &mut self,
        space_id: String,
        item_id: String,
        cx: &mut Context<Self>,
    ) {
        let key = row_key::item(&space_id, &item_id);
        let details = cx.new(|cx| {
            WorkItemDetails::new(
                self.state.clone(),
                self.shell.clone(),
                space_id,
                item_id,
                cx,
            )
        });
        let events = cx.subscribe(&details, |this: &mut Self, _, event, cx| match event {
            // `request_close` already confirmed any discard.
            DetailsEvent::Close => this.close_work_item_details(cx),
            DetailsEvent::OpenChat(chat_id) => {
                this.leave_work_item_details(DetailsExit::OpenChat(chat_id.clone()), cx);
            }
            DetailsEvent::OpenItem { space_id, item_id } => {
                this.open_work_item_details(space_id.clone(), item_id.clone(), cx);
            }
            DetailsEvent::ReassociateChat {
                space_id,
                chat_id,
                work_item_id,
            } => {
                this.outline.update(cx, |outline, cx| {
                    outline.reassociate_chat(space_id, chat_id, work_item_id.clone(), cx)
                });
            }
            DetailsEvent::StartChat { space_id, item_id } => {
                if let Err(notice) = this.start_chat_for_item(space_id.clone(), item_id.clone(), cx)
                {
                    this.outline
                        .update(cx, |outline, cx| outline.show_error(notice, cx));
                }
            }
            DetailsEvent::StartPrReview {
                space_id,
                item_id,
                action,
            } => {
                if let Err(notice) =
                    this.start_pr_review_for_item(space_id.clone(), item_id.clone(), *action, cx)
                {
                    this.outline
                        .update(cx, |outline, cx| outline.show_error(notice, cx));
                }
            }
            DetailsEvent::AddLinearIssues { space_id, item_id } => {
                if let Err(notice) =
                    this.open_project_issue_picker(space_id.clone(), item_id.clone(), cx)
                {
                    this.outline
                        .update(cx, |outline, cx| outline.show_notice(notice, cx));
                }
            }
        });
        self.outline
            .update(cx, |outline, cx| outline.select(&key, cx));
        if let Some((space, item)) = self.pending_effective.take() {
            details.update(cx, |details, cx| {
                if details.space_id == space && details.item_id == item {
                    details.open_effective(cx);
                }
            });
        }
        if let Some((space, item)) = self.pending_proposals.take() {
            details.update(cx, |details, cx| {
                if details.space_id == space && details.item_id == item {
                    details.open_proposals(cx);
                }
            });
        }
        self.work_item_details = Some(details);
        self._details_events = Some(events);
        cx.notify();
    }

    /// Close Work Item Details; the right slot falls back to whatever the
    /// chat panel / peek state says (restoring the previous layout).
    pub(crate) fn close_work_item_details(&mut self, cx: &mut Context<Self>) {
        if self.work_item_details.take().is_some() {
            self._details_events = None;
            cx.notify();
        }
    }

    /// Run one Work Outline command in the Overview: switch to Outline mode
    /// and focus it, resolve the selected work item or Chat, then start the
    /// inline create / rename, open the move or associate picker. With no
    /// valid target it shows a short notice in the Outline; it never silently
    /// does nothing (m0-contract §7). Called by `Shell::run_work_outline_command`.
    pub(crate) fn run_work_outline_command(
        &mut self,
        command: WorkOutlineCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Load saved view prefs before changing mode: `set_mode` persists the
        // in-memory prefs, which would overwrite the saved file with defaults.
        self.ensure_prefs_loaded(cx);
        // The Linear toggle reads where focus is now, and keeps the mode: the
        // column sits beside every body. Only its notice needs the Outline.
        if command == WorkOutlineCommand::OpenLinear {
            if let Err(notice) = self.toggle_linear_panel(window, cx) {
                self.set_mode(OverviewMode::Outline, cx);
                self.outline
                    .update(cx, |outline, cx| outline.show_notice(notice, cx));
            }
            cx.notify();
            return;
        }
        self.set_mode(OverviewMode::Outline, cx);
        let _ = self.rows(cx);
        self.sync_outline(cx);
        let outline = self.outline.clone();
        let handle = gpui::Focusable::focus_handle(outline.read(cx), cx);
        window.focus(&handle, cx);
        let chat_hint = self.state.read(cx).selected_chat.clone();
        let result: Result<(), &'static str> = match command {
            WorkOutlineCommand::OpenOutline => Ok(()),
            // Handled by the Shell (`toggle_outline_focus`), which needs the
            // route; reaching here just opens and focuses the Outline.
            WorkOutlineCommand::ToggleOutlineFocus => Ok(()),
            // Handled by the Shell (`open_full_chat`) before routing here.
            WorkOutlineCommand::OpenFullChat => Ok(()),
            // Normally handled by the Shell before routing here; the
            // reference is a Shell overlay, so ask for it after this update.
            WorkOutlineCommand::ShowOutlineShortcuts => {
                if let Some(shell) = self.shell.upgrade() {
                    cx.defer(move |cx| {
                        shell.update(cx, |shell, cx| shell.open_outline_shortcuts(cx))
                    });
                }
                Ok(())
            }
            WorkOutlineCommand::CreateWorkItem => {
                outline.update(cx, |outline, cx| outline.begin_create(false, window, cx))
            }
            WorkOutlineCommand::CreateChildWorkItem => {
                outline.update(cx, |outline, cx| outline.begin_create(true, window, cx))
            }
            WorkOutlineCommand::RenameWorkItem => {
                outline.update(cx, |outline, cx| outline.begin_rename(window, cx))
            }
            WorkOutlineCommand::MoveWorkItem => {
                outline.update(cx, |outline, cx| outline.open_move_picker(window, cx))
            }
            WorkOutlineCommand::AssociateChatWithWorkItem => outline.update(cx, |outline, cx| {
                outline.open_associate_picker(chat_hint, window, cx)
            }),
            WorkOutlineCommand::OpenEffectiveInstructions => {
                match self.effective_instructions_target(chat_hint.as_deref(), cx) {
                    Some((space_id, item_id)) => {
                        self.open_effective_instructions(space_id, item_id, cx);
                        Ok(())
                    }
                    None => Err("Select a work item first"),
                }
            }
            // Handled by the Shell where the Chat is; reaching here means no
            // Chat (or no excerpt) was available.
            WorkOutlineCommand::CreateInstructionFromChat => {
                Err("Open a Chat and select text to create an instruction from it")
            }
            WorkOutlineCommand::OpenRequiredActions => {
                self.open_required_actions(cx);
                Ok(())
            }
            // Handled above, before the Outline takes focus.
            WorkOutlineCommand::OpenLinear => Ok(()),
            WorkOutlineCommand::StartChatForWorkItem => {
                match self.outline.read(cx).selected_item() {
                    Some((space_id, item_id)) => self.start_chat_for_item(space_id, item_id, cx),
                    None => Err("Select a work item first"),
                }
            }
            WorkOutlineCommand::MoveWorkItemUp
            | WorkOutlineCommand::MoveWorkItemDown
            | WorkOutlineCommand::IndentWorkItem
            | WorkOutlineCommand::OutdentWorkItem => match command.restructure_op() {
                Some(op) => outline.update(cx, |outline, cx| outline.restructure_selection(op, cx)),
                None => Ok(()),
            },
        };
        if let Err(notice) = result {
            outline.update(cx, |outline, cx| outline.show_notice(notice, cx));
        }
        cx.notify();
    }

    /// Toggle Outline Focus, Chat → tree: switch to Outline mode, reveal and
    /// select `chat`'s row (expanding collapsed ancestors, scrolled into
    /// view) and focus the tree. `chat` also becomes the return target. A
    /// Chat the Outline does not show (projectless, another Space, filtered
    /// out) leaves the selection alone; the tree is focused either way.
    pub(crate) fn focus_outline_on_chat(
        &mut self,
        chat: Option<OutlineChatReturn>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_prefs_loaded(cx);
        self.set_mode(OverviewMode::Outline, cx);
        let _ = self.rows(cx);
        self.sync_outline(cx);
        if let Some(chat) = chat {
            let key = row_key::chat(&chat.chat_id);
            self.outline
                .update(cx, |outline, cx| outline.reveal(&key, cx));
            self.outline_chat_return = Some(chat);
        }
        let handle = gpui::Focusable::focus_handle(self.outline.read(cx), cx);
        window.focus(&handle, cx);
        self.outline_focus_pending = true;
        cx.notify();
    }

    /// Toggle Outline Focus's return target (see `outline_chat_return`).
    pub(crate) fn outline_chat_return(&self) -> Option<&OutlineChatReturn> {
        self.outline_chat_return.as_ref()
    }

    /// Whether the Outline tree (or its inline editor) holds focus on screen.
    pub(crate) fn outline_tree_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.view_mode == OverviewMode::Outline
            && gpui::Focusable::focus_handle(self.outline.read(cx), cx).contains_focused(window, cx)
    }

    /// The Chat the panel shows right now, if it is open.
    pub(crate) fn shown_panel_chat(&self, cx: &gpui::App) -> Option<String> {
        panel_chat_id(
            self.chat_panel_open,
            self.state.read(cx).selected_chat.as_deref(),
        )
    }

    /// Toggle Outline Focus, tree → Chat panel: reopen the panel on
    /// `chat_id` the way Enter on its row does (Work Item Details close
    /// through the discard prompt first), which focuses the composer.
    pub(crate) fn return_to_outline_chat_panel(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.leave_work_item_details(DetailsExit::OpenChat(chat_id), cx);
    }

    /// The Chat Open Full Chat opens: the Outline's selected Chat row while
    /// the tree has focus, else the Chat the panel shows (the highlighted
    /// List row; its composer may have focus), else the Outline's
    /// selected Chat row. A panel hidden under Work Item Details or Required
    /// Actions does not count; neither does a Chat row whose Chat is gone.
    pub(crate) fn full_chat_target(&self, window: &Window, cx: &gpui::App) -> Option<String> {
        let selected_row = || {
            (self.view_mode == OverviewMode::Outline)
                .then(|| self.outline.read(cx).selected_row())
                .flatten()
                .and_then(|row| row.open_chat())
                .filter(|row| !row.dangling)
                .map(|row| row.chat.id.clone())
        };
        if self.outline_tree_focused(window, cx)
            && let Some(chat_id) = selected_row()
        {
            return Some(chat_id);
        }
        // The Linear panel has its own column; it never hides the chat panel.
        let panel_visible =
            self.work_item_details.is_none() && self.required_actions_panel.is_none();
        panel_visible
            .then(|| self.shown_panel_chat(cx))
            .flatten()
            .or_else(selected_row)
    }

    /// Preview-on-select: the user moved the Outline selection. On a Chat
    /// row, show that Chat in the chat panel once the selection has rested
    /// for [`OUTLINE_PREVIEW_DEBOUNCE`] (a newer move restarts the wait). On
    /// any other row, cancel a pending switch: the panel keeps the last
    /// previewed Chat.
    fn schedule_outline_preview(&mut self, cx: &mut Context<Self>) {
        if self.outline.read(cx).selected_open_chat().is_none() {
            self.outline_preview = None;
            return;
        }
        self.outline_preview = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(OUTLINE_PREVIEW_DEBOUNCE)
                .await;
            this.update(cx, |this, cx| this.preview_outline_selection(cx))
                .ok();
        }));
    }

    /// Show the selected Outline Chat row in the chat panel WITHOUT moving
    /// focus (the tree keeps it, so arrows keep browsing). Viewing it the
    /// way the panel does: its unread done is acknowledged and it becomes
    /// the selected Chat, which is what the panel shows; it also becomes
    /// Toggle Outline Focus's return target. Never the full chat route. A
    /// slot held by Work Item Details, Required Actions or a subagent peek
    /// is left alone (they are never closed by browsing). The switch itself
    /// has no animation, so reduced motion needs nothing more here.
    fn preview_outline_selection(&mut self, cx: &mut Context<Self>) {
        self.outline_preview = None;
        if self.view_mode != OverviewMode::Outline
            || self.work_item_details.is_some()
            || self.required_actions_panel.is_some()
            || self.subagent_peek.is_some()
        {
            return;
        }
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        if !shell.read(cx).is_overview_route() {
            return;
        }
        let Some(chat_id) = self.outline.read(cx).selected_open_chat() else {
            return;
        };
        if self.shown_panel_chat(cx).as_deref() == Some(chat_id.as_str()) {
            return;
        }
        self.acknowledge_done(&chat_id, cx);
        self.outline_chat_return = Some(OutlineChatReturn {
            chat_id: chat_id.clone(),
            full_route: false,
        });
        shell.update(cx, |shell, cx| {
            shell.show_chat_in_overview_panel(chat_id, cx)
        });
        self.chat_panel_open = true;
        cx.notify();
    }

    /// Tests: whether a preview-on-select is waiting out its debounce.
    #[cfg(test)]
    pub(crate) fn outline_preview_pending(&self) -> bool {
        self.outline_preview.is_some()
    }

    /// Tests: switch the view mode (after loading saved prefs, as the real
    /// toggle does).
    #[cfg(test)]
    pub(crate) fn set_mode_for_test(&mut self, mode: OverviewMode, cx: &mut Context<Self>) {
        self.ensure_prefs_loaded(cx);
        self.set_mode(mode, cx);
    }

    /// Tests: the search field's text.
    #[cfg(test)]
    pub(crate) fn search_text(&self, cx: &gpui::App) -> String {
        self.search.read(cx).text().to_string()
    }

    /// Tests: set the search text (emits an edit, as typing does).
    #[cfg(test)]
    pub(crate) fn set_search_text_for_test(&mut self, text: &str, cx: &mut Context<Self>) {
        self.search
            .update(cx, |search, cx| search.set_text(text, cx));
    }

    /// Tests: set (or clear) the List selection.
    #[cfg(test)]
    pub(crate) fn set_list_selection_for_test(
        &mut self,
        id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.list_selected = id;
        cx.notify();
    }

    /// Tests: whether the List surface holds focus.
    #[cfg(test)]
    pub(crate) fn body_focused(&self, window: &Window) -> bool {
        self.body_focus.is_focused(window)
    }

    /// Tests: the keyboard-selected List row and the rendered row order.
    #[cfg(test)]
    pub(crate) fn list_selection_for_test(&self) -> (Option<String>, Vec<String>) {
        (
            self.list_selected.clone(),
            self.list_order.iter().map(|(id, _)| id.clone()).collect(),
        )
    }

    /// Tests: switch to List mode and open the chat panel on `chat_id` (the
    /// row's "View full chat →"), which highlights that row.
    #[cfg(test)]
    pub(crate) fn show_list_with_panel_for_test(
        &mut self,
        chat_id: String,
        cx: &mut Context<Self>,
    ) {
        self.ensure_prefs_loaded(cx);
        self.set_mode(OverviewMode::List, cx);
        self.open_chat_panel(chat_id, cx);
    }

    /// Open Full Chat with nothing to open: a notice in the Outline (the
    /// List has no notice line; there the command does nothing).
    pub(crate) fn show_full_chat_notice(&mut self, cx: &mut Context<Self>) {
        if self.view_mode == OverviewMode::Outline {
            self.show_outline_notice("Select a Chat or open one beside the Outline first", cx);
        }
    }

    /// The Overview half of leaving for the full chat route (the panel's
    /// "Open full chat →"): close the panel and acknowledge the Chat's unread
    /// done, as opening it from a row does. The Shell then opens the route.
    pub(crate) fn prepare_full_chat(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        self.close_chat_panel(cx);
        self.acknowledge_done(chat_id, cx);
    }

    /// Show a short notice in the Outline.
    pub(crate) fn show_outline_notice(&mut self, text: &'static str, cx: &mut Context<Self>) {
        self.outline
            .update(cx, |outline, cx| outline.show_notice(text, cx));
    }

    /// The item Open Effective Instructions targets: the selected work item,
    /// else the selected Chat row's item, else the app's selected Chat's item.
    fn effective_instructions_target(
        &self,
        chat_hint: Option<&str>,
        cx: &gpui::App,
    ) -> Option<(String, String)> {
        let outline = self.outline.read(cx);
        if let Some(target) = outline.selected_item() {
            return Some(target);
        }
        if let Some(target) = outline
            .selected_row()
            .and_then(|row| row.chat())
            .and_then(|chat| chat.work_item.clone())
        {
            return Some(target);
        }
        let chat_id = chat_hint?;
        outline
            .model()
            .rows
            .iter()
            .filter_map(|row| row.chat())
            .find(|chat| chat.chat.id == chat_id)
            .and_then(|chat| chat.work_item.clone())
    }

    /// Start Chat in a work item (Outline row action, details button,
    /// palette). When the item's instructions changed since its last launch,
    /// Effective Instructions open in details first (the pre-launch check);
    /// the Shell's Start Chat dialog then shows them expanded too.
    pub(crate) fn start_chat_for_item(
        &mut self,
        space_id: String,
        item_id: String,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let snapshot = self
            .outline_snapshot(&space_id, cx)
            .ok_or("This project's outline is not loaded")?;
        let item = snapshot.item(&item_id).ok_or("That work item is gone")?;
        if item.archived_at.is_some() {
            return Err("Restore the work item to start a Chat in it");
        }
        // A PR review item (PR review sync) starts a code review of its PR,
        // filed under the item, instead of the generic Start Chat dialog.
        if crate::github::pr_review_item(&snapshot, &item_id).is_some() {
            return self.start_pr_review_for_item(
                space_id,
                item_id,
                mast_proto::PrReviewAction::CodeReview,
                cx,
            );
        }
        if crate::work_outline::launch::instructions_changed_since_last_launch(&snapshot, &item_id)
        {
            self.open_effective_instructions(space_id.clone(), item_id.clone(), cx);
        }
        #[cfg(test)]
        if self.shell.upgrade().is_none() {
            self.captured_start_chat = Some((space_id, item_id));
            return Ok(());
        }
        let shell = self
            .shell
            .upgrade()
            .ok_or("Starting a Chat needs the app window")?;
        // Deferred: the palette path runs inside a Shell update.
        cx.defer(move |cx| {
            shell.update(cx, |shell, cx| {
                shell.open_start_chat_or_notice(&space_id, &item_id, cx)
            });
        });
        Ok(())
    }

    /// Open Work Item Details for the item with Effective Instructions
    /// expanded (the explicit open path; Milestone 3's pre-launch check
    /// reuses it).
    pub(crate) fn open_effective_instructions(
        &mut self,
        space_id: String,
        item_id: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(details) = self.work_item_details.as_ref().filter(|details| {
            let details = details.read(cx);
            details.space_id == space_id && details.item_id == item_id
        }) {
            details.update(cx, |details, cx| details.open_effective(cx));
            return;
        }
        self.pending_effective = Some((space_id.clone(), item_id.clone()));
        self.open_work_item_details(space_id, item_id, cx);
    }

    /// Open Work Item Details (`item_id` empty = Space root) with Proposed
    /// instructions expanded.
    pub(crate) fn open_proposals(
        &mut self,
        space_id: String,
        item_id: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(details) = self.work_item_details.as_ref().filter(|details| {
            let details = details.read(cx);
            details.space_id == space_id && details.item_id == item_id
        }) {
            details.update(cx, |details, cx| details.open_proposals(cx));
            return;
        }
        self.pending_proposals = Some((space_id.clone(), item_id.clone()));
        self.open_work_item_details(space_id, item_id, cx);
    }

    /// Every loaded Space's snapshot, in Outline Space order (the global
    /// Required Actions view).
    fn required_action_spaces(
        &self,
        cx: &gpui::App,
    ) -> Vec<crate::work_outline::required_actions::SpaceSnapshot> {
        self.outline_spaces(cx)
            .into_iter()
            .filter_map(|space| {
                Some(crate::work_outline::required_actions::SpaceSnapshot {
                    snapshot: space.load.snapshot()?.clone(),
                    space_id: space.id,
                    label: space.label,
                })
            })
            .collect()
    }

    /// Open the global Required Actions surface (palette command, shortcut,
    /// Outline toolbar). Work Item Details close first (through the discard
    /// policy) so the surface is visible.
    pub(crate) fn open_required_actions(&mut self, cx: &mut Context<Self>) {
        if self.required_actions_panel.is_none() {
            use crate::work_outline::required_actions_view::{
                RequiredActionsEvent, RequiredActionsList,
            };
            let shell = self.shell.clone();
            let list = cx.new(|cx| {
                let mut list = RequiredActionsList::new(
                    self.state.clone(),
                    crate::work_outline::required_actions::RequestScope::Global,
                    true,
                    cx,
                );
                list.shell = Some(shell);
                list
            });
            let events = cx.subscribe(&list, |this: &mut Self, _, event, cx| match event {
                RequiredActionsEvent::Close => this.close_required_actions(cx),
                RequiredActionsEvent::OpenChat(chat_id) => {
                    this.leave_work_item_details(DetailsExit::OpenChat(chat_id.clone()), cx);
                }
                RequiredActionsEvent::OpenItem { space_id, item_id } => {
                    this.open_work_item_details(space_id.clone(), item_id.clone(), cx);
                }
            });
            self.required_actions_panel = Some(list);
            self._required_actions_events = Some(events);
        }
        if self.work_item_details.is_some() {
            self.leave_work_item_details(DetailsExit::Close, cx);
        }
        cx.notify();
    }

    pub(crate) fn close_required_actions(&mut self, cx: &mut Context<Self>) {
        if self.required_actions_panel.take().is_some() {
            self._required_actions_events = None;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub(crate) fn required_actions_panel(
        &self,
    ) -> Option<Entity<crate::work_outline::required_actions_view::RequiredActionsList>> {
        self.required_actions_panel.clone()
    }

    /// Replace the per-Space outline loads (tests and the offline fixture;
    /// the app reads `AppState::work_outline`).
    pub(crate) fn set_outline_loads(
        &mut self,
        loads: HashMap<String, WorkOutlineLoad>,
        cx: &mut Context<Self>,
    ) {
        self.outline_fixture = Some(loads);
        self.rows_dirty = true;
        self.outline_dirty = true;
        cx.notify();
    }

    /// Offline fixture hook (`examples/outline-fixture.rs`): every Space gets
    /// the store fixture (ids suffixed per Space after the first), and the
    /// Overview switches to Outline.
    #[doc(hidden)]
    pub fn show_work_outline_fixture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut ids: Vec<(String, String)> = self
            .state
            .read(cx)
            .spaces
            .iter()
            .map(|space| (space.display_name().to_lowercase(), space.id.clone()))
            .collect();
        ids.sort();
        let loads = ids
            .into_iter()
            .enumerate()
            .map(|(ix, (_, id))| {
                let snapshot = fixture_snapshot_for(&id, ix);
                (id, WorkOutlineLoad::Ready(std::sync::Arc::new(snapshot)))
            })
            .collect();
        self.set_outline_loads(loads, cx);
        self.set_mode(OverviewMode::Outline, cx);
        let handle = gpui::Focusable::focus_handle(self.outline.read(cx), cx);
        window.focus(&handle, cx);
    }

    /// Outline navigation bindings for fixtures that run without a Shell.
    #[doc(hidden)]
    pub fn work_outline_key_bindings() -> Vec<gpui::KeyBinding> {
        crate::work_outline::outline_view::key_bindings()
    }
}

/// The Chat Toggle Outline Focus returns to, and how it was shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutlineChatReturn {
    pub chat_id: String,
    /// `true`: the full chat route; `false`: the Overview's chat panel.
    pub full_route: bool,
}

/// Where the Overview goes when it leaves Work Item Details.
enum DetailsExit {
    Close,
    OpenChat(String),
    OpenItem { space_id: String, item_id: String },
}

/// The store fixture for the `ix`-th Space: the first verbatim, later ones
/// with every work item and Chat id suffixed `-<ix>` so Spaces never share
/// ids (chat ids are global).
fn fixture_snapshot_for(space_id: &str, ix: usize) -> WorkOutlineSnapshot {
    use crate::work_outline::store::fixture_ids::*;
    let snapshot = crate::work_outline::store::fixture_snapshot(space_id);
    if ix == 0 {
        return snapshot;
    }
    let mut json = serde_json::to_string(&snapshot).unwrap_or_default();
    for id in [
        LAUNCH,
        API,
        API_AUTH,
        UI,
        HOUSEKEEPING,
        RETIRED,
        CHAT_API,
        CHAT_AUTH,
        CHAT_UI,
        CHAT_LOOSE_A,
        CHAT_LOOSE_B,
        CHAT_GHOST,
        CHAT_ORPHAN,
        "w-deleted",
    ] {
        json = json.replace(&format!("\"{id}\""), &format!("\"{id}-{ix}\""));
    }
    serde_json::from_str(&json).unwrap_or(snapshot)
}

#[cfg(test)]
mod logic_tests {
    use super::*;
    use gpui::{Bounds, Pixels};

    fn pr(url: &str) -> ParsedLink {
        ParsedLink {
            kind: LinkKind::Pr,
            value: url.to_string(),
        }
    }

    fn ticket(id: &str) -> ParsedLink {
        ParsedLink {
            kind: LinkKind::Ticket,
            value: id.to_string(),
        }
    }

    #[test]
    fn link_input_parses_github_pr_urls_and_normalizes() {
        let canonical = "https://github.com/cofactr/zeron/pull/123";
        for raw in [
            "https://github.com/cofactr/zeron/pull/123",
            "  https://github.com/cofactr/zeron/pull/123  ",
            "http://github.com/cofactr/zeron/pull/123",
            "github.com/cofactr/zeron/pull/123",
            "https://www.github.com/cofactr/zeron/pull/123",
            "https://github.com/cofactr/zeron/pull/123/files",
            "https://github.com/cofactr/zeron/pull/123?foo=bar",
            "https://github.com/cofactr/zeron/pull/123#discussion_r1",
            "https://GitHub.com/cofactr/zeron/pull/123",
        ] {
            assert_eq!(parse_link_input(raw), Ok(pr(canonical)), "{raw:?}");
        }
    }

    #[test]
    fn link_input_parses_ticket_ids_and_linear_urls() {
        assert_eq!(parse_link_input("ENG-1234"), Ok(ticket("ENG-1234")));
        assert_eq!(parse_link_input(" eng-1234 "), Ok(ticket("ENG-1234")));
        assert_eq!(parse_link_input("ab-1"), Ok(ticket("AB-1")));
        assert_eq!(
            parse_link_input("https://linear.app/cofactr/issue/ENG-42/fix-login"),
            Ok(ticket("ENG-42"))
        );
    }

    #[test]
    fn link_input_rejects_garbage() {
        for raw in [
            "",
            "   ",
            "#123",
            "123",
            "hello world",
            "ENG-",
            "ENG1234",
            "E-1",       // one letter
            "ABCDEFG-1", // seven letters
            "ENG-12a",
            "ENG-1234 extra",
            "https://github.com/cofactr/zeron",
            "https://github.com/cofactr/zeron/issues/5",
            "https://github.com/cofactr/zeron/pull/",
            "https://github.com/cofactr/zeron/pull/12x",
            "https://gitlab.com/cofactr/zeron/pull/1",
            "https://linear.app/cofactr/project/abc",
        ] {
            assert_eq!(parse_link_input(raw), Err(LINK_PARSE_ERROR), "{raw:?}");
        }
    }

    #[test]
    fn link_source_labels_cover_every_provenance() {
        use ChatLinkSource::*;
        assert_eq!(
            link_source_label(LinkKind::Pr, Some(Manual)),
            "Linked manually"
        );
        assert_eq!(
            link_source_label(LinkKind::Ticket, Some(Manual)),
            "Linked manually"
        );
        assert_eq!(
            link_source_label(LinkKind::Pr, Some(CreatedInChat)),
            "PR created in this chat"
        );
        assert_eq!(
            link_source_label(LinkKind::Pr, Some(Mentioned)),
            "Mentioned in conversation"
        );
        assert_eq!(
            link_source_label(LinkKind::Ticket, Some(Mentioned)),
            "Mentioned in conversation"
        );
        assert_eq!(
            link_source_label(LinkKind::Pr, None),
            "Inferred from branch"
        );
        assert_eq!(
            link_source_label(LinkKind::Ticket, None),
            "Inferred from branch"
        );
    }

    /// `CHAT_LINK_STATUS` wire shape with the new source fields — decoded
    /// straight into `ChatLinkStatus` by `ensure_link_status`; snake_case
    /// source values, `null` = inferred, and an older engine that omits them
    /// still decodes.
    #[test]
    fn chat_link_status_decodes_source_fields() {
        let status: OverviewLinkStatus = serde_json::from_value(serde_json::json!({
            "pr": null, "ticket": null, "isWorktree": false, "diffStat": null,
            "prSource": "created_in_chat", "ticketSource": "manual"
        }))
        .unwrap();
        assert_eq!(status.pr_source, Some(ChatLinkSource::CreatedInChat));
        assert_eq!(status.ticket_source, Some(ChatLinkSource::Manual));
        let legacy: OverviewLinkStatus = serde_json::from_value(serde_json::json!({
            "pr": null, "ticket": null, "isWorktree": false, "diffStat": null
        }))
        .unwrap();
        assert_eq!(legacy.pr_source, None);
        assert_eq!(legacy.ticket_source, None);
    }

    #[test]
    fn chat_link_status_prefers_multi_pr_links_and_matches_every_url() {
        let status: OverviewLinkStatus = serde_json::from_value(serde_json::json!({
            "pr": null,
            "ticket": null,
            "prLinks": [
                {
                    "url": "https://github.com/a/b/pull/7",
                    "source": "manual",
                    "detail": null
                },
                {
                    "url": "https://github.com/a/b/pull/8",
                    "source": "mentioned",
                    "detail": null
                }
            ]
        }))
        .unwrap();

        let links = effective_pr_links(&status);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].url, "https://github.com/a/b/pull/7");
        assert_eq!(links[1].url, "https://github.com/a/b/pull/8");
        assert_eq!(link_fingerprint(&status).1.len(), 2);
    }

    #[test]
    fn multi_pr_reply_appends_optimistically_and_removes_only_target() {
        let mut status = OverviewLinkStatus::default();
        let two: SetChatLinkReply = serde_json::from_value(serde_json::json!({
            "linkedPrLinks": [
                {"url": "https://github.com/a/b/pull/7", "source": "manual"},
                {"url": "https://github.com/a/b/pull/8", "source": "manual"}
            ]
        }))
        .unwrap();
        apply_link_reply(&mut status, LinkKind::Pr, &two);
        assert_eq!(effective_pr_links(&status).len(), 2);
        assert_eq!(
            status.pr_links[0].detail.as_ref().map(|pr| pr.number),
            Some(7)
        );

        let one: SetChatLinkReply = serde_json::from_value(serde_json::json!({
            "linkedPrLinks": [
                {"url": "https://github.com/a/b/pull/8", "source": "manual"}
            ]
        }))
        .unwrap();
        apply_link_reply(&mut status, LinkKind::Pr, &one);
        assert_eq!(effective_pr_links(&status).len(), 1);
        assert_eq!(
            effective_pr_links(&status)[0].url,
            "https://github.com/a/b/pull/8"
        );
    }

    #[test]
    fn multi_pr_refetch_keeps_each_pending_placeholder_by_url() {
        let previous = OverviewLinkStatus {
            pr_links: vec![OverviewPrLink {
                url: "https://github.com/a/b/pull/8".into(),
                source: Some(ChatLinkSource::Manual),
                detail: Some(placeholder_pr("https://github.com/a/b/pull/8")),
            }],
            ..Default::default()
        };
        let next = OverviewLinkStatus {
            pr_links: vec![OverviewPrLink {
                url: "https://github.com/a/b/pull/8".into(),
                source: Some(ChatLinkSource::Manual),
                detail: None,
            }],
            ..Default::default()
        };
        let merged = merge_refetched_link(Some(&previous), next);
        assert_eq!(
            merged.pr_links[0].detail.as_ref().map(|pr| pr.number),
            Some(8)
        );
    }

    #[test]
    fn set_chat_link_reply_applies_optimistically_and_merges_refetch() {
        let reply: SetChatLinkReply = serde_json::from_value(serde_json::json!({
            "linkedPrUrl": "https://github.com/a/b/pull/7", "linkedPrSource": "manual",
            "linkedTicketId": null, "linkedTicketSource": null
        }))
        .unwrap();
        let mut status = OverviewLinkStatus::default();
        apply_link_reply(&mut status, LinkKind::Pr, &reply);
        let placeholder = status.pr.as_ref().unwrap();
        assert_eq!(placeholder.number, 7);
        assert!(
            placeholder.action_reasons().is_empty(),
            "placeholder must not badge"
        );
        assert_eq!(status.pr_source, Some(ChatLinkSource::Manual));

        // Refetch lands before the engine cached the PR detail: keep ours.
        let refetched = OverviewLinkStatus {
            pr_source: Some(ChatLinkSource::Manual),
            ..Default::default()
        };
        let merged = merge_refetched_link(Some(&status), refetched);
        assert_eq!(merged.pr.as_ref().map(|p| p.number), Some(7));

        // Ticket set, then cleared.
        let set = SetChatLinkReply {
            linked_ticket_id: Some("ENG-9".into()),
            linked_ticket_source: Some(ChatLinkSource::Manual),
            ..Default::default()
        };
        apply_link_reply(&mut status, LinkKind::Ticket, &set);
        assert_eq!(
            status.ticket.as_ref().map(|t| t.identifier.as_str()),
            Some("ENG-9")
        );
        apply_link_reply(&mut status, LinkKind::Ticket, &SetChatLinkReply::default());
        assert!(status.ticket.is_none());
        assert_eq!(status.ticket_source, None);

        // Cleared (no source) refetch never resurrects the old value.
        let merged = merge_refetched_link(Some(&status), OverviewLinkStatus::default());
        assert!(merged.pr.is_none());
    }

    /// The panel follows the selection and closes itself when it's gone.
    #[test]
    fn panel_chat_id_follows_selection_only_while_open() {
        assert_eq!(panel_chat_id(false, Some("a")), None);
        assert_eq!(panel_chat_id(true, Some("a")), Some("a".to_string()));
        assert_eq!(panel_chat_id(true, None), None);
        assert_eq!(panel_chat_id(true, Some("")), None);
    }

    /// `ensure_subagents` decodes `SCAN_CHAT_SUBAGENTS`'s reply straight into
    /// `SubagentItem`; a key-casing drift would silently yield no rows
    /// (the decode error is swallowed). Payload shape captured verbatim from
    /// a live engine reply, plus the pinned `status` field.
    #[test]
    fn scan_chat_subagents_wire_payload_decodes() {
        let wire = serde_json::json!([
            {
                "agentId": "a21f1650e0d5284f6",
                "description": "Research slash and @ mention semantics",
                "agentType": "fork",
                "transcriptPath": "/p/8de11ea8/subagents/agent-a21f1650e0d5284f6.jsonl",
                "status": "running"
            },
            { "agentId": "b", "transcriptPath": "/p/b.jsonl", "status": "done" },
            // Pre-`status` engine / unknown value: settled default.
            { "agentId": "c", "agentType": "Explore", "transcriptPath": "/p/c.jsonl" },
            { "agentId": "d", "transcriptPath": "/p/d.jsonl", "status": "exploded" }
        ]);
        let decoded: Vec<SubagentItem> = serde_json::from_value(wire).unwrap();
        assert_eq!(decoded.len(), 4);
        assert_eq!(decoded[0].agent_id, "a21f1650e0d5284f6");
        assert_eq!(decoded[0].agent_type.as_deref(), Some("fork"));
        assert_eq!(decoded[0].status, SubagentStatus::Running);
        assert_eq!(
            decoded[0].display_name(),
            "Research slash and @ mention semantics"
        );
        assert_eq!(decoded[1].description, None);
        assert_eq!(decoded[1].status, SubagentStatus::Done);
        // Web name fallback: description || agent_type || id.
        assert_eq!(decoded[1].display_name(), "b");
        assert_eq!(decoded[2].display_name(), "Explore");
        assert_eq!(decoded[2].status, SubagentStatus::Done);
        assert_eq!(decoded[3].status, SubagentStatus::Done);
        assert_eq!(
            SubagentStatus::Running.color(),
            reference_status_color(ChatIndicator::Working)
        );
        assert_eq!(
            SubagentStatus::Done.color(),
            reference_status_color(ChatIndicator::Completed)
        );
    }

    /// The pinned `READ_SUBAGENT_TRANSCRIPT` reply shape decodes, with the
    /// nullable fields (`timestamp`, `resultPreview`, `model`) and missing
    /// optional ones tolerated.
    #[test]
    fn read_subagent_transcript_wire_payload_decodes() {
        let wire = serde_json::json!({
            "turns": [
                {"role": "user", "text": "Find the bug\nin foo.rs", "timestamp": "2026-09-29T10:15:00Z", "tools": []},
                {"role": "assistant", "text": "Looking.", "timestamp": null, "tools": [
                    {"name": "Read", "inputPreview": "foo.rs", "resultPreview": "fn foo() {}"},
                    {"name": "Bash", "inputPreview": "cargo test", "resultPreview": null}
                ]},
                {"role": "assistant", "text": "", "timestamp": null, "tools": []}
            ],
            "model": "claude-opus-5-5"
        });
        let t: SubagentTranscript = serde_json::from_value(wire).unwrap();
        assert_eq!(t.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(t.turns.len(), 3);
        assert_eq!(t.turns[0].role, "user");
        assert_eq!(t.turns[0].text.as_deref(), Some("Find the bug\nin foo.rs"));
        assert_eq!(
            t.turns[0].timestamp.as_deref(),
            Some("2026-09-29T10:15:00Z")
        );
        assert!(!format_turn_time(t.turns[0].timestamp.as_deref()).is_empty());
        assert_eq!(t.turns[1].timestamp, None);
        assert_eq!(format_turn_time(None), "");
        assert_eq!(format_turn_time(Some("not a time")), "");
        assert_eq!(
            t.turns[1].tools,
            vec![
                TranscriptTool {
                    name: "Read".into(),
                    input_preview: "foo.rs".into(),
                    result_preview: Some("fn foo() {}".into())
                },
                TranscriptTool {
                    name: "Bash".into(),
                    input_preview: "cargo test".into(),
                    result_preview: None
                },
            ]
        );
        // `model: null` and an empty object both decode.
        let t: SubagentTranscript =
            serde_json::from_value(serde_json::json!({"turns": [], "model": null})).unwrap();
        assert_eq!(t, SubagentTranscript::default());
        let t: SubagentTranscript = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(t.turns.is_empty());
        assert_eq!(turn_role_label("user"), "You");
        assert_eq!(turn_role_label("assistant"), "Assistant");
    }

    #[test]
    fn peek_reply_guard_drops_stale_and_closed_requests() {
        let peek = |request_id| SubagentPeek {
            chat_id: "c".into(),
            agent_id: "a".into(),
            name: "n".into(),
            agent_type: None,
            request_id,
            load: PeekLoad::Loading,
            open_tools: HashSet::new(),
        };
        // The current request lands.
        assert!(peek_reply_is_current(Some(&peek(2)), 2));
        // An earlier request superseded by a quicker click (id 1 → 2) is dropped.
        assert!(!peek_reply_is_current(Some(&peek(2)), 1));
        // Closed since: nothing to land into.
        assert!(!peek_reply_is_current(None, 2));
    }

    #[test]
    fn fenced_code_splits_out_of_plain_text() {
        assert_eq!(
            split_fenced_code("just text\nline 2"),
            vec![(false, "just text\nline 2".to_string())]
        );
        assert_eq!(
            split_fenced_code("before\n```rust\nfn a() {}\n```\nafter"),
            vec![
                (false, "before".to_string()),
                (true, "fn a() {}".to_string()),
                (false, "after".to_string()),
            ]
        );
        // Unterminated fence runs to the end.
        assert_eq!(
            split_fenced_code("```\nx\ny"),
            vec![(true, "x\ny".to_string())]
        );
        assert!(split_fenced_code("").is_empty());
    }

    #[test]
    fn repo_key_matches_reference_repo_group_key() {
        assert_eq!(repo_key(Some("/Users/m/Projects/zeron")), "zeron");
        assert_eq!(repo_key(Some("/Users/m/Projects/zeron/")), "zeron");
        assert_eq!(repo_key(Some("/Users/m/Projects/zeron///")), "zeron");
        assert_eq!(repo_key(Some("relative")), "relative");
        assert_eq!(repo_key(Some("/")), overview_grouping::NONE_KEY);
        assert_eq!(repo_key(Some("")), overview_grouping::NONE_KEY);
        assert_eq!(repo_key(None), overview_grouping::NONE_KEY);
    }

    #[test]
    fn repo_key_does_not_probe_privacy_managed_history() {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .expect("test requires HOME");
        for child in ["Music", "Pictures", "Documents"] {
            let cwd = home.join(child).join("historical-chat");
            assert!(workspace_root_of(cwd.to_str().unwrap()).is_none());
            assert_eq!(repo_key(cwd.to_str()), "historical-chat");
        }
        assert!(!mast_engine::repos::is_automatic_access_blocked(
            &home.join("Projects").join("zeron")
        ));
    }

    #[test]
    fn repo_key_resolves_worktree_via_workspace_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".workspace-root"),
            "/Users/m/Projects/agent-mode-tools\n",
        )
        .unwrap();
        let cwd = dir.path().to_str().unwrap();
        assert_eq!(repo_key(Some(cwd)), "agent-mode-tools");

        // Trailing slash on the recorded root is stripped like a cwd's.
        std::fs::write(
            dir.path().join(".workspace-root"),
            "/Users/m/Projects/zeron/\n",
        )
        .unwrap();
        assert_eq!(repo_key(Some(cwd)), "zeron");
    }

    #[test]
    fn repo_key_workspace_root_empty_or_missing_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_str().unwrap();
        let basename = dir.path().file_name().unwrap().to_str().unwrap();

        // No file: unchanged behavior.
        assert_eq!(repo_key(Some(cwd)), basename);

        // Empty / whitespace-only file: fallback.
        std::fs::write(dir.path().join(".workspace-root"), "").unwrap();
        assert_eq!(repo_key(Some(cwd)), basename);
        std::fs::write(dir.path().join(".workspace-root"), "  \n").unwrap();
        assert_eq!(repo_key(Some(cwd)), basename);

        // Unreadable (a directory, not a file): fallback, no panic.
        std::fs::remove_file(dir.path().join(".workspace-root")).unwrap();
        std::fs::create_dir(dir.path().join(".workspace-root")).unwrap();
        assert_eq!(repo_key(Some(cwd)), basename);
    }

    #[test]
    fn worktree_repo_key_uses_owning_space_without_workspace_root_marker() {
        use chrono::TimeZone;

        let space = Space {
            id: "workspace-space".into(),
            device_id: "device".into(),
            path: "/Users/mikey/Projects/cofactr/workspace".into(),
            name: None,
            git_detected: true,
            git_checked_at: None,
            checkout_id: Some("main-checkout".into()),
            created_at: Utc.timestamp_opt(0, 0).unwrap(),
        };
        let mut chat = row(
            "chat",
            "/Users/mikey/.zeron/worktrees/workspace/swift-otter",
            None,
        )
        .chat;
        chat.space_id = Some(space.id.clone());

        assert_eq!(repo_key_for_chat(&chat, Some(&space)), "workspace");
    }

    #[test]
    fn owning_space_does_not_change_ordinary_cwd_repo_key() {
        use chrono::TimeZone;

        let space = Space {
            id: "workspace-space".into(),
            device_id: "device".into(),
            path: "/Users/mikey/Projects/cofactr/workspace/".into(),
            name: Some("Custom project name".into()),
            git_detected: true,
            git_checked_at: None,
            checkout_id: Some("main-checkout".into()),
            created_at: Utc.timestamp_opt(0, 0).unwrap(),
        };
        let chat = row("chat", "/Users/mikey/Projects/cofactr/workspace", None).chat;

        assert_eq!(repo_key_for_chat(&chat, Some(&space)), "workspace");
        assert_eq!(repo_key_for_chat(&chat, None), "workspace");
    }

    #[test]
    fn ticket_group_key_prefers_ticket_then_pr_then_none() {
        assert_eq!(ticket_group_key(Some("ENG-1"), Some(7)), "ENG-1");
        assert_eq!(ticket_group_key(None, Some(7)), "PR #7");
        assert_eq!(ticket_group_key(None, None), overview_grouping::NONE_KEY);
    }

    #[test]
    fn repo_filter_toggle_all_none_and_label() {
        let known: BTreeSet<String> = ["ui", "zeron", overview_grouping::NONE_KEY]
            .into_iter()
            .map(String::from)
            .collect();
        let mut hidden = BTreeSet::new();
        assert_eq!(repo_filter_label(hidden.len()), "Repos");

        toggle_repo_hidden(&mut hidden, "ui");
        assert!(hidden.contains("ui"));
        assert_eq!(repo_filter_label(hidden.len()), "Repos (1 hidden)");
        toggle_repo_hidden(&mut hidden, "ui");
        assert!(hidden.is_empty());

        // "None" hides every known repo; "All" clears.
        hidden.extend(known.iter().cloned());
        assert_eq!(repo_filter_label(hidden.len()), "Repos (3 hidden)");
        hidden.clear();
        assert_eq!(repo_filter_label(hidden.len()), "Repos");
    }

    #[test]
    fn repo_list_is_alphabetical_with_none_last() {
        let known: BTreeSet<String> = [
            overview_grouping::NONE_KEY,
            "zeron",
            "agent-mode-tools",
            "ui",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(
            sorted_repo_keys(&known),
            vec![
                "agent-mode-tools",
                "ui",
                "zeron",
                overview_grouping::NONE_KEY
            ]
        );
    }

    #[test]
    fn search_is_case_insensitive_substring_over_present_fields() {
        let fields = [
            Some("Fix Login Bug"),
            None,
            Some("/Users/m/ui"),
            Some("ENG-42"),
        ];
        assert!(matches_search("", &fields));
        assert!(matches_search("login", &fields));
        assert!(matches_search("eng-42", &fields));
        assert!(matches_search("users/m", &fields));
        assert!(!matches_search("zeron", &fields));
        assert!(!matches_search("x", &[None, None]));
    }

    #[test]
    fn hovered_row_reconciliation_drops_only_unmounted_ids() {
        let mounted = vec!["a".to_string(), "b".to_string()];
        let mut hovered = Some("b".to_string());
        assert!(!reconcile_hovered_row(&mut hovered, &mounted));
        assert_eq!(hovered.as_deref(), Some("b"));

        let mounted = vec!["a".to_string()];
        assert!(reconcile_hovered_row(&mut hovered, &mounted));
        assert!(hovered.is_none());
        assert!(!reconcile_hovered_row(&mut hovered, &[]));
    }

    #[test]
    fn view_prefs_round_trip_through_the_flag_set_and_keep_other_flags() {
        let prefs = ViewPrefs {
            mode: OverviewMode::Outline,
            hide_stale: true,
            show_archived: true,
            groups: vec![GroupDimension::Ticket, GroupDimension::Repo],
        };
        let mut flags: BTreeSet<String> = ["someFutureFlag".to_string()].into();
        apply_view_prefs_to_flags(&mut flags, &prefs);
        assert_eq!(
            view_prefs_from_flags(&flags),
            prefs,
            "activation order survives"
        );
        assert!(flags.contains("groupBy:ticket,repo"));
        // Unrelated flags survive and don't read as view prefs.
        assert!(flags.contains("someFutureFlag"));

        // Turning everything off removes every view-pref entry.
        apply_view_prefs_to_flags(&mut flags, &ViewPrefs::default());
        assert_eq!(view_prefs_from_flags(&flags), ViewPrefs::default());
        assert!(!flags.iter().any(|f| f.starts_with(GROUP_BY_PREFIX)));
        // Changing the grouping replaces (never accumulates) the groupBy entry.
        let mut flags = BTreeSet::new();
        apply_view_prefs_to_flags(
            &mut flags,
            &ViewPrefs {
                groups: vec![GroupDimension::Repo],
                ..Default::default()
            },
        );
        apply_view_prefs_to_flags(
            &mut flags,
            &ViewPrefs {
                groups: vec![GroupDimension::Ticket],
                ..Default::default()
            },
        );
        assert_eq!(flags, BTreeSet::from(["groupBy:ticket".to_string()]));
    }

    /// A saved `groupBy` written while Category and Origin grouping existed
    /// still loads: the removed dimensions are dropped, the rest kept.
    #[test]
    fn saved_groupings_with_removed_dimensions_load_without_them() {
        let load = |value: &str| {
            view_prefs_from_flags(&BTreeSet::from([format!("{GROUP_BY_PREFIX}{value}")])).groups
        };
        assert_eq!(load("category,repo"), vec![GroupDimension::Repo]);
        assert_eq!(load("category,origin,repo"), vec![GroupDimension::Repo]);
        assert_eq!(load("origin"), Vec::<GroupDimension>::new());
        assert_eq!(
            load("origin,ticket,category,repo"),
            vec![GroupDimension::Ticket, GroupDimension::Repo]
        );
    }

    #[test]
    fn view_prefs_defaults_and_junk_degrade_gracefully() {
        assert_eq!(
            view_prefs_from_flags(&BTreeSet::new()),
            ViewPrefs::default()
        );
        let junk: BTreeSet<String> = [
            "groupBy:bogus,repo,repo,,Origin, ticket".to_string(),
            "viewMode:myPrs".to_string(),
        ]
        .into();
        let prefs = view_prefs_from_flags(&junk);
        // Unknown dropped, duplicate dropped, whitespace trimmed, case-sensitive keys.
        assert_eq!(
            prefs.groups,
            vec![GroupDimension::Repo, GroupDimension::Ticket]
        );
        assert_eq!(prefs.mode, OverviewMode::List);
        // Every dimension's key round-trips.
        for dim in [GroupDimension::Repo, GroupDimension::Ticket] {
            assert_eq!(
                GroupDimension::from_persist_key(dim.persist_key()),
                Some(dim)
            );
        }
    }

    /// Overview-mode persistence: the removed Canvas view's legacy flag loads
    /// as Outline, exactly one flag is ever written (never the legacy one),
    /// and unknown/conflicting flags degrade deterministically.
    #[test]
    fn overview_mode_loads_a_saved_canvas_view_as_outline_and_writes_exactly_one() {
        let flags =
            |keys: &[&str]| -> BTreeSet<String> { keys.iter().map(|k| k.to_string()).collect() };
        assert_eq!(OverviewMode::from_flags(&flags(&[])), OverviewMode::List);
        // The removed Canvas view: alone, or next to the outline flag.
        assert_eq!(
            OverviewMode::from_flags(&flags(&["viewMode:canvas"])),
            OverviewMode::Outline
        );
        assert_eq!(
            OverviewMode::from_flags(&flags(&["viewMode:outline", "viewMode:canvas"])),
            OverviewMode::Outline
        );
        assert_eq!(
            OverviewMode::from_flags(&flags(&["viewMode:outline"])),
            OverviewMode::Outline
        );
        // Unknown / stale values read as List, never fail.
        assert_eq!(
            OverviewMode::from_flags(&flags(&["viewMode:myPrs", "viewMode:board"])),
            OverviewMode::List
        );
        // The writer purges every `viewMode:` entry and writes one.
        for mode in [OverviewMode::List, OverviewMode::Outline] {
            let mut set = flags(&[
                "viewMode:canvas",
                "viewMode:outline",
                "viewMode:myPrs",
                "hideStale",
                "prSidebarOpen",
            ]);
            apply_view_prefs_to_flags(
                &mut set,
                &ViewPrefs {
                    mode,
                    hide_stale: true,
                    ..Default::default()
                },
            );
            let modes: Vec<&String> = set
                .iter()
                .filter(|f| f.starts_with(VIEW_MODE_PREFIX))
                .collect();
            assert_eq!(
                modes.len(),
                usize::from(mode != OverviewMode::List),
                "{mode:?}: {set:?}"
            );
            assert!(!set.contains(LEGACY_VIEW_CANVAS_FLAG));
            assert_eq!(view_prefs_from_flags(&set).mode, mode);
            assert!(set.contains("prSidebarOpen"), "unrelated flags survive");
        }
        // Re-saving a loaded Canvas view writes only the outline flag.
        for saved in [
            &["viewMode:canvas", "hideStale"][..],
            &["viewMode:canvas", "viewMode:outline", "hideStale"][..],
        ] {
            let mut set = flags(saved);
            let prefs = view_prefs_from_flags(&set);
            assert_eq!(prefs.mode, OverviewMode::Outline);
            apply_view_prefs_to_flags(&mut set, &prefs);
            assert_eq!(set, flags(&["viewMode:outline", "hideStale"]), "{saved:?}");
        }
    }

    /// A flags file written while the "My PRs" sidebar existed still loads:
    /// the retired `prSidebarOpen` / `prSectionCollapsed:*` entries are
    /// ignored and the surviving flags keep their values.
    #[test]
    fn flags_file_with_retired_pr_sidebar_entries_still_loads() {
        let dir =
            std::env::temp_dir().join(format!("zeron-overview-oldflags-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(overview_positions::UI_FLAGS_FILE),
            r#"["prSidebarOpen","prSectionCollapsed:needs-review","prSectionCollapsed:repo:cofactr/zeron","viewMode:outline","hideStale"]"#,
        )
        .unwrap();
        let loaded = overview_positions::load_string_set(&dir, overview_positions::UI_FLAGS_FILE);
        assert_eq!(loaded.len(), 5, "the old file loads without error");
        let prefs = view_prefs_from_flags(&loaded);
        assert_eq!(prefs.mode, OverviewMode::Outline);
        assert!(prefs.hide_stale);
        assert!(!prefs.show_archived);
        // The retired entries are left in place, not rewritten.
        let mut rewritten = loaded.clone();
        apply_view_prefs_to_flags(&mut rewritten, &prefs);
        assert!(rewritten.contains("prSidebarOpen"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn view_prefs_round_trip_through_the_string_set_file() {
        let dir =
            std::env::temp_dir().join(format!("zeron-overview-viewprefs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let prefs = ViewPrefs {
            mode: OverviewMode::Outline,
            hide_stale: false,
            show_archived: true,
            groups: vec![GroupDimension::Ticket, GroupDimension::Repo],
        };
        let mut flags =
            overview_positions::load_string_set(&dir, overview_positions::UI_FLAGS_FILE);
        apply_view_prefs_to_flags(&mut flags, &prefs);
        overview_positions::save_string_set(&dir, overview_positions::UI_FLAGS_FILE, &flags)
            .unwrap();
        let loaded = overview_positions::load_string_set(&dir, overview_positions::UI_FLAGS_FILE);
        assert_eq!(view_prefs_from_flags(&loaded), prefs);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_repo_filter_detection() {
        let set =
            |keys: &[&str]| -> BTreeSet<String> { keys.iter().map(|k| k.to_string()).collect() };
        // No chats known yet: says nothing.
        assert!(!restored_repo_filter_hides_everything(
            &set(&["a"]),
            &set(&[])
        ));
        // Names only repos that no longer have chats: hides nothing.
        assert!(!restored_repo_filter_hides_everything(
            &set(&["gone"]),
            &set(&["a", "b"])
        ));
        // Partial hide is a legitimate filter.
        assert!(!restored_repo_filter_hides_everything(
            &set(&["a"]),
            &set(&["a", "b"])
        ));
        // Hides every repo that has chats.
        assert!(restored_repo_filter_hides_everything(
            &set(&["a", "b", "gone"]),
            &set(&["a", "b"])
        ));
        // Nothing hidden.
        assert!(!restored_repo_filter_hides_everything(
            &set(&[]),
            &set(&["a"])
        ));
    }

    #[test]
    fn chats_become_stale_only_after_a_full_week_of_inactivity() {
        let now = Utc::now();
        let mut chat = row("chat", "/repo", None).chat;
        chat.last_message_at = Some(now - chrono::Duration::days(7));
        assert!(!is_stale(&chat, ChatIndicator::Idle, now));

        chat.last_message_at = Some(now - chrono::Duration::days(7) - chrono::Duration::seconds(1));
        assert!(is_stale(&chat, ChatIndicator::Idle, now));
        assert!(!is_stale(&chat, ChatIndicator::Working, now));
        assert!(!is_stale(&chat, ChatIndicator::AwaitingInput, now));
    }

    /// Build an `Overview` window over `state` (seeded with chats in the
    /// given repos) and render once so `ensure_prefs_loaded` runs.
    fn overview_over<'a>(
        cx: &'a mut gpui::TestAppContext,
        state: &Entity<AppState>,
    ) -> (Entity<Overview>, &'a mut gpui::VisualTestContext) {
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
        let state = state.clone();
        let (overview, vcx) = cx.add_window_view(move |_, cx| {
            Overview::new(
                state,
                gpui::WeakEntity::new_invalid(),
                transcript,
                composer,
                cx,
            )
        });
        vcx.run_until_parked();
        (overview, vcx)
    }

    #[gpui::test]
    fn find_focuses_and_selects_the_search_in_every_mode(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with_repos(cx, dir.path(), &["alpha"]);
        let (overview, vcx) = overview_over(cx, &state);
        for mode in [OverviewMode::List, OverviewMode::Outline] {
            overview.update(vcx, |overview, cx| {
                overview.view_mode = mode;
                overview
                    .search
                    .update(cx, |search, cx| search.set_text("old query", cx));
                cx.notify();
            });
            vcx.update(|window, cx| {
                window.blur();
                window.draw(cx).clear();
            });

            vcx.update(|window, cx| {
                assert!(overview.update(cx, |overview, cx| overview.focus_search(window, cx)));
                assert!(overview.read(cx).search_focused(window, cx), "{mode:?}");
            });
            vcx.simulate_keystrokes("replacement");
            overview.read_with(vcx, |overview, cx| {
                assert_eq!(overview.search.read(cx).text(), "replacement", "{mode:?}");
            });
        }
    }

    fn state_with_repos(
        cx: &mut gpui::TestAppContext,
        dir: &std::path::Path,
        repos: &[&str],
    ) -> Entity<AppState> {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let state = cx.new(|_| AppState::new());
        let repos: Vec<String> = repos.iter().map(|r| format!("/work/{r}")).collect();
        let dir = dir.to_path_buf();
        state.update(cx, |state, _| {
            state.data_dir = Some(dir);
            state.chats = repos
                .iter()
                .enumerate()
                .map(|(i, cwd)| {
                    let mut chat = row(&format!("c{i}"), cwd, None).chat;
                    chat.last_message_at = Some(Utc::now());
                    chat
                })
                .collect();
        });
        state
    }

    /// Reload simulation: one overview changes every persisted piece of view
    /// state through the real handlers, a brand-new overview over the same
    /// data dir (app restart, or the page being rebuilt) restores all of it.
    #[gpui::test]
    fn view_state_survives_a_fresh_overview(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with_repos(cx, dir.path(), &["alpha", "beta"]);
        let (first, vcx) = overview_over(cx, &state);
        first.update(vcx, |o, cx| {
            assert!(o.prefs_loaded, "data_dir was available at first render");
            o.view_mode = OverviewMode::Outline;
            o.view_prefs_changed(cx);
            o.hide_stale = true;
            o.view_prefs_changed(cx);
            o.show_archived = true;
            o.view_prefs_changed(cx);
            o.toggle_group(GroupDimension::Repo, cx);
            o.toggle_group(GroupDimension::Ticket, cx);
            o.toggle_group(GroupDimension::Repo, cx); // off again
            toggle_repo_hidden(&mut o.hidden_repos, "beta");
            o.repo_filter_changed(cx);
        });

        let (second, vcx) = overview_over(cx, &state);
        second.update(vcx, |o, _| {
            assert_eq!(o.view_mode, OverviewMode::Outline);
            assert!(o.hide_stale);
            assert!(o.show_archived);
            assert_eq!(o.active_groups, vec![GroupDimension::Ticket]);
            assert_eq!(o.hidden_repos, BTreeSet::from(["beta".to_string()]));
        });

        // And turning things back off persists too.
        second.update(vcx, |o, cx| {
            o.view_mode = OverviewMode::List;
            o.view_prefs_changed(cx);
            o.hide_stale = false;
            o.view_prefs_changed(cx);
            o.show_archived = false;
            o.view_prefs_changed(cx);
            o.toggle_group(GroupDimension::Ticket, cx);
        });
        let (third, vcx) = overview_over(cx, &state);
        third.update(vcx, |o, _| {
            assert_eq!(o.view_prefs(), ViewPrefs::default());
            assert_eq!(o.hidden_repos, BTreeSet::from(["beta".to_string()]));
        });
    }

    /// A restored repo filter that hides every repo with chats is reset (and
    /// the reset persisted); a filter naming only vanished repos is inert; a
    /// partial filter is kept.
    #[gpui::test]
    fn clean_frames_share_the_cached_rows(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with_repos(cx, dir.path(), &["alpha", "beta"]);
        let (overview, vcx) = overview_over(cx, &state);
        overview.update(vcx, |o, cx| {
            let first = o.rows(cx);
            let second = o.rows(cx);
            assert!(
                std::rc::Rc::ptr_eq(&first, &second),
                "hover/scroll frames must not rebuild or deep-clone rows"
            );
            o.rows_dirty = true;
            assert!(!std::rc::Rc::ptr_eq(&first, &o.rows(cx)));
        });
    }

    /// Synthetic busy session: 250 chats, one of them streaming. Counts what
    /// the Overview does per `AppState` change; prints the counts (run with
    /// `--nocapture`) and pins the post-fix bounds.
    #[gpui::test]
    fn streaming_one_chat_among_many_does_little_overview_work(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let names: Vec<String> = (0..250).map(|i| format!("repo{i}")).collect();
        let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let state = state_with_repos(cx, dir.path(), &name_refs);
        let (overview, vcx) = overview_over(cx, &state);
        let draw = |vcx: &mut gpui::VisualTestContext| {
            vcx.update(|window, cx| {
                window.draw(cx).clear();
            });
        };
        draw(vcx);
        // Steady state: every chat's fetches have already landed (no engine
        // here to do it for real).
        overview.update(vcx, |o, _| {
            for name in 0..250 {
                let id = format!("c{name}");
                o.link_status.insert(
                    id.clone(),
                    LinkStatusEntry {
                        status: OverviewLinkStatus::default(),
                        diff_stat: None,
                        is_worktree: None,
                        fetched_at: Instant::now(),
                    },
                );
                o.context_usage_fetched_at
                    .insert(id.clone(), Instant::now());
                o.subagents_fetched_at.insert(id, Instant::now());
            }
            o.rows_dirty = true;
        });
        draw(vcx);
        let before = overview.read_with(vcx, |o, _| o.perf);
        let started = std::time::Instant::now();
        for i in 0..50 {
            // A streaming frame: the selected chat's transcript changes, no
            // chat/space/session field does.
            state.update(vcx, |state, cx| {
                state.selected_chat = Some("c0".into());
                state.transcript.clear();
                state.queue.clear();
                let _ = i;
                cx.notify();
            });
            vcx.run_until_parked();
            draw(vcx);
        }
        let elapsed = started.elapsed();
        let after = overview.read_with(vcx, |o, _| o.perf);
        let delta = OverviewPerf {
            rows_rebuilds: after.rows_rebuilds - before.rows_rebuilds,
            observer_calls: after.observer_calls - before.observer_calls,
            observer_notifies: after.observer_notifies - before.observer_notifies,
            fetch_checks: after.fetch_checks - before.fetch_checks,
            fetch_requests: after.fetch_requests - before.fetch_requests,
        };
        eprintln!("PERF 250 chats x 50 streaming updates: {delta:?} in {elapsed:?}");
        // Before the fingerprint gate, the row-list gate and the fetch-due
        // gate: 50 rebuilds, 50 notifies, 12500 fetch checks. Only the first
        // update (it changes the selection) may rebuild or repaint.
        assert_eq!(delta.observer_calls, 50);
        assert!(delta.rows_rebuilds <= 1, "{delta:?}");
        assert!(delta.observer_notifies <= 1, "{delta:?}");
        assert_eq!(delta.fetch_checks, 0, "{delta:?}");

        // A real change to the streaming chat's row still gets through.
        state.update(vcx, |state, cx| {
            state.chats[0].last_message_preview = Some("new preview".into());
            cx.notify();
        });
        vcx.run_until_parked();
        draw(vcx);
        let rows = overview.update(vcx, |o, cx| o.rows(cx));
        let row = rows.iter().find(|r| r.chat.id == "c0").unwrap();
        assert_eq!(
            row.chat.last_message_preview.as_deref(),
            Some("new preview")
        );
    }

    #[gpui::test]
    fn restored_stale_repo_filter_degrades_gracefully(cx: &mut gpui::TestAppContext) {
        let save = |dir: &std::path::Path, keys: &[&str]| {
            let set: BTreeSet<String> = keys.iter().map(|k| k.to_string()).collect();
            overview_positions::save_string_set(dir, overview_positions::HIDDEN_REPOS_FILE, &set)
                .unwrap();
        };

        // Hides every repo that has chats (plus one that vanished) -> reset.
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &["alpha", "beta", "vanished"]);
        let state = state_with_repos(cx, dir.path(), &["alpha", "beta"]);
        let (overview, vcx) = overview_over(cx, &state);
        overview.update(vcx, |o, cx| {
            let rows = o.rows(cx);
            assert_eq!(rows.len(), 2, "nothing is filtered out invisibly");
            assert!(o.hidden_repos.is_empty());
        });
        assert!(
            overview_positions::load_string_set(dir.path(), overview_positions::HIDDEN_REPOS_FILE)
                .is_empty()
        );

        // Only a vanished repo: no crash, nothing hidden, entry left alone.
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &["vanished"]);
        let state = state_with_repos(cx, dir.path(), &["alpha"]);
        let (overview, vcx) = overview_over(cx, &state);
        overview.update(vcx, |o, cx| {
            assert_eq!(o.rows(cx).len(), 1);
            assert_eq!(o.hidden_repos, BTreeSet::from(["vanished".to_string()]));
        });

        // Partial filter stays.
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &["alpha"]);
        let state = state_with_repos(cx, dir.path(), &["alpha", "beta"]);
        let (overview, vcx) = overview_over(cx, &state);
        overview.update(vcx, |o, cx| {
            let rows = o.rows(cx);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].repo, "beta");
            assert_eq!(o.hidden_repos, BTreeSet::from(["alpha".to_string()]));
        });

        // A deliberate "None" made AFTER the restore is not undone.
        overview.update(vcx, |o, cx| {
            let known = o.known_repos.clone();
            o.hidden_repos.extend(known);
            o.repo_filter_changed(cx);
            assert_eq!(o.rows(cx).len(), 0);
            assert_eq!(o.hidden_repos.len(), 2);
        });
    }

    #[gpui::test]
    fn embedded_chat_panel_exposes_and_copies_chat_identity(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| {
            let mut chat = row("chat-7", "/r", None).chat;
            chat.title = Some("Fix login".into());
            chat.harness_session_id = Some("sess-9".into());
            chat.last_message_at = Some(Utc::now());
            state.chats = vec![chat];
            state.selected_chat = Some("chat-7".into());
        });
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
        let (overview, vcx) = cx.add_window_view(|_, cx| {
            Overview::new(
                state.clone(),
                gpui::WeakEntity::new_invalid(),
                transcript,
                composer,
                cx,
            )
        });
        overview.update(vcx, |overview, cx| {
            overview.chat_panel_open = true;
            let theme = Theme::of(cx).clone();
            assert!(overview.render_chat_panel(&theme, cx).is_some());
            overview.copy_panel_chat_identity("chat-7".into(), cx);
            cx.notify();
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
        assert!(
            vcx.debug_bounds("chat-header-metadata").is_some(),
            "embedded chat panel mounts the shared metadata strip"
        );
        overview.read_with(vcx, |overview, cx| {
            assert!(overview.chat_panel_open);
            assert_eq!(
                overview.state.read(cx).selected_chat.as_deref(),
                Some("chat-7")
            );
            assert_eq!(overview.copied_panel_identity.as_deref(), Some("chat-7"));
        });
        assert_eq!(
            vcx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("Fix login \u{2014} chat chat-7 \u{2014} session sess-9")
        );
    }

    #[test]
    fn acknowledged_done_resets_when_status_leaves_done() {
        let mut ack: BTreeSet<String> = ["a".to_string()].into();
        assert!(!is_unread_done(&ack, "a", true));
        assert!(is_unread_done(&ack, "b", true));
        assert!(!is_unread_done(&ack, "b", false));
        // Still done: nothing to reconcile.
        assert!(!reconcile_acknowledged(&mut ack, "a", true));
        assert!(ack.contains("a"));
        // Left done: dropped, so the next completion is unread again.
        assert!(reconcile_acknowledged(&mut ack, "a", false));
        assert!(!ack.contains("a"));
        assert!(is_unread_done(&ack, "a", true));
        // Not in the set: no change reported (no redundant save).
        assert!(!reconcile_acknowledged(&mut ack, "a", false));
    }

    #[test]
    fn status_style_maps_zeron_indicators_to_reference_vocabulary() {
        assert_eq!(
            status_style(ChatIndicator::AwaitingInput),
            ("●", "needs input")
        );
        assert_eq!(status_style(ChatIndicator::Completed), ("✓", "done"));
        assert_eq!(status_style(ChatIndicator::Idle), ("–", "idle"));
        assert_eq!(status_style(ChatIndicator::Working), ("●", "working"));
        assert_eq!(status_style(ChatIndicator::Errored), ("✕", "error"));
    }

    #[test]
    fn worktree_cwds_match_reference_rules() {
        assert!(cwd_is_worktree(Some("/r/ws/.worktrees/eng-1")));
        assert!(cwd_is_worktree(Some("/r/ws/.agent-worktrees/x/y")));
        assert!(!cwd_is_worktree(Some("/r/ws")));
        assert!(!cwd_is_worktree(None));
    }

    #[test]
    fn group_labels_use_reference_meta_tables() {
        assert_eq!(
            group_label(GroupDimension::Repo, overview_grouping::NONE_KEY),
            "No repo"
        );
        assert_eq!(
            group_label(GroupDimension::Ticket, overview_grouping::NONE_KEY),
            "No ticket / PR"
        );
        assert_eq!(group_label(GroupDimension::Repo, "zeron"), "zeron");
    }

    #[test]
    fn context_label_and_fill_match_reference_face() {
        assert_eq!(context_pct_label(None), "— context");
        assert_eq!(context_pct_label(Some(0.0)), "0% context");
        assert_eq!(context_pct_label(Some(0.426)), "43% context");
        assert_eq!(context_pct_label(Some(1.0)), "100% context");
        assert_eq!(context_fill_fraction(None), 0.0);
        assert_eq!(context_fill_fraction(Some(0.426)), 0.43);
        assert_eq!(context_fill_fraction(Some(2.0)), 1.0);
    }

    #[test]
    fn context_refetch_only_counts_as_changed_when_the_percent_moves() {
        // First fetch: null is no change, a value is.
        assert!(!context_pct_changed(None, None));
        assert!(context_pct_changed(None, Some(0.3)));
        // Same value, or sub-percent drift: no-op (no relayout).
        assert!(!context_pct_changed(Some(Some(0.3)), Some(0.3)));
        assert!(!context_pct_changed(Some(Some(0.301)), Some(0.304)));
        // Crossing a whole percent, or going to/from null: changed.
        assert!(context_pct_changed(Some(Some(0.30)), Some(0.31)));
        assert!(context_pct_changed(Some(Some(0.30)), None));
        assert!(!context_pct_changed(Some(None), None));
    }

    fn row(id: &str, cwd: &str, pct: Option<f32>) -> OverviewRow {
        use chrono::TimeZone;
        let chat = Chat {
            id: id.into(),
            device_id: "d".into(),
            title: None,
            archived: false,
            cwd: Some(cwd.into()),
            branch: None,
            checkout_id: None,
            source_context: None,
            config: None,
            last_message_preview: None,
            last_message_at: None,
            created_at: Utc.timestamp_opt(0, 0).unwrap(),
            harness_session_id: None,
            harness_session_cwd: None,
            actual_model: None,
            parent_chat_id: None,
            space_id: None,
            last_seen_at: None,
            room_gen: None,
            linked_pr_url: None,
            linked_pr_source: None,
            linked_ticket_id: None,
            linked_ticket_source: None,
        };
        OverviewRow {
            status: ChatIndicator::Idle,
            repo: repo_key(chat.cwd.as_deref()),
            chat,
            stale: false,
            ticket: None,
            pr_number: None,
            pr_title: None,
            pr_search_values: Vec::new(),
            context_pct: pct,
            subagent_count: 0,
            closeable: false,
            work_item_path: None,
            user_action_required: false,
            user_action_count: 0,
            instruction_drift: false,
            execution_exception: false,
        }
    }

    #[test]
    fn card_model_label_uses_explicit_id_or_honest_default() {
        let mut chat = row("c", "/r/zeron", None).chat;
        assert_eq!(chat_model_label(&chat), "default model");
        chat.config = Some(mast_proto::ChatConfig {
            harness: mast_proto::HarnessId::Codex,
            model: Some("gpt-6-codex".into()),
            reasoning: None,
            model_options: Default::default(),
            sandbox: mast_proto::SandboxLevel::WorkspaceWrite,
            auto_approve: false,
        });
        assert_eq!(chat_model_label(&chat), "gpt-6-codex");
        // What the harness reported wins over the stored pick, and a chat
        // with no stored model shows what ran (the CLI default), not "default".
        chat.actual_model = Some("claude-fable-5".into());
        assert_eq!(chat_model_label(&chat), "claude-fable-5");
        chat.config = None;
        assert_eq!(chat_model_label(&chat), "claude-fable-5");
    }

    #[test]
    fn unread_done_overrides_stale_dimming_but_not_archived() {
        // Fresh unread done, stale unread done: full opacity, ribbon shown.
        assert_eq!(card_dimming(false, false, true), (None, true));
        assert_eq!(card_dimming(false, true, true), (None, true));
        // Acknowledged: normal dimming returns.
        assert_eq!(card_dimming(false, true, false), (Some(0.55), false));
        assert_eq!(card_dimming(false, false, false), (None, false));
        // Archived always dims and never shows the ribbon.
        assert_eq!(card_dimming(true, true, true), (Some(0.4), false));
        assert_eq!(card_dimming(true, false, false), (Some(0.4), false));
    }

    #[gpui::test]
    fn list_multi_pr_and_ticket_badges_stay_on_a_single_line(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| {
            let mut chat = row("c1", "/r", None).chat;
            chat.last_message_at = Some(Utc::now());
            state.chats = vec![chat];
        });
        let status = OverviewLinkStatus {
            ticket: Some(TicketStatus {
                identifier: "ENG-1234".into(),
                title: Some("Compact badges".into()),
                url: None,
                status: Some("In Progress".into()),
            }),
            pr_links: (1..=5)
                .map(|number| OverviewPrLink {
                    url: format!("https://github.com/a/b/pull/{number}"),
                    source: Some(ChatLinkSource::Manual),
                    detail: None,
                })
                .collect(),
            ..Default::default()
        };
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
        let (_overview, vcx) = cx.add_window_view(|_, cx| {
            let mut overview = Overview::new(
                state.clone(),
                gpui::WeakEntity::new_invalid(),
                transcript,
                composer,
                cx,
            );
            overview.view_mode = OverviewMode::List;
            overview.link_status.insert(
                "c1".into(),
                LinkStatusEntry {
                    status,
                    diff_stat: None,
                    is_worktree: None,
                    fetched_at: Instant::now(),
                },
            );
            overview
        });
        vcx.run_until_parked();

        let row = vcx.debug_bounds("overview-list-row").expect("row rendered");
        let badges = vcx
            .debug_bounds("overview-row-badges")
            .expect("badge row rendered");
        assert!(
            (f32::from(badges.size.height) - BADGE_ROW_H).abs() < 0.5,
            "badge row must remain a single line: {badges:?}"
        );
        assert!(
            badges.top() >= row.top() && badges.bottom() <= row.bottom() + px(0.5),
            "single-line badges must fit inside the row: {badges:?} in {row:?}"
        );
        for selector in [
            "overview-badge-pr-c1-0",
            "overview-badge-pr-c1-1",
            "overview-badge-pr-c1-2",
            "overview-badge-pr-c1-3",
            "overview-badge-pr-c1-4",
        ] {
            assert!(
                vcx.debug_bounds(selector).is_some(),
                "{selector} remains in the clipped row"
            );
        }
        assert!(
            vcx.debug_bounds("overview-badge-ticket-c1").is_some(),
            "ticket badge remains in the clipped row"
        );
    }

    /// End to end over the real UI path: a chat with a harness session gets a
    /// `SCAN_CHAT_SUBAGENTS` request from `rows()`, the reply lands in
    /// `subagents`, the row's `subagent_count` follows, and List mode lays the
    /// subagent block out INSIDE the row's bounds (not clipped past its
    /// bottom edge by the `overflow_hidden` row).
    #[gpui::test]
    fn list_row_renders_scanned_subagent_rows_inside_its_bounds(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let (out, mut requests) = tokio::sync::mpsc::channel::<String>(64);
        let (replies, inbound) = tokio::sync::mpsc::channel::<String>(64);
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| {
            state.set_test_engine(crate::state::EngineHandle::from_test_client(
                mast_rpc::RpcClient::new(out, inbound),
            ));
            let mut chat = row("c1", "/r", None).chat;
            chat.harness_session_id = Some("s1".into());
            chat.last_message_at = Some(Utc::now());
            state.chats = vec![chat];
        });
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
        let (overview, vcx) = cx.add_window_view(|_, cx| {
            let mut overview = Overview::new(
                state.clone(),
                gpui::WeakEntity::new_invalid(),
                transcript,
                composer,
                cx,
            );
            overview.view_mode = OverviewMode::List;
            overview
        });
        vcx.run_until_parked();

        // Answer the scan (the other per-row lookups just fail — irrelevant here).
        let mut scanned = false;
        while let Ok(frame) = requests.try_recv() {
            let request: serde_json::Value = serde_json::from_str(&frame).unwrap();
            let reply = if request["method"] == methods::SCAN_CHAT_SUBAGENTS {
                assert_eq!(request["params"]["chatId"], "c1");
                scanned = true;
                serde_json::json!({"id": request["id"], "ok": [
                    {"agentId": "a", "description": "Research", "agentType": "fork", "transcriptPath": "/a"},
                    {"agentId": "b", "transcriptPath": "/b"},
                    {"agentId": "c", "transcriptPath": "/c"}
                ]})
            } else {
                serde_json::json!({"id": request["id"], "err": "unavailable"})
            };
            runtime.block_on(async {
                replies.send(reply.to_string()).await.unwrap();
                while replies.capacity() < replies.max_capacity() {
                    tokio::task::yield_now().await;
                }
            });
        }
        assert!(
            scanned,
            "rows() must request SCAN_CHAT_SUBAGENTS for a chat with a harness session"
        );
        vcx.run_until_parked();

        overview.update(vcx, |overview, cx| {
            assert_eq!(overview.subagents.get("c1").map(Vec::len), Some(3));
            assert_eq!(overview.rows(cx)[0].subagent_count, 3);
        });
        let row = vcx.debug_bounds("overview-list-row").expect("row rendered");
        let block = vcx
            .debug_bounds("overview-list-subagents")
            .expect("subagent rows rendered in the row");
        assert!(
            block.top() >= row.top() && block.bottom() <= row.bottom(),
            "subagent block {block:?} must sit inside row {row:?}"
        );
        // Exactly three agents need no remainder row.
        assert!(
            f32::from(block.size.height) >= 3.0 * SUBAGENT_TILE_H + 2.0 * SUBAGENT_TILE_GAP - 0.5
        );
    }

    /// Collapsed/hovered subagent stack + the peek, end to end on the List:
    /// a 36-subagent parent rests at three rows plus an indicator, expands to
    /// every compact row on hover, and clicking one opens the read-only
    /// transcript panel WITHOUT toggling the parent's expand, issues
    /// `READ_SUBAGENT_TRANSCRIPT` for that `{chatId, agentId}`, shows a
    /// loading state until the reply lands, then renders the turns; Escape
    /// closes it back to the bare list.
    #[gpui::test]
    fn list_subagent_row_click_opens_readonly_peek(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let (out, mut requests) = tokio::sync::mpsc::channel::<String>(256);
        let (replies, inbound) = tokio::sync::mpsc::channel::<String>(256);
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| {
            state.set_test_engine(crate::state::EngineHandle::from_test_client(
                mast_rpc::RpcClient::new(out, inbound),
            ));
            let mut chat = row("c1", "/r", None).chat;
            chat.harness_session_id = Some("s1".into());
            chat.last_message_at = Some(Utc::now());
            state.chats = vec![chat];
        });
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
        let (overview, vcx) = cx.add_window_view(|_, cx| {
            let mut overview = Overview::new(
                state.clone(),
                gpui::WeakEntity::new_invalid(),
                transcript,
                composer,
                cx,
            );
            overview.view_mode = OverviewMode::List;
            overview
        });
        vcx.run_until_parked();

        let subagents: Vec<serde_json::Value> = (0..36)
            .map(|i| {
                serde_json::json!({
                    "agentId": format!("a{i}"),
                    "description": format!("Subagent task {i}"),
                    "agentType": "Explore",
                    "transcriptPath": format!("/a{i}"),
                    "status": if i == 0 { "running" } else { "done" }
                })
            })
            .collect();
        // Answers every pending request; returns the transcript requests seen.
        let answer = |requests: &mut tokio::sync::mpsc::Receiver<String>| {
            let mut transcript_requests = Vec::new();
            while let Ok(frame) = requests.try_recv() {
                let request: serde_json::Value = serde_json::from_str(&frame).unwrap();
                let reply = if request["method"] == methods::SCAN_CHAT_SUBAGENTS {
                    serde_json::json!({"id": request["id"], "ok": subagents})
                } else if request["method"] == methods::READ_SUBAGENT_TRANSCRIPT {
                    transcript_requests.push(request["params"].clone());
                    serde_json::json!({"id": request["id"], "ok": {
                        "turns": [
                            {"role": "user", "text": "Go look", "timestamp": null, "tools": []},
                            {"role": "assistant", "text": "Found it", "timestamp": null, "tools": [
                                {"name": "Grep", "inputPreview": "foo", "resultPreview": "bar.rs:1"}
                            ]}
                        ],
                        "model": "m"
                    }})
                } else {
                    serde_json::json!({"id": request["id"], "err": "unavailable"})
                };
                runtime.block_on(async {
                    replies.send(reply.to_string()).await.unwrap();
                    while replies.capacity() < replies.max_capacity() {
                        tokio::task::yield_now().await;
                    }
                });
            }
            transcript_requests
        };
        answer(&mut requests);
        vcx.run_until_parked();

        overview.update(vcx, |overview, cx| {
            assert_eq!(overview.subagents.get("c1").map(Vec::len), Some(36));
            let row = &overview.rows(cx)[0];
            assert_eq!(row.subagent_count, 36);
        });
        vcx.run_until_parked();
        let row = vcx.debug_bounds("overview-list-row").expect("row rendered");
        assert!(
            vcx.debug_bounds("overview-card-model").is_some(),
            "row renders model metadata"
        );
        let block = vcx
            .debug_bounds("overview-list-subagents")
            .expect("subagent stack rendered");
        assert!(
            vcx.debug_bounds("overview-subagent-more").is_some(),
            "resting row shows remainder row"
        );
        assert!(
            block.top() >= row.top() && block.bottom() <= row.bottom() + px(0.5),
            "preview stack {block:?} must fit inside the resting row {row:?}"
        );
        // The block carries the row's 8px bottom padding.
        let stack = |rows: f32| rows * SUBAGENT_TILE_H + (rows - 1.0) * SUBAGENT_TILE_GAP + 8.0;
        assert!(
            (f32::from(block.size.height) - stack((SUBAGENT_PREVIEW_LIMIT + 1) as f32)).abs() < 1.0,
            "preview rows laid out at their fixed height: {block:?}"
        );

        // Hovering anywhere on the row replaces the indicator with the full
        // list.
        vcx.simulate_mouse_move(row.center(), None, gpui::Modifiers::default());
        vcx.run_until_parked();
        overview.update(vcx, |overview, _| {
            assert_eq!(overview.hovered_row.as_deref(), Some("c1"));
        });
        assert!(vcx.debug_bounds("overview-subagent-more").is_none());
        let block = vcx
            .debug_bounds("overview-list-subagents")
            .expect("expanded subagent stack rendered");
        assert!(
            (f32::from(block.size.height) - stack(36.0)).abs() < 1.0,
            "every hovered row laid out at its fixed height: {block:?}"
        );

        // Click the first compact row.
        let first = gpui::point(
            block.left() + px(30.0),
            block.top() + px(SUBAGENT_TILE_H / 2.0),
        );
        vcx.simulate_click(first, gpui::Modifiers::none());
        vcx.run_until_parked();
        overview.update(vcx, |overview, _| {
            let peek = overview.subagent_peek.as_ref().expect("peek opened");
            assert_eq!(
                (peek.chat_id.as_str(), peek.agent_id.as_str()),
                ("c1", "a0")
            );
            assert_eq!(peek.load, PeekLoad::Loading);
            assert!(!overview.chat_panel_open);
            assert!(
                !overview.expanded.contains("c1"),
                "subagent click must not toggle the parent"
            );
        });
        assert!(
            vcx.debug_bounds("overview-subagent-peek").is_some(),
            "peek panel rendered"
        );

        let transcript_requests = answer(&mut requests);
        assert_eq!(
            transcript_requests,
            vec![serde_json::json!({"chatId": "c1", "agentId": "a0"})]
        );
        vcx.run_until_parked();
        overview.update(vcx, |overview, _| {
            match &overview.subagent_peek.as_ref().expect("still open").load {
                PeekLoad::Loaded(t) => assert_eq!(t.turns.len(), 2),
                other => panic!("expected loaded, got {other:?}"),
            }
        });

        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        overview.update(vcx, |overview, _| {
            assert!(overview.subagent_peek.is_none(), "Escape closes the peek");
        });
    }

    /// List mode with more rows than fit on screen: each row keeps its full
    /// natural height (subagent rows included) and the list scrolls. The
    /// row root is `overflow_hidden` for the unread ribbon, which zeroes its
    /// flex auto-min-height, so without `flex_none` the scrolling column
    /// squeezed every row (live repro: a 64px row down to 11.5px) and clipped
    /// the subagent block away.
    #[gpui::test]
    fn list_rows_keep_subagent_rows_when_the_list_overflows(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let state = cx.new(|_| AppState::new());
        let ids: Vec<String> = (0..60).map(|i| format!("c{i:02}")).collect();
        state.update(cx, |state, _| {
            state.chats = ids
                .iter()
                .map(|id| {
                    let mut chat = row(id, "/r", None).chat;
                    chat.last_message_at = Some(Utc::now());
                    chat
                })
                .collect();
        });
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
        let (_overview, vcx) = cx.add_window_view(|_, cx| {
            let mut overview = Overview::new(
                state.clone(),
                gpui::WeakEntity::new_invalid(),
                transcript,
                composer,
                cx,
            );
            overview.view_mode = OverviewMode::List;
            for id in &ids {
                overview.subagents.insert(
                    id.clone(),
                    serde_json::from_value(serde_json::json!([
                        {"agentId": "a", "description": "Research", "transcriptPath": "/a"},
                        {"agentId": "b", "transcriptPath": "/b"},
                        {"agentId": "c", "transcriptPath": "/c"}
                    ]))
                    .unwrap(),
                );
                overview
                    .subagents_fetched_at
                    .insert(id.clone(), Instant::now());
            }
            overview
        });
        vcx.run_until_parked();
        let row = vcx
            .debug_bounds("overview-list-row")
            .expect("list row rendered");
        let block = vcx
            .debug_bounds("overview-list-subagents")
            .expect("subagent rows rendered");
        assert!(
            f32::from(block.size.height) >= 3.0 * SUBAGENT_TILE_H,
            "all three subagent rows laid out: {block:?}"
        );
        assert!(
            f32::from(row.size.height) > f32::from(block.size.height),
            "row {row:?} must not be squeezed below its subagent block {block:?}"
        );
    }

    // ----------------------------------------------------------------------
    // Work Outline presentation (Milestone 1)
    // ----------------------------------------------------------------------

    use crate::work_outline::model::row_key;
    use crate::work_outline::store::fixture_ids::*;

    /// An Overview over one Space `s1` whose chats match the store fixture's
    /// live chat ids, with the fixture installed as its outline.
    fn outline_overview<'a>(
        cx: &'a mut gpui::TestAppContext,
        dir: &std::path::Path,
    ) -> (
        Entity<Overview>,
        Entity<AppState>,
        &'a mut gpui::VisualTestContext,
    ) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let state = cx.new(|_| AppState::new());
        let dir = dir.to_path_buf();
        state.update(cx, |state, _| {
            state.data_dir = Some(dir);
            state.spaces = vec![
                serde_json::from_value(serde_json::json!({
                    "id": "s1", "deviceId": "d", "path": "/work/alpha",
                    "createdAt": "2026-01-01T00:00:00Z"
                }))
                .unwrap(),
            ];
            state.chats = [
                CHAT_API,
                CHAT_AUTH,
                CHAT_UI,
                CHAT_LOOSE_A,
                CHAT_LOOSE_B,
                CHAT_ORPHAN,
            ]
            .iter()
            .map(|id| {
                let mut chat = row(id, "/work/alpha", None).chat;
                chat.space_id = Some("s1".into());
                chat.title = Some(format!("Chat {id}"));
                chat.last_message_at = Some(Utc::now());
                chat
            })
            .collect();
        });
        let (overview, vcx) = overview_over(cx, &state);
        overview.update(vcx, |o, cx| o.set_outline_loads(fixture_loads(None), cx));
        (overview, state, vcx)
    }

    fn fixture_loads(snapshot: Option<WorkOutlineSnapshot>) -> HashMap<String, WorkOutlineLoad> {
        let snapshot =
            snapshot.unwrap_or_else(|| crate::work_outline::store::fixture_snapshot("s1"));
        HashMap::from([(
            "s1".to_string(),
            WorkOutlineLoad::Ready(std::sync::Arc::new(snapshot)),
        )])
    }

    /// `debug_bounds` takes a `'static` selector; leak the (test-only) string.
    fn row_bounds(vcx: &mut gpui::VisualTestContext, selector: String) -> Option<Bounds<Pixels>> {
        vcx.debug_bounds(Box::leak(selector.into_boxed_str()))
    }

    fn draw(vcx: &mut gpui::VisualTestContext) {
        vcx.update(|window, cx| window.draw(cx).clear());
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
    }

    #[gpui::test]
    fn outline_mode_survives_a_fresh_overview(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (first, state, vcx) = outline_overview(cx, dir.path());
        first.update(vcx, |o, cx| o.set_mode(OverviewMode::Outline, cx));
        let flags =
            overview_positions::load_string_set(dir.path(), overview_positions::UI_FLAGS_FILE);
        assert!(flags.contains(VIEW_OUTLINE_FLAG));
        assert!(!flags.contains(LEGACY_VIEW_CANVAS_FLAG));
        let (second, vcx) = overview_over(cx, &state);
        second.update(vcx, |o, _| assert_eq!(o.mode(), OverviewMode::Outline));
    }

    /// Regression: the command can reach an Overview that has never rendered
    /// (so its saved prefs are not loaded yet). Switching to Outline persists
    /// the in-memory prefs, which must not overwrite the saved file with
    /// defaults.
    #[gpui::test]
    fn open_outline_command_on_an_unrendered_overview_keeps_saved_prefs(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with_repos(cx, dir.path(), &["alpha"]);
        let mut flags = BTreeSet::new();
        apply_view_prefs_to_flags(
            &mut flags,
            &ViewPrefs {
                mode: OverviewMode::List,
                hide_stale: true,
                show_archived: true,
                groups: vec![GroupDimension::Repo],
            },
        );
        overview_positions::save_string_set(dir.path(), overview_positions::UI_FLAGS_FILE, &flags)
            .unwrap();

        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
        let overview = {
            let state = state.clone();
            cx.new(move |cx| {
                Overview::new(
                    state,
                    gpui::WeakEntity::new_invalid(),
                    transcript,
                    composer,
                    cx,
                )
            })
        };
        overview.read_with(cx, |o, _| assert!(!o.prefs_loaded, "no render yet"));
        let (_, vcx) = cx.add_window_view(|_, _| gpui::Empty);
        vcx.update(|window, cx| {
            overview.update(cx, |o, cx| {
                o.run_work_outline_command(WorkOutlineCommand::OpenOutline, window, cx)
            })
        });
        overview.read_with(vcx, |o, _| {
            assert_eq!(o.mode(), OverviewMode::Outline);
            assert!(o.hide_stale);
            assert!(o.show_archived);
            assert_eq!(o.active_groups, vec![GroupDimension::Repo]);
        });
        let saved =
            overview_positions::load_string_set(dir.path(), overview_positions::UI_FLAGS_FILE);
        assert!(saved.contains(VIEW_OUTLINE_FLAG));
        assert!(saved.contains(HIDE_STALE_FLAG));
        assert!(saved.contains(SHOW_ARCHIVED_FLAG));
        assert!(saved.iter().any(|f| f.starts_with(GROUP_BY_PREFIX)));
    }

    #[gpui::test]
    fn outline_renders_compact_rows_with_stable_selectors(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, _state, vcx) = outline_overview(cx, dir.path());
        overview.update(vcx, |o, cx| o.set_mode(OverviewMode::Outline, cx));
        draw(vcx);
        for key in [
            row_key::item("s1", LAUNCH),
            // w-api-auth's only Chat is folded into its row.
            row_key::item("s1", API_AUTH),
            row_key::chat(CHAT_GHOST),
            row_key::unassociated(Some("s1")),
            row_key::chat(CHAT_LOOSE_A),
        ] {
            assert!(
                row_bounds(vcx, format!("outline-row-{key}")).is_some(),
                "{key} renders"
            );
        }
        assert!(row_bounds(vcx, format!("outline-row-{}", row_key::chat(CHAT_AUTH))).is_none());
        // Archived items stay out by default.
        assert!(row_bounds(vcx, format!("outline-row-{}", row_key::item("s1", RETIRED))).is_none());
        let row = row_bounds(vcx, format!("outline-row-{}", row_key::item("s1", API))).unwrap();
        assert!(
            f32::from(row.size.height) <= 28.0,
            "one compact line: {row:?}"
        );
    }

    /// The "Updated since you last viewed" dot shows in the Details list and on
    /// the Outline row after a new revision, and clears once the last-viewed
    /// record catches up (what opening the artifact does).
    #[gpui::test]
    fn changed_marker_shows_in_details_and_outline_and_clears(cx: &mut gpui::TestAppContext) {
        use crate::artifacts::tests::{artifact, id, snapshot as artifacts_snapshot};
        use mast_proto::ArtifactKind;
        let dir = tempfile::tempdir().unwrap();
        let (overview, state, vcx) = outline_overview(cx, dir.path());
        vcx.update(|_, cx| {
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx)
        });
        let viewed = |vcx: &mut gpui::VisualTestContext, entries: &[(u8, u32)]| {
            let entries: Vec<(String, u32)> = entries.iter().map(|(n, r)| (id(*n), *r)).collect();
            vcx.update(|_, cx| {
                crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
                    settings.artifact_last_viewed = entries.into_iter().collect();
                });
                cx.refresh_windows();
            });
        };
        let mut plan = artifact(&id(1), "Plan", ArtifactKind::Markdown);
        plan.pinned_as_plan = true;
        let flow = artifact(&id(2), "Flow", ArtifactKind::HtmlFragment);
        let notes = artifact(&id(3), "Notes", ArtifactKind::Markdown);
        state.update(vcx, |state, _| {
            state.local_device_id = Some("d".into());
            state.set_test_artifacts(artifacts_snapshot(vec![
                plan.clone(),
                flow.clone(),
                notes.clone(),
            ]));
        });
        // The plan and the notes were seen at r2 (current); the flow at r1.
        viewed(vcx, &[(1, 2), (2, 1), (3, 2)]);
        overview.update(vcx, |o, cx| {
            o.set_outline_loads(
                fixture_loads(Some(crate::work_outline::store::fixture_snapshot("s1"))),
                cx,
            );
            o.set_mode(OverviewMode::Outline, cx);
        });
        let _details = open_details(&overview, vcx, API);
        let outline = format!("outline-artifacts-changed-{}", row_key::item("s1", API));
        let marked = |vcx: &mut gpui::VisualTestContext, n: u8| {
            row_bounds(vcx, format!("work-item-artifact-changed-{}", id(n))).is_some()
        };
        assert!(!marked(vcx, 1) && marked(vcx, 2) && !marked(vcx, 3));
        assert!(row_bounds(vcx, outline.clone()).is_some());

        // Opening the flow records r2: every list clears.
        viewed(vcx, &[(1, 2), (2, 2), (3, 2)]);
        draw(vcx);
        assert!(!marked(vcx, 2));
        assert!(row_bounds(vcx, outline.clone()).is_none());

        // A new revision of the plan brings the marker back.
        let mut plan = plan;
        plan.current_revision = 3;
        state.update(vcx, |state, _| {
            state.set_test_artifacts(artifacts_snapshot(vec![plan, flow, notes]));
        });
        draw(vcx);
        assert!(marked(vcx, 1) && !marked(vcx, 2));
        assert!(row_bounds(vcx, outline).is_some());
    }

    /// Planning artifacts: Details lists the pinned plan first (with the
    /// item's Linear project beside it), then newest first; the row menu pins;
    /// "Register existing" sends `ArtifactRegister` for the item; the Outline
    /// row shows the artifact count.
    #[gpui::test]
    fn work_item_details_list_artifacts_and_the_outline_counts_them(cx: &mut gpui::TestAppContext) {
        use crate::artifacts::tests::{artifact, id, scripted, snapshot as artifacts_snapshot};
        use mast_proto::ArtifactKind;
        let dir = tempfile::tempdir().unwrap();
        let (overview, state, vcx) = outline_overview(cx, dir.path());
        let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
        snapshot
            .external_references
            .push(mast_proto::WorkItemExternalReferenceView {
                id: "ref-project".into(),
                space_id: "s1".into(),
                work_item_id: API.into(),
                provider: mast_proto::linear::LINEAR_PROVIDER.into(),
                reference_type: mast_proto::linear::LINEAR_PROJECT_REFERENCE.into(),
                external_id: "p1".into(),
                url: "https://linear.app/acme/project/receiving-1".into(),
                cached_title: Some("Receiving".into()),
                created_by: None,
                created_at: 1,
            });
        let mut plan = artifact(&id(1), "Plan", ArtifactKind::Markdown);
        plan.pinned_as_plan = true;
        plan.updated_at = 1;
        let mut newer = artifact(&id(2), "Flow", ArtifactKind::HtmlFragment);
        newer.updated_at = 30;
        let mut older = artifact(&id(3), "Notes", ArtifactKind::Markdown);
        older.updated_at = 10;
        let (actions, calls) = scripted(std::sync::Arc::new(|method, _| {
            Ok(match method {
                mast_rpc::methods::ARTIFACT_REGISTER => serde_json::json!({
                    "artifactId": id(4), "revision": 1, "url": "mast-artifact:x",
                    "storedPath": "/work/alpha/.zeron/artifacts/api/plan.md", "changed": true
                }),
                _ => serde_json::json!({ "artifactId": id(1), "pinned": true, "unpinned": [] }),
            })
        }));
        state.update(vcx, |state, _| {
            state.local_device_id = Some("d".into());
            state.work_outline_actions_override = Some(actions);
            state.set_test_artifacts(artifacts_snapshot(vec![older, newer, plan]));
        });
        overview.update(vcx, |o, cx| {
            o.set_outline_loads(fixture_loads(Some(snapshot)), cx);
            o.set_mode(OverviewMode::Outline, cx);
        });
        draw(vcx);
        assert!(
            row_bounds(
                vcx,
                format!("outline-artifacts-{}", row_key::item("s1", API))
            )
            .is_some(),
            "the Outline row counts the item's artifacts"
        );
        assert!(
            row_bounds(
                vcx,
                format!("outline-artifacts-{}", row_key::item("s1", UI))
            )
            .is_none()
        );

        let details = open_details(&overview, vcx, API);
        let y = |vcx: &mut gpui::VisualTestContext, n: u8| {
            row_bounds(vcx, format!("work-item-artifact-{}", id(n)))
                .unwrap_or_else(|| panic!("artifact {n} row"))
                .origin
                .y
        };
        let (plan_y, newer_y, older_y) = (y(vcx, 1), y(vcx, 2), y(vcx, 3));
        assert!(
            plan_y < newer_y && newer_y < older_y,
            "plan, then newest first"
        );
        let chip = vcx
            .debug_bounds("work-item-artifact-plan-linear")
            .expect("Linear project next to the plan");
        assert!((f32::from(chip.center().y) - f32::from(plan_y)).abs() < 24.0);

        // Right-click a row: its menu pins it.
        let row = row_bounds(vcx, format!("work-item-artifact-{}", id(2))).unwrap();
        vcx.simulate_mouse_down(
            row.center(),
            gpui::MouseButton::Right,
            gpui::Modifiers::default(),
        );
        vcx.run_until_parked();
        draw(vcx);
        let pin = vcx
            .debug_bounds("work-item-artifact-menu-pin")
            .expect("menu");
        vcx.simulate_click(pin.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
        let sent = |method: &str| {
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(m, _)| m == method)
                .map(|(_, params)| params.clone())
                .collect::<Vec<_>>()
        };
        let pins = sent(mast_rpc::methods::ARTIFACT_PIN);
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0]["artifactId"], id(2));
        assert_eq!(pins[0]["pinned"], true);

        // Register existing: outside the Space is refused; inside registers.
        details.update(vcx, |d, cx| d.register_existing("/etc/passwd".into(), cx));
        vcx.run_until_parked();
        assert!(sent(mast_rpc::methods::ARTIFACT_REGISTER).is_empty());
        details.update(vcx, |d, cx| {
            d.register_existing("/work/alpha/work/plan.md".into(), cx)
        });
        vcx.run_until_parked();
        let registers = sent(mast_rpc::methods::ARTIFACT_REGISTER);
        assert_eq!(registers.len(), 1);
        assert_eq!(registers[0]["spaceId"], "s1");
        assert_eq!(registers[0]["workItemId"], API);
        assert_eq!(registers[0]["path"], "/work/alpha/work/plan.md");
        assert!(registers[0].get("targetDeviceId").is_none());
        details.read_with(vcx, |d, _| {
            assert_eq!(d.artifacts.status.as_deref(), Some("Registered as r1"));
        });
        // The register button opens the path field in tests (no native picker).
        draw(vcx);
        let register = vcx.debug_bounds("work-item-artifacts-register").unwrap();
        vcx.simulate_click(register.center(), gpui::Modifiers::default());
        draw(vcx);
        assert!(
            vcx.debug_bounds("work-item-artifacts-register-input")
                .is_some()
        );
    }

    /// Acceptance 30: only the visible slice of a large outline renders.
    #[gpui::test]
    fn large_outlines_render_only_the_visible_slice(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, _state, vcx) = outline_overview(cx, dir.path());
        let (snapshot, _) = crate::work_outline::model::tests::large_snapshot("s1", 10, 100);
        overview.update(vcx, |o, cx| {
            o.set_outline_loads(fixture_loads(Some(snapshot)), cx);
            o.set_mode(OverviewMode::Outline, cx);
        });
        draw(vcx);
        let total = overview.read_with(vcx, |o, cx| o.outline.read(cx).model().len());
        assert!(total > 2_000, "{total}");
        assert!(row_bounds(vcx, format!("outline-row-{}", row_key::item("s1", "r0"))).is_some());
        assert!(row_bounds(vcx, format!("outline-row-{}", row_key::item("s1", "r9c99"))).is_none());
    }

    #[gpui::test]
    fn outline_keyboard_navigation_follows_visual_order_and_sibling_jumps(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, _state, vcx) = outline_overview(cx, dir.path());
        vcx.update(|_, cx| cx.bind_keys(crate::work_outline::outline_view::key_bindings()));
        overview.update(vcx, |o, cx| o.set_mode(OverviewMode::Outline, cx));
        draw(vcx);
        vcx.update(|window, cx| {
            let handle = gpui::Focusable::focus_handle(overview.read(cx).outline.read(cx), cx);
            window.focus(&handle, cx);
        });
        let selected = |vcx: &mut gpui::VisualTestContext| {
            overview.read_with(vcx, |o, cx| {
                o.outline.read(cx).selected_key().map(str::to_string)
            })
        };
        vcx.simulate_keystrokes("down");
        assert_eq!(selected(vcx), Some(row_key::item("s1", LAUNCH)));
        // Down enters the expanded parent; Up returns to it.
        vcx.simulate_keystrokes("down");
        assert_eq!(selected(vcx), Some(row_key::item("s1", API)));
        vcx.simulate_keystrokes("up");
        assert_eq!(selected(vcx), Some(row_key::item("s1", LAUNCH)));
        vcx.simulate_keystrokes("down");
        // Alt+Down jumps to the next sibling, Alt+Up back.
        vcx.simulate_keystrokes("alt-down");
        assert_eq!(selected(vcx), Some(row_key::item("s1", UI)));
        vcx.simulate_keystrokes("alt-up");
        assert_eq!(selected(vcx), Some(row_key::item("s1", API)));
        vcx.simulate_keystrokes("alt-up");
        assert_eq!(selected(vcx), Some(row_key::item("s1", LAUNCH)));
        // Left collapses, a second Left on the root stays put.
        vcx.simulate_keystrokes("left");
        let collapsed = overview.read_with(vcx, |o, cx| {
            o.outline
                .read(cx)
                .model()
                .row(&row_key::item("s1", LAUNCH))
                .unwrap()
                .collapsed
        });
        assert!(collapsed);
        // Focus survives a snapshot refresh.
        overview.update(vcx, |o, cx| o.set_outline_loads(fixture_loads(None), cx));
        draw(vcx);
        assert_eq!(selected(vcx), Some(row_key::item("s1", LAUNCH)));
        vcx.simulate_keystrokes("right");
        vcx.simulate_keystrokes("right");
        assert_eq!(selected(vcx), Some(row_key::item("s1", API)));
    }

    #[gpui::test]
    fn commands_without_a_selection_open_the_outline_with_a_notice(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, _state, vcx) = outline_overview(cx, dir.path());
        assert_eq!(overview.read_with(vcx, |o, _| o.mode()), OverviewMode::List);
        vcx.update(|window, cx| {
            overview.update(cx, |o, cx| {
                o.run_work_outline_command(WorkOutlineCommand::RenameWorkItem, window, cx)
            })
        });
        overview.read_with(vcx, |o, cx| {
            assert_eq!(o.mode(), OverviewMode::Outline);
            assert_eq!(
                o.outline.read(cx).notice(),
                Some("Select a work item first")
            );
        });
        // Create needs no selection: an inline editor row appears.
        vcx.update(|window, cx| {
            overview.update(cx, |o, cx| {
                o.run_work_outline_command(WorkOutlineCommand::CreateWorkItem, window, cx)
            })
        });
        draw(vcx);
        assert!(vcx.debug_bounds("outline-row-editor").is_some());
        // Open Required Actions needs no selection: the global surface opens.
        vcx.update(|window, cx| {
            overview.update(cx, |o, cx| {
                o.run_work_outline_command(WorkOutlineCommand::OpenRequiredActions, window, cx)
            })
        });
        overview.read_with(vcx, |o, _| assert!(o.required_actions_panel.is_some()));
    }

    /// Acceptance 26: details render in the right slot and closing them
    /// restores the chat panel exactly as it was.
    #[gpui::test]
    fn work_item_details_open_and_close_restore_the_previous_layout(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, state, vcx) = outline_overview(cx, dir.path());
        state.update(vcx, |state, _| state.selected_chat = Some(CHAT_API.into()));
        overview.update(vcx, |o, cx| {
            o.chat_panel_open = true;
            o.set_mode(OverviewMode::Outline, cx);
        });
        draw(vcx);
        assert!(vcx.debug_bounds("work-item-details").is_none());
        overview.update(vcx, |o, cx| {
            o.open_work_item_details("s1".into(), API.into(), cx)
        });
        draw(vcx);
        assert!(vcx.debug_bounds("work-item-details").is_some());
        assert!(vcx.debug_bounds("work-item-breadcrumbs").is_some());
        overview.read_with(vcx, |o, cx| {
            assert!(
                o.chat_panel_open,
                "details never touch the chat panel state"
            );
            assert!(o.subagent_peek.is_none());
            assert_eq!(state.read(cx).selected_chat.as_deref(), Some(CHAT_API));
        });
        // Escape inside the details (no draft) closes them.
        overview.update(vcx, |o, cx| {
            let details = o.work_item_details.clone().unwrap();
            details.update(cx, |d, cx| d.request_close(cx));
        });
        draw(vcx);
        overview.read_with(vcx, |o, _| {
            assert!(o.work_item_details.is_none());
            assert!(o.chat_panel_open);
        });
        assert!(vcx.debug_bounds("work-item-details").is_none());
    }

    /// Switching items or opening a Chat with an unsaved draft asks first;
    /// Keep Editing keeps the draft and the item, Discard then switches.
    #[gpui::test]
    fn leaving_details_with_a_draft_asks_before_discarding(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, _state, vcx) = outline_overview(cx, dir.path());
        overview.update(vcx, |o, cx| {
            o.set_mode(OverviewMode::Outline, cx);
            o.open_work_item_details("s1".into(), API.into(), cx);
        });
        draw(vcx);
        let details = overview.read_with(vcx, |o, _| o.work_item_details.clone().unwrap());
        vcx.update(|window, cx| {
            details.update(cx, |d, cx| d.begin_title_draft("API v2", window, cx))
        });
        let shown = |vcx: &mut gpui::VisualTestContext| {
            overview.read_with(vcx, |o, cx| {
                o.work_item_details
                    .as_ref()
                    .map(|d| d.read(cx).item_id.clone())
            })
        };

        // Switch item: the confirmation is requested, nothing switches yet.
        overview.update(vcx, |o, cx| {
            o.open_work_item_details("s1".into(), UI.into(), cx)
        });
        assert_eq!(shown(vcx).as_deref(), Some(API));
        let request = details
            .update(vcx, |d, _| d.captured_confirm.take())
            .expect("asks first");
        assert_eq!(request.confirm_label.as_ref(), "Discard");
        assert_eq!(request.cancel_label.as_ref(), "Keep Editing");
        // Keep Editing: same item, draft intact.
        vcx.update(|window, cx| (request.on_decision)(false, window, cx));
        assert_eq!(shown(vcx).as_deref(), Some(API));
        assert!(details.read_with(vcx, |d, cx| d.has_unsaved_draft(cx)));

        // Opening a Chat asks too; Keep Editing keeps everything.
        overview.update(vcx, |o, cx| {
            o.leave_work_item_details(DetailsExit::OpenChat(CHAT_API.into()), cx)
        });
        let request = details
            .update(vcx, |d, _| d.captured_confirm.take())
            .expect("asks first");
        vcx.update(|window, cx| (request.on_decision)(false, window, cx));
        assert_eq!(shown(vcx).as_deref(), Some(API));
        assert!(!overview.read_with(vcx, |o, _| o.chat_panel_open));

        // Discard: the switch happens.
        overview.update(vcx, |o, cx| {
            o.open_work_item_details("s1".into(), UI.into(), cx)
        });
        let request = details
            .update(vcx, |d, _| d.captured_confirm.take())
            .expect("asks first");
        vcx.update(|window, cx| (request.on_decision)(true, window, cx));
        vcx.run_until_parked();
        assert_eq!(shown(vcx).as_deref(), Some(UI));

        // Without a draft, switching and closing need no confirmation.
        overview.update(vcx, |o, cx| {
            o.open_work_item_details("s1".into(), API.into(), cx)
        });
        assert_eq!(shown(vcx).as_deref(), Some(API));
        let details = overview.read_with(vcx, |o, _| o.work_item_details.clone().unwrap());
        details.update(vcx, |d, cx| d.request_close(cx));
        vcx.run_until_parked();
        assert_eq!(shown(vcx), None);
    }

    #[gpui::test]
    fn list_rows_show_the_work_item_path_and_attention(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, _state, vcx) = outline_overview(cx, dir.path());
        draw(vcx);
        let rows = overview.update(vcx, |o, cx| o.rows(cx));
        let auth = rows.iter().find(|r| r.chat.id == CHAT_AUTH).unwrap();
        assert_eq!(
            auth.work_item_path.as_ref().map(|p| p.full.as_str()),
            Some("Launch v2 › API › Auth endpoints")
        );
        let ui = rows.iter().find(|r| r.chat.id == CHAT_UI).unwrap();
        assert!(ui.user_action_required);
        let loose = rows.iter().find(|r| r.chat.id == CHAT_LOOSE_A).unwrap();
        assert!(loose.work_item_path.is_none());
        assert!(vcx.debug_bounds("work-item-path-chip").is_some());
        assert!(vcx.debug_bounds("work-item-attention").is_some());
    }

    #[gpui::test]
    fn work_item_filter_keeps_subtrees_and_resets_a_stale_restore(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, _state, vcx) = outline_overview(cx, dir.path());
        let ids = |rows: std::rc::Rc<Vec<OverviewRow>>| {
            let mut ids: Vec<String> = rows.iter().map(|r| r.chat.id.clone()).collect();
            ids.sort();
            ids
        };
        overview.update(vcx, |o, cx| {
            o.toggle_work_item_filter(LAUNCH, cx);
            assert_eq!(ids(o.rows(cx)), [CHAT_API, CHAT_AUTH, CHAT_UI]);
            o.toggle_work_item_filter(LAUNCH, cx);
            o.toggle_work_item_filter(work_item_filter::UNASSOCIATED_KEY, cx);
            assert_eq!(ids(o.rows(cx)), [CHAT_LOOSE_A, CHAT_LOOSE_B, CHAT_ORPHAN]);
        });
        assert_eq!(
            work_item_filter::load_filter(dir.path()),
            BTreeSet::from([work_item_filter::UNASSOCIATED_KEY.to_string()])
        );

        // A restored filter naming only a deleted item hides everything:
        // it resets once the outline has loaded.
        let dir = tempfile::tempdir().unwrap();
        work_item_filter::save_filter(dir.path(), &BTreeSet::from(["w-gone".to_string()])).unwrap();
        let (overview, _state, vcx) = outline_overview(cx, dir.path());
        overview.update(vcx, |o, cx| {
            assert_eq!(o.rows(cx).len(), 6, "nothing is filtered out invisibly");
            assert!(o.work_item_filter.is_empty());
        });
        assert!(work_item_filter::load_filter(dir.path()).is_empty());
    }

    /// Execution exceptions: only the exception Chat carries the compact
    /// mark on List rows.
    #[gpui::test]
    fn execution_exception_marks_flag_only_the_exception_chat_on_list_rows(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, _state, vcx) = outline_overview(cx, dir.path());
        let marks = |vcx: &mut gpui::VisualTestContext, prefix: &str| {
            [CHAT_API, CHAT_AUTH, CHAT_UI, CHAT_LOOSE_A]
                .into_iter()
                .filter(|id| row_bounds(vcx, format!("{prefix}{id}")).is_some())
                .collect::<Vec<_>>()
        };
        overview.update(vcx, |o, cx| {
            o.set_outline_loads(fixture_loads(None), cx);
        });
        draw(vcx);
        assert!(marks(vcx, "overview-row-execution-exception-").is_empty());
        overview.update(vcx, |o, cx| {
            o.set_outline_loads(fixture_loads(Some(execution_fixture::snapshot("s1"))), cx);
        });
        draw(vcx);
        assert_eq!(marks(vcx, "overview-row-execution-exception-"), [CHAT_API]);
    }

    /// Drift marks: only drifted Chats carry one, on List rows and the
    /// chat-panel header.
    #[gpui::test]
    fn drift_marks_flag_only_drifted_chats(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, state, vcx) = outline_overview(cx, dir.path());
        let drifted = instruction_fixture::snapshot("s1");
        let mut current = drifted.clone();
        for chat_state in &mut current.chat_instruction_states {
            chat_state.drift = false;
        }
        let marks = |vcx: &mut gpui::VisualTestContext, prefix: &str| {
            [CHAT_API, CHAT_AUTH, CHAT_UI, CHAT_LOOSE_A]
                .into_iter()
                .filter(|id| row_bounds(vcx, format!("{prefix}{id}")).is_some())
                .collect::<Vec<_>>()
        };
        overview.update(vcx, |o, cx| {
            o.set_outline_loads(fixture_loads(Some(current)), cx);
        });
        overview.update(vcx, |o, cx| {
            o.set_outline_loads(fixture_loads(Some(drifted)), cx);
            let rows = o.rows(cx);
            let flagged: Vec<_> = rows
                .iter()
                .filter(|r| r.instruction_drift)
                .map(|r| r.chat.id.as_str())
                .collect();
            assert_eq!(flagged, [CHAT_API]);
        });

        draw(vcx);
        assert_eq!(marks(vcx, "overview-row-instruction-drift-"), [CHAT_API]);

        // The chat-panel header: marked for the drifted Chat only.
        for (chat, expected) in [(CHAT_AUTH, false), (CHAT_API, true)] {
            state.update(vcx, |state, _| state.selected_chat = Some(chat.into()));
            overview.update(vcx, |o, cx| {
                o.chat_panel_open = true;
                cx.notify();
            });
            draw(vcx);
            assert_eq!(
                vcx.debug_bounds("overview-chat-panel-instruction-drift")
                    .is_some(),
                expected,
                "{chat}"
            );
        }
    }

    // ----------------------------------------------------------------------
    // Work Outline instructions (Milestone 2)
    // ----------------------------------------------------------------------

    use crate::work_outline::instructions::fixture as instruction_fixture;

    type OutlineCalls = std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>;

    /// The instruction fixture installed, and a scripted engine that records
    /// every Work Outline call and answers through `reply`.
    fn instruction_overview<'a>(
        cx: &'a mut gpui::TestAppContext,
        dir: &std::path::Path,
        snapshot: WorkOutlineSnapshot,
        reply: impl Fn(&serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError>
        + Send
        + Sync
        + 'static,
    ) -> (
        Entity<Overview>,
        OutlineCalls,
        &'a mut gpui::VisualTestContext,
    ) {
        let (overview, state, vcx) = outline_overview(cx, dir);
        let calls: OutlineCalls = Default::default();
        let sink = calls.clone();
        state.update(vcx, |state, _| {
            state.work_outline_actions_override =
                Some(crate::work_outline::WorkOutlineActions::scripted(
                    std::sync::Arc::new(move |_, params| {
                        sink.lock().unwrap().push(params.clone());
                        reply(&params)
                    }),
                ));
        });
        overview.update(vcx, |o, cx| {
            o.set_outline_loads(fixture_loads(Some(snapshot)), cx);
            o.set_mode(OverviewMode::Outline, cx);
        });
        draw(vcx);
        (overview, calls, vcx)
    }

    fn reply_ok(params: &serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError> {
        Ok(match params["op"].as_str() {
            Some("createWorkItemInstruction") => serde_json::json!({"instructionId": "i-new"}),
            _ => serde_json::json!({}),
        })
    }

    fn open_details(
        overview: &Entity<Overview>,
        vcx: &mut gpui::VisualTestContext,
        item: &str,
    ) -> Entity<WorkItemDetails> {
        overview.update(vcx, |o, cx| {
            o.open_work_item_details("s1".into(), item.into(), cx)
        });
        vcx.run_until_parked();
        draw(vcx);
        overview.read_with(vcx, |o, _| o.work_item_details.clone().unwrap())
    }

    fn ops(calls: &OutlineCalls) -> Vec<String> {
        calls
            .lock()
            .unwrap()
            .iter()
            .map(|c| c["op"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    /// Acceptance 28: an instruction is saved only by Save; Cancel discards;
    /// closing or switching with an unsaved draft asks first and never saves.
    #[gpui::test]
    fn instruction_drafts_save_only_on_save_and_ask_before_discarding(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, calls, vcx) = instruction_overview(
            cx,
            dir.path(),
            instruction_fixture::snapshot("s1"),
            reply_ok,
        );
        let details = open_details(&overview, vcx, UI);
        // Collapsed until opened.
        assert!(vcx.debug_bounds("work-item-instructions").is_some());
        assert!(vcx.debug_bounds("work-item-instruction-add").is_none());
        details.update(vcx, |d, cx| {
            d.instructions_open = true;
            cx.notify();
        });
        draw(vcx);
        assert!(vcx.debug_bounds("work-item-instruction-add").is_some());

        // New draft, then Cancel: nothing is written, nothing asked.
        vcx.update(|window, cx| {
            details.update(cx, |d, cx| {
                d.begin_instruction_draft(None, window, cx);
                d.set_instruction_text("Keep the UI accessible.", cx);
            })
        });
        draw(vcx);
        assert!(vcx.debug_bounds("work-item-instruction-save").is_some());
        let cancel = vcx
            .debug_bounds("work-item-instruction-cancel")
            .unwrap()
            .center();
        vcx.simulate_click(cancel, gpui::Modifiers::default());
        draw(vcx);
        details.read_with(vcx, |d, _| {
            assert!(d.instruction_draft.is_none());
            assert!(d.captured_confirm.is_none());
        });
        assert!(calls.lock().unwrap().is_empty());

        // Draft, then close: asks; Keep Editing keeps it; no write.
        vcx.update(|window, cx| {
            details.update(cx, |d, cx| {
                d.begin_instruction_draft(None, window, cx);
                d.set_instruction_text("Keep the UI accessible.", cx);
            })
        });
        // Blur (focus moves to the outline) never saves.
        vcx.update(|window, cx| {
            let outline = overview.read(cx).outline.clone();
            let handle = gpui::Focusable::focus_handle(outline.read(cx), cx);
            window.focus(&handle, cx);
        });
        draw(vcx);
        assert!(calls.lock().unwrap().is_empty(), "blur does not save");
        details.update(vcx, |d, cx| d.request_close(cx));
        let request = details
            .update(vcx, |d, _| d.captured_confirm.take())
            .expect("closing with a draft asks");
        assert_eq!(
            request.body.as_ref(),
            "Your instruction edits have not been saved."
        );
        vcx.update(|window, cx| (request.on_decision)(false, window, cx));
        vcx.run_until_parked();
        assert!(overview.read_with(vcx, |o, _| o.work_item_details.is_some()));
        assert!(details.read_with(vcx, |d, cx| d.has_unsaved_draft(cx)));

        // Switching items asks too; Discard switches and drops the draft.
        overview.update(vcx, |o, cx| {
            o.open_work_item_details("s1".into(), API.into(), cx)
        });
        let request = details
            .update(vcx, |d, _| d.captured_confirm.take())
            .expect("switching with a draft asks");
        vcx.update(|window, cx| (request.on_decision)(true, window, cx));
        vcx.run_until_parked();
        let shown = overview.read_with(vcx, |o, cx| {
            o.work_item_details
                .as_ref()
                .unwrap()
                .read(cx)
                .item_id
                .clone()
        });
        assert_eq!(shown, API);
        assert!(calls.lock().unwrap().is_empty(), "discard never saves");

        // Save (explicit) writes once, then closes the editor.
        let details = overview.read_with(vcx, |o, _| o.work_item_details.clone().unwrap());
        vcx.update(|window, cx| {
            details.update(cx, |d, cx| {
                d.begin_instruction_draft(None, window, cx);
                d.set_instruction_text("Version every endpoint.", cx);
            })
        });
        draw(vcx);
        let save = vcx
            .debug_bounds("work-item-instruction-save")
            .unwrap()
            .center();
        vcx.simulate_click(save, gpui::Modifiers::default());
        vcx.run_until_parked();
        let recorded = calls.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0]["op"], "createWorkItemInstruction");
        assert_eq!(recorded[0]["scope"]["workItemId"], API);
        assert_eq!(recorded[0]["text"], "Version every endpoint.");
        details.read_with(vcx, |d, _| assert!(d.instruction_draft.is_none()));
    }

    /// Editing an inherited instruction that changes assigned Chats asks for
    /// the drift confirmation before writing; Cancel writes nothing.
    #[gpui::test]
    fn editing_and_archiving_inherited_instructions_confirm_drift(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, calls, vcx) = instruction_overview(
            cx,
            dir.path(),
            instruction_fixture::snapshot("s1"),
            reply_ok,
        );
        let details = open_details(&overview, vcx, LAUNCH);
        vcx.update(|window, cx| {
            details.update(cx, |d, cx| {
                d.begin_instruction_draft(Some(instruction_fixture::I_LAUNCH.into()), window, cx);
                d.set_instruction_text("Ship behind the beta flag.", cx);
                d.save_instruction(cx);
            })
        });
        let request = details
            .update(vcx, |d, _| d.captured_drift.take())
            .expect("drift confirmation");
        // chat-api, chat-auth, chat-ui sit below Launch.
        assert_eq!(request.impact.chat_ids.len(), 3);
        assert_eq!(request.impact.item_ids.len(), 4);
        vcx.update(|window, cx| (request.on_decision)(false, window, cx));
        vcx.run_until_parked();
        assert!(calls.lock().unwrap().is_empty());
        details.update(vcx, |d, cx| d.save_instruction(cx));
        let request = details.update(vcx, |d, _| d.captured_drift.take()).unwrap();
        vcx.update(|window, cx| (request.on_decision)(true, window, cx));
        vcx.run_until_parked();
        assert_eq!(ops(&calls), ["updateWorkItemInstruction"]);
        assert_eq!(
            calls.lock().unwrap()[0]["instructionId"],
            instruction_fixture::I_LAUNCH
        );

        // Archiving the Space-root instruction from the Space-root details.
        overview.update(vcx, |o, cx| {
            o.open_work_item_details(
                "s1".into(),
                crate::work_outline::details_panel::SPACE_ROOT_ITEM_ID.into(),
                cx,
            )
        });
        vcx.run_until_parked();
        draw(vcx);
        assert!(vcx.debug_bounds("space-root-details").is_some());
        assert!(vcx.debug_bounds("work-item-instruction-0").is_some());
        let details = overview.read_with(vcx, |o, _| o.work_item_details.clone().unwrap());
        details.update(vcx, |d, cx| {
            d.archive_instruction(instruction_fixture::I_ROOT.into(), cx)
        });
        let request = details.update(vcx, |d, _| d.captured_drift.take()).unwrap();
        vcx.update(|window, cx| (request.on_decision)(true, window, cx));
        vcx.run_until_parked();
        assert_eq!(
            ops(&calls),
            ["updateWorkItemInstruction", "archiveWorkItemInstruction"]
        );

        // A Space root without Chats below would not ask: with no
        // instructions at all an edit is not a drift.
        let mut bare = instruction_fixture::snapshot("s1");
        bare.instructions
            .retain(|i| i.work_item_id.as_deref() == Some(HOUSEKEEPING));
        bare.instructions.push(instruction_fixture::instruction(
            "i-house",
            "s1",
            Some(HOUSEKEEPING),
            "a0",
            "Tidy up.",
        ));
        overview.update(vcx, |o, cx| {
            o.set_outline_loads(fixture_loads(Some(bare)), cx)
        });
        let details = open_details(&overview, vcx, HOUSEKEEPING);
        details.update(vcx, |d, cx| d.archive_instruction("i-house".into(), cx));
        vcx.run_until_parked();
        assert!(details.read_with(vcx, |d, _| d.captured_drift.is_none()));
        assert_eq!(
            ops(&calls).last().map(String::as_str),
            Some("archiveWorkItemInstruction")
        );
    }

    #[gpui::test]
    fn inheritance_toggle_and_association_confirm_only_when_effective_changes(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, calls, vcx) = instruction_overview(
            cx,
            dir.path(),
            instruction_fixture::snapshot("s1"),
            reply_ok,
        );
        let details = open_details(&overview, vcx, API_AUTH);
        details.update(vcx, |d, cx| {
            d.instructions_open = true;
            cx.notify();
        });
        draw(vcx);
        let toggle = vcx
            .debug_bounds("work-item-inherit-toggle")
            .unwrap()
            .center();
        vcx.simulate_click(toggle, gpui::Modifiers::default());
        let request = details
            .update(vcx, |d, _| d.captured_drift.take())
            .expect("turning inheritance off drifts chat-auth");
        assert_eq!(request.impact.chat_ids, [CHAT_AUTH]);
        vcx.update(|window, cx| (request.on_decision)(true, window, cx));
        vcx.run_until_parked();
        let recorded = calls.lock().unwrap().clone();
        assert_eq!(recorded[0]["op"], "updateWorkItem");
        assert_eq!(recorded[0]["changes"]["inheritParentInstructions"], false);

        // Associating a loose chat with Housekeeping gains the Space-root
        // instruction: applies at once with an Undo toast. Without
        // instructions: applies with no toast.
        let mut outline_calls = 1;
        for (snapshot, asks) in [
            (instruction_fixture::snapshot("s1"), true),
            (crate::work_outline::store::fixture_snapshot("s1"), false),
        ] {
            overview.update(vcx, |o, cx| {
                o.set_outline_loads(fixture_loads(Some(snapshot)), cx)
            });
            draw(vcx);
            let outline = overview.read_with(vcx, |o, _| o.outline.clone());
            outline.update(vcx, |outline, cx| {
                outline.dismiss_move_toast(cx);
                outline.select(&row_key::item("s1", HOUSEKEEPING), cx)
            });
            vcx.update(|window, cx| {
                outline.update(cx, |outline, cx| {
                    outline.open_associate_picker(None, window, cx).unwrap();
                    let ix = 0; // first unassociated chat
                    outline.choose_picker_entry(ix, window, cx);
                })
            });
            vcx.run_until_parked();
            let toast = outline.read_with(vcx, |outline, _| outline.move_toast_text());
            assert_eq!(toast.is_some(), asks);
            outline_calls += 1;
            assert_eq!(calls.lock().unwrap().len(), outline_calls);
            outline.update_in(vcx, |outline, window, cx| outline.close_picker(window, cx));
        }
    }

    /// Associating or disassociating a Chat from Work Item Details applies at
    /// once like an Outline move: an Undo toast (no modal) when the Chat's
    /// effective instructions change, nothing when they do not.
    #[gpui::test]
    fn details_chat_association_toasts_drift_instead_of_confirming(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, calls, vcx) = instruction_overview(
            cx,
            dir.path(),
            instruction_fixture::snapshot("s1"),
            reply_ok,
        );
        let outline = overview.read_with(vcx, |o, _| o.outline.clone());
        let toast = |vcx: &mut gpui::VisualTestContext| {
            outline.read_with(vcx, |outline, _| outline.move_toast_text())
        };
        let click = |vcx: &mut gpui::VisualTestContext, selector: &'static str| {
            let at = vcx.debug_bounds(selector).expect(selector).center();
            vcx.simulate_click(at, gpui::Modifiers::default());
            vcx.run_until_parked();
            draw(vcx);
        };

        // Housekeeping gains the Space-root instruction for the loose chat.
        let details = open_details(&overview, vcx, HOUSEKEEPING);
        click(vcx, "work-item-associate-toggle");
        click(vcx, "work-item-associate-0");
        assert!(details.read_with(vcx, |d, _| d.captured_drift.is_none()));
        assert_eq!(ops(&calls), ["assignChatToWorkItem"]);
        let text = toast(vcx).expect("toast");
        assert!(
            text.ends_with("instructions changed for 1 chat (it keeps its current instructions)"),
            "{text}"
        );
        // Undo sends the inverse (the chat was unassociated).
        click(vcx, "outline-move-toast-undo");
        assert_eq!(
            ops(&calls),
            ["assignChatToWorkItem", "removeChatFromWorkItem"]
        );
        assert_eq!(toast(vcx), None);

        // Disassociating chat-ui from Launch loses its instructions: toast
        // with Undo that re-assigns it.
        let details = open_details(&overview, vcx, UI);
        click(vcx, "work-item-chat-remove-0");
        assert!(details.read_with(vcx, |d, _| d.captured_drift.is_none()));
        assert_eq!(
            ops(&calls).last().map(String::as_str),
            Some("removeChatFromWorkItem")
        );
        assert!(toast(vcx).is_some());
        click(vcx, "outline-move-toast-undo");
        let last = calls.lock().unwrap().last().cloned().unwrap();
        assert_eq!(last["op"], "assignChatToWorkItem");
        assert_eq!(last["workItemId"], UI);

        // Without instructions nothing changes for the chat: no toast.
        overview.update(vcx, |o, cx| {
            o.set_outline_loads(
                fixture_loads(Some(crate::work_outline::store::fixture_snapshot("s1"))),
                cx,
            )
        });
        outline.update(vcx, |outline, cx| outline.dismiss_move_toast(cx));
        let before = calls.lock().unwrap().len();
        let details = open_details(&overview, vcx, HOUSEKEEPING);
        click(vcx, "work-item-associate-toggle");
        click(vcx, "work-item-associate-0");
        assert!(details.read_with(vcx, |d, _| d.captured_drift.is_none()));
        assert_eq!(calls.lock().unwrap().len(), before + 1);
        assert_eq!(toast(vcx), None);
    }

    #[gpui::test]
    fn moving_an_item_toasts_drift_only_when_assigned_chats_change(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, calls, vcx) = instruction_overview(
            cx,
            dir.path(),
            instruction_fixture::snapshot("s1"),
            reply_ok,
        );
        let outline = overview.read_with(vcx, |o, _| o.outline.clone());
        let move_to_top = |item: &str, vcx: &mut gpui::VisualTestContext| {
            outline.update(vcx, |outline, cx| {
                outline.dismiss_move_toast(cx);
                outline.select(&row_key::item("s1", item), cx)
            });
            vcx.update(|window, cx| {
                outline.update(cx, |outline, cx| {
                    outline.open_move_picker(window, cx).unwrap();
                    // Entry 0 is the top level.
                    outline.choose_picker_entry(0, window, cx);
                })
            });
            vcx.run_until_parked();
            outline.read_with(vcx, |outline, _| outline.move_toast_text())
        };
        // API (chat-api, and chat-auth below) loses Launch's instruction:
        // the move is sent at once, with a toast and no confirmation.
        let toast = move_to_top(API, vcx).expect("toasts");
        assert!(
            toast.contains("instructions changed for 2 chats"),
            "{toast}"
        );
        assert_eq!(ops(&calls), ["moveWorkItem"]);
        // Housekeeping is already top level with no Chats: no toast.
        outline.update_in(vcx, |outline, window, cx| outline.close_picker(window, cx));
        assert!(move_to_top(HOUSEKEEPING, vcx).is_none());
        assert_eq!(ops(&calls), ["moveWorkItem", "moveWorkItem"]);
    }

    #[gpui::test]
    fn effective_view_orders_entries_marks_inherited_and_shows_budget(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, _calls, vcx) = instruction_overview(
            cx,
            dir.path(),
            instruction_fixture::snapshot("s1"),
            reply_ok,
        );
        let details = open_details(&overview, vcx, API_AUTH);
        assert!(vcx.debug_bounds("work-item-effective").is_some());
        assert!(
            vcx.debug_bounds("work-item-effective-entry-0").is_none(),
            "collapsed until opened"
        );
        details.update(vcx, |d, cx| d.open_effective(cx));
        draw(vcx);
        for ix in 0..4 {
            assert!(row_bounds(vcx, format!("work-item-effective-entry-{ix}")).is_some());
        }
        assert!(vcx.debug_bounds("work-item-effective-entry-4").is_none());
        // Root-to-child order, top to bottom.
        let y = |vcx: &mut gpui::VisualTestContext, ix: usize| {
            row_bounds(vcx, format!("work-item-effective-entry-{ix}"))
                .unwrap()
                .origin
                .y
        };
        assert!(y(vcx, 0) < y(vcx, 1) && y(vcx, 2) < y(vcx, 3));
        let effective = instruction_fixture::snapshot("s1").effective_instructions(API_AUTH);
        assert_eq!(
            effective
                .entries
                .iter()
                .map(|e| (e.scope_title.as_str(), e.inherited))
                .collect::<Vec<_>>(),
            [
                ("Space root", true),
                ("Launch v2", true),
                ("API", true),
                ("Auth endpoints", false)
            ]
        );
        assert!(vcx.debug_bounds("work-item-effective-budget").is_some());
        assert!(vcx.debug_bounds("work-item-effective-precedence").is_some());
        assert!(
            vcx.debug_bounds("work-item-effective-budget-error")
                .is_none()
        );

        // Over budget: 33 entries.
        let mut big = instruction_fixture::snapshot("s1");
        for i in 0..30 {
            big.instructions.push(instruction_fixture::instruction(
                &format!("i-extra-{i:02}"),
                "s1",
                Some(API_AUTH),
                &format!("b{i:02}"),
                "Extra.",
            ));
        }
        overview.update(vcx, |o, cx| {
            o.set_outline_loads(fixture_loads(Some(big)), cx)
        });
        draw(vcx);
        assert!(
            vcx.debug_bounds("work-item-effective-budget-error")
                .is_some()
        );

        // The palette command opens the selected item's view expanded.
        overview.update(vcx, |o, cx| o.close_work_item_details(cx));
        let outline = overview.read_with(vcx, |o, _| o.outline.clone());
        outline.update(vcx, |outline, cx| {
            outline.select(&row_key::item("s1", API), cx)
        });
        vcx.update(|window, cx| {
            overview.update(cx, |o, cx| {
                o.run_work_outline_command(
                    WorkOutlineCommand::OpenEffectiveInstructions,
                    window,
                    cx,
                )
            })
        });
        vcx.run_until_parked();
        overview.read_with(vcx, |o, cx| {
            let details = o.work_item_details.as_ref().unwrap().read(cx);
            assert_eq!(details.item_id, API);
            assert!(details.effective_open);
        });
    }

    #[gpui::test]
    fn outline_chat_rows_flag_drift_only_for_drifted_chats(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (_overview, _calls, vcx) = instruction_overview(
            cx,
            dir.path(),
            instruction_fixture::snapshot("s1"),
            reply_ok,
        );
        assert!(row_bounds(vcx, format!("outline-drift-{}", row_key::chat(CHAT_API))).is_some());
        assert!(row_bounds(vcx, format!("outline-drift-{}", row_key::chat(CHAT_AUTH))).is_none());
        assert!(row_bounds(vcx, format!("outline-drift-{}", row_key::chat(CHAT_UI))).is_none());
    }

    /// Single Space: the Space-root entry opens the Space-root details.
    #[gpui::test]
    fn space_root_entry_opens_space_root_instructions(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, _calls, vcx) = instruction_overview(
            cx,
            dir.path(),
            instruction_fixture::snapshot("s1"),
            reply_ok,
        );
        let entry = vcx
            .debug_bounds("outline-space-root-entry")
            .expect("single-Space entry")
            .center();
        vcx.simulate_click(entry, gpui::Modifiers::default());
        draw(vcx);
        assert!(vcx.debug_bounds("space-root-details").is_some());
        overview.read_with(vcx, |o, cx| {
            assert!(
                o.work_item_details
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .is_space_root()
            );
        });
        // Its instruction list is open by default and lists the root entry.
        assert!(vcx.debug_bounds("work-item-instruction-0").is_some());
        assert!(vcx.debug_bounds("work-item-instruction-1").is_none());
    }

    #[gpui::test]
    fn long_instructions_collapse_and_controls_sit_in_the_header(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let mut snapshot = instruction_fixture::snapshot("s1");
        let long = "Keep every change small and reviewable. ".repeat(20);
        for instruction in snapshot.instructions.iter_mut() {
            if instruction.id == instruction_fixture::I_ROOT {
                instruction.text = long.clone();
            }
        }
        snapshot.instructions.push(instruction_fixture::instruction(
            "i-root-short",
            "s1",
            None,
            "zz",
            "Be brief.",
        ));
        let (overview, calls, vcx) = instruction_overview(cx, dir.path(), snapshot, reply_ok);
        overview.update(vcx, |o, cx| {
            o.open_work_item_details(
                "s1".into(),
                crate::work_outline::details_panel::SPACE_ROOT_ITEM_ID.into(),
                cx,
            )
        });
        vcx.run_until_parked();
        draw(vcx);

        // Only the long entry gets a toggle.
        assert!(vcx.debug_bounds("work-item-instruction-fold-0").is_some());
        assert!(vcx.debug_bounds("work-item-instruction-fold-1").is_none());
        let collapsed = vcx.debug_bounds("work-item-instruction-text-0").unwrap();
        let short = vcx.debug_bounds("work-item-instruction-text-1").unwrap();
        let line = f32::from(short.size.height);

        // The controls live in the header row, above the text, and the
        // short entry's controls are at the top of its entry too.
        for ix in 0..2 {
            let mut named =
                |name: String| vcx.debug_bounds(Box::leak(name.into_boxed_str())).unwrap();
            let header = named(format!("work-item-instruction-header-{ix}"));
            let text = named(format!("work-item-instruction-text-{ix}"));
            for control in ["edit", "archive"] {
                let b = named(format!("work-item-instruction-{control}-{ix}"));
                assert!(b.top() >= header.top() && b.bottom() <= header.bottom());
                assert!(b.bottom() <= text.top() + px(1.0));
            }
        }
        // Collapsed to four lines: well short of the full text, and the
        // controls did not move down with the text.
        assert!(f32::from(collapsed.size.height) <= line * 4.0 + 1.0);
        let edit_top = vcx
            .debug_bounds("work-item-instruction-edit-0")
            .unwrap()
            .top();

        // Show more expands, Show less collapses again.
        let fold = vcx
            .debug_bounds("work-item-instruction-fold-0")
            .unwrap()
            .center();
        vcx.simulate_click(fold, gpui::Modifiers::default());
        draw(vcx);
        let expanded = vcx.debug_bounds("work-item-instruction-text-0").unwrap();
        assert!(f32::from(expanded.size.height) > f32::from(collapsed.size.height) * 1.5);
        assert_eq!(
            vcx.debug_bounds("work-item-instruction-edit-0")
                .unwrap()
                .top(),
            edit_top
        );
        let fold = vcx
            .debug_bounds("work-item-instruction-fold-0")
            .unwrap()
            .center();
        vcx.simulate_click(fold, gpui::Modifiers::default());
        draw(vcx);
        let again = vcx.debug_bounds("work-item-instruction-text-0").unwrap();
        assert_eq!(again.size.height, collapsed.size.height);
        assert!(calls.lock().unwrap().is_empty());
    }

    /// Instruction "Archive" is now "Remove" (confirmation and tooltip); the
    /// archive operation itself is covered by the drift-confirmation test.
    #[test]
    fn removing_an_instruction_is_labelled_remove() {
        use crate::work_outline::hints::details_button_hint;
        use crate::work_outline::instructions::OutlineChange;
        let change = OutlineChange::ArchiveInstruction {
            instruction_id: "i".into(),
        };
        assert_eq!(change.title(), "Remove instruction?");
        assert_eq!(change.confirm_label(), "Remove");
        assert!(details_button_hint("Remove").is_some());
    }

    #[gpui::test]
    fn unsupported_engine_shows_a_calm_instruction_error(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, calls, vcx) =
            instruction_overview(cx, dir.path(), instruction_fixture::snapshot("s1"), |_| {
                Err(mast_rpc::RpcError::Failed(
                    "work-outline/unsupported: instruction operations are not available".into(),
                ))
            });
        let details = open_details(&overview, vcx, UI);
        vcx.update(|window, cx| {
            details.update(cx, |d, cx| {
                d.begin_instruction_draft(None, window, cx);
                d.set_instruction_text("Anything.", cx);
                d.save_instruction(cx);
            })
        });
        vcx.run_until_parked();
        draw(vcx);
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert!(vcx.debug_bounds("work-item-instructions-error").is_some());
        details.read_with(vcx, |d, cx| {
            assert!(d.instruction_draft.is_some(), "the draft survives");
            assert!(d.has_unsaved_draft(cx));
        });
    }

    // ----------------------------------------------------------------------
    // Execution configuration (Milestone 3)
    // ----------------------------------------------------------------------

    use crate::work_outline::execution::fixture as execution_fixture;

    fn identity(repo: &str, path: &str) -> serde_json::Value {
        serde_json::json!({
            "repoId": repo, "repoFingerprint": format!("root-{repo}"), "repoPath": path,
            "checkoutId": format!("checkout:{repo}"), "mainCheckout": true,
            "worktrees": [
                {"path": path, "checkoutId": format!("checkout:{repo}"), "branch": "main", "main": true},
                {"path": format!("{path}-wt"), "checkoutId": format!("checkout:{repo}-wt"),
                 "branch": "feature", "main": false},
            ],
        })
    }

    /// Mutations answer `{}`; `DescribeExecutionRepository` knows the fixture
    /// repositories and refuses anything else as non-Git; validation answers
    /// with `validate`.
    fn execution_reply(
        validate: std::sync::Arc<std::sync::Mutex<serde_json::Value>>,
    ) -> impl Fn(&serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError>
    + Send
    + Sync
    + 'static {
        move |params| {
            if params.get("op").is_some() {
                return Ok(serde_json::json!({}));
            }
            if let Some(path) = params.get("path").and_then(|p| p.as_str()) {
                return match path {
                    "/work/alpha/ui" => Ok(identity("repo:ui", path)),
                    "/work/alpha/flagship" => Ok(identity("repo:flagship", path)),
                    other => Err(mast_rpc::RpcError::Failed(format!(
                        "{other} is not a Git repository"
                    ))),
                };
            }
            Ok(validate.lock().unwrap().clone())
        }
    }

    fn validate_ok() -> std::sync::Arc<std::sync::Mutex<serde_json::Value>> {
        std::sync::Arc::new(std::sync::Mutex::new(
            serde_json::to_value(execution_fixture::launch_validated()).unwrap(),
        ))
    }

    fn mutations(calls: &OutlineCalls) -> Vec<serde_json::Value> {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.get("op").is_some())
            .cloned()
            .collect()
    }

    /// The Execution section is collapsed until opened, shows the resolved
    /// configuration, offers Reset only for an item with its own row, and
    /// lists exception Chats (with the compact marker on the header and on
    /// the Outline row).
    #[gpui::test]
    fn execution_section_shows_resolution_reset_and_exceptions(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, _calls, vcx) = instruction_overview(
            cx,
            dir.path(),
            execution_fixture::snapshot("s1"),
            execution_reply(validate_ok()),
        );
        // Outline rows: only API (one exception) carries the marker.
        assert!(
            row_bounds(
                vcx,
                format!("outline-execution-exceptions-{}", row_key::item("s1", API))
            )
            .is_some()
        );
        assert!(
            row_bounds(
                vcx,
                format!("outline-execution-exceptions-{}", row_key::item("s1", UI))
            )
            .is_none()
        );

        let details = open_details(&overview, vcx, API);
        assert!(vcx.debug_bounds("work-item-execution").is_some());
        assert!(
            vcx.debug_bounds("work-item-execution-exceptions-marker")
                .is_some()
        );
        assert!(
            vcx.debug_bounds("work-item-execution-source").is_none(),
            "collapsed"
        );
        details.update(vcx, |d, cx| {
            d.execution_open = true;
            cx.notify();
        });
        draw(vcx);
        assert!(vcx.debug_bounds("work-item-execution-source").is_some());
        assert!(vcx.debug_bounds("work-item-execution-repo-0").is_some());
        assert!(vcx.debug_bounds("work-item-execution-reset").is_some());
        assert!(
            vcx.debug_bounds("work-item-execution-exception-0")
                .is_some()
        );

        // UI inherits from Launch: no Reset, no exceptions.
        let details = open_details(&overview, vcx, UI);
        details.update(vcx, |d, cx| {
            d.execution_open = true;
            cx.notify();
        });
        draw(vcx);
        assert!(vcx.debug_bounds("work-item-execution-repo-1").is_some());
        assert!(vcx.debug_bounds("work-item-execution-reset").is_none());
        assert!(
            vcx.debug_bounds("work-item-execution-exceptions-marker")
                .is_none()
        );
        assert!(vcx.debug_bounds("work-item-execution-exceptions").is_none());
    }

    /// Edit form: explicit Save/Cancel, the discard policy, the drift
    /// confirmation for item Chats, and the written configuration.
    #[gpui::test]
    fn execution_edits_save_explicitly_and_confirm_drift(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, calls, vcx) = instruction_overview(
            cx,
            dir.path(),
            execution_fixture::snapshot("s1"),
            execution_reply(validate_ok()),
        );
        let details = open_details(&overview, vcx, UI);
        vcx.update(|window, cx| details.update(cx, |d, cx| d.begin_execution_draft(window, cx)));
        vcx.run_until_parked();
        draw(vcx);
        // Prefilled with the inherited configuration; worktree choices were
        // described on the owning device.
        let described: Vec<String> = calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| c.get("path").and_then(|p| p.as_str()).map(str::to_owned))
            .collect();
        assert_eq!(described, ["/work/alpha/ui", "/work/alpha/flagship"]);
        // The Space lives on device `d`, not this app's engine: Describe is
        // routed there (only the owner can fingerprint its repositories).
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .all(|c| c["targetDeviceId"] == "d")
        );
        assert!(vcx.debug_bounds("work-item-execution-editor").is_some());
        assert!(
            vcx.debug_bounds("work-item-execution-worktree-1-2")
                .is_some(),
            "linked worktree offered"
        );
        details.read_with(vcx, |d, cx| {
            assert_eq!(d.execution_draft.as_ref().unwrap().bindings.len(), 2);
            assert!(!d.execution_draft_dirty(cx));
        });

        // Cancel discards; nothing written, nothing asked.
        let cancel = vcx
            .debug_bounds("work-item-execution-cancel")
            .unwrap()
            .center();
        vcx.simulate_click(cancel, gpui::Modifiers::default());
        draw(vcx);
        details.read_with(vcx, |d, _| {
            assert!(d.execution_draft.is_none());
            assert!(d.captured_confirm.is_none());
        });
        assert!(mutations(&calls).is_empty());

        // Edit: flagship becomes primary (radio: ui stops being primary) and
        // pins its linked worktree.
        vcx.update(|window, cx| details.update(cx, |d, cx| d.begin_execution_draft(window, cx)));
        vcx.run_until_parked();
        draw(vcx);
        let primary = vcx
            .debug_bounds("work-item-execution-primary-1")
            .unwrap()
            .center();
        vcx.simulate_click(primary, gpui::Modifiers::default());
        draw(vcx);
        let worktree = vcx
            .debug_bounds("work-item-execution-worktree-1-2")
            .unwrap()
            .center();
        vcx.simulate_click(worktree, gpui::Modifiers::default());
        draw(vcx);
        details.read_with(vcx, |d, cx| {
            let draft = d.execution_draft.as_ref().unwrap();
            let primaries: Vec<bool> = draft.bindings.iter().map(|b| b.binding.primary).collect();
            assert_eq!(primaries, [false, true]);
            assert_eq!(
                draft.bindings[1].binding.worktree_path.as_deref(),
                Some("/work/alpha/flagship-wt")
            );
            assert!(d.execution_draft_dirty(cx));
        });

        // Closing with the dirty draft asks; Keep Editing keeps it.
        details.update(vcx, |d, cx| d.request_close(cx));
        let request = details
            .update(vcx, |d, _| d.captured_confirm.take())
            .expect("asks");
        assert_eq!(
            request.body.as_ref(),
            "Your execution configuration edits have not been saved."
        );
        vcx.update(|window, cx| (request.on_decision)(false, window, cx));
        vcx.run_until_parked();
        assert!(details.read_with(vcx, |d, cx| d.has_unsaved_draft(cx)));
        assert!(mutations(&calls).is_empty(), "blur and close never save");

        // Save: chat-ui runs with UI's current configuration, so the drift
        // confirmation states it keeps its captured configuration.
        let save = vcx
            .debug_bounds("work-item-execution-save")
            .unwrap()
            .center();
        vcx.simulate_click(save, gpui::Modifiers::default());
        vcx.run_until_parked();
        let request = details
            .update(vcx, |d, _| d.captured_confirm.take())
            .expect("drift confirmation");
        assert_eq!(
            request.title.as_ref(),
            "Change the execution configuration?"
        );
        assert!(request.body.contains("keep their captured configuration"));
        assert!(request.body.contains("exceptions"));
        assert!(mutations(&calls).is_empty(), "nothing before confirming");
        vcx.update(|window, cx| (request.on_decision)(true, window, cx));
        vcx.run_until_parked();
        let written = mutations(&calls);
        assert_eq!(written.len(), 1);
        assert_eq!(written[0]["op"], "configureExecution");
        assert_eq!(
            written[0]["scope"],
            serde_json::json!({"type": "workItem", "workItemId": UI})
        );
        let bindings = written[0]["configuration"]["bindings"].as_array().unwrap();
        assert_eq!(bindings[1]["primary"], true);
        assert_eq!(bindings[1]["worktreeId"], "checkout:repo:flagship-wt");
        assert_eq!(bindings[0]["primary"], false);
        details.read_with(vcx, |d, _| assert!(d.execution_draft.is_none()));
    }

    /// Repositories are added through the folder picker and described on the
    /// owning device; a non-Git folder is refused. No bound Chats: no
    /// confirmation.
    #[gpui::test]
    fn execution_repositories_are_described_and_non_git_folders_refused(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, calls, vcx) = instruction_overview(
            cx,
            dir.path(),
            execution_fixture::snapshot("s1"),
            execution_reply(validate_ok()),
        );
        let details = open_details(&overview, vcx, HOUSEKEEPING);
        vcx.update(|window, cx| details.update(cx, |d, cx| d.begin_execution_draft(window, cx)));
        draw(vcx);
        details.read_with(vcx, |d, cx| {
            assert!(d.execution_draft.as_ref().unwrap().bindings.is_empty());
            assert_eq!(
                d.execution_draft_configuration(cx).unwrap().execution_root,
                "/work/alpha"
            );
        });
        let pick = |vcx: &mut gpui::VisualTestContext, selector: &'static str, path: &str| {
            let button = vcx.debug_bounds(selector).unwrap().center();
            vcx.simulate_click(button, gpui::Modifiers::default());
            vcx.run_until_parked();
            let request = details
                .update(vcx, |d, _| d.captured_folder_pick.take())
                .expect("folder picker");
            assert_eq!(request.device_id, "d");
            let path = path.to_owned();
            vcx.update(|_, cx| (request.on_pick)(path, cx));
            vcx.run_until_parked();
            draw(vcx);
        };
        pick(vcx, "work-item-execution-add-repo", "/work/alpha/docs");
        details.read_with(vcx, |d, _| {
            assert!(d.execution_draft.as_ref().unwrap().bindings.is_empty());
            assert!(
                d.execution_error
                    .as_deref()
                    .unwrap()
                    .contains("not a Git repository")
            );
        });
        pick(vcx, "work-item-execution-add-repo", "/work/alpha/ui");
        pick(vcx, "work-item-execution-choose-root", "/work/alpha/ui");
        details.read_with(vcx, |d, cx| {
            let draft = d.execution_draft.as_ref().unwrap();
            assert_eq!(draft.bindings.len(), 1);
            assert_eq!(draft.bindings[0].binding.repo_fingerprint, "root-repo:ui");
            assert_eq!(
                d.execution_draft_configuration(cx).unwrap().execution_root,
                "/work/alpha/ui"
            );
        });
        let save = vcx
            .debug_bounds("work-item-execution-save")
            .unwrap()
            .center();
        vcx.simulate_click(save, gpui::Modifiers::default());
        vcx.run_until_parked();
        details.read_with(vcx, |d, _| assert!(d.captured_confirm.is_none()));
        let written = mutations(&calls);
        assert_eq!(written.len(), 1);
        assert_eq!(
            written[0]["configuration"]["executionRoot"],
            "/work/alpha/ui"
        );
        assert_eq!(
            written[0]["configuration"]["bindings"][0]["repoId"],
            "repo:ui"
        );
    }

    /// Validate runs on the owning device (the engine forwards; no target
    /// device here) and shows each error's message and remediation, with
    /// offline and unsupported owners as their own states.
    #[gpui::test]
    fn execution_validation_shows_remediation_and_distinct_owner_states(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let validate = validate_ok();
        let (overview, calls, vcx) = instruction_overview(
            cx,
            dir.path(),
            execution_fixture::snapshot("s1"),
            execution_reply(validate.clone()),
        );
        let details = open_details(&overview, vcx, API);
        details.update(vcx, |d, cx| {
            d.execution_open = true;
            cx.notify();
        });
        draw(vcx);
        let cases = [
            (
                mast_proto::ExecutionValidationError::DeviceOffline {
                    device_id: "d".into(),
                    detail: String::new(),
                },
                "work-item-execution-validation-device-offline",
            ),
            (
                mast_proto::ExecutionValidationError::HostUnsupported {
                    device_id: "d".into(),
                    capability: "work-outline-multi-repo-v1".into(),
                },
                "work-item-execution-validation-host-unsupported",
            ),
            (
                mast_proto::ExecutionValidationError::WorktreeMoved {
                    repo_id: "repo:flagship".into(),
                    worktree_id: "wt".into(),
                    expected_path: "/a".into(),
                    found_path: "/b".into(),
                },
                "work-item-execution-validation-blocked",
            ),
        ];
        for (error, selector) in cases {
            *validate.lock().unwrap() = serde_json::to_value(
                mast_proto::ValidateExecutionConfigurationReply::from_result(
                    None,
                    Err(vec![error]),
                ),
            )
            .unwrap();
            let button = vcx
                .debug_bounds("work-item-execution-validate")
                .unwrap()
                .center();
            vcx.simulate_click(button, gpui::Modifiers::default());
            vcx.run_until_parked();
            draw(vcx);
            assert!(vcx.debug_bounds(selector).is_some(), "{selector}");
            assert!(vcx.debug_bounds("work-item-execution-issue-0").is_some());
        }
        let last = calls.lock().unwrap().last().cloned().unwrap();
        assert_eq!(
            last["scope"],
            serde_json::json!({"type": "workItem", "workItemId": API})
        );
        assert!(last.get("targetDeviceId").is_none());
        details.read_with(vcx, |d, _| {
            let crate::work_outline::execution::ValidationState::Done(reply) =
                &d.execution_validation
            else {
                panic!("validated");
            };
            let issue = &crate::work_outline::execution::validation_issues(reply)[0];
            assert!(issue.message.contains("moved from /a to /b"));
            assert!(issue.remediation.contains("Update the worktree path"));
        });
        // Ready: no issues.
        *validate.lock().unwrap() =
            serde_json::to_value(execution_fixture::launch_validated()).unwrap();
        let button = vcx
            .debug_bounds("work-item-execution-validate")
            .unwrap()
            .center();
        vcx.simulate_click(button, gpui::Modifiers::default());
        vcx.run_until_parked();
        draw(vcx);
        assert!(
            vcx.debug_bounds("work-item-execution-validation-ready")
                .is_some()
        );
        assert!(vcx.debug_bounds("work-item-execution-issue-0").is_none());

        // A dirty draft validates the draft (as a resolved snapshot).
        vcx.update(|window, cx| details.update(cx, |d, cx| d.begin_execution_draft(window, cx)));
        details.update(vcx, |d, cx| d.set_execution_root("/work/alpha", cx));
        details.update(vcx, |d, cx| d.validate_execution(cx));
        vcx.run_until_parked();
        let last = calls.lock().unwrap().last().cloned().unwrap();
        assert_eq!(
            last["resolved"]["configuration"]["executionRoot"],
            "/work/alpha"
        );
        assert_eq!(last["resolved"]["deviceId"], "d");
        assert!(last.get("scope").is_none());
    }

    /// Reset to inherited confirms when bound Chats would differ and writes
    /// a null configuration; the Space-root variant edits the Space default.
    #[gpui::test]
    fn execution_reset_and_space_default(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (overview, calls, vcx) = instruction_overview(
            cx,
            dir.path(),
            execution_fixture::snapshot("s1"),
            execution_reply(validate_ok()),
        );
        let details = open_details(&overview, vcx, API);
        details.update(vcx, |d, cx| d.reset_execution(cx));
        let request = details
            .update(vcx, |d, _| d.captured_confirm.take())
            .expect("chat-auth would differ");
        assert_eq!(request.confirm_label.as_ref(), "Reset");
        vcx.update(|window, cx| (request.on_decision)(true, window, cx));
        vcx.run_until_parked();
        let written = mutations(&calls);
        assert_eq!(written.len(), 1);
        assert!(written[0]["configuration"].is_null());

        let details = open_details(&overview, vcx, "");
        assert!(details.read_with(vcx, |d, _| d.is_space_root()));
        assert!(vcx.debug_bounds("work-item-execution").is_some());
        vcx.update(|window, cx| details.update(cx, |d, cx| d.begin_execution_draft(window, cx)));
        details.update(vcx, |d, cx| {
            d.set_execution_root("/work/alpha/ui", cx);
            d.save_execution(cx);
        });
        vcx.run_until_parked();
        // Housekeeping (implicit) and Launch? Only Housekeeping changes, and
        // it has no Chats: no confirmation.
        details.read_with(vcx, |d, _| assert!(d.captured_confirm.is_none()));
        let written = mutations(&calls);
        assert_eq!(written.len(), 2);
        assert_eq!(
            written[1]["scope"],
            serde_json::json!({"type": "space", "spaceId": "s1"})
        );
    }

    /// Start Chat entry points: the Outline row action on the selected item,
    /// the Work Item Details button, and the palette command (which needs a
    /// selected item). Changed instructions open Effective Instructions in
    /// details first.
    #[gpui::test]
    fn start_chat_entry_points_reach_the_shell_and_review_changed_instructions(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut snapshot = execution_fixture::snapshot("s1");
        let instructions = instruction_fixture::snapshot("s1");
        snapshot.instructions = instructions.instructions;
        snapshot.chat_instruction_states = instructions.chat_instruction_states;
        snapshot.revisions = instructions.revisions;
        let (overview, _calls, vcx) =
            instruction_overview(cx, dir.path(), snapshot, execution_reply(validate_ok()));

        // Palette without a selection: a notice, no request.
        vcx.update(|window, cx| {
            overview.update(cx, |o, cx| {
                o.run_work_outline_command(WorkOutlineCommand::StartChatForWorkItem, window, cx)
            })
        });
        draw(vcx);
        assert!(overview.read_with(vcx, |o, _| o.captured_start_chat.is_none()));

        // Outline row action on the selected item (Auth: instructions
        // current, so details stay closed).
        let auth_key = row_key::item("s1", API_AUTH);
        overview.update(vcx, |o, cx| {
            o.outline
                .update(cx, |outline, cx| outline.select(&auth_key, cx))
        });
        draw(vcx);
        assert!(
            row_bounds(
                vcx,
                format!("outline-start-chat-{}", row_key::item("s1", API))
            )
            .is_none()
        );
        let action = row_bounds(vcx, format!("outline-start-chat-{auth_key}"))
            .unwrap()
            .center();
        vcx.simulate_click(action, gpui::Modifiers::default());
        draw(vcx);
        overview.update(vcx, |o, _| {
            assert_eq!(
                o.captured_start_chat.take(),
                Some(("s1".to_string(), API_AUTH.to_string()))
            );
            assert!(o.work_item_details.is_none());
        });

        // Palette with API selected: API's instructions changed since its
        // last launch, so Effective Instructions open in details first.
        let api_key = row_key::item("s1", API);
        overview.update(vcx, |o, cx| {
            o.outline
                .update(cx, |outline, cx| outline.select(&api_key, cx))
        });
        vcx.update(|window, cx| {
            overview.update(cx, |o, cx| {
                o.run_work_outline_command(WorkOutlineCommand::StartChatForWorkItem, window, cx)
            })
        });
        vcx.run_until_parked();
        draw(vcx);
        let details = overview.update(vcx, |o, _| {
            assert_eq!(
                o.captured_start_chat.take(),
                Some(("s1".to_string(), API.to_string()))
            );
            o.work_item_details
                .clone()
                .expect("Effective Instructions opened")
        });
        assert!(details.read_with(vcx, |d, _| d.effective_open));

        // The details button.
        let button = vcx.debug_bounds("work-item-start-chat").unwrap().center();
        vcx.simulate_click(button, gpui::Modifiers::default());
        vcx.run_until_parked();
        assert_eq!(
            overview.update(vcx, |o, _| o.captured_start_chat.take()),
            Some(("s1".to_string(), API.to_string()))
        );
    }

    /// Resizable Work Item Details panel; shares this module's fixtures.
    mod resizable_panes_tests {
        include!("work_outline/resizable_panes_tests.rs");
    }

    /// Milestone 4 (Required Actions, proposals); the body lives beside the
    /// Work Outline modules and shares this module's fixtures.
    mod required_actions_tests {
        include!("work_outline/required_actions_tests.rs");
    }

    /// Linear integration (panel, gating, chips, drops); the body lives
    /// beside the Work Outline modules and shares this module's fixtures.
    mod linear_tests {
        include!("work_outline/linear_tests.rs");
    }

    /// PR review sync (destination, chips, the GitHub section, ⌘↩ reviews,
    /// system Required Actions); shares this module's fixtures.
    mod pr_sync_tests {
        include!("work_outline/pr_sync_tests.rs");
    }

    /// Direct restructuring (keyboard, palette, picker placement, drag and
    /// drop); shares this module's fixtures.
    mod restructure_tests {
        include!("work_outline/restructure_tests.rs");
    }

    /// Undo / redo and Recent changes; shares this module's fixtures.
    mod undo_tests {
        include!("work_outline/undo_tests.rs");
    }

    /// Multi-selection and "Group into…"; shares this module's fixtures.
    mod group_tests {
        include!("work_outline/group_tests.rs");
    }
    mod project_ui_tests {
        include!("work_outline/project_ui_tests.rs");
    }

    /// First-use empty state of the Outline; shares this module's fixtures.
    mod guidance_tests {
        include!("work_outline/guidance_tests.rs");
    }

    /// `/` skill autocomplete and auto-grow of the Work Item Details
    /// editors; shares this module's fixtures.
    mod editor_skills_tests {
        include!("work_outline/editor_skills_tests.rs");
    }

    /// Space-root reorder and hide / unhide; shares this module's fixtures.
    mod space_roots_tests {
        include!("work_outline/space_roots_tests.rs");
    }

    /// Outline tree guides, active path, drop labels, move flash; shares
    /// this module's fixtures.
    mod tree_guides_tests {
        include!("work_outline/tree_guides_tests.rs");
    }

    /// Compact single-chat rows: rendering, open, menu, keys, drag.
    mod compact_rows_tests {
        include!("work_outline/compact_rows_tests.rs");
    }
}

#[cfg(test)]
#[path = "perf/overview_bench.rs"]
mod perf_bench;
