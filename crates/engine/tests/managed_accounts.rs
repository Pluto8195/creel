//! Per-project managed accounts: homes, project defaults, chat pins, config
//! sharing, sign-in with a fake CLI, and the end-to-end dispatch seam. Every
//! path lives under a tempdir (never the real `~/.claude` / `~/.codex`), and
//! no real login or API call is made.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use zeron_doc::{MessageRole, MessageStatus, SessionCommandPayload};
use zeron_engine::{EngineCore, HarnessRegistry, ManagedAccounts, ManagedAccountsConfig};
use zeron_harness::{Harness, HarnessError, RunControls};
use zeron_proto::{
    AgentEvent, AgentLoginStatus, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest,
    SandboxLevel, SteeringMode,
};

fn accounts(root: &Path) -> (ManagedAccounts, PathBuf, PathBuf) {
    let claude = root.join("live/.claude");
    let codex = root.join("live/.codex");
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::create_dir_all(&codex).unwrap();
    let config = ManagedAccountsConfig::new(&root.join("data"), claude.clone(), codex.clone());
    (ManagedAccounts::new(config), claude, codex)
}

#[test]
fn homes_live_under_the_data_dir_and_are_private() {
    let tmp = tempfile::tempdir().unwrap();
    let (accounts, claude, _) = accounts(tmp.path());
    let a = accounts.create(HarnessId::ClaudeCode, "Work").unwrap();
    let b = accounts.create(HarnessId::ClaudeCode, "Personal").unwrap();
    assert_ne!(a.id, b.id);
    for acct in [&a, &b] {
        let home = accounts.home_of(HarnessId::ClaudeCode, &acct.id);
        assert!(home.starts_with(tmp.path().join("data/managed-accounts")));
        assert!(!home.starts_with(&claude));
        assert!(home.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = home.metadata().unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
    }
    assert!(accounts.create(HarnessId::Cursor, "x").is_err());
    assert!(accounts.create(HarnessId::ClaudeCode, "  ").is_err());
    // The live dir was never written to.
    assert_eq!(std::fs::read_dir(&claude).unwrap().count(), 0);
}

#[test]
fn new_chats_take_the_project_default_and_stay_pinned() {
    let tmp = tempfile::tempdir().unwrap();
    let (accounts, _, _) = accounts(tmp.path());
    let work = accounts.create(HarnessId::ClaudeCode, "Work").unwrap();
    let side = accounts.create(HarnessId::ClaudeCode, "Side").unwrap();
    accounts
        .set_project_account("space-a", HarnessId::ClaudeCode, Some(&work.id))
        .unwrap();

    let first = accounts
        .resolve_for_chat("chat-1", Some("space-a"), HarnessId::ClaudeCode, false)
        .unwrap()
        .expect("uses the project default");
    assert_eq!(first.id, work.id);

    // The project's account changes: the chat keeps its pin, a new chat follows.
    accounts
        .set_project_account("space-a", HarnessId::ClaudeCode, Some(&side.id))
        .unwrap();
    let again = accounts
        .resolve_for_chat("chat-1", Some("space-a"), HarnessId::ClaudeCode, true)
        .unwrap()
        .unwrap();
    assert_eq!(again.id, work.id);
    assert_eq!(again.home, first.home);
    let fresh = accounts
        .resolve_for_chat("chat-2", Some("space-a"), HarnessId::ClaudeCode, false)
        .unwrap()
        .unwrap();
    assert_eq!(fresh.id, side.id);

    // Clearing the project goes back to Default for new chats only.
    accounts
        .set_project_account("space-a", HarnessId::ClaudeCode, None)
        .unwrap();
    assert!(
        accounts
            .resolve_for_chat("chat-3", Some("space-a"), HarnessId::ClaudeCode, false)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        accounts
            .resolve_for_chat("chat-2", Some("space-a"), HarnessId::ClaudeCode, true)
            .unwrap()
            .unwrap()
            .id,
        side.id
    );
}

#[test]
fn chats_that_already_ran_stay_on_default_and_other_projects_are_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let (accounts, _, _) = accounts(tmp.path());
    let work = accounts.create(HarnessId::ClaudeCode, "Work").unwrap();
    accounts
        .set_project_account("space-a", HarnessId::ClaudeCode, Some(&work.id))
        .unwrap();
    // Existing chat with a harness session: its history is in the live home.
    assert!(
        accounts
            .resolve_for_chat("old", Some("space-a"), HarnessId::ClaudeCode, true)
            .unwrap()
            .is_none()
    );
    // And it stays Default even though the project now has an account.
    assert!(
        accounts
            .resolve_for_chat("old", Some("space-a"), HarnessId::ClaudeCode, false)
            .unwrap()
            .is_none()
    );
    // A project with no account, a projectless chat, and a harness without
    // managed accounts all resolve to Default.
    for (chat, space, harness) in [
        ("c1", Some("space-b"), HarnessId::ClaudeCode),
        ("c2", None, HarnessId::ClaudeCode),
        ("c3", Some("space-a"), HarnessId::Opencode),
        ("c4", Some("space-a"), HarnessId::Codex),
    ] {
        assert!(
            accounts
                .resolve_for_chat(chat, space, harness, false)
                .unwrap()
                .is_none(),
            "{chat}"
        );
    }
}

