//! Per-account CLI homes. A chat pinned to a Mast-managed account runs its
//! CLI with `CLAUDE_CONFIG_DIR` / `CODEX_HOME` pointing at that account's
//! home, so each account has its own credentials, settings and session
//! history. Chats on the Default account get no override at all: the CLI
//! sees exactly the environment Mast itself has, and the live `~/.claude` /
//! `~/.codex` login is untouched.
//!
//! Claude Code keys its macOS Keychain item to the config dir
//! (`Claude Code-credentials-<first 8 hex of sha256(dir)>`, verified from the
//! 2.1.285 binary), so two homes never share credentials. [`CLAUDE_KEYCHAIN_ISOLATED`]
//! is the switch the engine reads when it prepares a managed Claude home: when
//! false it forces file credentials (`.credentials.json`, mode 0600) instead.
use std::path::Path;
use zeron_proto::HarnessId;

/// Phase 0 finding, see the module docs. Flip to `false` to make the engine
/// force file credentials in managed Claude homes.
pub const CLAUDE_KEYCHAIN_ISOLATED: bool = true;

/// The env var that relocates this harness's home, if it has one.
pub fn home_env(id: HarnessId) -> Option<&'static str> {
    match id {
        HarnessId::ClaudeCode => Some("CLAUDE_CONFIG_DIR"),
        HarnessId::Codex => Some("CODEX_HOME"),
        _ => None,
    }
}

/// Point `cmd` at a managed account home. `None` (Default) leaves the env alone.
pub fn apply(cmd: &mut crate::process::Command, id: HarnessId, home: Option<&Path>) {
    if let (Some(var), Some(home)) = (home_env(id), home) {
        cmd.as_std_mut().env(var, home);
    }
}
