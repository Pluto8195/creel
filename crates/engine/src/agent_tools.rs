//! Agent tools (Work Outline Milestone 4): engine-minted per-Run capability
//! tokens and the allow-listed agent RPC surface behind the injected
//! `zeron mcp` server (wire contract: `zeron_proto::agent_tools`).
//!
//! - [`AgentTokens`]: when `SessionsEngine` spawns a Run of a Chat in a Space
//!   on a harness that can inject MCP servers, it mints a random token bound
//!   to `(chatId, runId, spaceId, deviceId)`. The [`AgentTokenLease`] lives
//!   exactly as long as the Run's driver task and revokes the token on drop,
//!   whatever path ends the Run. Tokens are memory-only (an engine restart
//!   revokes every token, and the Runs die with it); only a SHA-256 of each
//!   token is kept. An absolute expiry ([`DEFAULT_TOKEN_TTL`]) backs up the
//!   lease; the Run's driver renews it on every heartbeat, so only a hung
//!   driver (one that keeps its lease but stops running) lets it lapse.
//! - [`AgentToolsService`]: `AgentAuthenticate` / `AgentToolCall`. Every call
//!   authenticates by token lookup (plus the Chat row's Space), never by
//!   materializing the outline; the originating Chat is the token's, never a
//!   client-supplied id. Mutations run through
//!   `WorkOutlineService::mutate_as` with `agent_actor(chatId)`, which
//!   enforces the shared agent policy (`authorize_agent`) under the registry
//!   lock, and derive their `operationId` from `(token Chat, idempotencyKey)`,
//!   so a retry with the same key dedupes across Runs and processes.
//!
//! The policy (contract section 8): read the Chat's item and effective
//! instructions; create a child under the Chat's item or a descendant;
//! associate only the calling Chat; set status `active` / `completed` on the
//! Chat's item or a descendant (completing an item with open requests is
//! `openUserActionRequests`, never auto-resolved); create requests on the
//! Chat's item with the Chat as origin; resolve requests the Chat raised;
//! propose instructions for the item, its parent, or the Space root.
//! Everything else is `unauthorized`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zeron_doc::SessionCommandPayload;
use zeron_harness::AgentToolsInjection;
use zeron_proto::agent_tools::{self, AgentOrigin, AgentToolCallParams, AgentToolErrorCode, tools};
use zeron_proto::work_outline::{derive_id, roles};
use zeron_proto::{
    AssignChatToWorkItemParams, CreateUserActionRequestParams, CreateWorkItemParams,
    InstructionProposalOrigin, InstructionProposalTarget, ProposeWorkItemInstructionParams,
    ResolveUserActionRequestParams, RunRequest, SandboxLevel, UpdateWorkItemParams,
    UserActionRequestOrigin, WorkItemChanges, WorkItemStatus, WorkOutlineError,
    WorkOutlineMutateReply, WorkOutlineMutation, WorkOutlinePlacement,
};

use crate::doc_host::DocHost;
use crate::work_outline::WorkOutlineService;
use crate::workspace_host::WorkspaceHost;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A refused agent call: a gate failure ([`AgentToolErrorCode`]) or the
/// underlying Work Outline operation's error, passed through unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentToolError {
    Refused {
        code: AgentToolErrorCode,
        message: String,
    },
    WorkOutline(WorkOutlineError),
}

impl AgentToolError {
    pub fn refused(code: AgentToolErrorCode, message: impl Into<String>) -> Self {
        Self::Refused {
            code,
            message: message.into(),
        }
    }

    fn unauthenticated() -> Self {
        Self::refused(
            AgentToolErrorCode::Unauthenticated,
            "this agent's Zeron token is missing, unknown, or expired (it is only valid while \
             the Run that received it is running)",
        )
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::refused(AgentToolErrorCode::Invalid, message)
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Refused { code, .. } => code.as_str(),
            Self::WorkOutline(error) => error.code.as_str(),
        }
    }

    /// `agent-tools/<code>: …` or `work-outline/<code>: …`.
    pub fn to_wire_message(&self) -> String {
        match self {
            Self::Refused { code, message } => agent_tools::error_wire_message(*code, message),
            Self::WorkOutline(error) => error.to_wire_message(),
        }
    }
}

impl From<WorkOutlineError> for AgentToolError {
    fn from(error: WorkOutlineError) -> Self {
        Self::WorkOutline(error)
    }
}

impl std::fmt::Display for AgentToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_wire_message())
    }
}

impl std::error::Error for AgentToolError {}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

