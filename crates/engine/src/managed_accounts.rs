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
    ensure_noop_browser, harness_slug, jwt_claims, lock, read_json, str_field, write_file_atomic,
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
    /// Where a promoted Claude login goes besides the file fallback: the
    /// managed home's own Keychain item. Tests inject a fake.
    pub credential_store: Arc<dyn HomeCredentialStore>,
}

/// Keychain writes for a managed Claude home. Only ever the home's own item
/// (`Claude Code-credentials-<hash>`); the default item is never touched.
pub trait HomeCredentialStore: Send + Sync + std::fmt::Debug {
    /// False ⇒ the engine writes `.credentials.json` in the home instead.
    fn available(&self) -> bool;
    fn put(&self, service: &str, secret: &str) -> Result<(), EngineError>;
    fn delete(&self, service: &str);
}

/// The macOS login Keychain via `security -i` (the secret goes over stdin as
/// hex, never on a command line). Unavailable elsewhere.
#[derive(Debug)]
pub struct SystemCredentialStore;

impl HomeCredentialStore for SystemCredentialStore {
    fn available(&self) -> bool {
        cfg!(target_os = "macos")
    }

    fn put(&self, service: &str, secret: &str) -> Result<(), EngineError> {
        let hex: String = secret.bytes().map(|b| format!("{b:02x}")).collect();
        let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
        run_security(&format!(
            "add-generic-password -U -a \"{user}\" -s \"{service}\" -X {hex}\n"
        ))
    }

    fn delete(&self, service: &str) {
        let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
        let _ = run_security(&format!(
            "delete-generic-password -a \"{user}\" -s \"{service}\"\n"
        ));
    }
}

fn run_security(script: &str) -> Result<(), EngineError> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = std::process::Command::new("security")
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| EngineError::Other(format!("Keychain unavailable: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(script.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| EngineError::Other(format!("Keychain write failed: {e}")))?;
    // `security -i` reports command errors on stderr and still exits 0.
    if out.status.success() && out.stderr.is_empty() {
        Ok(())
    } else {
        Err(EngineError::Other("Keychain write failed.".into()))
    }
}

/// Claude Code's Keychain service for a config dir: `Claude Code-credentials-`
/// plus the first 8 hex chars of sha256(dir).
pub fn claude_keychain_service(home: &Path) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(home.to_string_lossy().as_bytes());
    let hex: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("Claude Code-credentials-{hex}")
}

impl ManagedAccountsConfig {
    pub fn new(data_dir: &Path, claude_dir: PathBuf, codex_home: PathBuf) -> Self {
        Self {
            root: data_dir.join("managed-accounts"),
            default_claude_dir: claude_dir,
            default_codex_home: codex_home,
            login_executables: BTreeMap::new(),
            credential_store: Arc::new(SystemCredentialStore),
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
    harness: HarnessId,
    proc: LoginProc,
    started: Instant,
}

/// A login CLI running on a PTY: output is scanned for the URL and prompts,
/// and the pasted code goes to its stdin. Output is never logged or returned.
struct LoginProc {
    output: Arc<Mutex<String>>,
    exit: Arc<Mutex<Option<Option<i32>>>>,
    /// The output reader hit EOF (so nothing printed is still in flight).
    closed: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(unix)]
    writer: Mutex<Box<dyn std::io::Write + Send>>,
    #[cfg(unix)]
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    /// Keeps the PTY open for the life of the login.
    #[cfg(unix)]
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

impl LoginProc {
    #[cfg(unix)]
    fn spawn(command: &mast_harness::process::Command) -> Result<Self, EngineError> {
        use portable_pty::{CommandBuilder, PtySize, native_pty_system};
        let std = command.as_std();
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                // Wide, so the CLI never hard-wraps the long authorize URL.
                cols: 4000,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| EngineError::Other(format!("Could not open a terminal: {e}")))?;
        let mut cmd = CommandBuilder::new(std.get_program());
        cmd.args(std.get_args());
        for (name, value) in std.get_envs() {
            match value {
                Some(value) => cmd.env(name, value),
                None => cmd.env_remove(name),
            }
        }
        if let Some(dir) = std.get_current_dir() {
            cmd.cwd(dir);
        }
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| EngineError::Other(format!("Could not start the sign-in: {e}")))?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| EngineError::Other(format!("Could not read the sign-in: {e}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| EngineError::Other(format!("Could not write to the sign-in: {e}")))?;
        let output = Arc::new(Mutex::new(String::new()));
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let output = output.clone();
            let closed = closed.clone();
            std::thread::spawn(move || {
                use std::io::Read;
                let mut buf = [0u8; 4096];
                let mut pending: Vec<u8> = Vec::new();
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    pending.extend_from_slice(&buf[..n]);
                    // Keep a split UTF-8 tail for the next read.
                    let valid = match std::str::from_utf8(&pending) {
                        Ok(_) => pending.len(),
                        Err(e) => e.valid_up_to(),
                    };
                    let text = String::from_utf8_lossy(&pending[..valid]).into_owned();
                    pending.drain(..valid);
                    let mut out = lock(&output);
                    out.push_str(&strip_ansi(&text));
                    if out.len() > 256 * 1024 {
                        let cut = out.len() - 128 * 1024;
                        let cut = (cut..out.len())
                            .find(|i| out.is_char_boundary(*i))
                            .unwrap_or(out.len());
                        out.drain(..cut);
                    }
                }
                closed.store(true, std::sync::atomic::Ordering::SeqCst);
            });
        }
        let exit: Arc<Mutex<Option<Option<i32>>>> = Arc::new(Mutex::new(None));
        {
            let exit = exit.clone();
            std::thread::spawn(move || {
                let code = child.wait().ok().map(|s| s.exit_code() as i32);
                *lock(&exit) = Some(code);
            });
        }
        Ok(Self {
            output,
            exit,
            closed,
            writer: Mutex::new(writer),
            killer: Mutex::new(killer),
            _master: pair.master,
        })
    }

