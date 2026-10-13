//! Work Outline runtime instruction delivery, end to end through the command
//! plane with the instruction-aware mock harness and a fake registry
//! (acceptance 9-12; `m0-contract.md` 5.3-5.7):
//! - fresh Run, warm next Turn, queued Turn, crash respawn, restart recovery
//!   (native resume), explicit revision change;
//! - a queued Turn is delivered with the revision captured at queue time even
//!   when a newer one exists;
//! - a revision applied during a running Turn starts the next Turn only;
//! - unsupported harnesses, integrity failures, and missing revisions reject
//!   without writing the user message and never substitute another revision.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zeron_doc::{
    MessageRole, SessionCommandEntry, SessionCommandPayload, SessionCommandStatus,
    SessionMessageEntry,
};
use zeron_engine::instruction_delivery::RevisionWait;
use zeron_engine::instruction_registry::InstructionRegistry;
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::mock::InstructionMockHarness;
use zeron_harness::{InstructionChannel, InstructionReplaceStrategy, InstructionSupport};
use zeron_proto::{
    ExecutionSessionReport, HarnessId, InstructionRevisionEntry, InstructionRevisionIntegrity,
    InstructionRevisionPin, InstructionRevisionView, InstructionSourceKind, InstructionTransition,
    RunRequest, SandboxLevel, build_revision_body,
};

const CHAT: &str = "chat-instructions";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FakeRegistry {
    revisions: Mutex<HashMap<String, InstructionRevisionView>>,
    selected: Mutex<Option<InstructionRevisionPin>>,
    reports: Mutex<Vec<ExecutionSessionReport>>,
}

impl FakeRegistry {
    /// Store a revision with one Space-root entry and return its pin.
    fn add(&self, id: &str, text: &str) -> InstructionRevisionPin {
        let body = build_revision_body(vec![InstructionRevisionEntry {
            instruction_id: format!("{id}-i1"),
            source_kind: InstructionSourceKind::SpaceRoot,
            scope_id: "space".into(),
            scope_title: "Space root".into(),
            text: text.into(),
            order: 0,
            provenance: Default::default(),
        }])
        .unwrap();
        let pin = InstructionRevisionPin {
            revision_id: id.into(),
            revision_hash: body.hash.clone(),
        };
        self.revisions.lock().unwrap().insert(
            id.into(),
            InstructionRevisionView {
                id: id.into(),
                space_id: "space".into(),
                work_item_id: "item".into(),
                body,
                created_by: None,
                created_at: 0,
                integrity: InstructionRevisionIntegrity::Ok,
            },
        );
        pin
    }

    fn select(&self, pin: &InstructionRevisionPin) {
        *self.selected.lock().unwrap() = Some(pin.clone());
    }

    /// What a `keepSession` application without a revision selects.
    fn clear_selection(&self) {
        *self.selected.lock().unwrap() = None;
    }

    fn reports(&self) -> Vec<ExecutionSessionReport> {
        self.reports.lock().unwrap().clone()
    }
}

impl InstructionRegistry for FakeRegistry {
    fn instruction_revision(&self, revision_id: &str) -> Option<InstructionRevisionView> {
        self.revisions.lock().unwrap().get(revision_id).cloned()
    }
    fn selected_instruction_revision(&self, _chat_id: &str) -> Option<InstructionRevisionPin> {
        self.selected.lock().unwrap().clone()
    }
    fn report_execution_session(&self, _chat_id: &str, report: ExecutionSessionReport) {
        let mut reports = self.reports.lock().unwrap();
        if !reports.contains(&report) {
            reports.push(report);
        }
    }
}

struct Rig {
    core: EngineCore,
    mock: InstructionMockHarness,
    registry: Arc<FakeRegistry>,
}

fn assemble(
    dir: &std::path::Path,
    mock: InstructionMockHarness,
    registry: Arc<FakeRegistry>,
) -> Rig {
    let harnesses = HarnessRegistry::new();
    harnesses.register(Arc::new(mock.clone()));
    let core = EngineCore::assemble(dir, Arc::new(harnesses), HarnessId::Mock, None)
        .expect("engine core assembles");
    core.doc_host
        .set_instruction_registry(registry.clone() as Arc<dyn InstructionRegistry>);
    core.doc_host.set_revision_wait(RevisionWait {
        max: Duration::from_millis(1_500),
        initial: Duration::from_millis(50),
        max_step: Duration::from_millis(200),
    });
    Rig {
        core,
        mock,
        registry,
    }
}

fn rig(dir: &std::path::Path) -> Rig {
    let rig = assemble(
        dir,
        InstructionMockHarness::default(),
        Arc::new(FakeRegistry::default()),
    );
    pre_title(&rig.core);
    rig
}

/// Create + title the chat up front so the auto-titler stays out of the
/// harness's recorded runs.
fn pre_title(core: &EngineCore) {
    core.workspace
        .create_space("space-instr", &core.device_id, "/tmp", None, false)
        .expect("create space row");
    core.workspace
        .create_chat(CHAT, Some("space-instr"), None, None, None)
        .expect("create chat row");
    core.workspace
        .rename_chat(CHAT, "Pre-titled")
        .expect("rename chat");
}

fn run_request(prompt: &str) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        harness: Some(HarnessId::Mock),
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: "/tmp".into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
        instructions: None,
    }
}

fn pinned(prompt: &str, message_id: &str, pin: &InstructionRevisionPin) -> SessionCommandPayload {
    SessionCommandPayload::RunWithInstructions {
        request: run_request(prompt),
        message_id: message_id.into(),
        instructions: pin.clone(),
        context_reset_confirmed: false,
    }
}

fn queue(core: &EngineCore, payload: SessionCommandPayload) -> String {
    core.doc_host
        .queue_command(CHAT, payload)
        .expect("queue command")
}

fn command(core: &EngineCore, id: &str) -> Option<SessionCommandEntry> {
    core.doc_host
        .open(CHAT)
        .ok()?
        .doc()
        .read_commands()
        .ok()?
        .into_iter()
        .find(|c| c.id == id)
}

fn entries(core: &EngineCore) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(CHAT)
        .ok()
        .and_then(|h| h.doc().read_entries().ok())
        .unwrap_or_default()
}

fn has_user_message(core: &EngineCore, id: &str) -> bool {
    entries(core)
        .iter()
        .any(|e| e.role == MessageRole::User && e.id == id)
}

