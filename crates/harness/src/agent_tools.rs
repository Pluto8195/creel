//! Mast agent tools injection (Work Outline Milestone 4).
//!
//! The engine hands a Run an [`AgentToolsInjection`] only for Chats in a
//! Space whose harness reports [`crate::Harness::supports_agent_tools`]: the
//! `zeron mcp` stdio server, with the engine-minted per-Run capability token
//! in its environment. Each adapter adds it through the CLI's own supported
//! mechanism, alongside (never instead of) the user's MCP configuration:
//!
//! - Claude Code: `--mcp-config <file>` WITHOUT `--strict-mcp-config`, so the
//!   user's own servers still load. The file is written in a fresh per-Run
//!   `0700` directory under a verified owner-only Mast directory, and both
//!   are removed when the Run ends; the token lives in the file's `env`
//!   block, never in argv (visible to `ps`).
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

/// A Claude `--mcp-config` file in its own per-Run owner-only directory;
/// both are deleted when dropped (the adapter holds it for the Run's
/// lifetime).
#[derive(Debug)]
pub struct McpConfigFile {
    path: PathBuf,
    run_dir: PathBuf,
}

impl McpConfigFile {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for McpConfigFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(&self.run_dir);
    }
}

fn private_dir_builder(recursive: bool) -> std::fs::DirBuilder {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(recursive);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
}

/// Create `dir` (with parents) owner-only and verify it: a real directory
/// (not a symlink) owned by this user. One left group/other-accessible by
/// this user is tightened to `0700`; anything owned by someone else is
/// refused (a pre-created directory in a shared temp dir must never receive
/// a token).
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    private_dir_builder(true).create(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let refuse = |why: &str| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("refusing agent-tools directory {}: {why}", dir.display()),
            ))
        };
        let meta = std::fs::symlink_metadata(dir)?;
        if !meta.file_type().is_dir() {
            return refuse("not a directory");
        }
        // SAFETY: geteuid has no preconditions and cannot fail.
        if meta.uid() != unsafe { libc::geteuid() } {
            return refuse("owned by another user");
        }
        if meta.mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

/// Write `{"mcpServers": {<name>: <server>}}` to `mcp.json` in a fresh
/// owner-only per-Run directory under `dir` (itself created and verified
/// owner-only). The directory name is random, so concurrent Runs never share
/// a file and no Run can list another Run's files.
pub fn write_claude_mcp_config(
    dir: &Path,
    injection: &AgentToolsInjection,
) -> std::io::Result<McpConfigFile> {
    ensure_private_dir(dir)?;
    let run_dir = dir.join(format!("run-{}", uuid::Uuid::new_v4().simple()));
    // Not recursive: the per-Run directory must be new.
    private_dir_builder(false).create(&run_dir)?;
    let body = json!({
        "mcpServers": { injection.server_name.clone(): injection.claude_server_config() }
    });
    let guard = McpConfigFile {
        path: run_dir.join("mcp.json"),
        run_dir,
    };
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    {
        use std::io::Write;
        let mut file = options.open(&guard.path)?;
        file.write_all(body.to_string().as_bytes())?;
        file.sync_all()?;
    }
    Ok(guard)
}

/// Default Mast-owned directory for Claude MCP config files: beside the
/// profile's model-discovery directory (`<store_root>/agent-tools`), else a
/// per-process directory in the temp dir (created and verified owner-only by
/// [`write_claude_mcp_config`]). Never the user's checkout or `~/.claude`.
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
            server_name: "mast-agent".into(),
            command: PathBuf::from("/opt/zeron/bin/zeron"),
            args: vec!["mcp".into()],
            env: BTreeMap::from([
                ("MAST_AGENT_TOKEN".into(), "secret-token".into()),
                ("MAST_IPC_PORT".into(), "27654".into()),
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
        for private in [
            dir.path().join("agent-tools"),
            file.path().parent().unwrap().into(),
        ] {
            let dir_mode = std::fs::metadata(&private).unwrap().permissions().mode();
            assert_eq!(dir_mode & 0o777, 0o700, "{private:?}");
        }
        // Each Run gets its own directory: the shared one lists no files.
        let run_dir = file.path().parent().unwrap().to_owned();
        assert_eq!(run_dir.parent().unwrap(), dir.path().join("agent-tools"));
        let second =
            write_claude_mcp_config(&dir.path().join("agent-tools"), &injection()).unwrap();
        assert_ne!(second.path().parent(), file.path().parent());
        assert!(
            std::fs::read_dir(dir.path().join("agent-tools"))
                .unwrap()
                .all(|e| e.unwrap().file_type().unwrap().is_dir())
        );
        drop(second);
        let body: Value =
            serde_json::from_str(&std::fs::read_to_string(file.path()).unwrap()).unwrap();
        let server = &body["mcpServers"]["mast-agent"];
        assert_eq!(server["type"], "stdio");
        assert_eq!(server["command"], "/opt/zeron/bin/zeron");
        assert_eq!(server["args"], json!(["mcp"]));
        assert_eq!(server["env"]["MAST_AGENT_TOKEN"], "secret-token");
        let path = file.path().to_owned();
        drop(file);
        assert!(!path.exists());
        assert!(!run_dir.exists(), "the per-Run directory goes with it");
    }

    #[cfg(unix)]
    #[test]
    fn private_dirs_are_tightened_or_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        // Ours but too open: tightened to 0700.
        let open = dir.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
        ensure_private_dir(&open).unwrap();
        let mode = std::fs::metadata(&open).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        // A symlink (e.g. planted in a shared temp dir) is refused.
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&open, &link).unwrap();
        let err = write_claude_mcp_config(&link, &injection()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
        // A regular file in the way is refused too.
        let file = dir.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        assert!(ensure_private_dir(&file).is_err());
        // Someone else's directory (root-owned, when not running as root).
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            let err = ensure_private_dir(Path::new("/")).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
        }
    }

    #[test]
    fn fallback_dir_is_per_process_in_the_temp_dir() {
        let fallback = default_dir(None);
        assert!(fallback.starts_with(std::env::temp_dir()));
        assert!(
            fallback
                .to_string_lossy()
                .ends_with(&format!("zeron-agent-tools-{}", std::process::id()))
        );
        let store = Path::new("/profile/store/models");
        assert_eq!(
            default_dir(Some(store)),
            Path::new("/profile/store/agent-tools")
        );
    }

    #[test]
    fn codex_overrides_name_one_server() {
        let overrides = injection().codex_config_overrides();
        assert_eq!(
            overrides["mcp_servers.mast-agent.command"],
            "/opt/zeron/bin/zeron"
        );
        assert_eq!(overrides["mcp_servers.mast-agent.args"], json!(["mcp"]));
        assert_eq!(
            overrides["mcp_servers.mast-agent.env"]["MAST_AGENT_TOKEN"],
            "secret-token"
        );
        assert_eq!(overrides["mcp_servers.mast-agent.enabled"], true);
        assert!(
            overrides
                .keys()
                .all(|k| k.starts_with("mcp_servers.mast-agent."))
        );
    }
}
