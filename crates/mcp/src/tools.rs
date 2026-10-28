//! The tool catalog and its dispatch.
//!
//! Every tool is a thin composition of engine reads/writes from
//! [`Zeron`]; the only logic that lives here is argument resolution (chat
//! by prefix, project by path), sender attribution, and the "how do I
//! deliver a message to a chat in this state" choice the composer makes
//! for humans.
//!
//! Agent mode (an engine-injected per-Run token): every call first
//! re-authenticates the token with the engine, and the Work Outline agent
//! tools are listed and forwarded to `AgentToolCall`. Agent mode is scoped to
//! the token's Chat, Space, and device:
//! - `create_chat` takes a mandatory idempotency key (the Chat, message, and
//!   command ids derive from the token's Chat and that key, so a retry never
//!   creates a second Chat or Turn; a retry with other inputs is
//!   `operationConflict`), creates only children of the caller, in the
//!   token's Space on the token's device, with a cwd inside the Space or the
//!   caller's cwd, and a sandbox / approval mode never more permissive than
//!   the caller's;
//! - `send_message`, `interrupt_chat`, `respond_to_input`, `wait_for_turn`,
//!   and `archive_chat` reach only Chats the caller created; `get_chat` and
//!   `read_chat` also the caller itself; `list_chats` lists only those;
//! - `list_projects` / `list_devices` list only the token's Space / device;
//!   `list_harnesses` / `list_models` (this device's capabilities) and
//!   `whoami` are unscoped.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeron_doc::SessionCommandPayload;
use zeron_proto::agent_tools::{self, AgentOrigin, tools as agent_tool_names};
use zeron_proto::{
    Chat, ChatConfig, ChatModelDisplay, HarnessId, ReasoningLevel, RunRequest, SandboxLevel,
    Session, SessionStatus, Space, UserInputAnswer,
};

use crate::transcript::{RenderOptions, RenderedMessage, render_entries};
use crate::zeron::{HarnessInfo, TurnOutcome, Zeron, resolve_chat_in, session_for, short};

/// Default and ceiling for the blocking waits.
const DEFAULT_WAIT: Duration = Duration::from_secs(600);
const MAX_WAIT: Duration = Duration::from_secs(3600);
/// A session row older than this is not trusted to still be working
/// (the UI's staleness window): a crashed host must not read as busy forever.
const SESSION_STALE: chrono::Duration = chrono::Duration::seconds(45);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

pub struct Tools {
    zeron: Arc<Zeron>,
}

fn chat_key_schema(extra: Value) -> Value {
    let mut properties = json!({
        "chat": {
            "type": "string",
            "description": "Chat id, unique id prefix, or exact title."
        }
    });
    if let (Some(base), Some(more)) = (properties.as_object_mut(), extra.as_object()) {
        for (k, v) in more {
            base.insert(k.clone(), v.clone());
        }
    }
    json!({ "type": "object", "properties": properties, "required": ["chat"] })
}

fn catalog() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "whoami",
            description: "Which chat and device this server speaks for, plus the engine's workspace mode. Call this first when you need to know your own chat id.",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolDef {
            name: "list_devices",
            description: "Devices in this workspace (the local engine's device is flagged). Chats and projects are hosted on a device.",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolDef {
            name: "list_projects",
            description: "Projects: a folder on a device. Each chat belongs to one project, which fixes its host device and working directory.",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolDef {
            name: "list_harnesses",
            description: "Agent harnesses (claude-code, codex, cursor, …) and whether each is available on this device.",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolDef {
            name: "list_models",
            description: "Models a harness offers on this device. Model ids are harness-specific strings; pass one to create_chat.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "harness": { "type": "string", "description": "Harness id, e.g. claude-code or codex." }
                },
                "required": ["harness"]
            }),
        },
        ToolDef {
            name: "list_chats",
            description: "Chats with their project, host device, harness, model (the model that actually ran when the harness reported it: see modelIsActual, actualModel, configuredModel), live status, and last activity. Newest activity first.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Only chats in this project (id, path, or name)." },
                    "device": { "type": "string", "description": "Only chats hosted on this device (id or name)." },
                    "include_archived": { "type": "boolean", "default": false },
                    "parent": { "type": "string", "description": "Only chats created by this chat (id, prefix, or title) — e.g. your own id to list the chats you spawned." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 50 }
                }
            }),
        },
        ToolDef {
            name: "get_chat",
            description: "One chat's metadata, live status, and any question its agent is currently blocked on.",
            input_schema: chat_key_schema(json!({})),
        },
        ToolDef {
            name: "create_chat",
            description: "Create a chat in a project (or project-less on a device) with a harness and model. The new chat records your chat as its parent (parentChatId); a child in your project inherits your work item and instructions. To run work on a descendant of your work item, pass `workItemId`: the child is started for that item (the same way Start Chat on an item is), attached to it with that item's effective instructions and execution setup, and a `prompt` is required (`cwd` and `branch` come from the item, so they are refused). `workItemId` must be your own item or one of its descendants (see `work_outline_read_subtree`; create descendants with `work_outline_create_child`). A child cannot message its parent: collect its result with `wait_for_turn` / `read_chat`, or from the Required Actions it raises (poll `work_outline_read_subtree` for open requests such as \"Ready to merge\"). Optionally send a first prompt and wait for the reply. Returns the new chat id. When you run inside a Mast chat, idempotency_key (idempotencyKey also works) is required: reuse it when retrying the same request and you get the same chat back (never a duplicate; the same key with different inputs is an error), and the chat is created in your own project on your device, with a sandbox no wider than yours.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "idempotency_key": { "type": "string", "description": "Stable key for this request (required inside a Mast chat). The same key returns the same chat; a new chat needs a new key." },
                    "workItemId": { "type": "string", "description": "Start the child for this work item: your chat's item or one of its descendants. Requires prompt; incompatible with cwd and branch." },
                    "project": { "type": "string", "description": "Project id, path, or name. Required unless device is given." },
                    "device": { "type": "string", "description": "Host device (id or name) for a project-less chat; defaults to this device." },
                    "parent": { "type": "string", "description": "Parent chat to record (id, prefix, or title). Defaults to the chat you are speaking from." },
                    "harness": { "type": "string", "description": "Harness id (see list_harnesses). Defaults to claude-code when available." },
                    "model": { "type": "string", "description": "Model id from list_models. Omit for Mast's default for the harness (Claude Code: claude-opus-5-5)." },
                    "reasoning": { "type": "string", "description": "Reasoning level the model supports (e.g. low, medium, high, max)." },
                    "sandbox": { "type": "string", "enum": ["read-only", "workspace-write", "danger-full-access"], "default": "workspace-write" },
                    "title": { "type": "string", "description": "Sidebar title. Otherwise the engine titles it from the first exchange." },
                    "branch": { "type": "string", "description": "Branch label to record on the chat." },
                    "cwd": { "type": "string", "description": "Working directory override (an existing worktree path). Defaults to the project folder." },
                    "prompt": { "type": "string", "description": "First message to send right away." },
                    "wait": { "type": "boolean", "default": false, "description": "With prompt: block until the first turn finishes and return the reply." },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 3600, "default": 600 }
                }
            }),
        },
        ToolDef {
            name: "read_chat",
            description: "Read a chat's transcript as plain messages (newest window by default). Tool calls are summarized one per line.",
            input_schema: chat_key_schema(json!({
                "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 40, "description": "How many messages, counted from the newest." },
                "offset": { "type": "integer", "minimum": 0, "default": 0, "description": "Skip this many newest messages (paging backwards)." },
                "include_reasoning": { "type": "boolean", "default": false },
                "include_tools": { "type": "boolean", "default": true }
            })),
        },
        ToolDef {
            name: "send_message",
            description: "Send a message to a chat. Messages are attributed to your chat, and a chat cannot message itself. mode 'auto' starts a turn when the chat is idle, steers a running turn when the harness supports it, and otherwise queues for the end of the turn. With wait=true, blocks until the turn finishes and returns the assistant's reply (the request/response pattern); if you did not wait, collect it later with `wait_for_turn`.",
            input_schema: chat_key_schema(json!({
                "text": { "type": "string" },
                "mode": { "type": "string", "enum": ["auto", "run", "steer", "queue"], "default": "auto" },
                "wait": { "type": "boolean", "default": false },
                "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 3600, "default": 600 }
            })),
        },
        ToolDef {
            name: "wait_for_turn",
            description: "Block until a chat is no longer working: returns completed, awaitingInput (answer with respond_to_input), errored, or timedOut, with the newest assistant message.",
            input_schema: chat_key_schema(json!({
                "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 3600, "default": 600 }
            })),
        },
        ToolDef {
            name: "interrupt_chat",
            description: "Stop the chat's running turn.",
            input_schema: chat_key_schema(json!({})),
        },
        ToolDef {
            name: "respond_to_input",
            description: "Answer a question the chat's agent is blocked on (see get_chat / wait_for_turn pendingInput). Labels are the option strings, or free text for open questions.",
            input_schema: chat_key_schema(json!({
                "request_id": { "type": "string", "description": "The pending request id. Defaults to the chat's current pending question." },
                "answers": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "question_id": { "type": "string" },
                            "labels": { "type": "array", "items": { "type": "string" } }
                        },
                        "required": ["question_id", "labels"]
                    }
                }
            })),
        },
        ToolDef {
            name: "archive_chat",
            description: "Archive a chat (hide it from the active sidebar list); pass archived=false to restore. Use it to tidy up chats you created.",
            input_schema: chat_key_schema(json!({
                "archived": { "type": "boolean", "default": true }
            })),
        },
    ]
}

/// For tools that derive a key when it is omitted (naturally idempotent).
fn optional_idempotency_key_schema() -> Value {
    json!({
        "type": "string",
        "description": "Optional. Omit it and Mast derives a key from the call's arguments. Pass your own to make a retry of this exact call replay the first result."
    })
}

fn idempotency_key_schema() -> Value {
    json!({
        "type": "string",
        "description": "Stable key for this change. Reuse it when you retry the same call (the retry returns the first result and changes nothing); use a new key for a new change."
    })
}

/// The Work Outline agent tools (agent mode only). The engine authorizes
/// each call against the calling Chat; arguments are camelCase like the
/// rest of the Work Outline wire.
fn work_outline_catalog() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: agent_tool_names::READ_CURRENT,
            description: "Your chat's Work Outline item: title, status, path, children, the effective instructions it inherits, and its open user action requests. Null when your chat is not associated with an item. In Spaces with Linear turned on, also `linearIssue` (the linked Linear ticket of the item or its nearest ancestor, including its description) and `linearProject` (the nearest linked Linear project).",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolDef {
            name: agent_tool_names::READ_SUBTREE,
            description: "Read-only view of your chat's work item (or a descendant) and everything below it as a nested tree: per item its id, title, status and effectiveStatus, the chats attached to it (id, title, status working/awaitingInput/errored/idle, lastMessageAt), and its open user action requests (id, text, detail, detailTruncated, originChatId, createdAt; detail is capped at 1000 characters, so when detailTruncated is true read the full text with `read_chat` on originChatId). Use it as a coordinator to see what your children are doing and to poll for requests such as \"Ready to merge\". Limited to your item's subtree; at most 300 items (`truncated` says when more exist; `childrenOmitted` marks items whose children were cut by `maxDepth` or the cap).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workItemId": { "type": "string", "description": "Your chat's item or a descendant. Defaults to your chat's item." },
                    "maxDepth": { "type": "integer", "minimum": 0, "maximum": 8, "default": 4, "description": "Levels of descendants below the item to include." },
                    "includeArchived": { "type": "boolean", "default": false, "description": "Also list archived items and chats." }
                }
            }),
        },
        ToolDef {
            name: agent_tool_names::CREATE_CHILD,
            description: "Create a work item under your chat's item (default) or one of its descendants. The item records your chat as its creator. Required: title and idempotencyKey. Optional: description, parentWorkItemId, status.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "description": { "type": "string" },
                    "parentWorkItemId": { "type": "string", "description": "Your chat's item or a descendant. Defaults to your chat's item." },
                    "status": { "type": "string", "enum": ["pending", "active"], "default": "pending" },
                    "idempotencyKey": idempotency_key_schema()
                },
                "required": ["title", "idempotencyKey"]
            }),
        },
        ToolDef {
            name: agent_tool_names::ASSOCIATE_CURRENT_CHAT,
            description: "Associate your own chat with a work item of its project (never another chat). Required: workItemId. Optional: idempotencyKey (derived when omitted).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workItemId": { "type": "string" },
                    "idempotencyKey": optional_idempotency_key_schema()
                },
                "required": ["workItemId"]
            }),
        },
        ToolDef {
            name: agent_tool_names::SET_STATUS,
            description: "Mark your chat's item (default) or one of its descendants active or completed. Completing an item with open user action requests fails; they are never resolved for you. Required: status. Optional: workItemId (default your chat's item), idempotencyKey (derived when omitted).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workItemId": { "type": "string", "description": "Defaults to your chat's item." },
                    "status": { "type": "string", "enum": ["active", "completed"] },
                    "idempotencyKey": optional_idempotency_key_schema()
                },
                "required": ["status"]
            }),
        },
        ToolDef {
            name: agent_tool_names::CREATE_USER_ACTION_REQUEST,
            description: "Use this, alongside asking in chat and instead of relying only on your built-in ask-user prompt, whenever your chat's work item is blocked on a decision, approval, or missing information from the user; it stays open in Required Actions until resolved. Put a one-line question in `request`, context in `detail`, and a `dedupeKey` per topic. The user may answer via Required Actions' Reply & resolve, which arrives as a chat message starting `Re: \"…\"`; resolve the matching request when it does. Check that your chat has a work item with `work_outline_read_current`. Use your built-in quick question prompt only for small clarifications that do not block the work item. Required: request, and either idempotencyKey or dedupeKey (the dedupeKey stands in for the key). Optional: detail, dedupeKey, toolCallId.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "request": { "type": "string", "description": "The concise question or requested action." },
                    "detail": { "type": "string" },
                    "dedupeKey": { "type": "string", "description": "Equivalent open requests with the same key are not repeated. With a dedupeKey, idempotencyKey is optional." },
                    "toolCallId": { "type": "string" },
                    "idempotencyKey": json!({
                        "type": "string",
                        "description": "Stable key for this change; required unless you pass dedupeKey. Reuse it when you retry the same call."
                    })
                },
                "required": ["request"]
            }),
        },
        ToolDef {
            name: agent_tool_names::RESOLVE_USER_ACTION_REQUEST,
            description: "Resolve a user action request your chat, or a chat it created (a child started with `create_chat`), raised once the user has answered, in chat or via a chat message starting `Re: \"…\"` from Required Actions' Reply & resolve, if it is still open (see `work_outline_read_current`). Do not leave answered requests open. Required: userActionRequestId. Optional: resolution (a note), idempotencyKey (derived when omitted).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "userActionRequestId": { "type": "string" },
                    "resolution": { "type": "string" },
                    "idempotencyKey": optional_idempotency_key_schema()
                },
                "required": ["userActionRequestId"]
            }),
        },
        ToolDef {
            name: agent_tool_names::PROPOSE_INSTRUCTION,
            description: "Propose a durable instruction for your chat's item, its parent, or the project root. The user reviews it; nothing changes until they accept. Required: text and idempotencyKey. Optional: target (item, parent, or spaceRoot).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "target": { "type": "string", "enum": ["item", "parent", "spaceRoot"], "default": "item" },
                    "text": { "type": "string" },
                    "toolCallId": { "type": "string" },
                    "idempotencyKey": idempotency_key_schema()
                },
                "required": ["text", "idempotencyKey"]
            }),
        },
    ]
}