async fn wait_for(mut predicate: impl FnMut() -> bool, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

async fn wait_resolved(core: &EngineCore, id: &str) -> SessionCommandEntry {
    wait_for(
        || command(core, id).is_some_and(|c| c.status != SessionCommandStatus::Pending),
        "the command to resolve",
    )
    .await;
    command(core, id).unwrap()
}

async fn wait_turns(rig: &Rig, n: usize, what: &str) {
    wait_for(|| rig.mock.turns().len() >= n, what).await;
    // Let the Turn settle (Done folds, status goes idle) before the next send.
    wait_for(
        || !rig.core.sessions.turn_in_flight(CHAT),
        "the turn to settle",
    )
    .await;
}

fn revision_of(turn: &zeron_harness::mock::RecordedInstructionTurn) -> Option<&str> {
    turn.revision_id.as_deref()
}

// ---------------------------------------------------------------------------
// Acceptance 9: fresh Run, warm next Turn, explicit revision change
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fresh_run_warm_turn_and_explicit_change_deliver_exact_revisions() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = rig(tmp.path());
    let a = rig.registry.add("rev-a", "Prefer small commits");
    let b = rig.registry.add("rev-b", "Write tests first");
    rig.registry.select(&a);

    // Fresh Run.
    let first = queue(&rig.core, pinned("first", "m-1", &a));
    assert_eq!(
        wait_resolved(&rig.core, &first).await.status,
        SessionCommandStatus::Applied
    );
    wait_turns(&rig, 1, "the fresh Turn").await;
    let runs = rig.mock.runs();
    assert_eq!(runs.len(), 1);
    let delivery = runs[0]
        .instructions
        .as_ref()
        .expect("fresh run carries instructions");
    assert_eq!(delivery.envelope.pin(), a);
    assert_eq!(delivery.transition, InstructionTransition::Fresh);
    assert!(runs[0].resume.is_none());
    assert!(delivery.envelope.rendered.contains("Prefer small commits"));
    // Separate from the visible prompt.
    assert_eq!(runs[0].prompt, "first");
    assert!(
        entries(&rig.core)
            .iter()
            .all(|e| !format!("{:?}", e.parts).contains("zeron_work_outline_instructions"))
    );
    // The host reported what the Execution Session received.
    wait_for(|| !rig.registry.reports().is_empty(), "the session report").await;
    let report = &rig.registry.reports()[0];
    assert_eq!(report.revision_id.as_deref(), Some("rev-a"));
    assert_eq!(
        report.revision_hash.as_deref(),
        Some(a.revision_hash.as_str())
    );
    assert_eq!(report.native_resume_id.as_deref(), Some("mock-session-1"));
    assert_eq!(report.harness, HarnessId::Mock);

    // Warm next Turn, same revision: the live Execution Session takes it.
    queue(&rig.core, pinned("second", "m-2", &a));
    wait_turns(&rig, 2, "the warm Turn").await;
    assert_eq!(rig.mock.runs().len(), 1, "same Execution Session");
    let turns = rig.mock.turns();
    assert_eq!(turns[1].prompt, "second");
    assert_eq!(turns[1].run_index, 0);
    assert_eq!(revision_of(&turns[1]), Some("rev-a"));

    // Explicit revision change at the Turn boundary: a new Execution Session
    // resumes the same native conversation with the new revision.
    rig.registry.select(&b);
    queue(&rig.core, pinned("third", "m-3", &b));
    wait_turns(&rig, 3, "the Turn after the change").await;
    let runs = rig.mock.runs();
    assert_eq!(runs.len(), 2, "a new Execution Session");
    assert_eq!(runs[1].resume.as_deref(), Some("mock-session-1"));
    let delivery = runs[1].instructions.as_ref().unwrap();
    assert_eq!(delivery.envelope.pin(), b);
    assert_eq!(
        delivery.transition,
        InstructionTransition::Replace {
            previous_revision_id: "rev-a".into()
        }
    );
    assert_eq!(revision_of(&rig.mock.turns()[2]), Some("rev-b"));
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Capture at ingestion: clients send plain `Run`
// ---------------------------------------------------------------------------

fn plain(prompt: &str, message_id: &str) -> SessionCommandPayload {
    SessionCommandPayload::Run {
        request: run_request(prompt),
        message_id: message_id.into(),
    }
}

/// Append a command as another device's (possibly older) sender would: it
/// never passes through this engine's ingestion.
fn write_foreign_command(core: &EngineCore, id: &str, payload: SessionCommandPayload) {
    core.doc_host
        .open(CHAT)
        .unwrap()
        .doc()
        .queue_command(&SessionCommandEntry {
            id: id.into(),
            payload,
            issued_by: "other-device".into(),
            issued_at: chrono::Utc::now().timestamp_millis(),
            based_on: None,
            expires_at: None,
            status: SessionCommandStatus::Pending,
            resolution: None,
        })
        .unwrap();
}

#[tokio::test]
async fn a_plain_run_after_apply_delivers_the_new_revision_at_the_next_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = rig(tmp.path());
    let a = rig.registry.add("rev-a", "Prefer small commits");
    rig.registry.select(&a);

    let first = queue(&rig.core, plain("first", "m-1"));
    // The ingesting engine captured the selection on the written command.
    assert!(matches!(
        command(&rig.core, &first).unwrap().payload,
        SessionCommandPayload::RunWithInstructions { ref instructions, .. } if *instructions == a
    ));
    wait_turns(&rig, 1, "the first Turn").await;
    assert_eq!(revision_of(&rig.mock.turns()[0]), Some("rev-a"));

    // The user applies rev-b; the composer still sends a plain Run.
    let b = rig.registry.add("rev-b", "Write tests first");
    rig.registry.select(&b);
    queue(&rig.core, plain("second", "m-2"));
    wait_turns(&rig, 2, "the Turn after Apply").await;
    let runs = rig.mock.runs();
    assert_eq!(runs.len(), 2, "a new Execution Session at the boundary");
    assert_eq!(runs[1].resume.as_deref(), Some("mock-session-1"));
    let delivery = runs[1].instructions.as_ref().unwrap();
    assert_eq!(
        delivery.envelope.pin(),
        b,
        "the new revision, not the session's"
    );
    assert_eq!(
        delivery.transition,
        InstructionTransition::Replace {
            previous_revision_id: "rev-a".into()
        }
    );

    // A plain Run from a sender that skipped capture is still a new user
    // message on the host: it prefers the selection too.
    let c = rig.registry.add("rev-c", "Explain before editing");
    rig.registry.select(&c);
    write_foreign_command(&rig.core, "cmd-foreign", plain("third", "m-3"));
    let handle = rig.core.doc_host.open(CHAT).unwrap();
    rig.core.doc_host.drain_commands(&handle).await;
    wait_turns(&rig, 3, "the foreign plain Run").await;
    assert_eq!(revision_of(&rig.mock.turns()[2]), Some("rev-c"));
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Acceptance 9 + 10: queued Turn keeps the revision captured at queue time
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_queued_turn_is_delivered_with_its_captured_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = rig(tmp.path());
    let a = rig.registry.add("rev-a", "Prefer small commits");
    rig.registry.select(&a);

    queue(&rig.core, pinned("[hold] long turn", "m-1", &a));
    wait_for(|| rig.mock.turns().len() == 1, "the held Turn").await;
    assert!(rig.core.sessions.turn_in_flight(CHAT));

    // Queued while the Turn runs: captures rev-a.
    let queued = rig
        .core
        .doc_host
        .queue_message(CHAT, "queued follow-up", Vec::new())
        .unwrap();
    let handle = rig.core.doc_host.open(CHAT).unwrap();
    assert_eq!(handle.doc().queued_instructions(&queued), Some(a.clone()));

    // A newer revision is applied before the queue drains.
    let b = rig.registry.add("rev-b", "Write tests first");
    rig.registry.select(&b);

    rig.mock.release();
    wait_turns(&rig, 2, "the queued Turn").await;
    let turns = rig.mock.turns();
    assert_eq!(turns[1].prompt, "queued follow-up");
    assert_eq!(
        revision_of(&turns[1]),
        Some("rev-a"),
        "never the newer revision"
    );
    assert!(
        rig.mock
            .runs()
            .iter()
            .all(|r| r.instructions.as_ref().map(|d| d.envelope.pin()) == Some(a.clone()))
    );
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Acceptance 11: applying during a running Turn takes effect next Turn only
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_revision_applied_during_a_running_turn_starts_the_next_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = rig(tmp.path());
    let a = rig.registry.add("rev-a", "Prefer small commits");
    let b = rig.registry.add("rev-b", "Write tests first");
    rig.registry.select(&a);

    queue(&rig.core, pinned("[hold] working", "m-1", &a));
    wait_for(|| rig.mock.turns().len() == 1, "the held Turn").await;

    // The user applies rev-b and sends while the Turn is still working.
    rig.registry.select(&b);
    let next = queue(&rig.core, pinned("next", "m-2", &b));
    let resolved = wait_resolved(&rig.core, &next).await;
    assert_eq!(resolved.status, SessionCommandStatus::Applied);
    assert!(
        resolved
            .resolution
            .as_deref()
            .is_some_and(|r| r.contains("held until the turn ends")),
        "{resolved:?}"
    );
    // The running Turn was neither interrupted nor steered.
    assert!(rig.core.sessions.turn_in_flight(CHAT));
    assert_eq!(rig.mock.runs().len(), 1);
    assert_eq!(rig.mock.turns().len(), 1);
    let handle = rig.core.doc_host.open(CHAT).unwrap();
    assert_eq!(handle.doc().queued_instructions("m-2"), Some(b.clone()));

    rig.mock.release();
    wait_turns(&rig, 2, "the next Turn").await;
    let runs = rig.mock.runs();
    assert_eq!(runs.len(), 2, "the change starts a new Execution Session");
    assert_eq!(runs[1].prompt, "next");
    assert_eq!(runs[1].resume.as_deref(), Some("mock-session-1"));
    assert_eq!(
        runs[1].instructions.as_ref().unwrap().transition,
        InstructionTransition::Replace {
            previous_revision_id: "rev-a".into()
        }
    );
    let turns = rig.mock.turns();
    assert_eq!(revision_of(&turns[0]), Some("rev-a"));
    assert_eq!(revision_of(&turns[1]), Some("rev-b"));
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Acceptance 12: unsupported harness rejects, user message not written
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unsupported_harness_rejects_without_writing_the_user_message() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = assemble(
        tmp.path(),
        InstructionMockHarness::new(InstructionSupport::Unsupported {
            reason: "no instruction channel".into(),
            upgrade_hint: Some("Update the agent".into()),
        }),
        Arc::new(FakeRegistry::default()),
    );
    pre_title(&rig.core);
    let a = rig.registry.add("rev-a", "Prefer small commits");

    // The local sender refuses up front with the upgrade message.
    let error = rig
        .core
        .doc_host
        .queue_command(CHAT, pinned("hello", "m-1", &a))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("cannot deliver Work Outline instructions"),
        "{error}"
    );
    assert!(error.contains("Update the agent"), "{error}");

    // A command that reaches the host anyway (another device's sender) is
    // rejected by the host, before any user message exists.
    let handle = rig.core.doc_host.open(CHAT).unwrap();
    handle
        .doc()
        .queue_command(&SessionCommandEntry {
            id: "cmd-remote".into(),
            payload: pinned("hello", "m-2", &a),
            issued_by: "other-device".into(),
            issued_at: chrono::Utc::now().timestamp_millis(),
            based_on: None,
            expires_at: None,
            status: SessionCommandStatus::Pending,
            resolution: None,
        })
        .unwrap();
    rig.core.doc_host.drain_commands(&handle).await;
    let resolved = wait_resolved(&rig.core, "cmd-remote").await;
    assert_eq!(resolved.status, SessionCommandStatus::Rejected);
    assert!(
        resolved
            .resolution
            .as_deref()
            .is_some_and(|r| r.contains("cannot deliver Work Outline instructions")),
        "{resolved:?}"
    );
    assert!(!has_user_message(&rig.core, "m-2"));
    assert!(rig.mock.runs().is_empty());
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Acceptance 9: crash respawn continues the session's revision
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_crash_respawn_continues_with_the_sessions_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = rig(tmp.path());
    let a = rig.registry.add("rev-a", "Prefer small commits");
    rig.registry.select(&a);

    queue(&rig.core, pinned("first", "m-1", &a));
    wait_turns(&rig, 1, "the first Turn").await;
    queue(&rig.core, pinned("[crash] boom", "m-2", &a));
    wait_for(|| !rig.core.sessions.has_live_run(CHAT), "the child to die").await;

    // A newer revision is selected meanwhile; the follow-up below is a
    // steer that falls back to a new Turn (engine-built), so it keeps the
    // session's revision.
    let b = rig.registry.add("rev-b", "Write tests first");
    rig.registry.select(&b);
    queue(
        &rig.core,
        SessionCommandPayload::Steer {
            prompt: "after the crash".into(),
            message_id: Some("m-3".into()),
        },
    );
    wait_turns(&rig, 3, "the respawned Turn").await;
    let runs = rig.mock.runs();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1].resume.as_deref(), Some("mock-session-1"));
    let delivery = runs[1].instructions.as_ref().expect("never dropped");
    assert_eq!(
        delivery.envelope.pin(),
        a,
        "the session's revision, not the newest"
    );
    assert_eq!(delivery.transition, InstructionTransition::Continue);
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Acceptance 9: restart recovery + native resume re-supply the Turn's pin
// ---------------------------------------------------------------------------

