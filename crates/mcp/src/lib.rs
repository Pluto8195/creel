//! zeron-mcp — a Model Context Protocol server over the running engine.
//!
//! `zeron mcp` speaks MCP (JSON-RPC 2.0, newline-delimited) on stdin/stdout
//! and proxies every tool into the engine's localhost IPC — the same
//! `zeron_rpc` WebSocket the headed app and `zeron sync` dial. Nothing here
//! talks to the edge or touches the filesystem: the engine stays the single
//! authority for chats, devices, projects, and the command plane.
//!
//! The server is hand-rolled rather than an SDK: the MCP surface a stdio
//! tool server needs is five methods (`initialize`, `ping`, `tools/list`,
//! `tools/call`, and the `initialized` notification), and the repo already
//! owns JSON-RPC framing for the Codex and ACP drivers. Zero new crates.
//!
//! When the engine launches a Run of a Chat in a Space on Claude Code or
//! Codex it injects this server with an engine-minted per-Run capability
//! token in `ZERON_AGENT_TOKEN`. Every tool call re-authenticates the token
//! with the engine (`AgentAuthenticate`), which answers with the Chat the
//! token was minted for; nothing self-declared is trusted. Such an agent also
//! gets the Work Outline agent tools (`AgentToolCall`), its sends are
//! attributed to its Chat, a Chat can never message itself, and `create_chat`
//! requires an idempotency key. Without a token (a human running `zeron mcp`
//! in a terminal) the original unattributed tools work as before.

mod jsonrpc;
mod tools;
mod transcript;
mod zeron;

pub use jsonrpc::serve_stdio;
pub use tools::{ToolDef, Tools};
pub use transcript::{RenderOptions, RenderedMessage, render_entries};
pub use zeron::{Zeron, token_from_env};

/// How `zeron mcp` finds the engine and which Run's token it presents.
#[derive(Clone)]
pub struct McpConfig {
    /// Loopback IPC port of the engine to proxy (`ZERON_IPC_PORT`, default 27654).
    pub ipc_port: u16,
    /// The engine-minted per-Run token (`ZERON_AGENT_TOKEN`), when injected.
    pub token: Option<String>,
}

impl std::fmt::Debug for McpConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpConfig")
            .field("ipc_port", &self.ipc_port)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl McpConfig {
    /// Resolve from the process environment: `ZERON_IPC_PORT` for the engine,
    /// `ZERON_AGENT_TOKEN` for the Run. `ZERON_CHAT_ID` is ignored.
    pub fn from_env() -> Self {
        let ipc_port = std::env::var(zeron_proto::agent_tools::IPC_PORT_ENV)
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(27654);
        Self {
            ipc_port,
            token: token_from_env(),
        }
    }
}

/// Run the MCP server on this process's stdin/stdout until stdin closes.
pub async fn run(config: McpConfig) -> anyhow::Result<()> {
    let zeron = Zeron::new(format!("ws://127.0.0.1:{}", config.ipc_port), config.token);
    let tools = Tools::new(std::sync::Arc::new(zeron));
    serve_stdio(std::sync::Arc::new(tools)).await
}
