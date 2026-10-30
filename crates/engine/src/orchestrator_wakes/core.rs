//! The pure batching and user-first policy behind orchestrator wakes. No I/O,
//! no clock of its own: every call takes `now` (epoch ms), so tests drive it
//! with a fake clock.

use std::collections::{HashMap, VecDeque};

use mast_proto::{
    WakeEventKind, WakeEventView, WakeHoldReason, WakeSettings, WakeStatus,
};

/// How long events are collected before they go out as one message. At most
/// one wake is delivered per window.
pub const WAKE_WINDOW_MS: i64 = 45_000;
/// Events are held while the user interacted with the chat this recently
/// (focused, typed, sent).
pub const USER_QUIET_MS: i64 = 120_000;
/// A delivered event key suppresses an identical event for this long.
pub const DEDUPE_TTL_MS: i64 = 10 * 60_000;
/// Recent events remembered per orchestrator for the "unseen" marks.
const LOG_CAP: usize = 50;

/// One worker event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeEvent {
    pub id: String,
    /// Identity for dedupe (same action re-raised = same key).
    pub key: String,
    pub worker_chat_id: String,
    pub worker_title: Option<String>,
    pub kind: WakeEventKind,
    /// One line, as the orchestrator will read it.
    pub summary: String,
    pub at_ms: i64,
}

/// What the engine knows about the orchestrator chat right now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ctx {
    /// A turn is in flight (running or parked on a question).
    pub busy: bool,
    /// The user has queued messages in this chat.
    pub queue_nonempty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Nothing is held.
    Nothing,
    /// Deliver this batch now.
    Deliver(Batch),
    /// Held until `at_ms`; re-evaluate then.
    WaitUntil { at_ms: i64, reason: WakeHoldReason },
    /// Held until something changes (turn end, queue drain, draft cleared).
    Hold(WakeHoldReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub events: Vec<WakeEvent>,
}

impl Batch {
    /// The one message the orchestrator receives.
    pub fn render(&self) -> String {
        let mut out = String::from(
            "[Mast system message, not from the user: worker updates for you, the \
             orchestrator. Act on them with your Mast tools (work_outline_read_subtree, \
             read_chat, send_message, respond_to_input); ignore ones you've handled.]\n\
             Worker updates:",
        );
        for event in &self.events {
            out.push_str("\n- ");
            out.push_str(&event.summary);
        }
        out
    }
}

#[derive(Debug, Clone)]
struct LogEntry {
    event: WakeEvent,
    delivered: bool,
}

#[derive(Debug, Default)]
struct Orch {
    settings: WakeSettings,
    held: Vec<WakeEvent>,
    /// When the first held event arrived (start of the batch window).
    window_start_ms: Option<i64>,
    last_delivery_ms: Option<i64>,
    last_turn_start_ms: i64,
    draft_non_empty: bool,
    last_user_ms: Option<i64>,
    delivered_keys: HashMap<String, i64>,
    log: VecDeque<LogEntry>,
}

#[derive(Debug, Default)]
pub struct WakeCore {
    orchs: HashMap<String, Orch>,
    seq: u64,
}