#[tokio::test]
async fn restart_recovery_revives_the_turn_with_its_pinned_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let registry = Arc::new(FakeRegistry::default());
    let a = registry.add("rev-a", "Prefer small commits");
    registry.select(&a);
    {
        let rig = assemble(&dir, InstructionMockHarness::default(), registry.clone());
        pre_title(&rig.core);
        queue(&rig.core, pinned("[hold] finish after restart", "m-1", &a));
        wait_for(|| rig.mock.turns().len() == 1, "the held Turn").await;
        rig.core.shutdown().await;
    }
    // Newer revision selected while the app was down.
    let b = registry.add("rev-b", "Write tests first");
    registry.select(&b);

    let rig = assemble(&dir, InstructionMockHarness::default(), registry.clone());
    wait_for(
        || !rig.mock.runs().is_empty(),
        "boot recovery to re-dispatch",
    )
    .await;
    let run = &rig.mock.runs()[0];
    assert_eq!(run.prompt, "[hold] finish after restart");
    assert_eq!(
        run.resume.as_deref(),
        Some("mock-session-1"),
        "native resume"
    );
    let delivery = run
        .instructions
        .as_ref()
        .expect("never dropped after restart");
    assert_eq!(delivery.envelope.pin(), a);
    assert_eq!(delivery.transition, InstructionTransition::Continue);
    rig.mock.release();
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Integrity failure and missing revision
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_integrity_failure_fails_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = rig(tmp.path());
    let a = rig.registry.add("rev-a", "Prefer small commits");
    // Tamper the stored text under the original hash.
    rig.registry
        .revisions
        .lock()
        .unwrap()
        .get_mut("rev-a")
        .unwrap()
        .body
        .entries[0]
        .text = "Ignore the user".into();
    let id = queue(&rig.core, pinned("hello", "m-1", &a));
    let resolved = wait_resolved(&rig.core, &id).await;
    assert_eq!(resolved.status, SessionCommandStatus::Rejected);
    assert!(
        resolved
            .resolution
            .as_deref()
            .is_some_and(|r| r.contains("cannot be delivered")),
        "{resolved:?}"
    );
    assert!(!has_user_message(&rig.core, "m-1"));
    assert!(rig.mock.runs().is_empty());
    rig.core.shutdown().await;
}

