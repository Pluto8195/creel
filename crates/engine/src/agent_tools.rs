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
//!   token is kept. An optional absolute expiry backs up the lease.
//! - [`AgentToolsService`]: `AgentAuthenticate` / `AgentToolCall`. Every call
//!   authenticates by token; the originating Chat is the token's, never a
//!   client-supplied id. Mutations run as `Actor { kind: agent, chatId }`,
//!   pass the agent policy ([`AgentToolsService::authorize_agent`]), and
//!   derive their `operationId` from `(token Chat, idempotencyKey)`, so a
//!   retry with the same key dedupes across Runs and processes.
//!
//! The policy mirrors `WorkOutlineService::authorize_agent` (owned by the
//! user-actions branch) for the operations that exist on this branch:
//! read the Chat's item and effective instructions; create a child under the
//! Chat's item or a descendant; associate only the calling Chat; set status
//! `active` / `completed` on the Chat's item or a descendant (completing an
//! item with open requests is refused, never auto-resolved). Everything else
//! is refused.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zeron_doc::SessionCommandPayload;
use zeron_harness::AgentToolsInjection;
use zeron_proto::agent_tools::{self, AgentOrigin, AgentToolCallParams, AgentToolErrorCode, tools};
use zeron_proto::work_outline::{WorkOutlineErrorCode, derive_id, roles};
use zeron_proto::{
    AssignChatToWorkItemParams, CreateWorkItemParams, UpdateWorkItemParams, WorkItemChanges,
    WorkItemChatAssignmentState, WorkItemEffectiveStatus, WorkItemStatus, WorkItemView,
    WorkOutlineActor, WorkOutlineActorKind, WorkOutlineError, WorkOutlineMutateReply,
    WorkOutlineMutation, WorkOutlinePlacement, WorkOutlineSnapshot,
};

use crate::doc_host::DocHost;
use crate::work_outline::{ledger_inputs_key, ledger_operation_key, writes_disabled};
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

/// The engine's live per-Run tokens (one registry per engine).
#[derive(Default)]
pub struct AgentTokens {
    grants: Mutex<HashMap<TokenKey, AgentGrant>>,
}

impl AgentTokens {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Mint a token for one Run. Valid until the returned lease drops.
    pub fn mint(
        self: &Arc<Self>,
        chat_id: &str,
        run_id: &str,
        space_id: &str,
        device_id: &str,
    ) -> AgentTokenLease {
        self.mint_with_ttl(chat_id, run_id, space_id, device_id, None)
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

/// How a Run reaches this engine's agent surface: the `zeron mcp` command
/// and the loopback IPC port it dials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentToolsLaunch {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub ipc_port: u16,
}

impl AgentToolsLaunch {
    /// The running `zeron` binary's `mcp` subcommand and the IPC port the
    /// engine serves (`ZERON_IPC_PORT`, default 27654, the same resolution
    /// the binary's engine config uses). `None` when the executable path is
    /// unknown.
    pub fn from_current_exe() -> Option<Self> {
        let command = std::env::current_exe().ok()?;
        let ipc_port = std::env::var(agent_tools::IPC_PORT_ENV)
            .ok()
            .and_then(|port| port.trim().parse().ok())
            .unwrap_or(27654);
        Some(Self {
            command,
            args: vec!["mcp".into()],
            ipc_port,
        })
    }