/// The planning artifact tools (agent mode only;
/// `docs/planning-artifact-conventions.md`). The engine scopes each call to
/// the calling Chat's work item (or the Chat).
fn artifact_catalog() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: agent_tool_names::ARTIFACT_PUBLISH,
            description: "Publish every file you wrote that the user should see (plan, diagram, state machine, option comparison) as a planning artifact of your chat's work item. Mast copies it into the work item's artifact folder (<canonical checkout>/.zeron/artifacts/<work item>/), records a revision when the content changed, and returns {artifactId, revision, url, storedPath}. Later publishes with the same title and role add revisions to the same artifact. Link it in your message as [title](zeron-artifact:<artifactId>), never as a bare path. HTML: write <name>.fragment.html (body content only) and publish it; a sibling <name>.preview.html is copied too. Options: publish the JSON manifest with kind options; its fragments are copied with it.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "The file you wrote: absolute, or relative to your working directory (your worktree, the project folder, or a temp folder)." },
                    "title": { "type": "string", "description": "Human title, e.g. \"Receiving flow — plan\". Same title + role on the same work item = same artifact." },
                    "kind": { "type": "string", "enum": ["markdown", "html-fragment", "html-page", "mermaid", "image", "options"], "description": "Defaults from the file extension." },
                    "role": { "type": "string", "description": "plan, diagram, state-machine, comparison, or doc (free-form)." },
                    "note": { "type": "string", "description": "One line: what changed in this revision." },
                    "pinAsPlan": { "type": "boolean", "default": false, "description": "Make this the work item's plan (unpins the previous one)." },
                    "idempotencyKey": idempotency_key_schema()
                },
                "required": ["path", "title", "note", "idempotencyKey"]
            }),
        },
        ToolDef {
            name: agent_tool_names::ARTIFACT_GET_PLAN,
            description: "The pinned plan of your chat's work item: {artifactId, title, revision, storedPath, content, openQuestions, unverifiedClaims, answeredQuestions: [{id, answer, at, revision}]}, or null. answeredQuestions are answers the user gave in Mast (the plan's question card or Required Actions) that the document may not reflect yet: fold each into the plan (`status: resolved` + `decision:`) in your next revision. Call it at the start of every chat under a work item; if a plan exists, work from it and publish new revisions of it instead of writing a new one.",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolDef {
            name: agent_tool_names::ARTIFACT_RENDER_CHECK,
            description: "Render an artifact exactly as Mast shows it (same wrapper, sandbox, and theme) in the Mast window on this device and return screenshots plus errors (console errors, blocked resources, Mermaid parse errors, empty render) and warnings (overflow, content outside the viewport, low contrast, and layout heuristics in layoutWarnings: clipped text, overlapping siblings, narrow text columns, elements wider than their parent, fixed widths wider than the viewport). By default it renders a narrow (800x800) and a wide (1440x900) viewport in the current theme; pass `themes: [\"light\", \"dark\"]` to check both themes (at most 4 images per call; each image comes with a label). Warnings are heuristics: always look at every screenshot. Run it after creating or changing any visual artifact, fix what is wrong, and check again. If it fails with renderUnavailable, say the artifact was not checked.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "artifactId": { "type": "string", "description": "A published artifact of your work item." },
                    "path": { "type": "string", "description": "Or an unpublished file you wrote." },
                    "viewports": {
                        "type": "array",
                        "description": "Viewports to render. Default: [{width: 800, height: 800, label: \"narrow\"}, {width: 1440, height: 900, label: \"wide\"}].",
                        "items": {
                            "type": "object",
                            "properties": {
                                "width": { "type": "integer", "minimum": 200, "maximum": 4096 },
                                "height": { "type": "integer", "minimum": 200, "maximum": 4096 },
                                "label": { "type": "string" }
                            },
                            "required": ["width", "height"]
                        }
                    },
                    "viewport": {
                        "type": "object",
                        "description": "A single viewport; replaces the default list.",
                        "properties": {
                            "width": { "type": "integer", "minimum": 200, "maximum": 4096 },
                            "height": { "type": "integer", "minimum": 200, "maximum": 4096 }
                        }
                    },
                    "themes": {
                        "type": "array",
                        "description": "Themes to render every viewport in. Default: the user's current theme only. At most 4 images (viewports x themes) per call.",
                        "items": { "type": "string", "enum": ["light", "dark"] }
                    },
                    "theme": { "type": "string", "enum": ["light", "dark"], "description": "A single theme. Default: the user's current theme." }
                }
            }),
        },
    ]
}

/// Chat diff target tools (agent mode only; `m0-contract.md`, "Amendment:
/// chat diff targets"). The engine binds for the calling Chat only.
const DIFF_TARGET_TOOLS: [&str; 1] = [agent_tool_names::BIND_CHAT_WORKTREE];

fn diff_target_catalog() -> Vec<ToolDef> {
    vec![ToolDef {
        name: agent_tool_names::BIND_CHAT_WORKTREE,
        description: "Tell Mast which git worktree this chat is working in, so the Changes pane shows that worktree instead of the folder the chat started in. Call it right after `git worktree add` (including worktrees of a submodule such as flagship, created under `.worktrees/`), and again when you resume an existing worktree for this chat's work. Call it once per worktree: a chat that works in several submodules binds each one. It only records the path; it does not create, switch, or modify anything. Pass the worktree's absolute top-level path (the output of `git rev-parse --show-toplevel` inside it); it must be a worktree of this chat's repository or of one of its submodules. Required: path. Optional: idempotencyKey (not needed: binding the same path again just re-selects it).",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path of the worktree's top-level folder." },
                "idempotencyKey": optional_idempotency_key_schema()
            },
            "required": ["path"]
        }),
    }]
}

/// Whether `name` is an engine-side agent tool (`AgentToolCall`).
fn is_engine_agent_tool(name: &str) -> bool {
    agent_tool_names::WORK_OUTLINE.contains(&name)
        || agent_tool_names::ARTIFACTS.contains(&name)
        || DIFF_TARGET_TOOLS.contains(&name)
}

/// Split a render-check result into MCP content: every screenshot as an
/// `image` block (each preceded by a text label naming its viewport and
/// theme), the rest as structured data without the image bytes. Falls back to
/// the single `screenshot` field for engines that predate `screenshots`.
fn render_check_content(mut value: Value) -> (Value, Vec<Value>) {
    let mut content = Vec::new();
    let Some(object) = value.as_object_mut() else {
        return (value, content);
    };
    let single = object.remove("screenshot");
    let mime = |shot: &Value| {
        shot.get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or("image/png")
            .to_owned()
    };
    let mut attached = false;
    if let Some(Value::Array(shots)) = object.get_mut("screenshots") {
        for shot in shots.iter_mut() {
            let Some(data) = shot.get("png").and_then(Value::as_str).map(str::to_owned) else {
                continue;
            };
            let label = format!(
                "Screenshot: {} ({}x{}), {} theme",
                shot.get("viewport")
                    .and_then(Value::as_str)
                    .unwrap_or("viewport"),
                shot.get("width").and_then(Value::as_u64).unwrap_or(0),
                shot.get("height").and_then(Value::as_u64).unwrap_or(0),
                shot.get("theme")
                    .and_then(Value::as_str)
                    .unwrap_or("current"),
            );
            content.push(json!({ "type": "text", "text": label }));
            content.push(json!({ "type": "image", "data": data, "mimeType": "image/png" }));
            if let Some(shot) = shot.as_object_mut() {
                shot.remove("png");
                shot.insert("attached".into(), json!(true));
            }
            attached = true;
        }
    }
    if let Some(screenshot) = single {
        let mime = mime(&screenshot);
        if !attached && let Some(data) = screenshot.get("data").and_then(Value::as_str) {
            content.push(json!({ "type": "image", "data": data, "mimeType": mime }));
            attached = true;
        }
        if attached {
            object.insert(
                "screenshot".into(),
                json!({ "mimeType": mime, "attached": true }),
            );
        }
    }
    (value, content)
}

// ---- argument shapes ---------------------------------------------------------

#[derive(Deserialize)]
struct ChatArgs {
    chat: String,
}

#[derive(Deserialize)]
struct ListModelsArgs {
    harness: String,
}

#[derive(Deserialize, Default)]
struct ListChatsArgs {
    project: Option<String>,
    device: Option<String>,
    #[serde(default)]
    include_archived: bool,
    parent: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize, Default)]
struct CreateChatArgs {
    #[serde(alias = "idempotencyKey")]
    idempotency_key: Option<String>,
    #[serde(alias = "workItemId")]
    work_item_id: Option<String>,
    project: Option<String>,
    device: Option<String>,
    parent: Option<String>,
    harness: Option<String>,
    model: Option<String>,
    reasoning: Option<String>,
    sandbox: Option<String>,
    title: Option<String>,
    branch: Option<String>,
    cwd: Option<String>,
    prompt: Option<String>,
    #[serde(default)]
    wait: bool,
    timeout_secs: Option<u64>,
}

#[derive(Deserialize)]
struct ReadChatArgs {
    chat: String,
    limit: Option<usize>,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    include_reasoning: bool,
    include_tools: Option<bool>,
}

#[derive(Deserialize)]
struct SendArgs {
    chat: String,
    text: String,
    mode: Option<String>,
    #[serde(default)]
    wait: bool,
    timeout_secs: Option<u64>,
}

#[derive(Deserialize)]
struct WaitArgs {
    chat: String,
    timeout_secs: Option<u64>,
}

#[derive(Deserialize)]
struct ArchiveArgs {
    chat: String,
    archived: Option<bool>,
}

#[derive(Deserialize)]
struct RespondArgs {
    chat: String,
    request_id: Option<String>,
    #[serde(default)]
    answers: Vec<AnswerArg>,
}

#[derive(Deserialize)]
struct AnswerArg {
    question_id: String,
    labels: Vec<String>,
}

fn parse<T: serde::de::DeserializeOwned>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|e| format!("invalid arguments: {e}"))
}

