//! Worker-event wakes for orchestrator chats, against a fake harness and a
//! fake clock: batching, idle-only delivery, the user-first holds, the
//! interrupt of an event-started turn, settings, dedupe and non-children.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use mast_engine::orchestrator_wakes::{Clock, OrchestratorWakes, USER_QUIET_MS, WAKE_WINDOW_MS};
use mast_engine::{EngineCore, HarnessRegistry};
use mast_harness::{Harness, HarnessError, RunControls};
use mast_proto::{
    AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, Session, SessionStatus,
    SteeringMode, WakeEventKind, WakeSettings, WorkOutlineSnapshot,
};

const ORCH: &str = "orch";

/// A turn that ends only when the test says so.
struct HeldHarness {
    finish: tokio::sync::broadcast::Sender<()>,
    prompts: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Harness for HeldHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Held"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.prompts.lock().unwrap().push(request.prompt.clone());
        let mut finish = self.finish.subscribe();
        let started = futures::stream::iter(vec![Ok(AgentEvent::SessionStarted {
            harness: HarnessId::Mock,
            model: "mock-1".into(),
            tools: vec![],
            cwd: request.cwd.clone(),
            session_id: "sess-wake".into(),
            assistant_message_id: format!("a-{}", request.prompt.len()),
        })]);
        let done = futures::stream::once(async move {
            let _ = finish.recv().await;
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("sess-wake".into()),
            })
        });
        Ok(started.chain(done).boxed())
    }
}

struct Rig {
    core: EngineCore,
    wakes: OrchestratorWakes,
    clock: Arc<AtomicI64>,
    finish: tokio::sync::broadcast::Sender<()>,
    prompts: Arc<Mutex<Vec<String>>>,
    store: std::path::PathBuf,
}