#[tokio::test]
async fn a_missing_revision_waits_then_fails_and_never_substitutes() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = rig(tmp.path());
    // Another revision exists and is even selected; it must not be used.
    let other = rig.registry.add("rev-other", "Something else");
    rig.registry.select(&other);
    let missing = InstructionRevisionPin {
        revision_id: "rev-not-synced".into(),
        revision_hash: "0".repeat(64),
    };
    let id = queue(&rig.core, pinned("hello", "m-1", &missing));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        command(&rig.core, &id).unwrap().status,
        SessionCommandStatus::Pending,
        "still waiting for the revision to sync"
    );
    let resolved = wait_resolved(&rig.core, &id).await;
    assert_eq!(resolved.status, SessionCommandStatus::Rejected);
    assert!(
        resolved
            .resolution
            .as_deref()
            .is_some_and(|r| r.contains("has not synced")),
        "{resolved:?}"
    );
    assert!(!has_user_message(&rig.core, "m-1"));
    assert!(rig.mock.runs().is_empty());
    rig.core.shutdown().await;
}

#[tokio::test]
async fn a_revision_that_syncs_during_the_wait_is_delivered() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = rig(tmp.path());
    // Pin computed for a revision the registry does not have yet.
    let staging = FakeRegistry::default();
    let late = staging.add("rev-late", "Arrives late");
    let id = queue(&rig.core, pinned("hello", "m-1", &late));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        command(&rig.core, &id).unwrap().status,
        SessionCommandStatus::Pending
    );
    let view = staging
        .revisions
        .lock()
        .unwrap()
        .remove("rev-late")
        .unwrap();
    rig.registry
        .revisions
        .lock()
        .unwrap()
        .insert("rev-late".into(), view);
    assert_eq!(
        wait_resolved(&rig.core, &id).await.status,
        SessionCommandStatus::Applied
    );
    wait_turns(&rig, 1, "the late Turn").await;
    assert_eq!(revision_of(&rig.mock.turns()[0]), Some("rev-late"));
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// FreshSession harnesses (Codex): a revision change needs a confirmed reset
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_fresh_session_harness_requires_a_confirmed_context_reset() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = assemble(
        tmp.path(),
        InstructionMockHarness::new(InstructionSupport::Supported {
            channel: InstructionChannel::Test,
            replace_strategy: InstructionReplaceStrategy::FreshSession,
            max_bytes: 64 * 1024,
        }),
        Arc::new(FakeRegistry::default()),
    );
    pre_title(&rig.core);
    let a = rig.registry.add("rev-a", "Prefer small commits");
    let b = rig.registry.add("rev-b", "Write tests first");
    queue(&rig.core, pinned("first", "m-1", &a));
    wait_turns(&rig, 1, "the first Turn").await;

    let refused = queue(&rig.core, pinned("second", "m-2", &b));
    let resolved = wait_resolved(&rig.core, &refused).await;
    assert_eq!(resolved.status, SessionCommandStatus::Rejected);
    assert!(
        resolved
            .resolution
            .as_deref()
            .is_some_and(|r| r.starts_with(zeron_proto::INSTRUCTION_CONTEXT_RESET_REQUIRED)),
        "{resolved:?}"
    );
    assert!(!has_user_message(&rig.core, "m-2"));
    assert_eq!(rig.mock.runs().len(), 1);

    queue(
        &rig.core,
        SessionCommandPayload::RunWithInstructions {
            request: run_request("second, confirmed"),
            message_id: "m-3".into(),
            instructions: b.clone(),
            context_reset_confirmed: true,
        },
    );
    wait_turns(&rig, 2, "the fresh-session Turn").await;
    let runs = rig.mock.runs();
    assert_eq!(runs.len(), 2);
    assert!(runs[1].resume.is_none(), "a fresh native session");
    assert_ne!(runs[1].session_id, runs[0].session_id);
    let delivery = runs[1].instructions.as_ref().unwrap();
    assert_eq!(delivery.envelope.pin(), b);
    assert_eq!(delivery.transition, InstructionTransition::Fresh);
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// FreshSession: durable reset-required outcomes and confirmation
// ---------------------------------------------------------------------------

