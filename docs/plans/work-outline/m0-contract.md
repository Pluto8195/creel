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
3. **Re-keyed siblings are order-only writes.** The group P invariant (parent + order key in one
   op, one clock) applies to user placement ops: create, move, and the items placed by archive
   `moveDescendants`. A tie-group or overflow re-key of a sibling OTHER than the placed item writes
   `orderKey` with a fresh clock but carries `parentWorkItemId` at the sibling's existing value and
   existing clock (`RowOp.clocks`; a never-written parent, or a victim already repaired in the same
   batch, omits the field). This device's view of a sibling's parent may be stale: stamping it with
   a fresh clock would silently revert a concurrent offline move of that sibling on another device
   (acceptance 2). With the old clock a concurrent move always wins, and a torn pair can only
   misorder (the item lands in its new parent with a key minted for the old one), never lose the
   move. A re-key keeps the stored raw parent, so a derived repair stays derived. Every op in one
   batch gets its own fresh HLC: two ops on one row sharing a clock would make the second a no-op
   under strict `>` LWW.
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
10. **Idle sweep, delete clocks, import, op budget.** The orphan sweep computes doomed rows on a
    read path and only mutates (change bump, publish, snapshot) when there are any. Every
    tombstone batch (`delete_row_ops`: Space cascade, sweep, chat deletes) observes the doomed
    rows' clocks before minting (R3), so a fast-clock device's rows cannot outlive the delete.
    `local_import` skips `work-outline:` ledger keys because outline rows are not imported (a
    carried key would make a retried operation falsely succeed without writing). An in-limit
    field whose serialized op still exceeds the 15,000-unit budget (JSON escaping) fails with
    `limitExceeded` naming the dominant field, for example "Description is too long to save (N of
    15000 units after encoding)".
11. **Overview watches every Space's outline.** The Overview declares all Spaces visible for
    `WatchWorkOutline` in every mode, not only in Outline mode, so the List and Canvas path chips
    (and attention) have data. This deliberately deviates from m0-ui-integration section 9; the
    cost is bounded by the Space count.

## Milestone 2 contract notes

Recorded by Milestone 2 stage 1 (wire contracts and pure core: `crates/proto/src/
work_outline_instructions.rs`, additions to `crates/proto/src/work_outline.rs`, `crates/harness/
src/instruction_support.rs`). Each item is a decision later stages build on.

1. **Revision entries carry `scopeTitle`.** Registry audit 7.8 lists `instructionId`, `sourceKind`,
   `scopeId`, `text`, `order`, `provenance`; entries also store `scopeTitle` so the envelope is
   reproducible from the stored entries alone. Titles are snapshotted: renaming an item changes the
   current effective hash (drift), never an existing revision. Space-root entries use the Space id
   as `scopeId` and the fixed title `Space root` (a Space rename never drifts).
2. **Envelope v1** (`render_instruction_envelope`, golden-tested). An opening tag
   `<zeron_work_outline_instructions version="1" hash="<first 12 hex of the revision hash>">`, the
   fixed `ENVELOPE_V1_PREAMBLE` (section 5.8: supplements and never overrides repository
   instruction files, global instructions, skills, and runtime safety rules; later, more specific
   entries take precedence; replaces earlier Work Outline instructions), then entries numbered
   from 1, grouped under one label per consecutive scope (`[Space root]`, `[Work item: <title>]`),
   then the closing tag and a final newline. Text: CRLF/CR become LF, trailing whitespace is
   trimmed, continuation lines are indented three spaces, a literal closing tag inside text or
   titles is neutralized to `</ zeron_work_outline_instructions`. No entries: the body is `No Work
   Outline instructions apply.` Deviations from the delivery audit section 5 sketch: the revision
   id is not in the text (rendering is a pure function of `(version, entries)`; the hash
   identifies the content, and `InstructionEnvelope.revisionId` travels beside it); scope labels
   group entries instead of prefixing each line; local versus inherited is not marked (it is a
   property of the viewing item, not of the revision).
3. **Hashes.** `hash = hex(sha256(canonical_json({"entries", "envelopeVersion"})))`; canonical
   JSON = object keys sorted by byte order, no whitespace, `serde_json` scalar spelling, non-ASCII
   as raw UTF-8, absent optional fields omitted (a later optional entry field leaves old hashes
   valid while absent). `envelopeSha256 = hex(sha256(utf8(rendered)))`.
4. **Budgets** (`limits`): instruction text <= 6,000 and excerpt <= 1,500 Unicode scalar values;
   <= 50 live instructions per scope; a revision has <= 32 entries and <= 9,000 effective
   characters (sum of entry text; preamble and labels not counted) and its serialized `body` <=
   14,000 UTF-16 units (headroom under the 15,000-unit op gate). `build_revision_body` fails with
   `limitExceeded` and an actionable message before any write; the resolver never fails and
   reports `withinBudget` / `budgetError` instead.
5. **Provenance.** Instruction rows keep the copied excerpt; effective entries and revisions carry
   `createdBy`, `source` without `excerpt`, and `sourceProposalId` (the excerpt would consume the
   revision budget; `contentHash` still identifies it). `contentHash` is `sha256(excerpt)` hex,
   reading registry 7.2 "exact text at creation" as the copied source text (acceptance 21).
6. **Resolver lives in proto** (`resolve_effective_instructions`, `..._in`,
   `WorkOutlineSnapshot::effective_instructions`): it walks materialized `WorkItemView`s, whose
   `parentWorkItemId` is already the repaired parent, so no doc types are needed. Space-root
   entries come first and are `inherited: true`; an unknown item yields no entries; a work-item
   instruction whose `spaceId` differs from its item's is excluded (orphan).
7. **Applications** follow section 5.6, superseding registry 7.9 and delivery audit section 7:
   `reason` is `launch | explicit | forkInherit` on the wire (`fork-inherit` accepted on read);
   `result` is only `accepted` (`success` and unknown values read as `accepted`); `turnBoundary`
   is `{messageId?}`. Delivery outcome is never an application row; hosts report it through
   `ExecutionSessionReport` on the session-status row.
