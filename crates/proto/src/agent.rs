//! Agent-side wire types: harness identity, run requests, streaming events, tool calls.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HarnessId {
    ClaudeCode,
    Codex,
    Cursor,
    /// Cognition's Devin agent, driven over ACP (`devin acp`).
    Devin,
    /// xAI's Grok Build agent, driven over ACP (`grok agent stdio`).
    Grok,
    /// Nous Research's Hermes Agent, driven over ACP (`hermes acp`).
    Hermes,
    /// The pi coding agent (pi.dev), driven over ACP via the `pi-acp` adapter.
    Pi,
    /// SST's opencode agent, driven natively over its own HTTP/SSE server
    /// protocol (`opencode serve` — the same wire the opencode desktop app
    /// speaks).
    Opencode,
    /// google's antigravity agent over acp (`agy_acp_server`, installed from
    /// its pinned release archive).
    Antigravity,
    /// Test harness; never shown in production pickers.
    Mock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningLevel {
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
    Ultra,
    /// xhigh + harness-specific setting.
    Ultracode,
    /// Prompt-prefix driven (Claude).
    Ultrathink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxLevel {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SteeringMode {
    /// Steer delivered at the next step boundary within the live turn.
    StepBoundary,
    /// Steer delivered only between turns.
    TurnBoundary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub label: String,
    /// Short tagline rendered under the name in the model picker (11px muted),
    /// mirroring the Electron app's `ModelInfo.description`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub reasoning_levels: Vec<ReasoningLevel>,
    #[serde(default)]
    pub options: Vec<ModelOption>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOption {
    pub id: String,
    pub label: String,
    pub choices: Vec<ModelOptionChoice>,
    pub default_choice: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOptionChoice {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRequest {
    pub prompt: String,
    /// The harness picked at send time. Rides the command plane so
    /// claim-on-first-command (chat row still in flight on the registry
    /// channel) dispatches — and records — the picked harness instead of the
    /// engine default. Additive + serde-defaulted for wire compat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<HarnessId>,
    pub model: Option<String>,
    pub reasoning: Option<ReasoningLevel>,
    /// Harness-specific option selections (option id -> choice id), JSON round-tripped.
    #[serde(default)]
    pub model_options: serde_json::Map<String, serde_json::Value>,
    pub cwd: String,
    pub sandbox: SandboxLevel,
    #[serde(default)]
    pub auto_approve: bool,
    /// Harness-native session id to resume, if any.
    pub resume: Option<String>,
    /// Absolute paths of image attachments already staged on the run device
    /// (composer uploads: UploadChunk/UploadCommit → durable path). The same
    /// paths also ride the prompt text as `Attached images (local files …)`
    /// refs (zeron's `withAttachments` transport — that's what persists in the
    /// doc); this field additionally lets a harness inline the bytes as image
    /// content blocks. Additive + serde-defaulted for wire compat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
    /// Host-side isolated-worktree creation (see [`WorktreeSpec`]): when set,
    /// the HOST materializes the worktree at command-drain time and runs there
    /// instead of `cwd`. Additive + serde-defaulted for wire compat — an old
    /// host ignores it and runs in `cwd` (the repo's main checkout).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeSpec>,
    /// Work Outline runtime instructions, resolved by the HOST just before
    /// dispatch from the pinned revision (`m0-instruction-delivery.md` 5).
    /// Never part of `prompt`. Additive for serde, but only ever sent inside
    /// instruction-bearing commands so old hosts skip the command instead of
    /// running without instructions. An adapter that cannot deliver it must
    /// fail with `HarnessError::Capability`, never ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<crate::work_outline_instructions::InstructionDelivery>,
}

/// Isolated-worktree directive riding [`RunRequest`]. The worktree is created
/// by the HOST while draining the queued Run — not by the sender over a
/// blocking CreateWorktree RPC — so the send path stays durable: a lost relay
/// frame can't wedge the composer on "Sending…" while the session runs anyway
/// (2026-08-18 user report).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeSpec {
    /// The repo whose worktree to create (the space's folder on the host).
    pub repo_path: String,
    /// Base ref the fresh `zeron/<name>` branch is created off.
    pub base: String,
    /// Owning project used to resolve host-local setup Actions. Optional for
    /// wire compatibility with clients that only request worktree creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
}

/// The session-scoped singleton id for the live plan/todo chip. ACP plan
/// updates carry no wire id; adapters emit every update under this one id so
/// the fold refreshes the same chip in place. Consumers that de-duplicate
/// tool ids across segment boundaries (the engine's stale-echo filter) must
/// EXEMPT this id — it legitimately reappears in every segment for the whole
/// life of a run.
pub const LIVE_PLAN_TOOL_ID: &str = "acp-plan";

/// A decoded tool invocation, reduced to the fields each kind renders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ToolCall {
    Exec {
        command: String,
    },
    ReadFile {
        path: String,
    },
    WriteFile {
        path: String,
        /// Full content; STRIPPED by the render-parts policy before entering the doc.
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<String>,
    },
    EditFile {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        old_string: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        new_string: Option<String>,
    },
    ApplyPatch {
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    Search {
        pattern: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    Glob {
        pattern: String,
    },
    WebFetch {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    WebSearch {
        query: String,
    },
    Todo {
        #[serde(default)]
        items: Vec<TodoItem>,
    },
    Mcp {
        server: String,
        tool: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<serde_json::Value>,
    },
    Unknown {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<serde_json::Value>,
    },
}

impl ToolCall {
    /// A subagent SPAWN call — the `Agent[: <description>]` naming convention
    /// every driver decodes its spawn tool into (claude/codex `Task`, cursor
    /// `task`, grok `spawn_subagent`, opencode `task`). This is the single
    /// genus gate for subagent binding: tagged subagent traffic may only ever
    /// stamp a ref/status onto a spawn call, so a driver keying bug can never
    /// turn an ordinary Run/Read chip into a spawn chip (2026-08-20: claude's
    /// background-shell `task_notification` did exactly that — the chip
    /// linked to a never-created doc and opened an empty panel).
    pub fn is_subagent_spawn(&self) -> bool {
        let name = match self {
            ToolCall::Unknown { name, .. } => name,
            ToolCall::Mcp { tool, .. } => tool,
            _ => return false,
        };
        name == "Agent" || name.starts_with("Agent: ")
    }

    /// The model a subagent SPAWN was given, when the spawn named one.
    ///
    /// Read off the spawn's own input rather than the session's picked model:
    /// a spawn may override it per child (claude's `Agent` takes `model`, grok
    /// `spawn_subagent` a `model_id`), and two chips spawned in one turn can
    /// legitimately name different models. `None` means the spawn didn't say —
    /// the child inherits the parent's model, which the chip already implies,
    /// so nothing is rendered rather than guessing a name.
    ///
    /// Only ever answers for [`is_subagent_spawn`](Self::is_subagent_spawn)
    /// calls: an ordinary tool with a stray `model` argument is not a spawn.
    pub fn subagent_model(&self) -> Option<&str> {
        if !self.is_subagent_spawn() {
            return None;
        }
        let input = match self {
            ToolCall::Unknown { input, .. } | ToolCall::Mcp { input, .. } => input.as_ref()?,
            _ => return None,
        };
        SUBAGENT_MODEL_KEYS
            .iter()
            .find_map(|key| input.get(key).and_then(serde_json::Value::as_str))
            .map(str::trim)
            .filter(|model| !model.is_empty())
    }
}

/// Spawn-input keys that carry a child model, in precedence order. Drivers
/// disagree on the spelling, so the lookup is by key set, not by harness —
/// a new adapter naming it any of these needs no code change here.
pub const SUBAGENT_MODEL_KEYS: [&str; 4] = ["model", "modelId", "model_id", "subagent_model"];

/// The spawn-input keys [`sanitize_tool_call`](crate::) must preserve so the
/// chip can name the child's model. Deliberately tiny: everything else on a
/// spawn's input (the whole prompt, most of all) stays host-local.
pub const SUBAGENT_INPUT_KEEP: [&str; 5] = [
    "model",
    "modelId",
    "model_id",
    "subagent_model",
    "subagent_type",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoItem {
    pub text: String,
    pub done: bool,
}

/// A slash command advertised by the agent (ACP `availableCommands`): typed as
/// `/name` at the start of the composer, sent to the agent as prompt text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommand {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Placeholder hint for the command's argument, when it takes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_hint: Option<String>,
}

/// A file modification carried inline on a tool result (ACP
/// `ToolCallContent::Diff`). `old_text: None` means a new file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDiff {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_text: Option<String>,
    pub new_text: String,
    /// Provider-native unified patch when full before/after file bodies are
    /// unavailable (Codex fileChange items). May contain multiple `diff --git`
    /// sections; `path` is the single file path or `workspace` for an aggregate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unified_diff: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<String>,
    #[serde(default)]
    pub multi_select: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputAnswer {
    pub question_id: String,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DoneStatus {
    Completed,
    Interrupted,
    Errored,
}

/// The normalized streaming event every harness emits.
///
/// Mirrors zeron's `AgentEvent` tagged enum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AgentEvent {
    #[serde(rename_all = "camelCase")]
    SessionStarted {
        harness: HarnessId,
        model: String,
        #[serde(default)]
        tools: Vec<String>,
        cwd: String,
        /// Harness-native session id (used for resume).
        session_id: String,
        assistant_message_id: String,
    },
    TextDelta {
        text: String,
    },
    /// A generated raster asset. The engine materializes this path before publication.
    #[serde(rename_all = "camelCase")]
    GeneratedImage {
        id: String,
        path: String,
        name: String,
        mime_type: String,
    },
    ReasoningDelta {
        text: String,
    },
    /// Backend-internal steering boundary marker.
    #[serde(rename_all = "camelCase")]
    AssistantMessageCompleted {
        assistant_message_id: String,
    },
    ToolCall {
        id: String,
        call: ToolCall,
    },
    #[serde(rename_all = "camelCase")]
    ToolResult {
        id: String,
        is_error: bool,
        /// Tool output text, capped by the emitting harness (ACP tool-call
        /// content; claude/codex adapters never populate it). The doc-side
        /// fold applies its own byte cap before anything persists.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        /// Inline file diff for edit-shaped tools (ACP `Diff` content).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<ToolDiff>,
    },
    /// Latest context occupancy, independent of cumulative billing usage.
    /// Missing fields preserve the previous measurement; zero tokens is valid.
    #[serde(rename_all = "camelCase")]
    ContextUsage {
        tokens: Option<u64>,
        window: Option<u64>,
    },
    /// Kept as a harness passthrough (rate-limit probes); never persisted to docs.
    #[serde(rename_all = "camelCase")]
    Usage {
        input_tokens: u64,
        output_tokens: u64,
    },
    /// The agent advertised (or changed) its slash-command set — ACP
    /// `available_commands_update`. The engine caches the latest list per
    /// harness for the composer's `/` popup; never persisted to docs.
    #[serde(rename_all = "camelCase")]
    AvailableCommands {
        commands: Vec<SlashCommand>,
    },
    Error {
        message: String,
    },
    #[serde(rename_all = "camelCase")]
    InputRequested {
        request_id: String,
        questions: Vec<UserInputQuestion>,
    },
    #[serde(rename_all = "camelCase")]
    InputResolved {
        request_id: String,
    },
    /// A confirmed new assignment. When tagged as Subagent, this reopens the
    /// same child transcript even if the provider does not echo the user text.
    #[serde(rename_all = "camelCase")]
    Steered {
        assistant_message_id: Option<String>,
        next_assistant_message_id: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Done {
        status: DoneStatus,
        result: Option<String>,
        error: Option<String>,
        session_id: Option<String>,
    },
    /// A USER-role message injected into a running session — today only seen
    /// wrapped in [`AgentEvent::Subagent`]: the PARENT agent steering its
    /// subagent mid-run (claude: a tagged user frame's text blocks). The
    /// engine writes it to the subagent doc as its own user entry, closing
    /// the streaming assistant segment above it — the subagent transcript
    /// then reads like any steered chat. Never emitted untagged (the parent
    /// chat's user messages come from doc commands, not the wire).
    #[serde(rename_all = "camelCase")]
    UserMessage {
        text: String,
    },
    /// An event belonging to a SUBAGENT's nested transcript, attributed to
    /// the spawning tool call (`parent_tool_use_id` = the parent-feed
    /// `ToolCall::id` that launched it). Never folded into the parent chat
    /// doc — the engine routes these to the subagent's own doc; the parent
    /// keeps only the spawn chip. Additive: old consumers that don't match
    /// this variant drop the nested traffic, which is the pre-subagent-viz
    /// behavior.
    #[serde(rename_all = "camelCase")]
    Subagent {
        parent_tool_use_id: String,
        event: Box<AgentEvent>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_event_round_trips() {
        let ev = AgentEvent::ToolCall {
            id: "t1".into(),
            call: ToolCall::Exec {
                command: "cargo test".into(),
            },
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(serde_json::from_str::<AgentEvent>(&json).unwrap(), ev);
    }

    /// Drivers spell the key differently; the chip must not care which one
    /// spawned the child. Non-spawns never answer, whatever they carry.
    #[test]
    fn subagent_model_reads_every_spelling_and_only_off_a_spawn() {
        let spawn = |input: serde_json::Value| ToolCall::Unknown {
            name: "Agent: scan".into(),
            input: Some(input),
        };
        for key in SUBAGENT_MODEL_KEYS {
            let call = spawn(serde_json::json!({ key: "haiku" }));
            assert_eq!(call.subagent_model(), Some("haiku"), "key {key}");
        }
        // An MCP-shaped spawn (cursor routes its `task` through MCP) too.
        assert_eq!(
            ToolCall::Mcp {
                server: "s".into(),
                tool: "Agent: scan".into(),
                input: Some(serde_json::json!({ "model": "sonnet" })),
            }
            .subagent_model(),
            Some("sonnet")
        );
        // Not a spawn: the name gate wins over the key.
        assert_eq!(
            ToolCall::Unknown {
                name: "Bash".into(),
                input: Some(serde_json::json!({ "model": "haiku" })),
            }
            .subagent_model(),
            None
        );
        // A spawn that named nothing usable inherits — nothing to render.
        assert_eq!(
            spawn(serde_json::json!({ "model": " " })).subagent_model(),
            None
        );
        assert_eq!(
            spawn(serde_json::json!({ "prompt": "x" })).subagent_model(),
            None
        );
        assert_eq!(
            ToolCall::Unknown {
                name: "Agent".into(),
                input: None
            }
            .subagent_model(),
            None
        );
        // Non-string values are not names.
        assert_eq!(
            spawn(serde_json::json!({ "model": 5 })).subagent_model(),
            None
        );
    }

    #[test]
    fn run_request_attachments_default_and_round_trip() {
        // Old-wire JSON without the field parses (additive compat)…
        let old = r#"{"prompt":"p","model":null,"reasoning":null,"cwd":".","sandbox":"workspace-write","resume":null}"#;
        let req: RunRequest = serde_json::from_str(old).unwrap();
        assert!(req.attachments.is_empty());
        // …and an empty list serializes away (old readers never see it).
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("attachments").is_none());
        // Populated lists round-trip.
        let req = RunRequest {
            attachments: vec!["/tmp/a.png".into()],
            ..req
        };
        let round: RunRequest =
            serde_json::from_value(serde_json::to_value(&req).unwrap()).unwrap();
        assert_eq!(round.attachments, vec!["/tmp/a.png".to_string()]);
    }

    #[test]
    fn run_request_worktree_default_and_round_trip() {
        // Old-wire JSON without the field parses (additive compat)…
        let old = r#"{"prompt":"p","model":null,"reasoning":null,"cwd":".","sandbox":"workspace-write","resume":null}"#;
        let req: RunRequest = serde_json::from_str(old).unwrap();
        assert!(req.worktree.is_none());
        // …and `None` serializes away (old readers never see it).
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("worktree").is_none());
        // A populated spec round-trips camelCased.
        let req = RunRequest {
            worktree: Some(WorktreeSpec {
                repo_path: "/repos/comet".into(),
                base: "main".into(),
                space_id: Some("space-1".into()),
            }),
            ..req
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["worktree"]["repoPath"], "/repos/comet");
        assert_eq!(json["worktree"]["spaceId"], "space-1");
        let round: RunRequest = serde_json::from_value(json).unwrap();
        assert_eq!(round.worktree, req.worktree);
    }

    #[test]
    fn tool_diff_unified_patch_is_additive_wire_data() {
        let old = r#"{"path":"src/a.rs","oldText":"old","newText":"new"}"#;
        let decoded: ToolDiff = serde_json::from_str(old).unwrap();
        assert!(decoded.unified_diff.is_none());
        assert!(
            serde_json::to_value(&decoded)
                .unwrap()
                .get("unifiedDiff")
                .is_none()
        );

        let patch = ToolDiff {
            path: "src/a.rs".into(),
            old_text: None,
            new_text: String::new(),
            unified_diff: Some("@@ -1 +1 @@\n-old\n+new\n".into()),
        };
        let value = serde_json::to_value(&patch).unwrap();
        assert_eq!(value["unifiedDiff"], patch.unified_diff.unwrap());
    }

    #[test]
    fn harness_id_uses_kebab_case() {
        assert_eq!(
            serde_json::to_string(&HarnessId::ClaudeCode).unwrap(),
            "\"claude-code\""
        );
    }
}

/// Host-owned context snapshot, replicated with the chat document.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    pub tokens: Option<u64>,
    pub window: Option<u64>,
}

impl ContextUsage {
    pub fn fraction(self) -> Option<f64> {
        Some(self.tokens? as f64 / self.window.filter(|n| *n > 0)? as f64)
    }
}

#[cfg(test)]
mod generated_image_tests {
    use super::*;

    #[test]
    fn generated_image_event_round_trip() {
        let event = AgentEvent::GeneratedImage {
            id: "item:image".into(),
            path: "/uploads/generated.png".into(),
            name: "generated.png".into(),
            mime_type: "image/png".into(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "generatedImage");
        assert_eq!(value["mimeType"], "image/png");
        assert_eq!(serde_json::from_value::<AgentEvent>(value).unwrap(), event);
    }
}

/// Engine-authenticated agent tools (Work Outline Milestone 4): the wire
/// contract shared by the engine's agent RPC surface and the `zeron mcp`
/// server it injects into Claude Code and Codex runs of Chats in a Space.
///
/// Authentication: the engine mints a random per-Run capability token bound
/// to `(chatId, runId, spaceId)` when it spawns the Run, hands it to the MCP
/// server in [`TOKEN_ENV`], and revokes it when the Run ends. Every agent
/// call carries the token; the engine derives the originating Chat from it
/// and never trusts a client-supplied Chat id. Both methods are local to the
/// engine that minted the token (never device-forwarded).
pub mod agent_tools {
    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    /// Name of the injected MCP server (Claude tools surface as
    /// `mcp__zeron-agent__<tool>`). Distinct from a plain `zeron`, so a
    /// user-defined `zeron` server (a terminal `zeron mcp`) never collides.
    pub const SERVER_NAME: &str = "zeron-agent";
    /// Environment variable carrying the per-Run capability token.
    pub const TOKEN_ENV: &str = "ZERON_AGENT_TOKEN";
    /// Environment variable naming the engine's loopback IPC port.
    pub const IPC_PORT_ENV: &str = "ZERON_IPC_PORT";
    /// `{token}` → [`AgentOrigin`]. Fails with `agent-tools/unauthenticated`
    /// for an unknown, revoked, or expired token.
    pub const AUTHENTICATE_METHOD: &str = "AgentAuthenticate";
    /// [`AgentToolCallParams`] → the tool's JSON result. Failures are
    /// `agent-tools/<code>: …` ([`AgentToolErrorCode`]) or a Work Outline
    /// `work-outline/<code>: …` string from the underlying operation.
    pub const TOOL_CALL_METHOD: &str = "AgentToolCall";
    /// Longest accepted `idempotencyKey`.
    pub const MAX_IDEMPOTENCY_KEY_CHARS: usize = 200;

    /// Tool wire names. Every mutating tool takes an `idempotencyKey`; only the
    /// naturally idempotent ones derive it when omitted ([`derived_idempotency_key`]).
    pub mod tools {
        /// The Chat's work item, its effective instructions, and the Space.
        pub const READ_CURRENT: &str = "work_outline_read_current";
        /// The Chat's item (or a descendant) with its descendants, their
        /// attached Chats, and open user action requests; read-only.
        pub const READ_SUBTREE: &str = "work_outline_read_subtree";
        /// Create a child under the Chat's item or one of its descendants.
        pub const CREATE_CHILD: &str = "work_outline_create_child";
        /// Associate the calling Chat (only) with an item of its Space.
        pub const ASSOCIATE_CURRENT_CHAT: &str = "work_outline_associate_current_chat";
        /// Mark the Chat's item or a descendant active or completed.
        pub const SET_STATUS: &str = "work_outline_set_status";
        /// Ask the user for an action on the Chat's item.
        pub const CREATE_USER_ACTION_REQUEST: &str = "work_outline_create_user_action_request";
        /// Resolve a request this Chat created.
        pub const RESOLVE_USER_ACTION_REQUEST: &str = "work_outline_resolve_user_action_request";
        /// Propose an instruction for the item, its parent, or the Space root.
        pub const PROPOSE_INSTRUCTION: &str = "work_outline_propose_instruction";
        /// Engine-internal (not an MCP tool): queue the first Turn of a child
        /// Chat the MCP `create_chat` tool created, with ids derived from the
        /// calling Chat and its idempotency key so retries never duplicate it.
        pub const QUEUE_CHILD_CHAT_TURN: &str = "chat_queue_child_turn";
        /// Engine-internal (not an MCP tool): start the child Chat the MCP
        /// `create_chat` tool was asked to create for a work item
        /// (`workItemId`: the caller's item or a descendant), through the
        /// same launch workflow as the UI's Start Chat on an item. The Chat
        /// records the caller as its parent; ids derive from the calling
        /// Chat and its idempotency key.
        pub const LAUNCH_CHILD_CHAT: &str = "chat_launch_child_for_item";
        /// Engine-internal (not an MCP tool): bind an MCP `create_chat`
        /// idempotency key to the hash of its inputs (durably, in the
        /// processed-command ledger). The same key with other inputs is
        /// `work-outline/operationConflict`; returns `{replayed}`.
        pub const CLAIM_CHILD_CHAT: &str = "chat_claim_child";

        /// Copy a file the agent wrote into the work item's artifact folder
        /// and record a revision (`docs/planning-artifact-conventions.md`).
        pub const ARTIFACT_PUBLISH: &str = "artifact_publish";
        /// The pinned plan of the Chat's work item (or of the Chat).
        pub const ARTIFACT_GET_PLAN: &str = "artifact_get_plan";
        /// Render an artifact offscreen in the headed app and return a
        /// screenshot plus errors and warnings.
        pub const ARTIFACT_RENDER_CHECK: &str = "artifact_render_check";

        /// Record the git worktree the calling Chat works in as its diff
        /// target (`chatDiffTargets`): the Changes pane then shows that
        /// checkout. Call right after `git worktree add`, or when resuming an
        /// existing worktree for the Chat's work.
        pub const BIND_CHAT_WORKTREE: &str = "bind_chat_worktree";

        /// The planning artifact tools, in catalog order.
        pub const ARTIFACTS: [&str; 3] =
            [ARTIFACT_PUBLISH, ARTIFACT_GET_PLAN, ARTIFACT_RENDER_CHECK];

        /// The Work Outline tools, in catalog order.
        pub const WORK_OUTLINE: [&str; 8] = [
            READ_CURRENT,
            READ_SUBTREE,
            CREATE_CHILD,
            ASSOCIATE_CURRENT_CHAT,
            SET_STATUS,
            CREATE_USER_ACTION_REQUEST,
            RESOLVE_USER_ACTION_REQUEST,
            PROPOSE_INSTRUCTION,
        ];
    }

    /// Params of [`AUTHENTICATE_METHOD`].
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct AgentAuthenticateParams {
        pub token: String,
    }

    /// Who a valid token speaks for (engine-derived, never client-declared).
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct AgentOrigin {
        pub chat_id: String,
        pub run_id: String,
        pub space_id: String,
        pub device_id: String,
    }

    /// Params of [`TOOL_CALL_METHOD`].
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct AgentToolCallParams {
        pub token: String,
        pub tool: String,
        #[serde(default)]
        pub arguments: serde_json::Value,
    }

    /// Why an agent call was refused before reaching a Work Outline operation.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub enum AgentToolErrorCode {
        /// Absent, unknown, revoked, or expired token.
        Unauthenticated,
        /// The token is valid but the policy does not allow this target.
        Forbidden,
        /// Malformed arguments (including a missing `idempotencyKey`).
        Invalid,
        /// Unknown tool, or an operation this host cannot perform yet.
        #[serde(other)]
        Unsupported,
    }

    impl AgentToolErrorCode {
        pub fn as_str(self) -> &'static str {
            match self {
                Self::Unauthenticated => "unauthenticated",
                Self::Forbidden => "forbidden",
                Self::Invalid => "invalid",
                Self::Unsupported => "unsupported",
            }
        }
    }

    /// Prefix of a wire-encoded [`AgentToolErrorCode`] failure.
    pub const ERROR_PREFIX: &str = "agent-tools/";

    /// `agent-tools/<code>: <message>`.
    pub fn error_wire_message(code: AgentToolErrorCode, message: &str) -> String {
        format!("{ERROR_PREFIX}{}: {message}", code.as_str())
    }

    /// The Work Outline `operationId` of an agent mutation: derived from the
    /// token's Chat and the agent-supplied key only, so a retry with the same
    /// key dedupes across Runs (and processes) of that Chat, and two Chats
    /// can never collide. 32 lowercase hex characters.
    pub fn operation_id(chat_id: &str, idempotency_key: &str) -> String {
        crate::work_outline::derive_id(
            &format!("zeron-agent-tool\u{0}{chat_id}\u{0}{idempotency_key}"),
            "agent-operation",
        )
    }

    /// Trimmed key, or why it is unusable.
    pub fn validate_idempotency_key(key: Option<&str>) -> Result<&str, String> {
        let key = key.map(str::trim).unwrap_or_default();
        if key.is_empty() {
            return Err(
                "idempotencyKey is required: pass a stable key (for example a short \
                 description of this step) and reuse it when you retry the same call"
                    .into(),
            );
        }
        if key.chars().count() > MAX_IDEMPOTENCY_KEY_CHARS {
            return Err(format!(
                "idempotencyKey is longer than {MAX_IDEMPOTENCY_KEY_CHARS} characters"
            ));
        }
        Ok(key)
    }

    /// Idempotency key for a naturally idempotent call whose caller omitted
    /// one: `auto:` plus a [`derive_id`](crate::work_outline::derive_id) hash
    /// (37 characters, so the registry operation id stays within its limit)
    /// of the tool and the parts that identify the change. Include whatever
    /// state the call depends on (for example the item's `updatedAt`) so a
    /// later, different call never replays an older result.
    pub fn derived_idempotency_key(tool: &str, parts: &[&str]) -> String {
        format!(
            "auto:{}",
            crate::work_outline::derive_id(
                &parts.join("\u{0}"),
                &format!("derived-key\u{0}{tool}")
            )
        )
    }

    /// One parameter of a tool as shown in argument errors.
    #[derive(Debug, Clone, Copy)]
    pub struct ParamUsage {
        pub name: &'static str,
        pub hint: &'static str,
    }

    /// What a tool accepts, for self-correcting argument errors.
    #[derive(Debug, Clone, Copy)]
    pub struct ToolUsage {
        pub tool: &'static str,
        pub required: &'static [ParamUsage],
        pub optional: &'static [ParamUsage],
        pub example: &'static str,
    }

    const fn p(name: &'static str, hint: &'static str) -> ParamUsage {
        ParamUsage { name, hint }
    }

    const KEY_DERIVED: ParamUsage = p("idempotencyKey", "derived when omitted");
    const KEY_CREATE: ParamUsage = p(
        "idempotencyKey",
        "a stable string; reuse it only to retry the same call",
    );

    /// Usage of every engine-side agent tool and `create_chat`.
    pub const USAGE: &[ToolUsage] = &[
        ToolUsage {
            tool: tools::READ_CURRENT,
            required: &[],
            optional: &[],
            example: "{}",
        },
        ToolUsage {
            tool: tools::READ_SUBTREE,
            required: &[],
            optional: &[
                p("workItemId", "defaults to your chat's item"),
                p("maxDepth", "0-8, default 4"),
                p("includeArchived", "boolean"),
            ],
            example: r#"{"maxDepth": 2}"#,
        },
        ToolUsage {
            tool: tools::CREATE_CHILD,
            required: &[p("title", "string"), KEY_CREATE],
            optional: &[
                p("description", "string"),
                p("parentWorkItemId", "defaults to your chat's item"),
                p("status", "pending or active"),
            ],
            example: r#"{"title": "Fix the importer", "idempotencyKey": "create-fix-importer"}"#,
        },
        ToolUsage {
            tool: tools::ASSOCIATE_CURRENT_CHAT,
            required: &[p("workItemId", "id of the item to attach this chat to")],
            optional: &[KEY_DERIVED],
            example: r#"{"workItemId": "<item id>"}"#,
        },
        ToolUsage {
            tool: tools::SET_STATUS,
            required: &[p("status", "active or completed")],
            optional: &[p("workItemId", "defaults to your chat's item"), KEY_DERIVED],
            example: r#"{"status": "completed"}"#,
        },
        ToolUsage {
            tool: tools::CREATE_USER_ACTION_REQUEST,
            required: &[
                p("request", "the one-line question or action"),
                p("idempotencyKey", "or pass dedupeKey instead"),
            ],
            optional: &[
                p("detail", "context for the user"),
                p(
                    "dedupeKey",
                    "one per topic; open requests with the same key are not repeated",
                ),
                p("toolCallId", "string"),
            ],
            example: r#"{"request": "Which branch should this target?", "detail": "main or release?", "dedupeKey": "target-branch"}"#,
        },
        ToolUsage {
            tool: tools::RESOLVE_USER_ACTION_REQUEST,
            required: &[p(
                "userActionRequestId",
                "id from work_outline_read_current",
            )],
            optional: &[
                p("resolution", "note recorded with the resolution"),
                KEY_DERIVED,
            ],
            example: r#"{"userActionRequestId": "<request id>", "resolution": "Answered in chat"}"#,
        },
        ToolUsage {
            tool: tools::PROPOSE_INSTRUCTION,
            required: &[p("text", "the instruction"), KEY_CREATE],
            optional: &[
                p("target", "item (default), parent, or spaceRoot"),
                p("toolCallId", "string"),
            ],
            example: r#"{"text": "Run cargo fmt before committing.", "idempotencyKey": "propose-fmt"}"#,
        },
        ToolUsage {
            tool: tools::BIND_CHAT_WORKTREE,
            required: &[p("path", "absolute top-level path of the worktree")],
            optional: &[KEY_DERIVED],
            example: r#"{"path": "/abs/path/to/worktree"}"#,
        },
        ToolUsage {
            tool: tools::ARTIFACT_PUBLISH,
            required: &[
                p("path", "file you wrote"),
                p("title", "human title"),
                p("note", "one line: what changed"),
                KEY_CREATE,
            ],
            optional: &[
                p(
                    "kind",
                    "markdown, html-fragment, html-page, mermaid, image, or options",
                ),
                p("role", "plan, diagram, state-machine, comparison, or doc"),
                p("pinAsPlan", "boolean"),
            ],
            example: r#"{"path": "plan.md", "title": "Importer plan", "note": "first draft", "idempotencyKey": "publish-plan-1"}"#,
        },
        ToolUsage {
            tool: tools::ARTIFACT_GET_PLAN,
            required: &[],
            optional: &[],
            example: "{}",
        },
        ToolUsage {
            tool: tools::ARTIFACT_RENDER_CHECK,
            required: &[p("artifactId", "or path, for an unpublished file")],
            optional: &[
                p("themes", r#"["light", "dark"]"#),
                p("viewports", "list of {width, height, label}"),
            ],
            example: r#"{"artifactId": "<artifact id>", "themes": ["light", "dark"]}"#,
        },
        ToolUsage {
            tool: "create_chat",
            required: &[p("idempotencyKey", "inside a Mast chat; a stable string")],
            optional: &[
                p("project", "project path or id"),
                p("prompt", "first message (required with workItemId)"),
                p("workItemId", "your item or a descendant"),
                p("title", "string"),
                p("harness", "see list_harnesses"),
                p("model", "see list_models"),
            ],
            example: r#"{"workItemId": "<item id>", "prompt": "Do X, then finish with Ready to merge.", "idempotencyKey": "start-x", "wait": false}"#,
        },
    ];

    /// The usage entry of `tool`, if it has one.
    pub fn usage_of(tool: &str) -> Option<&'static ToolUsage> {
        USAGE.iter().find(|usage| usage.tool == tool)
    }

    /// Multi-line text appended to an argument error: the tool's required and
    /// optional parameters plus a minimal example call.
    pub fn usage_text(tool: &str) -> Option<String> {
        use std::fmt::Write as _;
        let usage = usage_of(tool)?;
        let list = |params: &[ParamUsage]| {
            if params.is_empty() {
                "none".to_owned()
            } else {
                params
                    .iter()
                    .map(|p| format!("{} ({})", p.name, p.hint))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        };
        let mut out = format!("Usage of {}:", usage.tool);
        let _ = write!(out, "\n  required: {}", list(usage.required));
        let _ = write!(out, "\n  optional: {}", list(usage.optional));
        let _ = write!(out, "\n  example: {}", usage.example);
        Some(out)
    }

    /// Whether `tool`'s arguments are camelCase engine arguments (as opposed
    /// to the snake_case chat tools served by the MCP process).
    fn is_engine_tool(tool: &str) -> bool {
        tools::WORK_OUTLINE.contains(&tool)
            || tools::ARTIFACTS.contains(&tool)
            || tool == tools::BIND_CHAT_WORKTREE
    }

    /// `snake_case` -> `camelCase`; other names unchanged.
    fn camel_case(name: &str) -> String {
        if !name.contains('_') {
            return name.to_owned();
        }
        let mut out = String::with_capacity(name.len());
        let mut upper = false;
        for ch in name.chars() {
            if ch == '_' {
                upper = !out.is_empty();
            } else if upper {
                out.extend(ch.to_uppercase());
                upper = false;
            } else {
                out.push(ch);
            }
        }
        out
    }

    /// The parameter a guessed `name` stands for on `tool` (camelCase in,
    /// canonical camelCase out), or `name` itself.
    fn canonical_param(tool: &str, name: &str) -> &'static str {
        match (tool, name) {
            (tools::CREATE_USER_ACTION_REQUEST, "text" | "title" | "question") => "request",
            (tools::RESOLVE_USER_ACTION_REQUEST, "id" | "requestId") => "userActionRequestId",
            (
                tools::CREATE_CHILD,
                "workItemId" | "workItem" | "itemId" | "parentId" | "parentItemId" | "parent",
            ) => "parentWorkItemId",
            (_, "workItem" | "itemId") => "workItemId",
            _ => "",
        }
    }

    /// Normalise the arguments of an agent tool before they are parsed:
    /// `snake_case` keys become `camelCase` (engine tools), and common
    /// guessed names map to the real parameter (`text`/`title`/`question` ->
    /// `request`, `id`/`requestId` -> `userActionRequestId`, `workItem`/
    /// `itemId` -> `workItemId`). A parameter that is already present wins
    /// over an alias. Other tools' arguments are returned unchanged.
    pub fn normalize_arguments(tool: &str, arguments: &Value) -> Value {
        let engine = is_engine_tool(tool);
        if !engine && tool != "create_chat" {
            return arguments.clone();
        }
        let object = match arguments {
            Value::Null => return Value::Object(Default::default()),
            Value::Object(object) => object,
            other => return other.clone(),
        };
        let renamed = |key: &str| -> String {
            let camel = if engine {
                camel_case(key)
            } else {
                key.to_owned()
            };
            match canonical_param(tool, &camel) {
                "" => camel,
                canonical => canonical.to_owned(),
            }
        };
        let mut out = serde_json::Map::new();
        // Exact (already canonical) names first, so an alias never shadows one.
        for (key, value) in object {
            if renamed(key) == *key {
                out.insert(key.clone(), value.clone());
            }
        }
        for (key, value) in object {
            let target = renamed(key);
            if target != *key && !out.contains_key(&target) {
                out.insert(target, value.clone());
            }
        }
        Value::Object(out)
    }

    /// `argument error + usage` for an `invalid` agent tool failure.
    pub fn with_usage(tool: &str, message: &str) -> String {
        match usage_text(tool) {
            Some(usage) if !message.contains("\nUsage of ") => format!("{message}\n{usage}"),
            _ => message.to_owned(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn operation_ids_are_per_chat_and_stable() {
            let a = operation_id("chat-a", "step-1");
            assert_eq!(a, operation_id("chat-a", "step-1"));
            assert_eq!(a.len(), 32);
            assert_ne!(a, operation_id("chat-b", "step-1"));
            assert_ne!(a, operation_id("chat-a", "step-2"));
            // The separator keeps `(ab, c)` and `(a, bc)` apart.
            assert_ne!(operation_id("ab", "c"), operation_id("a", "bc"));
        }

        #[test]
        fn idempotency_keys_are_required_and_bounded() {
            assert!(validate_idempotency_key(None).is_err());
            assert!(validate_idempotency_key(Some("  ")).is_err());
            assert_eq!(validate_idempotency_key(Some(" k ")).unwrap(), "k");
            let long = "x".repeat(MAX_IDEMPOTENCY_KEY_CHARS + 1);
            assert!(validate_idempotency_key(Some(&long)).is_err());
        }

        #[test]
        fn derived_keys_are_stable_short_and_scoped_to_their_parts() {
            let key = derived_idempotency_key("set", &["item", "completed", "7"]);
            assert_eq!(
                key,
                derived_idempotency_key("set", &["item", "completed", "7"])
            );
            assert!(key.starts_with("auto:"));
            // Long inputs still give a short key and a registry-sized operation id.
            let long = "x".repeat(5000);
            let long_key = derived_idempotency_key("set", &[&long, &long]);
            assert_eq!(long_key.len(), key.len());
            assert!(validate_idempotency_key(Some(&long_key)).is_ok());
            assert!(operation_id("chat", &long_key).len() <= 64);
            assert_ne!(
                key,
                derived_idempotency_key("set", &["item", "completed", "8"])
            );
            assert_ne!(
                key,
                derived_idempotency_key("other", &["item", "completed", "7"])
            );
            assert_ne!(
                derived_idempotency_key("t", &["ab", "c"]),
                derived_idempotency_key("t", &["a", "bc"])
            );
        }

        #[test]
        fn guessed_names_normalise_to_the_real_parameters() {
            use serde_json::json;
            let n = |tool: &str, args: Value| normalize_arguments(tool, &args);
            assert_eq!(
                n(
                    tools::CREATE_USER_ACTION_REQUEST,
                    json!({ "text": "Q?", "idempotency_key": "k" })
                ),
                json!({ "request": "Q?", "idempotencyKey": "k" })
            );
            for alias in ["title", "question", "text"] {
                assert_eq!(
                    n(tools::CREATE_USER_ACTION_REQUEST, json!({ alias: "Q?" })),
                    json!({ "request": "Q?" }),
                    "{alias}"
                );
            }
            for alias in ["id", "requestId", "request_id", "user_action_request_id"] {
                assert_eq!(
                    n(tools::RESOLVE_USER_ACTION_REQUEST, json!({ alias: "r1" })),
                    json!({ "userActionRequestId": "r1" }),
                    "{alias}"
                );
            }
            for alias in ["workItem", "itemId", "work_item_id"] {
                assert_eq!(
                    n(tools::SET_STATUS, json!({ alias: "w", "status": "active" })),
                    json!({ "workItemId": "w", "status": "active" }),
                    "{alias}"
                );
                assert_eq!(
                    n(tools::ASSOCIATE_CURRENT_CHAT, json!({ alias: "w" })),
                    json!({ "workItemId": "w" }),
                    "{alias}"
                );
            }
            assert_eq!(
                n(
                    tools::CREATE_CHILD,
                    json!({ "title": "t", "workItemId": "p" })
                ),
                json!({ "title": "t", "parentWorkItemId": "p" })
            );
            // A canonical name wins over an alias, whatever the order.
            assert_eq!(
                n(
                    tools::CREATE_USER_ACTION_REQUEST,
                    json!({ "text": "alias", "request": "real" })
                ),
                json!({ "request": "real" })
            );
            // `text` is the real parameter of propose_instruction.
            assert_eq!(
                n(tools::PROPOSE_INSTRUCTION, json!({ "text": "t" })),
                json!({ "text": "t" })
            );
            // create_chat keeps its snake_case names but takes the item aliases.
            assert_eq!(
                n(
                    "create_chat",
                    json!({ "workItem": "w", "idempotency_key": "k", "timeout_secs": 5 })
                ),
                json!({ "workItemId": "w", "idempotency_key": "k", "timeout_secs": 5 })
            );
            // Null arguments become an empty object; other tools are untouched.
            assert_eq!(n(tools::READ_CURRENT, Value::Null), json!({}));
            assert_eq!(
                n("read_chat", json!({ "chat": "c", "text": "x" })),
                json!({ "chat": "c", "text": "x" })
            );
        }

        #[test]
        fn every_engine_tool_has_usage_with_its_required_and_optional_parameters() {
            let mut names: Vec<&str> = tools::WORK_OUTLINE.to_vec();
            names.extend(tools::ARTIFACTS);
            names.extend([tools::BIND_CHAT_WORKTREE, "create_chat"]);
            for name in names {
                let usage = usage_text(name).unwrap_or_else(|| panic!("no usage for {name}"));
                assert!(usage.contains(&format!("Usage of {name}:")), "{usage}");
                assert!(
                    usage.contains("required:") && usage.contains("optional:"),
                    "{usage}"
                );
                let example = usage_of(name).unwrap().example;
                serde_json::from_str::<Value>(example)
                    .unwrap_or_else(|e| panic!("{name} example is not JSON: {e}"));
            }
            let text = with_usage(
                tools::SET_STATUS,
                "invalid arguments: missing field `status`",
            );
            assert!(text.starts_with("invalid arguments: missing field `status`\nUsage of"));
            assert!(text.contains("optional: workItemId (defaults to your chat's item), idempotencyKey (derived when omitted)"));
            assert!(text.contains(r#"example: {"status": "completed"}"#));
            // Not appended twice, and unknown tools pass through.
            assert_eq!(with_usage(tools::SET_STATUS, &text), text);
            assert_eq!(with_usage("nope", "x"), "x");
        }

        #[test]
        fn error_codes_are_lenient() {
            let code: AgentToolErrorCode = serde_json::from_str("\"newer\"").unwrap();
            assert_eq!(code, AgentToolErrorCode::Unsupported);
            assert_eq!(
                error_wire_message(AgentToolErrorCode::Forbidden, "no"),
                "agent-tools/forbidden: no"
            );
        }
    }
}
