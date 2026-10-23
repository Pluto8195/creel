//! MCP stdio transport: newline-delimited JSON-RPC 2.0 on stdin/stdout.
//!
//! Requests are handled concurrently (a `wait_for_turn` may block for
//! minutes while the client keeps pinging); responses are serialized through
//! one writer task so frames never interleave. Notifications from the client
//! (`notifications/initialized`, `notifications/cancelled`) are accepted and
//! ignored — there is no server-side state to initialize or cancel.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::tools::Tools;

/// Protocol revisions this server knows. The client's requested revision
/// is echoed when it is one of these; otherwise the newest is offered and
/// the client decides whether to continue (per the MCP handshake rules).
const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const LATEST_PROTOCOL: &str = "2025-11-25";

/// Claude Code truncates MCP server instructions at about 2000 characters, so
/// this text must stay under 1900 characters (checked by a test) with the
/// guidance every agent needs first. Detail that does not fit belongs in the
/// tool descriptions (`tools.rs`).
const INSTRUCTIONS: &str = "\
Creel runs coding agents in chats, each in a project (a folder on a device). \
These tools operate the local Creel engine.\n\
\n\
Worktrees: right after `git worktree add`, or when resuming a worktree for \
this chat's work, call `bind_chat_worktree` with its absolute top-level \
path.\n\
\n\
Blocked decisions: if your chat has a work item \
(`work_outline_read_current`) and is blocked on a decision, approval, or \
missing information from the user, call \
`work_outline_create_user_action_request` (one-line question, context in \
`detail`, a `dedupeKey` per topic) as well as asking in chat; resolve it \
with `work_outline_resolve_user_action_request` once answered. Use the \
built-in question prompt only for small clarifications that don't block the \
item.\n\
\n\
Planning artifacts (plans, diagrams, option comparisons): publish every file \
the user should see with `artifact_publish` and link the returned \
`zeron-artifact:<id>` URL, never a bare path. Under a work item, call \
`artifact_get_plan` at chat start. After changing a visual artifact run \
`artifact_render_check`, fix what it reports, and look at the screenshot \
before calling it done.\n\
\n\
Chats: reference by id, unique id prefix, or exact title; `whoami` gives \
yours. `send_message` with `wait: true` (or `wait_for_turn` later) returns \
the reply once that turn finishes; answer an `awaitingInput` chat with \
`respond_to_input`.\n\
\n\
Work Outline tools (`work_outline_*`) manage your item, its subtree, and \
child items. Each mutating call (and `create_chat`) needs an idempotency \
key: reuse it when retrying the same change, use a new one for a new change.\n\
\n\
Coordinating: `create_chat` with `workItemId` (your item or a descendant) \
and a `prompt` runs work on a descendant. A child cannot message its parent: \
collect results with `wait_for_turn` / `read_chat` or its Required Actions, \
and poll `work_outline_read_subtree` for requests such as \"Ready to \
merge\".";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Serve MCP on this process's stdin/stdout until stdin closes.
pub async fn serve_stdio(tools: Arc<Tools>) -> anyhow::Result<()> {
    let (out_tx, mut out_rx) = mpsc::channel::<String>(64);
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = out_rx.recv().await {
            if stdout.write_all(line.as_bytes()).await.is_err()
                || stdout.write_all(b"\n").await.is_err()
                || stdout.flush().await.is_err()
            {
                break;
            }
        }
    });

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(err) => {
                let _ = out_tx
                    .send(
                        error_response(Value::Null, PARSE_ERROR, &format!("parse error: {err}"))
                            .to_string(),
                    )
                    .await;
                continue;
            }
        };
        let Some(response) = route(&tools, message, &out_tx) else {
            continue;
        };
        let _ = out_tx.send(response.to_string()).await;
    }
    drop(out_tx);
    let _ = writer.await;
    Ok(())
}

