# M0 audit: idempotent multi-step operations and the command plane

Branch audited: `work-outline-implementation` (read-only). Spec: `docs/plans/work-outline-implementation-specification.md` (Persistence and convergence, last bullet at :92; Service operations :516-554; Materialization :510-514; acceptance 10, 16, 19 at :603, :609, :612).
All paths below are relative to `/Users/mikey/Projects/agent-mode-tools/zeron`.

## 0. Headline findings

1. There is no operation-id or idempotency-key concept anywhere (RPC or registry). Idempotence today is per-record and structural: "skip if the row already exists", "dedupe by client-minted message id", "processed-command ledger by command id". `QueueCommand` mints the command id inside the engine, so a retried RPC appends a second Run.
2. `SessionDoc::queue_command` pushes unconditionally and is not idempotent by id. A deterministic command id is unsafe until that is fixed (section 2.6).
3. Registry writes are one batch per typed call (`write` at `crates/doc/src/registry.rs:629`). The server applies a batch atomically (`edge/src/registry-room.ts:373-396`), but no public multi-row writer exists. Registry and session-doc are two separate stores, each snapshotted on a 1 s debounce, so a crash can lose the tail of either independently. Atomicity across registry and command plane is impossible; recoverable states are required, as the spec says.
4. Crash recovery and follow-up dispatch rebuild the `RunRequest` from memory or the chat row, not from the original command. An instruction envelope on `RunRequest` would be silently dropped after a restart, steer fallback, or queue promotion unless persisted separately (section 2.7). This is the single biggest risk to acceptance 9/10.
5. `zeron-mcp` exists with 14 tools, but the engine never injects it into any harness, and "origin" is a self-declared env var over an unauthenticated loopback socket. It cannot support "narrowly authorized, origin-aware" tools without new engine-side work (section 5).
6. Spec conflicts and gaps are collected in section 8.

## 1. How a chat is created today

**Who mints the id: the client.** In the composer, `chat_id = uuid::Uuid::new_v4()` when no chat is selected (`crates/ui/src/composer.rs:6412-6415`), and the first user message id is another `Uuid::new_v4()` (`composer.rs:6519`). The MCP `create_chat` tool also mints v4 client-side (`crates/mcp/src/tools.rs:601`). The engine mints only command ids (`crates/engine/src/doc_host.rs:2527`, `new_id()` = v4 at `crates/engine/src/lib.rs:111`) and run ids.

End-to-end flow (new chat from composer `send`, `composer.rs:6403-7260`):

1. Optional pre-steps: `PlanChatWorkspace` / `CreateChatWorktree` RPCs (`composer.rs:6975-7040`), IPC-only and non-idempotent (random worktree names; see section 8 item 9).
2. `Mutate {op:"createChat", chatId, spaceId|deviceId, cwd?, branch?, config?}` (`composer.rs:6915-6965`). The error is swallowed: "doc host will materialize the chat" (`:6964`).
3. Engine: `MutateParams::CreateChat` (`crates/engine/src/rpc.rs:603-636`, handler `:1218-1240`) calls `WorkspaceHost::create_chat_with_parent` (`crates/engine/src/workspace_host.rs:919-985`). It early-returns `Ok(())` if a chat row exists (`:928-930`, "idempotent: optimistic client retries never duplicate"), else `upsert_chat` writes one registry row op (`crates/doc/src/registry.rs:894-948`, `write(KIND_CHATS, ...)`).
4. `QueueCommand {chatId, command: Run{request, messageId}}` (`composer.rs:7089-7110`). The engine arm (`rpc.rs:1885-1892`) calls `DocHost::queue_command_with_transfers` (`doc_host.rs:2518-2557`), which opens the session doc, mints the command id, sets `based_on` from the last message, builds the entry with 24 h TTL, appends it (`SessionDoc::queue_command`, `crates/doc/src/schema.rs:478-512`), unarchives the chat, nudges a remote host, and spawns a relay escort (`doc_host.rs:3375-3440`). The reply is `{commandId}`.
5. Host drain (`doc_host.rs:4037-4261`) then `execute` Run (`:4283-4440`): materialize worktree, `ws.claim_chat` (creates a partial chat row if none, `workspace_host.rs:780-795`), back-fill `ChatConfig`, `write_user_message` (idempotent by message id, `doc_host.rs:687-705`), `dispatch_with_source_context` (`doc_host.rs:4613`) into `SessionsEngine::dispatch` (`crates/engine/src/sessions.rs:382`).
6. On failure of a NEW chat the UI best-effort `deleteChat`s the minted chat and restores the draft (`composer.rs:7180-7260`). A work-item launch must NOT do this: the chat, assignment, and revision may already be synced.

