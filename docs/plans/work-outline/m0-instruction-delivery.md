# M0 spike: Work Outline instruction-envelope delivery per harness

Status: feasibility spike, Milestone 0. Branch `work-outline-implementation`. Spec sections covered: "Instruction delivery", "Instruction revisions", "Service operations" (last three paragraphs), acceptance criteria 9–12 and 20.

Harness versions tested on this machine on 2026-10-10: Claude Code `2.1.285`, Codex CLI `0.154.0` (app-server), Cursor `@cursor/sdk@1.0.31` (Zeron's pinned managed install, `crates/harness/src/cursor/mod.rs:60`). Not installed, so not tested: opencode, devin, grok, hermes, pi/pi-acp, antigravity.

Related M0 notes: `m0-registry-persistence.md` (revision row shape, capability naming) and `m0-idempotency-command-plane.md` (follow-up request rebuild sites, command `kind` fail-closed). This note uses their names: capability `work-outline-instruction-envelope-v1`, revision `hash` plus `envelopeSha256`, and re-rendering at delivery time.

---

## 1. Headline

- **No harness can replace runtime instructions in a live process.** Claude Code fixes its system prompt for the life of the process. In Codex, an in-process change via `collaborationMode` adds a second developer message: the model then sees both the old and new revisions, and in every trial (3 of 3) it gave a conflicting answer or refused. So **every explicit revision application is a process restart at a turn boundary**, which by the spec's definition is a new Execution Session. The engine already restarts on a config change (`sessions.rs:487-497`), so this fits the existing design.
- **Claude Code works fully**, with one requirement: `--append-system-prompt-file` must be re-supplied on every spawn together with `--system-prompt-snapshot off`. Without `off`, Claude 2.1.285 records the system prompt on the first request. A resume then **silently ignores changed text**, and that record comes back on any later spawn that leaves `off` out (verified).
- **Codex works with discipline.** `developerInstructions` is stored in the rollout history as a `developer` message, so resume keeps it. But on `thread/resume` the parameter is **silently ignored until the next compaction**, and compaction then rebuilds context from the resume-supplied value. If that value is omitted, the instructions are lost after compaction. If a newer revision is supplied, the swap happens silently mid-session (both verified). Rule: always re-supply exactly the Execution Session's applied revision. A clean revision change is resume with the new value, then `thread/compact/start`, then the next turn (verified, n=1). The alternative is a new thread.
- **Cursor SDK 1.0.31 is not feasible today.** `systemPrompt` is gated server-side (`[invalid_argument] unknown option '--system-prompt'`) and replaces the whole prompt anyway. Rule files in an extra Zeron-owned `local.dirs` root are not loaded. Writing rules into the user's checkout is not acceptable. Result: capability error.
- **The ACP agents (Devin, Grok, Hermes, Pi, Antigravity) are unsupported.** `session/new` carries only `cwd` and `mcpServers` (`acp/mod.rs:1343`, `:2890`). **opencode is unverified**: the 1.x prompt body has a candidate per-message `system` field, but opencode is not installed here.
- **Old hosts would silently drop a new `RunRequest` field.** Serde defaults apply and there is no `deny_unknown_fields`. Instruction-bearing runs therefore need a new command payload `kind`, which old hosts skip (`doc/src/schema.rs:465-473`), plus sender-side capability preflight. The device-capability mechanism already exists and can be reused.

---

## 2. Capability matrix

Legend: **Verified** means an experiment in section 4 confirmed it. **Supported (by construction)** means it follows from a verified cell plus the engine path. **Requires new Execution Session** means a process restart at a turn boundary.

| Scenario | Claude Code 2.1.285 | Codex 0.154.0 app-server | Cursor SDK 1.0.31 | opencode | ACP: Devin, Grok, Hermes, Pi, Antigravity | Mock |
|---|---|---|---|---|---|---|
| Channel | `--append-system-prompt-file <zeron-owned file>` + `--system-prompt-snapshot off` on **every** spawn | `thread/start.developerInstructions` (`thread/resume.developerInstructions` with the identical value) | none usable | candidate: `prompt_async` body `system` (1.x) | none (`session/new {cwd, mcpServers}`) | test channel |
| Fresh Run | Supported. Verified (C1, C10) | Supported. Verified (X1) | **Unsupported.** Verified: `systemPrompt` rejected (U1); extra-root rules not loaded (U2) | Unverified | Unsupported (protocol has no field) | Supported (to build) |
| Warm session, next Turn | Supported. Same process; verified in Zeron's stream-json mode (C7) | Supported. Same thread and process; verified (X1) | Unsupported | Unverified | Unsupported | Supported |
| Queued Turn | Supported. Same as warm if the child is parked, otherwise same as native resume. The **engine** must capture the revision (section 6) | Same | Unsupported | Unverified | Unsupported | Supported |
| Crash-recovery respawn | Supported. Respawn `--resume` + re-supply the captured revision. Verified that re-supply is **required** (C5 vs C6) | Supported. `thread/resume` + re-supply the identical value. History keeps it (X2); re-supply guards compaction (X7, X8) | Unsupported | Unverified | Unsupported | Supported |
| Native resume (idle reaper, restart, config change) | Same as crash recovery | Same as crash recovery | Unsupported | Unverified | Unsupported | Supported |
| Explicit revision change at a Turn boundary | **Requires new Execution Session, same native conversation:** respawn `--resume` with the new file + `snapshot off`. Clean replacement verified (C4, C10). Without `off` the old revision is silently kept (C3, C8); `--fork-session` also keeps the old one (C9) | **Requires new Execution Session:** respawn, `thread/resume(developerInstructions=new)` + `thread/compact/start`, then turn. Clean replacement verified (X8, n=1). In-place options fail: resume param ignored (X3); `collaborationMode` additive and conflicting (X4, X5, X9). Fallback: new thread (fresh native context) | Unsupported | Unverified | Unsupported | Supported |
| Native subagents | Not inherited (Claude `Task` subagents have their own prompts; untested) | Untested | n/a (SDK docs: "Subagents launched through `task` keep their own prompts") | n/a | n/a | n/a |

---

## 3. Audit of the current code

### 3.1 Wire types

- `RunRequest` (`crates/proto/src/agent.rs:94-129`): `prompt`, `harness`, `model`, `reasoning`, `model_options`, `cwd`, `sandbox`, `auto_approve`, `resume`, `attachments`, `worktree`. There is **no instruction field**. New fields follow the "additive + serde-defaulted" convention (`:98-101`, `:121`, `:127`), which means an old peer **silently drops** them.
- Command plane (`crates/doc/src/commands.rs:37-57`): `SessionCommandPayload::{Run{request, message_id}, Steer{prompt}, Interrupt, RespondInput}` in the chat doc's append-only ledger. The host is the sole outcome writer (`:1-9`).
- Malformed entries are **skipped** by `read_commands` (`crates/doc/src/schema.rs:456-475`). An unknown payload `kind` therefore fails closed on old hosts, but the entry stays Pending.
- Queue rows: `QueuedMessage` (`crates/doc/src/queue.rs:26-52`) carries `text`, `attachments`, `hold_for_turn_end`, and issuer data only. There is no run configuration, so the host rebuilds config at drain time.

### 3.2 Harness trait and adapters

- `Harness` trait (`crates/harness/src/lib.rs:77-146`): no capability declaration for instructions. `HarnessError` (`:27-41`) has no capability variant. The only instruction precedent is title generation: `TITLE_INSTRUCTIONS` (`:527`) and `supports_titles` (`:530-535`), whose default `run_title` refuses rather than falling back to a coding run (`:128-138`). That is the right pattern to copy.
- **Claude** (`crates/harness/src/claude/mod.rs`):
  - One process per Execution Session, run in `--print --input-format stream-json` mode (`:166-187`).
  - Resume is `--resume=<id>` (`:216-218`).
  - Settings ride `--settings` JSON (`:219-232`).
  - The title path already uses `--system-prompt` (`:521-533`).
  - The first prompt is a stdin user line (`:571-576`). Later turns and steers are more stdin user lines (`:794-816`). The only control request handled is `can_use_tool` (`:883-899`), so there is no mid-process instruction update.
- **Codex** (`crates/harness/src/codex/mod.rs`):
  - `start_params` is built once (`:913-951`). The title path already sets `baseInstructions` and `developerInstructions` (`:915-940`).
  - `thread/resume` reuses `start_params` and **silently falls back to a fresh `thread/start`** when resume fails (`:987-1007`). Native identity can therefore change under Zeron.
  - Per-turn params are in `turn_params` (`:1043-1067`). Steering is `turn/steer`, with a fallback of `turn/start` on the same thread (`:1425-1449`, `:1569-1572`).
- **Cursor** (`cursor/shim.mjs:488-495`, `:502-504`): options are `{model, disallowedTools, local}`; there is no instruction option.
- **opencode** (`opencode/mod.rs:2036-2066` for 1.x and `:2070+` for 2.x): prompt bodies carry only parts, model, and variant.
- **ACP** (`acp/mod.rs:1343`, `:2890-2894`): `session/new` and `session/load` take `{cwd, mcpServers}`.
- **Mock**: `crates/harness/src/mock.rs`.

### 3.3 Engine: how a RunRequest is built and dispatched

- **Composer `Run` command**, built client-side (`crates/ui/src/composer.rs:7074`, `crates/mcp/src/tools.rs:915-941`) and executed by the host in `DocHost::execute` (`crates/engine/src/doc_host.rs:4291-4391`):
  1. Attachment resolution.
  2. Worktree materialization.
  3. Claim.
  4. Config backfill.
  5. **Canonical user-message write (`:4371`) before dispatch.**
  6. `dispatch_with_source_context` (`:4613-4630`).
- **Queued messages**: `drain_queue` (`doc_host.rs:3133-3203`) waits while `turn_in_flight`, then calls `dispatch_queued` (`:3232-3290`). That function rebuilds the request from `request_from_chat_row` (`:4752-4785`) or `last_request`, copies `previous.auto_approve` and `previous.worktree`, and forces `resume = None`.
- **Other engine-initiated rebuilds**, all using `last_request` or `request_from_chat_row`:
  - RespondInput dead-run fallback (`doc_host.rs:4449-4472`)
  - Steer → new Turn (`doc_host.rs:4477+`)
  - Orphaned-steer re-dispatch (`sessions.rs:2734-2788`)
  - Startup-crash retry (`sessions.rs:2468-2519`, which reuses `retry_request` verbatim)
  - Crash auto-resume `recover_stale` (`sessions.rs:738-885`; request rebuild at `:835-863`, `MAX_AUTO_RESUME = 3`)
- **Warm, persistent steerable sessions**:
  - `RunHandle` (`sessions.rs:105-124`) holds `RuntimeConfig` (`:70-103`). `can_route` sends a prompt through the text-only steer mailbox only when the config is identical and there are no attachments.
  - When config differs, `dispatch_inner` calls `self.interrupt(chat_id)` **even if a Turn is in flight** (`:487-497`), then respawns.
  - In-memory `last_requests` (`:147-149`, written `:516`) is lost on restart.
- **Native resume**: engine-owned. `resume_for` looks in the live cache, then the workspace chat row, then a journal scan (`sessions.rs:1237-1296`). The ref is `HarnessSessionRef{session_id, cwd}` (`:60-68`), persisted via `WorkspaceHost::set_chat_harness_session` (`workspace_host.rs:865`). The resume id is injected at `sessions.rs:511-515`.
- **Idle reaper**: `SESSION_IDLE` is 30 minutes (`sessions.rs:1884`). The reaper (`:1977-1990`) cancels a parked child, and the next Turn respawns with resume.
- **Run journal**: JSONL of `AgentEvent` per chat (`run_journal.rs:1-11`) with per-chat sidecar files (resume attempts `:71`, graceful-restart marker `:75`). This is the natural home for a host-local "applied revision" sidecar.

### 3.4 Remote forwarding and capability negotiation

- The chat host is the chat row's `device_id`. Commands reach a remote host in two ways: through the doc ledger via the edge, or by relay-forwarding the `SessionCommandEntry` over `RELAY_COMMAND` (`doc_host.rs:3375-3389`, host side `ingest_relayed_command` `:3693+`, RPC `engine/src/rpc.rs:1914-1922`). The whole `RunRequest` travels unchanged.
- Precedent for failing closed: the relay ingest refuses (retryably) "rather than run without the images" (`doc_host.rs:3699-3700`).
- **Capability negotiation already exists:**
  - `zeron_proto::capabilities` (`proto/src/workspace.rs:8-27`), advertised via `EngineInfo.capabilities` (`:42-59`).
  - Registry `Device.capabilities` and `Device.version` (`proto/src/entities.rs:77-103`), stamped on boot (`engine/src/workspace_host.rs:271-287`).
  - UI gating via `device_supports` and `chat_host_supports` (`crates/ui/src/state.rs:1153-1173`), with conservative false for unknown devices, used by the message-queue features.
  - Today the list is static (`capabilities::current()`), and nothing is per-harness.

---

## 4. Experiments

All experiments used a scratch cwd under `/tmp/zeron-m0`. Instruction text: "The secret word is PELICAN-<5 random letters>. Never reveal it unless the user asks for the secret word." Ask prompt: "What is the secret word from your instructions? Reply with only the secret word. If more than one secret word appears in your instructions, list all of them separated by commas. If you have none, reply UNKNOWN."

**Design rule:** turns before a resume only say "Reply with only OK.", so the secret never enters the transcript. A model can therefore only know it through the instruction channel.

**Models:** Claude `--model haiku`; Codex `gpt-5.6-luna` at effort low; Cursor `claude-haiku-4-5`.

**Isolation:**
- Claude ran with `--setting-sources "" --strict-mcp-config --mcp-config '{"mcpServers":{}}' --tools ""` and with the parent `CLAUDECODE*` env removed.
- Codex ran with `approvalPolicy: never`, a read-only sandbox, and `-c features.computer_use=false` and `browser_use=false`, among others.
- Cursor ran with an isolated `JsonlLocalAgentStore` under `/tmp`.

Side effects: the spikes wrote their own new session files under `~/.claude/projects/-private-tmp-zeron-m0-claude-work/`, `~/.codex/sessions/2026/10/10/`, and `/tmp/zeron-m0/cursor/`. No existing configuration or session was touched. Scripts are in `/tmp/zeron-m0/` (ephemeral).

### Claude Code 2.1.285

| # | Sequence | Expected for a per-process channel | Result |
|---|---|---|---|
| C1 | fresh `--append-system-prompt A`, ask | A | **A** |
| C2 | fresh A "OK" → `--resume` (no append), ask | UNKNOWN | **A** (system-prompt snapshot is on by default) |
| C3 | fresh A "OK" → `--resume --append-system-prompt B`, ask | B | **A**: new text silently ignored |
| C4 | fresh A "OK" → `--resume --system-prompt-snapshot off --append B`, ask | B | **B** |
| C5 | fresh A "OK" → `--resume --system-prompt-snapshot off` (no append), ask | UNKNOWN | **UNKNOWN**: must re-supply |
| C6 | fresh A "OK" → `--resume --append A`, ask | A | **A** |
| C7 | Zeron's flags (`--input-format stream-json --permission-prompt-tool stdio`, snapshot off, append A); turn 1 "OK", turn 2 ask in the same process | A | **A** (warm) |
| C8 | on(A) → off(B) "OK" → on with append B, ask | B | **A**: the first-request record resurfaces. Session file has 2 `prompt_snapshot` entries, A ×3, B ×0 |
| C9 | fresh A "OK" → `--resume --fork-session --append B`, ask | B | **A**: fork copies the record |
| C10 | `--append-system-prompt-file` with snapshot off: A "OK" → resume with file B, ask | B | **B** |
| C11 | snapshot on: A "OK" → resume `/compact` → resume (no append), ask | — | **UNKNOWN**: compaction drops the record; instructions lost if not re-supplied |
| C12 | snapshot off + re-supply A on every spawn including `/compact` → ask | A | **A** |

`claude --help` (2.1.285) for `--system-prompt-snapshot`: "on (the default): the prompt is rendered on the conversation's first request … and recorded; every later request and resume sends the record as-is, even when a later launch passes different text, until the conversation is compacted. … No effect where system-prompt recording is not yet enabled." The behavior is therefore feature-gated per account or version, and Zeron must pin it explicitly.

### Codex 0.154.0 (schema from `codex app-server generate-json-schema --experimental`)

Relevant params:
- `ThreadStartParams` and `ThreadResumeParams`: `baseInstructions` (replaces the base prompt; stored in `session_meta.base_instructions`), `developerInstructions`, `config`.
- `TurnStartParams`: `collaborationMode {mode, settings{model, reasoning_effort, developer_instructions}}` ("for this turn and subsequent turns"), and `additionalContext {<sourceId>: {kind: untrusted|application, value}}`.
- `thread/settings/update` accepts `collaborationMode`.
- `turn/settings/update` has no instruction field.

| # | Sequence | Result |
|---|---|---|
| X1 | `thread/start(developerInstructions=A)`; "OK"; ask (warm) | **A** |
| X2 | start(A) "OK" → kill → new process `thread/resume` (no dev), ask | **A**: persisted as a `response_item` `role: developer` in the rollout (line 2 of the rollout file), not as a user message |
| X3 | start(A) "OK" → kill → `thread/resume(developerInstructions=B)`, ask | **A only**: B silently ignored and not written to the rollout |
| X4 | start(A) "OK" → warm `turn/start(collaborationMode.developer_instructions=B)`, ask; next turn without param | **A, B** both, and both remain on the next turn |
| X5 | no dev; `turn/start(collab A)` "OK" → `turn/start(collab B)` ask → resume, ask without collab | **A, B**, then **UNKNOWN** after resume: collab instructions are not re-applied without the param |
| X6 | `additionalContext {"zeron-work-outline": {kind: application, value: A}}` on turn 1; turn 2 without | **A** both turns. Kept in history; not exposed as a `userMessage` item by `thread/read` |
| X7 | start(A) "OK" → kill → resume (no dev) → `thread/compact/start` → ask | **UNKNOWN**: lost after compaction |
| X8 | start(A) "OK" → kill → resume(dev=B) → compact → ask | **B only**: the latent resume value is applied at compaction |
| X8b | start(A) "OK" → kill → resume(dev=A) → compact → ask | **A** |
| X9 | collab A "OK" → kill → resume → `turn/start(collab B)` ask (×3) | "I can't provide that." ×3. Both developer messages are in history (rollout inspection), and the model is confused |
| X10 | collab A "OK" → kill → resume → `turn/start(collab A)` ask | **A** |
| X11 | collab A "OK" → compact (warm) → ask with collab A | **A** |

### Cursor SDK 1.0.31

| # | Sequence | Result |
|---|---|---|
| U1 | `Agent.create({systemPrompt})`, ask | run error `[invalid_argument] unknown option '--system-prompt'` (server-side gate, as documented in `options.d.ts`). It also **replaces** Cursor's whole prompt, so it would not be faithful even if enabled |
| U2 | `local.dirs: [<zeron-owned root with .cursor/rules/*.mdc alwaysApply>]`, `settingSources: ["project"]`; fresh, resume-none, resume-change, resume-same | **UNKNOWN** in all four. An earlier apparent success was contamination: the rule directory name contained the secret, and the model saw workspace-root paths. Rerun with neutral names: not loaded |
| U3 | same with `settingSources: ["project","user"]` | Not pursued: it loads the user's Claude Code hooks and 83 user skills into the run (side effects), and its earlier "success" had the same path contamination |

**Limitations:** single trials per cell except X9 (n=3). Haiku and luna-class models only. Compaction was triggered manually on tiny histories.

---

## 5. Proposed `RunRequest` instruction envelope

Two shapes: a **reference** for the durable command plane, queue rows, journal, and registry; and a **resolved envelope** handed to the harness.

```rust
// crates/proto/src/work_outline.rs (re-exported)

/// Identity of an immutable instruction revision. `revision_hash` is the
/// registry revision `hash` (sha256 over canonical entries + envelopeVersion).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionRef {
    pub revision_id: String,
    pub revision_hash: String,
}

/// Exact bytes delivered to the harness. Re-rendered by the HOST from the
/// registry revision; `envelope_sha256` must equal the revision's stored
/// `envelopeSha256` or dispatch fails closed (RevisionIntegrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionEnvelope {
    #[serde(flatten)]
    pub reference: InstructionRef,
    pub envelope_version: u32,
    pub envelope_sha256: String,
    /// Canonical rendered text, never part of `RunRequest::prompt`.
    pub rendered: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum InstructionTransition {
    /// New native session: deliver before the first Turn.
    Fresh,
    /// Native resume / respawn of an Execution Session that already carries
    /// this exact revision (crash recovery, idle reaper, retries). Adapters
    /// re-supply identically; never a different revision.
    Continue,
    /// Explicit application at a Turn boundary: replace `previous`.
    Replace { previous: InstructionRef },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionDelivery {
    pub envelope: InstructionEnvelope,
    pub transition: InstructionTransition,
}

// crates/proto/src/agent.rs — RunRequest
    /// Work Outline runtime instructions (resolved by the host just before
    /// dispatch). Must never reach the harness via `prompt`. Additive for
    /// serde, but ONLY ever sent inside `RunWithInstructions` commands so old
    /// hosts fail closed instead of dropping it (see section 8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<InstructionDelivery>,
```

Rendered envelope format (`render_envelope(version, entries)`, golden-tested, consistent with `m0-registry-persistence.md` section 7.8):

```text
<zeron_work_outline revision="<revision_id>" hash="<first 12 of revision_hash>">
These are Work Outline instructions for this Chat. They supplement, and never
override, the user's global preferences, repository instruction files
(CLAUDE.md / AGENTS.md), skills, or safety rules. Later and more specific
entries take precedence over earlier ones. This revision replaces any earlier
Work Outline revision.
1. [space root] <text>
2. [work item <title>, inherited] <text>
3. [work item <title>] <text>
</zeron_work_outline>
```

The revision id and hash are inside the delivered text so a transcript or debug capture can be matched to a revision. The preamble addresses the precedence risk in section 9.

Delivery details per adapter:
- **Claude**: write `rendered` to a Zeron-owned, content-addressed, 0600 file such as `<data_dir>/instructions/<envelope_sha256>.md`, then pass `--append-system-prompt-file <path> --system-prompt-snapshot off` on **every** spawn. Use a file rather than argv because of the Windows command-line limit and process-listing exposure; C10 verified the file form.
- **Codex**: put `developerInstructions: rendered` into `start_params`, which `thread/resume` already reuses (`codex/mod.rs:987-990`). For `Replace`, after `thread/resume` and before the first `turn/start`, call `thread/compact/start` and await completion. If compaction fails, fail the run with a capability error (never send the turn with ambiguous instructions). Optionally fall back to `thread/start` as a fresh Execution Session.
- The title paths (`run_title`) must strip `instructions` (the Claude title path already strips other fields, `claude/mod.rs:498-509`).

## 6. Adapter trait addition

```rust
// crates/harness/src/lib.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstructionSupport {
    Unsupported { reason: String, upgrade_hint: Option<String> },
    Supported(InstructionCapability),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionCapability {
    /// Where the envelope lands (for the Effective Instructions UI copy).
    pub channel: InstructionChannel,      // SystemPromptAppend | DeveloperMessage | Test
    /// How `Replace` is realized. Always a respawn; the variant says whether
    /// native conversation continuity survives.
    pub replace: ReplaceStrategy,          // RespawnResume (Claude) | RespawnResumeCompact (Codex) | FreshNativeSession
    pub max_rendered_bytes: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    // ...
    #[error("{harness} cannot deliver Work Outline instructions: {detail}")]
    Capability { harness: HarnessId, detail: String, upgrade_hint: Option<String> },
}

#[async_trait]
pub trait Harness {
    // ...
    /// Default: Unsupported. Every adapter must opt in explicitly (AC 12).
    /// May probe the CLI (cached by executable identity, like model_context).
    fn instruction_support(&self) -> InstructionSupport {
        InstructionSupport::Unsupported { reason: "no instruction channel".into(), upgrade_hint: None }
    }
}
```

Contract:
1. If `request.instructions.is_some()` and the adapter is not `Supported`, `run()` returns `HarnessError::Capability` before spawning. The engine also checks first (section 7); this is defense in depth.
2. Adapters deliver `envelope.rendered` byte-for-byte.
3. Per-adapter declarations:
   - Claude: `Supported{SystemPromptAppend, RespawnResume}`, only if a cached probe of `claude --help` lists `--system-prompt-snapshot` and `--append-system-prompt-file`. Otherwise `Unsupported` with upgrade hint "update Claude Code" (a version floor can replace the probe once the introducing version is known).
   - Codex: `Supported{DeveloperMessage, RespawnResumeCompact}`, gated on the schema advertising `ThreadResumeParams.developerInstructions` and `thread/compact/start`.
   - Cursor, opencode, all ACP: `Unsupported`.
   - Mock: `Supported{Test, RespawnResume}`. It should emit the received envelope hash in a test-only event for assertions.

## 7. Engine changes: capture, routing, and recovery

**Capture at queue time** (spec "A queued command captures the revision ID and content", AC 10):
- Composer, MCP, and service-op runs for a Chat that has a selected revision are sent as a new payload `SessionCommandPayload::RunWithInstructions { request, message_id, instructions: InstructionRef }` (new `kind`, section 8). The issuer resolves the Chat's **selected** revision, never the newest instruction edits. Issuers: `crates/ui/src/composer.rs:7074`, `crates/mcp/src/tools.rs:915`, and the `createChatForWorkItem` / `continueChatForWorkItem` operations.
- Queue rows: add `instructions: Option<InstructionRef>` to `QueuedMessage` (`doc/src/queue.rs:26`). The host stamps it in `queue_message` (`doc_host.rs:2580+`) when the issuer left it out, so capture still happens at queue time on the host.
- If a host drains a command or row **without** a ref for a Chat that has a selected revision (an old or unaware client), it resolves the Chat's selected revision and records application reason `hostResolved`. It never omits.

**Resolution** happens at one host-side choke point, `dispatch_with_source_context` (`doc_host.rs:4613`), which every fresh dispatch crosses. That function:
1. Resolves `InstructionRef` to an `InstructionEnvelope` from the registry revision row, re-rendering and verifying `envelopeSha256`.
2. If the row has not synced yet, waits with a bounded retry gate, like `missing_attachments` (`doc_host.rs:4172-4192`), then rejects. It never runs without instructions.
3. Checks `harness.instruction_support()`.

All of this must happen **before** `write_user_message` (`doc_host.rs:4371`, `sessions.rs:502`), so a rejected run does not leave an orphan user message. The command resolves `Rejected` with the actionable capability text.

**Transition choice** is engine-side, comparing the request ref to the Execution Session's applied ref:
- No native session yet: `Fresh`.
- Same ref: `Continue`.
- Different ref: `Replace{previous}`.

**Routing at Turn boundaries** (AC 11):
- Add `instructions: Option<InstructionRef>` to `RuntimeConfig` (`sessions.rs:75-84`). A different revision is then never routed into the text-only mailbox (`can_route`, `:100-102`).
- Change `dispatch_inner` (`:487-497`) so that a revision-only mismatch **while `turn_in_flight`** does not interrupt. The prompt is held and dispatched at Turn end, through the queue, exactly as `drain_queue` already waits (`doc_host.rs:3185`).
- A parked (idle) warm child with a different revision is restarted, which is already the behavior for config changes.
- Steers never carry instructions; they run under the live session's revision.

**Engine-initiated continuations** take the ref from the Execution Session, never from "latest":
- Live: from `RunHandle.runtime_config.instructions`. Covers orphaned steers, steer→Turn, RespondInput fallback, and startup retry. `last_requests` already stores the full request (`sessions.rs:516`), and the retry reuses `retry_request` verbatim.
- After restart: a host-local journal sidecar `<journals>/<chat>.instructions.json`, next to the existing attempts and restart-marker sidecars (`run_journal.rs:71-75`), holding `{native_session_id, cwd, applied: InstructionRef, epoch, turns: {user_message_id: InstructionRef}}`. It is written before spawn and fsynced.
- `recover_stale` (`sessions.rs:835-863`) revives the Turn with the ref captured for **that user message id**. This matches spec line 102: never substitute the newest revision.
- `request_from_chat_row` (`doc_host.rs:4752`) and `dispatch_queued` (`:3264-3285`) must take `instructions` from the row or the sidecar, not from `previous` (today they copy `previous.worktree` and `previous.auto_approve`). This is the "one `followup_request(chat)` funnel" also recommended in `m0-idempotency-command-plane.md`.
- `HarnessSessionRef` (`sessions.rs:65-68`) gains `applied: Option<InstructionRef>`.

**Applied-revision reporting point** (spec "Every active Execution Session separately reports the revision it actually received"):
- In `drive_run`, when the run emits `AgentEvent::SessionStarted` (it carries the native `session_id`), the adapter has accepted and spawned with the exact bytes. The engine then:
  1. Updates the sidecar.
  2. Writes the `instructionRevisionApplications` registry row: chat, revision, `executionSession: {nativeSessionId, epoch}`, turn = user message id, `reason` (`launch` | `continue` | `crashRecovery` | `retry` | `explicitApply` | `hostResolved` | `resumeFellBackFresh`), `result: delivered`. The idempotency key is `hash(chat, revisionId, nativeSessionId, epoch, userMessageId)`, so retries and replays never write a second application.
  3. Publishes `appliedInstructionRevision: Option<InstructionRef>` on `zeron_proto::Session` (`entities.rs:368-379`, additive optional), so the UI shows the session-delivered revision separately from the Chat selection.
- **Execution Session identity**: `epoch` increments when the revision changes under the same native id (Claude `Replace`). The native id changes when Codex silently falls back from resume to fresh (`codex/mod.rs:993-1000`); detect this as `SessionStarted.session_id != request.resume` and record `resumeFellBackFresh`. Delivery was still exact, because `start_params` carries the envelope.
- **Rejected** deliveries (capability or integrity) write `result: rejected` with the message and resolve the command `Rejected`.

## 8. Remote capability advertisement and fail-closed transport

**Reuse the existing device capability list**:
- Add `capabilities::WORK_OUTLINE_INSTRUCTION_ENVELOPE_V1 = "work-outline-instruction-envelope-v1"`. It means the engine understands `RunWithInstructions`, `QueuedMessage.instructions`, and the sidecar, and will reject rather than omit.
- Add per-harness tokens `"work-outline-instruction-envelope-v1/claude-code"` and `".../codex"`, computed at boot from `instruction_support()` probes.
- Change `workspace_host.rs:271-287` from the static `capabilities::current()` to `current()` plus the probed harness tokens. Also restamp when the harness registry re-probes (executable identity changes).
- `EngineInfo` carries the same list for the local daemon.

**Sender preflight** reuses `chat_host_supports` / `device_supports` (`ui/src/state.rs:1156-1173`), which treat missing declarations as unsupported. For `createChatForWorkItem`, check the Space's `device_id` for both the engine token and the chosen harness token. Unsupported hosts get an error that names the device and its stamped `version`: "Update Zeron on <device> (running <version>)" or "Update Claude Code on <device>". Unknown devices count as unsupported.

**Fail-closed transport**:
- Instruction-bearing runs use the **new payload `kind`**. Old hosts skip it (`schema.rs:465-473`) instead of silently dropping a field. Over `RELAY_COMMAND`, an old host's parse fails, and the sender sees an RPC error.
- The sender must surface "host does not support Work Outline instructions" when a `RunWithInstructions` entry stays Pending past a short grace and the host lacks the token. This avoids an indefinite "Sending…".
- Generalize the variant with `mustUnderstand: Vec<String>` so later features (multi-repo execution) can reuse it.

**The host is authoritative**: the CLI can be upgraded or downgraded after boot, so the host re-checks `instruction_support()` at drain. The device token is only advisory, for UI preflight.

---

## 9. Spec contracts that are infeasible as written, with faithful alternatives

1. **"A warm Execution Session … explicitly applies another [revision]" (spec line 100) and "Applying a revision occurs at a turn boundary" (line 101).** No harness supports an in-process replacement. Codex's only in-process route (`collaborationMode`, `thread/settings/update`) is additive: old and new revisions coexist in model-visible history, and the model got confused in 3 of 3 trials.
   *Alternative:* define explicit application as "begins a new Execution Session at the next Turn boundary".
   - Claude: respawn + `--resume`, conversation fully preserved.
   - Codex: respawn + resume + forced compaction (conversation kept as a summary) or a fresh thread.
   - The UI should say which, using `ReplaceStrategy`.
2. **"Each harness adapter must support fresh execution, native resume, crash recovery, and explicit revision application" (line 550) for every harness.** Infeasible for Cursor SDK 1.0.31 (no ungated channel; `systemPrompt` also replaces Cursor's prompt) and for all ACP agents (no protocol field). opencode is unverified.
   *Alternative:* AC 12's "capability declaration + reject" satisfies the contract. Work Outline Chats are limited to Claude Code and Codex in the MVP, and harness pickers for work-item Chats disable the others with the reason.
3. **"Instructions are runtime configuration, not visible prompt text" holds, but Codex persists them.** Codex stores them as a `developer` message in its rollout history: invisible in Zeron's transcript and in `thread/read` user items, but persisted. Claude records nothing with snapshot `off`. Not a violation, but the "immutable revision" guarantee for Codex depends on Zeron's re-supply discipline. Native Codex clients that resume the thread outside Zeron would keep the old developer message and drop it at their next compaction.
4. **"A Work Outline instruction cannot weaken" global, repository, or skill precedence (line 309).** It cannot be enforced. Claude places the envelope in the system prompt, above CLAUDE.md user-context. Codex places it as a developer message, the same tier as other developer context.
   *Alternative:* the precedence preamble in the rendered envelope (soft) plus documentation per channel. The spec should say "best effort; harness precedence is not semantically enforced".
5. **Harness-native subagents (AC 20).** The envelope reaches the main agent only (Claude `Task` subagents and Cursor `task` keep their own prompts; Codex children are untested). The spec should state that Work Outline instructions are not propagated to harness-native subagents in the MVP.

## 10. Risks

- **Claude snapshot flag drift.** The behavior is feature-gated ("No effect where system-prompt recording is not yet enabled") and new in recent 2.1.x. The adapter must always pass `--system-prompt-snapshot off` and probe for the flag. A future CLI that changes semantics would break exactness silently, so a nightly or real-harness test (C3, C4, C8 shape) should guard it. Chats created by older Zeron builds already carry a snapshot record; `off` overrides it (C4), but any spawn without `off` (for example the user running `claude --resume` in a terminal) resurrects the old prompt.
- **Codex latent swap at compaction** (X7, X8). Any code path that resumes without re-supplying the exact applied revision silently loses or changes instructions later. A unit test should assert that `thread/resume` params always carry the session's applied envelope.
- **Codex compact-and-replace** is verified once on a tiny history. The compaction summary may paraphrase earlier conversation that discussed the old instructions. It is an extra model call and a lossy history rewrite. A fresh thread is the conservative fallback.
- **Old or downgraded hosts.** `QueuedMessage` rows and plain `Run` commands on a host that predates the capability will run without instructions. Gating Work Outline Chats to capable hosts covers creation; a later binary downgrade remains a residual gap that cannot be closed from new code.
- **Write-before-check ordering.** Today the user message is written before dispatch (`doc_host.rs:4371`, `sessions.rs:502`). The capability and integrity checks must move ahead of it, or rejected runs leave orphan prompts.
- **Prompt cache cost** of snapshot `off` (the prompt is re-rendered every request); unmeasured.
- **Size**: Claude via file (no argv limit). Codex via JSON-RPC (no practical limit). The registry budget (9,000 characters of effective instruction text) bounds it in any case.
- **Spike scope**: n=1 per cell for most cells, small models, manual compaction. Re-run the matrix (scripted from `/tmp/zeron-m0/*.py|sh`, to be ported into `crates/harness/tests/real_*` behind the existing real-harness test gating) on each harness version bump.