#[test]
fn pins_and_defaults_survive_a_restart_and_removal_is_guarded() {
    let tmp = tempfile::tempdir().unwrap();
    let (accounts, claude, codex) = accounts(tmp.path());
    let work = accounts.create(HarnessId::ClaudeCode, "Work").unwrap();
    accounts
        .set_project_account("space-a", HarnessId::ClaudeCode, Some(&work.id))
        .unwrap();
    accounts
        .resolve_for_chat("chat-1", Some("space-a"), HarnessId::ClaudeCode, false)
        .unwrap();

    let config = ManagedAccountsConfig::new(&tmp.path().join("data"), claude, codex);
    let reopened = ManagedAccounts::new(config);
    let snapshot = reopened.snapshot();
    assert_eq!(snapshot.accounts.len(), 1);
    assert_eq!(snapshot.project_defaults.len(), 1);
    assert_eq!(snapshot.chat_pins.len(), 1);
    assert_eq!(
        reopened.pinned_home("chat-1", HarnessId::ClaudeCode),
        Some(reopened.home_of(HarnessId::ClaudeCode, &work.id))
    );
    // A pinned account can't be removed out from under its chats.
    assert!(reopened.remove(&work.id).is_err());
}

#[test]
fn user_config_is_shared_without_credentials() {
    let tmp = tempfile::tempdir().unwrap();
    let (accounts, claude, codex) = accounts(tmp.path());
    std::fs::create_dir_all(claude.join("skills/mine")).unwrap();
    std::fs::write(claude.join("CLAUDE.md"), "be nice").unwrap();
    std::fs::write(claude.join(".credentials.json"), "{\"secret\":1}").unwrap();
    std::fs::create_dir_all(claude.join("projects/x")).unwrap();
    std::fs::write(
        claude.join("settings.json"),
        r#"{"model":"opus","apiKeyHelper":"/bin/key","env":{"FOO":"1","MY_API_KEY":"s","GH_TOKEN":"t"},"hooks":{"a":1}}"#,
    )
    .unwrap();
    std::fs::write(
        codex.join("config.toml"),
        "model = \"gpt\"\ncli_auth_credentials_store = \"keyring\"\n[tui]\nx = 1\n",
    )
    .unwrap();

    let cc = accounts.create(HarnessId::ClaudeCode, "Work").unwrap();
    let home = accounts.home_of(HarnessId::ClaudeCode, &cc.id);
    assert!(home.join("skills").is_dir());
    assert!(home.join("skills").symlink_metadata().unwrap().is_symlink());
    assert_eq!(
        std::fs::read_to_string(home.join("CLAUDE.md")).unwrap(),
        "be nice"
    );
    assert!(!home.join(".credentials.json").exists());
    assert!(!home.join("projects").exists());
    let settings: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join("settings.json")).unwrap()).unwrap();
    assert_eq!(settings["model"], "opus");
    assert_eq!(settings["hooks"]["a"], 1);
    assert!(settings.get("apiKeyHelper").is_none());
    assert_eq!(settings["env"]["FOO"], "1");
    assert!(settings["env"].get("MY_API_KEY").is_none());
    assert!(settings["env"].get("GH_TOKEN").is_none());

    let cx = accounts.create(HarnessId::Codex, "Work").unwrap();
    let cfg = std::fs::read_to_string(
        accounts
            .home_of(HarnessId::Codex, &cx.id)
            .join("config.toml"),
    )
    .unwrap();
    assert!(cfg.contains("cli_auth_credentials_store = \"file\""));
    assert!(!cfg.contains("keyring"));
    assert!(cfg.contains("model = \"gpt\"") && cfg.contains("[tui]"));

    // Default's settings change → the managed copy follows at the next prepare.
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(claude.join("settings.json"), r#"{"model":"sonnet"}"#).unwrap();
    accounts
        .set_project_account("space-a", HarnessId::ClaudeCode, Some(&cc.id))
        .unwrap();
    accounts
        .resolve_for_chat("c", Some("space-a"), HarnessId::ClaudeCode, false)
        .unwrap();
    let settings: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join("settings.json")).unwrap()).unwrap();
    assert_eq!(settings["model"], "sonnet");
}

