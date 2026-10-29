//! Usage windows per managed account, and the Start Chat account pin. The
//! usage endpoints are a local fake; homes and the Keychain are temp/fake, so
//! nothing real is read or written and no network call leaves the machine.

use std::sync::{Arc, Mutex};

use mast_engine::{
    AgentAccounts, AgentAccountsConfig, HomeCredentialStore, ManagedAccounts,
    ManagedAccountsConfig, claude_keychain_service,
};
use mast_proto::HarnessId;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Default)]
struct FakeKeychain {
    items: Mutex<Vec<(String, String)>>,
}

#[derive(Debug)]
struct Shared(Arc<FakeKeychain>);

impl HomeCredentialStore for Shared {
    fn available(&self) -> bool {
        true
    }
    fn put(&self, service: &str, secret: &str) -> Result<(), mast_engine::EngineError> {
        self.0
            .items
            .lock()
            .unwrap()
            .push((service.to_string(), secret.to_string()));
        Ok(())
    }
    fn delete(&self, _service: &str) {}
    fn get(&self, service: &str) -> Option<String> {
        self.0
            .items
            .lock()
            .unwrap()
            .iter()
            .find(|(s, _)| s == service)
            .map(|(_, v)| v.clone())
    }
}

/// A one-route HTTP fake: records each request's `authorization` header and
/// answers `status` with `body`.
struct FakeUsage {
    url: String,
    auth: Arc<Mutex<Vec<String>>>,
}

async fn fake_usage(status: u16, body: &'static str) -> FakeUsage {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/usage", listener.local_addr().unwrap());
    let auth = Arc::new(Mutex::new(Vec::new()));
    let seen = auth.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0u8; 8192];
            let n = socket.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            for line in request.lines() {
                if line.to_ascii_lowercase().starts_with("authorization:") {
                    seen.lock()
                        .unwrap()
                        .push(line.split_once(':').unwrap().1.trim().to_string());
                }
            }
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(reply.as_bytes()).await;
        }
    });
    FakeUsage { url, auth }
}

struct Rig {
    _tmp: tempfile::TempDir,
    agent: AgentAccounts,
    managed: ManagedAccounts,
    keychain: Arc<FakeKeychain>,
}

fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let claude = tmp.path().join("live/.claude");
    let codex = tmp.path().join("live/.codex");
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::create_dir_all(&codex).unwrap();
    let agent = AgentAccounts::new(AgentAccountsConfig {
        data_dir: data.clone(),
        claude_config_dir: claude.clone(),
        claude_config_file: claude.join(".claude.json"),
        codex_home: codex.clone(),
        cursor_sdk_auth_file: tmp.path().join("live/cursor-auth.json"),
    });
    let keychain = Arc::new(FakeKeychain::default());
    let mut config = ManagedAccountsConfig::new(&data, claude, codex);
    config.credential_store = Arc::new(Shared(keychain.clone()));
    Rig {
        _tmp: tmp,
        agent,
        managed: ManagedAccounts::new(config),
        keychain,
    }
}

const CLAUDE_BODY: &str = r#"{"five_hour":{"utilization":42.0,"resets_at":null},"seven_day":{"utilization":10.0,"resets_at":null}}"#;
const CODEX_BODY: &str = r#"{"plan_type":"plus","rate_limit":{"primary_window":{"used_percent":25.0,"limit_window_seconds":18000,"reset_at":null}}}"#;

