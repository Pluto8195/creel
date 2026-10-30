//! Worker-event wakes for orchestrator chats.
//!
//! An orchestrator is a chat whose workers are its child chats (`create_chat`
//! sets `parentChatId`) or chats on items under its project's worker
//! destination. The engine watches what those workers do, from the events it
//! already has: the Work Outline snapshot (Required Actions raised/resolved),
//! the session watch (errored, turn finished) and chat rows (PR linked). There
//! is no polling loop; the only timers are one-shot sleeps to the end of a
//! hold window.
//!
//! Events are batched per orchestrator ([`core::WAKE_WINDOW_MS`]) and delivered
//! as ONE system-attributed message, only while the orchestrator is idle and
//! the user is not in the chat. The user always goes first: a user message
//! pulls back a wake that has not started and interrupts an event-started
//! turn (see `DocHost::yield_event_turn_to_user`).

mod core;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use mast_proto::{
    Chat, Session, SessionStatus, WakeEventKind, WakeSettings, WakeStatus, WorkOutlineSnapshot,
};

pub use self::core::{
    Batch, Ctx, DEDUPE_TTL_MS, Decision, USER_QUIET_MS, WAKE_WINDOW_MS, WakeCore, WakeEvent,
    resolved_key,
};
use crate::doc_host::DocHost;
use crate::sessions::SessionsEngine;
use crate::workspace_host::WorkspaceHost;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Epoch milliseconds.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(|| chrono::Utc::now().timestamp_millis())
}

#[derive(Debug)]
struct EventTurn {
    message_id: String,
    events: Vec<WakeEvent>,
    /// The drain took the queue row.
    dispatched: bool,
    /// The turn was seen in flight.
    started: bool,
}

/// Wake messages in flight, shared between the wake service and the doc host
/// (which makes way for the user).
#[derive(Debug, Default)]
pub struct EventTurns {
    turns: Mutex<HashMap<String, EventTurn>>,
    cancelled: Mutex<HashMap<String, Vec<WakeEvent>>>,
}

impl EventTurns {
    pub fn register(&self, chat_id: &str, message_id: String, events: Vec<WakeEvent>) {
        lock(&self.turns).insert(
            chat_id.to_owned(),
            EventTurn {
                message_id,
                events,
                dispatched: false,
                started: false,
            },
        );
    }

    pub fn is_event_message(&self, message_id: &str) -> bool {
        lock(&self.turns)
            .values()
            .any(|t| t.message_id == message_id)
    }

    /// The queue row id of a wake that has not been taken yet.
    pub fn pending_message(&self, chat_id: &str) -> Option<String> {
        lock(&self.turns)
            .get(chat_id)
            .filter(|t| !t.dispatched && !t.started)
            .map(|t| t.message_id.clone())
    }

    /// The queue row was pulled back before it ran: its events go back to
    /// the wake service.
    pub fn cancel_pending(&self, chat_id: &str) {
        if let Some(turn) = lock(&self.turns).remove(chat_id) {
            lock(&self.cancelled)
                .entry(chat_id.to_owned())
                .or_default()
                .extend(turn.events);
        }
    }

    pub fn take_cancelled(&self, chat_id: &str) -> Vec<WakeEvent> {
        lock(&self.cancelled).remove(chat_id).unwrap_or_default()
    }

    pub fn mark_dispatched(&self, chat_id: &str) {
        if let Some(turn) = lock(&self.turns).get_mut(chat_id) {
            turn.dispatched = true;
        }
    }

    /// An event turn is running (or about to).
    pub fn has_started(&self, chat_id: &str) -> bool {
        lock(&self.turns)
            .get(chat_id)
            .is_some_and(|t| t.dispatched || t.started)
    }

    pub fn clear(&self, chat_id: &str) {
        lock(&self.turns).remove(chat_id);
    }

