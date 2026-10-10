# Work Outline — Milestone 0 contract

Status: accepted for implementation (2026-10-10). This file is the binding resolution of the
Milestone 0 audits. Where it differs from `../work-outline-implementation-specification.md`, this
file wins and the specification's "Implementation contract amendments" section points here.

Evidence (file:line citations, experiments, test plans):

| Area | Audit |
|---|---|
| Registry rows, ordering, materialization, Space suppression | `m0-registry-persistence.md` |
| Instruction envelope delivery per harness (live spike) | `m0-instruction-delivery.md` |
| Multi-repository execution scope | `m0-execution-scope.md` |
| Idempotent workflows and the command plane | `m0-idempotency-command-plane.md` |
| UI integration and Milestone 1 file plan | `m0-ui-integration.md` |

## 1. Vocabulary (fixed)

- **Chat** — durable user-facing conversation; the only Work Outline association unit. Keyed by the
  existing chat id.
- **Execution Session** — bounded harness continuity: `(chat id, harness, native resume id,
  execution root, applied revision id)`. Any change to the applied revision produces a new
  Execution Session, even when the harness continues the same native transcript (Claude Code
  `--resume` with a replaced system-prompt file). There is no Execution Session entity today; the
  host reports it (section 5.6). Never an association key.
- **Run** — one harness process or remote invocation inside an Execution Session.
- **Turn** — one user prompt until settle, input request, or failure.
- **Execution State** — transient Working / Awaiting Input / Idle / Error. Never stored as a work-item
  state.

## 2. Ownership boundary

- Owner = existing Space (`spaces` registry row, `(deviceId, absolute path)`). No Project entity.
  Space-root scope = rows whose `workItemId` is null; no synthetic root work item.
- Projectless Chats (no `spaceId`) cannot be assigned. They render under Unassociated Chats only in
  the "All spaces" Outline view.
- `spaceId` is write-once on every outline row. LWW cannot enforce immutability, so it is a
  writer contract (mutators never emit it after create) plus a reader contract (materialization
  quarantines rows whose references disagree on Space).
- Assignment never writes `chats`, `spaces`, or `sessions` rows (registry audit R8).

## 3. Persistence

Registry is a row table keyed by `(kind, id)` with per-field HLC LWW and tombstones, synced through
the private per-user registry room; the edge preserves unknown kinds and fields. Outline data is
additive row kinds only. No SQL tables, no transcript-doc metadata, no `PersistedState.v` bump.

### 3.1 Row kinds

Schemas exactly as `m0-registry-persistence.md` §7.1–7.9, kinds:
`workItems`, `workItemInstructions`, `workItemChatAssignments`, `executionConfigurations`,
`workItemExternalReferences`, `userActionRequests`, `workItemInstructionProposals`,
`instructionRevisions`, `instructionRevisionApplications`, plus `chatExecutionBindings` (§6.2 here).

Milestone 1 ships `workItems`, `workItemChatAssignments`, `workItemExternalReferences`.
Later kinds land with their milestone; materialization tolerates their absence.

### 3.2 Mandatory write-path rules

From registry audit §10, all required in Milestone 1 shared code:

- R1 `RegistryDoc::row_state(kind, id) -> Absent | Live | Tombstoned`; create-if-absent never revives a
  tombstone unless the operation is an explicit restore.
- R2 After creation, mutations are `update` ops that never carry immutable fields.
- R3 Before every outline write, observe the row's clocks (and the destination parent's) into the
  local `HlcClock` so a slow-clock device cannot lose causally later edits.