#[cfg(unix)]
#[tokio::test]
async fn sign_in_runs_the_cli_against_the_managed_home_only() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let claude_live = tmp.path().join("live/.claude");
    let codex_live = tmp.path().join("live/.codex");
    std::fs::create_dir_all(&claude_live).unwrap();
    std::fs::create_dir_all(&codex_live).unwrap();
    let record = tmp.path().join("record");
    let fake = tmp.path().join("fake-claude");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\n\
             printf '%s|%s\\n' \"$*\" \"${{CLAUDE_CONFIG_DIR-<unset>}}\" >> {rec}\n\
             echo 'Open https://claude.example/authorize?x=1 to sign in'\n\
             printf '{{\"oauthAccount\":{{\"emailAddress\":\"me@work.test\"}}}}' > \"$CLAUDE_CONFIG_DIR/.claude.json\"\n\
             exit 0\n",
            rec = record.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut config =
        ManagedAccountsConfig::new(&tmp.path().join("data"), claude_live.clone(), codex_live);
    config
        .login_executables
        .insert("claude-code".into(), fake.clone());
    let accounts = ManagedAccounts::new(config);
    let acct = accounts.create(HarnessId::ClaudeCode, "Work").unwrap();
    assert!(!acct.signed_in);

    let (login_id, url) = accounts.start_login(&acct.id).await.unwrap();
    assert_eq!(url, "https://claude.example/authorize?x=1");
    let mut status = AgentLoginStatus::Pending;
    for _ in 0..100 {
        status = accounts.poll_login(&login_id).status;
        if status != AgentLoginStatus::Pending {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(status, AgentLoginStatus::Done);

    let home = accounts.home_of(HarnessId::ClaudeCode, &acct.id);
    let recorded = std::fs::read_to_string(&record).unwrap();
    assert_eq!(recorded.trim(), format!("auth login|{}", home.display()));
    let snapshot = accounts.snapshot();
    assert!(snapshot.accounts[0].signed_in);
    assert_eq!(snapshot.accounts[0].email.as_deref(), Some("me@work.test"));
    // The live dir was never written.
    assert_eq!(std::fs::read_dir(&claude_live).unwrap().count(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_sign_in_reports_an_error() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let fake = tmp.path().join("fake-codex");
    std::fs::write(&fake, "#!/bin/sh\nexit 3\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut config = ManagedAccountsConfig::new(
        &tmp.path().join("data"),
        tmp.path().join("c"),
        tmp.path().join("x"),
    );
    config.login_executables.insert("codex".into(), fake);
    let accounts = ManagedAccounts::new(config);
    let acct = accounts.create(HarnessId::Codex, "Work").unwrap();
    let (login_id, _) = accounts.start_login(&acct.id).await.unwrap();
    let mut poll = accounts.poll_login(&login_id);
    for _ in 0..100 {
        if poll.status != AgentLoginStatus::Pending {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        poll = accounts.poll_login(&login_id);
    }
    assert_eq!(poll.status, AgentLoginStatus::Error);
}

// ── end to end: dispatch resolves the pinned account ─────────────────────────

type HomeLog = Arc<Mutex<Vec<(String, Option<PathBuf>, Option<String>)>>>;

/// A ClaudeCode-id harness that records (chat cwd, account home, resume) per
/// run. Its scoped copies share the log; `gate` holds every run open until all
/// expected runs have started, proving two accounts run concurrently.
struct HomeRecorder {
    home: Option<PathBuf>,
    log: HomeLog,
    started: Arc<tokio::sync::Barrier>,
}

#[async_trait]
impl Harness for HomeRecorder {
    fn id(&self) -> HarnessId {
        HarnessId::ClaudeCode
    }
    fn display_name(&self) -> &str {
        "Recorder"
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
    fn scoped_to_account_home(&self, home: &Path) -> Option<Arc<dyn Harness>> {
        Some(Arc::new(HomeRecorder {
            home: Some(home.to_path_buf()),
            log: self.log.clone(),
            started: self.started.clone(),
        }))
    }
    async fn run(
        &self,
        request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        self.log.lock().unwrap().push((
            request.prompt.clone(),
            self.home.clone(),
            request.resume.clone(),
        ));
        let barrier = self.started.clone();
        let wait_for_peers = request.prompt.starts_with("pair");
        let events: Vec<Result<AgentEvent, HarnessError>> = vec![
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::ClaudeCode,
                model: "m".into(),
                tools: vec![],
                cwd: request.cwd,
                session_id: format!("hs-{}", request.prompt),
                assistant_message_id: format!("a-{}", request.prompt),
            }),
            Ok(AgentEvent::TextDelta { text: "ok".into() }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some(format!("hs-{}", request.prompt)),
            }),
        ];
        Ok(futures::stream::once(async move {
            if wait_for_peers {
                // Both accounts' runs must be live at once to pass this.
                tokio::time::timeout(Duration::from_secs(5), barrier.wait())
                    .await
                    .expect("the two runs never overlapped");
            }
            futures::stream::iter(events)
        })
        .flatten()
        .boxed())
    }
}

fn run_request(prompt: &str) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        harness: None,
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: "/tmp".into(),
        sandbox: SandboxLevel::DangerFullAccess,
        auto_approve: true,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
        instructions: None,
    }
}