fn wait_duration(secs: Option<u64>) -> Duration {
    secs.map(Duration::from_secs)
        .unwrap_or(DEFAULT_WAIT)
        .min(MAX_WAIT)
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

fn parse_enum<T: serde::de::DeserializeOwned>(what: &str, raw: &str) -> Result<T, String> {
    serde_json::from_value(Value::String(raw.trim().to_owned()))
        .map_err(|_| format!("unknown {what}: {raw:?}"))
}

// ---- summaries ---------------------------------------------------------------

/// Live posture of one chat as the tools report it.
fn status_of(session: Option<&Session>) -> (String, Option<i64>) {
    let Some(session) = session else {
        return ("idle".into(), None);
    };
    let age = chrono::Utc::now() - session.updated_at;
    let stale = age > SESSION_STALE;
    let label = match session.status {
        SessionStatus::Working if stale => "working (stale — host may be offline)",
        SessionStatus::Working => "working",
        SessionStatus::AwaitingInput => "awaitingInput",
        SessionStatus::Errored => "errored",
        SessionStatus::Idle => "idle",
    };
    (label.into(), Some(age.num_seconds().max(0)))
}

fn summarize_chat(chat: &Chat, spaces: &[Space], sessions: &[Session]) -> Value {
    let space = chat
        .space_id
        .as_deref()
        .and_then(|id| spaces.iter().find(|s| s.id == id));
    let session = session_for(sessions, chat);
    let (status, status_age) = status_of(session.as_ref());
    json!({
        "id": chat.id,
        "title": chat.title,
        "project": space.map(|s| json!({
            "id": s.id,
            "name": s.display_name(),
            "path": s.path,
        })),
        "deviceId": chat.device_id,
        "cwd": chat.cwd.clone().or_else(|| space.map(|s| s.path.clone())),
        "branch": chat.branch,
        "harness": chat.config.as_ref().map(|c| c.harness),
        // `model` is what ran when the harness reported it, else what the chat
        // is configured to use (`modelIsActual` says which); null = harness
        // default decides.
        "model": match chat.model_display() {
            ChatModelDisplay::Actual { actual, .. } => Some(actual),
            ChatModelDisplay::Configured(model) => Some(model),
            ChatModelDisplay::Default => None,
        },
        "modelIsActual": matches!(chat.model_display(), ChatModelDisplay::Actual { .. }),
        "actualModel": chat.actual_model,
        "configuredModel": chat.config.as_ref().and_then(|c| c.model.clone()),
        "reasoning": chat.config.as_ref().and_then(|c| c.reasoning),
        "archived": chat.archived,
        "parentChatId": chat.parent_chat_id,
        "status": status,
        "statusAgeSecs": status_age,
        "lastMessageAt": chat.last_message_at,
        "lastMessagePreview": chat.last_message_preview,
        "createdAt": chat.created_at,
    })
}

fn last_pending_input(messages: &[RenderedMessage]) -> Option<Value> {
    messages.iter().rev().find_map(|m| m.pending_input.clone())
}

// ---- dispatch ----------------------------------------------------------------

impl Tools {
    pub fn new(zeron: Arc<Zeron>) -> Self {
        Self { zeron }
    }

    /// The original tools, plus the Work Outline agent tools when the
    /// engine injected this server with a token.
    pub fn list(&self) -> Vec<ToolDef> {
        let mut defs = catalog();
        if self.zeron.has_token() {
            defs.extend(work_outline_catalog());
            defs.extend(artifact_catalog());
            defs.extend(diff_target_catalog());
        }
        defs
    }

    pub fn has(&self, name: &str) -> bool {
        self.list().iter().any(|t| t.name == name)
    }

    /// `Ok` is the tool's structured result; `Err` is a message the model
    /// should read (surfaced as `isError`, never as a protocol error).
    ///
    /// With a token, every call (reads included) first re-authenticates it:
    /// once the Run that received it ends, nothing works.
    pub async fn call(&self, name: &str, args: Value) -> Result<Value, String> {
        self.call_with_content(name, args)
            .await
            .map(|(value, _)| value)
    }

    /// [`Self::call`] plus extra MCP content blocks (the render check's
    /// screenshot as an `image` block).
    pub async fn call_with_content(
        &self,
        name: &str,
        args: Value,
    ) -> Result<(Value, Vec<Value>), String> {
        if name == agent_tool_names::ARTIFACT_RENDER_CHECK {
            let value = self
                .zeron
                .agent_tool(name, args)
                .await
                .map_err(|e| e.to_string())?;
            return Ok(render_check_content(value));
        }
        self.call_value(name, args)
            .await
            .map(|value| (value, Vec::new()))
    }

    /// Run one tool. Guessed argument names are normalised first (one place:
    /// `agent_tools::normalize_arguments`), and an argument error from
    /// `create_chat` lists its parameters and an example (engine tools add
    /// theirs engine-side).
    async fn call_value(&self, name: &str, args: Value) -> Result<Value, String> {
        let args = agent_tools::normalize_arguments(name, &args);
        self.call_normalized(name, args).await.map_err(|error| {
            let argument_error = error.starts_with("invalid arguments")
                || error.contains("idempotencyKey is required");
            if name == "create_chat" && argument_error {
                agent_tools::with_usage(name, &error)
            } else {
                error
            }
        })
    }

    async fn call_normalized(&self, name: &str, args: Value) -> Result<Value, String> {
        // `AgentToolCall` authenticates the token itself: one round trip.
        if is_engine_agent_tool(name) {
            return self
                .zeron
                .agent_tool(name, args)
                .await
                .map_err(|e| e.to_string());
        }
        let origin = self.zeron.authenticate().await.map_err(|e| e.to_string())?;
        let origin = origin.as_ref();
        let result = match name {
            "whoami" => self.whoami(origin).await,
            "list_devices" => self.list_devices(origin).await,
            "list_projects" => self.list_projects(origin).await,
            "list_harnesses" => self.list_harnesses().await,
            "list_models" => self.list_models(parse(args)?).await,
            "list_chats" => self.list_chats(origin, parse(args)?).await,
            "get_chat" => self.get_chat(origin, parse(args)?).await,
            "create_chat" => self.create_chat(origin, parse(args)?).await,
            "read_chat" => self.read_chat(origin, parse(args)?).await,
            "send_message" => self.send_message(origin, parse(args)?).await,
            "wait_for_turn" => self.wait_for_turn(origin, parse(args)?).await,
            "interrupt_chat" => self.interrupt_chat(origin, parse(args)?).await,
            "respond_to_input" => self.respond_to_input(origin, parse(args)?).await,
            "archive_chat" => self.archive_chat(origin, parse(args)?).await,
            other => return Err(format!("unknown tool: {other}")),
        };
        result.map_err(|e| e.to_string())
    }

    async fn whoami(&self, origin: Option<&AgentOrigin>) -> anyhow::Result<Value> {
        let local_device = self.zeron.local_device_id().await?;
        let engine = self.zeron.engine_info().await.unwrap_or(Value::Null);
        let chat = match origin {
            Some(origin) => match self.zeron.resolve_chat(&origin.chat_id).await {
                Ok(chat) => {
                    let (spaces, sessions) =
                        tokio::try_join!(self.zeron.spaces(), self.zeron.sessions())?;
                    summarize_chat(&chat, &spaces, &sessions)
                }
                Err(_) => json!({ "id": origin.chat_id }),
            },
            None => Value::Null,
        };
        Ok(json!({
            "chat": chat,
            "authenticated": origin.is_some(),
            "originDeviceId": origin.map(|o| o.device_id.clone()),
            "runId": origin.map(|o| o.run_id.clone()),
            "localDeviceId": local_device,
            "workspaceScope": engine.get("workspaceScope").cloned().unwrap_or(Value::Null),
            "workOutlineTools": if origin.is_some() {
                json!(agent_tool_names::WORK_OUTLINE)
            } else {
                json!([])
            },
            "artifactTools": if origin.is_some() {
                json!(agent_tool_names::ARTIFACTS)
            } else {
                json!([])
            },
            "note": if origin.is_some() {
                "Messages you send are attributed to this chat; it cannot message itself. \
                 You can reach only the chats you created (plus read this one), and \
                 create_chat works only in this chat's project, never with a wider \
                 sandbox than this chat's. Mutating Work Outline tools and create_chat \
                 need an idempotency key."
            } else {
                "Not running inside a chat: messages are sent without attribution."
            },
        }))
    }

    async fn list_devices(&self, origin: Option<&AgentOrigin>) -> anyhow::Result<Value> {
        let (mut devices, local) =
            tokio::try_join!(self.zeron.devices(), self.zeron.local_device_id())?;
        if let Some(origin) = origin {
            devices.retain(|d| d.id == origin.device_id);
        }
        Ok(json!({
            "devices": devices.iter().map(|d| json!({
                "id": d.id,
                "name": d.name,
                "platform": d.platform,
                "local": d.id == local,
                "lastSeenAt": d.last_seen_at,
                "version": d.version,
            })).collect::<Vec<_>>()
        }))
    }

    async fn list_projects(&self, origin: Option<&AgentOrigin>) -> anyhow::Result<Value> {
        let (mut spaces, devices) = tokio::try_join!(self.zeron.spaces(), self.zeron.devices())?;
        if let Some(origin) = origin {
            spaces.retain(|s| s.id == origin.space_id);
        }
        let device_name = |id: &str| devices.iter().find(|d| d.id == id).map(|d| d.name.clone());
        Ok(json!({
            "projects": spaces.iter().map(|s| json!({
                "id": s.id,
                "name": s.display_name(),
                "path": s.path,
                "deviceId": s.device_id,
                "deviceName": device_name(&s.device_id),
                "git": s.git_detected,
            })).collect::<Vec<_>>()
        }))
    }

    async fn list_harnesses(&self) -> anyhow::Result<Value> {
        let harnesses = self.zeron.harnesses().await?;
        Ok(json!({
            "harnesses": harnesses.iter().map(|h| json!({
                "id": h.id,
                "name": h.name,
                "available": h.available(),
                "installed": h.installed,
                "steersMidTurn": h.steers_mid_turn(),
                "reasoningLevels": h.reasoning_levels,
            })).collect::<Vec<_>>()
        }))
    }

    async fn list_models(&self, args: ListModelsArgs) -> anyhow::Result<Value> {
        let harness: HarnessId =
            parse_enum("harness", &args.harness).map_err(anyhow::Error::msg)?;
        let models = self.zeron.models(harness).await?;
        Ok(json!({
            "harness": harness,
            "models": models.iter().map(|m| json!({
                "id": m.id,
                "label": m.label,
                "description": m.description,
                "reasoningLevels": m.reasoning_levels,
            })).collect::<Vec<_>>()
        }))
    }

    async fn list_chats(
        &self,
        origin: Option<&AgentOrigin>,
        args: ListChatsArgs,
    ) -> anyhow::Result<Value> {
        let (mut chats, spaces, sessions) = tokio::try_join!(
            self.zeron.chats(),
            self.zeron.spaces(),
            self.zeron.sessions()
        )?;
        if let Some(origin) = origin {
            chats.retain(|c| reachable(origin, c, true));
        }
        if let Some(project) = args.project.as_deref() {
            let space = self.zeron.resolve_space(project).await?;
            chats.retain(|c| c.space_id.as_deref() == Some(space.id.as_str()));
        }
        if args.device.is_some() {
            let device = self.zeron.resolve_device_id(args.device.as_deref()).await?;
            chats.retain(|c| c.device_id == device);
        }
        if !args.include_archived {
            chats.retain(|c| !c.archived);
        }
        if let Some(parent) = args.parent.as_deref() {
            let parent = self.reach_chat(origin, parent, Reach::SelfOrChild).await?;
            chats.retain(|c| c.parent_chat_id.as_deref() == Some(parent.id.as_str()));
        }
        chats.sort_by(|a, b| {
            let a_at = a.last_message_at.unwrap_or(a.created_at);
            let b_at = b.last_message_at.unwrap_or(b.created_at);
            b_at.cmp(&a_at)
        });
        let total = chats.len();
        let limit = args.limit.unwrap_or(50).clamp(1, 500);
        Ok(json!({
            "total": total,
            "chats": chats.iter().take(limit)
                .map(|c| summarize_chat(c, &spaces, &sessions))
                .collect::<Vec<_>>()
        }))
    }

    async fn get_chat(
        &self,
        origin: Option<&AgentOrigin>,
        args: ChatArgs,
    ) -> anyhow::Result<Value> {
        let chat = self
            .reach_chat(origin, &args.chat, Reach::SelfOrChild)
            .await?;
        let (spaces, sessions, entries) = tokio::try_join!(
            self.zeron.spaces(),
            self.zeron.sessions(),
            self.zeron.transcript(&chat.id)
        )?;
        let rendered = render_entries(&entries, RenderOptions::default());
        let mut summary = summarize_chat(&chat, &spaces, &sessions);
        summary["messageCount"] = json!(rendered.len());
        summary["pendingInput"] = last_pending_input(&rendered).unwrap_or(Value::Null);
        summary["lastMessage"] = rendered.last().map(|m| json!(m)).unwrap_or(Value::Null);
        Ok(summary)
    }

    async fn create_chat(
        &self,
        origin: Option<&AgentOrigin>,
        args: CreateChatArgs,
    ) -> anyhow::Result<Value> {
        // Agent mode: the ids derive from (token Chat, key), so a retry
        // finds the Chat it already created instead of minting another.
        let agent_key = match origin {
            Some(_) => Some(
                agent_tools::validate_idempotency_key(args.idempotency_key.as_deref())
                    .map_err(anyhow::Error::msg)?
                    .to_owned(),
            ),
            None => None,
        };
        let harnesses = self.zeron.harnesses().await?;
        let harness = match args.harness.as_deref() {
            Some(raw) => {
                let id: HarnessId = parse_enum("harness", raw).map_err(anyhow::Error::msg)?;
                if let Some(info) = harnesses.iter().find(|h| h.id == id)
                    && !info.available()
                {
                    anyhow::bail!(
                        "harness {raw} is not available on this device (see list_harnesses)"
                    );
                }
                id
            }
            None => default_harness(&harnesses)?,
        };
        if let Some(model) = args.model.as_deref().filter(|m| !m.trim().is_empty())
            && let Ok(models) = self.zeron.models(harness).await
            && !models.is_empty()
            && !models.iter().any(|m| m.id == model)
        {
            anyhow::bail!(
                "model {model:?} is not offered by {harness:?}; available: {}",
                models
                    .iter()
                    .map(|m| m.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        let reasoning: Option<ReasoningLevel> = match args.reasoning.as_deref() {
            Some(raw) => Some(parse_enum("reasoning level", raw).map_err(anyhow::Error::msg)?),
            None => None,
        };
        let requested_sandbox: Option<SandboxLevel> = match args.sandbox.as_deref() {
            Some(raw) => Some(parse_enum("sandbox", raw).map_err(anyhow::Error::msg)?),
            None => None,
        };
        // Agent mode: the calling Chat (its sandbox caps the child's, its cwd
        // bounds the child's cwd).
        let caller = match origin {
            Some(origin) => Some(
                self.zeron
                    .chats()
                    .await?
                    .into_iter()
                    .find(|c| c.id == origin.chat_id)
                    .ok_or_else(|| anyhow::anyhow!("this agent's chat no longer exists"))?,
            ),
            None => None,
        };
        let sandbox = match &caller {
            Some(caller) => {
                let cap = caller
                    .config
                    .as_ref()
                    .map(|c| c.sandbox)
                    .unwrap_or(SandboxLevel::WorkspaceWrite);
                match requested_sandbox {
                    Some(level) if sandbox_rank(level) > sandbox_rank(cap) => anyhow::bail!(
                        "sandbox {} is more permissive than this chat's ({}); a child chat \
                         can never have a wider sandbox than its parent",
                        sandbox_name(level),
                        sandbox_name(cap)
                    ),
                    Some(level) => level,
                    None => std::cmp::min_by_key(SandboxLevel::WorkspaceWrite, cap, |l| {
                        sandbox_rank(*l)
                    }),
                }
            }
            None => requested_sandbox.unwrap_or(SandboxLevel::WorkspaceWrite),
        };
        // Never leave the model empty: the harness CLI would fall back to its
        // own default (e.g. `~/.claude/settings.json`), not the composer's.
        let model = zeron_proto::resolve_model(harness, args.model.as_deref());
        if args.model.as_deref().is_none_or(|m| m.trim().is_empty()) {
            tracing::debug!(?harness, model = ?model, "create_chat had no model; filled in the Mast default");
        }
        let config = ChatConfig {
            harness,
            model: model.clone(),
            reasoning,
            model_options: Default::default(),
            sandbox,
            // Always Ask: never more permissive than any parent.
            auto_approve: false,
        };

        let (space, device_id) = match (origin, args.project.as_deref()) {
            // Agent mode: the token's Space on the token's device, only.
            (Some(origin), project) => {
                let space = self
                    .zeron
                    .resolve_space(project.unwrap_or(&origin.space_id))
                    .await?;
                if space.id != origin.space_id {
                    anyhow::bail!(
                        "agents can only create chats in their own project ({})",
                        short(&origin.space_id)
                    );
                }
                if let Some(device) = args.device.as_deref()
                    && self.zeron.resolve_device_id(Some(device)).await? != origin.device_id
                {
                    anyhow::bail!("agents can only create chats on their own device");
                }
                if space.device_id != origin.device_id {
                    anyhow::bail!("agents can only create chats on their own device");
                }
                (Some(space), origin.device_id.clone())
            }
            (None, Some(project)) => {
                let space = self.zeron.resolve_space(project).await?;
                let device_id = space.device_id.clone();
                (Some(space), device_id)
            }
            (None, None) => (
                None,
                self.zeron.resolve_device_id(args.device.as_deref()).await?,
            ),
        };
        let cwd = args
            .cwd
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_owned);
        if let (Some(caller), Some(cwd)) = (&caller, &cwd) {
            let roots = [
                space.as_ref().map(|s| s.path.as_str()),
                caller.cwd.as_deref(),
            ];
            if !path_within_any(cwd, &roots) {
                anyhow::bail!(
                    "agents can only start a child chat inside this project's folder or \
                     this chat's working directory (cwd {cwd:?} is outside both)"
                );
            }
        }

        let work_item_id = args
            .work_item_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        if work_item_id.is_some() {
            if origin.is_none() {
                anyhow::bail!("workItemId is only available inside a Mast chat");
            }
            if args.prompt.as_deref().is_none_or(|p| p.trim().is_empty()) {
                anyhow::bail!("workItemId needs a prompt: the child is started with it");
            }
            if cwd.is_some() || args.branch.as_deref().is_some_and(|b| !b.trim().is_empty()) {
                anyhow::bail!(
                    "cwd and branch cannot be set with workItemId: the child runs where the \
                     work item's execution setup says"
                );
            }
        }

        // Parent: the explicit `parent` argument, else the chat this server
        // speaks for. Resolved so a prefix/title works and a typo fails loud.
        // An agent may only record its own Chat (a child inherits the
        // parent's work item and instructions, so another parent would
        // reach into someone else's work).
        let explicit_parent = match args
            .parent
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            Some(key) => Some(self.zeron.resolve_chat(key).await?.id),
            None => None,
        };
        let parent_chat_id = match (origin, explicit_parent) {
            (Some(origin), Some(parent)) if parent != origin.chat_id => {
                anyhow::bail!("agents can only create child chats of their own chat; omit parent")
            }
            (Some(origin), _) => Some(origin.chat_id.clone()),
            (None, parent) => parent,
        };

        let (chat_id, replayed) = match (origin, agent_key.as_deref()) {
            (Some(origin), Some(key)) => {
                // Bind the key to these inputs first: a retry that changed its
                // request is a conflict, never the old chat returned silently.
                let inputs = json!({
                    "spaceId": space.as_ref().map(|s| &s.id),
                    "deviceId": device_id,
                    "config": config,
                    "parentChatId": parent_chat_id,
                    "title": args.title.as_deref().map(str::trim),
                    "branch": args.branch.as_deref().map(str::trim),
                    "cwd": cwd,
                    "prompt": args.prompt,
                });
                let mut inputs = inputs;
                if let Some(item) = &work_item_id {
                    // Only when set, so keys claimed before `workItemId`
                    // existed still replay.
                    inputs["workItemId"] = json!(item);
                }
                let inputs_hash =
                    zeron_proto::derive_id(&inputs.to_string(), "mcp-create-chat-inputs");
                self.zeron
                    .agent_tool(
                        agent_tool_names::CLAIM_CHILD_CHAT,
                        json!({ "idempotencyKey": key, "inputsHash": inputs_hash }),
                    )
                    .await?;
                let operation_id = agent_tools::operation_id(&origin.chat_id, key);
                let chat_id = zeron_proto::derive_id(&operation_id, zeron_proto::roles::CHAT);
                let existing = self
                    .zeron
                    .chats()
                    .await?
                    .into_iter()
                    .find(|c| c.id == chat_id);
                if let Some(existing) = &existing
                    && existing.parent_chat_id.as_deref() != Some(origin.chat_id.as_str())
                {
                    anyhow::bail!("idempotency_key names a chat this chat did not create");
                }
                (chat_id, existing.is_some())
            }
            _ => (uuid::Uuid::new_v4().to_string(), false),
        };
        // For a work item the engine starts the child through the launch
        // workflow (Chat, assignment, execution binding, effective
        // instructions, first Turn), after checking the item is the
        // caller's or a descendant; ids derive from the same
        // (caller, key) operation, so retries replay.
        let launched = match (&work_item_id, agent_key.as_deref(), &args.prompt) {
            (Some(item), Some(key), Some(prompt)) => {
                let reply = self
                    .zeron
                    .agent_tool(
                        agent_tool_names::LAUNCH_CHILD_CHAT,
                        json!({
                            "idempotencyKey": key,
                            "workItemId": item,
                            "prompt": prompt,
                            "config": config,
                        }),
                    )
                    .await?;
                if reply.get("chatId").and_then(Value::as_str) != Some(chat_id.as_str()) {
                    anyhow::bail!("the engine started an unexpected chat for this request");
                }
                Some(reply)
            }
            _ => None,
        };
        let cwd = match launched
            .as_ref()
            .and_then(|r| r.get("executionRoot"))
            .and_then(Value::as_str)
        {
            Some(root) => Some(root.to_owned()),
            None => cwd,
        };
        if launched.is_none() {
            let mut mutate = json!({
                "op": "createChat",
                "chatId": chat_id,
                "deviceId": device_id,
                "config": config,
            });
            if let Some(space) = &space {
                mutate["spaceId"] = json!(space.id);
            }
            if let Some(parent) = &parent_chat_id {
                mutate["parentChatId"] = json!(parent);
            }
            if let Some(branch) = args
                .branch
                .as_deref()
                .map(str::trim)
                .filter(|b| !b.is_empty())
            {
                mutate["branch"] = json!(branch);
            }
            if let Some(cwd) = &cwd {
                mutate["cwd"] = json!(cwd);
            }
            // `createChat` is a no-op for an existing row (never a retarget).
            self.zeron.mutate(mutate).await?;
        }
        // A replay leaves the title alone (the user may have renamed it).
        if let Some(title) = args
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty() && !replayed)
        {
            self.zeron
                .mutate(json!({ "op": "renameChat", "chatId": chat_id, "title": title }))
                .await?;
        }

        let mut result = json!({
            "chatId": chat_id,
            "deviceId": device_id,
            "project": space.as_ref().map(|s| json!({ "id": s.id, "name": s.display_name(), "path": s.path })),
            "harness": harness,
            "model": model,
            "reasoning": reasoning,
            "title": args.title,
            "parentChatId": parent_chat_id,
            "replayed": replayed,
        });
        if let Some(item) = &work_item_id {
            result["workItemId"] = json!(item);
            result["executionRoot"] = json!(cwd);
        }
        if let Some(prompt) = args.prompt.filter(|p| !p.trim().is_empty()) {
            // The row may not have folded into WatchChats yet; build the
            // chat locally from what we just wrote rather than re-reading.
            let chat = Chat {
                id: chat_id.clone(),
                device_id: device_id.clone(),
                title: args.title.clone(),
                archived: false,
                cwd: cwd.clone(),
                branch: args.branch.clone(),
                checkout_id: None,
                source_context: None,
                config: Some(config),
                last_message_preview: None,
                last_message_at: None,
                created_at: chrono::Utc::now(),
                harness_session_id: None,
                harness_session_cwd: None,
                actual_model: None,
                parent_chat_id: parent_chat_id.clone(),
                space_id: space.as_ref().map(|s| s.id.clone()),
                last_seen_at: None,
                room_gen: None,
                linked_pr_url: None,
                linked_pr_source: None,
                linked_ticket_id: None,
                linked_ticket_source: None,
            };
            let sent = match agent_key.as_deref() {
                // The launch already queued the first Turn.
                Some(_) if launched.is_some() => {
                    let launch = launched.as_ref();
                    json!({
                        "delivery": "run",
                        "id": launch.and_then(|r| r.get("commandId")).cloned().unwrap_or(Value::Null),
                        "state": launch.and_then(|r| r.get("state")).cloned().unwrap_or(Value::Null),
                        "detail": launch.and_then(|r| r.get("detail")).cloned().unwrap_or(Value::Null),
                        "chatStatusAtSend": "idle",
                    })
                }
                // The engine derives the message and command ids from the
                // key, queues at most one first Turn per key, and builds the
                // Run from the child's stored config (only the prompt is ours).
                Some(key) => {
                    let queued = self
                        .zeron
                        .agent_tool(
                            agent_tool_names::QUEUE_CHILD_CHAT_TURN,
                            json!({
                                "chatId": chat.id,
                                "idempotencyKey": key,
                                "prompt": prompt,
                            }),
                        )
                        .await?;
                    json!({
                        "delivery": "run",
                        "id": queued.get("commandId").cloned().unwrap_or(Value::Null),
                        "queued": queued.get("queued").cloned().unwrap_or(Value::Null),
                        "chatStatusAtSend": "idle",
                    })
                }
                None => {
                    self.deliver(&chat, space.as_ref(), &harnesses, None, prompt, "run")
                        .await?
                }
            };
            result["sent"] = sent;
            if args.wait {
                result["turn"] = self
                    .await_turn(
                        &chat,
                        None,
                        true,
                        wait_duration(args.timeout_secs),
                        now_millis(),
                    )
                    .await?;
            }
        }
        Ok(result)
    }

    async fn read_chat(
        &self,
        origin: Option<&AgentOrigin>,
        args: ReadChatArgs,
    ) -> anyhow::Result<Value> {
        let chat = self
            .reach_chat(origin, &args.chat, Reach::SelfOrChild)
            .await?;
        let entries = self.zeron.transcript(&chat.id).await?;
        let rendered = render_entries(
            &entries,
            RenderOptions {
                include_reasoning: args.include_reasoning,
                include_tools: args.include_tools.unwrap_or(true),
            },
        );
        let total = rendered.len();
        let limit = args.limit.unwrap_or(40).clamp(1, 500);
        let end = total.saturating_sub(args.offset);
        let start = end.saturating_sub(limit);
        let window = &rendered[start..end];
        Ok(json!({
            "chatId": chat.id,
            "title": chat.title,
            "total": total,
            "returned": window.len(),
            "olderRemaining": start,
            "newerSkipped": total - end,
            "pendingInput": last_pending_input(&rendered),
            "messages": window,
        }))
    }

    async fn send_message(
        &self,
        origin: Option<&AgentOrigin>,
        args: SendArgs,
    ) -> anyhow::Result<Value> {
        let text = args.text.trim();
        if text.is_empty() {
            anyhow::bail!("text is empty");
        }
        let chat = self
            .reach_chat(origin, &args.chat, Reach::SelfOrChild)
            .await?;
        if origin.is_some_and(|o| o.chat_id == chat.id) {
            anyhow::bail!(
                "refusing to send a message to your own chat ({})",
                short(&chat.id)
            );
        }
        let (spaces, sessions, harnesses) = tokio::try_join!(
            self.zeron.spaces(),
            self.zeron.sessions(),
            self.zeron.harnesses()
        )?;
        let space = chat
            .space_id
            .as_deref()
            .and_then(|id| spaces.iter().find(|s| s.id == id));
        let baseline = session_for(&sessions, &chat);
        let mode = args.mode.as_deref().unwrap_or("auto");
        let sent_at = now_millis();
        let body = self.attribute(origin, &chat, text).await;
        let mut result = json!({
            "chatId": chat.id,
            "title": chat.title,
        });
        result["sent"] = self
            .deliver(&chat, space, &harnesses, baseline.as_ref(), body, mode)
            .await?;
        if args.wait {
            result["turn"] = self
                .await_turn(
                    &chat,
                    baseline.as_ref(),
                    true,
                    wait_duration(args.timeout_secs),
                    sent_at,
                )
                .await?;
        }
        Ok(result)
    }

    async fn wait_for_turn(
        &self,
        origin: Option<&AgentOrigin>,
        args: WaitArgs,
    ) -> anyhow::Result<Value> {
        let chat = self.reach_chat(origin, &args.chat, Reach::Child).await?;
        let turn = self
            .await_turn(&chat, None, false, wait_duration(args.timeout_secs), 0)
            .await?;
        Ok(json!({ "chatId": chat.id, "title": chat.title, "turn": turn }))
    }

    async fn archive_chat(
        &self,
        origin: Option<&AgentOrigin>,
        args: ArchiveArgs,
    ) -> anyhow::Result<Value> {
        let chat = self
            .reach_chat(origin, &args.chat, Reach::Child)
            .await
            .map_err(|e| anyhow::anyhow!("agents can only archive chats they created: {e}"))?;
        let archived = args.archived.unwrap_or(true);
        self.zeron
            .mutate(json!({ "op": "setChatArchived", "chatId": chat.id, "archived": archived }))
            .await?;
        Ok(json!({ "chatId": chat.id, "title": chat.title, "archived": archived }))
    }

    async fn interrupt_chat(
        &self,
        origin: Option<&AgentOrigin>,
        args: ChatArgs,
    ) -> anyhow::Result<Value> {
        let chat = self.reach_chat(origin, &args.chat, Reach::Child).await?;
        let command_id = self
            .zeron
            .queue_command(&chat.id, &SessionCommandPayload::Interrupt {})
            .await?;
        Ok(json!({ "chatId": chat.id, "commandId": command_id }))
    }

    async fn respond_to_input(
        &self,
        origin: Option<&AgentOrigin>,
        args: RespondArgs,
    ) -> anyhow::Result<Value> {
        let chat = self.reach_chat(origin, &args.chat, Reach::Child).await?;
        let request_id = match args.request_id {
            Some(id) => id,
            None => {
                let entries = self.zeron.transcript(&chat.id).await?;
                let rendered = render_entries(&entries, RenderOptions::default());
                last_pending_input(&rendered)
                    .and_then(|p| {
                        p.get("requestId")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!("chat {} has no pending question", short(&chat.id))
                    })?
            }
        };
        if args.answers.is_empty() {
            anyhow::bail!("answers is empty");
        }
        let answers = args
            .answers
            .into_iter()
            .map(|a| UserInputAnswer {
                question_id: a.question_id,
                labels: a.labels,
            })
            .collect();
        let command_id = self
            .zeron
            .queue_command(
                &chat.id,
                &SessionCommandPayload::RespondInput {
                    request_id: request_id.clone(),
                    answers,
                },
            )
            .await?;
        Ok(json!({ "chatId": chat.id, "requestId": request_id, "commandId": command_id }))
    }

    // ---- shared pieces -------------------------------------------------------

    /// Resolve a chat key. Without a token: any chat. With one: only among
    /// the chats the token's Chat may reach (so a foreign chat is neither
    /// targeted nor revealed by an ambiguity error).
    async fn reach_chat(
        &self,
        origin: Option<&AgentOrigin>,
        key: &str,
        reach: Reach,
    ) -> anyhow::Result<Chat> {
        let Some(origin) = origin else {
            return self.zeron.resolve_chat(key).await;
        };
        let allow_self = reach == Reach::SelfOrChild;
        let chats: Vec<Chat> = self
            .zeron
            .chats()
            .await?
            .into_iter()
            .filter(|c| reachable(origin, c, allow_self))
            .collect();
        resolve_chat_in(&chats, key).map_err(|_| {
            anyhow::anyhow!(
                "no chat this chat created{} matches {key:?}; agents can only reach the chats \
                 they created (list_chats shows them)",
                if allow_self {
                    " (or this chat itself)"
                } else {
                    ""
                }
            )
        })
    }

    /// Prefix the sender's identity when this server speaks for a chat, so
    /// the receiving agent (and the human reading that transcript) can tell
    /// an agent-to-agent message from a typed one.
    async fn attribute(&self, origin: Option<&AgentOrigin>, target: &Chat, text: &str) -> String {
        let Some(origin_id) = origin.map(|o| o.chat_id.as_str()) else {
            return text.to_owned();
        };
        if origin_id == target.id {
            return text.to_owned();
        }
        let title = match self.zeron.resolve_chat(origin_id).await {
            Ok(chat) => chat.title,
            Err(_) => None,
        };
        let label = match title {
            Some(title) if !title.trim().is_empty() => {
                format!("{} ({})", title.trim(), short(origin_id))
            }
            _ => short(origin_id).to_owned(),
        };
        format!(
            "[Message from Mast chat {label}. Reply to it with the Mast `send_message` tool, chat {}.]\n\n{text}",
            short(origin_id)
        )
    }

    /// Pick and perform the delivery the composer would.
    async fn deliver(
        &self,
        chat: &Chat,
        space: Option<&Space>,
        harnesses: &[HarnessInfo],
        session: Option<&Session>,
        text: String,
        mode: &str,
    ) -> anyhow::Result<Value> {
        let harness = chat
            .config
            .as_ref()
            .map(|c| c.harness)
            .map_or_else(|| default_harness(harnesses), Ok)?;
        let info = harnesses.iter().find(|h| h.id == harness);
        let (status, _) = status_of(session);
        let live = session
            .map(|s| {
                let stale = chrono::Utc::now() - s.updated_at > SESSION_STALE;
                if stale && s.status == SessionStatus::Working {
                    SessionStatus::Idle
                } else {
                    s.status
                }
            })
            .unwrap_or(SessionStatus::Idle);
        let chosen = match mode {
            "auto" => match live {
                SessionStatus::Idle | SessionStatus::Errored => "run",
                SessionStatus::Working => {
                    if info.is_some_and(HarnessInfo::steers_mid_turn) {
                        "steer"
                    } else {
                        "queue"
                    }
                }
                SessionStatus::AwaitingInput => anyhow::bail!(
                    "chat {} is waiting for an answer; use respond_to_input (or mode 'queue' to hold this message for after the turn)",
                    short(&chat.id)
                ),
            },
            "run" | "steer" | "queue" => mode,
            other => anyhow::bail!("unknown mode {other:?}; expected auto, run, steer, or queue"),
        };
        let id = match chosen {
            "run" => {
                let request = run_request(chat, space, harness, text);
                self.zeron
                    .queue_command(
                        &chat.id,
                        &SessionCommandPayload::Run {
                            request,
                            message_id: uuid::Uuid::new_v4().to_string(),
                        },
                    )
                    .await?
            }
            "steer" => {
                self.zeron
                    .queue_command(
                        &chat.id,
                        &SessionCommandPayload::Steer {
                            prompt: text,
                            message_id: Some(uuid::Uuid::new_v4().to_string()),
                        },
                    )
                    .await?
            }
            _ => self.zeron.queue_message(&chat.id, &text).await?,
        };
        Ok(json!({
            "delivery": chosen,
            "id": id,
            "chatStatusAtSend": status,
        }))
    }

    /// Wait, then report the outcome with the assistant messages that
    /// landed since `since_millis`.
    async fn await_turn(
        &self,
        chat: &Chat,
        baseline: Option<&Session>,
        expect_turn: bool,
        timeout: Duration,
        since_millis: i64,
    ) -> anyhow::Result<Value> {
        let (outcome, session) = self
            .zeron
            .wait_for_turn(chat, baseline, expect_turn, timeout)
            .await?;
        let entries = self.zeron.transcript(&chat.id).await.unwrap_or_default();
        let rendered = render_entries(&entries, RenderOptions::default());
        let replies: Vec<&RenderedMessage> = rendered
            .iter()
            .filter(|m| m.role == zeron_doc::MessageRole::Assistant)
            .filter(|m| m.created_at >= since_millis.saturating_sub(2_000))
            .collect();
        let replies: Vec<&RenderedMessage> = if replies.is_empty() {
            rendered
                .iter()
                .rev()
                .find(|m| m.role == zeron_doc::MessageRole::Assistant)
                .into_iter()
                .collect()
        } else {
            replies
        };
        let (status, _) = status_of(session.as_ref());
        Ok(json!({
            "outcome": outcome,
            "status": status,
            "timedOut": outcome == TurnOutcome::TimedOut,
            "pendingInput": last_pending_input(&rendered),
            "replies": replies,
        }))
    }
}

/// Which chats an agent-mode call may target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// Only chats the caller created (`parentChatId` = the caller).
    Child,
    /// Those, or the caller itself (reads).
    SelfOrChild,
}