    /// Follow the orchestrator's turn: seen in flight → started; finished
    /// after that → no longer an event turn.
    fn observe(&self, chat_id: &str, in_flight: bool) {
        let mut turns = lock(&self.turns);
        let Some(turn) = turns.get_mut(chat_id) else {
            return;
        };
        if in_flight {
            turn.started = true;
        } else if turn.started {
            turns.remove(chat_id);
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ChatInfo {
    title: Option<String>,
    ticket: Option<String>,
    pr_url: Option<String>,
    parent: Option<String>,
}

#[derive(Default)]
struct State {
    core: WakeCore,
    chats: HashMap<String, ChatInfo>,
    chats_seeded: bool,
    /// Per Space: worker chat → orchestrators by project worker destination.
    destination_parents: HashMap<String, HashMap<String, Vec<String>>>,
    /// Required Action request id → open (unresolved) when last seen.
    requests: HashMap<String, bool>,
    seeded_spaces: HashSet<String>,
    statuses: HashMap<String, SessionStatus>,
    sessions_seeded: bool,
    timers: HashMap<String, i64>,
}

struct Inner {
    state: Mutex<State>,
    sessions: SessionsEngine,
    doc_host: DocHost,
    workspace: WorkspaceHost,
    event_turns: Arc<EventTurns>,
    clock: Clock,
    store: Option<PathBuf>,
}

#[derive(Clone)]
pub struct OrchestratorWakes {
    inner: Arc<Inner>,
}

impl OrchestratorWakes {
    /// Build the service without starting its watchers (tests drive the
    /// `ingest_*` entry points directly).
    pub fn new(
        sessions: SessionsEngine,
        doc_host: DocHost,
        workspace: WorkspaceHost,
        store: Option<PathBuf>,
        clock: Clock,
    ) -> Self {
        let event_turns = doc_host.event_turns();
        let mut state = State::default();
        if let Some(path) = &store
            && let Ok(bytes) = std::fs::read(path)
            && let Ok(saved) = serde_json::from_slice::<HashMap<String, WakeSettings>>(&bytes)
        {
            for (chat, settings) in saved {
                state.core.set_settings(&chat, settings);
            }
        }
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(state),
                sessions,
                doc_host,
                workspace,
                event_turns,
                clock,
                store,
            }),
        }
    }

    /// Build the service and start its event-driven watchers.
    pub fn start(
        sessions: SessionsEngine,
        doc_host: DocHost,
        workspace: WorkspaceHost,
        store: Option<PathBuf>,
    ) -> Self {
        let this = Self::new(sessions, doc_host, workspace, store, system_clock());
        this.spawn_watchers();
        this
    }

    fn now(&self) -> i64 {
        (self.inner.clock)()
    }