#[tokio::test]
async fn claude_usage_is_probed_with_the_managed_homes_own_keychain_token() {
    let rig = rig();
    let fake = fake_usage(200, CLAUDE_BODY).await;
    rig.agent.set_usage_urls(&fake.url, &fake.url);
    let account = rig.managed.create(HarnessId::ClaudeCode, "Work").unwrap();
    let home = rig.managed.home_of(HarnessId::ClaudeCode, &account.id);
    rig.keychain.items.lock().unwrap().push((
        claude_keychain_service(&home),
        r#"{"claudeAiOauth":{"accessToken":"tok-work","refreshToken":"r"}}"#.into(),
    ));

    let (harness, creds) = rig.managed.credentials_of(&account.id).unwrap();
    assert_eq!(harness, HarnessId::ClaudeCode);
    // Not forced and never probed: cache only, no request.
    assert!(
        rig.agent
            .managed_usage(harness, &account.id, &creds, false)
            .await
            .is_none()
    );
    assert!(fake.auth.lock().unwrap().is_empty());

    let (windows, _) = rig
        .agent
        .managed_usage(harness, &account.id, &creds, true)
        .await
        .unwrap();
    let labels: Vec<_> = windows.iter().map(|w| w.label.as_str()).collect();
    assert_eq!(labels, ["Session", "Week"]);
    assert!((windows[0].used_fraction - 0.42).abs() < 1e-6);
    assert_eq!(*fake.auth.lock().unwrap(), vec!["Bearer tok-work"]);

    // A plain list afterwards answers from the cache, with no new request.
    assert!(
        rig.agent
            .managed_usage(harness, &account.id, &creds, false)
            .await
            .is_some()
    );
    assert_eq!(fake.auth.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn claude_usage_falls_back_to_the_homes_credentials_file() {
    let rig = rig();
    let fake = fake_usage(200, CLAUDE_BODY).await;
    rig.agent.set_usage_urls(&fake.url, &fake.url);
    let account = rig.managed.create(HarnessId::ClaudeCode, "Work").unwrap();
    let home = rig.managed.home_of(HarnessId::ClaudeCode, &account.id);
    std::fs::write(
        home.join(".credentials.json"),
        r#"{"claudeAiOauth":{"accessToken":"tok-file"}}"#,
    )
    .unwrap();
    let (harness, creds) = rig.managed.credentials_of(&account.id).unwrap();
    assert!(
        rig.agent
            .managed_usage(harness, &account.id, &creds, true)
            .await
            .is_some()
    );
    assert_eq!(*fake.auth.lock().unwrap(), vec!["Bearer tok-file"]);
}

#[tokio::test]
async fn a_rejected_managed_token_is_unavailable_and_never_refreshed() {
    let rig = rig();
    let fake = fake_usage(401, "{}").await;
    rig.agent.set_usage_urls(&fake.url, &fake.url);
    let account = rig.managed.create(HarnessId::ClaudeCode, "Work").unwrap();
    let home = rig.managed.home_of(HarnessId::ClaudeCode, &account.id);
    let creds_file = home.join(".credentials.json");
    let original = r#"{"claudeAiOauth":{"accessToken":"stale","refreshToken":"r1"}}"#;
    std::fs::write(&creds_file, original).unwrap();
    let (harness, creds) = rig.managed.credentials_of(&account.id).unwrap();
    assert!(
        rig.agent
            .managed_usage(harness, &account.id, &creds, true)
            .await
            .is_none()
    );
    // One probe only (a refresh would hit another endpoint) and the file the
    // CLI owns is untouched.
    assert_eq!(fake.auth.lock().unwrap().len(), 1);
    assert_eq!(std::fs::read_to_string(&creds_file).unwrap(), original);
}

#[tokio::test]
async fn codex_usage_reads_the_homes_auth_json_and_the_live_plan() {
    let rig = rig();
    let fake = fake_usage(200, CODEX_BODY).await;
    rig.agent.set_usage_urls(&fake.url, &fake.url);
    let account = rig.managed.create(HarnessId::Codex, "Side").unwrap();
    let home = rig.managed.home_of(HarnessId::Codex, &account.id);
    std::fs::write(
        home.join("auth.json"),
        r#"{"tokens":{"access_token":"tok-codex","account_id":"acct"}}"#,
    )
    .unwrap();
    let (harness, creds) = rig.managed.credentials_of(&account.id).unwrap();
    assert_eq!(harness, HarnessId::Codex);
    let (windows, plan) = rig
        .agent
        .managed_usage(harness, &account.id, &creds, true)
        .await
        .unwrap();
    assert_eq!(windows.len(), 1);
    assert!((windows[0].used_fraction - 0.25).abs() < 1e-6);
    assert!(plan.is_some());
    assert_eq!(*fake.auth.lock().unwrap(), vec!["Bearer tok-codex"]);
}

#[test]
fn an_account_without_a_sign_in_has_no_credentials() {
    let rig = rig();
    let account = rig.managed.create(HarnessId::ClaudeCode, "Empty").unwrap();
    assert!(rig.managed.credentials_of(&account.id).is_none());
    assert!(rig.managed.credentials_of("missing").is_none());
}

#[test]
fn pinning_a_chat_at_start_overrides_the_project_default_once() {
    let rig = rig();
    let work = rig.managed.create(HarnessId::ClaudeCode, "Work").unwrap();
    let side = rig.managed.create(HarnessId::ClaudeCode, "Side").unwrap();
    rig.managed
        .set_project_account("s1", HarnessId::ClaudeCode, Some(&work.id))
        .unwrap();

    // Chip override: this chat uses Side although the project defaults to Work.
    rig.managed
        .pin_chat("c1", HarnessId::ClaudeCode, &side.id)
        .unwrap();
    let resolved = rig
        .managed
        .resolve_for_chat("c1", Some("s1"), HarnessId::ClaudeCode, false)
        .unwrap()
        .unwrap();
    assert_eq!(resolved.id, side.id);
    // A retry (or a later pick) never repins.
    rig.managed
        .pin_chat("c1", HarnessId::ClaudeCode, &work.id)
        .unwrap();
    assert_eq!(
        rig.managed
            .resolve_for_chat("c1", Some("s1"), HarnessId::ClaudeCode, false)
            .unwrap()
            .unwrap()
            .id,
        side.id
    );
    // An empty id pins Default even though the project has a default.
    rig.managed
        .pin_chat("c2", HarnessId::ClaudeCode, "")
        .unwrap();
    assert!(
        rig.managed
            .resolve_for_chat("c2", Some("s1"), HarnessId::ClaudeCode, false)
            .unwrap()
            .is_none()
    );
    // Unknown or wrong-harness accounts are refused.
    assert!(
        rig.managed
            .pin_chat("c3", HarnessId::ClaudeCode, "nope")
            .is_err()
    );
    let codex = rig.managed.create(HarnessId::Codex, "Codex").unwrap();
    assert!(
        rig.managed
            .pin_chat("c3", HarnessId::ClaudeCode, &codex.id)
            .is_err()
    );
}