8. **Delivery wire names.** The delivery audit's `InstructionRef` is `InstructionRevisionPin
   {revisionId, revisionHash}`; `InstructionEnvelope` carries `revisionId` / `revisionHash` as
   plain fields (no flatten); `Replace` is `{"kind":"replace","previousRevisionId":…}`.
9. **Integrity.** `verify_revision_body` checks, in order, `entries[i].order == i`, the envelope
   version, the hash, and the envelope digest; any failure is `revisionIntegrity`. A revision
   view whose `integrity` is missing or unknown reads `corrupt`. New repairs:
   `orphanInstruction {instructionId, workItemId}`, `corruptRevision {revisionId}`.
10. **Comparison** (`compare_instruction_revisions`): entry-level `added` / `removed` /
    `changed` by instruction id, `reordered` = shared ids outside one LCS of their order, and a
    deterministic LCS line diff of the two rendered envelopes (tables over 4,000,000 cells fall
    back to remove-then-add). Entry numbers shift when entries are inserted, so the UI should lead
    with the entry-level lists. `identical` iff both hashes are equal and non-empty.
11. **Operations.** New `WorkOutlineMutate` ops: `createWorkItemInstruction` (`spaceId` required
    for both scopes; `placement` tagged by `type` with `siblingInstructionId`; reply
    `instructionId`), `updateWorkItemInstruction` (text only), `moveWorkItemInstruction`,
    `archiveWorkItemInstruction`, `createInstructionRevision` (reply `revisionId`),
    `applyInstructionRevision` (reply `applicationId`). Read-only questions use a new local,
    non-forwardable method `WorkOutlineQuery`, tagged by `query`:
    `resolveEffectiveInstructions`, `compareInstructionRevisions`, `getInstructionRevision`. Until
    stage 2 the engine and doc layer answer every new op and query with `unsupported`, before
    claiming replay-ledger keys, so a later build can apply the same operation id.
12. **Snapshot** gains `instructions` (all, archived flagged), `chatInstructionStates` (`drift` =
    a selection exists and its hash differs from `currentEffectiveHash`), and `revisions` (only
    those referenced by Chat selections or deliveries). The materializer emits them empty until
    instruction rows are read.
13. **Capabilities.** `work-outline-instruction-envelope-v1` and
    `work-outline-instruction-envelope-v1/<harness wire id>` are defined but not in `CURRENT`
    until delivery ships.
14. **Harness.** `InstructionSupport::{Unsupported {reason, upgrade_hint}, Supported {channel:
    SystemPromptAppend | DeveloperInstructions | Test, replace_strategy: RespawnResume |
    CompactSwap | FreshSession, max_bytes}}`, default `Unsupported` for every adapter;
    `HarnessError::Capability {harness, detail, upgrade_hint}`; `require_instruction_support` is
    the adapter-side fail-closed check, not yet called from adapters (stage 2 wires it with
    delivery). Every `RunRequest` construction site passes `instructions: None`.

## Milestone 2 registry discoveries

Recorded by the Milestone 2 registry backend (`crates/doc/src/registry/work_outline/instructions.rs`,
`.../materialize.rs`, `crates/engine/src/work_outline.rs`, `crates/engine/src/workspace_host.rs`).
One additive proto change: `ChatInstructionState.selectedRevisionIntegrity` (optional,
`SelectedRevisionIntegrity = ok | missing | corrupt`, lenient: unknown reads `corrupt`), and the
`build_revision_body` body-size message now explains per-entry overhead. Everything else below is
registry row fields and behavior.

1. **Additive row fields.** `workItemInstructions`, `instructionRevisions`, and
   `instructionRevisionApplications` carry an immutable `inputsHash` (as `workItems` does), so a
   replay of an operation id with other inputs is `operationConflict` on any device, not only
   where the ledger holds it. Applications also carry `revisionHash`, the hash the writer verified.
   `reason` is written in wire spelling (`launch`, `explicit`, `forkInherit`); `result` is always
   `accepted`; `appliedHlc` equals the clock of the op that wrote it. `sessionRef` is never written
   (the request has no Execution Session target).
2. **Selection rule.** The selected revision is the newest accepted application by
   `(appliedHlc, id)`, never substituted (section 5.3): when its revision is missing (not synced) or
   corrupt (fails integrity or belongs to another Space), the snapshot reports it through
   `selectedRevisionIntegrity` and `InstructionRegistry::selected_instruction_revision` still
   returns that pin, carrying the hash the application recorded (`revisionHash`), so delivery fails
   closed on the integrity check instead of delivering an older revision. Re-applying an intact
   revision is the user's remedy. Applying observes every existing application of the Chat first,
   so an application made after seeing another device's always sorts after it. The doc reader and
   the snapshot use the same rule.
3. **Budget enforcement.** Text <= 6,000 and excerpt <= 1,500 characters are checked on every
   write. Create checks every item whose effective instructions include the scope (the item and
   its descendants; for the Space root every item plus the root alone) with `build_revision_body`,
   memoized per chain of contributing scopes, and fails with `limitExceeded` naming the item. An
   edit is checked only when the text grows (characters or JSON-escaped units), so shortening is
   always allowed even when an item is already over budget (two devices can push one over
   concurrently); archive is never checked. Two observations: the 50-per-scope cap is unreachable
   through the API because the 32-entry revision cap binds first (it guards rows written by other
   writers), and about 180 units of JSON overhead per entry mean many-entry sets hit the
   14,000-unit body cap well before 9,000 characters; that error reads "Instructions exceed the
   revision size limit (N of 14000 units after encoding; each instruction adds about 200 units of
   overhead beyond its text, …)".
4. **Errors.** Creating an instruction on an archived item is `archived`; editing or moving an
   archived instruction is `archived`; archiving twice is a no-op. `applyInstructionRevision`:
   missing Chat or revision `notFound`, projectless Chat `projectless`, Chat without a live
   assigned item `invalid` ("Assign the Chat to a work item before applying instructions"),
   revision or assignment of another Space `crossSpace`, corrupt revision `revisionIntegrity`.
   Revisions may be created for archived items, and applications target the Chat's current item.
5. **Provenance.** `source.contentHash` is recomputed as `sha256(excerpt)` whenever an excerpt is
   present (a client-supplied hash is replaced); a hash without an excerpt is kept. `source`,
   `workItemId`, `spaceId`, `createdBy`, `createdAt` are written only by the create upsert.
6. **Ordering.** Instructions sort `(orderKey, id)` on raw strings within a scope (the resolver's
   order); invalid keys are re-keyed by the placement window like work items. Re-keyed sibling
   instructions are order-only `orderKey` writes.
7. **Snapshot.** `instructions` lists every instruction of the Space, archived and orphan ones
   included (orphans also get `orphanInstruction` and are never effective).
   `chatInstructionStates` covers Chats of the Space that are associated or have an application
   here. A Chat without an associated item has no `currentEffectiveHash` and never drifts; an
   associated Chat with no selection drifts iff its item has effective instructions. Only the
   currently selected and delivered revisions are verified and listed in `revisions` (corrupt ones
   flagged and reported as `corruptRevision`); revision rows are never pruned, so frames must not
   grow with launches. Materializer inputs carry only each Chat's newest application in this
   Space, the revision rows those and the session reports reference, and session rows that have
   an `executionSession`.
8. **Execution Session report.** `WorkspaceHost::report_execution_session` writes only the
   `executionSession` field with an `update` (so status writes and reports never clobber each
   other), observes the row's clocks first, writes nothing for an equal report (no change-counter
   bump), never revives a tombstoned row, and, when no status row exists yet, creates one as
   `idle` so `read_sessions` keeps decoding it. Nothing calls it yet; delivery wires it on
   `SessionStarted`.
9. **Queries.** `getInstructionRevision` returns a corrupt revision flagged (`integrity: corrupt`)
   rather than an error; `compareInstructionRevisions` requires both revisions to exist
   (`notFound`) and verify (`revisionIntegrity`); `resolveEffectiveInstructions` is `notFound` for
   an unknown item or a suppressed Space. Queries ignore the kill switch; every instruction
   mutation honors it and the replay ledger exactly like Milestone 1 operations.
10. **Not done.** The `work-outline-instruction-envelope-v1` capability is not advertised (delivery
    owns it). Application rows are cascaded only with their Space, not on Chat deletion.
    Instruction rows are not copied by local-to-synced import (same as Milestone 1 rows).

## Milestone 2 UI discoveries

Recorded by the Milestone 2 user interface (`crates/ui/src/work_outline/{actions,instructions,
revision_diff,details_instructions}.rs`, `shell/instructions_ui.rs`, edits to `details_panel.rs`,
`outline_view.rs`, `overview.rs`, `shell/work_outline_ui.rs`). Each item is a reading of an
ambiguous rule or a gap later milestones must close.

1. **Local first.** Effective Instructions, the drift analysis, and Compare are computed from the
   `WatchWorkOutline` snapshot with the shared pure functions (`effective_instructions`,
   `canonical_revision_hash`, `build_revision_body`, `compare_instruction_revisions`). The
   `WorkOutlineQuery` facade (`WorkOutlineActions::query` + `expect_*`) is used only when a Chat's
   applied revision is missing from `snapshot.revisions` (`getInstructionRevision`). The UI
   therefore works while the engine answers instruction operations with `unsupported`; writes then
   show "Instructions aren't available on this host yet" (`actions::instruction_error_text`) and
   keep the draft.
2. **Operation ids.** Transport retries resend identical params (M1). Across user retries an
   `OperationSlot` reuses the id while the inputs are unchanged and re-mints it once they change
   (a host that recorded the first attempt would otherwise answer `operationConflict`). Apply keys
   the `createInstructionRevision` id by `(item, current effective hash)` and the
   `applyInstructionRevision` id by `(chat, revision id)`; both reset after success.
3. **Persistence policy (acceptance 28).** An instruction is written only by Save (button, or Enter
   in the generic composer; Shift+Enter breaks a line). Cancel discards at once (explicit, no
   prompt). Blur never saves. Closing details, switching items, opening a Chat, or Escape with a
   dirty instruction draft goes through the existing "Keep Editing / Discard" confirmation. One
   instruction draft is open at a time; saving unchanged text writes nothing.
4. **Drift rule.** The confirmation appears iff at least one assigned Chat (assignment `associated`;
   dangling Chats excluded) would see its current canonical effective hash change. Triggers: Chat
   association/move (an unassociated Chat counts as an empty list), item move (Outline move
   picker), the inheritance toggle, and edit/archive of an instruction of any scope ("ancestor" is
   read relative to the Chat, so a local edit on an item with its own Chats also asks). Counts are
   the affected Chats and the live items whose effective instructions change. Not gated: creating
   or reordering instructions (not in the spec list; affected Chats still show drift afterwards),
   archive with `moveDescendants`, and renames (titles are snapshotted, so a rename drifts).
5. **Apply.** `createInstructionRevision(itemId)` then `applyInstructionRevision(chatId,
   revisionId, reason: explicit)` with no `turnBoundary`. A working Chat shows "Applied · takes
   effect at the next Turn". The Compare dialog stays open after Apply and shows the outcome.
   Caveat: a replayed revision operation returns the revision the host minted first, which is the
   intended idempotent outcome even if the outline changed in between under the same inputs key.
6. **Delivery.** "Selected r… · Delivered r…" (or "Not delivered yet") appears whenever the host's
   delivered revision id differs from the selection, in the chat header and the Compare dialog.
   Labels use the first 7 hex characters of the revision hash (the id when no hash is known).
7. **Space root.** The details surface has a Space-root variant (`item_id == ""`,
   `details_panel::SPACE_ROOT_ITEM_ID`) that edits Space-root instructions only. Entries: an
   "Instructions" button on each Space row (multi-Space), a "Space root instructions (N)" line atop
   a single-Space Outline, the "Space root" breadcrumb in item details, and inherited Space-root
   entries in Effective Instructions.
8. **Effective view.** Collapsed until opened; entries in effective order with a Local/Inherited
   marker, the source title (inherited sources link to their details), "Parent instructions stop
   at …" when a boundary applies, the precedence rule, and "N / 9000 characters · M / 32 entries"
   in the danger color with `budgetError` when over budget. Palette Open Effective Instructions
   targets the selected item, else the selected Chat row's item, else the app's selected Chat's
   item; `Overview::open_effective_instructions` is the open path Milestone 3's pre-launch check
   should call. The inheritance toggle is hidden on root items (nothing to inherit).
9. **Drift surfaces.** Full-chat header: one compact line with Compare / Apply. Outline Chat rows:
   an "Instructions changed" chip (warning token and icon) that opens Compare, where Apply lives;
   two inline buttons would not fit the compact row. List rows, Canvas cards, and the Overview
   chat-panel header carry the same warning-triangle mark (`chips::instruction_drift_mark`; label
   only in the panel header) on their title line, so it never feeds `tile_size`; clicking opens
   Compare. `AppState` watches the selected Chat's Space in addition to the Spaces the Overview
   declares (union), so the header shows drift from app start, before the Overview exists.
10. **Create Instruction from Chat.** The palette command acts in place (Shell modal) when a Chat
    is active; otherwise it falls back to the Outline with a notice. The excerpt is the global
    markdown selection when its anchor key (`{rowId}:…` or `{rowId}-code…`) belongs to a row of
    the active transcript (message id = the row's message entry id); else the last assistant
    message's copy text (message-level, tools excluded). Targets: this item, the live parent, the
    Space root; the dialog shows the target path and the reached descendant count. The excerpt is
    truncated to 1,500 characters before hashing (`contentHash = sha256(stored excerpt)`), the
    editable text to 6,000. No drift confirmation for creation (see item 4).
11. **Generic diff.** `work_outline/revision_diff.rs` renders the wire `lineDiff` directly (dual
    line numbers, `diff_add`/`diff_del` tints, accent bar, code font and size, 3 lines of context
    around changes, 400-row cap) after the entry-level changed/added/removed/reordered summary. It
    does not use `transcript::diff_to_file` or `changes::diff_line_row`: the wire already carries
    the line diff and the Changes renderer stays private and checkout-bound.
12. **Palette.** Eight Work Outline entries; Open Required Actions stays out until Milestone 4.
13. **Tests.** `WorkOutlineActions::scripted` and `AppState::work_outline_actions_override`
    (test-only) answer mutations and queries synchronously and record their params.
14. **Host capability (Part A/B).** Compare and the header Apply read the Chat host's synced device
    row, the local engine included (its `EngineInfo` predates the harness restamp):
    `work-outline-instruction-envelope-v1`, plus `work-outline-instruction-envelope-v1/<harness>`
    when the Chat's config names its harness (no config: the base token decides). Missing, Apply is
    inert and the surface says why: unsynced host ("Open Zeron on …"), outdated Zeron ("Update
    Zeron on …"), or the agent CLI ("Update codex on …"). `apply_chat_instructions` refuses too.
    The engine's own ingestion check stays authoritative.
15. **Context reset (Part B).** The composer keeps sending plain `Run` (the engine pins the
    selection at ingestion). A refused send arrives as `WatchQueue` `rejectedSends`
    (`AppState::rejected_sends`); its echo and Working overlay drop. For
    `instructionContextResetRequired` the composer shows "Start a fresh session with the new
    instructions? The agent loses this conversation's context." Start fresh session resends
    `runWithInstructions { request, messageId (same), instructions: {revisionId, revisionHash},
    contextResetConfirmed: true }` via `QueueCommand`; Cancel restores the typed text (remembered
    per message id for recent sends, else the request prompt) into the draft. Answered refusals
    stay hidden locally until the host drops them.
16. **Queued reset gate (Part B).** A row gated with `InstructionContextResetRequired` reads "New
    instructions need a fresh session · <text>" and offers Start fresh session
    (`confirmInstructionContextReset { queuedMessageId, revisionId }` via `QueueCommand`) and Cancel
    (removes the row, its text returns to the draft; attachments stay on the host) instead of the
    usual actions.
17. **Ingestion refusal (Part B).** A `QueueCommand` / `QueueMessage` failure carrying
    `instruction-delivery/{…}` (`instructionHostUnsupported`) shows the rejection's `message`
    instead of "Send failed: …"; the failed-send path keeps the draft, attachments, and comments.

## Milestone 2 delivery discoveries

Recorded by Milestone 2 runtime instruction delivery (branch `work-outline-instruction-delivery`:
`crates/harness`, `crates/doc/src/{commands,schema,queue}.rs`, `crates/engine/src/
{instruction_delivery,sessions,doc_host,run_journal}.rs`). Each item amends or sharpens section 5.

1. **Codex replace strategy is `FreshSession`.** The Milestone 2 trials (delivery audit, "Milestone 2
   trials") passed 4 of 6 for resume-with-new-value + `thread/compact/start`, so `CompactSwap` is not
   used. A revision change on Codex rejects the Turn with a resolution that starts with
   `instructionContextResetRequired` (`zeron_proto::INSTRUCTION_CONTEXT_RESET_REQUIRED`) and writes no
   user message; the client confirms and resends with `contextResetConfirmed: true` on the
   `runWithInstructions` payload, and the engine starts a fresh thread. Codex support is gated on
   `codex --version` >= 0.154.0; Claude on `claude --help` listing `--system-prompt-snapshot` and
   the `--append-system-prompt[-file]` form (2.1.285 lists the file flag only in prose).
2. **Command kind.** `SessionCommandPayload::RunWithInstructions { request, messageId,
   instructions: {revisionId, revisionHash}, contextResetConfirmed? }`, kind
   `runWithInstructions`. The sender never supplies an envelope (`request.instructions` is ignored);
   the host resolves, verifies (`verify_revision_body`, pinned hash, `envelope_sha256`), and
   re-renders. Hosts that predate it skip the entry (tested with an older-shaped decoder).
3. **Ledger idempotency (audit 9 item 1) shipped.** `queue_command` is a no-op returning `false`
   for a duplicate id; `read_commands` collapses duplicate ids (applied > other terminal >
   pending, at the first position); `set_command_status` writes every pending duplicate and never
   overwrites a terminal one while a pending duplicate exists.
4. **Queued rows carry the pin as a row-map key**, not a `QueuedMessage` field (`instructions` on
   the Loro row map; `push_queued_with_instructions`, `queued_instructions`,
   `set_queued_instructions`). This keeps the struct literal stable for existing callers; older
   readers ignore the key. The engine stamps the Chat's selected revision at `queue_message` time;
   a row without a pin runs under its Execution Session's revision.
5. **Missing revisions** wait with backoff (default 120 s, measured from the first deferral, not
   `issuedAt`, so a command that sat offline is not rejected the moment its host returns), before the
   processed mark, then reject with an actionable message. Relay ingestion refuses retryably while
   the revision is missing. Integrity failures reject at once. Neither ever substitutes a revision.
6. **Transition rule.** No native session to resume: `fresh`. A resumable native session whose
   journal sidecar says it received the same pin: `continue`. Anything else (different pin, or a
   session with no or unknown instructions): `replace { previousRevisionId }`, where an empty
   `previousRevisionId` means the session had no (known) Work Outline instructions.
7. **Turn boundary.** A `runWithInstructions` for a different revision than the working Execution
   Session's is held in the queue (pinned) and resolves `applied` with "held until the turn ends";
   the running Turn is never interrupted or steered. A queued row with a different pin cannot be
   "steered now". An explicit "send now" (interrupt) still works: the user ended the Turn.
8. **Follow-ups.** `SessionsEngine::last_request` strips `instructions`; every engine-built
   dispatch without explicit instructions takes, in order: the Turn's pin from the journal sidecar
   (by user message id), the live Execution Session's pin, the pin the chat's last native session
   received. Never the selected or newest revision. A plain `Run` from an older client, for a chat
   with a selected revision and no session record, gets the selection (host-resolved).
9. **Sidecar.** `<journals>/<chat>.instructions.json` holds `{nativeSessionId, cwd, applied,
   turns[{messageId, pin}]}` (last 256 turns), fsynced. Turn pins are written before spawn;
   `applied` on `SessionStarted`. Chats that never had instructions get no sidecar.
10. **Execution Session id** in `ExecutionSessionReport.id` is the engine run id (one harness process
    = one Execution Session); `nativeResumeId` is the harness session id. Reported on every
    `SessionStarted` of a chat that has instructions, a sidecar, or a selected revision.
11. **Capabilities.** `work-outline-instruction-envelope-v1` is now in `CURRENT`; per-harness tokens
    come from `capabilities::current_with_instruction_harnesses` via
    `instruction_delivery::advertised_capabilities` (probes run off the async runtime) and are
    stamped by `restamp_device_capabilities` after boot. Senders check a remote host's stamped
    tokens (unknown device = unsupported) and the local harness's support before queueing.
12. **Claude envelope files** are content-addressed (`<store_root>/instructions/<envelopeSha256>.md`,
    0600, atomic write, rewritten if not byte-identical) rather than one file per session: identical
    bytes, no unbounded growth. The digest is re-checked before writing.

## Milestone 3 execution discoveries

Recorded by the Milestone 3 execution-configuration backend (`crates/proto/src/
work_outline_execution.rs`, `crates/doc/src/registry/work_outline/execution.rs`,
`crates/engine/src/execution_scope.rs`, edits to `work_outline.rs`, `workspace_host.rs`,
`rpc.rs`, `diff_sync.rs`, `crates/rpc/src/lib.rs`). Each item is a reading of an ambiguous rule
or a gap stage 2 (start workflow, dispatch hookups) must close.

1. **Wire surface.** `configureExecution {scope, configuration | null, operationId}` is a
   `WorkOutlineMutate` op (kill switch, replay ledger, shape-only checks; reply `{}`);
   `resolveExecutionConfiguration {scope}` is a `WorkOutlineQuery` (pure, local). Validation is
   NOT a query: it needs the owner's disk, so it is its own forwardable method
   `ValidateExecutionConfiguration {scope | chatId | resolved, hostCapabilities}` replying
   `{ok, validated?, errors: [{error: {code, …}, message, remediation}], resolved?}`
   (validation failures are a normal reply, not an RPC error). A second forwardable method,
   `DescribeExecutionRepository {path}`, returns the identity a binding must record (topology repo
   id, root-commit fingerprint, main path, checkouts with ids): only the owning device can compute
   fingerprints and checkout ids, so a configuring client needs it. `ExecutionScope` is tagged
   `{"type":"space","spaceId"}` / `{"type":"workItem","workItemId"}`; `sourceScope` is
   `workItem | spaceDefault | implicitSpaceRoot` (lenient). Capability
   `work-outline-multi-repo-v1` is in `CURRENT`.
2. **Error vocabulary.** The requested variants plus `hostUnsupported` (owner lacks the
   capability). Every variant has `message()` and `remediation()`; none names an alternative
   checkout. There are no warnings: an `expectedRef` mismatch is the blocking `branchMismatch`
   (deviation from the audit's non-blocking `BranchDrift`), detached HEAD reads `HEAD (<sha12>)`,
   and an `expectedRef` that looks like a sha (7-64 hex) matches a HEAD prefix. Dispatch
   rejections render as `execution-scope:<code>: <message>. <remediation> (+N more)`
   (`execution_scope::rejection_message`).
3. **Rows.** `executionConfigurations` (`s:{spaceId}` / `w:{workItemId}`): `spaceId`,
   `workItemId` (immutable, upsert only), `configuration` (one field, ≤ 8,000 UTF-16 units,
   ≤ 16 bindings), `updatedBy`, `updatedAt`, `v`. Compare-before-write; reset tombstones; a later
   configure of a reset scope revives the row (explicit user write, the scope id is the only
   key; R1 exception like assignment revival). Configuring an archived item is `archived`;
   resetting it is allowed. `chatExecutionBindings` (id = Chat id): `chatId`, `spaceId`,
   `resolved` (the whole `ResolvedExecutionConfiguration`), `capturedAt`, `capturedBy`,
   `operationId`, `inputsHash`, `v`; written once by
   `WorkOutlineService::capture_chat_execution_binding` /
   `RegistryDoc::capture_chat_execution_binding`. The row itself is the replay record (no
   ledger key): same operation id and snapshot is a no-op on any device; another operation id,
   or the same id with another snapshot, is `operationConflict`; a tombstoned binding is never
   revived (`notFound`); projectless Chats cannot be bound. Both kinds are in
   `WORK_OUTLINE_KINDS`, so the Space cascade and the orphan sweep cover them.
4. **Resolution.** Whole-row, pure, in proto (`resolve_execution_configuration`) over
   materialized views (repaired parents), so the client resolves from the snapshot with
   `WorkOutlineSnapshot::resolve_execution`; the snapshot carries `executionSpace {spaceId,
   path, deviceId}` for the implicit root. Fail closed: a nearer row that does not decode or
   fails the shape check still wins and carries `invalidReason` (validation reports
   `configInvalid` / `multiplePrimary`); it never falls through to an ancestor. Two primaries are
   rejected at write time too.
5. **Exception marker.** Not a `WorkItemView` field (the UI builds `WorkItemView` literals
   without a default): the snapshot lists `executionExceptions [{workItemId, chatIds}]`, and each
   `chatExecutionBindings` entry carries `workItemId` (current association) and `exception`.
   Rule: an associated Chat whose captured configuration differs in CONTENT from the item's
   current resolution (a different source with equal content is not an exception); a current
   resolution that is invalid also marks it. Unassociated Chats and Chats without a binding are
   never exceptions.
6. **Repository identity.** `repoId` = topology `stable_id("repo", device, canonical common
   dir)`. `repoFingerprint` = the smallest root commit of HEAD's history at describe time;
   validation accepts when ANY current root commit equals it (a later merge of an unrelated
   history keeps it valid); an empty fingerprint means "no commits when configured" and requires
   that still. Root commits are cached per `(common dir, HEAD)`, never per path alone (a re-clone
   has another HEAD). A binding path must be the checkout's top level: an uninitialized submodule
   directory resolves to its superproject and reports `repositoryMissing`.
7. **Worktrees.** A binding pins a checkout with both `worktreeId` and `worktreePath`, or uses
   the checkout at `repoPath` (stage 2 offers the worktree choice; `worktreeConfigured` in the
   reply). Main checkouts are recognized by `git-dir == git-common-dir`; a submodule's main path
   comes from the module's `core.worktree`, never from the first `git worktree list` stanza
   (tested). Missing path + prunable stanza = `worktreePruned`; identity found at another
   registered path (`git worktree move` keeps the id) = `worktreeMoved`; otherwise
   `worktreeMissing`; something else at the path is never adopted. Residual risk (audit B.7,
   tested): a worktree removed and re-added at the same path with the same basename has the same
   checkout id and validates unless `expectedRef` pins its branch.
8. **Validation scope.** Device and Space failures return before any path is probed (the paths
   belong to another device). Relative or empty roots are never probed. A root that is not a Git
   checkout is NOT rejected (contract 6.4.4 keeps non-Git roots out of the MVP; stage 2's
   configuration UI should not offer them). `memberOutsideRoot` applies to every binding's
   checkout and is lifted only by `hostCapabilities.additionalDirectories`, which no caller sets
   yet (no harness passes extra directories), so it is always blocking today.
9. **Remote owners.** A validation for another device's Space forwards the RESOLVED snapshot
   (`{resolved, targetDeviceId}`), so the owner never re-reads a lagging replica. Unknown owner
   or a failed forward = `deviceOffline`; owner without the capability = `hostUnsupported`. Peer
   liveness is not consulted (the forward attempt decides). `configureExecution` on a remote
   Space is shape-only and works offline.
10. **Per-member diff data.** Diff-sync reconcile adds one checkout entry per member or primary
    binding of each bound Chat on this device (pinned worktree, else `repoPath`), resolved
    through the same identity memo; member entries carry no Chats, so the Chat row's
    `checkoutId` and the edge sidecar stay keyed to the execution root. A missing member gets no
    entry (validation reports it). The per-member `cwd` / `checkoutId` / `branch` / `headSha`
    come from the validation reply's `repositories`; the UI then uses the existing scoped
    `WatchCheckoutDiffs {cwd, checkoutId}` per member. Not done: a Changes-pane repository
    selector, per-member edge sidecars, PR lookup from the primary member's path.
11. **Stage 2 hookup sites** (function names stable; line numbers are this branch's
    `crates/engine/src/doc_host.rs` / `sessions.rs`). Call
    `ExecutionScopeService::validate_for_dispatch(chat_id, caps)` (`Ok(None)` = unbound, keep
    today's behavior; `Ok(Some(v))` = set `request.cwd = v.execution_root`; `Err` = reject with
    `rejection_message`):
    - `DocHost::execute`, `SessionCommandPayload::Run` arm (~4283): after
      `resolve_request_attachments` (~4300) and BEFORE `request.worktree.take()` /
      `materialize_worktree` (~4308-4311) and `claim_chat` (~4320); a bound Chat must reject a
      request that also carries `worktree`.
    - `DocHost::dispatch_with_source_context` (~4613), the chokepoint for queue promotion
      (~3287), Run (~4378), steer fallback (~4467), input fallback (~4532), crash auto-resume
      (`sessions.rs` ~867) and orphaned steer (`sessions.rs` ~2772): validate first and
      overwrite `request.cwd` (collapses fallback F9).
    - `SessionsEngine::resume_for` (`sessions.rs` ~1246): for bound Chats a stored
      `harness_session_cwd` that differs from the validated root (or is empty) must be an error,
      not a fresh session (F6).
    - `DocHost::request_from_chat_row` (~4752): return no request instead of an empty cwd for
      bound Chats (F4); `WorkspaceHost::claim_chat` (`workspace_host.rs` ~964) must never mint
      a Space for a bound request (F7); `MutateParams::SetChatCwd` (`rpc.rs` ~1397) and MCP
      `create_chat`/`send` (`crates/mcp/src/tools.rs`) must not retarget bound Chats.
    - The start workflow captures the binding (`capture_chat_execution_binding`) before
      queueing the first Turn and blocks a fresh launch on `deviceOffline` (decision 6.4.1).
12. **Not done here.** `createChatForWorkItem`, every dispatch hookup above, a
    `RunRequest.execution` snapshot field (`agent.rs` belongs to delivery), the explicit "adopt
    the item's newer configuration" flow, the idempotent bound worktree creation (decision
    6.4.2), UI, local-to-synced import of the new kinds, and edge `validateOp` backstops.
13. **Pre-existing failure.** `cargo check --workspace --all-targets` fails in the `zeron-ui`
    lib tests on this base (`ChatInstructionState` literals without `selected_revision_integrity`
    at `crates/ui/src/work_outline/instructions.rs:539,548`); unrelated to this work. Every other
    target checks.

## Milestone 3 launch discoveries

Recorded by Milestone 3 stage 2, engine (`crates/proto/src/work_outline_launch.rs`,
`crates/engine/src/work_outline_launch.rs`, edits to `doc_host.rs`, `sessions.rs`,
`workspace_host.rs`, `rpc.rs`, `titles.rs`, `chat_workspace_plan.rs`, `execution_scope.rs`,
`crates/harness/src/mock.rs`). Each item is a reading of an ambiguous rule or a gap.

1. **Dedicated methods, not `WorkOutlineMutate` ops.** `CreateChatForWorkItem` (forwardable,
   120 s forward deadline) and `ContinueChatForWorkItem` (local). The launch must run on the
   Space's owner: the receiving engine forwards it there itself (owner unknown or forward failure
   = `execution-scope:deviceOffline`, owner without `work-outline-launch-v1` =
   `hostUnsupported`; a typed `work-outline/…` failure from the owner passes through) and never
   launches locally for another device's Space. Continue queues durably from any device; it
   validates up front only when this device hosts the Chat (decision 6.4.1), else the host
   revalidates at drain. New capability `work-outline-launch-v1` is in `CURRENT`.
2. **Wire shapes.** Params `{workItemId, initialPrompt: {text, attachments?}, config?: ChatConfig,
   executionOverride?: {configuration}, worktreeChoice?: {"type":"useExisting", worktreeId,
   worktreePath} | {"type":"createNew", name}, operationId, targetDeviceId?}`; reply `{chatId,
   messageId, commandId, revisionId?, applicationId?, executionRoot, state: queued | started |
   rejected, detail?}` (`revisionId` / `applicationId` absent for an instruction-free launch,
   item 4). Continue `{chatId, prompt: {text, attachments?}, operationId}` → `{chatId,
   messageId, commandId, state, detail?}`. Failures are `work-outline/<code>: …`; validation
   failures are `invalid` whose message is the `execution-scope:<code>: …` rejection text,
   harness capability failures are `capability`. `config` absent = the host's default harness.
   Attachment refs ride the prompt text with the composer's trailer (`queued_message_prompt`)
   and `request.attachments`.
3. **State machine as implemented.** Inputs (everything but `operationId` and
   `targetDeviceId`) are claimed in the device-local ledger BEFORE any write
   (`work-outline:launch-in:{inputsHash}:{op}` then `work-outline:launch:{op}`, immediate SQLite
   inserts), so a replay with other inputs is `operationConflict` even after a pre-write failure.
   Then: resolve (the captured binding when one exists, else override / item resolution plus the
   worktree choice), validate on the owner, check `instruction_support` of the harness, create
   the Chat (`cwd` = validated root; an existing row must match Space and root), assign (only when
   no assignment row exists; another item or an explicit removal is `operationConflict`, never
   stomped), capture the binding, `createInstructionRevision` and `applyInstructionRevision`
   (reason `launch`, turn boundary = derived message id) with the launch `operationId`, then
   queue `runWithInstructions` with the derived command id (`DocHost::queue_command_with_id`,
   a no-op for an existing id). Registry sub-steps use no ledger keys; they are replay-safe in
   the doc layer. A Turn whose latest attempt was rejected or expired before its message landed
   is re-issued through `retry_delivery`. `state` reads the latest attempt for the derived
   message id. Crash points are injectable (`WorkItemLaunchService::inject_fault`), and the test
   retries after each of the seven steps.
4. **Instruction-free launches.** When the item has no effective instructions, the launch writes
   no revision or application and queues a plain `Run` (no selection exists, so ingestion pins
   nothing, and later Turns stay plain), so harnesses without instruction support (Cursor, ACP)
   launch from work items. Instructions added later reach the Chat through drift + Apply, which
   rejects on unsupported harnesses as before. The choice is stable across retries: an existing
   Turn of the operation (plain or `runWithInstructions`) or an existing launch revision fixes
   it; otherwise the current effective instructions decide. With effective instructions the
   harness capability is checked FIRST, before the ledger claim and any record or worktree, so
   a `capability` refusal writes nothing (the same `operationId` may be retried with other
   inputs).
5. **Worktree choice.** Applies to the primary binding, else the only binding, else the
   repository at the execution root (a binding is synthesized from `DescribeExecutionRepository`,
   `member` and `primary` true). Several bindings without a primary, or a binding that already
   pins a worktree, is `invalid`. When the root is that repository's checkout the chosen worktree
   becomes the execution root. `createNew {name}` uses `chat_worktree_target` +
   `create_chat_worktree` (`.worktrees/<label>/<slug>`, branch = slug, no `zeron/` prefix); the
   target path is claimed in the ledger (`work-outline:launch-wt:…`) before `git worktree add`, a
   retry reuses the path only when this operation claimed it and it belongs to the same
   repository, anything else fails the launch. Exception rule (amends M3 item 5): a captured
   binding equal to the item's resolution except for exactly that fill (one binding unpinned in
   the item gains `worktreeId` + `worktreePath`, root unchanged or moved from that repository's
   checkout to the worktree; or, for an item without bindings, the synthesized root binding) is
   NOT an exception (`zeron_proto::is_launch_worktree_fill`, used by the materializer). Any other
   content difference is, so an `executionOverride` that changes content is an exception (an
   override identical to the item's configuration is not distinguishable and is not). Paths
   compare as strings, so a symlinked Space path canonicalized by the launch still reads as an
   exception.
6. **Dispatch hookups (item 11).** `DocHost::validate_bound_dispatch` (lazy
   `ExecutionScopeService` from workspace + repos; `set_execution_scope` injects one) runs in
   `execute_run` right after attachment resolution, before instruction resolution, worktree
   materialization, `claim_chat`, and the user message (rejections write nothing), and at
   `dispatch_with_source_context`, which overwrites `request.cwd` with the validated root;
   `execute_run` then calls the unchecked dispatch to avoid validating twice. A bound request
   carrying a `WorktreeSpec` is `boundChatRetarget`. `request_from_chat_row` uses the bound root
   and returns `None` for an empty one. `SessionsEngine::checked_resume_for` replaces
   `resume_for` in `plan_instructions` and `dispatch_inner`: for bound Chats a stored native
   session whose cwd differs (canonical comparison) or is empty is `resumeRootMismatch`; no
   stored session, or the empty-id tombstone, starts fresh. `claim_chat` refuses a bound Chat
   without a live row. `WorkspaceHost::set_chat_cwd` / `set_chat_target` refuse any cwd other
   than the bound root (covers `SetChatCwd`, the worktree-spec stamp, and any internal caller);
   `CreateChatWorktree` refuses a bound Chat before creating anything. Auto-titling skips the
   `zeron/*` branch rename for bound Chats (the title itself is still set). The MCP tools need no
   change: `create_chat` cannot retarget an existing Chat (`createChat` early-returns) and a
   `send` Run's cwd is overwritten at the chokepoint.
7. **New validation errors** (additive, lenient decode): `resumeRootMismatch {expectedPath,
   foundPath}` and `boundChatRetarget {chatId, executionRoot, requestedPath}`. When a chosen
   worktree IS the execution root and is pruned, moved, or missing, validation reports the
   worktree error instead of the generic `rootMissing`.
8. **Child Chats.** `Mutate createChat` with `parentChatId` in the parent's Space: the child's
   cwd defaults to the parent's bound root; after creation the engine assigns the child to the
   parent's item (if the child has no assignment), captures the parent's resolved binding (only
   when the child's cwd is that root; an explicit other cwd opts out of the binding), and applies
   the parent's selected revision with reason `forkInherit`. Ids derive from
   `derive_id(childId, "fork-inherit")`, so a retried create is a no-op. Failures are logged, not
   returned (the Chat exists; an error would make an agent retry into a duplicate). Harness-native
   subagents never create Chat rows (tested with the mock's new `[subagent]` directive).
9. **Not done.** UI for both methods, expected-hash parameters (`expectedInstructionsHash` /
   `expectedExecutionConfigHash`; the launch snapshots the item's instructions at revision time),
   additional-directory harness support, the explicit "start a new Execution Session" remedy for
   `resumeRootMismatch` (today: start a new Chat), and per-Run agent authorization (Milestone 4). The device-local launch ledger is never pruned
   (same as M1).

## Milestone 3 UI discoveries

Recorded by the Milestone 3 user interface (`crates/ui/src/work_outline/{execution,
details_execution}.rs`, edits to `details_panel.rs`, `actions.rs`, `model.rs`, `outline_view.rs`,
`filter.rs`, `chips.rs`, `overview.rs`, `changes.rs`, `pickers.rs`, `state.rs`,
`shell/spaces.rs`). Each item is a reading of an ambiguous rule or a gap later work must close.

1. **Local resolution, remote facts.** The Execution section resolves from the snapshot
   (`WorkOutlineSnapshot::resolve_execution`); the source label reads "This item", "Inherited from
   <title>", "Space default", or "Implicit Space folder". `DescribeExecutionRepository` carries
   `targetDeviceId` when the Space's device is not this app's engine; `ValidateExecutionConfiguration`
   never does (the engine forwards to the owner itself). A dirty draft validates as `{resolved}` with
   the Space's device and the scope as source; otherwise `{scope}`.
2. **Edit form.** Opening Edit prefills the current resolution (an inherited configuration becomes
   this scope's own row on Save) and describes every bound repository to offer its non-prunable,
   resolvable checkouts as the worktree choice ("Repository path" = no pinned worktree; a pinned
   worktree the description does not list stays shown as pinned). A repository added from a linked
   worktree binds the main checkout as `repoPath` and pins the picked worktree. Primary is a radio
   that can be cleared (a configuration may have no primary). Persistence follows the instruction
   policy (Save only; Cancel discards; blur never saves; closing, switching, Escape with a dirty
   draft asks "Keep Editing / Discard"); unchanged content writes nothing; the local
   `check_execution_configuration` runs before any write. Edit is hidden on archived items; Reset
   to inherited appears only when the scope has its own row.
3. **Folder picker.** The Shell's New-project browser has a pick mode (`Shell::open_folder_pick`,
   `ExecutionFolderPick`): fixed to the Space's device (the Devices step is unreachable), opened at
   the current root, its submit hands the open folder to the caller instead of creating a Space.
   The root may be any folder (the workspace use case); repositories must be Git: a folder whose
   describe fails or returns no `repoId` is refused and nothing is added. Deviation: discovery 8
   suggested not offering non-Git roots; the orchestrator scoped that rule to repositories.
4. **Drift confirmation.** Uses the generic Shell confirm modal, not the instruction drift dialog
   (its body is instruction-specific). It appears iff saving or resetting changes the resolution of
   a live item that has an associated bound Chat that is not already an exception and whose captured
   configuration would differ; the body states those Chats keep their captured configuration, are
   never retargeted, and will show as exceptions. Unbound Chats never count.
5. **Validation display.** Headline Ready / "<device> is offline — nothing was checked" /
   "<device> runs a Zeron that can't validate execution" / "Blocked · N problems", then every
   error's host-rendered `message` and `remediation` (local vocabulary only when a field is empty).
   A failed call (transport, older host) reads "Execution configuration isn't available on this
   host yet" for `unsupported`.
6. **Exceptions.** Outline item rows, the Execution header, List rows, and Canvas cards carry a
   compact folder mark (accent token; List/Canvas without label on the title line, so `tile_size`
   is unchanged). Details list each exception Chat with "uses a different configuration" and its
   captured root and repository names. `WorkItemIndex` indexes `binding.exception`; the work-item
   filter needs no change for bound Chats (tested).
7. **Per-member branch/diff.** `AppState` keeps one `ChatExecutionState` per bound Chat (the latest
   `ValidateExecutionConfiguration {chatId}` reply, keyed by the binding's `operationId|capturedAt`,
   plus the picked repository). Opening a Changes pane refetches; the composer footer and later
   pane syncs fetch only when missing. Members are primary + `member` bindings, primary first; the
   default is the primary, else the first. The Changes pane shows one pill per member
   (`name · branch` or `HEAD (sha7)`) and retargets its owner to the member's `checkoutPath` /
   `checkoutId` / `branch`, so the existing scoped watch and branch/turn scopes follow it. Before
   a reply arrives (or when validation fails) members come from the captured binding (path only,
   no checkout id) and the first validation error shows under the pills. The composer footer's
   branch label for bound Chats names the same member and cycles on click (tooltip lists all,
   following the worktree-chip click-to-cycle pattern rather than a new popover kind); the footer
   also shows for bound Chats whose Space root is not Git. PR lookup is unchanged.
8. **Not done.** No refresh button on the member pills (reopen the pane); the exception marker is
   not in the sidebar; the root input accepts typed paths without a device-side existence check
   until Validate.