/// What one live token authorizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentGrant {
    pub chat_id: String,
    pub run_id: String,
    pub space_id: String,
    pub device_id: String,
    /// Absolute backstop; `None` = valid until the Run's lease drops.
    pub expires_at: Option<Instant>,
    /// What [`AgentTokenLease::renew`] extends `expires_at` by.
    pub ttl: Option<Duration>,
}

impl AgentGrant {
    pub fn origin(&self) -> AgentOrigin {
        AgentOrigin {
            chat_id: self.chat_id.clone(),
            run_id: self.run_id.clone(),
            space_id: self.space_id.clone(),
            device_id: self.device_id.clone(),
        }
    }
}

type TokenKey = [u8; 32];

fn token_key(token: &str) -> TokenKey {
    Sha256::digest(token.as_bytes()).into()
}

/// Absolute token lifetime for engine-minted tokens, renewed while the Run's
/// driver is alive (generous: a renewal lands every heartbeat).
pub const DEFAULT_TOKEN_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// The engine's live per-Run tokens (one registry per engine).
#[derive(Default)]
pub struct AgentTokens {
    grants: Mutex<HashMap<TokenKey, AgentGrant>>,
}

impl AgentTokens {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Mint a token for one Run. Valid until the returned lease drops or
    /// [`DEFAULT_TOKEN_TTL`] passes without a [`AgentTokenLease::renew`].
    pub fn mint(
        self: &Arc<Self>,
        chat_id: &str,
        run_id: &str,
        space_id: &str,
        device_id: &str,
    ) -> AgentTokenLease {
        self.mint_with_ttl(
            chat_id,
            run_id,
            space_id,
            device_id,
            Some(DEFAULT_TOKEN_TTL),
        )
    }

    /// [`Self::mint`] with an absolute expiry as well.
    pub fn mint_with_ttl(
        self: &Arc<Self>,
        chat_id: &str,
        run_id: &str,
        space_id: &str,
        device_id: &str,
        ttl: Option<Duration>,
    ) -> AgentTokenLease {
        // Two v4 UUIDs: 244 random bits from the OS generator.
        let token = format!(
            "zat_{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let key = token_key(&token);
        lock(&self.grants).insert(
            key,
            AgentGrant {
                chat_id: chat_id.to_owned(),
                run_id: run_id.to_owned(),
                space_id: space_id.to_owned(),
                device_id: device_id.to_owned(),
                expires_at: ttl.map(|ttl| Instant::now() + ttl),
                ttl,
            },
        );
        AgentTokenLease {
            tokens: Arc::downgrade(self),
            key,
            token,
        }
    }

    /// The grant of a live token. Unknown, revoked, and expired tokens (an
    /// expired one is dropped here) are `unauthenticated`.
    pub fn authenticate(&self, token: &str) -> Result<AgentGrant, AgentToolError> {
        let token = token.trim();
        if token.is_empty() {
            return Err(AgentToolError::unauthenticated());
        }
        let key = token_key(token);
        let mut grants = lock(&self.grants);
        let grant = grants
            .get(&key)
            .cloned()
            .ok_or_else(AgentToolError::unauthenticated)?;
        if grant.expires_at.is_some_and(|at| Instant::now() >= at) {
            grants.remove(&key);
            return Err(AgentToolError::unauthenticated());
        }
        Ok(grant)
    }

    /// Number of live tokens (diagnostics and tests).
    pub fn live_count(&self) -> usize {
        lock(&self.grants).len()
    }

    fn revoke(&self, key: &TokenKey) {
        lock(&self.grants).remove(key);
    }

    fn renew(&self, key: &TokenKey) {
        let mut grants = lock(&self.grants);
        if let Some(grant) = grants.get_mut(key)
            && let (Some(ttl), Some(at)) = (grant.ttl, grant.expires_at)
            // An expired token stays expired (authenticate drops it).
            && Instant::now() < at
        {
            grant.expires_at = Some(Instant::now() + ttl);
        }
    }
}

/// Owns one token for one Run; dropping it revokes the token.
pub struct AgentTokenLease {
    tokens: Weak<AgentTokens>,
    key: TokenKey,
    token: String,
}

impl AgentTokenLease {
    /// The secret handed to the Run's MCP server. Never log it.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Push the absolute expiry out by the token's TTL (the Run's driver
    /// calls this on every heartbeat). A lapsed token is not revived.
    pub fn renew(&self) {
        if let Some(tokens) = self.tokens.upgrade() {
            tokens.renew(&self.key);
        }
    }
}

impl std::fmt::Debug for AgentTokenLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AgentTokenLease(..)")
    }
}

impl Drop for AgentTokenLease {
    fn drop(&mut self) {
        if let Some(tokens) = self.tokens.upgrade() {
            tokens.revoke(&self.key);
        }
    }
}