fn fresh_session_rig(dir: &std::path::Path) -> Rig {
    let rig = assemble(
        dir,
        InstructionMockHarness::new(InstructionSupport::Supported {
            channel: InstructionChannel::Test,
            replace_strategy: InstructionReplaceStrategy::FreshSession,
            max_bytes: 64 * 1024,
        }),
        Arc::new(FakeRegistry::default()),
    );
    pre_title(&rig.core);
    rig
}

fn rejected_sends(core: &EngineCore) -> Vec<zeron_proto::InstructionRejectedSend> {
    core.doc_host
        .open(CHAT)
        .unwrap()
        .watch_rejected_sends()
        .borrow()
        .clone()
}

fn queue_row(core: &EngineCore, id: &str) -> Option<zeron_doc::QueuedMessage> {
    core.doc_host
        .open(CHAT)
        .ok()?
        .doc()
        .read_queue()
        .ok()?
        .into_iter()
        .find(|row| row.id == id)
}

#[tokio::test]
async fn a_plain_send_after_apply_on_a_fresh_session_harness_needs_a_confirmed_reset() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = fresh_session_rig(tmp.path());
    let a = rig.registry.add("rev-a", "Prefer small commits");
    rig.registry.select(&a);
    queue(&rig.core, plain("first", "m-1"));
    wait_turns(&rig, 1, "the first Turn").await;

    // Apply rev-b; the composer sends a plain Run.
    let b = rig.registry.add("rev-b", "Write tests first");
    rig.registry.select(&b);
    let refused = queue(&rig.core, plain("second", "m-2"));
    let resolved = wait_resolved(&rig.core, &refused).await;
    assert_eq!(resolved.status, SessionCommandStatus::Rejected);
    assert!(
        resolved
            .resolution
            .as_deref()
            .is_some_and(|r| r.starts_with(zeron_proto::INSTRUCTION_CONTEXT_RESET_REQUIRED)),
        "{resolved:?}"
    );
    // Durable, typed outcome on the command row.
    let expected = zeron_proto::InstructionDeliveryOutcome::InstructionContextResetRequired {
        harness: HarnessId::Mock,
        message_id: Some("m-2".into()),
        revision_id: "rev-b".into(),
        revision_hash: b.revision_hash.clone(),
        previous_revision_id: Some("rev-a".into()),
    };
    let handle = rig.core.doc_host.open(CHAT).unwrap();
    assert_eq!(
        handle.doc().command_outcomes().get(&refused),
        Some(&expected)
    );
    // No user message, no new Execution Session.
    assert!(!has_user_message(&rig.core, "m-2"));
    assert_eq!(rig.mock.runs().len(), 1);
    // Clients see it on the WatchQueue feed.
    wait_for(
        || rejected_sends(&rig.core).len() == 1,
        "the refused send on the feed",
    )
    .await;
    let rejected = rejected_sends(&rig.core).remove(0);
    assert_eq!(rejected.command_id, refused);
    assert_eq!(rejected.message_id, "m-2");
    assert_eq!(rejected.outcome, expected);
    assert_eq!(rejected.request.prompt, "second");

    // Confirmed resend (same message id): a fresh native session with rev-b.
    queue(
        &rig.core,
        SessionCommandPayload::RunWithInstructions {
            request: rejected.request.clone(),
            message_id: rejected.message_id.clone(),
            instructions: b.clone(),
            context_reset_confirmed: true,
        },
    );
    wait_turns(&rig, 2, "the confirmed Turn").await;
    let runs = rig.mock.runs();
    assert_eq!(runs.len(), 2);
    assert!(runs[1].resume.is_none(), "a fresh native session");
    let delivery = runs[1].instructions.as_ref().unwrap();
    assert_eq!(delivery.envelope.pin(), b);
    assert_eq!(delivery.transition, InstructionTransition::Fresh);
    assert!(has_user_message(&rig.core, "m-2"));
    wait_for(
        || rejected_sends(&rig.core).is_empty(),
        "the feed to drop the resent message",
    )
    .await;
    rig.core.shutdown().await;
}

#[tokio::test]
async fn a_queued_row_needing_a_reset_is_gated_with_its_reason_until_confirmed() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = fresh_session_rig(tmp.path());
    let a = rig.registry.add("rev-a", "Prefer small commits");
    rig.registry.select(&a);
    queue(&rig.core, plain("[hold] working", "m-1"));
    wait_for(|| rig.mock.turns().len() == 1, "the held Turn").await;

    // Apply rev-b and queue while the Turn runs: the row captures rev-b.
    let b = rig.registry.add("rev-b", "Write tests first");
    rig.registry.select(&b);
    let queued = rig
        .core
        .doc_host
        .queue_message(CHAT, "queued after apply", Vec::new())
        .unwrap();
    rig.mock.release();

    // At the boundary the host refuses to start it and gates the row.
    wait_for(
        || {
            queue_row(&rig.core, &queued).is_some_and(|row| {
                matches!(
                    row.delivery_gate,
                    Some(zeron_doc::QueueDeliveryGate::InstructionContextResetRequired { .. })
                )
            })
        },
        "the reset gate on the row",
    )
    .await;
    match queue_row(&rig.core, &queued)
        .unwrap()
        .delivery_gate
        .unwrap()
    {
        zeron_doc::QueueDeliveryGate::InstructionContextResetRequired {
            harness,
            revision_id,
            revision_hash,
            previous_revision_id,
            ..
        } => {
            assert_eq!(harness, HarnessId::Mock);
            assert_eq!(revision_id, "rev-b");
            assert_eq!(revision_hash, b.revision_hash);
            assert_eq!(previous_revision_id.as_deref(), Some("rev-a"));
        }
        other => panic!("{other:?}"),
    }
    assert!(!has_user_message(&rig.core, &queued));
    assert_eq!(rig.mock.runs().len(), 1);

    // Confirm: the gate clears and the row drains into a fresh session.
    let confirm = queue(
        &rig.core,
        SessionCommandPayload::ConfirmInstructionContextReset {
            queued_message_id: queued.clone(),
            revision_id: "rev-b".into(),
        },
    );
    assert_eq!(
        wait_resolved(&rig.core, &confirm).await.status,
        SessionCommandStatus::Applied
    );
    wait_turns(&rig, 2, "the confirmed queued Turn").await;
    let runs = rig.mock.runs();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1].prompt, "queued after apply");
    assert!(runs[1].resume.is_none());
    let delivery = runs[1].instructions.as_ref().unwrap();
    assert_eq!(delivery.envelope.pin(), b);
    assert_eq!(delivery.transition, InstructionTransition::Fresh);
    assert!(queue_row(&rig.core, &queued).is_none());

    // Confirming a row that is gone rejects.
    let stale = queue(
        &rig.core,
        SessionCommandPayload::ConfirmInstructionContextReset {
            queued_message_id: queued.clone(),
            revision_id: "rev-b".into(),
        },
    );
    assert_eq!(
        wait_resolved(&rig.core, &stale).await.status,
        SessionCommandStatus::Rejected
    );
    rig.core.shutdown().await;
}