    fn spawn_watchers(&self) {
        let this = self.clone();
        tokio::spawn(async move {
            let mut rx = this.inner.sessions.watch_sessions();
            loop {
                let sessions = rx.borrow_and_update().clone();
                this.ingest_sessions(&sessions).await;
                if rx.changed().await.is_err() {
                    break;
                }
            }
        });
        let this = self.clone();
        tokio::spawn(async move {
            let mut rx = this.inner.workspace.watch_chats();
            loop {
                let chats = rx.borrow_and_update().clone();
                this.ingest_chats(&chats).await;
                if rx.changed().await.is_err() {
                    break;
                }
            }
        });
        let this = self.clone();
        tokio::spawn(async move {
            let mut changes = this.inner.workspace.watch_changes();
            let mut markers: HashMap<String, (u64, u64, bool)> = HashMap::new();
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                changes.borrow_and_update();
                let spaces = this.inner.workspace.read_spaces().unwrap_or_default();
                for space in spaces {
                    let marker = this.inner.workspace.work_outline_inputs_marker(&space.id);
                    if markers.get(&space.id) == Some(&marker) {
                        continue;
                    }
                    let workspace = this.inner.workspace.clone();
                    let space_id = space.id.clone();
                    let Ok((marker, snapshot)) = tokio::task::spawn_blocking(move || {
                        let (marker, inputs) = workspace.work_outline_inputs_marked(&space_id);
                        (marker, mast_doc::materialize_work_outline(&space_id, &inputs))
                    })
                    .await
                    else {
                        continue;
                    };
                    markers.insert(space.id.clone(), marker);
                    this.ingest_snapshot(&snapshot).await;
                }
                if changes.changed().await.is_err() {
                    break;
                }
            }
        });
    }

    // ── settings / status / user activity ─────────────────────────────────

    pub fn settings(&self, chat_id: &str) -> WakeSettings {
        lock(&self.inner.state).core.settings(chat_id)
    }

    pub async fn set_settings(&self, chat_id: &str, settings: WakeSettings) -> WakeStatus {
        {
            let mut state = lock(&self.inner.state);
            state.core.set_settings(chat_id, settings);
            self.persist(&state);
        }
        self.evaluate(chat_id, false).await;
        self.status(chat_id)
    }

    fn persist(&self, state: &State) {
        let Some(path) = &self.inner.store else {
            return;
        };
        let map = state.core.all_settings();
        if let Ok(bytes) = serde_json::to_vec_pretty(&map) {
            let _ = std::fs::write(path, bytes);
        }
    }

    pub fn status(&self, chat_id: &str) -> WakeStatus {
        let ctx = self.ctx(chat_id);
        let now = self.now();
        let mut state = lock(&self.inner.state);
        let mut status = state.core.status(chat_id, now, ctx);
        status.is_orchestrator = is_orchestrator(&state, chat_id);
        status
    }

    /// The user is focused / typing / has a draft in `chat_id`.
    pub async fn note_user_activity(&self, chat_id: &str, draft_non_empty: Option<bool>) {
        let now = self.now();
        lock(&self.inner.state)
            .core
            .note_user_activity(chat_id, now, draft_non_empty);
        self.evaluate(chat_id, false).await;
    }

    /// "Deliver now": skips the window and the user-activity hold, never a
    /// running turn or a queued user message.
    pub async fn deliver_now(&self, chat_id: &str) -> WakeStatus {
        self.evaluate(chat_id, true).await;
        self.status(chat_id)
    }

    // ── ingestion (event driven) ──────────────────────────────────────────

    /// Session transitions of workers (errored, turn finished) and of
    /// orchestrators (turn start/end re-evaluates held events).
    pub async fn ingest_sessions(&self, sessions: &[Session]) {
        let now = self.now();
        let mut recorded: HashSet<String> = HashSet::new();
        {
            let mut state = lock(&self.inner.state);
            let seeded = state.sessions_seeded;
            for session in sessions {
                let prev = state.statuses.insert(session.chat_id.clone(), session.status);
                let in_flight = matches!(
                    session.status,
                    SessionStatus::Working | SessionStatus::AwaitingInput
                );
                self.inner.event_turns.observe(&session.chat_id, in_flight);
                if in_flight && prev.is_none_or(|p| !is_in_flight(p)) {
                    state.core.note_turn_start(&session.chat_id, now);
                }
                if !seeded {
                    continue;
                }
                let Some(prev) = prev else { continue };
                let kind = match (is_in_flight(prev), session.status) {
                    (_, s) if s == prev => continue,
                    (_, SessionStatus::Errored) => WakeEventKind::Errored,
                    (true, SessionStatus::Idle) => WakeEventKind::TurnFinished,
                    _ => continue,
                };
                let summary = match kind {
                    WakeEventKind::Errored => "errored (read_chat for details)",
                    _ => "finished a turn",
                };
                let key = format!("{}:{}:{}", kind_tag(kind), session.chat_id, now / WAKE_WINDOW_MS);
                let line = format!("{} — {summary}", label(&state, &session.chat_id));
                for orch in parents_of(&state, &session.chat_id) {
                    let event = make_event(
                        &mut state,
                        &session.chat_id,
                        kind,
                        key.clone(),
                        &line,
                        now,
                    );
                    state.core.record(&orch, event, now);
                    recorded.insert(orch);
                }
            }
            state.sessions_seeded = true;
        }
        self.after_ingest(recorded).await;
    }

    /// Chat rows: parent links, titles, and PR links (PR opened).
    pub async fn ingest_chats(&self, chats: &[Chat]) {
        let now = self.now();
        let mut recorded: HashSet<String> = HashSet::new();
        {
            let mut state = lock(&self.inner.state);
            let seeded = state.chats_seeded;
            for chat in chats {
                let info = ChatInfo {
                    title: chat.title.clone(),
                    ticket: chat.linked_ticket_id.clone(),
                    pr_url: chat.linked_pr_url.clone(),
                    parent: chat.parent_chat_id.clone(),
                };
                let prev = state.chats.insert(chat.id.clone(), info.clone());
                if !seeded {
                    continue;
                }
                if let Some(url) = &info.pr_url
                    && prev.as_ref().is_none_or(|p| p.pr_url.as_ref() != Some(url))
                {
                    let line = format!("{} — PR opened: {url}", label(&state, &chat.id));
                    for orch in parents_of(&state, &chat.id) {
                        let event = make_event(
                            &mut state,
                            &chat.id,
                            WakeEventKind::PrOpened,
                            format!("pr:{}:{url}", chat.id),
                            &line,
                            now,
                        );
                        state.core.record(&orch, event, now);
                        recorded.insert(orch);
                    }
                }
            }
            state.chats_seeded = true;
        }
        self.after_ingest(recorded).await;
    }

    /// A worker's PR merged (fed by whoever learns PR state).
    pub async fn note_pr_merged(&self, worker_chat_id: &str, pr_url: &str) {
        let now = self.now();
        let mut recorded = HashSet::new();
        {
            let mut state = lock(&self.inner.state);
            let line = format!("{} — PR merged: {pr_url}", label(&state, worker_chat_id));
            for orch in parents_of(&state, worker_chat_id) {
                let event = make_event(
                    &mut state,
                    worker_chat_id,
                    WakeEventKind::PrMerged,
                    format!("prm:{worker_chat_id}:{pr_url}"),
                    &line,
                    now,
                );
                state.core.record(&orch, event, now);
                recorded.insert(orch);
            }
        }
        self.after_ingest(recorded).await;
    }

    /// Required Actions raised/resolved, plus the project-destination
    /// worker relation, from one Space's snapshot.
    pub async fn ingest_snapshot(&self, snapshot: &WorkOutlineSnapshot) {
        if !snapshot.available {
            return;
        }
        let now = self.now();
        let mut recorded: HashSet<String> = HashSet::new();
        {
            let mut state = lock(&self.inner.state);
            state
                .destination_parents
                .insert(snapshot.space_id.clone(), destination_parents(snapshot));
            let seeded = !state.seeded_spaces.insert(snapshot.space_id.clone());
            for request in &snapshot.user_action_requests {
                let open = request.is_unresolved();
                let key = format!(
                    "act:{}:{}",
                    request.work_item_id,
                    request.dedupe_key.as_deref().unwrap_or(&request.text)
                );
                let prev = state.requests.insert(request.id.clone(), open);
                if !seeded || request.chat_id.is_empty() {
                    continue;
                }
                let was_open = prev.unwrap_or(false);
                let (kind, event_key, text) = if open && !was_open {
                    let mut text = request.text.trim().to_owned();
                    if let Some(info) = request.info.as_deref().map(str::trim)
                        && !info.is_empty()
                    {
                        text = format!("{text}: {}", truncate(info, 160));
                    }
                    (WakeEventKind::ActionRaised, key, text)
                } else if !open && was_open {
                    (
                        WakeEventKind::ActionResolved,
                        resolved_key(&key),
                        format!("resolved: {}", request.text.trim()),
                    )
                } else {
                    continue;
                };
                let id8 = request.id.chars().take(8).collect::<String>();
                let mut line = format!("{} — {text}", label(&state, &request.chat_id));
                if let Some(pr) = state.chats.get(&request.chat_id).and_then(|c| c.pr_url.clone()) {
                    line.push_str(&format!(" (PR {pr})"));
                }
                if kind == WakeEventKind::ActionRaised {
                    line.push_str(&format!(" [request {id8}]"));
                }
                for orch in parents_of(&state, &request.chat_id) {
                    let event = make_event(
                        &mut state,
                        &request.chat_id,
                        kind,
                        event_key.clone(),
                        &line,
                        now,
                    );
                    state.core.record(&orch, event, now);
                    recorded.insert(orch);
                }
            }
        }
        self.after_ingest(recorded).await;
    }

    async fn after_ingest(&self, recorded: HashSet<String>) {
        // Orchestrators that recorded something, plus any holding events
        // whose blocker (a running turn) may just have ended.
        let mut targets: HashSet<String> = recorded;
        targets.extend(lock(&self.inner.state).core.orchestrators_with_held());
        for orch in targets {
            self.evaluate(&orch, false).await;
        }
    }

    // ── delivery ──────────────────────────────────────────────────────────

    fn ctx(&self, orch: &str) -> Ctx {
        let queue_nonempty = self
            .inner
            .doc_host
            .open(orch)
            .ok()
            .and_then(|h| h.doc().read_queue().ok())
            .is_some_and(|q| !q.is_empty());
        Ctx {
            busy: self.inner.sessions.turn_in_flight(orch),
            queue_nonempty,
        }
    }

    /// Whether this device hosts `orch` (only the host delivers).
    fn hosted_here(&self, orch: &str) -> bool {
        self.inner
            .workspace
            .chat(orch)
            .ok()
            .flatten()
            .is_some_and(|c| c.device_id == self.inner.doc_host.device_id())
    }

    /// Decide for one orchestrator and act: deliver, arm a one-shot timer, or
    /// keep holding until the next event (turn end, draft cleared).
    pub async fn evaluate(&self, orch: &str, force: bool) {
        let now = self.now();
        let cancelled = self.inner.event_turns.take_cancelled(orch);
        let ctx = self.ctx(orch);
        let decision = {
            let mut state = lock(&self.inner.state);
            if !cancelled.is_empty() {
                state.core.restore(orch, cancelled, now);
            }
            state.core.decide(orch, now, ctx, force)
        };
        match decision {
            Decision::Nothing | Decision::Hold(_) => {}
            Decision::WaitUntil { at_ms, .. } => self.arm_timer(orch, at_ms),
            Decision::Deliver(batch) => {
                if !self.hosted_here(orch) {
                    return;
                }
                let text = batch.render();
                match self.inner.doc_host.queue_event_message(orch, &text) {
                    Ok(message_id) => {
                        self.inner
                            .event_turns
                            .register(orch, message_id, batch.events.clone());
                        lock(&self.inner.state).core.delivered(orch, &batch, now);
                    }
                    Err(err) => {
                        tracing::warn!(chat = %orch, error = %err, "orchestrator wake delivery failed");
                    }
                }
            }
        }
    }

    fn arm_timer(&self, orch: &str, at_ms: i64) {
        let now = self.now();
        {
            let mut state = lock(&self.inner.state);
            if state.timers.get(orch).is_some_and(|&t| t > now && t <= at_ms) {
                return; // an earlier timer re-evaluates (and re-arms) first
            }
            state.timers.insert(orch.to_owned(), at_ms);
        }
        let this = self.clone();
        let orch = orch.to_owned();
        let wait = (at_ms - now).max(0) as u64;
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
            {
                let mut state = lock(&this.inner.state);
                if state.timers.get(&orch) == Some(&at_ms) {
                    state.timers.remove(&orch);
                }
            }
            this.evaluate(&orch, false).await;
        });
    }
}

