//! Promoting a saved (global-switch) login into a managed project account.
//! Everything lives under a tempdir with a fake Keychain: the live
//! `~/.claude` / `~/.codex` and the default Keychain item are never touched.

use std::path::Path;
use std::sync::{Arc, Mutex};

use mast_engine::{
    AgentAccounts, AgentAccountsConfig, HomeCredentialStore, ManagedAccounts,
    ManagedAccountsConfig, claude_keychain_service,
};
use mast_proto::HarnessId;

#[derive(Debug, Default)]
struct FakeKeychain {
    unavailable: bool,
    items: Mutex<Vec<(String, String)>>,
    deleted: Mutex<Vec<String>>,
}

#[derive(Debug)]
struct Shared(Arc<FakeKeychain>);

impl HomeCredentialStore for Shared {
    fn available(&self) -> bool {
        !self.0.unavailable
    }
    fn put(&self, service: &str, secret: &str) -> Result<(), mast_engine::EngineError> {
        self.0
            .items
            .lock()
            .unwrap()
            .push((service.to_string(), secret.to_string()));
        Ok(())
    }
    fn delete(&self, service: &str) {
        self.0.deleted.lock().unwrap().push(service.to_string());
    }
}

struct Rig {
    tmp: tempfile::TempDir,
    agent: AgentAccounts,
    managed: ManagedAccounts,
    keychain: Arc<FakeKeychain>,
}

fn rig(unavailable: bool) -> Rig {
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
    let keychain = Arc::new(FakeKeychain {
        unavailable,
        ..Default::default()
    });
    let mut config = ManagedAccountsConfig::new(&data, claude, codex);
    config.credential_store = Arc::new(Shared(keychain.clone()));
    Rig {
        tmp,
        agent,
        managed: ManagedAccounts::new(config),
        keychain,
    }
}

const SLOT_A: &str = "aaaaaaaaaaaaaaaa";
const SLOT_B: &str = "bbbbbbbbbbbbbbbb";

fn write_slot(root: &Path, harness: &str, id: &str, key: &str, credentials: serde_json::Value) {
    let dir = root.join("data/agent-accounts").join(harness);
    std::fs::create_dir_all(&dir).unwrap();
    let slot = serde_json::json!({
        "id": id,
        "harness": harness,
        "accountKey": key,
        "profile": { "email": format!("{key}@test"), "authKind": "oauth" },
        "credentials": credentials,
        "claudeConfig": { "oauthAccount": { "accountUuid": key, "emailAddress": format!("{key}@test") }, "userID": "u1" },
        "savedAt": 1,
    });
    std::fs::write(dir.join(format!("{id}.json")), slot.to_string()).unwrap();
}

fn claude_creds(token: &str) -> serde_json::Value {
    serde_json::json!({
        "claudeAiOauth": { "accessToken": token, "refreshToken": format!("r-{token}") },
        "mcpOAuth": { "server": "machine-shared" },
    })
}

/// Make `key` the live Default Claude login (file credentials, so the Keychain
/// is never consulted) and let `list` record it.
async fn set_live(rig: &Rig, key: &str) {
    let live = rig.tmp.path().join("live/.claude");
    std::fs::write(
        live.join(".claude.json"),
        serde_json::json!({ "oauthAccount": { "accountUuid": key, "emailAddress": format!("{key}@test") } })
            .to_string(),
    )
    .unwrap();
    std::fs::write(live.join(".credentials.json"), claude_creds("live").to_string()).unwrap();
    rig.agent.list(false).await.unwrap();
}