impl Rig {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.keep();
        let (finish, _) = tokio::sync::broadcast::channel(16);
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let registry = HarnessRegistry::new();
        registry.register(Arc::new(HeldHarness {
            finish: finish.clone(),
            prompts: prompts.clone(),
        }));
        let core = EngineCore::assemble(
            &path.join("data"),
            Arc::new(registry),
            HarnessId::Mock,
            None,
        )
        .expect("engine core assembles");
        let store = path.join("wakes.json");
        let clock = Arc::new(AtomicI64::new(1_000_000));
        let tick = clock.clone();
        let clock_fn: Clock = Arc::new(move || tick.load(Ordering::SeqCst));
        // A separate, inert service on the fake clock; the core's own one
        // keeps watching on the real clock and never reaches its window.
        let wakes = OrchestratorWakes::new(
            core.sessions.clone(),
            core.doc_host.clone(),
            core.workspace.clone(),
            Some(store.clone()),
            clock_fn,
        );
        let rig = Self {
            core,
            wakes,
            clock,
            finish,
            prompts,
            store,
        };
        rig.chat(ORCH, None);
        rig.chat("w1", Some(ORCH));
        rig.chat("w2", Some(ORCH));
        rig.chat("stranger", None);
        rig.wakes
            .ingest_chats(&rig.core.workspace.read_chats().unwrap())
            .await;
        rig.wakes
            .ingest_sessions(&[
                session("w1", SessionStatus::Working),
                session("w2", SessionStatus::Working),
                session("stranger", SessionStatus::Working),
            ])
            .await;
        rig
    }

    fn chat(&self, id: &str, parent: Option<&str>) {
        self.core
            .workspace
            .create_chat_with_parent(
                id,
                None,
                Some(&self.core.device_id),
                None,
                None,
                parent.map(str::to_owned),
            )
            .unwrap();
        self.core
            .workspace
            .rename_chat(id, &format!("Title {id}"))
            .unwrap();
    }

    fn advance(&self, ms: i64) {
        self.clock.fetch_add(ms, Ordering::SeqCst);
    }

    async fn worker_errors(&self, id: &str) {
        self.wakes
            .ingest_sessions(&[session(id, SessionStatus::Errored)])
            .await;
    }

    fn wake_prompts(&self) -> Vec<String> {
        self.prompts
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.contains("Worker updates:"))
            .cloned()
            .collect()
    }

    async fn wait_for<F: FnMut() -> bool>(&self, mut f: F, what: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !f() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    }

    async fn settle(&self) {
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

fn session(chat: &str, status: SessionStatus) -> Session {
    Session {
        last_completed_turn: None,
        chat_id: chat.into(),
        device_id: "d".into(),
        status,
        started_at: None,
        updated_at: chrono::Utc::now(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_errors_batch_into_one_system_message_after_the_window() {
    let rig = Rig::new().await;
    rig.worker_errors("w1").await;
    rig.advance(10_000);
    rig.worker_errors("w2").await;
    // Not before the window ends.
    rig.advance(10_000);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty());
    assert_eq!(rig.wakes.status(ORCH).held.len(), 2);

    rig.advance(WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(|| !rig.wake_prompts().is_empty(), "the wake message")
        .await;
    let prompts = rig.wake_prompts();
    assert_eq!(prompts.len(), 1, "ONE message for both workers");
    assert!(prompts[0].contains("Title w1") && prompts[0].contains("Title w2"));
    assert!(prompts[0].contains("not from the user"));
    assert!(rig.wakes.status(ORCH).held.is_empty());
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_children_wake_their_orchestrator() {
    let rig = Rig::new().await;
    rig.worker_errors("stranger").await;
    rig.advance(10 * WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty());
    let status = rig.wakes.status(ORCH);
    assert!(status.is_orchestrator);
    assert!(status.held.is_empty() && status.recent.is_empty());
    assert!(!rig.wakes.status("w1").is_orchestrator);
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_turn_holds_the_batch_until_it_ends() {
    let rig = Rig::new().await;
    rig.core
        .doc_host
        .queue_message(ORCH, "opening", Vec::new())
        .unwrap();
    rig.wait_for(
        || rig.core.sessions.turn_in_flight(ORCH),
        "the orchestrator's turn",
    )
    .await;
    rig.worker_errors("w1").await;
    rig.advance(10 * WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty(), "never delivered mid-turn");
    // Even Deliver now cannot interrupt the turn.
    rig.wakes.deliver_now(ORCH).await;
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty());
    assert_eq!(rig.wakes.status(ORCH).held.len(), 1);

    let _ = rig.finish.send(());
    rig.wait_for(
        || !rig.core.sessions.turn_in_flight(ORCH),
        "the turn to end",
    )
    .await;
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(|| !rig.wake_prompts().is_empty(), "delivery after the turn")
        .await;
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_draft_and_recent_activity_hold_delivery() {
    let rig = Rig::new().await;
    rig.worker_errors("w1").await;
    rig.wakes.note_user_activity(ORCH, Some(true)).await;
    rig.advance(10 * USER_QUIET_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(
        rig.wake_prompts().is_empty(),
        "a draft holds however long it sits"
    );
    assert_eq!(
        rig.wakes.status(ORCH).hold_reason,
        Some(mast_proto::WakeHoldReason::Draft)
    );

    rig.advance(1_000);
    rig.wakes.note_user_activity(ORCH, Some(false)).await;
    rig.advance(USER_QUIET_MS / 2);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(
        rig.wake_prompts().is_empty(),
        "the user was here a minute ago"
    );

    rig.advance(USER_QUIET_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(
        || !rig.wake_prompts().is_empty(),
        "delivery once the user is gone",
    )
    .await;
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deliver_now_skips_the_window_and_the_draft() {
    let rig = Rig::new().await;
    rig.worker_errors("w1").await;
    rig.wakes.note_user_activity(ORCH, Some(true)).await;
    rig.wakes.deliver_now(ORCH).await;
    rig.wait_for(|| !rig.wake_prompts().is_empty(), "the forced delivery")
        .await;
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_message_interrupts_an_event_started_turn_and_runs_next() {
    let rig = Rig::new().await;
    rig.worker_errors("w1").await;
    rig.advance(WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(|| !rig.wake_prompts().is_empty(), "the event turn to start")
        .await;
    rig.wait_for(
        || rig.core.sessions.turn_in_flight(ORCH),
        "the event turn in flight",
    )
    .await;
    // The wake service sees the turn running (the session watch does this).
    rig.wakes
        .ingest_sessions(&[session(ORCH, SessionStatus::Working)])
        .await;

    // Mikey types: the event turn is interrupted (it never finished — the
    // test never sent `finish`), and his message is the very next turn.
    rig.core
        .doc_host
        .queue_message(ORCH, "hello from the user", Vec::new())
        .unwrap();
    rig.wait_for(
        || {
            rig.prompts
                .lock()
                .unwrap()
                .iter()
                .any(|p| p == "hello from the user")
        },
        "the user's message to run next",
    )
    .await;
    let prompts = rig.prompts.lock().unwrap().clone();
    let wake = prompts
        .iter()
        .position(|p| p.contains("Worker updates:"))
        .unwrap();
    let user = prompts
        .iter()
        .position(|p| p == "hello from the user")
        .unwrap();
    assert!(wake < user);
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_default_and_gate_event_kinds() {
    let rig = Rig::new().await;
    assert_eq!(rig.wakes.settings(ORCH), WakeSettings::default());
    // Turn-finished is off by default: it is logged (unseen mark) not held.
    rig.wakes
        .ingest_sessions(&[session("w1", SessionStatus::Idle)])
        .await;
    let status = rig.wakes.status(ORCH);
    assert!(status.held.is_empty());
    assert_eq!(status.recent.len(), 1);
    assert_eq!(status.recent[0].kind, WakeEventKind::TurnFinished);
    assert!(status.recent[0].unseen);

    rig.wakes
        .set_settings(
            ORCH,
            WakeSettings {
                turn_finished: true,
                ..WakeSettings::default()
            },
        )
        .await;
    rig.wakes
        .ingest_sessions(&[session("w2", SessionStatus::Idle)])
        .await;
    assert_eq!(rig.wakes.status(ORCH).held.len(), 1);

    // Master switch off clears what was held and holds nothing new.
    rig.wakes
        .set_settings(
            ORCH,
            WakeSettings {
                enabled: false,
                ..WakeSettings::default()
            },
        )
        .await;
    assert!(rig.wakes.status(ORCH).held.is_empty());
    rig.worker_errors("w1").await;
    assert!(rig.wakes.status(ORCH).held.is_empty());
    rig.advance(10 * WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.settle().await;
    assert!(rig.wake_prompts().is_empty());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_persist_across_service_restarts() {
    let rig = Rig::new().await;
    let custom = WakeSettings {
        prs: true,
        errors: false,
        ..WakeSettings::default()
    };
    rig.wakes.set_settings(ORCH, custom).await;
    let again = OrchestratorWakes::new(
        rig.core.sessions.clone(),
        rig.core.doc_host.clone(),
        rig.core.workspace.clone(),
        Some(rig.store.clone()),
        Arc::new(|| 0),
    );
    assert_eq!(again.settings(ORCH), custom);
    assert_eq!(again.settings("someone-else"), WakeSettings::default());
    rig.core.shutdown().await;
}

fn snapshot(requests: serde_json::Value) -> WorkOutlineSnapshot {
    serde_json::from_value(serde_json::json!({
        "spaceId": "s1", "available": true, "synced": true,
        "userActionRequests": requests,
    }))
    .unwrap()
}

fn request(id: &str, chat: &str, text: &str, state: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "spaceId": "s1", "workItemId": "item1", "chatId": chat,
        "text": text, "dedupeKey": id, "createdAt": 1, "state": state,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn required_actions_raised_wake_once_and_dedupe() {
    let rig = Rig::new().await;
    // The first snapshot only seeds what already exists.
    rig.wakes
        .ingest_snapshot(&snapshot(serde_json::json!([request(
            "r0", "w1", "old", "open"
        )])))
        .await;
    assert!(rig.wakes.status(ORCH).held.is_empty());

    let raised = snapshot(serde_json::json!([
        request("r0", "w1", "old", "open"),
        request("r1", "w1", "Ready to merge: parser-fix", "open"),
        request("r2", "stranger", "not mine", "open"),
    ]));
    rig.wakes.ingest_snapshot(&raised).await;
    rig.wakes.ingest_snapshot(&raised).await; // a re-read of the same state
    let held = rig.wakes.status(ORCH).held;
    assert_eq!(
        held.len(),
        1,
        "one event; strangers and repeats are dropped"
    );
    assert!(held[0].summary.contains("Ready to merge: parser-fix"));
    assert!(held[0].summary.contains("request r1"));

    rig.advance(WAKE_WINDOW_MS);
    rig.wakes.evaluate(ORCH, false).await;
    rig.wait_for(
        || !rig.wake_prompts().is_empty(),
        "the Required Action wake",
    )
    .await;
    assert!(rig.wake_prompts()[0].contains("Ready to merge: parser-fix"));

    // Re-raised after delivery: the same action is not news again.
    let resolved = snapshot(serde_json::json!([request(
        "r1",
        "w1",
        "Ready to merge: parser-fix",
        "resolved"
    )]));
    rig.wakes.ingest_snapshot(&resolved).await;
    let held = rig.wakes.status(ORCH).held;
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].kind, WakeEventKind::ActionResolved);
    let _ = rig.finish.send(());
    rig.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raised_and_resolved_inside_one_window_say_nothing() {
    let rig = Rig::new().await;
    rig.wakes
        .ingest_snapshot(&snapshot(serde_json::json!([])))
        .await;
    rig.wakes
        .ingest_snapshot(&snapshot(serde_json::json!([request(
            "r1",
            "w1",
            "Approve plan",
            "open"
        )])))
        .await;
    assert_eq!(rig.wakes.status(ORCH).held.len(), 1);
    rig.wakes
        .ingest_snapshot(&snapshot(serde_json::json!([request(
            "r1",
            "w1",
            "Approve plan",
            "resolved"
        )])))
        .await;
    assert!(rig.wakes.status(ORCH).held.is_empty());
    rig.core.shutdown().await;
}