/// Whether the token's Chat may reach `chat`: one it created (Zeron-created
/// children carry the creating Chat as `parentChatId`), or itself.
fn reachable(origin: &AgentOrigin, chat: &Chat, allow_self: bool) -> bool {
    chat.parent_chat_id.as_deref() == Some(origin.chat_id.as_str())
        || (allow_self && chat.id == origin.chat_id)
}

/// Permissiveness order of sandbox levels.
fn sandbox_rank(level: SandboxLevel) -> u8 {
    match level {
        SandboxLevel::ReadOnly => 0,
        SandboxLevel::WorkspaceWrite => 1,
        SandboxLevel::DangerFullAccess => 2,
    }
}

fn sandbox_name(level: SandboxLevel) -> &'static str {
    match level {
        SandboxLevel::ReadOnly => "read-only",
        SandboxLevel::WorkspaceWrite => "workspace-write",
        SandboxLevel::DangerFullAccess => "danger-full-access",
    }
}

/// Whether `path` is absolute, has no `..`, and lies at or under one of
/// `roots` (component-wise).
fn path_within_any(path: &str, roots: &[Option<&str>]) -> bool {
    let path = std::path::Path::new(path);
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return false;
    }
    roots
        .iter()
        .flatten()
        .map(std::path::Path::new)
        .any(|root| root.is_absolute() && path.starts_with(root))
}