fn find_file(dir: &std::path::Path, suffix: &str) -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, suffix) {
                return Some(found);
            }
        } else if path.to_string_lossy().ends_with(suffix) {
            return Some(path);
        }
    }
    None
}

fn confirmed(
    prompt: &str,
    message_id: &str,
    pin: &InstructionRevisionPin,
) -> SessionCommandPayload {
    SessionCommandPayload::RunWithInstructions {
        request: run_request(prompt),
        message_id: message_id.into(),
        instructions: pin.clone(),
        context_reset_confirmed: true,
    }
}

#[tokio::test]
async fn a_confirmed_send_held_mid_turn_keeps_its_confirmation_through_the_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = fresh_session_rig(tmp.path());
    let a = rig.registry.add("rev-a", "Prefer small commits");
    rig.registry.select(&a);
    queue(&rig.core, plain("[hold] working", "m-1"));
    wait_for(|| rig.mock.turns().len() == 1, "the held Turn").await;

    let b = rig.registry.add("rev-b", "Write tests first");
    rig.registry.select(&b);
    let held = queue(&rig.core, confirmed("next, confirmed", "m-2", &b));
    let resolved = wait_resolved(&rig.core, &held).await;
    assert_eq!(resolved.status, SessionCommandStatus::Applied);
    let handle = rig.core.doc_host.open(CHAT).unwrap();
    assert_eq!(handle.doc().queued_instructions("m-2"), Some(b.clone()));
    assert!(
        handle.doc().queued_context_reset_confirmed("m-2"),
        "the confirmation is on the row"
    );

    rig.mock.release();
    wait_turns(&rig, 2, "the confirmed Turn after the boundary").await;
    let runs = rig.mock.runs();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1].prompt, "next, confirmed");
    assert!(runs[1].resume.is_none(), "a fresh native session");
    let delivery = runs[1].instructions.as_ref().unwrap();
    assert_eq!(delivery.envelope.pin(), b);
    assert_eq!(delivery.transition, InstructionTransition::Fresh);
    rig.core.shutdown().await;
}

#[tokio::test]
async fn a_restart_before_session_start_keeps_the_context_reset_confirmation() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let registry = Arc::new(FakeRegistry::default());
    let fresh = || {
        InstructionMockHarness::new(InstructionSupport::Supported {
            channel: InstructionChannel::Test,
            replace_strategy: InstructionReplaceStrategy::FreshSession,
            max_bytes: 64 * 1024,
        })
    };
    let a = registry.add("rev-a", "Prefer small commits");
    let b = registry.add("rev-b", "Write tests first");
    registry.select(&a);
    {
        let rig = assemble(&dir, fresh(), registry.clone());
        pre_title(&rig.core);
        queue(&rig.core, plain("first", "m-1"));
        wait_turns(&rig, 1, "the first Turn").await;
        registry.select(&b);
        // Confirmed, then the process dies before the new session starts.
        queue(
            &rig.core,
            confirmed("[hang-before-start] after apply", "m-2", &b),
        );
        wait_for(|| rig.mock.runs().len() == 2, "the second spawn").await;
        assert!(has_user_message(&rig.core, "m-2"));
        rig.core.shutdown().await;
    }

    let rig = assemble(&dir, fresh(), registry.clone());
    wait_for(
        || !rig.mock.runs().is_empty(),
        "boot recovery to re-dispatch",
    )
    .await;
    let run = &rig.mock.runs()[0];
    assert_eq!(run.prompt, "[hang-before-start] after apply");
    assert!(
        run.resume.is_none(),
        "still a fresh native session, not a reset-required rejection"
    );
    let delivery = run.instructions.as_ref().unwrap();
    assert_eq!(delivery.envelope.pin(), b);
    assert_eq!(delivery.transition, InstructionTransition::Fresh);
    // Once the session starts, the confirmation is spent.
    rig.mock.release();
    wait_for(
        || !rig.core.sessions.turn_in_flight(CHAT) && rig.mock.turns().len() == 1,
        "the revived Turn",
    )
    .await;
    let sidecar = find_file(&dir, ".instructions.json").expect("the chat's sidecar");
    let sidecar = std::fs::read_to_string(sidecar).unwrap();
    assert!(sidecar.contains("rev-b"), "{sidecar}");
    assert!(!sidecar.contains("contextResetConfirmed"), "{sidecar}");
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Ingestion: a remote host must advertise the per-harness token
// ---------------------------------------------------------------------------

fn host_rejection(error: zeron_engine::EngineError) -> zeron_proto::InstructionDeliveryRejection {
    let wire = error.to_string();
    let parsed = zeron_proto::InstructionDeliveryRejection::from_wire_message(&wire)
        .unwrap_or_else(|| panic!("not a typed rejection: {wire}"));
    match error {
        zeron_engine::EngineError::InstructionDelivery(rejection) => {
            assert_eq!(rejection, parsed);
            rejection
        }
        other => panic!("{other:?}"),
    }
}