fn is_in_flight(status: SessionStatus) -> bool {
    matches!(status, SessionStatus::Working | SessionStatus::AwaitingInput)
}

fn kind_tag(kind: WakeEventKind) -> &'static str {
    match kind {
        WakeEventKind::ActionRaised => "act",
        WakeEventKind::ActionResolved => "res",
        WakeEventKind::Errored => "err",
        WakeEventKind::TurnFinished => "turn",
        WakeEventKind::PrOpened => "pr",
        WakeEventKind::PrMerged => "prm",
    }
}

fn truncate(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max).collect();
    format!("{cut}…")
}

/// "ENG-2818" when the worker has a linked ticket, else its title, then the
/// chat id prefix the tools accept.
fn label(state: &State, chat_id: &str) -> String {
    let short: String = chat_id.chars().take(12).collect();
    let info = state.chats.get(chat_id);
    let name = info
        .and_then(|c| c.ticket.clone())
        .or_else(|| info.and_then(|c| c.title.clone()))
        .filter(|n| !n.trim().is_empty());
    match name {
        Some(name) => format!("{} (chat {short})", name.trim()),
        None => format!("chat {short}"),
    }
}

fn make_event(
    state: &mut State,
    worker: &str,
    kind: WakeEventKind,
    key: String,
    summary: &str,
    now: i64,
) -> WakeEvent {
    WakeEvent {
        id: state.core.next_event_id(),
        key,
        worker_chat_id: worker.to_owned(),
        worker_title: state.chats.get(worker).and_then(|c| c.title.clone()),
        kind,
        summary: summary.to_owned(),
        at_ms: now,
    }
}