/// The Run a composer send would queue for `chat` (its own config and cwd;
/// the host overwrites the cwd of an execution-bound Chat).
fn run_request(chat: &Chat, space: Option<&Space>, harness: HarnessId, text: String) -> RunRequest {
    let config = chat.config.clone();
    let cwd = chat
        .cwd
        .clone()
        .or_else(|| space.map(|s| s.path.clone()))
        .unwrap_or_else(|| "~".into());
    RunRequest {
        prompt: text,
        harness: Some(harness),
        model: config.as_ref().and_then(|c| c.model.clone()),
        reasoning: config.as_ref().and_then(|c| c.reasoning),
        model_options: config
            .as_ref()
            .map(|c| c.model_options.clone())
            .unwrap_or_default(),
        cwd,
        sandbox: config
            .as_ref()
            .map(|c| c.sandbox)
            .unwrap_or(SandboxLevel::WorkspaceWrite),
        // The chat's own approval mode (default Ask).
        auto_approve: config.as_ref().is_some_and(|c| c.auto_approve),
        resume: None,
        attachments: Vec::new(),
        worktree: None,
        instructions: None,
    }
}

/// claude-code when it is offered here, else the first available harness.
fn default_harness(harnesses: &[HarnessInfo]) -> anyhow::Result<HarnessId> {
    if harnesses.is_empty() {
        return Ok(HarnessId::ClaudeCode);
    }
    harnesses
        .iter()
        .find(|h| h.id == HarnessId::ClaudeCode && h.available())
        .or_else(|| {
            harnesses
                .iter()
                .find(|h| h.available() && h.id != HarnessId::Mock)
        })
        .map(|h| h.id)
        .ok_or_else(|| {
            anyhow::anyhow!("no harness is available on this device (see list_harnesses)")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::StreamExt;
    use std::sync::Mutex;
    use zeron_rpc::{RpcError, RpcReply, RpcService, memory_client, methods};

    const BETA_TOKEN: &str = "tok-beta";

    /// A fixed little workspace: one device, one project, two chats with a
    /// two-message transcript, and one live agent token (`tok-beta`, minted
    /// for chat-beta-2). Writes are recorded; created chats join the list.
    #[derive(Default)]
    struct World {
        writes: Mutex<Vec<(String, Value)>>,
        created: Mutex<Vec<Value>>,
        /// `create_chat` claims: idempotency key → inputs hash.
        claims: Mutex<std::collections::HashMap<String, String>>,
    }

    fn stream(item: Value) -> RpcReply {
        RpcReply::Stream(futures::stream::iter(vec![item]).boxed())
    }

    impl World {
        fn agent_calls(&self, tool: &str) -> Vec<Value> {
            self.writes_of(agent_tools::TOOL_CALL_METHOD)
                .into_iter()
                .filter(|p| p["tool"] == tool)
                .collect()
        }

        fn writes_of(&self, method: &str) -> Vec<Value> {
            self.writes
                .lock()
                .unwrap()
                .iter()
                .filter(|(m, _)| m == method)
                .map(|(_, p)| p.clone())
                .collect()
        }
    }

    #[async_trait]
    impl RpcService for World {
        async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
            Ok(match method {
                methods::LOCAL_DEVICE => RpcReply::Value(json!({ "deviceId": "dev-local" })),
                methods::ENGINE_INFO => RpcReply::Value(json!({
                    "deviceId": "dev-local", "workspaceScope": "local"
                })),
                methods::WATCH_DEVICES => stream(json!([{
                    "id": "dev-local", "name": "Laptop", "platform": "linux",
                    "lastSeenAt": null
                }, {
                    "id": "dev-other", "name": "Desktop", "platform": "linux",
                    "lastSeenAt": null
                }])),
                methods::WATCH_SPACES => stream(json!([{
                    "id": "space-1", "deviceId": "dev-local", "path": "/repo/comet",
                    "gitDetected": true, "createdAt": "2026-09-01T00:00:00Z"
                }, {
                    "id": "space-2", "deviceId": "dev-local", "path": "/repo/nova",
                    "gitDetected": true, "createdAt": "2026-09-01T00:00:00Z"
                }])),
                methods::WATCH_CHATS => {
                    let mut chats = vec![
                        json!({
                            "id": "chat-alpha-1", "deviceId": "dev-local", "title": "Alpha",
                            "archived": false, "spaceId": "space-1",
                            "config": { "harness": "claude-code", "model": "opus", "reasoning": null, "sandbox": "workspace-write" },
                            "createdAt": "2026-09-01T00:00:00Z"
                        }),
                        json!({
                            "id": "chat-beta-2", "deviceId": "dev-local", "title": "Beta",
                            "archived": false, "spaceId": "space-1",
                            "createdAt": "2026-09-02T00:00:00Z"
                        }),
                    ];
                    chats.extend(self.created.lock().unwrap().iter().cloned());
                    stream(Value::Array(chats))
                }
                methods::WATCH_SESSIONS => stream(json!([])),
                methods::LIST_HARNESSES => RpcReply::Value(json!([
                    { "id": "claude-code", "name": "Claude Code", "supportsSteering": true,
                      "steeringMode": "step-boundary", "reasoningLevels": [], "installed": true, "enabled": true },
                    { "id": "codex", "name": "Codex", "supportsSteering": true,
                      "steeringMode": "turn-boundary", "reasoningLevels": [], "installed": false, "enabled": true }
                ])),
                methods::LIST_MODELS => RpcReply::Value(json!([
                    { "id": "opus", "label": "Opus" }, { "id": "sonnet", "label": "Sonnet" }
                ])),
                methods::WATCH_DOC_MESSAGES => stream(json!({ "reset": [
                    { "id": "u1", "role": "user", "createdAt": 1, "deviceId": "dev-local",
                      "parts": [{ "kind": "text", "id": "t", "text": "hi" }] },
                    { "id": "a1", "role": "assistant", "createdAt": 2, "deviceId": "dev-local",
                      "status": "complete",
                      "parts": [{ "kind": "text", "id": "t", "text": "hello back" }] }
                ]})),
                agent_tools::AUTHENTICATE_METHOD => {
                    if params["token"] != BETA_TOKEN {
                        return Err(RpcError::Failed(
                            "agent-tools/unauthenticated: unknown token".into(),
                        ));
                    }
                    RpcReply::Value(json!({
                        "chatId": "chat-beta-2", "runId": "run-1",
                        "spaceId": "space-1", "deviceId": "dev-local"
                    }))
                }
                agent_tools::TOOL_CALL_METHOD => {
                    if params["token"] != BETA_TOKEN {
                        return Err(RpcError::Failed(
                            "agent-tools/unauthenticated: unknown token".into(),
                        ));
                    }
                    self.writes
                        .lock()
                        .unwrap()
                        .push((method.to_owned(), params.clone()));
                    if params["tool"] == agent_tool_names::ARTIFACT_RENDER_CHECK {
                        return Ok(RpcReply::Value(json!({
                            "artifactId": "a1", "errors": [], "warnings": ["overflow"],
                            "screenshots": [
                                { "viewport": "narrow", "width": 800, "height": 800,
                                  "theme": "light", "png": "iVBORw0KGgo=" },
                                { "viewport": "wide", "width": 1440, "height": 900,
                                  "theme": "dark", "png": "AAAA" },
                            ],
                            "screenshot": { "mimeType": "image/png", "data": "iVBORw0KGgo=" },
                        })));
                    }
                    if params["tool"] == agent_tool_names::CLAIM_CHILD_CHAT {
                        let key = params["arguments"]["idempotencyKey"].as_str().unwrap();
                        let hash = params["arguments"]["inputsHash"].as_str().unwrap();
                        let mut claims = self.claims.lock().unwrap();
                        return match claims.get(key) {
                            Some(stored) if stored != hash => Err(RpcError::Failed(
                                "work-outline/operationConflict: different inputs".into(),
                            )),
                            Some(_) => Ok(RpcReply::Value(json!({ "replayed": true }))),
                            None => {
                                claims.insert(key.to_owned(), hash.to_owned());
                                Ok(RpcReply::Value(json!({ "replayed": false })))
                            }
                        };
                    }
                    if params["tool"] == agent_tool_names::LAUNCH_CHILD_CHAT {
                        // The engine's launch: derives the same Chat id as the
                        // MCP server and starts it parented to the caller.
                        let key = params["arguments"]["idempotencyKey"].as_str().unwrap();
                        let operation = agent_tools::operation_id("chat-beta-2", key);
                        let chat_id = zeron_proto::derive_id(&operation, zeron_proto::roles::CHAT);
                        let mut created = self.created.lock().unwrap();
                        if !created.iter().any(|c| c["id"] == chat_id) {
                            created.push(json!({
                                "id": chat_id, "deviceId": "dev-local", "archived": false,
                                "spaceId": "space-1", "parentChatId": "chat-beta-2",
                                "config": params["arguments"]["config"],
                                "cwd": "/repo/comet/.worktrees/item",
                                "createdAt": "2026-09-03T00:00:00Z"
                            }));
                        }
                        return Ok(RpcReply::Value(json!({
                            "chatId": chat_id, "messageId": "m-1", "commandId": "cmd-launch",
                            "executionRoot": "/repo/comet/.worktrees/item", "state": "queued"
                        })));
                    }
                    RpcReply::Value(json!({ "commandId": "cmd-agent", "queued": true }))
                }
                methods::MUTATE | methods::QUEUE_COMMAND | methods::QUEUE_MESSAGE => {
                    if params["op"] == "createChat"
                        && !self
                            .created
                            .lock()
                            .unwrap()
                            .iter()
                            .any(|c| c["id"] == params["chatId"])
                    {
                        self.created.lock().unwrap().push(json!({
                            "id": params["chatId"], "deviceId": params["deviceId"],
                            "archived": false, "spaceId": params["spaceId"],
                            "parentChatId": params["parentChatId"],
                            "config": params["config"], "cwd": params["cwd"],
                            "createdAt": "2026-09-03T00:00:00Z"
                        }));
                    }
                    self.writes
                        .lock()
                        .unwrap()
                        .push((method.to_owned(), params));
                    RpcReply::Value(json!({ "commandId": "cmd-1", "id": "q-1" }))
                }
                other => return Err(RpcError::UnknownMethod(other.into())),
            })
        }
    }

    fn tools(world: Arc<World>, token: Option<&str>) -> Tools {
        let client = memory_client(world);
        Tools::new(Arc::new(Zeron::with_client(
            client,
            token.map(str::to_owned),
        )))
    }

    #[tokio::test]
    async fn catalog_is_well_formed() {
        let mut defs = catalog();
        defs.extend(work_outline_catalog());
        let mut names: Vec<&str> = defs.iter().map(|d| d.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), defs.len(), "tool names must be unique");
        for def in &defs {
            assert_eq!(def.input_schema["type"], "object", "{}", def.name);
            assert!(!def.description.is_empty());
        }
        // Every mutating Work Outline tool takes an idempotency key; only the
        // create-style ones demand it (the rest derive one when omitted).
        for def in work_outline_catalog() {
            if def.name != agent_tool_names::READ_CURRENT
                && def.name != agent_tool_names::READ_SUBTREE
            {
                assert!(
                    def.input_schema["properties"]["idempotencyKey"].is_object(),
                    "{}",
                    def.name
                );
                let required = def.input_schema["required"].as_array().unwrap();
                let demands_key = required.contains(&json!("idempotencyKey"));
                assert_eq!(
                    demands_key,
                    [
                        agent_tool_names::CREATE_CHILD,
                        agent_tool_names::PROPOSE_INSTRUCTION
                    ]
                    .contains(&def.name),
                    "{}",
                    def.name
                );
            }
        }
        let wire: Vec<&str> = work_outline_catalog().iter().map(|d| d.name).collect();
        assert_eq!(wire, agent_tool_names::WORK_OUTLINE);
        let artifacts: Vec<&str> = artifact_catalog().iter().map(|d| d.name).collect();
        assert_eq!(artifacts, agent_tool_names::ARTIFACTS);
        let publish = &artifact_catalog()[0];
        let required = publish.input_schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("idempotencyKey")));
    }

    #[tokio::test]
    async fn bind_chat_worktree_is_an_in_chat_engine_tool() {
        let def = diff_target_catalog().remove(0);
        assert_eq!(def.name, "bind_chat_worktree");
        assert_eq!(def.input_schema["type"], "object");
        let required = def.input_schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("path")));
        // The key is optional: binding the same path again just re-selects it.
        assert!(!required.contains(&json!("idempotencyKey")));
        assert!(def.input_schema["properties"]["idempotencyKey"].is_object());
        assert!(def.input_schema["properties"].get("chatId").is_none());
        assert!(def.description.contains("git worktree add"));
        assert!(def.description.contains("resume"));
        assert!(def.description.contains("submodule"));
        assert!(def.description.len() < 1000, "{}", def.description.len());

        let world = Arc::new(World::default());
        let anonymous = tools(world.clone(), None);
        assert!(!anonymous.has(agent_tool_names::BIND_CHAT_WORKTREE));
        let agent = tools(world.clone(), Some(BETA_TOKEN));
        assert!(agent.has(agent_tool_names::BIND_CHAT_WORKTREE));
        let mut names: Vec<&str> = agent.list().iter().map(|d| d.name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "tool names must be unique");

        // Forwarded to the engine as an engine tool with the token.
        agent
            .call(
                agent_tool_names::BIND_CHAT_WORKTREE,
                json!({ "path": "/tmp/wt", "idempotencyKey": "k" }),
            )
            .await
            .unwrap();
        let calls = world.agent_calls(agent_tool_names::BIND_CHAT_WORKTREE);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["token"], BETA_TOKEN);
        assert_eq!(calls[0]["arguments"]["path"], "/tmp/wt");
    }

    #[test]
    fn get_plan_documents_the_answers_the_agent_must_fold_in() {
        let def = artifact_catalog()
            .into_iter()
            .find(|d| d.name == agent_tool_names::ARTIFACT_GET_PLAN)
            .expect("artifact_get_plan is defined");
        assert!(def.description.contains("answeredQuestions"));
        assert!(def.description.contains("status: resolved"));
    }

    #[tokio::test]
    async fn artifact_tools_need_a_token_and_return_the_screenshot_as_an_image() {
        let world = Arc::new(World::default());
        let anonymous = tools(world.clone(), None);
        assert!(!anonymous.has(agent_tool_names::ARTIFACT_PUBLISH));
        let agent = tools(world.clone(), Some(BETA_TOKEN));
        for name in agent_tool_names::ARTIFACTS {
            assert!(agent.has(name), "{name}");
        }
        agent
            .call(
                agent_tool_names::ARTIFACT_PUBLISH,
                json!({ "path": "plan.md", "title": "Plan", "note": "n",
                        "idempotencyKey": "k" }),
            )
            .await
            .unwrap();
        let calls = world.agent_calls(agent_tool_names::ARTIFACT_PUBLISH);
        assert_eq!(calls[0]["token"], BETA_TOKEN);
        assert_eq!(calls[0]["arguments"]["path"], "plan.md");

        let response = crate::jsonrpc::handle_request(
            &agent,
            json!(7),
            "tools/call",
            json!({ "name": agent_tool_names::ARTIFACT_RENDER_CHECK,
                    "arguments": { "artifactId": "a1" } }),
        )
        .await;
        let content = response["result"]["content"].as_array().unwrap();
        // The JSON, then a label + image per screenshot.
        assert_eq!(content.len(), 5, "{response}");
        assert_eq!(content[0]["type"], "text");
        assert!(
            !content[0]["text"]
                .as_str()
                .unwrap()
                .contains("iVBORw0KGgo=")
        );
        assert_eq!(content[1]["type"], "text");
        assert_eq!(
            content[1]["text"],
            "Screenshot: narrow (800x800), light theme"
        );
        assert_eq!(content[2]["type"], "image");
        assert_eq!(content[2]["mimeType"], "image/png");
        assert_eq!(content[2]["data"], "iVBORw0KGgo=");
        assert_eq!(
            content[3]["text"],
            "Screenshot: wide (1440x900), dark theme"
        );
        assert_eq!(content[4]["type"], "image");
        assert_eq!(content[4]["data"], "AAAA");
        let structured = &response["result"]["structuredContent"];
        assert_eq!(structured["warnings"][0], "overflow");
        assert_eq!(structured["screenshot"]["attached"], true);
        assert_eq!(structured["screenshots"][1]["attached"], true);
        assert!(structured["screenshots"][1].get("png").is_none());
    }

    #[test]
    fn render_check_content_falls_back_to_the_single_screenshot() {
        let (value, content) = render_check_content(json!({
            "errors": [],
            "screenshot": { "mimeType": "image/png", "data": "QUJD" },
        }));
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["data"], "QUJD");
        assert_eq!(value["screenshot"]["attached"], true);
    }

    #[test]
    fn render_check_schema_lists_viewports_and_themes() {
        let catalog = artifact_catalog();
        let tool = catalog
            .iter()
            .find(|t| t.name == agent_tool_names::ARTIFACT_RENDER_CHECK)
            .unwrap();
        let props = &tool.input_schema["properties"];
        assert_eq!(props["viewports"]["type"], "array");
        assert_eq!(
            props["viewports"]["items"]["required"],
            json!(["width", "height"])
        );
        assert!(
            props["viewports"]["items"]["properties"]
                .get("label")
                .is_some()
        );
        assert_eq!(props["themes"]["items"]["enum"], json!(["light", "dark"]));
        assert!(props.get("viewport").is_some() && props.get("theme").is_some());
    }

    #[tokio::test]
    async fn list_and_read_chats() {
        let world = Arc::new(World::default());
        let tools = tools(world, None);
        let listed = tools.call("list_chats", json!({})).await.unwrap();
        assert_eq!(listed["total"], 2);
        // Newest activity first: Beta was created later.
        assert_eq!(listed["chats"][0]["title"], "Beta");
        assert_eq!(listed["chats"][1]["project"]["name"], "comet");
        assert_eq!(listed["chats"][1]["status"], "idle");

        let read = tools
            .call("read_chat", json!({ "chat": "alpha" }))
            .await
            .unwrap();
        assert_eq!(read["total"], 2);
        assert_eq!(read["messages"][1]["text"], "hello back");

        let read = tools
            .call("read_chat", json!({ "chat": "chat-alpha", "limit": 1 }))
            .await
            .unwrap();
        assert_eq!(read["returned"], 1);
        assert_eq!(read["olderRemaining"], 1);
        assert_eq!(read["messages"][0]["id"], "a1");
    }

    #[tokio::test]
    async fn send_attributes_and_refuses_self() {
        let world = Arc::new(World::default());
        let tools = tools(world.clone(), Some(BETA_TOKEN));
        let err = tools
            .call("send_message", json!({ "chat": "beta", "text": "loop" }))
            .await
            .unwrap_err();
        assert!(err.contains("own chat"), "{err}");

        // An agent reaches only chats it created.
        let err = tools
            .call("send_message", json!({ "chat": "alpha", "text": "hi" }))
            .await
            .unwrap_err();
        assert!(err.contains("agents can only reach"), "{err}");
        let child = tools
            .call(
                "create_chat",
                json!({ "project": "comet", "model": "opus", "idempotency_key": "reviewer" }),
            )
            .await
            .unwrap();
        let child = child["chatId"].as_str().unwrap().to_owned();

        let sent = tools
            .call(
                "send_message",
                json!({ "chat": child, "text": "please review" }),
            )
            .await
            .unwrap();
        assert_eq!(sent["sent"]["delivery"], "run");
        let writes = world.writes_of(methods::QUEUE_COMMAND);
        let params = writes.last().expect("a queued command");
        assert_eq!(params["chatId"], child);
        assert_eq!(params["command"]["kind"], "run");
        let prompt = params["command"]["request"]["prompt"].as_str().unwrap();
        assert!(
            prompt.starts_with("[Message from Mast chat Beta (chat-bet)"),
            "{prompt}"
        );
        assert!(prompt.ends_with("please review"));
        assert_eq!(params["command"]["request"]["cwd"], "/repo/comet");
        assert_eq!(params["command"]["request"]["harness"], "claude-code");
        assert_eq!(params["command"]["request"]["model"], "opus");
    }

    #[tokio::test]
    async fn terminal_sends_reach_any_chat() {
        let world = Arc::new(World::default());
        let tools = tools(world.clone(), None);
        for (name, args) in [
            ("send_message", json!({ "chat": "alpha", "text": "hi" })),
            ("get_chat", json!({ "chat": "beta" })),
            ("interrupt_chat", json!({ "chat": "alpha" })),
        ] {
            tools.call(name, args).await.unwrap();
        }
        assert_eq!(
            tools.call("list_projects", json!({})).await.unwrap()["projects"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn agent_calls_reach_only_their_own_chat_space_and_device() {
        let world = Arc::new(World::default());
        let tools = tools(world.clone(), Some(BETA_TOKEN));
        let child = tools
            .call(
                "create_chat",
                json!({ "idempotency_key": "child", "title": "Kid" }),
            )
            .await
            .unwrap();
        assert_eq!(
            child["project"]["id"], "space-1",
            "defaults to its own project"
        );
        let child = child["chatId"].as_str().unwrap().to_owned();

        // Allow / deny matrix over the chat-targeting tools: alpha is
        // someone else's chat, beta is the caller, child is its own child.
        let matrix: [(&str, Value, [bool; 3]); 7] = [
            ("get_chat", json!({}), [false, true, true]),
            ("read_chat", json!({}), [false, true, true]),
            (
                "wait_for_turn",
                json!({ "timeout_secs": 1 }),
                [false, false, true],
            ),
            ("interrupt_chat", json!({}), [false, false, true]),
            (
                "respond_to_input",
                json!({ "request_id": "r1",
                        "answers": [{ "question_id": "q", "labels": ["Yes"] }] }),
                [false, false, true],
            ),
            (
                "send_message",
                json!({ "text": "hi" }),
                [false, false, true],
            ),
            ("archive_chat", json!({}), [false, false, true]),
        ];
        for (name, extra, allowed) in matrix {
            for (target, allow) in ["chat-alpha-1", "chat-beta-2", child.as_str()]
                .into_iter()
                .zip(allowed)
            {
                let mut args = extra.clone();
                args["chat"] = json!(target);
                let result = tools.call(name, args).await;
                assert_eq!(result.is_ok(), allow, "{name} -> {target}: {result:?}");
            }
        }
        // A foreign chat cannot be found by title either.
        let err = tools
            .call("read_chat", json!({ "chat": "Alpha" }))
            .await
            .unwrap_err();
        assert!(err.contains("agents can only reach"), "{err}");

        // Lists show only what the token reaches.
        let listed = tools
            .call("list_chats", json!({ "include_archived": true }))
            .await
            .unwrap();
        let ids: Vec<&str> = listed["chats"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), 2, "{ids:?}");
        assert!(ids.contains(&"chat-beta-2") && ids.contains(&child.as_str()));
        let projects = tools.call("list_projects", json!({})).await.unwrap();
        assert_eq!(projects["projects"].as_array().unwrap().len(), 1);
        assert_eq!(projects["projects"][0]["id"], "space-1");
        let devices = tools.call("list_devices", json!({})).await.unwrap();
        assert_eq!(devices["devices"].as_array().unwrap().len(), 1);
        assert_eq!(devices["devices"][0]["id"], "dev-local");
        assert!(tools.call("list_harnesses", json!({})).await.is_ok());
    }

    #[tokio::test]
    async fn agent_create_chat_stays_in_its_space_device_folder_and_sandbox() {
        let world = Arc::new(World::default());
        let tools = tools(world.clone(), Some(BETA_TOKEN));
        let refused = [
            (json!({ "project": "nova" }), "own project"),
            (json!({ "device": "Desktop" }), "own device"),
            (
                json!({ "sandbox": "danger-full-access" }),
                "more permissive",
            ),
            (json!({ "cwd": "/etc" }), "outside"),
            (json!({ "cwd": "/repo/comet/../nova" }), "outside"),
            (json!({ "cwd": "relative/path" }), "outside"),
        ];
        for (i, (mut args, why)) in refused.into_iter().enumerate() {
            args["idempotency_key"] = json!(format!("deny-{i}"));
            let err = tools.call("create_chat", args.clone()).await.unwrap_err();
            assert!(err.contains(why), "{args}: {err}");
        }
        assert!(
            world.writes_of(methods::MUTATE).is_empty(),
            "nothing created"
        );

        // Within bounds: read-only and an in-project worktree are fine; the
        // default sandbox is capped at the parent's (workspace-write here).
        let ok = tools
            .call(
                "create_chat",
                json!({ "idempotency_key": "ro", "sandbox": "read-only",
                        "cwd": "/repo/comet/.worktrees/x" }),
            )
            .await
            .unwrap();
        assert_eq!(ok["deviceId"], "dev-local");
        tools
            .call("create_chat", json!({ "idempotency_key": "default" }))
            .await
            .unwrap();
        let creates: Vec<Value> = world
            .writes_of(methods::MUTATE)
            .into_iter()
            .filter(|m| m["op"] == "createChat")
            .collect();
        assert_eq!(creates[0]["config"]["sandbox"], "read-only");
        assert_eq!(creates[0]["cwd"], "/repo/comet/.worktrees/x");
        assert_eq!(creates[1]["config"]["sandbox"], "workspace-write");
        for create in &creates {
            assert_eq!(create["config"]["autoApprove"], false);
            assert_eq!(create["spaceId"], "space-1");
            assert_eq!(create["parentChatId"], "chat-beta-2");
        }
    }

    #[tokio::test]
    async fn create_chat_writes_the_row_and_validates_model() {
        let world = Arc::new(World::default());
        let tools = tools(world.clone(), None);
        let err = tools
            .call(
                "create_chat",
                json!({ "project": "comet", "model": "nope" }),
            )
            .await
            .unwrap_err();
        assert!(err.contains("not offered"), "{err}");

        let created = tools
            .call(
                "create_chat",
                json!({ "project": "/repo/comet", "model": "sonnet", "title": "Review", "prompt": "go" }),
            )
            .await
            .unwrap();
        let chat_id = created["chatId"].as_str().unwrap().to_owned();
        let writes = world.writes.lock().unwrap();
        assert_eq!(writes.len(), 3, "createChat, renameChat, run");
        assert_eq!(writes[0].1["op"], "createChat");
        assert_eq!(writes[0].1["chatId"], chat_id);
        assert_eq!(writes[0].1["spaceId"], "space-1");
        assert!(
            writes[0].1.get("parentChatId").is_none(),
            "no origin, no parent"
        );
        assert_eq!(writes[0].1["config"]["harness"], "claude-code");
        assert_eq!(writes[0].1["config"]["model"], "sonnet");
        assert_eq!(writes[1].1["op"], "renameChat");
        assert_eq!(writes[2].0, methods::QUEUE_COMMAND);
        assert_eq!(writes[2].1["command"]["request"]["prompt"], "go");
    }

    #[tokio::test]
    async fn create_chat_without_a_model_resolves_to_the_mast_default() {
        for blank in [None, Some(""), Some("  ")] {
            let world = Arc::new(World::default());
            let tools = tools(world.clone(), None);
            let mut args = json!({ "project": "/repo/comet", "prompt": "go" });
            if let Some(blank) = blank {
                args["model"] = json!(blank);
            }
            let created = tools.call("create_chat", args).await.unwrap();
            assert_eq!(created["model"], "claude-opus-5-5");
            let writes = world.writes.lock().unwrap();
            assert_eq!(writes[0].1["config"]["model"], "claude-opus-5-5");
            let run = writes.last().unwrap();
            assert_eq!(run.0, methods::QUEUE_COMMAND);
            assert_eq!(run.1["command"]["request"]["model"], "claude-opus-5-5");
        }
    }

    #[tokio::test]
    async fn agent_create_chat_needs_a_key_and_retries_return_the_same_chat() {
        let world = Arc::new(World::default());
        let tools = tools(world.clone(), Some(BETA_TOKEN));
        let err = tools
            .call("create_chat", json!({ "project": "/repo/comet" }))
            .await
            .unwrap_err();
        assert!(err.contains("idempotencyKey is required"), "{err}");
        // The error lists the parameters and shows an example call.
        assert!(err.contains("Usage of create_chat:"), "{err}");
        assert!(err.contains("required: idempotencyKey"), "{err}");
        assert!(err.contains("optional: project"), "{err}");
        assert!(err.contains("example: {\"workItemId\""), "{err}");

        let args = json!({ "project": "/repo/comet", "title": "Child", "prompt": "go",
                           "idempotency_key": "spawn-reviewer" });
        let first = tools.call("create_chat", args.clone()).await.unwrap();
        let chat_id = first["chatId"].as_str().unwrap().to_owned();
        let op = agent_tools::operation_id("chat-beta-2", "spawn-reviewer");
        assert_eq!(
            chat_id,
            zeron_proto::derive_id(&op, zeron_proto::roles::CHAT)
        );
        assert_eq!(first["parentChatId"], "chat-beta-2");
        assert_eq!(first["replayed"], false);
        assert_eq!(first["sent"]["id"], "cmd-agent");

        let again = tools.call("create_chat", args.clone()).await.unwrap();
        assert_eq!(again["chatId"], chat_id);
        assert_eq!(again["replayed"], true);

        // The same key with different inputs is a conflict, never the old chat.
        let mut changed = args.clone();
        changed["prompt"] = json!("something else");
        let err = tools.call("create_chat", changed).await.unwrap_err();
        assert!(err.contains("operationConflict"), "{err}");

        // One rename only (a replay never overwrites the title); every
        // createChat names the same derived id; both first Turns go through
        // the engine's key-derived, deduping queue (never QueueCommand).
        let mutates = world.writes_of(methods::MUTATE);
        assert_eq!(
            mutates.iter().filter(|m| m["op"] == "renameChat").count(),
            1
        );
        assert!(
            mutates
                .iter()
                .filter(|m| m["op"] == "createChat")
                .all(|m| m["chatId"] == chat_id && m["parentChatId"] == "chat-beta-2")
        );
        assert!(world.writes_of(methods::QUEUE_COMMAND).is_empty());
        let turns = world.agent_calls(agent_tool_names::QUEUE_CHILD_CHAT_TURN);
        assert_eq!(turns.len(), 2);
        for turn in &turns {
            assert_eq!(turn["token"], BETA_TOKEN);
            assert_eq!(turn["tool"], agent_tool_names::QUEUE_CHILD_CHAT_TURN);
            assert_eq!(turn["arguments"]["chatId"], chat_id);
            assert_eq!(turn["arguments"]["idempotencyKey"], "spawn-reviewer");
            // Only the prompt: the engine rebuilds the Run from the child.
            assert_eq!(turn["arguments"]["prompt"], "go");
            assert!(turn["arguments"].get("command").is_none());
        }

        // An agent may not hang a child under another chat.
        let err = tools
            .call(
                "create_chat",
                json!({ "project": "/repo/comet", "parent": "Alpha", "idempotency_key": "k2" }),
            )
            .await
            .unwrap_err();
        assert!(err.contains("own chat"), "{err}");
    }

    #[tokio::test]
    async fn agent_create_chat_with_a_work_item_launches_through_the_engine() {
        let world = Arc::new(World::default());
        let tools = tools(world.clone(), Some(BETA_TOKEN));
        let created = tools
            .call(
                "create_chat",
                json!({
                    "idempotencyKey": "leaf-1", "workItemId": "item-leaf",
                    "prompt": "do the leaf", "title": "Leaf worker",
                }),
            )
            .await
            .unwrap();
        let chat_id = created["chatId"].as_str().unwrap().to_owned();
        assert_eq!(created["workItemId"], "item-leaf");
        assert_eq!(created["parentChatId"], "chat-beta-2");
        assert_eq!(created["executionRoot"], "/repo/comet/.worktrees/item");
        assert_eq!(created["sent"]["id"], "cmd-launch");
        assert_eq!(created["replayed"], false);

        // One launch call (token, item, prompt, config), and none of the
        // plain-chat writes: no createChat row, no separate first turn.
        let launches = world.agent_calls(agent_tool_names::LAUNCH_CHILD_CHAT);
        assert_eq!(launches.len(), 1);
        assert_eq!(launches[0]["token"], BETA_TOKEN);
        let arguments = &launches[0]["arguments"];
        assert_eq!(arguments["workItemId"], "item-leaf");
        assert_eq!(arguments["prompt"], "do the leaf");
        assert_eq!(arguments["idempotencyKey"], "leaf-1");
        assert_eq!(arguments["config"]["autoApprove"], false);
        assert_eq!(arguments["config"]["sandbox"], "workspace-write");
        assert!(
            world
                .writes_of(methods::MUTATE)
                .iter()
                .all(|m| m["op"] != "createChat")
        );
        assert!(
            world
                .agent_calls(agent_tool_names::QUEUE_CHILD_CHAT_TURN)
                .is_empty()
        );
        // The key was claimed against inputs that include the item.
        assert_eq!(
            world.agent_calls(agent_tool_names::CLAIM_CHILD_CHAT).len(),
            1
        );

        // A retry replays the same chat and does not rename it again.
        let again = tools
            .call(
                "create_chat",
                json!({
                    "idempotencyKey": "leaf-1", "workItemId": "item-leaf",
                    "prompt": "do the leaf", "title": "Leaf worker",
                }),
            )
            .await
            .unwrap();
        assert_eq!(again["chatId"], chat_id);
        assert_eq!(again["replayed"], true);
        // Another item under the same key is a conflict, never the old chat.
        let err = tools
            .call(
                "create_chat",
                json!({
                    "idempotencyKey": "leaf-1", "workItemId": "item-other",
                    "prompt": "do the leaf", "title": "Leaf worker",
                }),
            )
            .await
            .unwrap_err();
        assert!(err.contains("operationConflict"), "{err}");

        // workItemId needs a prompt and takes its folder from the item.
        for (extra, expect) in [
            (json!({}), "needs a prompt"),
            (
                json!({ "prompt": "x", "cwd": "/repo/comet" }),
                "cwd and branch",
            ),
            (json!({ "prompt": "x", "branch": "b" }), "cwd and branch"),
        ] {
            let mut args = json!({ "idempotencyKey": "bad", "workItemId": "item-leaf" });
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let err = tools.call("create_chat", args).await.unwrap_err();
            assert!(err.contains(expect), "{err}");
        }

        // A terminal (no token) cannot verify the item is its own.
        let terminal = super::tests::tools(world, None);
        let err = terminal
            .call(
                "create_chat",
                json!({ "project": "/repo/comet", "workItemId": "item-leaf", "prompt": "x" }),
            )
            .await
            .unwrap_err();
        assert!(err.contains("only available inside a Mast chat"), "{err}");
    }

    #[tokio::test]
    async fn terminal_create_chat_records_an_explicit_parent() {
        let world = Arc::new(World::default());
        let tools = tools(world.clone(), None);
        let created = tools
            .call(
                "create_chat",
                json!({ "project": "/repo/comet", "parent": "Alpha" }),
            )
            .await
            .unwrap();
        assert_eq!(created["parentChatId"], "chat-alpha-1");
        let err = tools
            .call(
                "create_chat",
                json!({ "project": "/repo/comet", "parent": "nope" }),
            )
            .await
            .unwrap_err();
        assert!(err.contains("no chat matches"), "{err}");
    }

    #[tokio::test]
    async fn agents_archive_only_chats_they_created() {
        let world = Arc::new(World::default());
        let tools = tools(world.clone(), Some(BETA_TOKEN));
        let err = tools
            .call("archive_chat", json!({ "chat": "alpha" }))
            .await
            .unwrap_err();
        assert!(err.contains("only archive chats they created"), "{err}");
        let child = tools
            .call(
                "create_chat",
                json!({ "project": "/repo/comet", "idempotency_key": "child" }),
            )
            .await
            .unwrap();
        let archived = tools
            .call("archive_chat", json!({ "chat": child["chatId"] }))
            .await
            .unwrap();
        assert_eq!(archived["archived"], true);
    }

    #[tokio::test]
    async fn work_outline_tools_need_a_token_and_forward_it() {
        let world = Arc::new(World::default());
        let anonymous = tools(world.clone(), None);
        assert!(!anonymous.has(agent_tool_names::READ_CURRENT));
        assert!(
            anonymous
                .list()
                .iter()
                .all(|t| !t.name.starts_with("work_outline_"))
        );
        let err = anonymous
            .call(agent_tool_names::READ_CURRENT, json!({}))
            .await
            .unwrap_err();
        assert!(err.contains("only available to agents"), "{err}");

        let agent = tools(world.clone(), Some(BETA_TOKEN));
        assert!(agent.has(agent_tool_names::SET_STATUS));
        agent
            .call(
                agent_tool_names::SET_STATUS,
                json!({ "status": "active", "idempotencyKey": "k" }),
            )
            .await
            .unwrap();
        let calls = world.agent_calls(agent_tool_names::SET_STATUS);
        assert_eq!(calls[0]["token"], BETA_TOKEN);
        assert_eq!(calls[0]["tool"], agent_tool_names::SET_STATUS);
        assert_eq!(calls[0]["arguments"]["status"], "active");
        // The tool never sends a chat id: the engine derives it.
        assert!(calls[0]["arguments"].get("chatId").is_none());
    }

    /// Agents often call before the full schema has loaded and guess names:
    /// the server maps the common guesses before the engine (or `create_chat`)
    /// parses them, and an argument error says what to pass.
    #[tokio::test]
    async fn guessed_argument_names_are_normalised_before_dispatch() {
        let world = Arc::new(World::default());
        let agent = tools(world.clone(), Some(BETA_TOKEN));
        agent
            .call(
                agent_tool_names::CREATE_USER_ACTION_REQUEST,
                json!({ "question": "Which branch?", "dedupe_key": "branch" }),
            )
            .await
            .unwrap();
        agent
            .call(
                agent_tool_names::RESOLVE_USER_ACTION_REQUEST,
                json!({ "id": "req-1" }),
            )
            .await
            .unwrap();
        agent
            .call(
                agent_tool_names::SET_STATUS,
                json!({ "itemId": "item-1", "status": "completed" }),
            )
            .await
            .unwrap();
        let request = &world.agent_calls(agent_tool_names::CREATE_USER_ACTION_REQUEST)[0];
        assert_eq!(request["arguments"]["request"], "Which branch?");
        assert_eq!(request["arguments"]["dedupeKey"], "branch");
        assert!(request["arguments"].get("question").is_none());
        let resolve = &world.agent_calls(agent_tool_names::RESOLVE_USER_ACTION_REQUEST)[0];
        assert_eq!(resolve["arguments"]["userActionRequestId"], "req-1");
        let status = &world.agent_calls(agent_tool_names::SET_STATUS)[0];
        assert_eq!(status["arguments"]["workItemId"], "item-1");

        // create_chat: camelCase key and `workItem` reach the real fields.
        let created = agent
            .call(
                "create_chat",
                json!({ "idempotencyKey": "leaf-2", "workItem": "item-leaf", "prompt": "go" }),
            )
            .await
            .unwrap();
        assert_eq!(created["workItemId"], "item-leaf");
        let launch = &world.agent_calls(agent_tool_names::LAUNCH_CHILD_CHAT)[0];
        assert_eq!(launch["arguments"]["idempotencyKey"], "leaf-2");

        // A wrong value type is an argument error with the usage appended.
        let err = agent
            .call(
                "create_chat",
                json!({ "idempotency_key": "k", "wait": "yes" }),
            )
            .await
            .unwrap_err();
        assert!(err.starts_with("invalid arguments"), "{err}");
        assert!(err.contains("Usage of create_chat:"), "{err}");
    }

    #[tokio::test]
    async fn a_rejected_token_disables_every_tool() {
        let world = Arc::new(World::default());
        let tools = tools(world.clone(), Some("tok-forged"));
        for (name, args) in [
            ("whoami", json!({})),
            ("list_chats", json!({})),
            ("send_message", json!({ "chat": "alpha", "text": "hi" })),
            (agent_tool_names::READ_CURRENT, json!({})),
        ] {
            let err = tools.call(name, args).await.unwrap_err();
            assert!(
                err.contains("refused this agent's token")
                    || err.contains("agent-tools/unauthenticated"),
                "{name}: {err}"
            );
        }
        assert!(world.writes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn whoami_reports_the_engine_authenticated_origin() {
        let world = Arc::new(World::default());
        let agent = tools(world.clone(), Some(BETA_TOKEN));
        let me = agent.call("whoami", json!({})).await.unwrap();
        assert_eq!(me["chat"]["id"], "chat-beta-2");
        assert_eq!(me["authenticated"], true);
        assert_eq!(me["runId"], "run-1");
        let anonymous = tools(world, None);
        let me = anonymous.call("whoami", json!({})).await.unwrap();
        assert_eq!(me["chat"], Value::Null);
        assert_eq!(me["authenticated"], false);
    }

    #[tokio::test]
    async fn wait_on_an_idle_chat_returns_immediately() {
        let world = Arc::new(World::default());
        let tools = tools(world, None);
        let waited = tools
            .call(
                "wait_for_turn",
                json!({ "chat": "alpha", "timeout_secs": 5 }),
            )
            .await
            .unwrap();
        assert_eq!(waited["turn"]["outcome"], "completed");
        assert_eq!(waited["turn"]["replies"][0]["text"], "hello back");
    }

    #[tokio::test]
    async fn send_with_wait_keeps_waiting_until_a_turn_lands() {
        // The stub never grows a session row, so a send-then-wait must run
        // to its deadline rather than declaring the (unstarted) run done.
        let world = Arc::new(World::default());
        let tools = tools(world, None);
        let started = std::time::Instant::now();
        let sent = tools
            .call(
                "send_message",
                json!({ "chat": "alpha", "text": "hi", "wait": true, "timeout_secs": 1 }),
            )
            .await
            .unwrap();
        assert_eq!(sent["turn"]["outcome"], "timedOut");
        assert!(started.elapsed() >= Duration::from_millis(900));
    }

    #[tokio::test]
    async fn initialize_and_list_over_jsonrpc() {
        let world = Arc::new(World::default());
        let tools = tools(world, None);
        let init = crate::jsonrpc::handle_request(
            &tools,
            json!(1),
            "initialize",
            json!({ "protocolVersion": "2025-03-26" }),
        )
        .await;
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "zeron");
        let list =
            crate::jsonrpc::handle_request(&tools, json!(2), "tools/list", Value::Null).await;
        assert!(list["result"]["tools"].as_array().unwrap().len() >= 10);
        let bad = crate::jsonrpc::handle_request(
            &tools,
            json!(3),
            "tools/call",
            json!({ "name": "nope" }),
        )
        .await;
        assert_eq!(bad["error"]["code"], -32602);
        let whoami = crate::jsonrpc::handle_request(
            &tools,
            json!(4),
            "tools/call",
            json!({ "name": "whoami" }),
        )
        .await;
        assert_eq!(whoami["result"]["isError"], false);
        assert_eq!(
            whoami["result"]["structuredContent"]["localDeviceId"],
            "dev-local"
        );
    }
}
