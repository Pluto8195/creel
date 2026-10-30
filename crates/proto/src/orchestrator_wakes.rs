//! Worker-event wakes for orchestrator chats: wire types.
//!
//! An orchestrator (a chat that started worker chats) is told about what its
//! workers do through one batched, system-attributed message instead of
//! waiting for the user to ask "status?". The engine owns the batching and
//! the user-first rules; these are the settings and the read model the UI
//! renders (Team tab header, Item tab, "N updates held" chip, unseen marks).

use serde::{Deserialize, Serialize};

/// What happened to a worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WakeEventKind {
    /// A Required Action was raised (including "Ready to merge").
    ActionRaised,
    /// A Required Action was resolved.
    ActionResolved,
    /// The worker's run errored.
    Errored,
    /// The worker finished a turn.
    TurnFinished,
    /// A PR was opened (linked) for the worker.
    PrOpened,
    /// A PR was merged for the worker.
    PrMerged,
}

impl WakeEventKind {
    pub const ALL: [WakeEventKind; 6] = [
        Self::ActionRaised,
        Self::ActionResolved,
        Self::Errored,
        Self::TurnFinished,
        Self::PrOpened,
        Self::PrMerged,
    ];
}

/// Per-orchestrator-chat wake settings. Required Actions (raised and
/// resolved) and errors are on by default; turn-finished and PR events are
/// off by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WakeSettings {
    /// Master switch for this chat.
    pub enabled: bool,
    /// Required Actions raised / resolved.
    pub required_actions: bool,
    /// A worker errored.
    pub errors: bool,
    /// A worker finished a turn.
    pub turn_finished: bool,
    /// A PR was opened / merged for a worker.
    pub prs: bool,
}

impl Default for WakeSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            required_actions: true,
            errors: true,
            turn_finished: false,
            prs: false,
        }
    }
}

impl WakeSettings {
    /// Whether an event of `kind` should wake the orchestrator.
    pub fn wakes_for(&self, kind: WakeEventKind) -> bool {
        self.enabled
            && match kind {
                WakeEventKind::ActionRaised | WakeEventKind::ActionResolved => {
                    self.required_actions
                }
                WakeEventKind::Errored => self.errors,
                WakeEventKind::TurnFinished => self.turn_finished,
                WakeEventKind::PrOpened | WakeEventKind::PrMerged => self.prs,
            }
    }
}

/// Why held events are not being delivered right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WakeHoldReason {
    /// Collecting the batch window.
    Window,
    /// The orchestrator is mid-turn (or waiting on a question).
    Busy,
    /// The user has messages queued in this chat.
    Queued,
    /// The user has a non-empty composer draft in this chat.
    Draft,
    /// The user interacted with this chat recently.
    RecentActivity,
}

/// One worker event as shown to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeEventView {
    pub id: String,
    pub worker_chat_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_title: Option<String>,
    pub kind: WakeEventKind,
    /// One-line text, exactly what the orchestrator is told.
    pub summary: String,
    pub at_ms: i64,
    /// Told to the orchestrator already.
    pub delivered: bool,
    /// Happened after the orchestrator's last turn started, so the
    /// orchestrator may not have seen it.
    pub unseen: bool,
}

/// Read model for one chat: its settings, what is held, what it has not seen.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeStatus {
    pub chat_id: String,
    /// The chat has workers (child chats or chats under its project's
    /// worker destination), so wakes apply to it.
    pub is_orchestrator: bool,
    #[serde(default)]
    pub settings: WakeSettings,
    /// Events waiting for delivery.
    #[serde(default)]
    pub held: Vec<WakeEventView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_reason: Option<WakeHoldReason>,
    /// When the earliest time-based hold ends (epoch ms), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_delivery_at_ms: Option<i64>,
    /// Recent events (held or delivered) newest first, `unseen` marked.
    #[serde(default)]
    pub recent: Vec<WakeEventView>,
    /// Count of `recent` events with `unseen`.
    #[serde(default)]
    pub unseen_count: u32,
}

impl Default for WakeHoldReason {
    fn default() -> Self {
        Self::Window
    }
}

/// `SetWakeSettings` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetWakeSettingsParams {
    pub chat_id: String,
    pub settings: WakeSettings,
}

/// `NoteChatUserActivity` params: the user is acting in this chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteChatUserActivityParams {
    pub chat_id: String,
    /// The composer draft is non-empty (`None` = unchanged). Held events stay
    /// held while this is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_non_empty: Option<bool>,
}