/// Orchestrators told about `worker`: its parent chat, and the chats whose
/// project worker destination contains the worker's item.
fn parents_of(state: &State, worker: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(parent) = state.chats.get(worker).and_then(|c| c.parent.clone())
        && parent != worker
    {
        out.push(parent);
    }
    for map in state.destination_parents.values() {
        for orch in map.get(worker).into_iter().flatten() {
            if orch != worker && !out.contains(orch) {
                out.push(orch.clone());
            }
        }
    }
    out
}

fn is_orchestrator(state: &State, chat_id: &str) -> bool {
    state
        .chats
        .values()
        .any(|c| c.parent.as_deref() == Some(chat_id))
        || state
            .destination_parents
            .values()
            .any(|m| m.values().any(|orchs| orchs.iter().any(|o| o == chat_id)))
}

/// Worker chat → orchestrators, for one Space: a chat on item X is the
/// orchestrator of every chat on an item inside X's project worker
/// destination (`worker_destination_for`).
fn destination_parents(snapshot: &WorkOutlineSnapshot) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for item in &snapshot.items {
        if item.chat_ids.is_empty() {
            continue;
        }
        let Some(destination) = snapshot.worker_destination_for(&item.id) else {
            continue;
        };
        for worker_item in &snapshot.items {
            if worker_item.chat_ids.is_empty() || !snapshot.is_within(destination, &worker_item.id)
            {
                continue;
            }
            for worker in &worker_item.chat_ids {
                for orch in &item.chat_ids {
                    if orch != worker {
                        let list = out.entry(worker.clone()).or_default();
                        if !list.contains(orch) {
                            list.push(orch.clone());
                        }
                    }
                }
            }
        }
    }
    out
}