**Retry safety today:**
- Chat row: safe by existence check, but the check is buggy for our purposes. A claim-on-first-command stub row (`registry.rs:951-975`, written when a Run outruns createChat) makes the later real `createChat` return early at `workspace_host.rs:928`, silently dropping `config`/`branch`/`parentChatId`. A retried work-item launch must "ensure fields", not "skip if exists".
- First turn: the user message is deduped by `message_id`, but the dispatch is not. A second Run command with the same `message_id` and a different command id runs the dispatch again (`sessions.rs:454-515` only dedupes the transcript write). Exactly-once is per command id, via the processed ledger.
- `QueueCommand` as an RPC is not retry-safe: each call mints a new command id.
- Deleted-then-retried: a tombstoned chat row reads as absent (`doc.chat()` returns None), so a late retry with the same chat id would re-create (revive) it (`registry.rs:177-260`, revival branch).

## 2. The durable command plane

### 2.1 Representation
`QueueCommand` entries live in the session doc `commands` list (`crates/doc/src/commands.rs`, `schema.rs:456-560`). `SessionCommandEntry {id, payload, issuedBy, issuedAt, basedOn?, expiresAt?, status, resolution?}` (`commands.rs:77-93`). Payload kinds: `Run{request: RunRequest, messageId}`, `Steer{prompt, messageId?}`, `Interrupt`, `RespondInput` (`:39-57`). Statuses: Pending/Applied/Rejected/Expired/Superseded/Cancelled (`:28-35`).

**A queued first turn is a `Run` entry** whose `RunRequest` (`crates/proto/src/agent.rs:96-129`) carries prompt, harness, model, cwd, sandbox, attachments, optional `WorktreeSpec`, and `messageId`. `RunRequest` has no instruction or revision field today.

### 2.2 Ledger rules (`commands.rs:1-9`, `evaluate_command` `:138-173`)
1. Each device inserts only its own entries; append-only, immutable.
2. The chat's host is the sole writer of outcomes (`set_command_status`, `schema.rs:514-545`); a composer may only cancel its own pending entry (`can_composer_cancel`, `:107`).
3. Evaluation order: processed-id dedupe -> Skip; `now >= effective_expiry` -> Expired (default TTL 24 h, `crates/doc/src/constants.rs:20`); a newer pending Interrupt supersedes older Interrupts; an Interrupt whose `basedOn.turnId` is already past -> Superseded; else Execute. Runs and Steers never supersede each other (test at `commands.rs:316`).

### 2.3 Processed-command ledger
SQLite table `processed_commands(command_id PRIMARY KEY, processed_at)` in the profile `DocsStore` (`crates/sync/src/store.rs:43-46`). `mark_processed` is `INSERT OR IGNORE` and returns whether this call claimed the id (`:501-510`). It is device-local, unsynced, and never pruned. The doc `status` field (Applied etc.) is the cross-device record; the drain only picks Pending and unprocessed entries (`doc_host.rs:4079-4086`), so a wiped ledger does not re-run Applied commands.

### 2.4 Mark-before-execute and crash windows (`drain_commands`, `doc_host.rs:4037-4261`)
Order: read -> dead-command sweep -> pick first Pending and unprocessed -> evaluate -> attachment gate (before marking, so a deferred command stays eligible) -> in-flight `executing` claim -> `mark_processed` -> `execute` -> `resolve_command`. Relay ingestion repeats the same discipline (`ingest_relayed_command`, `:3693-3760`) so the later-arriving doc row dedupes.

