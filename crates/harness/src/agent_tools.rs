//! Zeron agent tools injection (Work Outline Milestone 4).
//!
//! The engine hands a Run an [`AgentToolsInjection`] only for Chats in a
//! Space whose harness reports [`crate::Harness::supports_agent_tools`]: the
//! `zeron mcp` stdio server, with the engine-minted per-Run capability token
//! in its environment. Each adapter adds it through the CLI's own supported
//! mechanism, alongside (never instead of) the user's MCP configuration:
//!
//! - Claude Code: `--mcp-config <file>` WITHOUT `--strict-mcp-config`, so the
//!   user's own servers still load. The file is written under a Zeron-owned
//!   directory, owner-only, and removed when the Run ends; the token lives in
//!   the file's `env` block, never in argv (visible to `ps`).
//! - Codex: dotted `mcp_servers.<name>.*` overrides in the `thread/start` /
//!   `thread/resume` `config` map (the app-server's per-thread config
//!   layer), so `~/.codex/config.toml` is untouched.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// One MCP stdio server the engine wants a Run to have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentToolsInjection {
    /// MCP server name (`zeron`).
    pub server_name: String,
    /// Executable to spawn (the running `zeron` binary).
    pub command: PathBuf,
    /// Arguments (`["mcp"]`).
    pub args: Vec<String>,
    /// Environment for the server: the capability token and the IPC port.
    /// Secret: never logged, never placed in argv.
    pub env: BTreeMap<String, String>,
}

impl AgentToolsInjection {
    /// The server entry in Claude Code's `mcpServers` shape.
    pub fn claude_server_config(&self) -> Value {
        json!({
            "type": "stdio",
            "command": self.command.to_string_lossy(),
            "args": self.args,
            "env": self.env,
        })
    }

    /// The `config` overrides that add this server to one Codex thread.
    pub fn codex_config_overrides(&self) -> serde_json::Map<String, Value> {
        let prefix = format!("mcp_servers.{}", self.server_name);
        let mut overrides = serde_json::Map::new();
        overrides.insert(
            format!("{prefix}.command"),
            Value::String(self.command.to_string_lossy().into_owned()),
        );
        overrides.insert(format!("{prefix}.args"), json!(self.args));
        overrides.insert(format!("{prefix}.env"), json!(self.env));
        overrides.insert(format!("{prefix}.enabled"), Value::Bool(true));
        overrides
    }
}

/// A Claude `--mcp-config` file that is deleted when dropped (the adapter
/// holds it for the Run's lifetime).
#[derive(Debug)]
pub struct McpConfigFile {
    path: PathBuf,
}

impl McpConfigFile {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for McpConfigFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Write `{"mcpServers": {<name>: <server>}}` to a fresh owner-only file in
/// `dir` (created owner-only if missing). The name is random, so concurrent
/// Runs never share or clobber a file.
pub fn write_claude_mcp_config(
    dir: &Path,
    injection: &AgentToolsInjection,
) -> std::io::Result<McpConfigFile> {
    {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(dir)?;
    }
    let body = json!({
        "mcpServers": { injection.server_name.clone(): injection.claude_server_config() }
    });
    let path = dir.join(format!("mcp-{}.json", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let guard = McpConfigFile { path };
    {
        use std::io::Write;
        let mut file = options.open(&guard.path)?;
        file.write_all(body.to_string().as_bytes())?;
        file.sync_all()?;
    }
    Ok(guard)
}

/// Default Zeron-owned directory for Claude MCP config files: beside the
/// profile's model-discovery directory (`<store_root>/agent-tools`), else a
/// private temp directory. Never the user's checkout or `~/.claude`.
pub fn default_dir(model_discovery_cwd: Option<&Path>) -> PathBuf {
    match model_discovery_cwd.and_then(Path::parent) {
        Some(store_root) => store_root.join("agent-tools"),
        None => std::env::temp_dir().join(format!("zeron-agent-tools-{}", std::process::id())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn injection() -> AgentToolsInjection {
        AgentToolsInjection {
            server_name: "zeron-agent".into(),
            command: PathBuf::from("/opt/zeron/bin/zeron"),
            args: vec!["mcp".into()],
            env: BTreeMap::from([
                ("ZERON_AGENT_TOKEN".into(), "secret-token".into()),
                ("ZERON_IPC_PORT".into(), "27654".into()),
            ]),
        }
    }

    #[cfg(unix)]
    #[test]
    fn claude_config_file_is_private_complete_and_removed_on_drop() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = write_claude_mcp_config(&dir.path().join("agent-tools"), &injection()).unwrap();
        let mode = std::fs::metadata(file.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let dir_mode = std::fs::metadata(dir.path().join("agent-tools"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, 0o700);
        let body: Value =
            serde_json::from_str(&std::fs::read_to_string(file.path()).unwrap()).unwrap();
        let server = &body["mcpServers"]["zeron-agent"];
        assert_eq!(server["type"], "stdio");
        assert_eq!(server["command"], "/opt/zeron/bin/zeron");
        assert_eq!(server["args"], json!(["mcp"]));
        assert_eq!(server["env"]["ZERON_AGENT_TOKEN"], "secret-token");
        let path = file.path().to_owned();
        drop(file);
        assert!(!path.exists());
    }

    #[test]
    fn codex_overrides_name_one_server() {
        let overrides = injection().codex_config_overrides();
        assert_eq!(
            overrides["mcp_servers.zeron-agent.command"],
            "/opt/zeron/bin/zeron"
        );
        assert_eq!(overrides["mcp_servers.zeron-agent.args"], json!(["mcp"]));
        assert_eq!(
            overrides["mcp_servers.zeron-agent.env"]["ZERON_AGENT_TOKEN"],
            "secret-token"
        );
        assert_eq!(overrides["mcp_servers.zeron-agent.enabled"], true);
        assert!(
            overrides
                .keys()
                .all(|k| k.starts_with("mcp_servers.zeron-agent."))
        );
    }
}