// ---------------------------------------------------------------------------
// Injection
// ---------------------------------------------------------------------------

/// The loopback IPC port this process's engine actually bound (0 = none).
static BOUND_IPC_PORT: AtomicU16 = AtomicU16::new(0);

/// Record the port `serve_ipc` bound, so injected servers dial this engine
/// rather than whatever `ZERON_IPC_PORT` / the default names. One engine
/// serves IPC per process; the latest bind wins.
pub fn note_bound_ipc_port(port: u16) {
    BOUND_IPC_PORT.store(port, Ordering::Relaxed);
}

/// The port recorded by [`note_bound_ipc_port`], if any.
pub fn bound_ipc_port() -> Option<u16> {
    match BOUND_IPC_PORT.load(Ordering::Relaxed) {
        0 => None,
        port => Some(port),
    }
}

/// How a Run reaches this engine's agent surface: the `zeron mcp` command
/// and the loopback IPC port it dials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentToolsLaunch {
    pub command: PathBuf,
    pub args: Vec<String>,
    /// `None`: the port this process's engine bound ([`bound_ipc_port`]),
    /// read at injection time.
    pub ipc_port: Option<u16>,
}

impl AgentToolsLaunch {
    /// The running `zeron` binary's `mcp` subcommand, dialing the IPC port
    /// the engine actually bound. `None` when the executable path is unknown.
    pub fn from_current_exe() -> Option<Self> {
        let command = std::env::current_exe().ok()?;
        Some(Self {
            command,
            args: vec!["mcp".into()],
            ipc_port: None,
        })
    }