| Crash point | Durable state | Recovery today |
| --- | --- | --- |
| After append, before drain | Pending, unprocessed | Host drains on next open; nudge or relay escort delivers; TTL 24 h then Expired |
| After `mark_processed`, before outcome write | Pending in doc, processed in ledger | Dead-command sweep rejects it: "interrupted before completion - retry" (`doc_host.rs:4059-4077`) |
| Mid-`execute` (message written, dispatch not started) | as above, user message landed | Rejected; `retry_delivery` will NOT re-issue because the message landed (`:3598-3600`). Gap: a user message with no run and no journal |
| Dead attempt, message not landed | Rejected/Expired/dead-Pending | `retry_delivery` (`:3560-3690`) re-issues: NEW command id, same payload and `messageId`, one per message, skipped if a live attempt for the same `messageId` exists |
| Engine crash mid-run | run journal stale | `recover` path (`sessions.rs:740-870`) re-dispatches the last user message with `request.resume=None` |

Note the dedupe identity for a first turn is the `messageId`, not the command id (retry re-issue mints a fresh command id, `doc_host.rs:3660`).

### 2.5 Registry persistence
Registry mutations stay in a local overlay with persisted `pending` batches (`registry.rs:299-309`, `PersistedState` `:330-343`), replayed on reconnect and idempotent by strict-`>` per-field HLC compare. Snapshots are debounced 1 s in both stores (`workspace_host.rs:76`, `doc_host.rs:92`). The server rejects any op over 16 KiB serialized (`edge/src/registry-core.ts:60`, `MAX_OP_BYTES`) and any batch over 500 ops (`registry-room.ts:31`; the client chunks at 400, `registry.rs:597-626`). Kind regex `^[a-z][a-zA-Z0-9]{0,31}$`, id regex `[A-Za-z0-9_.:@/-]{1,256}` (`registry-core.ts:55-56`), so all proposed kind names and derived ids fit. Registry `enqueue_ops`/`write` are private; `delete_space` (`registry.rs:859`) is the precedent for a multi-row atomic batch.

### 2.6 Hazard: duplicate command ids
`queue_command` always appends (`schema.rs:478`). Two entries with the same id (concurrent retry from two devices, or a naive retry with a deterministic id) make the dead-command sweep call `set_command_status(id, Rejected)`, which updates the FIRST entry with that id (`schema.rs:514-545`), overwriting an Applied outcome. Required core fix before deterministic ids: make `queue_command` check-then-push by id inside one commit, and make `read_commands` collapse duplicate ids deterministically.

### 2.7 Hazard: follow-up dispatches rebuild the request without the original command
`SessionsEngine.last_requests` is an in-memory map (`sessions.rs:149`, written `:516`). After a restart the fallback is `request_from_chat_row` (`doc_host.rs:4752-4783`), which sets `worktree: None`, empty attachments, and nothing instruction-related. Rebuild sites: `sessions.rs:636`, `:836-837` (crash auto-resume), `:2757`; `doc_host.rs:3264-3266` (queue promotion), `:4450-4451`, `:4517-4518` (steer to new turn / dead-run answers). Also `can_route` (`sessions.rs:~455`) decides whether a warm run takes a prompt over the text-only mailbox or restarts; instruction identity must be part of that runtime-config comparison. Requirement: persist the applied revision per Execution Session on the host (journal sidecar, written before dispatch) and funnel all six rebuild sites through one `followup_request(chat)` that attaches the envelope from that pointer. Never "latest revision".

### 2.8 Old hosts and unknown payload fields
`read_commands` skips entries that fail to deserialize (`schema.rs:456-476`, "skipping malformed command entry"). An old host would silently ignore an extra `RunRequest` field (silent omission, violating spec :96) but would skip an unknown payload `kind` (fail-closed, but the entry sits Pending, so the sender sees "Sending..." forever). Capability negotiation already exists: `zeron_proto::capabilities` (`crates/proto/src/workspace.rs:8-27`), `Device.capabilities` (`entities.rs:93-102`), `EngineInfo.supports`, UI `chat_host_supports` (`crates/ui/src/queue.rs:1272`, `state.rs:1156`), and a version gate for relay (`doc_host.rs:157`). Add `WORK_OUTLINE_INSTRUCTION_ENVELOPE_V1` and gate at the sender, AND use a distinct payload `kind` or an explicit `requires` list so an old host fails closed.

