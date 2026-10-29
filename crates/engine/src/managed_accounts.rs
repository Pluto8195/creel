//! Managed agent accounts — per-project Claude Code / Codex logins.
//!
//! A managed account is a CLI home dir Mast creates under its own data dir
//! (`<data>/managed-accounts/<harness>/<id>/`, never under `~/.claude`). Chats
//! pinned to the account run their CLI with `CLAUDE_CONFIG_DIR` / `CODEX_HOME`
//! pointing there, so each account has its own credentials, settings and
//! session history and several can run at once. Mast never copies tokens: the
//! CLI itself writes the login (see `mast_harness::account_home`).
//!
//! The Default account (no entry anywhere) means "no env override": the live
//! `~/.claude` / `~/.codex`, untouched, exactly as before this module existed.
//!
//! State is device-local (homes are), in `<data>/managed-accounts/store.json`:
//! the account list, per-project defaults (`space id × harness → account`) and
//! per-chat pins. A chat's pin is decided at its first run and never changes:
//! a chat that already has a harness session keeps the Default login (its
//! history lives there), a brand-new chat takes its project's default.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use mast_proto::{
    AgentLoginPoll, AgentLoginStatus, ChatAccountPin, HarnessId, ManagedAccount,
    ManagedAccountsSnapshot, ProjectAccountDefault,
};

use crate::agent_accounts::{
    ensure_noop_browser, harness_slug, jwt_claims, lock, read_json, str_field, wire_login_child,
    write_file_atomic,
};
use crate::{EngineError, new_id, now_ms};

/// Harnesses that support managed accounts.
pub fn supports_managed_accounts(harness: HarnessId) -> bool {
    matches!(harness, HarnessId::ClaudeCode | HarnessId::Codex)
}

/// Where the Default (live) homes are, for config sharing. Tests point these
/// at temp dirs; production uses `AgentAccountsConfig`.
#[derive(Clone, Debug)]
pub struct ManagedAccountsConfig {
    /// `<data>/managed-accounts`.
    pub root: PathBuf,
    pub default_claude_dir: PathBuf,
    pub default_codex_home: PathBuf,
    /// Sign-in CLI overrides (tests: fake CLIs). `None` = resolve the real one.
    pub login_executables: BTreeMap<String, PathBuf>,
}