fn queue(core: &EngineCore, chat: &str, prompt: &str, message_id: &str) {
    core.doc_host
        .queue_command(
            chat,
            SessionCommandPayload::Run {
                request: run_request(prompt),
                message_id: message_id.into(),
            },
        )
        .expect("queue run");
}

fn complete_count(core: &EngineCore, chat: &str) -> usize {
    core.doc_host
        .open(chat)
        .ok()
        .and_then(|h| h.doc().read_entries().ok())
        .unwrap_or_default()
        .iter()
        .filter(|e| e.role == MessageRole::Assistant && e.status == Some(MessageStatus::Complete))
        .count()
}

async fn wait_until(mut done: impl FnMut() -> bool, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !done() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

#[tokio::test]
async fn dispatch_runs_each_chat_on_its_projects_account() {
    let tmp = tempfile::tempdir().unwrap();
    let log: HomeLog = Arc::new(Mutex::new(Vec::new()));
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(HomeRecorder {
        home: None,
        log: log.clone(),
        started: Arc::new(tokio::sync::Barrier::new(2)),
    }));
    let core = EngineCore::assemble(
        &tmp.path().join("data"),
        Arc::new(registry),
        HarnessId::ClaudeCode,
        None,
    )
    .unwrap();

    let work = core
        .managed_accounts
        .create(HarnessId::ClaudeCode, "Work")
        .unwrap();
    let side = core
        .managed_accounts
        .create(HarnessId::ClaudeCode, "Side")
        .unwrap();
    for (space, acct) in [
        ("sp-work", Some(&work)),
        ("sp-side", Some(&side)),
        ("sp-none", None),
    ] {
        core.workspace
            .create_space(
                space,
                &core.device_id,
                &format!("/tmp/{space}"),
                None,
                false,
            )
            .unwrap();
        if let Some(acct) = acct {
            core.managed_accounts
                .set_project_account(space, HarnessId::ClaudeCode, Some(&acct.id))
                .unwrap();
        }
    }
    for (chat, space) in [
        ("chat-work", "sp-work"),
        ("chat-side", "sp-side"),
        ("chat-none", "sp-none"),
    ] {
        core.workspace
            .create_chat(chat, Some(space), None, None, None)
            .unwrap();
        core.workspace.rename_chat(chat, "Titled").unwrap();
    }

    // Two projects on different accounts run at the same time (the harness
    // blocks each until the other has started).
    queue(&core, "chat-work", "pair-work", "m1");
    queue(&core, "chat-side", "pair-side", "m2");
    wait_until(
        || complete_count(&core, "chat-work") == 1 && complete_count(&core, "chat-side") == 1,
        "both concurrent runs",
    )
    .await;

    // Default project: no home at all.
    queue(&core, "chat-none", "solo-none", "m3");
    wait_until(|| complete_count(&core, "chat-none") == 1, "default run").await;

    // The project switches to the other account; the existing chat's resume
    // keeps its pinned home.
    core.managed_accounts
        .set_project_account("sp-work", HarnessId::ClaudeCode, Some(&side.id))
        .unwrap();
    queue(&core, "chat-work", "solo-again", "m4");
    wait_until(|| complete_count(&core, "chat-work") == 2, "resumed run").await;

    let log = log.lock().unwrap().clone();
    let by_prompt = |p: &str| {
        log.iter()
            .find(|(prompt, _, _)| prompt == p)
            .unwrap()
            .clone()
    };
    let home_of = |id: &str| core.managed_accounts.home_of(HarnessId::ClaudeCode, id);
    assert_eq!(by_prompt("pair-work").1, Some(home_of(&work.id)));
    assert_eq!(by_prompt("pair-side").1, Some(home_of(&side.id)));
    assert_eq!(by_prompt("solo-none").1, None);
    let again = by_prompt("solo-again");
    assert_eq!(
        again.1,
        Some(home_of(&work.id)),
        "resume keeps the pinned home"
    );
    assert_eq!(
        again.2.as_deref(),
        Some("hs-pair-work"),
        "and resumes its session"
    );
    core.shutdown().await;
}
