//! Where this device's Linear API key lives: the macOS Keychain (via
//! `security`, the same approach as `agent_accounts`), then the
//! `LINEAR_API_KEY` environment variable. Tests (and `ZERON_LINEAR_KEY_STORE=
//! memory`) inject an in-memory store so nothing touches the real Keychain.
//!
//! The key is never logged: errors carry `security`'s stderr only, and the
//! value is passed to `security` as an argument (as `agent_accounts` does).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Keychain service and account of the stored key.
pub const KEYCHAIN_SERVICE: &str = "sh.zeron.linear";
pub const KEYCHAIN_ACCOUNT: &str = "api-key";
/// Environment fallback (and the only source when no store holds a key).
pub const ENV_KEY: &str = "LINEAR_API_KEY";
/// `memory` forces the in-memory store (dev/test runs that must never
/// touch the Keychain); anything else is the platform default.
pub const STORE_OVERRIDE_ENV: &str = "ZERON_LINEAR_KEY_STORE";

/// A persistent home for the key.
pub trait LinearKeyStore: Send + Sync {
    /// `Ok(None)` when nothing is stored.
    fn read(&self) -> Result<Option<String>, String>;
    fn write(&self, key: &str) -> Result<(), String>;
    /// Deleting an absent key succeeds.
    fn delete(&self) -> Result<(), String>;
    /// Whether this store is the platform Keychain (reported as `keychain`).
    fn is_keychain(&self) -> bool {
        true
    }
}

/// An in-memory store (tests, `ZERON_LINEAR_KEY_STORE=memory`).
#[derive(Default)]
pub struct MemoryKeyStore {
    key: Mutex<Option<String>>,
    /// Calls by kind, for tests: `(reads, writes, deletes)`.
    calls: Mutex<(u32, u32, u32)>,
}

impl MemoryKeyStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn with_key(key: &str) -> Arc<Self> {
        let store = Self::default();
        *store.key.lock().unwrap() = Some(key.to_owned());
        Arc::new(store)
    }

    /// The stored key (tests).
    pub fn stored(&self) -> Option<String> {
        self.key.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// `(reads, writes, deletes)` so far.
    pub fn calls(&self) -> (u32, u32, u32) {
        *self.calls.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl LinearKeyStore for MemoryKeyStore {
    fn read(&self) -> Result<Option<String>, String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).0 += 1;
        Ok(self.stored())
    }

    fn write(&self, key: &str) -> Result<(), String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).1 += 1;
        *self.key.lock().unwrap_or_else(|e| e.into_inner()) = Some(key.to_owned());
        Ok(())
    }

    fn delete(&self) -> Result<(), String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).2 += 1;
        *self.key.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(())
    }
}

/// The platform store: the macOS Keychain; elsewhere nothing persists (the
/// environment variable is the only source).
pub struct KeychainKeyStore;

#[cfg(target_os = "macos")]
impl KeychainKeyStore {
    /// `security` blocks indefinitely on an unanswered consent dialog.
    const TIMEOUT: Duration = Duration::from_secs(15);

    fn run(args: &[&str]) -> Result<(bool, String, String), String> {
        use std::process::{Command, Stdio};
        let mut child = Command::new("security")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not run security: {e}"))?;
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed() > Self::TIMEOUT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("Keychain did not answer (is a consent dialog open?)".into());
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(e) => return Err(format!("security failed: {e}")),
            }
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("security failed: {e}"))?;
        Ok((
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ))
    }
}

#[cfg(target_os = "macos")]
impl LinearKeyStore for KeychainKeyStore {
    fn read(&self) -> Result<Option<String>, String> {
        let (ok, stdout, stderr) = Self::run(&[
            "find-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            KEYCHAIN_ACCOUNT,
            "-w",
        ])?;
        if ok {
            let key = stdout.trim().to_owned();
            return Ok((!key.is_empty()).then_some(key));
        }
        // errSecItemNotFound: "The specified item could not be found …".
        if stderr.contains("could not be found") {
            return Ok(None);
        }
        Err(format!("Keychain read failed: {stderr}"))
    }

    fn write(&self, key: &str) -> Result<(), String> {
        let (ok, _, stderr) = Self::run(&[
            "add-generic-password",
            "-U",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            KEYCHAIN_ACCOUNT,
            "-l",
            "Zeron Linear API key",
            "-w",
            key,
        ])?;
        if ok {
            Ok(())
        } else {
            Err(format!("Keychain write failed: {stderr}"))
        }
    }

    fn delete(&self) -> Result<(), String> {
        let (ok, _, stderr) = Self::run(&[
            "delete-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            KEYCHAIN_ACCOUNT,
        ])?;
        if ok || stderr.contains("could not be found") {
            Ok(())
        } else {
            Err(format!("Keychain delete failed: {stderr}"))
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl LinearKeyStore for KeychainKeyStore {
    fn read(&self) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn write(&self, _key: &str) -> Result<(), String> {
        Err("Storing a Linear API key needs macOS; set LINEAR_API_KEY instead".into())
    }

    fn delete(&self) -> Result<(), String> {
        Ok(())
    }
}

/// The store a new engine uses: in-memory when
/// `ZERON_LINEAR_KEY_STORE=memory`, else the Keychain.
pub fn default_store() -> Arc<dyn LinearKeyStore> {
    match std::env::var(STORE_OVERRIDE_ENV).as_deref() {
        Ok("memory") => MemoryKeyStore::new(),
        _ => Arc::new(KeychainKeyStore),
    }
}

/// Reads the environment fallback (injectable so tests ignore the
/// developer's own `LINEAR_API_KEY`).
pub type EnvKeySource = Arc<dyn Fn() -> Option<String> + Send + Sync>;

pub fn process_env_source() -> EnvKeySource {
    Arc::new(|| {
        std::env::var(ENV_KEY)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    })
}

pub fn no_env_source() -> EnvKeySource {
    Arc::new(|| None)
}