/// Classify one inbound message. Requests are answered asynchronously via
/// `out`; an immediate `Some` is a synchronous error reply.
fn route(tools: &Arc<Tools>, message: Value, out: &mpsc::Sender<String>) -> Option<Value> {
    let Some(object) = message.as_object() else {
        return Some(error_response(
            Value::Null,
            INVALID_REQUEST,
            "expected a JSON-RPC object",
        ));
    };
    let id = object.get("id").cloned();
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let params = object.get("params").cloned().unwrap_or(Value::Null);
    match (id, method) {
        (Some(id), Some(method)) => {
            let tools = tools.clone();
            let out = out.clone();
            tokio::spawn(async move {
                let response = handle_request(&tools, id, &method, params).await;
                let _ = out.send(response.to_string()).await;
            });
            None
        }
        (None, Some(method)) => {
            tracing::debug!(method, "mcp: notification");
            None
        }
        // A response to a server-initiated request: we never send any.
        (Some(_), None) => None,
        (None, None) => Some(error_response(
            Value::Null,
            INVALID_REQUEST,
            "missing method",
        )),
    }
}

/// Answer one request. Public for tests and for embedding the dispatcher
/// behind another transport.
pub async fn handle_request(tools: &Tools, id: Value, method: &str, params: Value) -> Value {
    match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(LATEST_PROTOCOL);
            let version = if PROTOCOL_VERSIONS.contains(&requested) {
                requested
            } else {
                LATEST_PROTOCOL
            };
            ok_response(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": {
                        "name": "zeron",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "instructions": INSTRUCTIONS,
                }),
            )
        }
        "ping" => ok_response(id, json!({})),
        "tools/list" => ok_response(id, json!({ "tools": tools.list() })),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return error_response(id, INVALID_PARAMS, "tools/call needs a tool name");
            };
            if !tools.has(name) {
                return error_response(id, INVALID_PARAMS, &format!("unknown tool: {name}"));
            }
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match tools.call_with_content(name, arguments).await {
                Ok((value, extra)) => {
                    let mut content = vec![json!({ "type": "text", "text": pretty(&value) })];
                    content.extend(extra);
                    ok_response(
                        id,
                        json!({
                            "content": content,
                            "structuredContent": wrap_structured(value),
                            "isError": false,
                        }),
                    )
                }
                Err(message) => ok_response(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": message }],
                        "isError": true,
                    }),
                ),
            }
        }
        // Optional capabilities we do not advertise; answer empty rather
        // than erroring so permissive clients do not log noise.
        "resources/list" => ok_response(id, json!({ "resources": [] })),
        "prompts/list" => ok_response(id, json!({ "prompts": [] })),
        other => error_response(id, METHOD_NOT_FOUND, &format!("unknown method: {other}")),
    }
}

/// `structuredContent` must be an object; arrays and scalars are wrapped.
fn wrap_structured(value: Value) -> Value {
    if value.is_object() {
        value
    } else {
        json!({ "result": value })
    }
}

fn pretty(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

fn ok_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

#[cfg(test)]
mod instructions_tests {
    use super::INSTRUCTIONS;

    /// Claude Code cuts MCP instructions at about 2000 characters.
    const BUDGET: usize = 1900;

    #[test]
    fn instructions_fit_the_truncation_budget() {
        let chars = INSTRUCTIONS.chars().count();
        assert!(
            chars <= BUDGET,
            "INSTRUCTIONS is {chars} characters; budget {BUDGET}"
        );
    }

    #[test]
    fn instructions_keep_every_guidance_area() {
        for phrase in [
            "bind_chat_worktree",
            "git worktree add",
            "work_outline_create_user_action_request",
            "work_outline_resolve_user_action_request",
            "create_chat",
            "send_message",
            "wait_for_turn",
            "work_outline_read_subtree",
            "artifact_publish",
            "artifact_get_plan",
            "artifact_render_check",
            "idempotency",
        ] {
            assert!(INSTRUCTIONS.contains(phrase), "missing `{phrase}`");
        }
    }

    #[test]
    fn worktree_guidance_survives_truncation() {
        let head: String = INSTRUCTIONS.chars().take(BUDGET).collect();
        let sentence = head
            .find("`bind_chat_worktree`")
            .expect("bind_chat_worktree in the first 1900 characters");
        assert!(head.find("git worktree add").unwrap() < sentence);
        // Blocked decisions and planning artifacts also come before the cut.
        for phrase in [
            "work_outline_create_user_action_request",
            "artifact_publish",
        ] {
            assert!(
                head.contains(phrase),
                "`{phrase}` must be inside the first {BUDGET} characters"
            );
        }
    }
}