impl WakeCore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next_event_id(&mut self) -> String {
        self.seq += 1;
        format!("wake-{}", self.seq)
    }

    pub fn settings(&self, orch: &str) -> WakeSettings {
        self.orchs.get(orch).map(|o| o.settings).unwrap_or_default()
    }

    /// Load or change settings. Turning wakes (or a kind) off drops held
    /// events of the kinds that no longer wake.
    pub fn set_settings(&mut self, orch: &str, settings: WakeSettings) {
        let o = self.orchs.entry(orch.to_owned()).or_default();
        o.settings = settings;
        o.held.retain(|e| settings.wakes_for(e.kind));
        if o.held.is_empty() {
            o.window_start_ms = None;
        }
    }

    pub fn all_settings(&self) -> HashMap<String, WakeSettings> {
        self.orchs
            .iter()
            .filter(|(_, o)| o.settings != WakeSettings::default())
            .map(|(id, o)| (id.clone(), o.settings))
            .collect()
    }

    pub fn has_held(&self, orch: &str) -> bool {
        self.orchs.get(orch).is_some_and(|o| !o.held.is_empty())
    }

    /// Orchestrators with held events, for re-evaluation after a state change.
    pub fn orchestrators_with_held(&self) -> Vec<String> {
        self.orchs
            .iter()
            .filter(|(_, o)| !o.held.is_empty())
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Record a worker event. Always logged (for the unseen marks); held for
    /// delivery only when the settings wake for its kind and it is not a
    /// duplicate. Returns whether it was held.
    pub fn record(&mut self, orch: &str, event: WakeEvent, now: i64) -> bool {
        let o = self.orchs.entry(orch.to_owned()).or_default();
        o.delivered_keys.retain(|_, at| now - *at < DEDUPE_TTL_MS);
        if o.held.iter().any(|e| e.key == event.key) || o.delivered_keys.contains_key(&event.key) {
            return false;
        }
        // Raised then resolved before the orchestrator heard of it: neither
        // needs to be said.
        if event.kind == WakeEventKind::ActionResolved
            && let Some(raised) = o.held.iter().position(|e| {
                e.kind == WakeEventKind::ActionRaised
                    && e.worker_chat_id == event.worker_chat_id
                    && resolved_key(&e.key) == event.key
            })
        {
            o.held.remove(raised);
            if o.held.is_empty() {
                o.window_start_ms = None;
            }
            push_log(o, event, false);
            return false;
        }
        let wakes = o.settings.wakes_for(event.kind);
        push_log(o, event.clone(), false);
        if !wakes {
            return false;
        }
        if o.held.is_empty() {
            o.window_start_ms = Some(now);
        }
        o.held.push(event);
        true
    }

    /// The orchestrator started a turn: everything before now is seen.
    pub fn note_turn_start(&mut self, orch: &str, now: i64) {
        self.orchs.entry(orch.to_owned()).or_default().last_turn_start_ms = now;
    }

    /// The user is acting in this chat.
    pub fn note_user_activity(&mut self, orch: &str, now: i64, draft_non_empty: Option<bool>) {
        let o = self.orchs.entry(orch.to_owned()).or_default();
        o.last_user_ms = Some(now);
        if let Some(draft) = draft_non_empty {
            o.draft_non_empty = draft;
        }
    }

    /// Put back events whose delivery was cancelled before the orchestrator
    /// saw them (the user's message went first).
    pub fn restore(&mut self, orch: &str, events: Vec<WakeEvent>, now: i64) {
        let o = self.orchs.entry(orch.to_owned()).or_default();
        for event in events {
            o.delivered_keys.remove(&event.key);
            if let Some(entry) = o.log.iter_mut().find(|l| l.event.id == event.id) {
                entry.delivered = false;
            }
            if !o.held.iter().any(|e| e.key == event.key) && o.settings.wakes_for(event.kind) {
                o.held.push(event);
            }
        }
        if !o.held.is_empty() && o.window_start_ms.is_none() {
            o.window_start_ms = Some(now);
        }
    }

    /// The hold rules. `force` is "Deliver now": it skips the window, the
    /// draft and the recent-activity hold, but never a running turn or a
    /// queued user message.
    pub fn decide(&mut self, orch: &str, now: i64, ctx: Ctx, force: bool) -> Decision {
        let Some(o) = self.orchs.get_mut(orch) else {
            return Decision::Nothing;
        };
        if o.held.is_empty() {
            return Decision::Nothing;
        }
        if ctx.busy {
            return Decision::Hold(WakeHoldReason::Busy);
        }
        if ctx.queue_nonempty {
            return Decision::Hold(WakeHoldReason::Queued);
        }
        if !force {
            if o.draft_non_empty {
                return Decision::Hold(WakeHoldReason::Draft);
            }
            if let Some(at) = o.last_user_ms.map(|t| t + USER_QUIET_MS)
                && at > now
            {
                return Decision::WaitUntil {
                    at_ms: at,
                    reason: WakeHoldReason::RecentActivity,
                };
            }
            // At most one wake per window: the batch window opens with the
            // first held event and the previous delivery also closes one.
            let window_open = o.window_start_ms.unwrap_or(now) + WAKE_WINDOW_MS;
            let after_last = o.last_delivery_ms.map_or(0, |t| t + WAKE_WINDOW_MS);
            let at = window_open.max(after_last);
            if at > now {
                return Decision::WaitUntil {
                    at_ms: at,
                    reason: WakeHoldReason::Window,
                };
            }
        }
        Decision::Deliver(Batch {
            events: o.held.clone(),
        })
    }

    /// The batch went out: it is no longer held, and its keys are remembered.
    pub fn delivered(&mut self, orch: &str, batch: &Batch, now: i64) {
        let Some(o) = self.orchs.get_mut(orch) else {
            return;
        };
        let ids: Vec<&str> = batch.events.iter().map(|e| e.id.as_str()).collect();
        o.held.retain(|e| !ids.contains(&e.id.as_str()));
        for event in &batch.events {
            o.delivered_keys.insert(event.key.clone(), now);
            if let Some(entry) = o.log.iter_mut().find(|l| l.event.id == event.id) {
                entry.delivered = true;
            }
        }
        o.last_delivery_ms = Some(now);
        o.window_start_ms = if o.held.is_empty() { None } else { Some(now) };
    }

    /// Read model for the UI.
    pub fn status(&mut self, orch: &str, now: i64, ctx: Ctx) -> WakeStatus {
        let decision = self.decide(orch, now, ctx, false);
        let Some(o) = self.orchs.get(orch) else {
            return WakeStatus {
                chat_id: orch.to_owned(),
                ..WakeStatus::default()
            };
        };
        let view = |e: &WakeEvent, delivered: bool| WakeEventView {
            id: e.id.clone(),
            worker_chat_id: e.worker_chat_id.clone(),
            worker_title: e.worker_title.clone(),
            kind: e.kind,
            summary: e.summary.clone(),
            at_ms: e.at_ms,
            delivered,
            unseen: e.at_ms >= o.last_turn_start_ms && !delivered,
        };
        let recent: Vec<WakeEventView> =
            o.log.iter().rev().map(|l| view(&l.event, l.delivered)).collect();
        let (hold_reason, next_delivery_at_ms) = match decision {
            Decision::Hold(r) => (Some(r), None),
            Decision::WaitUntil { at_ms, reason } => (Some(reason), Some(at_ms)),
            Decision::Nothing | Decision::Deliver(_) => (None, None),
        };
        WakeStatus {
            chat_id: orch.to_owned(),
            is_orchestrator: false, // the service stamps this
            settings: o.settings,
            held: o.held.iter().map(|e| view(e, false)).collect(),
            hold_reason,
            next_delivery_at_ms,
            unseen_count: recent.iter().filter(|e| e.unseen).count() as u32,
            recent,
        }
    }
}

fn push_log(o: &mut Orch, event: WakeEvent, delivered: bool) {
    o.log.push_back(LogEntry { event, delivered });
    while o.log.len() > LOG_CAP {
        o.log.pop_front();
    }
}

/// The `ActionResolved` key matching a raised key (`act:<x>` ↔ `res:<x>`).
pub fn resolved_key(raised_key: &str) -> String {
    format!("res:{}", raised_key.strip_prefix("act:").unwrap_or(raised_key))
}