#[tokio::test]
async fn promoting_writes_the_homes_own_keychain_item_and_marks_the_slot() {
    let rig = rig(false);
    write_slot(rig.tmp.path(), "claude-code", SLOT_A, "acct-a", claude_creds("tok-a"));
    write_slot(rig.tmp.path(), "claude-code", SLOT_B, "acct-b", claude_creds("tok-b"));
    set_live(&rig, "acct-b").await;

    let slot = rig
        .agent
        .export_for_promotion(HarnessId::ClaudeCode, SLOT_A)
        .unwrap();
    let account = rig.managed.promote(HarnessId::ClaudeCode, "", &slot).unwrap();
    rig.agent
        .mark_promoted(HarnessId::ClaudeCode, SLOT_A, &account.id)
        .unwrap();

    let home = rig.managed.home_of(HarnessId::ClaudeCode, &account.id);
    let items = rig.keychain.items.lock().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].0, claude_keychain_service(&home));
    assert!(items[0].0.starts_with("Claude Code-credentials-"));
    assert_ne!(items[0].0, "Claude Code-credentials");
    let blob: serde_json::Value = serde_json::from_str(&items[0].1).unwrap();
    assert_eq!(blob["claudeAiOauth"]["refreshToken"], "r-tok-a");
    // Machine-shared MCP secrets are not copied into the project home.
    assert!(blob.get("mcpOAuth").is_none());
    // No file fallback when the Keychain took it.
    assert!(!home.join(".credentials.json").exists());
    // Identity is in the home, so it reads as signed in.
    assert!(account.signed_in);
    assert_eq!(account.email.as_deref(), Some("acct-a@test"));

    // The slot left the picker and cannot be switched to or promoted twice.
    let offered: Vec<_> = rig.agent.saved_options().into_iter().map(|s| s.id).collect();
    assert!(!offered.contains(&SLOT_A.to_string()));
    assert!(rig.agent.export_for_promotion(HarnessId::ClaudeCode, SLOT_A).is_err());
    assert!(rig.agent.activate(HarnessId::ClaudeCode, SLOT_A).await.is_err());
    let listed = rig.agent.list(false).await.unwrap();
    let a = listed.accounts.iter().find(|a| a.id == SLOT_A).unwrap();
    assert!(a.promoted && !a.switchable);

    // Removing the project account deletes only its own Keychain item.
    rig.managed.remove(&account.id).unwrap();
    assert_eq!(
        *rig.keychain.deleted.lock().unwrap(),
        vec![claude_keychain_service(&home)]
    );
}

#[tokio::test]
async fn the_live_default_login_cannot_be_promoted() {
    let rig = rig(false);
    write_slot(rig.tmp.path(), "claude-code", SLOT_A, "acct-a", claude_creds("tok-a"));
    // Before any list, the live login is unknown: refuse rather than guess.
    assert!(rig.agent.export_for_promotion(HarnessId::ClaudeCode, SLOT_A).is_err());
    set_live(&rig, "acct-a").await;
    let err = rig
        .agent
        .export_for_promotion(HarnessId::ClaudeCode, SLOT_A)
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("Default login"), "{err}");
    let option = rig
        .agent
        .saved_options()
        .into_iter()
        .find(|s| s.id == SLOT_A)
        .unwrap();
    assert!(option.active);
    assert!(rig.keychain.items.lock().unwrap().is_empty());
}

#[tokio::test]
async fn without_a_keychain_the_credentials_file_is_the_fallback() {
    let rig = rig(true);
    write_slot(rig.tmp.path(), "claude-code", SLOT_A, "acct-a", claude_creds("tok-a"));
    set_live(&rig, "someone-else").await;
    let slot = rig.agent.export_for_promotion(HarnessId::ClaudeCode, SLOT_A).unwrap();
    let account = rig.managed.promote(HarnessId::ClaudeCode, "Work", &slot).unwrap();
    let home = rig.managed.home_of(HarnessId::ClaudeCode, &account.id);
    assert!(rig.keychain.items.lock().unwrap().is_empty());
    let blob: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join(".credentials.json")).unwrap()).unwrap();
    assert_eq!(blob["claudeAiOauth"]["accessToken"], "tok-a");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = home.join(".credentials.json").metadata().unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert_eq!(account.label, "Work");
}

#[tokio::test]
async fn codex_slots_become_auth_json_in_the_new_home() {
    let rig = rig(false);
    let auth = serde_json::json!({ "tokens": { "access_token": "at", "refresh_token": "rt" } });
    write_slot(rig.tmp.path(), "codex", SLOT_A, "acct-x", auth.clone());
    rig.agent.list(false).await.unwrap();
    let slot = rig.agent.export_for_promotion(HarnessId::Codex, SLOT_A).unwrap();
    let account = rig.managed.promote(HarnessId::Codex, "", &slot).unwrap();
    let home = rig.managed.home_of(HarnessId::Codex, &account.id);
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join("auth.json")).unwrap()).unwrap();
    assert_eq!(written, auth);
    assert!(rig.keychain.items.lock().unwrap().is_empty());
    // Nothing was written to the live codex home.
    assert_eq!(
        std::fs::read_dir(rig.tmp.path().join("live/.codex")).unwrap().count(),
        0
    );
}

#[tokio::test]
async fn bad_slot_ids_are_rejected_before_touching_disk() {
    let rig = rig(false);
    rig.agent.list(false).await.unwrap();
    for id in ["../../etc/passwd", "short", "zzzzzzzzzzzzzzzz"] {
        assert!(rig.agent.export_for_promotion(HarnessId::ClaudeCode, id).is_err());
    }
}