- R4 Client-side size gate for each op (stay well below the edge's 16 KiB op cap) and byte-aware
  chunking in `enqueue_ops`; an oversize op must never enter the queue (it would wedge all registry
  writes).
- R5 Operation ids (section 4).
- R6 One batch per logical mutation where it fits; larger cascades are replay-safe.
- R7 Lenient enums and optional fields on every decoded row; unknown status → `pending` + repair
  marker. Never add a strict field allowlist (the `sidebarPins` edge pattern) for outline kinds.

### 3.3 Ordering

Base-62 fractional order keys (rocicorp `fractional-indexing` construction, 1..=64 chars), sort
`(orderKey, id)`. `placement` = `{ before: id } | { after: id } | { first } | { last }` relative to
siblings, never an integer. Tie groups are re-keyed in the same batch; a parent is capped at 2,000
children; length overflow rebalances the sibling group. Parent + order key are always written
together in one op (placement group P).

### 3.4 Materialization and repair

Pure `materialize_work_outline(space_id, inputs)` per registry audit §8. Cycle victim =
greatest `(clock of parentWorkItemId, id)` — the spec's `updated_hlc` maps to the parent field's
clock because rows have no row-level HLC. Missing or cross-Space parents → root + repair marker.
Write-time cycle validation walks the raw parent graph and persists the pending repair in the same
batch as the move. `repairWorkOutline(spaceId)` persists derived repairs explicitly.

### 3.5 Space deletion and profile isolation

Materialization requires a live Space row (`available=false` otherwise). `delete_space` also
tombstones owned outline rows; the 2-minute `sweep_orphans` pass tombstones outline rows of
non-live Spaces only when `registry_synced()`. Profiles are isolated by the existing per-profile
registry store. Recreated folders get a new Space id and never adopt an old outline.

Accepted pre-existing risk: 30-day tombstone GC lets a device offline longer resurrect deleted rows
(including Spaces). Not addressed by this work.

### 3.6 Edge

No edge change is required for Milestone 1 (unknown kinds and fields pass). Optional
`validateOp` backstops are deferred because edge deploys happen only from `main`; client-side
gates (R4) are mandatory instead. Rust kill switch `ZERON_DISABLE_WORK_OUTLINE_WRITES`.

## 4. Idempotency

- `operationId` = client-generated UUID (v4 acceptable; v7 preferred where available).
- Derived ids: `derive_id(operationId, role) = hex(sha256(operationId ‖ 0x00 ‖ role))[..32]`.
  Roles: `chat`, `message`, `command`, `assignment`, `revision`, `application`, `binding`,
  `request`, `instruction`, `item`.
- Every multi-step workflow replays all steps with create-if-absent; workflow state is derived from
  durable facts, not a separate journal. Each derived record carries `inputsHash`; replaying an
  operation id with different inputs fails with `OperationConflict`.
- Core prerequisites (idempotency audit §9): `queue_command` becomes id-idempotent (a duplicate
  command id is a no-op returning the existing entry); a public multi-op registry batch writer;
  a tombstone-aware reader (R1). The UI must not delete a work-item-launched Chat on partial failure.

`createChatForWorkItem` state machine: idempotency audit §7. The first Turn is always queued last,
with the derived command id, so the processed ledger dedupes execution.

## 5. Instruction delivery

### 5.1 Envelope

Per delivery audit §5: `RunRequest.instructions: Option<InstructionDelivery>` with
`InstructionEnvelope { revision_id, revision_hash, envelope_version, envelope_sha256, rendered }`
and `transition: Fresh | Continue | Replace { previous }`. Rendering is a pure, versioned,
golden-tested function of the revision entries.

### 5.2 Revision storage (spec amendment)

`instructionRevisions` stores ordered entries, `envelopeVersion`, `envelopeSha256`, and the
canonical `hash` — not the rendered text, which would exceed the 16 KiB op cap. Delivery re-renders
and verifies `envelopeSha256`; a mismatch is a hard `RevisionIntegrity` error. Budget: ≤ 9,000
effective instruction characters and ≤ 32 entries per revision; over budget fails before any write.

### 5.3 Capture

The session-doc command carries only `{ revisionId, revisionHash }` (no outline content in
transcript docs). The host waits for the exact revision row to sync, verifies it, and never
substitutes another revision. Instruction-bearing runs use a new command kind, so hosts that
predate it skip the command instead of running without instructions. Engine-built follow-up runs
(crash recovery, steer fallback, queue promotion — six sites) take the revision from the live
Execution Session or a journal sidecar keyed by user message id.

### 5.4 Harness matrix (spike results)

| Harness | Channel | Fresh / warm / queued / crash respawn / resume | Revision change |
|---|---|---|---|
| Claude Code ≥ 2.1.285 | `--append-system-prompt-file` + `--system-prompt-snapshot off`, re-supplied on every spawn | supported | respawn with `--resume` and the new file at a turn boundary (conversation preserved) |
| Codex ≥ 0.154 | `developerInstructions` on `thread/start`; always re-supply the session's exact applied revision on `thread/resume` | supported | candidate: resume with new value + `thread/compact/start` (one trial; Milestone 2 must prove it with repeated trials); fallback: fresh thread with a visible context-reset notice. Never a live mid-thread swap (duplicate instructions caused refusals 3/3). |
| Cursor SDK, ACP agents (Devin, Grok, Hermes, Pi, Antigravity) | none | rejected with `HarnessError::Capability` | n/a |
| opencode | not installed; untested | rejected until proven | n/a |

The Claude snapshot flag is version-gated; Milestone 2 adds a regression check at boot that marks the
capability unsupported if the flag is rejected.

### 5.5 Capabilities

`Harness::instruction_support() -> InstructionSupport` (default `Unsupported { reason,
upgrade_hint }`). The engine checks before writing the user message; the adapter checks again.
Hosts stamp `Device.capabilities`: `work-outline-v1`, `work-outline-instruction-envelope-v1`,
`work-outline-instruction-envelope-v1/<harness>`, `work-outline-multi-repo-v1`. Clients reject
operations against hosts lacking a required token with an upgrade message.

### 5.6 Applications and Execution Session reporting (spec amendment)

`instructionRevisionApplications` rows are written once, by the client, when a revision is
selected for a Chat (`reason: launch | explicit | fork-inherit`, `result: accepted`, target
Execution Session optional, turn boundary = the user message id of the first Turn that uses it).
The Chat's selected revision is the latest accepted application by `(appliedHlc, id)`. Actual
delivery is reported separately by the host as an additive `executionSession { id, nativeResumeId,
harness, executionRoot, revisionId, revisionHash }` field on its host-written session-status row,
written on `SessionStarted`. The UI never infers delivery from selection.

### 5.7 Turn-boundary policy

Applying a revision while a Turn runs never touches that Turn; it takes effect at the next Turn by
starting a new Execution Session. If the harness cannot preserve the conversation across the swap,
the user confirms a context reset first.

### 5.8 Precedence (spec amendment)

Claude places the envelope in the system prompt, above repository files, so "cannot weaken
repository or global instructions" cannot be enforced mechanically. The rendered envelope opens with
a fixed preamble that states Work Outline instructions supplement and never override repository,
global, skill, or runtime-safety instructions. Instructions do not reach harness-native subagents.

## 6. Execution scope

### 6.1 Configuration

`executionConfigurations` row `s:{spaceId}` or `w:{workItemId}`, one JSON `configuration` field:
`{ executionRoot, bindings: [{ repoId, repoFingerprint, repoPath, member, primary, worktreeId?,
worktreePath?, expectedRef? }], schema: 1 }`. Repository identity = existing topology repo id +
root-commit fingerprint. Resolution is whole-row: nearest work item with a row, then ancestors,
then Space default, then implicit `{ executionRoot: Space.path }`.

### 6.2 Per-Chat captured binding (spec amendment)

A `chatExecutionBindings` row keyed by chat id captures the resolved configuration at launch.
Later edits to a work item's configuration never retarget existing Chats; the item shows Chats whose
binding differs as exceptions.

### 6.3 Validation

A single validator with explicit error variants and remediation text and no fallback variant
(execution audit §5.3). It runs at client preflight, at host `execute(Run)` before `claim_chat`,
and at `dispatch_with_source_context`. For bound Chats the ten documented fallbacks are disabled,
resume cwd mismatch is an error, and `zeron/*` branch auto-minting and auto-renaming are off.

### 6.4 Decisions

1. Offline: `createChatForWorkItem` is blocked when the owning device cannot be validated. Continuing
   an existing bound Chat queues durably; the host revalidates at drain and rejects rather than
   falling back.
2. Worktree creation for bound Chats uses the existing local `.worktrees/<label>/<task>` flow, made
   idempotent; selecting an existing worktree works on any device.
3. Member repositories outside the execution root are a blocking error unless the harness supports
   additional directories; Zeron never widens the sandbox silently.
4. Non-Git workspace roots are out of the MVP.

## 7. User interface decisions

- Overview mode is persisted as flags in `overview-ui-flags.json`: `viewMode:canvas`,
  `viewMode:outline`, else List; exactly one written. Older builds read Outline as List.
- New attention indicators use theme tokens (sidebar vocabulary), not the Overview's hardcoded
  red. Existing Overview colors are unchanged.
- Palette commands that need a selected work item open Outline, focus it, and show a short notice
  when nothing is selected; they are never hidden.
- Child Chats (`parentChatId`) render nested under their parent Chat in Outline when the parent is
  shown, otherwise under their own assignment or Unassociated Chats. They never disappear.
- External references open per the web-link preference where an internal browser surface exists;
  from Overview-hosted Work Item Details (no source Chat) they open in the external browser.
- Canvas positions are never written by outline code (`position_for` must not be called from
  association paths).

## 8. Agent tools (Milestone 4 prerequisites)

`crates/mcp` exists but is not injected into any harness, origin is a self-declared environment
variable on an unauthenticated loopback socket, and `create_chat` is not idempotent. Milestone 4
requires an engine-minted per-Run token, an allow-listed agent RPC surface, and a mandatory
idempotency key.

## Milestone 1 discoveries

Recorded by the Milestone 1 backend (`crates/doc/src/registry/work_outline.rs`,
`crates/engine/src/work_outline.rs`). Each item is either a faithful reading of an ambiguous rule
or a gap later milestones must close.

1. **Durable idempotency without a new table.** `createWorkItem` derives its id
   (`derive_id(operationId, "item")`) and stamps an additive, immutable `inputsHash` field on the
   `workItems` row; external references are content-keyed and assignments are keyed by Chat id.
   Every mutation is also recorded in the existing device-local processed-command ledger
   (`processed_commands` in the profile `DocsStore`, global key space shared with the chat command
   plane) under two reserved keys: `work-outline:op:{operationId}` and
   `work-outline:in:{inputsHash}:{operationId}`. A replay whose operation key is present returns
   the recomputed reply without writing (all reply ids are derived from inputs); a present
   operation key without the matching inputs key is `OperationConflict`. Crash safety: the keys
   are queued under the registry lock with the write and claimed in the same SQLite transaction
   as the registry snapshot that contains the write (`DocsStore::save_snapshot_marking_processed`),
   so the ledger never records a write the snapshot lacks, and a crash before that save loses
   both (the replay then re-applies a write that never became durable). The doc-layer mutators are
   additionally compare-before-write (unchanged fields, an item already at the requested
   placement, an already-archived/restored/assigned target are no-ops). TTL: none; the ledger is
   never pruned, so an operation id is consumed for the life of the profile store on that device.
   The ledger is device-local: a replay of the same id against another device's engine is guarded
   only by the compare-before-write mutators. An in-memory cache of the 4,096 most recent
   operation ids remains the fast path.
2. **Revival exceptions to R1.** An explicit `assignChatToWorkItem` may revive a tombstoned
   assignment row (assignment rows are only tombstoned by Space cleanup, and the Chat id is the
   only possible key), and re-adding a removed external reference revives its content-derived row
   (registry audit 7.5). A retried `createWorkItem` whose row is tombstoned fails with `notFound`.
3. **Re-keyed siblings keep their stored parent.** Tie-group and overflow re-keys write group P
   with the sibling's stored raw `parentWorkItemId` (not its repaired parent), so a derived repair
   stays derived. Every op in one batch gets its own fresh HLC: two ops on one row sharing a clock
   would make the second a no-op under strict `>` LWW.
4. **Archived targets.** Creating under, moving under, moving descendants to, or assigning a Chat
   to an archived work item fails with `archived`. Moving or renaming an archived item is allowed.
   Restore clears the item and descendants whose `archiveCause` is `cascade`, stopping at
   descendants the user archived themselves.
5. **Raw-cycle repair on create too.** The "persist the pending cycle repair in the same batch"
   rule is applied to every write that places items under a destination (create, move, archive
   with `moveDescendants`), not only moves.
6. **Rollup rule.** Status counts cover the item and all descendants that are not archived (an
   archived item is not counted; its non-archived descendants are). `associatedChatCount` counts
   the item's own `associated` Chats only (a `danglingChat` is listed in `chatIds` but not counted).
7. **Unattributable repairs.** A `workItems` row without a decodable `spaceId` cannot be assigned to
   a Space, so its `missingSpace`/`malformed` marker appears in every Space's snapshot. Assignment
   and reference markers appear only where the Chat or work item is attributable.
8. **Size gate scope.** The 15,000-unit op gate applies to outline writers and the public
   `RegistryDoc::write_ops`; pre-existing writers (chats, sessions, sidebar) are unchanged except
   that `enqueue_ops` now also chunks at 512,000 serialized units per batch (count chunking at 400
   is unchanged for small ops).
9. **Not done in Milestone 1 backend.** Local-to-synced import (registry audit C6) does not copy
   outline rows; outline rows are non-migrating until `local_import` is extended. No edge
   `validateOp` backstop (deferred per section 3.6). `repairWorkOutline` persists cycle victims and
   invalid order keys only; missing parents stay derived (the parent may not have synced yet).