    #[cfg(not(unix))]
    fn spawn(_command: &mast_harness::process::Command) -> Result<Self, EngineError> {
        Err(EngineError::Other(
            "Interactive sign-in is not supported on this platform.".into(),
        ))
    }

    /// Send one line to the CLI's stdin. `\r` is Enter on a PTY (translated
    /// to a newline when the terminal is in line mode).
    fn send_line(&self, line: &str) -> Result<(), EngineError> {
        #[cfg(unix)]
        {
            use std::io::Write;
            let mut writer = lock(&self.writer);
            writer
                .write_all(line.as_bytes())
                .and_then(|_| writer.write_all(b"\r"))
                .and_then(|_| writer.flush())
                .map_err(|_| EngineError::Other("The sign-in is no longer listening.".into()))
        }
        #[cfg(not(unix))]
        {
            let _ = line;
            Err(EngineError::Other("Not supported.".into()))
        }
    }

    fn kill(&self) {
        #[cfg(unix)]
        {
            let _ = lock(&self.killer).kill();
        }
    }
}

/// Drop terminal escape sequences (CSI, OSC) and bare control characters
/// other than newlines, so scans see plain text.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                Some('[') => {
                    for n in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(n) = chars.next() {
                        if n == '\x07' {
                            break;
                        }
                        if n == '\x1b' {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' => {}
            c if c.is_control() && c != '\n' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
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

    /// Copy a saved login (from Mast's own slot storage) into a new managed
    /// home, in the form the CLI reads: Claude → the home's own Keychain item
    /// (or `.credentials.json` when no Keychain) plus `oauthAccount` in the
    /// home's `.claude.json`; Codex → `auth.json`. After this the CLI is the
    /// only writer. Shared MCP/plugin secrets in the slot blob are dropped.
    pub fn promote(
        &self,
        harness: HarnessId,
        label: &str,
        slot: &crate::agent_accounts::PromotableSlot,
    ) -> Result<ManagedAccount, EngineError> {
        let label = if label.trim().is_empty() {
            slot.email.as_str()
        } else {
            label
        };
        let account = self.create(harness, label)?;
        let home = self.home_of(harness, &account.id);
        if let Err(err) = self.write_promoted_credentials(harness, &home, slot) {
            let _ = self.remove(&account.id);
            return Err(err);
        }
        Ok(self.view_by_id(&account.id).unwrap_or(account))
    }

    fn view_by_id(&self, id: &str) -> Option<ManagedAccount> {
        let store = lock(&self.store);
        store
            .accounts
            .iter()
            .find(|a| a.id == id)
            .map(|a| self.view(a))
    }

    fn write_promoted_credentials(
        &self,
        harness: HarnessId,
        home: &Path,
        slot: &crate::agent_accounts::PromotableSlot,
    ) -> Result<(), EngineError> {
        match harness {
            HarnessId::ClaudeCode => {
                let oauth = slot.credentials.get("claudeAiOauth").ok_or_else(|| {
                    EngineError::Other("The saved login has no sign-in to copy.".into())
                })?;
                let blob = serde_json::json!({ "claudeAiOauth": oauth }).to_string();
                let store = &self.config.credential_store;
                if mast_harness::account_home::CLAUDE_KEYCHAIN_ISOLATED && store.available() {
                    store.put(&claude_keychain_service(home), &blob)?;
                } else {
                    write_file_atomic(&home.join(".credentials.json"), blob.as_bytes(), true)?;
                }
                // Identity the CLI reads to treat the home as signed in.
                let mut config = serde_json::json!({});
                if let Some(saved) = &slot.claude_config {
                    for key in ["oauthAccount", "userID"] {
                        if let Some(v) = saved.get(key) {
                            config[key] = v.clone();
                        }
                    }
                }
                write_file_atomic(&home.join(".claude.json"), config.to_string().as_bytes(), true)
            }
            HarnessId::Codex => {
                let json = serde_json::to_string_pretty(&slot.credentials)
                    .map_err(|e| EngineError::Other(e.to_string()))?;
                write_file_atomic(&home.join("auth.json"), json.as_bytes(), true)
            }
            _ => Err(EngineError::Other("Unsupported harness.".into())),
        }
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
        if account.harness == HarnessId::ClaudeCode && self.config.credential_store.available() {
            // The home's own item only; never the default one.
            self.config
                .credential_store
                .delete(&claude_keychain_service(&home));
        }
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
            saved: Vec::new(),
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
        // Codex: the app opens the single authorization tab (as the global
        // Add account flow does), so mute the CLI's own browser open.
        #[cfg(unix)]
        if account.harness == HarnessId::Codex
            && let Some(noop) = ensure_noop_browser(&self.config.root)
        {
            command.env("BROWSER", noop);
        }
        let proc = LoginProc::spawn(&command)?;
        let output = proc.output.clone();
        let exit = proc.exit.clone();
        let closed = proc.closed.clone();
        let login_id = new_id();
        lock(&self.logins).insert(
            login_id.clone(),
            Login {
                account_id: account.id,
                harness: account.harness,
                proc,
                started: Instant::now(),
            },
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        let url = loop {
            if let Some(url) = scan_https_url(&lock(&output)) {
                break url;
            }
            let drained = lock(&exit).is_some()
                && closed.load(std::sync::atomic::Ordering::SeqCst);
            if drained || Instant::now() > deadline {
                break String::new();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
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
        let poll = |status, message: Option<String>| AgentLoginPoll {
            status,
            message,
            url: None,
            awaiting_code: false,
            device_code: None,
        };
        let Some(login) = logins.get(login_id) else {
            return poll(
                AgentLoginStatus::Error,
                Some("This sign-in is no longer running.".into()),
            );
        };
        let exit = *lock(&login.proc.exit);
        match exit {
            None if login.started.elapsed() > LOGIN_TIMEOUT => {
                login.proc.kill();
                logins.remove(login_id);
                poll(AgentLoginStatus::Error, Some("Sign-in timed out.".into()))
            }
            None => {
                let output = lock(&login.proc.output).clone();
                let url = scan_https_url(&output);
                let prompt = prompts_for_code(&output)
                    || (login.harness == HarnessId::ClaudeCode && url.is_some());
                AgentLoginPoll {
                    status: AgentLoginStatus::Pending,
                    message: None,
                    device_code: url.as_ref().and_then(|_| scan_device_code(&output)),
                    awaiting_code: prompt,
                    url,
                }
            }
            Some(code) => {
                let account_id = login.account_id.clone();
                logins.remove(login_id);
                drop(logins);
                match code {
                    Some(0) if self.is_signed_in(&account_id) => {
                        poll(AgentLoginStatus::Done, None)
                    }
                    Some(0) => poll(
                        AgentLoginStatus::Error,
                        Some(
                            "The sign-in finished, but no login was saved in this account's \
                             home. Try again."
                                .into(),
                        ),
                    ),
                    Some(code) => poll(
                        AgentLoginStatus::Error,
                        Some(format!("The CLI's sign-in exited with status {code}.")),
                    ),
                    None => poll(
                        AgentLoginStatus::Error,
                        Some("The CLI's sign-in was interrupted.".into()),
                    ),
                }
            }
        }
    }

    fn is_signed_in(&self, account_id: &str) -> bool {
        self.view_by_id(account_id).is_some_and(|a| a.signed_in)
    }

    /// Paste the code the browser showed into the running CLI. The code is
    /// only written to the process; it is not stored, logged or echoed back.
    pub fn submit_login_code(&self, login_id: &str, code: &str) -> Result<(), EngineError> {
        let code = code.trim();
        if code.is_empty() || code.len() > 4096 || code.chars().any(char::is_control) {
            return Err(EngineError::Other("That doesn't look like a code.".into()));
        }
        let logins = lock(&self.logins);
        let login = logins
            .get(login_id)
            .ok_or_else(|| EngineError::Other("This sign-in is no longer running.".into()))?;
        login.proc.send_line(code)
    }

    pub fn cancel_login(&self, login_id: &str) {
        if let Some(login) = lock(&self.logins).remove(login_id) {
            login.proc.kill();
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
            login.proc.kill();
        }
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

/// True when the CLI is asking for a pasted code.
fn prompts_for_code(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    ["paste", "code here", "authorization code"]
        .iter()
        .any(|needle| lower.contains(needle))
}

/// A `XXXX-XXXX` style device code the CLI tells the user to enter in the
/// browser (only when the output mentions a code at all).
fn scan_device_code(output: &str) -> Option<String> {
    if !output.to_ascii_lowercase().contains("code") {
        return None;
    }
    output
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-'))
        .find(|w| {
            let Some((a, b)) = w.split_once('-') else {
                return false;
            };
            let ok = |p: &str, n: std::ops::RangeInclusive<usize>| {
                n.contains(&p.len())
                    && p.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            };
            ok(a, 4..=5) && ok(b, 4..=5)
        })
        .map(str::to_string)
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