## 3. RPC registration pattern

- Method names: `pub const X: &str = "PascalCase"` in `crates/rpc/src/lib.rs` `methods` (`:33-200`); one pin test exists (`:465`).
- Server: `impl RpcService for EngineRpc` -> `handle(method, params)` (`crates/engine/src/rpc.rs:1808`): first, device-addressed forwarding if `forwardable(method)` and `params.targetDeviceId` names another device (`:1811-1816`); then `match method`. Unary arms use `parse_params::<P>` plus `RpcReply::value`; streams return `RpcReply::Stream(watch_stream(rx))` (`:2211-2228`, `watch_stream` at `:1527`).
- Params: `#[derive(Deserialize)] #[serde(rename_all = "camelCase")]` structs near `rpc.rs:111-240`. Entity mutations are a tagged enum `MutateParams {op}` (`:600-700`), accepted from any device because they are LWW registry writes.
- IPC-only vs forwardable: `forwardable()` (`rpc.rs:1425-1505`) is an allow-list of methods that honor `targetDeviceId`; anything absent is "IPC-only" by convention, pinned by tests (`:3845-3898`). `is_stream_method` (`:1507`) lists forwardable streams; `forward_deadline` (`:1411`) sets unary timeouts (default 30 s). Caveat: this is routing, not authorization. `HostRelay` serves the whole `EngineRpc` to any same-account peer over the device room (`crates/rpc/src/device_room.rs:312-400`; trust model in `ARCHITECTURE.md:80-92`), and the loopback IPC socket accepts any local process (only browser Origin headers are rejected, `crates/rpc/src/lib.rs` test `handshake_with_origin_header_is_rejected`).
- Wire-shape regression tests (pattern to copy): `parse_params` camelCase tests (`rpc.rs:3972-3990`), reply-shape asserts (`:3854-3880`), entity field-name pins (`crates/proto/src/entities.rs:1249-1272` for `autoApprove`, snake_case exception at `:1274`), `chat_link_status_json_field_names...` (`crates/engine/src/pr_ticket_cache.rs:1596-1640`). Every new request, reply, and row type needs one `json!({...})` equality pin including exact enum spellings.
- Unknown-method tolerance: UI treats `"unknown method: "` errors as "peer too old" (`composer.rs` TakeProjectActionSetup loop). Reuse for hosts lacking the Work Outline methods.

**Files touched to add one RPC (unary, device-addressable):**
1. `crates/rpc/src/lib.rs` - method const (+ pin test).
2. `crates/engine/src/rpc.rs` - params struct, `handle` arm, `forwardable()` (+ `is_stream_method`/`forward_deadline` if needed), unit tests.
3. `crates/proto/src/*.rs` (e.g. new `work_outline.rs`, re-exported from `lib.rs`) - wire types, capability const in `workspace.rs:8`, JSON pin tests.
4. `crates/doc/src/registry.rs` (+ `registry/tests.rs`) - `KIND_*` consts (`:33-40`), typed read/write, a public multi-op batch writer, merge/idempotence tests.
5. `crates/engine/src/workspace_host.rs` (or new `work_outline.rs` + field on `EngineRpc` and assembly in `crates/engine/src/lib.rs:~669`) - service logic, watch channel (pattern `watch_chats`, `:699`).
6. `crates/engine/tests/` - integration (`device_routing.rs` for forward behavior, a new test for crash/retry states).
7. `crates/ui/src/state.rs` (`spawn_watch` `:2467`, subscription wiring `:1963`) + the calling UI module - client side.
8. `crates/mcp/src/tools.rs` / `zeron.rs` only for agent tools (Milestone 4).
9. Docs: `docs/research/feature-inventory.md` section 2, `docs/mcp.md`, `ARCHITECTURE.md`.
No edge change is needed for new row kinds (the edge accepts any kind matching the regexes); iOS (`apps/ios/.../WorkspaceStore.swift`) reads by kind name and ignores the rest.