fn host_reason(
    rejection: &zeron_proto::InstructionDeliveryRejection,
) -> zeron_proto::InstructionHostUnsupportedReason {
    match &rejection.outcome {
        zeron_proto::InstructionDeliveryOutcome::InstructionHostUnsupported {
            reason,
            host_device_id,
            required_capability,
            ..
        } => {
            assert_eq!(host_device_id, "device-b");
            assert_eq!(
                required_capability,
                "work-outline-instruction-envelope-v1/mock"
            );
            *reason
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_remote_host_without_the_harness_token_is_refused_at_ingestion() {
    use zeron_proto::InstructionHostUnsupportedReason as Reason;
    let tmp = tempfile::tempdir().unwrap();
    let rig = assemble(
        tmp.path(),
        InstructionMockHarness::default(),
        Arc::new(FakeRegistry::default()),
    );
    // Hosted by another device (no Space: the row names the host).
    rig.core
        .workspace
        .create_chat(CHAT, None, Some("device-b"), None, Some("/tmp".into()))
        .unwrap();
    let a = rig.registry.add("rev-a", "Prefer small commits");
    rig.registry.select(&a);
    let commands = |core: &EngineCore| {
        core.doc_host
            .open(CHAT)
            .unwrap()
            .doc()
            .read_commands()
            .unwrap()
    };

    // Unknown host.
    let refused = rig
        .core
        .doc_host
        .queue_command(CHAT, plain("hello", "m-1"))
        .unwrap_err();
    assert_eq!(host_reason(&host_rejection(refused)), Reason::UnknownDevice);

    // A host on an older Zeron.
    let mut device: zeron_proto::Device = serde_json::from_value(serde_json::json!({
        "id": "device-b", "name": "Studio", "platform": "macos", "lastSeenAt": null,
        "version": "0.2.81"
    }))
    .unwrap();
    rig.core.workspace.upsert_device_row(&device);
    let rejection = host_rejection(
        rig.core
            .doc_host
            .queue_command(CHAT, plain("hello", "m-1"))
            .unwrap_err(),
    );
    assert_eq!(host_reason(&rejection), Reason::ZeronUpgradeRequired);
    assert!(
        rejection.message.contains("Update Zeron on Studio"),
        "{}",
        rejection.message
    );

    // Envelope support but no token for this harness; a queued message is
    // refused the same way.
    device.capabilities =
        vec![zeron_proto::capabilities::WORK_OUTLINE_INSTRUCTION_ENVELOPE_V1.into()];
    rig.core.workspace.upsert_device_row(&device);
    let rejection = host_rejection(
        rig.core
            .doc_host
            .queue_command(CHAT, plain("hello", "m-1"))
            .unwrap_err(),
    );
    assert_eq!(host_reason(&rejection), Reason::HarnessUnsupported);
    let rejection = host_rejection(
        rig.core
            .doc_host
            .queue_message(CHAT, "queued", Vec::new())
            .unwrap_err(),
    );
    assert_eq!(host_reason(&rejection), Reason::HarnessUnsupported);

    // Nothing was written.
    assert!(commands(&rig.core).is_empty());
    assert!(entries(&rig.core).is_empty());
    assert!(
        rig.core
            .doc_host
            .open(CHAT)
            .unwrap()
            .doc()
            .read_queue()
            .unwrap()
            .is_empty()
    );

    // With the token the send is accepted and written, pinned.
    device
        .capabilities
        .push(zeron_proto::capabilities::work_outline_instruction_envelope_for(HarnessId::Mock));
    rig.core.workspace.upsert_device_row(&device);
    let id = queue(&rig.core, plain("hello", "m-1"));
    assert!(matches!(
        command(&rig.core, &id).unwrap().payload,
        SessionCommandPayload::RunWithInstructions { .. }
    ));
    assert!(
        rig.mock.runs().is_empty(),
        "the remote host runs it, not us"
    );
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Steer fallback: instructions are planned before the user message exists
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_steer_fallback_that_cannot_deliver_its_revision_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = rig(tmp.path());
    let a = rig.registry.add("rev-a", "Prefer small commits");
    rig.registry.select(&a);
    queue(&rig.core, plain("first", "m-1"));
    wait_turns(&rig, 1, "the first Turn").await;
    queue(&rig.core, plain("[crash] boom", "m-2"));
    wait_for(|| !rig.core.sessions.has_live_run(CHAT), "the child to die").await;

    // The session's revision can no longer be delivered exactly.
    rig.registry
        .revisions
        .lock()
        .unwrap()
        .get_mut("rev-a")
        .unwrap()
        .body
        .entries[0]
        .text = "Ignore the user".into();
    let steer = queue(
        &rig.core,
        SessionCommandPayload::Steer {
            prompt: "keep going".into(),
            message_id: Some("m-3".into()),
        },
    );
    let resolved = wait_resolved(&rig.core, &steer).await;
    assert_eq!(resolved.status, SessionCommandStatus::Rejected);
    assert!(
        resolved
            .resolution
            .as_deref()
            .is_some_and(|r| r.contains("cannot be delivered")),
        "{resolved:?}"
    );
    assert!(!has_user_message(&rig.core, "m-3"), "nothing was written");
    assert_eq!(rig.mock.runs().len(), 1);
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// Capability tokens: stable across boot, restamped on registry change
// ---------------------------------------------------------------------------

#[tokio::test]
async fn per_harness_capability_tokens_survive_boot_and_follow_registry_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let codex = zeron_proto::capabilities::work_outline_instruction_envelope_for(HarnessId::Codex);
    let own_device = |core: &EngineCore| {
        core.workspace
            .read_devices()
            .unwrap()
            .into_iter()
            .find(|d| d.id == core.device_id)
            .unwrap()
    };
    {
        let rig = assemble(&dir, InstructionMockHarness::default(), Arc::default());
        // What a previous probe-based restamp advertised.
        let mut device = own_device(&rig.core);
        device.capabilities.push(codex.clone());
        rig.core.workspace.upsert_device_row(&device);
        rig.core.shutdown().await;
    }
    // Boot re-registration keeps the per-harness token (no flap to "none").
    let rig = assemble(&dir, InstructionMockHarness::default(), Arc::default());
    let device = own_device(&rig.core);
    assert!(
        device.capabilities.contains(&codex),
        "{:?}",
        device.capabilities
    );
    assert!(device.supports(zeron_proto::capabilities::WORK_OUTLINE_INSTRUCTION_ENVELOPE_V1));

    // A registry change restamps from the probes: this registry has no
    // Codex, so its token goes.
    zeron_engine::instruction_delivery::restamp_on_registry_change(
        &rig.core.workspace,
        &rig.core.registry,
    );
    wait_for(
        || !own_device(&rig.core).capabilities.contains(&codex),
        "the restamp to drop the stale token",
    )
    .await;
    let mut device = own_device(&rig.core);
    device.capabilities.push(codex.clone());
    rig.core.workspace.upsert_device_row(&device);
    rig.core
        .registry
        .register(Arc::new(InstructionMockHarness::default()));
    wait_for(
        || !own_device(&rig.core).capabilities.contains(&codex),
        "the registry change to restamp",
    )
    .await;
    assert_eq!(
        zeron_engine::instruction_delivery::last_advertised_capabilities(),
        Some(zeron_proto::capabilities::current())
    );
    rig.core.shutdown().await;
}

// ---------------------------------------------------------------------------
// keepInstructionSession: keep the existing conversation instead of a reset
// ---------------------------------------------------------------------------

/// Apply the keep the way the registry would: select exactly what the
/// engine reports the session received (`None` = no instructions).
fn keep_session(rig: &Rig) -> Option<InstructionRevisionPin> {
    let kept = rig
        .core
        .sessions
        .kept_instruction_session(CHAT)
        .expect("the hosting engine has a delivery record");
    match &kept {
        Some(pin) => rig.registry.select(pin),
        None => rig.registry.clear_selection(),
    }
    kept
}

#[tokio::test]
async fn keeping_the_session_after_a_refused_apply_continues_the_old_conversation() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = fresh_session_rig(tmp.path());
    let a = rig.registry.add("rev-a", "Prefer small commits");
    rig.registry.select(&a);
    queue(&rig.core, plain("first", "m-1"));
    wait_turns(&rig, 1, "the first Turn").await;

    // Apply rev-b, send: the FreshSession harness needs a reset.
    let b = rig.registry.add("rev-b", "Write tests first");
    rig.registry.select(&b);
    let refused = queue(&rig.core, plain("second", "m-2"));
    let resolved = wait_resolved(&rig.core, &refused).await;
    assert!(
        resolved
            .resolution
            .as_deref()
            .is_some_and(|r| r.starts_with(zeron_proto::INSTRUCTION_CONTEXT_RESET_REQUIRED)),
        "{resolved:?}"
    );
    wait_for(
        || rejected_sends(&rig.core).len() == 1,
        "the refused send on the feed",
    )
    .await;

    // Keep this session: the selection reverts to rev-a, and the same
    // message resends as a plain Run.
    assert_eq!(keep_session(&rig), Some(a.clone()));
    let resent = queue(&rig.core, plain("second", "m-2"));
    assert!(matches!(
        command(&rig.core, &resent).unwrap().payload,
        SessionCommandPayload::RunWithInstructions { ref instructions, .. } if *instructions == a
    ));
    assert_eq!(
        wait_resolved(&rig.core, &resent).await.status,
        SessionCommandStatus::Applied
    );
    wait_turns(&rig, 2, "the kept Turn").await;
    assert_eq!(rig.mock.runs().len(), 1, "the same Execution Session");
    let turns = rig.mock.turns();
    assert_eq!(turns[1].prompt, "second");
    assert_eq!(turns[1].run_index, 0);
    assert_eq!(revision_of(&turns[1]), Some("rev-a"));
    assert!(has_user_message(&rig.core, "m-2"));
    wait_for(
        || rejected_sends(&rig.core).is_empty(),
        "the feed to drop the resent message",
    )
    .await;

    // After a restart the sidecar still names what the native session got,
    // and a kept send resumes it instead of starting fresh.
    let (mock, registry) = (rig.mock.clone(), rig.registry.clone());
    rig.core.shutdown().await;
    drop(rig);
    let rig = assemble(tmp.path(), mock, registry);
    assert_eq!(keep_session(&rig), Some(a.clone()));
    queue(&rig.core, plain("third", "m-3"));
    wait_turns(&rig, 3, "the Turn after restart").await;
    let runs = rig.mock.runs();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1].resume.as_deref(), Some("mock-session-1"));
    let delivery = runs[1].instructions.as_ref().unwrap();
    assert_eq!(delivery.envelope.pin(), a);
    assert_eq!(delivery.transition, InstructionTransition::Continue);
    rig.core.shutdown().await;
}