    /// The injection for one Run holding `token`.
    pub fn injection(&self, token: &str) -> AgentToolsInjection {
        AgentToolsInjection {
            server_name: agent_tools::SERVER_NAME.into(),
            command: self.command.clone(),
            args: self.args.clone(),
            env: [
                (agent_tools::TOKEN_ENV.to_owned(), token.to_owned()),
                (
                    agent_tools::IPC_PORT_ENV.to_owned(),
                    self.ipc_port.to_string(),
                ),
            ]
            .into_iter()
            .collect(),
        }
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
    tool_call_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueChildTurnArgs {
    chat_id: String,
    command: SessionCommandPayload,
}

fn parse_args<T: serde::de::DeserializeOwned>(arguments: &Value) -> Result<T, AgentToolError> {
    let arguments = if arguments.is_null() {
        json!({})
    } else {
        arguments.clone()
    };
    serde_json::from_value(arguments)
        .map_err(|e| AgentToolError::invalid(format!("invalid arguments: {e}")))
}

/// The authenticated caller plus its Space's current outline.
struct CallContext {
    grant: AgentGrant,
    snapshot: WorkOutlineSnapshot,
}

impl CallContext {
    /// The work item the calling Chat is associated with, if any.
    fn current_item(&self) -> Option<&WorkItemView> {
        current_item_of(&self.snapshot, &self.grant.chat_id)
    }

    fn operation_id(&self, arguments: &Value) -> Result<String, AgentToolError> {
        let args: KeyArgs = parse_args(arguments)?;
        let key = agent_tools::validate_idempotency_key(args.idempotency_key.as_deref())
            .map_err(AgentToolError::invalid)?;
        Ok(agent_tools::operation_id(&self.grant.chat_id, key))
    }

    fn actor(&self) -> WorkOutlineActor {
        WorkOutlineActor {
            kind: WorkOutlineActorKind::Agent,
            device_id: self.grant.device_id.clone(),
            chat_id: Some(self.grant.chat_id.clone()),
            message_id: None,
        }
    }
}

fn current_item_of<'a>(
    snapshot: &'a WorkOutlineSnapshot,
    chat_id: &str,
) -> Option<&'a WorkItemView> {
    let assignment = snapshot.assignments.iter().find(|assignment| {
        assignment.chat_id == chat_id && assignment.state == WorkItemChatAssignmentState::Associated
    })?;
    snapshot.item(assignment.work_item_id.as_deref()?)
}

/// Whether `candidate` is `root` or one of its descendants (repaired tree).
fn within_subtree(snapshot: &WorkOutlineSnapshot, root: &str, candidate: &str) -> bool {
    let mut cursor = Some(candidate.to_owned());
    let mut steps = 0usize;
    while let Some(id) = cursor {
        if id == root {
            return true;
        }
        steps += 1;
        if steps > snapshot.items.len() + 1 {
            return false;
        }
        cursor = snapshot
            .item(&id)
            .and_then(|item| item.parent_work_item_id.clone());
    }
    false
}

fn denied(message: impl Into<String>) -> WorkOutlineError {
    WorkOutlineError::new(WorkOutlineErrorCode::Invalid, message)
}

fn has_open_requests(item: &WorkItemView) -> bool {
    item.rollup.unresolved_user_action_count > 0
        || item.effective_status == WorkItemEffectiveStatus::UserActionRequired
}

/// The engine's agent RPC surface (one per `EngineRpc`).
pub struct AgentToolsService {
    tokens: Arc<AgentTokens>,
    workspace: WorkspaceHost,
    doc_host: Option<DocHost>,
    /// Serializes replay-check-then-apply so two concurrent retries of one
    /// key cannot both pass the ledger check.
    gate: Mutex<()>,
}

impl AgentToolsService {
    pub fn new(
        tokens: Arc<AgentTokens>,
        workspace: WorkspaceHost,
        doc_host: Option<DocHost>,
    ) -> Self {
        Self {
            tokens,
            workspace,
            doc_host,
            gate: Mutex::new(()),
        }
    }

    pub fn tokens(&self) -> &Arc<AgentTokens> {
        &self.tokens
    }

    /// `AgentAuthenticate`: who the token speaks for.
    pub fn authenticate(&self, token: &str) -> Result<AgentOrigin, AgentToolError> {
        Ok(self.context(token)?.grant.origin())
    }