**Family layout:** `WatchWorkOutline {spaceId}` as a stream (current snapshot first, then changes; not forwardable, each device reads its own replica). Pure-registry mutations (`createWorkItem`, `updateWorkItem`, `moveWorkItem`, `archiveWorkItem`, instruction CRUD, `assignChatToWorkItem`, `removeChatFromWorkItem`, references, proposals) are LWW registry writes: handle locally on any device, like `Mutate`, not forwarded. Execution-touching operations (`createChatForWorkItem`, `continueChatForWorkItem`, `applyInstructionRevision`, `configureExecution` validation) must run on the Space's owning device, so add them to `forwardable()` and have the UI set `targetDeviceId = space.device_id`. Prefer one tagged `WorkOutlineMutate {op, operationId, ...}` for cheap mutations and distinct methods for the three long ones, to keep the unknown-method degradation path clear.

## 4. Existing idempotency handling

- RPC level: none. No `operationId`, `idempotencyKey`, or `requestId` for mutations. (`request_id` in the engine is the harness AskUserQuestion id.)
- Registry level: idempotent-by-record-id writes only. `create_chat` (`workspace_host.rs:928`), `create_space` (by id OR `(deviceId,path)`, `:986-1003`), `claim_chat`, row-level HLC LWW and tombstones (`registry.rs:177`). Import passes use "structural idempotence": a target row existing means skip (`local_import.rs:21`, `external_import.rs:1345-1500`).
- Session-doc level: user message deduped by id (`doc_host.rs:687`); command exactly-once by processed ledger (sections 2.3-2.4); relay dual-road delivery relies on client-minted command id (`rpc` doc for `RELAY_COMMAND`).
- Worktrees: reuse guard only when the chat's cwd is already a linked worktree (`doc_host.rs:4637-4651`); otherwise each call mints a new worktree.

## 5. Agent-callable tools (Milestone 4 hook)

- `crates/mcp` (`zeron-mcp`) is a hand-rolled MCP stdio server proxying the engine IPC (`lib.rs`, `docs/mcp.md`; `zeron mcp` subcommand at `apps/zeron/src/main.rs:162,224`). 14 tools: whoami, list_devices/projects/harnesses/models/chats, get_chat, create_chat, read_chat, send_message, wait_for_turn, interrupt_chat, respond_to_input, archive_chat (`tools.rs:59-182`).
- Origin: `Origin{chat_id, device_id}` read from `ZERON_CHAT_ID`/`ZERON_DEVICE_ID` (`zeron.rs:41-58`). Uses: `create_chat` records `parentChatId` (`tools.rs:591-612`), `send_message` prefixes attribution and refuses self-send (`:726`, `:840-858`). Tools issue `Mutate`/`QueueCommand` as ordinary IPC calls.
- Not injected: every harness launches with an empty MCP config (`docs/mcp.md` "Nothing is injected yet"; `crates/harness/src/claude/mod.rs:529`, `acp/mod.rs:2890`, `codex/mod.rs:977`). The injection work is unbuilt.
- Authorization: none. The origin is self-declared; the loopback socket exposes every method (including `Mutate deleteSpace`); an agent with a shell can bypass MCP entirely. There is no tool-call id in the MCP layer (`jsonrpc.rs:153` reads only the tool name and arguments) and `create_chat` mints a fresh v4 chat id per call with separate Mutate and QueueCommand steps: agent retries duplicate chats and prompts (`tools.rs:601`, `:940`).

**Recommendation for M4:** (1) the engine mints a per-run capability token bound to `(chatId, runId, spaceId, workItemId at launch)` and injects it into the MCP server env at harness launch; (2) serve agent tools through a separate allow-listed `AgentRpc` service (own listener or token-gated method set) that derives origin server-side and ignores any client-supplied chat id; (3) agent mutations call the same service operations with `origin: Agent{chatId, runId}` and an agent-supplied `idempotencyKey` argument (required on create operations; natural idempotence for status set); effective operationId = hash(origin chat, tool, key); (4) `create_child_chat` becomes one engine operation (the `createChatForWorkItem` state machine below, with inherited work item and applied revision), not Mutate createChat + assign; (5) spike per harness whether per-run MCP injection with env is possible (Claude `--mcp-config`, codex `mcp_servers.*`, ACP `session/new mcpServers`).