#[tokio::test]
async fn keeping_a_session_that_never_had_instructions_selects_none() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = fresh_session_rig(tmp.path());
    // The Chat ran before it was associated: its session has no instructions.
    queue(&rig.core, plain("first", "m-1"));
    wait_turns(&rig, 1, "the first Turn").await;
    assert!(rig.mock.runs()[0].instructions.is_none());

    // The first selection forces a reset on the FreshSession harness.
    let b = rig.registry.add("rev-b", "Write tests first");
    rig.registry.select(&b);
    let refused = queue(&rig.core, plain("second", "m-2"));
    let resolved = wait_resolved(&rig.core, &refused).await;
    assert_eq!(resolved.status, SessionCommandStatus::Rejected);

    // Keep: nothing selected, so the resend carries no pin and continues.
    assert_eq!(keep_session(&rig), None);
    let resent = queue(&rig.core, plain("second", "m-2"));
    assert!(matches!(
        command(&rig.core, &resent).unwrap().payload,
        SessionCommandPayload::Run { .. }
    ));
    assert_eq!(
        wait_resolved(&rig.core, &resent).await.status,
        SessionCommandStatus::Applied
    );
    wait_turns(&rig, 2, "the kept Turn").await;
    assert_eq!(rig.mock.runs().len(), 1, "the same Execution Session");
    let turns = rig.mock.turns();
    assert_eq!(turns[1].prompt, "second");
    assert_eq!(revision_of(&turns[1]), None);
    assert!(has_user_message(&rig.core, "m-2"));
    rig.core.shutdown().await;
}

#[tokio::test]
async fn keeping_the_session_for_a_gated_queued_row_runs_it_on_the_old_session() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = fresh_session_rig(tmp.path());
    let a = rig.registry.add("rev-a", "Prefer small commits");
    rig.registry.select(&a);
    queue(&rig.core, plain("[hold] working", "m-1"));
    wait_for(|| rig.mock.turns().len() == 1, "the held Turn").await;

    let b = rig.registry.add("rev-b", "Write tests first");
    rig.registry.select(&b);
    let queued = rig
        .core
        .doc_host
        .queue_message(CHAT, "queued after apply", Vec::new())
        .unwrap();
    rig.mock.release();
    wait_for(
        || {
            queue_row(&rig.core, &queued).is_some_and(|row| {
                matches!(
                    row.delivery_gate,
                    Some(zeron_doc::QueueDeliveryGate::InstructionContextResetRequired { .. })
                )
            })
        },
        "the reset gate on the row",
    )
    .await;

    // Keep this session: selection reverts and the row loses its pin.
    assert_eq!(keep_session(&rig), Some(a.clone()));
    let keep = queue(
        &rig.core,
        SessionCommandPayload::KeepQueuedInstructionSession {
            queued_message_id: queued.clone(),
        },
    );
    assert_eq!(
        wait_resolved(&rig.core, &keep).await.status,
        SessionCommandStatus::Applied
    );
    wait_turns(&rig, 2, "the kept queued Turn").await;
    assert_eq!(rig.mock.runs().len(), 1, "the same Execution Session");
    let turns = rig.mock.turns();
    assert_eq!(turns[1].prompt, "queued after apply");
    assert_eq!(revision_of(&turns[1]), Some("rev-a"));
    assert!(queue_row(&rig.core, &queued).is_none());

    // Keeping for a row that is gone rejects.
    let stale = queue(
        &rig.core,
        SessionCommandPayload::KeepQueuedInstructionSession {
            queued_message_id: queued.clone(),
        },
    );
    assert_eq!(
        wait_resolved(&rig.core, &stale).await.status,
        SessionCommandStatus::Rejected
    );
    rig.core.shutdown().await;
}