impl ManagedAccountsConfig {
    pub fn new(data_dir: &Path, claude_dir: PathBuf, codex_home: PathBuf) -> Self {
        Self {
            root: data_dir.join("managed-accounts"),
            default_claude_dir: claude_dir,
            default_codex_home: codex_home,
            login_executables: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredAccount {
    id: String,
    harness: HarnessId,
    label: String,
    created_at_ms: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Store {
    accounts: Vec<StoredAccount>,
    /// `"<space id>|<harness slug>"` → account id.
    project_defaults: BTreeMap<String, String>,
    /// `"<chat id>|<harness slug>"` → account id; empty string = Default.
    chat_pins: BTreeMap<String, String>,
}

fn key(a: &str, harness: HarnessId) -> String {
    format!("{a}|{}", harness_slug(harness))
}

fn split_key(key: &str) -> Option<(&str, HarnessId)> {
    let (id, slug) = key.rsplit_once('|')?;
    let harness = [HarnessId::ClaudeCode, HarnessId::Codex]
        .into_iter()
        .find(|h| harness_slug(*h) == slug)?;
    Some((id, harness))
}

struct Login {
    account_id: String,
    child: Arc<Mutex<Option<mast_harness::process::Child>>>,
    output: Arc<Mutex<String>>,
    exit: Arc<Mutex<Option<Option<i32>>>>,
    started: Instant,
}

const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Cheap to clone; shared by the engine, RPC layer and sessions.
#[derive(Clone)]
pub struct ManagedAccounts {
    config: Arc<ManagedAccountsConfig>,
    store: Arc<Mutex<Store>>,
    logins: Arc<Mutex<BTreeMap<String, Login>>>,
}

/// What a chat's run resolved to.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAccount {
    pub id: String,
    pub home: PathBuf,
}

impl ManagedAccounts {
    pub fn new(config: ManagedAccountsConfig) -> Self {
        let store = std::fs::read(config.root.join("store.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            config: Arc::new(config),
            store: Arc::new(Mutex::new(store)),
            logins: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    fn save(&self, store: &Store) -> Result<(), EngineError> {
        std::fs::create_dir_all(&self.config.root)?;
        let bytes =
            serde_json::to_vec_pretty(store).map_err(|e| EngineError::Other(e.to_string()))?;
        write_file_atomic(&self.config.root.join("store.json"), &bytes, false)
    }

    /// The CLI home of an account. Always under `root`, never `~/.claude`.
    pub fn home_of(&self, harness: HarnessId, id: &str) -> PathBuf {
        self.config.root.join(harness_slug(harness)).join(id)
    }

    // ── accounts ────────────────────────────────────────────────────────────

    pub fn create(&self, harness: HarnessId, label: &str) -> Result<ManagedAccount, EngineError> {
        if !supports_managed_accounts(harness) {
            return Err(EngineError::Other(format!(
                "{} does not support per-project accounts",
                harness_slug(harness)
            )));
        }
        let label = label.trim();
        if label.is_empty() {
            return Err(EngineError::Other("Give the account a name.".into()));
        }
        let account = StoredAccount {
            id: new_id(),
            harness,
            label: label.to_string(),
            created_at_ms: now_ms(),
        };
        let home = self.home_of(harness, &account.id);
        create_private_dir(&home)?;
        self.prepare_home(harness, &home)?;
        let mut store = lock(&self.store);
        store.accounts.push(account.clone());
        self.save(&store)?;
        Ok(self.view(&account))
    }

    /// Remove an account and its home. Refused while a chat is pinned to it
    /// (that chat's session history lives in the home) or a project uses it.
    pub fn remove(&self, id: &str) -> Result<(), EngineError> {
        let mut store = lock(&self.store);
        let Some(pos) = store.accounts.iter().position(|a| a.id == id) else {
            return Ok(());
        };
        if store.chat_pins.values().any(|v| v == id) {
            return Err(EngineError::Other(
                "Chats are pinned to this account; their history lives in it.".into(),
            ));
        }
        let account = store.accounts.remove(pos);
        store.project_defaults.retain(|_, v| v != id);
        self.save(&store)?;
        let home = self.home_of(account.harness, &account.id);
        if home.starts_with(&self.config.root) {
            let _ = std::fs::remove_dir_all(home);
        }
        Ok(())
    }

    fn view(&self, account: &StoredAccount) -> ManagedAccount {
        let home = self.home_of(account.harness, &account.id);
        let email = read_identity(account.harness, &home);
        ManagedAccount {
            id: account.id.clone(),
            harness: account.harness,
            label: account.label.clone(),
            signed_in: email.is_some() || has_credentials_file(account.harness, &home),
            email,
        }
    }

    pub fn snapshot(&self) -> ManagedAccountsSnapshot {
        let store = lock(&self.store);
        ManagedAccountsSnapshot {
            accounts: store.accounts.iter().map(|a| self.view(a)).collect(),
            project_defaults: store
                .project_defaults
                .iter()
                .filter_map(|(k, v)| {
                    let (space_id, harness) = split_key(k)?;
                    Some(ProjectAccountDefault {
                        space_id: space_id.to_string(),
                        harness,
                        account_id: Some(v.clone()),
                    })
                })
                .collect(),
            chat_pins: store
                .chat_pins
                .iter()
                .filter_map(|(k, v)| {
                    let (chat_id, harness) = split_key(k)?;
                    Some(ChatAccountPin {
                        chat_id: chat_id.to_string(),
                        harness,
                        account_id: (!v.is_empty()).then(|| v.clone()),
                    })
                })
                .collect(),
        }
    }

    // ── project defaults and chat pins ──────────────────────────────────────

    /// Set (or clear, with `None`) a project's default account for a harness.
    /// Affects only chats that have not run yet; existing chats keep their pin.
    pub fn set_project_account(
        &self,
        space_id: &str,
        harness: HarnessId,
        account_id: Option<&str>,
    ) -> Result<(), EngineError> {
        let mut store = lock(&self.store);
        let k = key(space_id, harness);
        match account_id {
            Some(id) => {
                if !store
                    .accounts
                    .iter()
                    .any(|a| a.id == id && a.harness == harness)
                {
                    return Err(EngineError::Other("Unknown account for this agent.".into()));
                }
                store.project_defaults.insert(k, id.to_string());
            }
            None => {
                store.project_defaults.remove(&k);
            }
        }
        self.save(&store)
    }

    /// The account a chat runs on, deciding and persisting its pin on the
    /// first call. `Ok(None)` = Default (no env override).
    ///
    /// - already pinned → the pin (an account that has since been removed is
    ///   an error, never a silent switch to another login);
    /// - no pin but the chat already has a harness session (it ran before
    ///   per-project accounts existed, or on Default) → pinned to Default,
    ///   since its history lives in the live home;
    /// - otherwise (brand-new chat) → the project's default, pinned.
    pub fn resolve_for_chat(
        &self,
        chat_id: &str,
        space_id: Option<&str>,
        harness: HarnessId,
        has_prior_session: bool,
    ) -> Result<Option<ResolvedAccount>, EngineError> {
        if !supports_managed_accounts(harness) {
            return Ok(None);
        }
        let mut store = lock(&self.store);
        let pin_key = key(chat_id, harness);
        let pinned = match store.chat_pins.get(&pin_key) {
            Some(pin) => pin.clone(),
            None => {
                let chosen = if has_prior_session {
                    String::new()
                } else {
                    space_id
                        .and_then(|s| store.project_defaults.get(&key(s, harness)))
                        .cloned()
                        .unwrap_or_default()
                };
                store.chat_pins.insert(pin_key, chosen.clone());
                self.save(&store)?;
                chosen
            }
        };
        if pinned.is_empty() {
            return Ok(None);
        }
        if !store.accounts.iter().any(|a| a.id == pinned) {
            return Err(EngineError::Other(
                "This chat's account was removed; start a new chat to use another.".into(),
            ));
        }
        drop(store);
        let home = self.home_of(harness, &pinned);
        // Refresh shared config (cheap mtime checks) before every spawn.
        self.prepare_home(harness, &home)?;
        Ok(Some(ResolvedAccount { id: pinned, home }))
    }

    /// The home a chat is already pinned to (no side effects): for work that
    /// belongs to the chat but runs outside its dispatch, such as titles.
    pub fn pinned_home(&self, chat_id: &str, harness: HarnessId) -> Option<PathBuf> {
        let store = lock(&self.store);
        let pinned = store.chat_pins.get(&key(chat_id, harness))?;
        (!pinned.is_empty() && store.accounts.iter().any(|a| &a.id == pinned))
            .then(|| self.home_of(harness, pinned))
    }

    // ── home preparation and config sharing ─────────────────────────────────

    fn prepare_home(&self, harness: HarnessId, home: &Path) -> Result<(), EngineError> {
        create_private_dir(home)?;
        match harness {
            HarnessId::ClaudeCode => share_claude_config(&self.config.default_claude_dir, home),
            HarnessId::Codex => share_codex_config(&self.config.default_codex_home, home),
            _ => Ok(()),
        }
    }

    // ── sign-in ─────────────────────────────────────────────────────────────

    /// Run the CLI's own login against the account's home. Returns the login
    /// id and the authorization URL when the CLI printed one in time.
    pub async fn start_login(&self, account_id: &str) -> Result<(String, String), EngineError> {
        let account = lock(&self.store)
            .accounts
            .iter()
            .find(|a| a.id == account_id)
            .cloned()
            .ok_or_else(|| EngineError::Other("Unknown account.".into()))?;
        let home = self.home_of(account.harness, &account.id);
        self.prepare_home(account.harness, &home)?;
        let mut command = self.login_command(account.harness, &home)?;
        command
            .stdin(mast_harness::process::Stdio::null())
            .stdout(mast_harness::process::Stdio::piped())
            .stderr(mast_harness::process::Stdio::piped());
        // Codex: the app opens the single authorization tab (as the global
        // Add account flow does), so mute the CLI's own browser open.
        #[cfg(unix)]
        if account.harness == HarnessId::Codex
            && let Some(noop) = ensure_noop_browser(&self.config.root)
        {
            command.env("BROWSER", noop);
        }
        let child = command
            .spawn()
            .map_err(|e| EngineError::Other(format!("Could not start the sign-in: {e}")))?;
        let (child, output, exit) = wire_login_child(child);
        let login_id = new_id();
        lock(&self.logins).insert(
            login_id.clone(),
            Login {
                account_id: account.id,
                child,
                output: output.clone(),
                exit: exit.clone(),
                started: Instant::now(),
            },
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        let url = loop {
            if let Some(url) = scan_https_url(&lock(&output)) {
                break url;
            }
            if lock(&exit).is_some() || Instant::now() > deadline {
                break String::new();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        Ok((login_id, url))
    }

    fn login_command(
        &self,
        harness: HarnessId,
        home: &Path,
    ) -> Result<mast_harness::process::Command, EngineError> {
        if let Some(exe) = self.config.login_executables.get(harness_slug(harness)) {
            let mut cmd = mast_harness::process::Command::new(exe);
            cmd.args(if harness == HarnessId::ClaudeCode {
                vec!["auth", "login"]
            } else {
                vec!["login"]
            });
            mast_harness::account_home::apply(&mut cmd, harness, Some(home));
            return Ok(cmd);
        }
        let resolved = match harness {
            HarnessId::ClaudeCode => mast_harness::claude::login_command(home),
            _ => mast_harness::codex::login_command(home),
        };
        resolved.map_err(|e| EngineError::Other(format!("The CLI was not found: {e}")))
    }

    pub fn poll_login(&self, login_id: &str) -> AgentLoginPoll {
        let mut logins = lock(&self.logins);
        let Some(login) = logins.get(login_id) else {
            return AgentLoginPoll {
                status: AgentLoginStatus::Error,
                message: Some("This sign-in is no longer running.".into()),
                url: None,
            };
        };
        let exit = *lock(&login.exit);
        let url = scan_https_url(&lock(&login.output));
        match exit {
            None if login.started.elapsed() > LOGIN_TIMEOUT => {
                kill_login(login);
                logins.remove(login_id);
                AgentLoginPoll {
                    status: AgentLoginStatus::Error,
                    message: Some("Sign-in timed out.".into()),
                    url: None,
                }
            }
            None => AgentLoginPoll {
                status: AgentLoginStatus::Pending,
                message: None,
                url,
            },
            Some(Some(0)) => {
                logins.remove(login_id);
                AgentLoginPoll {
                    status: AgentLoginStatus::Done,
                    message: None,
                    url: None,
                }
            }
            Some(code) => {
                logins.remove(login_id);
                AgentLoginPoll {
                    status: AgentLoginStatus::Error,
                    message: Some(match code {
                        Some(code) => format!("The CLI's sign-in exited with status {code}."),
                        None => "The CLI's sign-in was interrupted.".into(),
                    }),
                    url: None,
                }
            }
        }
    }

    pub fn cancel_login(&self, login_id: &str) {
        if let Some(login) = lock(&self.logins).remove(login_id) {
            kill_login(&login);
        }
    }

    /// The account a login belongs to (for refreshing its view after Done).
    pub fn login_account(&self, login_id: &str) -> Option<String> {
        lock(&self.logins)
            .get(login_id)
            .map(|login| login.account_id.clone())
    }

    pub fn shutdown(&self) {
        for (_, login) in std::mem::take(&mut *lock(&self.logins)) {
            kill_login(&login);
        }
    }
}

fn kill_login(login: &Login) {
    if let Some(child) = lock(&login.child).as_mut() {
        let _ = child.start_kill();
    }
}

fn create_private_dir(path: &Path) -> Result<(), EngineError> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn scan_https_url(output: &str) -> Option<String> {
    let start = output.find("https://")?;
    let rest = &output[start..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

fn has_credentials_file(harness: HarnessId, home: &Path) -> bool {
    match harness {
        HarnessId::ClaudeCode => home.join(".credentials.json").is_file(),
        HarnessId::Codex => home.join("auth.json").is_file(),
        _ => false,
    }
}

/// Email of the account signed in under `home`, read from the identity files
/// the CLI writes (never the credentials themselves).
fn read_identity(harness: HarnessId, home: &Path) -> Option<String> {
    match harness {
        HarnessId::ClaudeCode => {
            let config = read_json(&home.join(".claude.json"))?;
            str_field(config.get("oauthAccount")?, "emailAddress")
        }
        HarnessId::Codex => {
            let auth = read_json(&home.join("auth.json"))?;
            let id_token = auth.get("tokens")?.get("id_token")?.as_str()?;
            str_field(&jwt_claims(id_token)?, "email")
        }
        _ => None,
    }
}

// ── sharing the user's config into a managed home ───────────────────────────

/// Items symlinked from the Default Claude dir. Read-only from Mast's side:
/// edits made anywhere show everywhere. Session history, plugins and
/// credentials are never shared.
const CLAUDE_SHARED_ITEMS: &[&str] = &[
    "skills",
    "agents",
    "commands",
    "rules",
    "CLAUDE.md",
    "keybindings.json",
];

fn share_claude_config(default_dir: &Path, home: &Path) -> Result<(), EngineError> {
    for item in CLAUDE_SHARED_ITEMS {
        link_if_missing(&default_dir.join(item), &home.join(item));
    }
    // settings.json can carry auth (apiKeyHelper, tokens in env), so it is
    // generated, not linked: a copy minus those keys, refreshed when the
    // Default file changes.
    regenerate_if_stale(
        &default_dir.join("settings.json"),
        &home.join("settings.json"),
        |text| {
            let mut value: serde_json::Value = serde_json::from_str(text).ok()?;
            strip_auth_settings(&mut value);
            serde_json::to_string_pretty(&value).ok()
        },
    )
}

fn strip_auth_settings(settings: &mut serde_json::Value) {
    let Some(map) = settings.as_object_mut() else {
        return;
    };
    for key in [
        "apiKeyHelper",
        "awsAuthRefresh",
        "awsCredentialExport",
        "forceLoginOrgUUID",
    ] {
        map.remove(key);
    }
    if let Some(env) = map.get_mut("env").and_then(|e| e.as_object_mut()) {
        env.retain(|k, _| {
            let upper = k.to_ascii_uppercase();
            !(upper.contains("TOKEN")
                || upper.contains("KEY")
                || upper.contains("SECRET")
                || upper.contains("PASSWORD")
                || upper == "ANTHROPIC_BASE_URL")
        });
    }
}

fn share_codex_config(default_home: &Path, home: &Path) -> Result<(), EngineError> {
    for item in ["skills", "prompts", "AGENTS.md"] {
        link_if_missing(&default_home.join(item), &home.join(item));
    }
    // Isolation per home is only certain with file credentials, so the
    // generated config always forces them.
    let source = default_home.join("config.toml");
    let target = home.join("config.toml");
    let build = |text: &str| {
        let mut out = String::from(
            "# Generated by Mast from your default Codex config. Edits here are overwritten.\n\
             cli_auth_credentials_store = \"file\"\n",
        );
        let mut top_level = true;
        for line in text.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with('[') {
                top_level = false;
            }
            if top_level && trimmed.starts_with("cli_auth_credentials_store") {
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        Some(out)
    };
    if source.is_file() {
        regenerate_if_stale(&source, &target, build)
    } else if !target.exists() {
        write_file_atomic(
            &target,
            b"# Generated by Mast.\ncli_auth_credentials_store = \"file\"\n",
            false,
        )
    } else {
        Ok(())
    }
}

/// Symlink `link` -> `source` when the source exists and `link` doesn't.
fn link_if_missing(source: &Path, link: &Path) {
    if !source.exists() || link.symlink_metadata().is_ok() {
        return;
    }
    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink(source, link);
    #[cfg(not(unix))]
    let _ = (source, link);
}

/// Rewrite `target` from `source` through `transform` when `source` is newer
/// than the last generation (recorded in a `.mast-generated` sidecar).
fn regenerate_if_stale(
    source: &Path,
    target: &Path,
    transform: impl Fn(&str) -> Option<String>,
) -> Result<(), EngineError> {
    let Ok(meta) = std::fs::metadata(source) else {
        return Ok(());
    };
    let stamp = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos().to_string())
        .unwrap_or_default();
    let sidecar = target.with_extension("mast-generated");
    if target.exists() && std::fs::read_to_string(&sidecar).ok().as_deref() == Some(stamp.as_str())
    {
        return Ok(());
    }
    let Some(text) = std::fs::read_to_string(source)
        .ok()
        .and_then(|t| transform(&t))
    else {
        return Ok(());
    };
    write_file_atomic(target, text.as_bytes(), false)?;
    write_file_atomic(&sidecar, stamp.as_bytes(), false)
}