## 6. Proposal: operation-id scheme

- `operationId`: client-generated UUIDv7 string (time-ordered, helps pruning and debugging). The caller must reuse the same id on every retry of one logical action and mint a new one for a new action. Workspace uuid features are `v4,serde` only (`Cargo.toml:101`); enabling `v7`/`v5` is a one-line change (`sha1_smol` is already in `Cargo.lock`). Alternative with no dependency change: SHA-256 (`sha2` is already a dep of `zeron-doc`/`zeron-engine`) formatted as a UUID-shaped string.
- Derived record ids (all deterministic from the operationId, namespaced by a fixed UUID):
  - `chatId = uuid5(NS, "{opId}:chat")`, `messageId = uuid5(NS, "{opId}:msg")`, `commandId = uuid5(NS, "{opId}:cmd")`, `revisionId = uuid5(NS, "{opId}:rev")`, `applicationId = uuid5(NS, "{opId}:app")`. The assignment row is keyed by `chatId` per spec (:449), so it needs no derived id.
  - Single-record operations (`createWorkItem`, `createWorkItemInstruction`, ...): `id = uuid5(NS, "{opId}:{kind}")` unless the caller supplies an explicit id; the registry upsert is then idempotent by row id.
- Collision and conflict rule: every derived record stamps `createdByOperationId` and an `inputsHash` (SHA-256 of the canonicalized parameters excluding the id). Same operationId with a different inputsHash returns `OperationConflict`, never a silent merge.
- Hide-resurrection rule: if the derived chat row is tombstoned (user deleted the partial chat), the operation returns `Gone` and never re-creates it. Requires reading the tombstone, not just `chat()` (new registry accessor).
- Do not recompute immutable records on retry: if the revision row exists, reuse it verbatim even if instructions changed since. To keep the user's review honest, add `expectedInstructionsHash` and `expectedExecutionConfigHash` params; a mismatch on the first attempt returns `InstructionsChanged`/`ExecutionConfigChanged` (the drift confirmation of M2/M3).
- Optional `operation` ledger row kind (`workOutlineOperation`, id = operationId; kind, spaceId, inputsHash, terminal state) for audit and conflict detection. It is not required for correctness because state is derivable from facts; recommended only if the UI needs "incomplete launch" indicators without scanning chats. Flagged as an addition beyond the spec's row list.

## 7. Proposal: `createChatForWorkItem` state machine

Signature (extends spec :536, see section 8): `createChatForWorkItem(workItemId, initialPrompt, launchOptions{harness, model, reasoning, sandbox, autoApprove, existingWorktree?, attachments?}, expectedHashes?, operationId)`, executed on the Space's owning device (forwarded), so validation and chat creation are local to the host and no remote-registry-vs-session-doc ordering problem arises.

State is derived from facts (idempotent step detection), not a stored cursor:

| State | Facts | Recoverable? |
| --- | --- | --- |
| S0 | nothing | n/a (retry = start) |
| S1 chat | chat row exists with `createdByOperationId=opId` | yes |
| S2 assigned | + assignment row for chatId -> workItem with provenance opId | yes |
| S3 revised | + revision row `revisionId` | yes |
| S4 application-queued | + application row `applicationId` (reason `launch`, result `queued`) | yes |
| S5 turn-queued | + Run command with `messageId` for this op in the session doc (Pending) | yes (host drains) |
| S6 claimed | host marked processed; application result `claimed`; user message landed | at-most-once; crash here -> `claimed`, never re-applied |
| S7 delivered | command Applied; application result `delivered` + execution session ref | terminal success |
| F-blocked | validation failed at S0..S4 | retry after config fix, same operationId, same revision |
| F-rejected | command Rejected/Expired, message not landed | retry re-issues (new command id, same payload and `messageId`) |
| F-interrupted | command Rejected by dead-sweep, message landed | surfaced as "start interrupted"; explicit rerun is a new Turn, not the same op |