    fn context(&self, token: &str) -> Result<CallContext, AgentToolError> {
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
        let inputs = self.workspace.work_outline_inputs(&grant.space_id);
        let snapshot = zeron_doc::materialize_work_outline(&grant.space_id, &inputs);
        Ok(CallContext { grant, snapshot })
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

    /// The agent policy for one Work Outline mutation by `actor_chat_id`.
    /// Mirrors `WorkOutlineService::authorize_agent`; denials are `invalid`
    /// with a message naming the rule.
    pub fn authorize_agent(
        &self,
        actor_chat_id: &str,
        mutation: &WorkOutlineMutation,
    ) -> Result<(), WorkOutlineError> {
        let space_id = self
            .workspace
            .chat(actor_chat_id)
            .ok()
            .flatten()
            .and_then(|chat| chat.space_id)
            .ok_or_else(|| {
                WorkOutlineError::new(
                    WorkOutlineErrorCode::Projectless,
                    "Agent tools need a Chat in a Space",
                )
            })?;
        let inputs = self.workspace.work_outline_inputs(&space_id);
        let snapshot = zeron_doc::materialize_work_outline(&space_id, &inputs);
        authorize_agent_in(&snapshot, actor_chat_id, mutation)
    }

    /// Apply one authorized mutation as the agent: kill switch, durable
    /// replay ledger (same keys as `WorkOutlineMutate`), one registry write.
    fn apply(
        &self,
        context: &CallContext,
        mutation: &WorkOutlineMutation,
    ) -> Result<WorkOutlineMutateReply, AgentToolError> {
        authorize_agent_in(&context.snapshot, &context.grant.chat_id, mutation)?;
        if writes_disabled() {
            return Err(WorkOutlineError::new(
                WorkOutlineErrorCode::WritesDisabled,
                "Work Outline changes are turned off on this device",
            )
            .into());
        }
        let operation_id = mutation.operation_id().to_owned();
        let hash = zeron_doc::registry::work_outline::inputs_hash(mutation);
        let _gate = lock(&self.gate);
        let operation_key = ledger_operation_key(&operation_id);
        let inputs_key = ledger_inputs_key(&hash, &operation_id);
        if self.workspace.ledger_holds(&operation_key) {
            if !self.workspace.ledger_holds(&inputs_key) {
                return Err(WorkOutlineError::new(
                    WorkOutlineErrorCode::OperationConflict,
                    "idempotencyKey was already used by this Chat for a different change",
                )
                .into());
            }
            return Ok(replay_reply(mutation));
        }
        Ok(self.workspace.work_outline_mutate(
            mutation,
            &context.actor(),
            &[operation_key, inputs_key],
        )?)
    }

    // ---- tools ---------------------------------------------------------------

    fn read_current(&self, context: &CallContext) -> Value {
        let item = context.current_item();
        let path: Vec<Value> = item
            .map(|item| {
                let mut chain = Vec::new();
                let mut cursor = Some(item.id.clone());
                while let Some(id) = cursor {
                    let Some(node) = context.snapshot.item(&id) else {
                        break;
                    };
                    chain.push(json!({ "id": node.id, "title": node.title }));
                    if chain.len() > context.snapshot.items.len() {
                        break;
                    }
                    cursor = node.parent_work_item_id.clone();
                }
                chain.reverse();
                chain
            })
            .unwrap_or_default();
        let children: Vec<Value> = item
            .map(|item| {
                item.child_ids
                    .iter()
                    .filter_map(|id| context.snapshot.item(id))
                    .filter(|child| child.archived_at.is_none())
                    .map(|child| {
                        json!({
                            "id": child.id,
                            "title": child.title,
                            "status": child.status,
                            "effectiveStatus": child.effective_status,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        json!({
            "chatId": context.grant.chat_id,
            "spaceId": context.grant.space_id,
            "workItem": item.map(|item| json!({
                "id": item.id,
                "title": item.title,
                "description": item.description,
                "status": item.status,
                "effectiveStatus": item.effective_status,
                "parentWorkItemId": item.parent_work_item_id,
                "archived": item.archived_at.is_some(),
                "path": path,
                "children": children,
            })),
            "effectiveInstructions": item.map(|item| {
                serde_json::to_value(context.snapshot.effective_instructions(&item.id))
                    .unwrap_or(Value::Null)
            }),
            "note": if item.is_some() {
                "You may create children under this item or its descendants, set their status \
                 to active or completed, ask the user for an action on this item, and propose \
                 instructions for it, its parent, or the Space root."
            } else {
                "This Chat is not associated with a work item. Use \
                 work_outline_associate_current_chat to associate it with one of this Space's \
                 items before using the other Work Outline tools."
            },
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
        let parent = match args.parent_work_item_id.filter(|id| !id.trim().is_empty()) {
            Some(id) => Some(id),
            None => context.current_item().map(|item| item.id.clone()),
        };
        let mutation = WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: context.grant.space_id.clone(),
            parent_work_item_id: parent.clone(),
            placement: args.placement.unwrap_or_default(),
            title: args.title,
            description: args.description.filter(|d| !d.trim().is_empty()),
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
        let target = match args.work_item_id.filter(|id| !id.trim().is_empty()) {
            Some(id) => id,
            None => context
                .current_item()
                .map(|item| item.id.clone())
                .ok_or_else(|| {
                    AgentToolError::from(denied(
                        "This Chat is not associated with a work item; pass workItemId or \
                         associate the Chat first",
                    ))
                })?,
        };
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
        let item = context.current_item().ok_or_else(|| {
            AgentToolError::from(denied(
                "This Chat is not associated with a work item; associate it before asking the \
                 user for an action",
            ))
        })?;
        pending_operation(
            tools::CREATE_USER_ACTION_REQUEST,
            json!({
                "workItemId": item.id,
                "originChatId": context.grant.chat_id,
                "request": args.request,
                "detail": args.detail,
                "dedupeKey": args.dedupe_key,
                "toolCallId": args.tool_call_id,
                "operationId": operation_id,
            }),
        )
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
        pending_operation(
            tools::RESOLVE_USER_ACTION_REQUEST,
            json!({
                "userActionRequestId": args.user_action_request_id,
                "resolverChatId": context.grant.chat_id,
                "resolution": args.resolution,
                "operationId": operation_id,
            }),
        )
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
        let item = context.current_item().ok_or_else(|| {
            AgentToolError::from(denied(
                "This Chat is not associated with a work item; associate it before proposing \
                 instructions",
            ))
        })?;
        let scope = match args.target.as_deref().map(str::trim).unwrap_or("item") {
            "item" => json!({ "type": "workItem", "workItemId": item.id }),
            "spaceRoot" => json!({ "type": "spaceRoot" }),
            "parent" if item.parent_work_item_id.is_some() => {
                json!({ "type": "workItem", "workItemId": item.parent_work_item_id })
            }
            "parent" => {
                return Err(AgentToolError::from(denied(
                    "This Chat's work item has no parent; propose for the item or the Space root",
                )));
            }
            other => {
                return Err(AgentToolError::invalid(format!(
                    "target must be item, parent, or spaceRoot, not {other:?}"
                )));
            }
        };
        pending_operation(
            tools::PROPOSE_INSTRUCTION,
            json!({
                "spaceId": context.grant.space_id,
                "scope": scope,
                "originChatId": context.grant.chat_id,
                "text": args.text,
                "toolCallId": args.tool_call_id,
                "operationId": operation_id,
            }),
        )
    }

    /// Queue the first Turn of a child Chat created by the MCP `create_chat`
    /// tool. Every id derives from `(token Chat, idempotencyKey)`, so a retry
    /// (or a second call with the same key) never queues a second Turn; the
    /// Chat must be the one that key names and a child of the token's Chat.
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
        let message_id = derive_id(&operation_id, roles::MESSAGE);
        let command_id = derive_id(&operation_id, roles::COMMAND);
        let payload = match args.command {
            SessionCommandPayload::Run { request, .. } => SessionCommandPayload::Run {
                request,
                message_id: message_id.clone(),
            },
            _ => {
                return Err(AgentToolError::invalid(
                    "the child Chat's first command must be a run",
                ));
            }
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

/// The reply of an already-applied agent mutation, recomputed from its
/// inputs (every id an agent mutation returns is derived).
fn replay_reply(mutation: &WorkOutlineMutation) -> WorkOutlineMutateReply {
    match mutation {
        WorkOutlineMutation::CreateWorkItem(p) => WorkOutlineMutateReply {
            work_item_id: Some(derive_id(&p.operation_id, roles::ITEM)),
            ..Default::default()
        },
        _ => WorkOutlineMutateReply::default(),
    }
}

/// Request, resolve, and proposal tools validate their arguments, authorize
/// against the Chat's item, and derive their `operationId`, then stop here
/// with the would-be mutation's fields in `params`: at the merge of
/// `work-outline-user-actions`, the integrator replaces this call with the
/// `createUserActionRequest` / `resolveUserActionRequest` /
/// `proposeWorkItemInstruction` mutation built from `params` (actor = the
/// agent, origin = the token's Chat) applied through `AgentToolsService::apply`,
/// and switches `authorize_agent_in` to `WorkOutlineService::authorize_agent`.
fn pending_operation(tool: &str, params: Value) -> Result<Value, AgentToolError> {
    tracing::debug!(tool, %params, "agent operation not available on this host");
    Err(AgentToolError::refused(
        AgentToolErrorCode::Unsupported,
        format!("{tool} is not available on this Zeron version yet"),
    ))
}

/// The agent policy over one materialized Space (see the module docs).
fn authorize_agent_in(
    snapshot: &WorkOutlineSnapshot,
    actor_chat_id: &str,
    mutation: &WorkOutlineMutation,
) -> Result<(), WorkOutlineError> {
    let current = current_item_of(snapshot, actor_chat_id);
    let require_subtree = |target: &str, what: &str| -> Result<(), WorkOutlineError> {
        let current = current.ok_or_else(|| {
            denied(format!(
                "This Chat is not associated with a work item, so agents cannot {what}"
            ))
        })?;
        if snapshot.item(target).is_none() {
            return Err(WorkOutlineError::new(
                WorkOutlineErrorCode::NotFound,
                "Work item not found in this Chat's Space",
            ));
        }
        if !within_subtree(snapshot, &current.id, target) {
            return Err(denied(format!(
                "Agents can only {what} on this Chat's work item ({}) or its descendants",
                current.title
            )));
        }
        Ok(())
    };
    match mutation {
        WorkOutlineMutation::CreateWorkItem(p) => {
            if p.space_id != snapshot.space_id {
                return Err(WorkOutlineError::new(
                    WorkOutlineErrorCode::CrossSpace,
                    "Agents can only create items in their Chat's Space",
                ));
            }
            if p.status == Some(WorkItemStatus::Completed) {
                return Err(denied("Agents cannot create completed items"));
            }
            let parent = p.parent_work_item_id.as_deref().ok_or_else(|| {
                denied("Agents cannot create items at the Space root; create a child of this Chat's work item")
            })?;
            require_subtree(parent, "create items")
        }
        WorkOutlineMutation::AssignChatToWorkItem(p) => {
            if p.chat_id != actor_chat_id {
                return Err(denied("Agents can only associate their own Chat"));
            }
            if snapshot.item(&p.work_item_id).is_none() {
                return Err(WorkOutlineError::new(
                    WorkOutlineErrorCode::NotFound,
                    "Work item not found in this Chat's Space",
                ));
            }
            Ok(())
        }
        WorkOutlineMutation::UpdateWorkItem(p) => {
            let changes = &p.changes;
            let status = match changes.status {
                Some(status @ (WorkItemStatus::Active | WorkItemStatus::Completed))
                    if changes.title.is_none()
                        && changes.description.is_unchanged()
                        && changes.inherit_parent_instructions.is_none() =>
                {
                    status
                }
                _ => {
                    return Err(denied("Agents can only mark items active or completed"));
                }
            };
            require_subtree(&p.work_item_id, "change status")?;
            if status == WorkItemStatus::Completed
                && snapshot
                    .item(&p.work_item_id)
                    .is_some_and(has_open_requests)
            {
                return Err(denied(
                    "This item has open user action requests; they must be resolved before it \
                     can be completed (agents never resolve them implicitly)",
                ));
            }
            Ok(())
        }
        _ => Err(denied(
            "This Work Outline change is not available to agents",
        )),
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

    fn outline_with_open_request() -> WorkOutlineSnapshot {
        serde_json::from_value(json!({
            "spaceId": "s",
            "available": true,
            "items": [
                { "id": "root", "spaceId": "s", "title": "Root", "childIds": ["kid"],
                  "rollup": { "unresolvedUserActionCount": 1 },
                  "effectiveStatus": "userActionRequired" },
                { "id": "kid", "spaceId": "s", "parentWorkItemId": "root", "title": "Kid" },
                { "id": "other", "spaceId": "s", "title": "Other" }
            ],
            "assignments": [
                { "chatId": "chat", "spaceId": "s", "workItemId": "root", "state": "associated" }
            ]
        }))
        .unwrap()
    }

    fn status(item: &str, status: &str) -> WorkOutlineMutation {
        serde_json::from_value(json!({ "op": "updateWorkItem", "workItemId": item,
            "changes": { "status": status }, "operationId": "op" }))
        .unwrap()
    }

    #[test]
    fn completing_an_item_with_open_requests_is_refused_never_auto_resolved() {
        let snapshot = outline_with_open_request();
        let err = authorize_agent_in(&snapshot, "chat", &status("root", "completed")).unwrap_err();
        assert!(err.message.contains("open user action requests"), "{err:?}");
        assert!(authorize_agent_in(&snapshot, "chat", &status("root", "active")).is_ok());
        assert!(authorize_agent_in(&snapshot, "chat", &status("kid", "completed")).is_ok());
        assert!(authorize_agent_in(&snapshot, "chat", &status("other", "active")).is_err());
        assert!(authorize_agent_in(&snapshot, "chat", &status("kid", "pending")).is_err());
        // A Chat without an association owns no subtree.
        assert!(authorize_agent_in(&snapshot, "stranger", &status("kid", "active")).is_err());
    }

    #[test]
    fn injection_carries_the_token_in_env_only() {
        let launch = AgentToolsLaunch {
            command: PathBuf::from("/bin/zeron"),
            args: vec!["mcp".into()],
            ipc_port: 27655,
        };
        let injection = launch.injection("zat_secret");
        assert_eq!(injection.server_name, "zeron");
        assert_eq!(injection.args, vec!["mcp".to_owned()]);
        assert_eq!(injection.env["ZERON_AGENT_TOKEN"], "zat_secret");
        assert_eq!(injection.env["ZERON_IPC_PORT"], "27655");
        assert!(!injection.args.iter().any(|a| a.contains("zat_secret")));
    }
}