    /// The injection for one Run holding `token`; `None` when this engine
    /// serves no IPC port (the server would reach another engine, or none).
    pub fn injection(&self, token: &str) -> Option<AgentToolsInjection> {
        let ipc_port = self.ipc_port.or_else(bound_ipc_port)?;
        Some(AgentToolsInjection {
            server_name: agent_tools::SERVER_NAME.into(),
            command: self.command.clone(),
            args: self.args.clone(),
            env: [
                (agent_tools::TOKEN_ENV.to_owned(), token.to_owned()),
                (agent_tools::IPC_PORT_ENV.to_owned(), ipc_port.to_string()),
            ]
            .into_iter()
            .collect(),
        })
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// Arguments every mutating tool shares.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyArgs {
    #[serde(default)]
    idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateChildArgs {
    title: String,
    #[serde(default)]
    description: Option<String>,
    /// Default: the Chat's own item.
    #[serde(default)]
    parent_work_item_id: Option<String>,
    /// `pending` (default) or `active`.
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    placement: Option<WorkOutlinePlacement>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssociateArgs {
    work_item_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetStatusArgs {
    /// Default: the Chat's own item.
    #[serde(default)]
    work_item_id: Option<String>,
    status: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateUserActionRequestArgs {
    request: String,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    dedupe_key: Option<String>,
    #[serde(default)]
    message_id: Option<String>,
    #[serde(default)]
    tool_call_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolveUserActionRequestArgs {
    user_action_request_id: String,
    #[serde(default)]
    resolution: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProposeInstructionArgs {
    /// `item` (default), `parent`, or `spaceRoot`.
    #[serde(default)]
    target: Option<String>,
    text: String,
    #[serde(default)]
    message_id: Option<String>,
    #[serde(default)]
    tool_call_id: Option<String>,
}

/// Only the prompt and attachments come from the caller; every other Run
/// setting is the child Chat's own (rebuilt engine-side).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueChildTurnArgs {
    chat_id: String,
    prompt: String,
    #[serde(default)]
    attachments: Vec<String>,
}

/// Most attachments a child's first Turn may carry.
const MAX_CHILD_ATTACHMENTS: usize = 16;

fn parse_args<T: serde::de::DeserializeOwned>(arguments: &Value) -> Result<T, AgentToolError> {
    let arguments = if arguments.is_null() {
        json!({})
    } else {
        arguments.clone()
    };
    serde_json::from_value(arguments)
        .map_err(|e| AgentToolError::invalid(format!("invalid arguments: {e}")))
}

/// The authenticated caller. Holds no outline: reads that need one build it
/// themselves (only `read_current` materializes a snapshot).
struct CallContext<'a> {
    grant: AgentGrant,
    workspace: &'a WorkspaceHost,
}

impl CallContext<'_> {
    /// The work item the calling Chat is associated with (the same lookup
    /// the shared agent policy uses), if any.
    fn current_item_id(&self) -> Option<String> {
        self.workspace
            .chat_assigned_work_item(&self.grant.chat_id, &self.grant.space_id)
    }

    /// The Chat's item, or `unauthorized` naming what the agent tried.
    fn require_item(&self, what: &str) -> Result<String, AgentToolError> {
        self.current_item_id().ok_or_else(|| {
            WorkOutlineError::unauthorized(format!(
                "This Chat is not associated with a work item; associate it before {what}"
            ))
            .into()
        })
    }

    fn operation_id(&self, arguments: &Value) -> Result<String, AgentToolError> {
        let args: KeyArgs = parse_args(arguments)?;
        let key = agent_tools::validate_idempotency_key(args.idempotency_key.as_deref())
            .map_err(AgentToolError::invalid)?;
        Ok(agent_tools::operation_id(&self.grant.chat_id, key))
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

/// The engine's agent RPC surface (one per `EngineRpc`).
pub struct AgentToolsService {
    tokens: Arc<AgentTokens>,
    workspace: WorkspaceHost,
    work_outline: Arc<WorkOutlineService>,
    doc_host: Option<DocHost>,
}

impl AgentToolsService {
    pub fn new(
        tokens: Arc<AgentTokens>,
        workspace: WorkspaceHost,
        work_outline: Arc<WorkOutlineService>,
        doc_host: Option<DocHost>,
    ) -> Self {
        Self {
            tokens,
            workspace,
            work_outline,
            doc_host,
        }
    }

    pub fn tokens(&self) -> &Arc<AgentTokens> {
        &self.tokens
    }

    /// `AgentAuthenticate`: who the token speaks for (token lookup and the
    /// Chat row only; no outline is materialized).
    pub fn authenticate(&self, token: &str) -> Result<AgentOrigin, AgentToolError> {
        Ok(self.context(token)?.grant.origin())
    }

    fn context(&self, token: &str) -> Result<CallContext<'_>, AgentToolError> {
        let grant = self.tokens.authenticate(token)?;
        // The Chat must still exist in the Space the token was minted for
        // (Space is write-once; a deleted Chat ends the grant's use).
        let chat = self
            .workspace
            .chat(&grant.chat_id)
            .ok()
            .flatten()
            .ok_or_else(AgentToolError::unauthenticated)?;
        if chat.space_id.as_deref() != Some(grant.space_id.as_str()) {
            return Err(AgentToolError::unauthenticated());
        }
        Ok(CallContext {
            grant,
            workspace: &self.workspace,
        })
    }

    /// `AgentToolCall`: authenticate, authorize, and run one tool.
    pub fn call(&self, params: &AgentToolCallParams) -> Result<Value, AgentToolError> {
        let context = self.context(&params.token)?;
        let arguments = &params.arguments;
        match params.tool.as_str() {
            tools::READ_CURRENT => Ok(self.read_current(&context)),
            tools::CREATE_CHILD => self.create_child(&context, arguments),
            tools::ASSOCIATE_CURRENT_CHAT => self.associate_current_chat(&context, arguments),
            tools::SET_STATUS => self.set_status(&context, arguments),
            tools::CREATE_USER_ACTION_REQUEST => {
                self.create_user_action_request(&context, arguments)
            }
            tools::RESOLVE_USER_ACTION_REQUEST => {
                self.resolve_user_action_request(&context, arguments)
            }
            tools::PROPOSE_INSTRUCTION => self.propose_instruction(&context, arguments),
            tools::QUEUE_CHILD_CHAT_TURN => self.queue_child_chat_turn(&context, arguments),
            other => Err(AgentToolError::refused(
                AgentToolErrorCode::Unsupported,
                format!("unknown agent tool: {other}"),
            )),
        }
    }

    /// Apply one mutation as the token's Chat: `mutate_as` with the agent
    /// actor runs the shared policy under the registry lock, then the kill
    /// switch, the durable replay ledger, and one registry write.
    fn apply(
        &self,
        context: &CallContext,
        mutation: &WorkOutlineMutation,
    ) -> Result<WorkOutlineMutateReply, AgentToolError> {
        let actor = self.work_outline.agent_actor(&context.grant.chat_id, None);
        Ok(self.work_outline.mutate_as(mutation, &actor)?)
    }

    // ---- tools ---------------------------------------------------------------

    fn read_current(&self, context: &CallContext) -> Value {
        let unassociated = json!({
            "chatId": context.grant.chat_id,
            "spaceId": context.grant.space_id,
            "workItem": null,
            "effectiveInstructions": null,
            "openUserActionRequests": [],
            "note": "This Chat is not associated with a work item. Use \
                     work_outline_associate_current_chat to associate it with one of this \
                     Space's items before using the other Work Outline tools.",
        });
        // The shared read model: the item, its effective instructions, and
        // the open requests this Chat raised, from ONE snapshot (the path
        // and children below read the same one).
        let snapshot = self.work_outline.snapshot(&context.grant.space_id);
        let Ok(agent) = self.work_outline.agent_context_in(
            &context.grant.chat_id,
            &context.grant.space_id,
            &snapshot,
        ) else {
            return unassociated;
        };
        let item = &agent.work_item;
        let mut path = Vec::new();
        let mut cursor = Some(item.id.clone());
        while let Some(id) = cursor {
            let Some(node) = snapshot.item(&id) else {
                break;
            };
            path.push(json!({ "id": node.id, "title": node.title }));
            if path.len() > snapshot.items.len() {
                break;
            }
            cursor = node.parent_work_item_id.clone();
        }
        path.reverse();
        let children: Vec<Value> = item
            .child_ids
            .iter()
            .filter_map(|id| snapshot.item(id))
            .filter(|child| child.archived_at.is_none())
            .map(|child| {
                json!({
                    "id": child.id,
                    "title": child.title,
                    "status": child.status,
                    "effectiveStatus": child.effective_status,
                })
            })
            .collect();
        json!({
            "chatId": agent.chat_id,
            "spaceId": agent.space_id,
            "workItem": {
                "id": item.id,
                "title": item.title,
                "description": item.description,
                "status": item.status,
                "effectiveStatus": item.effective_status,
                "parentWorkItemId": item.parent_work_item_id,
                "archived": item.archived_at.is_some(),
                "path": path,
                "children": children,
            },
            "effectiveInstructions": agent.effective_instructions,
            "openUserActionRequests": agent.open_user_action_requests,
            "note": "You may create children under this item or its descendants, set their \
                     status to active or completed, ask the user for an action on this item, \
                     resolve requests you raised, and propose instructions for it, its parent, \
                     or the Space root.",
        })
    }

    fn create_child(
        &self,
        context: &CallContext,
        arguments: &Value,
    ) -> Result<Value, AgentToolError> {
        let operation_id = context.operation_id(arguments)?;
        let args: CreateChildArgs = parse_args(arguments)?;
        let status = match args.status.as_deref().map(str::trim) {
            None | Some("") | Some("pending") => None,
            Some("active") => Some(WorkItemStatus::Active),
            Some(other) => {
                return Err(AgentToolError::invalid(format!(
                    "status must be pending or active for a new item, not {other:?}"
                )));
            }
        };
        let parent = match non_empty(args.parent_work_item_id) {
            Some(id) => Some(id),
            None => context.current_item_id(),
        };
        let mutation = WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: context.grant.space_id.clone(),
            parent_work_item_id: parent.clone(),
            placement: args.placement.unwrap_or_default(),
            title: args.title,
            description: non_empty(args.description),
            status,
            operation_id,
        });
        let reply = self.apply(context, &mutation)?;
        Ok(json!({
            "workItemId": reply.work_item_id,
            "parentWorkItemId": parent,
            "createdByChatId": context.grant.chat_id,
        }))
    }

    fn associate_current_chat(
        &self,
        context: &CallContext,
        arguments: &Value,
    ) -> Result<Value, AgentToolError> {
        let operation_id = context.operation_id(arguments)?;
        let args: AssociateArgs = parse_args(arguments)?;
        let mutation = WorkOutlineMutation::AssignChatToWorkItem(AssignChatToWorkItemParams {
            chat_id: context.grant.chat_id.clone(),
            work_item_id: args.work_item_id.clone(),
            operation_id,
        });
        self.apply(context, &mutation)?;
        Ok(json!({ "chatId": context.grant.chat_id, "workItemId": args.work_item_id }))
    }

    fn set_status(
        &self,
        context: &CallContext,
        arguments: &Value,
    ) -> Result<Value, AgentToolError> {
        let operation_id = context.operation_id(arguments)?;
        let args: SetStatusArgs = parse_args(arguments)?;
        let status = match args.status.trim() {
            "active" => WorkItemStatus::Active,
            "completed" => WorkItemStatus::Completed,
            other => {
                return Err(AgentToolError::invalid(format!(
                    "status must be active or completed, not {other:?}"
                )));
            }
        };
        let target = match non_empty(args.work_item_id) {
            Some(id) => id,
            None => context.require_item("changing status")?,
        };
        // `resolveOpenRequests` stays false: completing an item with open
        // requests fails (`openUserActionRequests`), never auto-resolves.
        let mutation = WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: target.clone(),
            changes: WorkItemChanges {
                status: Some(status),
                ..WorkItemChanges::default()
            },
            operation_id,
        });
        self.apply(context, &mutation)?;
        Ok(json!({ "workItemId": target, "status": status }))
    }

    fn create_user_action_request(
        &self,
        context: &CallContext,
        arguments: &Value,
    ) -> Result<Value, AgentToolError> {
        let operation_id = context.operation_id(arguments)?;
        let args: CreateUserActionRequestArgs = parse_args(arguments)?;
        if args.request.trim().is_empty() {
            return Err(AgentToolError::invalid("request is empty"));
        }
        // Requests are created on the Chat's own item, with the Chat as origin.
        let work_item_id = context.require_item("asking the user for an action")?;
        let mutation =
            WorkOutlineMutation::CreateUserActionRequest(CreateUserActionRequestParams {
                work_item_id: work_item_id.clone(),
                origin: UserActionRequestOrigin {
                    chat_id: context.grant.chat_id.clone(),
                    message_id: non_empty(args.message_id),
                    tool_call_id: non_empty(args.tool_call_id),
                },
                text: args.request,
                info: non_empty(args.detail),
                dedupe_key: non_empty(args.dedupe_key),
                operation_id,
            });
        let reply = self.apply(context, &mutation)?;
        Ok(json!({
            "userActionRequestId": reply.user_action_request_id,
            "workItemId": work_item_id,
        }))
    }

    fn resolve_user_action_request(
        &self,
        context: &CallContext,
        arguments: &Value,
    ) -> Result<Value, AgentToolError> {
        let operation_id = context.operation_id(arguments)?;
        let args: ResolveUserActionRequestArgs = parse_args(arguments)?;
        if args.user_action_request_id.trim().is_empty() {
            return Err(AgentToolError::invalid("userActionRequestId is empty"));
        }
        let mutation =
            WorkOutlineMutation::ResolveUserActionRequest(ResolveUserActionRequestParams {
                user_action_request_id: args.user_action_request_id.clone(),
                note: non_empty(args.resolution),
                operation_id,
            });
        self.apply(context, &mutation)?;
        Ok(json!({ "userActionRequestId": args.user_action_request_id, "resolved": true }))
    }

    fn propose_instruction(
        &self,
        context: &CallContext,
        arguments: &Value,
    ) -> Result<Value, AgentToolError> {
        let operation_id = context.operation_id(arguments)?;
        let args: ProposeInstructionArgs = parse_args(arguments)?;
        if args.text.trim().is_empty() {
            return Err(AgentToolError::invalid("text is empty"));
        }
        let item = context.require_item("proposing instructions")?;
        let target = match args.target.as_deref().map(str::trim).unwrap_or("item") {
            "item" => InstructionProposalTarget::work_item(item),
            "spaceRoot" => InstructionProposalTarget::space_root(),
            "parent" => match self
                .workspace
                .effective_work_item_parent(&context.grant.space_id, &item)
            {
                Some(parent) => InstructionProposalTarget::work_item(parent),
                None => {
                    return Err(WorkOutlineError::unauthorized(
                        "This Chat's work item has no parent; propose for the item or the \
                         Space root",
                    )
                    .into());
                }
            },
            other => {
                return Err(AgentToolError::invalid(format!(
                    "target must be item, parent, or spaceRoot, not {other:?}"
                )));
            }
        };
        let mutation =
            WorkOutlineMutation::ProposeWorkItemInstruction(ProposeWorkItemInstructionParams {
                space_id: Some(context.grant.space_id.clone()),
                target: target.clone(),
                origin: InstructionProposalOrigin {
                    chat_id: Some(context.grant.chat_id.clone()),
                    message_id: non_empty(args.message_id),
                    tool_call_id: non_empty(args.tool_call_id),
                },
                text: args.text,
                operation_id,
            });
        let reply = self.apply(context, &mutation)?;
        Ok(json!({
            "proposalId": reply.proposal_id,
            "target": target,
            "state": "pending",
            "note": "The user reviews the proposal; nothing changes until they accept it.",
        }))
    }

    /// Queue the first Turn of a child Chat created by the MCP `create_chat`
    /// tool. Every id derives from `(token Chat, idempotencyKey)`, so a retry
    /// (or a second call with the same key) never queues a second Turn; the
    /// Chat must be the one that key names and a child of the token's Chat.
    /// The Run request is rebuilt here from the child's stored config (the
    /// caller supplies only the prompt and attachments), so an agent can
    /// never widen the child's sandbox, approval mode, cwd, or harness.
    fn queue_child_chat_turn(
        &self,
        context: &CallContext,
        arguments: &Value,
    ) -> Result<Value, AgentToolError> {
        let operation_id = context.operation_id(arguments)?;
        let args: QueueChildTurnArgs = parse_args(arguments)?;
        let chat_id = derive_id(&operation_id, roles::CHAT);
        if args.chat_id != chat_id {
            return Err(AgentToolError::refused(
                AgentToolErrorCode::Forbidden,
                "chatId is not the Chat this idempotencyKey created",
            ));
        }
        let chat = self
            .workspace
            .chat(&chat_id)
            .ok()
            .flatten()
            .ok_or_else(|| AgentToolError::invalid("the child Chat does not exist yet"))?;
        if chat.parent_chat_id.as_deref() != Some(context.grant.chat_id.as_str()) {
            return Err(AgentToolError::refused(
                AgentToolErrorCode::Forbidden,
                "agents may only start Chats they created",
            ));
        }
        if args.prompt.trim().is_empty() {
            return Err(AgentToolError::invalid("prompt is empty"));
        }
        if args.attachments.len() > MAX_CHILD_ATTACHMENTS
            || args
                .attachments
                .iter()
                .any(|path| !std::path::Path::new(path).is_absolute())
        {
            return Err(AgentToolError::invalid(format!(
                "attachments must be at most {MAX_CHILD_ATTACHMENTS} absolute paths"
            )));
        }
        let space = chat
            .space_id
            .as_deref()
            .and_then(|id| self.workspace.space(id).ok().flatten());
        let request = child_run_request(&chat, space.as_ref(), args.prompt, args.attachments);
        let message_id = derive_id(&operation_id, roles::MESSAGE);
        let command_id = derive_id(&operation_id, roles::COMMAND);
        let payload = SessionCommandPayload::Run {
            request,
            message_id: message_id.clone(),
        };
        let doc_host = self.doc_host.as_ref().ok_or_else(|| {
            AgentToolError::refused(AgentToolErrorCode::Unsupported, "no chat host")
        })?;
        let queued = doc_host
            .queue_command_with_id(&chat_id, &command_id, payload)
            .map_err(|e| AgentToolError::invalid(e.to_string()))?;
        Ok(json!({
            "chatId": chat_id,
            "messageId": message_id,
            "commandId": command_id,
            "queued": queued,
        }))
    }
}

/// The first Run of a child Chat, from the child's stored config and row
/// only: what a composer send in that Chat would queue (its harness, model,
/// sandbox, approval mode, and cwd; the host overwrites the cwd of an
/// execution-bound Chat and resolves instructions at dispatch).
fn child_run_request(
    chat: &zeron_proto::Chat,
    space: Option<&zeron_proto::Space>,
    prompt: String,
    attachments: Vec<String>,
) -> RunRequest {
    let config = chat.config.as_ref();
    RunRequest {
        prompt,
        harness: config.map(|c| c.harness),
        model: config.and_then(|c| c.model.clone()),
        reasoning: config.and_then(|c| c.reasoning),
        model_options: config.map(|c| c.model_options.clone()).unwrap_or_default(),
        cwd: chat
            .cwd
            .clone()
            .or_else(|| space.map(|s| s.path.clone()))
            .unwrap_or_else(|| "~".into()),
        sandbox: config
            .map(|c| c.sandbox)
            .unwrap_or(SandboxLevel::WorkspaceWrite),
        auto_approve: config.is_some_and(|c| c.auto_approve),
        resume: None,
        attachments,
        worktree: None,
        instructions: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arc() -> Arc<AgentTokens> {
        AgentTokens::new()
    }

    #[test]
    fn tokens_are_random_bound_and_revoked_with_their_lease() {
        let tokens = arc();
        let lease = tokens.mint("chat-1", "run-1", "space-1", "dev-1");
        let other = tokens.mint("chat-1", "run-2", "space-1", "dev-1");
        assert_ne!(lease.token(), other.token());
        assert!(lease.token().len() >= 64);
        let grant = tokens.authenticate(lease.token()).unwrap();
        assert_eq!(
            (
                grant.chat_id.as_str(),
                grant.run_id.as_str(),
                grant.space_id.as_str()
            ),
            ("chat-1", "run-1", "space-1")
        );
        let token = lease.token().to_owned();
        drop(lease);
        assert_eq!(
            tokens.authenticate(&token).unwrap_err().code(),
            "unauthenticated"
        );
        assert!(tokens.authenticate(other.token()).is_ok());
        assert_eq!(tokens.live_count(), 1);
        drop(other);
        assert_eq!(tokens.live_count(), 0);
    }

    #[test]
    fn forged_absent_and_expired_tokens_are_rejected() {
        let tokens = arc();
        let lease = tokens.mint("chat-1", "run-1", "space-1", "dev-1");
        for forged in ["", "   ", "zat_00000000", &format!("{}x", lease.token())] {
            let err = tokens.authenticate(forged).unwrap_err();
            assert_eq!(err.code(), "unauthenticated", "{forged:?}");
            assert!(
                err.to_wire_message()
                    .starts_with("agent-tools/unauthenticated: ")
            );
        }
        let short =
            tokens.mint_with_ttl("chat-1", "run-2", "space-1", "dev-1", Some(Duration::ZERO));
        assert!(tokens.authenticate(short.token()).is_err());
        let long = tokens.mint_with_ttl(
            "chat-1",
            "run-3",
            "space-1",
            "dev-1",
            Some(Duration::from_secs(3600)),
        );
        assert!(tokens.authenticate(long.token()).is_ok());
    }

    #[test]
    fn a_lease_outliving_its_registry_is_harmless() {
        let tokens = arc();
        let lease = tokens.mint("c", "r", "s", "d");
        drop(tokens);
        drop(lease);
    }

    #[test]
    fn injection_carries_the_token_in_env_only() {
        let launch = AgentToolsLaunch {
            command: PathBuf::from("/bin/zeron"),
            args: vec!["mcp".into()],
            ipc_port: Some(27655),
        };
        let injection = launch.injection("zat_secret").unwrap();
        assert_eq!(injection.server_name, "zeron-agent");
        assert_eq!(injection.args, vec!["mcp".to_owned()]);
        assert_eq!(injection.env["ZERON_AGENT_TOKEN"], "zat_secret");
        assert_eq!(injection.env["ZERON_IPC_PORT"], "27655");
        assert!(!injection.args.iter().any(|a| a.contains("zat_secret")));
        assert!(!injection.command.to_string_lossy().contains("zat_secret"));
    }

    #[test]
    fn default_launch_dials_the_bound_port_or_injects_nothing() {
        let launch = AgentToolsLaunch {
            command: PathBuf::from("/bin/zeron"),
            args: vec!["mcp".into()],
            ipc_port: None,
        };
        // Process-global: this is the only unit test that touches it.
        note_bound_ipc_port(0);
        assert!(
            launch.injection("zat_x").is_none(),
            "no bound port, no tools"
        );
        note_bound_ipc_port(41234);
        assert_eq!(
            launch.injection("zat_x").unwrap().env["ZERON_IPC_PORT"],
            "41234"
        );
        note_bound_ipc_port(0);
    }

    #[test]
    fn minted_tokens_expire_absolutely_unless_renewed() {
        let tokens = arc();
        let lease = tokens.mint("c", "r", "s", "d");
        let grant = tokens.authenticate(lease.token()).unwrap();
        assert_eq!(grant.ttl, Some(DEFAULT_TOKEN_TTL));
        let at = grant.expires_at.expect("an absolute expiry");
        assert!(at > Instant::now() + DEFAULT_TOKEN_TTL - Duration::from_secs(60));

        // Renewal pushes a live token's expiry out by its TTL.
        let short = tokens.mint_with_ttl("c", "r2", "s", "d", Some(Duration::from_millis(200)));
        std::thread::sleep(Duration::from_millis(120));
        short.renew();
        std::thread::sleep(Duration::from_millis(120));
        assert!(
            tokens.authenticate(short.token()).is_ok(),
            "renewed in time"
        );
        // A driver that stops renewing loses the token though it holds the lease.
        std::thread::sleep(Duration::from_millis(250));
        assert!(tokens.authenticate(short.token()).is_err());
        short.renew();
        assert!(tokens.authenticate(short.token()).is_err(), "never revived");
    }

    #[test]
    fn child_runs_take_only_prompt_and_attachments_from_the_caller() {
        let chat: zeron_proto::Chat = serde_json::from_value(json!({
            "id": "child", "deviceId": "dev", "archived": false,
            "createdAt": "2026-01-01T00:00:00Z",
            "config": { "harness": "codex", "model": "m-1", "sandbox": "read-only",
                        "autoApprove": false },
        }))
        .unwrap();
        let space: zeron_proto::Space = serde_json::from_value(json!({
            "id": "s", "deviceId": "dev", "path": "/repo", "createdAt": "2026-01-01T00:00:00Z",
        }))
        .unwrap();
        let request =
            child_run_request(&chat, Some(&space), "go".into(), vec!["/tmp/a.png".into()]);
        assert_eq!(request.prompt, "go");
        assert_eq!(request.attachments, vec!["/tmp/a.png".to_owned()]);
        assert_eq!(request.harness, Some(zeron_proto::HarnessId::Codex));
        assert_eq!(request.model.as_deref(), Some("m-1"));
        assert_eq!(request.sandbox, SandboxLevel::ReadOnly);
        assert!(!request.auto_approve);
        assert_eq!(request.cwd, "/repo");
        assert!(request.worktree.is_none() && request.instructions.is_none());
    }
}