Algorithm (each step reads before it writes; every step safe to repeat):
1. Compute derived ids and `inputsHash`. Look up the chat row (including tombstone). Tombstoned -> `Gone`. Present with a different operationId or inputsHash -> `OperationConflict`. State >= S5 with a live or applied command -> return the stored result `{chatId, revisionId, applicationId, commandId, state}` (response replay).
2. Validate on the host: workspace/Space, execution root, repositories, exact worktree, harness capability including `WORK_OUTLINE_INSTRUCTION_ENVELOPE_V1`, expected hashes. Re-run on every retry before queueing; failure leaves S1-S4 intact (`F-blocked`).
3. Registry write in ONE batch (new public multi-op writer, same mechanism as `delete_space`): chat row (full-field upsert, filling a claim stub's missing fields, not the early-return path), assignment (write only if absent or already carrying this operation's provenance, so a user's later reassign is never stomped by a retry), revision (only if absent), application row (only if absent; result `queued`). Server atomicity makes S1-S4 appear together; if only part is visible after a crash, steps 3's per-row existence checks fill the gap. Watch the 16 KiB per-op cap for the revision row (see section 8 item 5).
4. Queue the first turn, appended LAST (spec order, :566): look for a command whose `Run.messageId == derived messageId` or whose payload carries this `operationId`; if absent and the message has not landed, append `Run` with `id = commandId` (using the id-idempotent `queue_command` fix, section 2.6). If the existing one is Rejected/Expired with the message not landed, reuse the `retry_delivery` re-issue path keyed on `messageId`.
5. Host drain executes the Run. Before dispatch it re-validates capability and execution config; writes application result `claimed` (mark-before-execute, mirroring the command ledger); dispatches with the envelope; on success writes `delivered` with `{runId, harnessSessionId, turnBoundary}` and persists the per-session applied-revision pointer (section 2.7). Failure writes `failed` and resolves the command Rejected with a structured reason.
6. Return `{chatId, revisionId, applicationId, commandId, state}`.

Duplicate avoidance summary: Chat by derived id plus claim-stub merge; assignment by key `chatId`; revision and application by derived id with exists-check; command by derived id (after the dedupe fix) with `messageId` as the cross-reissue identity; Run by processed ledger; transcript message by `messageId`.

**How the queued command captures the revision.** The `Run` payload carries `workOutline: {operationId, revisionId, revisionHash, applicationId, envelope}` where `envelope` is the exact canonical rendered text (or a byte-exact copy of the revision's `renderedEnvelope`). It is self-contained so execution never depends on the host's registry replica having caught up (the registry and chat2 rooms have no cross-room ordering); the host verifies `sha256(canonical(envelope)) == revisionHash` before dispatch and rejects on mismatch. The envelope travels on a new `RunRequest.instruction_context` field (serde-default, skip-if-none, separate from `prompt`), never concatenated into the prompt, and becomes part of the `can_route` runtime-config comparison. Application at a Turn boundary: a warm session keeps its applied revision (the pointer), a fresh Execution Session receives the Chat's selected revision, and an explicit apply is a different operation with its own application row.

## 8. Conflicts with, or gaps in, the spec

1. **Application row is immutable but needs a result (spec :504-508, :548).** Spec says "deliver it with a successful launch application row" before "queue the initial Turn". Delivery success is only knowable on the host after drain, and the target Execution Session is not minted until dispatch. Proposal: application row is written at queue time with immutable identity fields and a write-once, host-only `result` group (`queued -> claimed -> delivered|failed`, execution session ref, applied HLC). The spec must state that "applied_hlc" is the delivered/claimed stamp, and that the Chat's selected revision counts a launch application from `queued` onward, otherwise a launch that fails before delivery loses the revision a retry must reuse.
2. **"Execution Session" has no entity today.** There is a Run id (engine-minted, `sessions.rs:518`) and a native `harnessSessionId` on the chat row. The application row's `target Execution Session` needs a defined identity (suggest `harnessSessionId` plus run id) and the spec should say so.
3. **Signature too thin.** `createChatForWorkItem(workItemId, initialPrompt, operationId)` (:536) lacks harness/model/sandbox/approval, attachments, existing-worktree selection, and expected hashes. Without the expected hashes, retries cannot honor "exact revision the user reviewed".
4. **"Validate first" is host-local.** Execution paths and worktrees exist only on the Space's device, but the registry convention is "any device writes" (`Mutate`). Route the three execution operations to the owning device (forwardable + `targetDeviceId`); the instruction-envelope capability check is the version gate for old hosts (spec :103, :552 compatible).
5. **Revision size vs edge op cap.** The edge rejects any op over 16 KiB (`registry-core.ts:60`); a revision row holds the ordered snapshot plus the rendered envelope. The spec's "size budgets" (:443, M2) must be sized under this cap, or the revision must be split across rows or chunked, or the cap raised in `edge/`. A rejected oversized op rejects the whole batch (`registry-room.ts:367-371`), wedging all later writes (see the 500-op wedge comment at `registry.rs:601-610`), so the engine must pre-validate size locally.
6. **"Do not place outline metadata inside transcript Loro documents" (:85) vs "queued command captures the revision ID and content" (:98).** The command lives in the session doc. This is the command plane, not outline metadata, but the envelope text is duplicated into it. Recommend the spec carve this out explicitly (self-contained command, section 7).
7. **Atomicity wording.** Spec :92 calls it "one idempotent multi-step operation with recoverable partial-failure states". True, but registry rows can be atomic only within one registry batch; the registry/session-doc boundary is never atomic, and each store has a 1 s snapshot debounce. The test plan must inject crashes between registry batch and command append and between claim and delivered, in both directions (registry tail lost, doc tail lost).
8. **Operation record.** Spec asks for "client-generated operation identifiers" but no operation row kind and no conflict semantics. Proposed `inputsHash` stamping and an optional `workOutlineOperation` row are additions.
9. **Worktree creation is not idempotent.** `CreateWorktree`/`CreateChatWorktree` mint new worktrees each call; the host reuse guard keys on the chat's cwd only (`doc_host.rs:4642-4651`). The spec's "select an existing worktree or create one through the existing flow" (:563) means worktree creation must be a prior, separately idempotent step (deterministic name from an operationId) and `createChatForWorkItem` must accept only an existing exact worktree, never a `WorktreeSpec` directive.
10. **Do not copy the composer's failure handling.** Today a failed new-chat send deletes the chat (`composer.rs:7180-7195`). For work-item launches the chat/assignment/revision may have synced; surface `F-blocked`/`F-rejected`, keep the Chat, restore the prompt to the draft, and retry with the same operationId.
11. **Agent child chats (acceptance 19).** Existing `create_chat` is not idempotent and not authorized (section 5). The spec's "origin-aware, narrowly authorized" tools need engine-side token authorization; today only convention exists.

## 9. Core changes required before the state machine is safe

1. `SessionDoc::queue_command` idempotent by id; `read_commands` collapses duplicate ids; `set_command_status` targets the pending duplicate (`crates/doc/src/schema.rs`).
2. Public multi-op registry batch writer (`crates/doc/src/registry.rs`), with local op-size and batch-size validation.
3. Registry tombstone-aware reader for derived ids.
4. `RunRequest.instruction_context` plus payload `requires`/distinct kind for fail-closed old hosts; capability constant; `can_route` includes instruction identity.
5. One `followup_request(chat)` builder replacing the six rebuild sites, backed by a durable per-session applied-revision pointer in the run journal.
6. Host-side application-result writer with claim-before-dispatch semantics and a dead-command sweep that reconciles `claimed` results.
7. Fix `create_chat_with_parent` to merge-fill instead of early-return on a claim stub (`workspace_host.rs:928`).
8. M4: per-run capability token, allow-listed agent RPC surface, per-harness MCP injection spike.
