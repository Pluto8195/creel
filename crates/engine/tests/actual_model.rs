//! The model the harness reports it ran (Claude Code `system`/`init`) lands on
//! the chat row — also for a chat with NO configured model, where the CLI
//! default (not Mast's picker default) decided — and is backfilled from the
//! run journal for chats that ran before the field existed.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use zeron_doc::SessionCommandPayload;
use zeron_engine::{EngineCore, HarnessRegistry, RunJournal};
use zeron_harness::{Harness, HarnessError, RunControls};
use zeron_proto::{
    AgentEvent, ChatModelDisplay, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest,
    SandboxLevel, SteeringMode,
};

const CHAT: &str = "chat-actual-model";

/// Reports `claude-fable-5` in its init event whatever the request asked for.
struct FableInitHarness;

#[async_trait]
impl Harness for FableInitHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Fable init"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let events: Vec<Result<AgentEvent, HarnessError>> = vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "claude-fable-5".into(),
                tools: vec![],
                cwd: request.cwd.clone(),
                session_id: "hs-fable".into(),
                assistant_message_id: "a-1".into(),
            }),
            Ok(AgentEvent::TextDelta { text: "ok".into() }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("hs-fable".into()),
            }),
        ];
        Ok(futures::stream::iter(events).boxed())
    }
}

fn assemble(dir: &std::path::Path) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(FableInitHarness));
    EngineCore::assemble(dir, Arc::new(registry), HarnessId::Mock, None)
        .expect("engine core assembles")
}

fn make_chat(core: &EngineCore) {
    core.workspace
        .create_space("space-am", &core.device_id, "/tmp", None, false)
        .expect("create space row");
    core.workspace
        .create_chat(CHAT, Some("space-am"), None, None, None)
        .expect("create chat row");
    // Titled up front so the auto-titler stays out of the way.
    core.workspace
        .rename_chat(CHAT, "Pre-titled")
        .expect("rename chat");
}

async fn wait_for<F: FnMut() -> bool>(mut predicate: F, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

fn actual_model(core: &EngineCore) -> Option<String> {
    core.workspace.chat(CHAT).ok()??.actual_model
}

#[tokio::test]
async fn a_chat_with_no_configured_model_records_and_displays_what_ran() {
    let tmp = tempfile::tempdir().unwrap();
    let core = assemble(&tmp.path().join("data"));
    make_chat(&core);
    let chat = core.workspace.chat(CHAT).unwrap().unwrap();
    assert!(chat.config.is_none() && chat.actual_model.is_none());
    assert_eq!(chat.model_display(), ChatModelDisplay::Default);

    core.doc_host
        .queue_command(
            CHAT,
            SessionCommandPayload::Run {
                request: RunRequest {
                    prompt: "hi".into(),
                    harness: None,
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
                },
                message_id: "msg-1".into(),
            },
        )
        .expect("queue run");
    wait_for(|| actual_model(&core).is_some(), "init model to be recorded").await;

    let chat = core.workspace.chat(CHAT).unwrap().unwrap();
    assert_eq!(chat.actual_model.as_deref(), Some("claude-fable-5"));
    assert_eq!(
        chat.model_display(),
        ChatModelDisplay::Actual {
            actual: "claude-fable-5",
            configured: None
        }
    );
    core.shutdown().await;
}

#[tokio::test]
async fn journaled_init_events_backfill_chats_that_predate_the_field() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    {
        let core = assemble(&dir);
        make_chat(&core);
        core.shutdown().await;
    }
    // A run from before `actual_model` existed left only its journal behind.
    let journal = RunJournal::open(dir.join("orgs/dev-org/dev-user/journals")).unwrap();
    journal
        .append(
            CHAT,
            &AgentEvent::SessionStarted {
                harness: HarnessId::ClaudeCode,
                model: "claude-fable-5".into(),
                tools: vec![],
                cwd: "/tmp".into(),
                session_id: "hs-old".into(),
                assistant_message_id: "a-old".into(),
            },
        )
        .unwrap();
    drop(journal);

    let core = assemble(&dir);
    // The boot pass runs in the background; the method is idempotent.
    wait_for(
        || {
            core.sessions.backfill_actual_models();
            actual_model(&core).is_some()
        },
        "journal backfill",
    )
    .await;
    assert_eq!(actual_model(&core).as_deref(), Some("claude-fable-5"));
    assert_eq!(core.sessions.backfill_actual_models(), 0, "already filled");
    core.shutdown().await;
}

#[tokio::test]
async fn echoed_request_models_are_not_recorded_as_actual() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    {
        let core = assemble(&dir);
        make_chat(&core);
        core.shutdown().await;
    }
    // Codex/ACP echo the request's model in `SessionStarted`; that proves nothing.
    let journal = RunJournal::open(dir.join("orgs/dev-org/dev-user/journals")).unwrap();
    journal
        .append(
            CHAT,
            &AgentEvent::SessionStarted {
                harness: HarnessId::Codex,
                model: "gpt-6-codex".into(),
                tools: vec![],
                cwd: "/tmp".into(),
                session_id: "hs-codex".into(),
                assistant_message_id: "a-codex".into(),
            },
        )
        .unwrap();
    drop(journal);
    let core = assemble(&dir);
    assert_eq!(core.sessions.backfill_actual_models(), 0);
    assert_eq!(actual_model(&core), None);
    core.shutdown().await;
}

#[tokio::test]
async fn backfill_marks_checked_journals_and_rechecks_only_after_growth() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    {
        let core = assemble(&dir);
        make_chat(&core);
        core.shutdown().await;
    }
    let journals = dir.join("orgs/dev-org/dev-user/journals");
    let started = |harness, model: &str| AgentEvent::SessionStarted {
        harness,
        model: model.into(),
        tools: vec![],
        cwd: "/tmp".into(),
        session_id: "hs".into(),
        assistant_message_id: "a".into(),
    };
    let journal = RunJournal::open(&journals).unwrap();
    journal.append(CHAT, &started(HarnessId::Codex, "echo")).unwrap();
    let core = assemble(&dir);
    // Let the boot pass settle, then run ours: no model, so the journal is marked.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(core.sessions.backfill_actual_models(), 0);
    let len = journal.byte_len(CHAT);
    assert_eq!(journal.model_checked_len(CHAT), Some(len));
    let before = std::fs::read(journals.join(format!("{CHAT}.jsonl"))).unwrap();

    // Same length: skipped even though a model has appeared (planted via a
    // marker matching the grown length), proving the journal is not reread.
    journal.append(CHAT, &started(HarnessId::ClaudeCode, "claude-fable-5")).unwrap();
    journal.mark_model_checked(CHAT, journal.byte_len(CHAT));
    assert_eq!(core.sessions.backfill_actual_models(), 0, "marked chat skipped");
    assert_eq!(actual_model(&core), None);

    // Growth past the marker triggers a recheck, which finds the model.
    journal.append(CHAT, &started(HarnessId::Codex, "echo")).unwrap();
    assert_eq!(core.sessions.backfill_actual_models(), 1);
    assert_eq!(actual_model(&core).as_deref(), Some("claude-fable-5"));
    // Found: no longer rechecked, and the pass only ever read the journal.
    assert_eq!(core.sessions.backfill_actual_models(), 0);
    let after = std::fs::read(journals.join(format!("{CHAT}.jsonl"))).unwrap();
    assert!(after.starts_with(&before));
    core.shutdown().await;
}
