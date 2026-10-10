# M0 audit: registry persistence for the Work Outline

Scope: how the profile-scoped workspace registry actually works, and the row schemas, materialization, ordering and write-path rules the Work Outline needs on top of it. Branch `work-outline-implementation`. All citations are `path:line` against this branch (paths relative to `zeron/`). Status of every claim is verified in code unless marked UNVERIFIED.

## 0. Verdict and spec conflicts (read first)

The registry is **not a Loro doc**. It is a keyed row table with **per-field** HLC last-writer-wins, a server-authoritative Durable Object, and a client replica with an optimistic pending-op overlay. The Loro `WorkspaceDoc` (`crates/doc/src/workspace.rs`) survives only as a one-time migration source (`workspace_host.rs:217-258`). The spec's "additive row kinds in the existing HLC/LWW registry" is feasible, and the edge DO is kind-agnostic. The conflicts below must be resolved in the spec before M1.

| # | Spec text | Reality | Resolution |
|---|---|---|---|
| C1 | "`(updated_hlc, work_item_id)` of the source row" (cycle repair, spec L428) | No row-level `updated_hlc` exists. Each field has its own clock (`RegistryRow.clocks`, `registry.rs:107-108`). Typed reads discard clocks (`read_kind`, `registry.rs:698-710`). | Use the clock of the `parentWorkItemId` field (the edge's own write clock). A title edit must not change which edge is cut. Needs a new clock-exposing read (section 8). |
| C2 | "immutable `space_id`" | LWW cannot enforce immutability: any later upsert with a newer clock overwrites a field (`apply_op`, `registry.rs:240-254`; TS `registry-core.ts:150-161`). The edge validator is stateless (`registry-core.ts:64`). | Immutability is a writer contract plus a reader contract (rows whose `spaceId` disagrees with their parent, chat or assignment are quarantined). Mutations use `update` ops, which never carry immutable fields. |
| C3 | "tombstone reserved for permanent deletion", "tombstones converge" | A tombstone clears all fields and clocks (`registry.rs:192-197`) and is purged after 30 days (`registry-room.ts:28`, `:445-463`). A device offline longer than that re-seeds its local rows on full resync (`registry.rs:487-512`), resurrecting deleted rows, including a deleted Space. | Accept and document. Space-tombstone suppression is "Space row absent or tombstoned" (not "tombstone present"), so a resurrected Space brings its outline back consistently. |
| C4 | Row = bag of fields; each revision row "contains the exact ordered snapshot and the canonically rendered envelope" | One op is capped at 16 KiB serialized (`registry-core.ts:60,85`). A rejected batch is never retired by the client and wedges all later writes (`registry.rs:601-608`; `sync/registry.rs:993-997` only logs `Error` frames, which carry no batch id). | Hard size budgets enforced client-side before enqueue (section 6). Revision row stores entries and an envelope hash, not a second copy of the rendered text (section 7.8). |
| C5 | `WatchWorkOutline` "one composed snapshot" | All existing watches are global `watch::Sender<Vec<_>>` lists fed by `publish_lists` (`workspace_host.rs:1416-1456`). No per-parameter composed watch exists on the registry. | Follow the `WATCH_PREVIEWS` unfold pattern, driven by the registry change counter (section 4). |
| C6 | "Profiles remain isolated" | True for store paths. But the local-to-synced import copies Space rows verbatim by id (`workspace_host.rs:1115`, `local_import.rs:331`) and knows nothing about outline kinds. | M1 must extend `local_import` to copy outline rows (or explicitly declare them non-migrating). New Space ids are not minted on import, so the same ids would collide if the user later re-adds the folder. |
| C7 | Spec L89 "Older clients ignore unknown row kinds without damaging them" | Proven for kinds (section 2.3). NOT true for strict field allowlists at the edge: `sidebarPins` rejects unknown fields (`registry-core.ts:86-93`), and a newer client's extra field then wedges the batch. | New kinds must NOT use a strict field allowlist at the edge; validate sizes and formats of known fields only. Deploy the edge before any client that writes the kinds. |
| C8 | Spec data model lists flat fields for multi-field invariants (placement, archive, resolution, execution config) | Per-field LWW tears across concurrent writers (parent from device A, order key from device B). | Group atomic invariants into one JSON field or write the group in a single op with one HLC (section 7). Precedent: `chats.config` is one JSON field (`registry.rs:895-915,1151-1167`). |
| C9 | "Revision application: HLC timestamp" | Rows expose no creating-op HLC. | Write the op's own HLC into a field (`appliedHlc`), as `pin_order_key_between` and sidebar sections already do (`sidebar_pins.rs:107-114`, `sidebar_sections.rs:157-170`). |
| C10 | ARCHITECTURE.md:107 lists "checkout-diff summary pointers" as registry content | Not a registry kind. Diffs ride the per-chat session DO sidecar (`diff_sync.rs:11,81-82`). Only 6 base kinds plus 3 sidebar kinds exist. | Doc drift; no action for outline. |

## 1. Storage model (answer to Q1)

**Shape.** One row table per user, `(kind, id) -> {seq, deleted, delHlc, fields, clocks}`.
- Edge DO: SQLite `rows (kind TEXT, id TEXT, seq INTEGER, deleted INTEGER, del_hlc TEXT, fields TEXT, clocks TEXT, PRIMARY KEY (kind,id))` and a `meta` table (`registry-room.ts:59-65`). Cold start is a table read; no history, no wasm (`docs/registry-sync.md:5-25`).
- Client replica `RegistryDoc` (`crates/doc/src/registry.rs:350-361`): `authoritative: HashMap<kind, HashMap<id, RegistryRow>>` (server truth, replaced by `state`/`rows` frames, never re-merged client-side), `pending: Vec<PendingBatch>` (unacked local writes), `clock: HlcClock`, `generation: u64`.
- Whole replica serialized as one JSON blob (`to_bytes`, `registry.rs:398-415`) into `DocsStore` snapshot row `registry1` (`REGISTRY_DOC_ID`, `registry.rs:46`; `workspace_host.rs:1594-1608`) on a 1 s debounce (`workspace_host.rs:76,1747-1775`). Unknown kinds are persisted (the `rows` list is a `flat_map` over every kind).

**HLC.** `{ms:013}-{counter:06}-{device}` (`registry.rs:53-55`, TS `registry-core.ts:18-19`); lexicographic string order is total because of the device suffix. Compare is strict `>` (`hlc_newer`, `registry.rs:58-63`; `hlcNewer`, `registry-core.ts:22`), so replays are no-ops. Generated only client-side by `HlcClock::next` (`registry.rs:74-86`, wall clock from `Utc::now()` at `:588-595`); the server never mints clocks. **`HlcClock` does not merge observed remote clocks.** A device with a slow wall clock loses LWW to a causally earlier write from a fast clock. Sidebar code works around this with `observe_sidebar_row` (`sidebar_sections.rs:10-24`, called before every pin/section write, `sidebar_pins.rs:61-76`). The outline must do the same (section 10, rule R3).

**Merge (`apply_op`, `registry.rs:177-261`; mirrored 1:1 by `registry-core.ts:114-164`; "shared test vectors, change together", `registry.rs:12-13`).**
- `upsert`: creates; revives a tombstone iff `op.hlc > row.delHlc` (the tombstone loses wholesale; only the upsert's fields survive: `registry.rs:220-235`). Each field in `set` applies iff `clock(field) > stored clock(field)`; `Null` deletes the field but still records its clock (`registry.rs:243-253`).
- `update`: never creates and never revives ("never invent rows", `registry.rs:207-209,221-223`).
- `delete`: tombstone-on-missing is allowed (`:181-184`). Against a live row it wins iff `op.hlc > maxClock(row)` (newest field clock or delHlc, `:125-133`), clearing fields and clocks (`:192-197`). A delete older than any field write loses and the row stays live.
- Server applies a batch atomically, assigns one `seq` per accepted batch, stamps touched rows with it, and broadcasts merged full rows to all sockets including the sender (`registry-room.ts:347-409`).

**Keying.** `(kind, id)` where `id` matches `^[A-Za-z0-9_.:@/-]{1,256}$`, kind `^[a-z][a-zA-Z0-9]{0,31}$`, field `^[a-zA-Z][a-zA-Z0-9]{0,63}$` (`registry-core.ts:55-57`). Existing ids: UUIDv4 for spaces (`ui/src/shell/spaces.rs:5549`), chat ids for chats and sessions, `sha256(chatId NUL url)` for `chatPrLinks` (`registry.rs:1572-1578`), session id for pins.

**Tombstone behavior.** Hard delete = `delete` op. Cascades are issued by the client as one logical batch: `delete_space` tombstones the space, all its chat and session rows, and their PR links (`registry.rs:859-890`). The cascade is computed from the local overlay at call time, so a chat created concurrently on another device survives as an orphan; `sweep_orphans` repairs those on the owning host, gated on `registry_synced()` (`spaces.rs:293-322`; `workspace_host.rs:589-596`). Batches are chunked to 400 ops, so a cascade is atomic per chunk only (`registry.rs:609-626`). Server GC of tombstones runs from a daily alarm (`registry-room.ts:445-463`).

**Soft-delete precedent.** `sidebarSections` uses a `deleted: true` field instead of a tombstone so the id is never revived (`sidebar_sections.rs:157-170`, doc `registry-sync.md:129-130`).

## 2. Row kinds and what adding one touches (answer to Q2)

### 2.1 Existing kinds

Constants at `registry.rs:32-41`: `devices`, `spaces`, `chats`, `chatPrLinks`, `sessions` (session-status, keyed by chat id), `preferences` (one readiness row), plus private `sidebarPins`, `sidebarSections`, `sidebarLocations` (`sidebar_sections.rs:6-7`). Typed row structs are private `Raw*` serde structs in `crates/doc/src/workspace.rs` (`RawSpace` :684, `RawChat` :717) read through `read_kind`/`row_to` with `warn!("skipping malformed registry row")` (`registry.rs:698-710,1610-1619`). Wire/domain structs (`Space`, `Chat`, `ChatPrLink`) live in `crates/proto/src/entities.rs` (`Space` :112-132, `ChatPrLink` :193).

### 2.2 Touch list for a new kind

1. **Rust schema/mirror** (`crates/doc`): `KIND_*` const, `Raw*` struct, typed `impl RegistryDoc` mutators that build `fields([...])` and call `write`/`enqueue_ops` (`registry.rs:629-655`). New module `crates/doc/src/registry/work_outline.rs` following `sidebar_pins.rs` (`mod` decl at `registry.rs:42-43`). `apply_op`/`PersistedState`/`to_bytes`/`from_bytes` need no change.
2. **Sync frames** (`crates/sync`): none. `RegistryClient` is kind-agnostic (grep of `crates/sync/src/registry.rs` shows no kind filtering); `mock_server.rs` mirrors the DO with no kind logic.
3. **Edge**: `registry-room.ts` needs no change. `registry-core.ts` `validateOp` is the only place that knows kinds (`:86-93` for `sidebarPins`). Add stateless size and format validators for new kinds, plus vitest vectors in `registry-core.test.ts`. Rust mirror of any validation goes in the typed mutators (the client must refuse to enqueue what the edge will reject).
4. **Engine/proto**: wire types, RPC method constants (`crates/rpc/src/lib.rs:99-110`), handler arm in `engine/src/rpc.rs`, `WorkspaceHost` accessors.
5. **Cleanup hooks**: `delete_space` (`registry.rs:859`) must also tombstone outline rows; orphan sweep gains an outline pass (section 11).
6. **Local-to-synced import**: extend `local_import.rs` (C6).
7. **Docs**: `docs/registry-sync.md` gets a "Work Outline rows" section, like the sections one (`:117-139`).

### 2.3 Do the edge or older clients reject, drop, or damage unknown kinds or fields? (proof)

- **Edge accepts any well-formed kind.** `validateOp` checks only `KIND_RE`, `ID_RE`, `FIELD_RE`, `HLC_RE`, op shape and total size (`registry-core.ts:64-85`). The only kind-specific branch is `sidebarPins` (`:86-93`). `applyPushBatch` loads/saves rows by `(kind,id)` with no kind switch (`registry-room.ts:376-387`); `rowsSince` returns all kinds (`:131-148`); the nightly backup dumps all rows (`:469-473`).
- **Older Rust clients keep unknown kinds intact.** `apply_rows`/`apply_state`/`put_authoritative` file rows by `row.kind` with no allowlist (`registry.rs:452-535,579-584`); `to_bytes` persists every kind (`:398-415`); `row_to_seed_op` re-seeds every kind with original clocks (`:265-295`); `read_all`/`read_*` only look at their own kinds (`:1410-1417`). `RegistryRow` and `ServerFrame::Rows` carry no `deny_unknown_fields` (grep: no `deny_unknown_fields` anywhere in `crates`).
- **Unknown fields on known kinds are ignored** by serde defaults (`RawSpace`/`RawChat` have no `deny_unknown_fields`). The `chats` row for outline purposes is never modified; the outline adds separate kinds.
- **Known hazard: unknown enum VALUES on known fields drop the whole row.** Field incident documented at `crates/doc/src/workspace.rs:732-737` and tested at `crates/doc/src/registry/tests.rs:468-511`. Outline `Raw*` structs must use lenient enum deserializers (pattern `lenient_chat_config`, `workspace.rs:772-797`) so a future `status` or `kind` value degrades the field, not the row.
- **What old clients do NOT do** (and the spec relies on new code for): older `delete_space` leaves outline rows live; older `delete_chat` does not touch assignments (`registry.rs:1334-1346`). Both are handled by reader-side suppression (section 11) and are consistent with spec L455.

## 3. Space, id minting, cascade, profile isolation (answer to Q3)

- **Representation.** `Space { id, device_id (immutable owner), path, name?, git_detected, git_checked_at?, checkout_id?, created_at }` (`proto/src/entities.rs:112-132`). Row kind `spaces`, fields camelCase (`registry.rs:783-796`; `RawSpace` `workspace.rs:684-698`). Rename, git stamps are `update` ops (`registry.rs:817-853`).
- **Id minting.** Client-side `uuid::Uuid::new_v4()` in the UI (`ui/src/shell/spaces.rs:5549`) sent via `Mutate{op:createSpace,spaceId,...}` (`engine/src/rpc.rs:632,1244`); `create_space` is idempotent by id and also no-ops when a live `(deviceId, path)` duplicate exists (`workspace_host.rs:990-1018`). Two devices racing can still produce two Space ids for one `(device,path)`; both survive. The outline attaches to the Space id only.
- **Cascade.** `delete_space` (`registry.rs:859-890`) described in section 1. RPC side interrupts and purges local runs for deleted chats (`rpc.rs:1259-1280`). Chats have `spaceId` (`registry.rs:930`), no move-between-Spaces mutator exists (only `set_chat_cwd`/`set_chat_target`, `:1089-1111`), so a chat's `spaceId` is effectively immutable after `claim_chat`/`upsert_chat`.
- **Profile isolation.** Local profile: `{data_dir}/profiles/local` store root, `org_id="local"`, `edge: None` so the registry never joins a room (`engine/src/profile.rs:36-45`; `workspace_host.rs:161-163,333,354`; `registry_synced()` returns true with no edge, `:589-592`). Synced/development: `{data_dir}/orgs/{org}/{user}` (`profile.rs:52-70`), `DocsStore::open(profile.store_root())` (`engine/src/lib.rs:243`), registry room `reg1/{orgId}/{userId}` derived from the authenticated caller, never from client input (`edge/src/index.ts:303-316`). Workspace scope is fixed per runtime (`proto/src/workspace.rs:29-39`). The outline needs no extra isolation work beyond C6.

## 4. Watch pattern (answer to Q4)

- Registry writes call `WorkspaceHost::mutate` (`workspace_host.rs:632-639`): lock registry, apply, `bump_changed()` (`changed_tx`, a `watch::Sender<u64>`), nudge the room client. Server frames bump the same counter. `workspace_task` (`:1747-1786`) wakes on every change, calls `publish()` -> `publish_lists` (`:1416-1456`) which reads `read_all` once (devices, spaces, chats, sessions) under the registry lock and fans out through `publish_if_changed` (`:1788-1797`) into per-kind `watch::Sender<Vec<T>>` fields (`:170-174`). Sidebar prefs use a hand-written `send_if_modified` with a `revision` and `synced` flag (`:1459-1489`, proto `SidebarPreferencesState` `entities.rs:51-66`).
- RPC: `WATCH_CHATS`/`WATCH_SPACES` are `watch_stream(workspace.watch_*())` (`rpc.rs:2211-2223`); `watch_stream` (`:1527-1542`) emits the current value first, then each change. These methods are NOT in `forwardable` (`:1425`) or `is_stream_method` (`:1512`), so they always serve the local profile replica.
- **Representative param-bearing composed stream:** `WATCH_PREVIEWS` (`rpc.rs:2148-2195`): `unfold` over a state tuple, `tokio::select!` on `changed()` of several watch receivers, recompute from `workspace.chat(..)`, `serde_json::to_value`, end on channel close.
- **Proposed `WatchWorkOutline(spaceId)`:**
  1. `WorkspaceHost::watch_changes() -> watch::Receiver<u64>` (new accessor, clone of `changed_tx.subscribe()`), and `WorkspaceHost::work_outline_snapshot(space_id) -> WorkOutlineSnapshot`.
  2. Snapshot step: under the registry lock only clone the inputs (`RegistryDoc::work_outline_inputs(space_id) -> OutlineInputs`, rows with clocks plus the Space row plus that Space's chats); materialize AFTER releasing the lock (the lock also guards sync frame application).
  3. Stream: unfold; first frame immediate; then `changed().await`, coalesce ~75 ms (burst of `rows` frames and `set_chat_seen` writes both bump the counter), recompute, `PartialEq`-dedupe against the previous snapshot (snapshot must be fully deterministic: sorted Vecs, no HashMap iteration order), emit only if changed.
  4. Payload carries `synced: bool` (same meaning as `SidebarPreferencesState.synced`: authoritative state received; absent rows are "not heard yet" otherwise) and a per-stream monotone `revision`, mirroring sidebar prefs so the UI optimistic queue can reject stale frames after a mutation response.
  5. Not forwardable (local replica, like `WATCH_CHATS`). `RegistryDoc::generation` (`registry.rs:360,386`) exists for gating if profiling shows recompute cost; per-kind dirty counters are an optimization, not a requirement (O(n) materialization per change is fine for 10^3-10^4 items).
  6. Performance note: `overlay_row` scans all pending ops per id (`registry.rs:660-677`), so `overlay_rows` is O(ids x pendingOps). With a large offline queue that is quadratic. Acceptable for MVP; mitigate by indexing pending ops by `(kind,id)` before shipping large-outline tests.

## 5. Existing ordering and idempotency utilities (answer to Q5)

**Ordering.**
- `pin_order_key_between(lower, upper, nonce)` and `valid_pin_order_key` in `crates/proto/src/sidebar_pins.rs:7-60`: lowercase-hex midpoint keys, trailing digit never `0`, each key ends with a 149-byte hex-encoded HLC nonce plus `8`, so every key is about 300 bytes and unique without an id tie-break; max 8192 bytes. Sorting is `(orderKey, id)` (`sidebar_pins.rs` registry side :15-18). Edge validates pin keys with `^[0-9a-f]*[1-9a-f]$` and `<= 8192` (`registry-core.ts:90`). Reuse is possible but wasteful (about 300 B/key x thousands of rows, rewritten into every snapshot, and the edge validator is `sidebarPins`-only).
- `loro_fractional_index` is in `Cargo.lock` only as a transitive dependency of loro; not a direct workspace dependency (`Cargo.lock:4826,4891`).
- Chat queue ordering uses a Loro MovableList (`doc/src/queue.rs:6-13`), not applicable.

**Idempotency.**
- No generic operation-id utility exists.
- Deterministic content-derived row ids: `chat_pr_link_row_id` (`registry.rs:1572-1578`). Create-if-absent: `create_space` (`workspace_host.rs:990-1004`), sidebar section `Create` (`sidebar_sections.rs:157-170`).
- Registry batch replay: `PendingBatch.batch = "b-{hlc}"`, strict-`>` merge makes re-push a no-op (`registry.rs:299-309,617-625`; `registry-room.ts` HTTP push comment :217-222).
- Device-local, unsynced "run once" ledger: `processed_commands` + `mark_processed` (claim BEFORE executing, `sync/src/store.rs:41-46,501-` ), and the chat command ledger `SessionCommandEntry` with `message_id` dedupe (`doc/src/commands.rs:1-60`). Chat outbox has `batch_id UNIQUE` (`store.rs:55-62`).
- Gap: nothing derives row ids from an operation id, and nothing journals multi-step workflow progress. Proposal in section 10 (R5).

## 6. Size and shape limits (answer to Q6)

| Limit | Value | Where | Failure mode |
|---|---|---|---|
| Serialized op | 16 KiB, measured as `JSON.stringify(op).length` (UTF-16 code units of the entire op including kind, id, hlc, clocks) | `registry-core.ts:60,85` | Whole batch rejected with an `error` frame that has no batch id; client never retires it; every later write is stuck behind it (`registry.rs:601-608`; `sync/registry.rs:993-997`). HTTP path returns 400 with `invalid_op` (`registry-room.ts:236`), also unretired. |
| Ops per batch | server 500 (`registry-room.ts:31,359`); client chunks at 400 (`registry.rs:609`) | | A client-side chunk is atomic; cross-chunk is not. |
| WS inbound frame | 1,000,000 chars | `registry-room.ts:33,267-270` | Socket closed (1009). **Client chunks by op COUNT, not bytes**: 400 ops x 16 KiB can exceed 1 MB, so a large-row batch would close the socket and replay forever. Required fix: byte-aware chunking in `enqueue_ops` (budget about 512 KB/batch). |
| HTTPS push body | 2 MiB declared content-length | `registry-room.ts:228` | 413. |
| Id | 256 chars of `[A-Za-z0-9_.:@/-]` | `registry-core.ts:55` | op invalid. |
| Kind | `^[a-z][a-zA-Z0-9]{0,31}$` (so max 32 chars); all proposed kinds fit (longest `instructionRevisionApplications` = 31) | `:56` | |
| Field name | `^[a-zA-Z][a-zA-Z0-9]{0,63}$` (no underscore, no dot) | `:57` | |
| Tombstone retention | 30 days, then purged; stale cursors get `full` | `registry-room.ts:28,445-463` | resurrection on stale re-seed (C3). |
| Pins | 200 pins, 8192-byte order keys | `proto/src/entities.rs:13` | |
| Local snapshot | whole row table + pending as one JSON blob rewritten on every debounce | `registry.rs:398-415`, `workspace_host.rs:1594` | Big text rows inflate every snapshot write; keep outline rows small. |
| DO SQLite row | platform limit of the Cloudflare SQLite DO engine (2 MB documented for SQLite-backed DOs) | UNVERIFIED from code | Far above the 16 KiB op cap. |
| Full `state` frame | no pagination, whole table for `full` hello (`registry-room.ts:312-327`) | | UNVERIFIED platform WS send limit; already multi-MB design per `docs/registry-sync.md:19-20`. |

**Client-side write validation required for every outline mutator (mechanism in the doc layer, like `change_sidebar_pin` returning `DocError::Schema`, `sidebar_pins.rs:48-58`):** reject any op whose serialized length, counted in UTF-16 units (`s.encode_utf16().count()`, to match JS `.length`), exceeds 15,000 (1,384 units of headroom). JSON escaping can double visible text, so budget on the serialized form, not on character count.

Proposed field budgets (all enforced twice: typed mutator and edge `validateOp`): title <= 256 chars; description <= 8,000; instruction text <= 6,000; instruction source excerpt <= 1,500; proposal text <= 6,000; action request text <= 1,000 plus supporting info <= 4,000; external reference title <= 256, url <= 2,048; execution configuration JSON <= 8,000 serialized, <= 16 bindings; revision: effective-context budget 9,000 chars of instruction text total and <= 32 entries (section 7.8).

## 7. Proposed row schemas

Conventions (match existing code):
- Wire field names camelCase, kinds plural camelCase (`chatPrLinks`, `sidebarPins`).
- Times are epoch-ms integers (`opt_ms`, `registry.rs:1587`). Display timestamps (`createdAt`, `updatedAt`) are informational. Conflict resolution always uses the per-field HLC clocks, never `updatedAt`.
- Nullable field is written as `Value::Null` (clocked deletion), read as `Option`.
- Enums serialize lowercase strings and deserialize leniently (unknown becomes a documented fallback plus a repair marker).
- `v: 1` additive schema field on every row; readers tolerate missing/higher.
- **Actor** (provenance) is one JSON field: `{ "k": "user"|"agent"|"system", "deviceId": "...", "chatId"?: "...", "msgId"?: "..." }`. One field, so it can never tear.
- **Writer rules (apply to all kinds).** Creation = one `upsert` with the full immutable set and initial values, only after a create-if-absent check that distinguishes `Absent` from `Tombstoned` (new `RegistryDoc::row_state`; `overlay_row` currently hides tombstones, `registry.rs:676`, so `create_space`-style checks revive tombstoned ids). Every later mutation = `update` op (never upsert), never including immutable fields; `update` cannot revive a tombstone (`registry.rs:221`). Observe the row's clock before each write (R3).

### 7.1 `workItems` — id: UUIDv4

| Field | Type | Mutability | Notes |
|---|---|---|---|
| `spaceId` | string | immutable | writer sets once; reader quarantines rows whose parent/chat disagree |
| `parentWorkItemId` | string or null | LWW, group P | null = Space-level root |
| `orderKey` | base62 string, 1..=64 | LWW, group P | section 9 |
| `title` | string <= 256 | LWW | required; missing => reader shows "Untitled" + repair marker |
| `description` | string or null <= 8000 | LWW | |
| `status` | `"pending"\|"active"\|"completed"` | LWW | lenient: unknown => `pending` |
| `inheritParentInstructions` | bool | LWW | missing => true |
| `archivedAt` / `archivedBy` / `archiveCause` | ms / Actor / `"self"` or `"cascade"` | LWW, group A | Group A is always written in one op (set all three, or null all three on restore). `archiveCause` records descendant policy for restore. |
| `createdBy` | Actor | immutable | |
| `createdAt` | ms | immutable | |
| `updatedAt` | ms | LWW | display only |
| `v` | 1 | | |

Group P (placement): every parent or order change writes BOTH `parentWorkItemId` and `orderKey` in one op (one HLC), so `clocks["parentWorkItemId"] == clocks["orderKey"]` for any row written by new clients. Concurrent "reorder within P" versus "move to Q" resolves as one whole placement, never a torn pair. Hard delete (`delete` op) is reserved for permanent deletion; the mutation API never uses it for normal removal. Cycle repair persists as an explicit `update` setting `parentWorkItemId` to null.

### 7.2 `workItemInstructions` — id: UUIDv4

`spaceId` (immutable); `workItemId` (immutable; null/absent = Space-root instruction; immutable so scope never changes under a revision), `text` (<= 6000), `orderKey` (base62, LWW), `createdBy` Actor (immutable), `createdAt` (immutable), `updatedAt`, `source`: one JSON field `{chatId?, messageId?, excerpt? (<=1500), contentHash?}` (immutable once set at creation, so a copied excerpt cannot be re-attributed), group A archive fields (`archivedAt`/`archivedBy`), `sourceProposalId` (immutable; set by approval), `v`. `contentHash` = sha256 of the exact text at creation, informational; the authoritative per-version hash lives in revisions. Text edits are LWW on `text`; revisions snapshot exact text so edits never mutate history.

### 7.3 `workItemChatAssignments` — id: `chatId` (kind namespace separates it from `chats`)

Fields: `spaceId` (immutable; must equal the chat's `spaceId`, checked at write, re-checked at read), `workItemId` (LWW; null/absent = explicit disassociation), `assignedBy` Actor, `assignedAt` ms; `workItemId`, `assignedBy`, `assignedAt` form group S written in one op. Keying by chat id gives structural "at most one primary item" and LWW concurrency resolution per chat. Create path: first assignment is an `upsert` (create-if-absent); later changes are `update`. Because the key is a chat id and chats are never re-spaced, `spaceId` cannot legitimately change. Not cascaded on `delete_chat` (spec L455: dangling assignment stays inspectable until explicitly removed; explicit removal = write null, not tombstone).

### 7.4 `executionConfigurations` — id: `s:{spaceId}` (Space default) or `w:{workItemId}` (override)

`:` is legal in ids (`registry-core.ts:55`). Id-as-scope makes "at most one live row per scope" structural. Fields: `spaceId` (immutable), `workItemId` (immutable, present iff id starts `w:`), `configuration` (ONE JSON field, one LWW register, <= 8000 serialized): `{ "executionRoot": path, "bindings": [ { "repoId", "repoPath", "member": bool, "primary": bool, "worktreeId"?, "worktreePath"?, "expectedRef"? } ] , "schema": 1 }`, `updatedBy` Actor, `updatedAt`. Whole-config atomicity prevents a torn root-from-A/bindings-from-B config (C8). Reader validity rules: at most one `primary`; violation => `ConfigInvalid` marker and launch blocked (fail closed; never choose). Tombstone only for explicit scope reset (or Space deletion).

### 7.5 `workItemExternalReferences` — id: `hex(sha256(workItemId NUL provider NUL refType NUL externalId))`

Pattern of `chat_pr_link_row_id` (`registry.rs:1572`): duplicate adds converge to one row; re-add after removal revives via a newer upsert. Fields: `spaceId`, `workItemId`, `provider`, `refType`, `externalId`, `url` (all immutable, they define the id), `cachedTitle` (LWW), `createdBy`, `createdAt`. Removal = tombstone (`delete`). Independent of `chatPrLinks`/`chats.linkedPrUrl`; never written by `set_chat_link`.

### 7.6 `userActionRequests` — id: UUIDv4 (or derived from the creating op id)

`spaceId`, `workItemId`, `chatId`, `messageRef` (optional), `text` (<= 1000), `info` (<= 4000), `dedupeKey` (optional), `createdBy`, `createdAt` (all immutable); `state` (`"open"|"resolved"`, only ever written `open` at create and `resolved` by resolve), `resolution` ONE JSON field `{by: Actor, at: ms, note?}` written in the same op as `state:"resolved"`. Reader rule: `resolution` present => resolved (monotone; no reopen). Duplicate-open collapse: among open rows with equal `(workItemId, dedupeKey)`, count the one with minimal `(createdHlc, id)` and mark the rest `Deduped`. Notification zero-to-one transition is computed per device in the engine from consecutive snapshots, not stored.

### 7.7 `workItemInstructionProposals` — id: UUIDv4

`spaceId`; `target` ONE JSON field (immutable) `{ scope: "space"|"workItem", workItemId?, instructionId?, expectedVersion? }` (modification proposals deferred per spec L487); `text` (<= 6000, immutable; reviewer's final text goes on the created instruction, not here); `proposedBy` Actor (immutable); `sourceRef` (immutable); `createdAt`; `review` ONE JSON field `{ state: "approved"|"rejected", by: Actor, at: ms, reason?, instructionId? }` or absent (= pending). Approval creates a `workItemInstructions` row with the deterministic id `derive_id(proposalId, "instruction")` and `sourceProposalId`, in the same batch. Reader precedence: if that instruction row exists, the proposal reads as approved regardless of a concurrent reject (approval has durable side effects); else `review.state`.

### 7.8 `instructionRevisions` — id: UUIDv4 (or `derive_id(operationId,"revision")` so retries re-hit the same row)

Immutable; written once by a single `upsert`. Fields: `spaceId`, `workItemId`, `createdBy`, `createdAt`, and ONE JSON field `body` (single LWW register so two racing writers of the same id cannot interleave fields): `{ "entries": [ { "instructionId", "sourceKind": "spaceRoot"|"workItem", "scopeId", "text", "order": int, "provenance": {..} } ], "envelopeVersion": int, "envelopeSha256": hex, "hash": hex }`, where `hash = sha256(canonical_json(entries, envelopeVersion))` and the envelope is the pure, versioned, golden-tested rendering `render_envelope(version, entries)`. **Deliberate deviation from spec L496 ("canonically rendered envelope"):** the rendered text is NOT stored (it would double the row under the 16 KiB cap). Delivery re-renders from the entries and verifies `envelopeSha256`; a mismatch is a hard `RevisionIntegrity` error (fail closed, never deliver a guessed prompt). If product requires the stored envelope, the alternative is `instructionRevisionParts` rows (`id = {revisionId}.{n}`, header carries `partCount` and hash, all in one batch; revision unavailable until all parts verify). Reader integrity check on every materialization: recompute `hash`; mismatch => revision flagged `Corrupt`, excluded from selection.

Budget: effective instruction text total <= 9,000 chars, <= 32 entries (about 250 B/entry JSON overhead). Over budget => `createInstructionRevision` fails with an actionable error (spec M2 "instruction and revision size budgets"), before any registry write.

### 7.9 `instructionRevisionApplications` — id: `derive_id(operationId, "application")`

Immutable. Fields: `spaceId`, `chatId`, `workItemId`, `revisionId`, `sessionRef` (target Execution Session id, optional), `turnBoundary` (JSON `{turnId?, seq?}`), `reason` (`"launch"|"explicit"|"fork-inherit"`), `idempotencyKey` (= operation id), `result` (`"success"|"failed"|"skipped"`, with `error?`), `appliedHlc` (the op's own HLC written into the field, `pin_order_key_between` precedent, C9), `createdBy`, `createdAt`. Chat's selected revision = argmax over `result=="success"` rows for that chat of `(appliedHlc, id)`. Idempotent: a replayed workflow re-upserts the same id (create-if-absent skips). Outcome rows are append-only; a failed attempt followed by success is two rows with two op ids (or a status update on the same row only from `pending` to a terminal result, if the workflow needs two-phase; decide in M2).

### 7.10 Kind-name table (all <= 32 chars, `registry-core.ts:56`)

`workItems`, `workItemInstructions`, `workItemChatAssignments`, `executionConfigurations`, `workItemExternalReferences`, `userActionRequests`, `workItemInstructionProposals`, `instructionRevisions`, `instructionRevisionApplications`.

### 7.11 Where validation lives

| Concern | Layer | Why |
|---|---|---|
| Sizes, formats, immutable-field omission, id charset, same-Space checks, cycle checks on raw graph, create-if-absent | `crates/doc/src/registry/work_outline.rs` typed mutators on `RegistryDoc` (returning `DocError::Schema`) | mechanism layer; precedent `change_sidebar_pin`/`change_sidebar_section` |
| Authorization of originating Chat/agent, Space ownership against engine state (`space.device_id`), capability checks, workflows, idempotency across steps | `crates/engine/src/work_outline.rs` over `WorkspaceHost` | policy layer; precedent `set_chat_link` precedence gate (`registry.rs:1192-1198`) |
| Stateless size/format/enum-shape backstop (never a field allowlist) | `edge/src/registry-core.ts` `validateOp` | cheap, prevents junk rows; must be deployed first |
| Read-side repair, quarantine, clamping | pure `materialize_work_outline` | tolerates anything older/newer/malicious writers produce |

## 8. Materialization and cycle repair

**Location.** Pure function, no I/O, no lock: `crates/doc/src/registry/work_outline/materialize.rs`:

```rust
pub struct OutlineInputs { space: Option<Space>, items: Vec<ItemRow>, /* all spaces' items: only id, spaceId, parent, hlc */ ..., chats: Vec<ChatLite> }
pub fn materialize_work_outline(space_id: &str, inputs: &OutlineInputs) -> WorkOutlineSnapshot;
```

`RegistryDoc::work_outline_inputs(space_id)` clones overlay rows with `(fields, clocks)` (new `overlay_rows_with_clocks`, exposing `RegistryRow.clocks`, which `read_kind` currently drops). Pending ops apply with their op hlc through `apply_op`, so optimistic clocks are correct. Wire types and limits live in `crates/proto/src/work_outline.rs`.

**Algorithm (deterministic; all output sorted).**
0. Space gate: Space row must be live in the overlay, else return `available=false` with no rows (this is the suppression rule of spec L691, section 11).
1. Decode each row leniently; malformed rows are dropped and listed as `Repair::Malformed{kind,id}`. Rows whose `spaceId` is absent are dropped with `Repair::MissingSpace`. Keep only rows with `spaceId == S`, except the global `(id -> spaceId, parentId, parentHlc)` item index used to classify parents.
2. Effective parent of item `x` with raw parent `p`: none => root. `p` absent from the global index (missing, tombstoned, or not yet synced) => root + `Repair::ParentMissing{x,p}`. `index[p].spaceId != x.spaceId` => root + `Repair::CrossSpaceParent{x,p}`. Archived parents are normal parents.
3. Cycle detection on the functional graph (out-degree <= 1): visit nodes in id order with white/grey/black coloring, following effective parent pointers; on reaching a grey node collect the cycle members (includes self-parent). Cycles are disjoint, so each needs exactly one edge removed.
4. Victim per cycle: the member with the greatest `(clocks["parentWorkItemId"], id)` tuple (absent clock sorts lowest; compare HLC strings). Treat its parent as none and emit `Repair::CycleBroken{victim, formerParent, members}`. This is the spec's rule with the faithful mapping `updated_hlc := clock of the parent field` (C1). The repair stays derived; `repairWorkOutline(spaceId)` persists it as `update parentWorkItemId=null` ops for the victims, which then carry newer clocks.
5. Children lists sorted by `(orderKeyValid? 0:1, orderKey bytes, id)`; invalid/missing keys sort last, by id.
6. Per-item rollups (unresolved action counts incl. ancestors) computed bottom-up over the repaired tree; archived flag carried, nothing filtered (default-view filtering is presentation).
7. Instructions: scope `workItemId` absent => Space-root; present but item missing/cross-Space => `Repair::OrphanInstruction`, excluded from effective instructions but listed. Effective instructions for an item: walk repaired ancestors until an item with `inheritParentInstructions==false` (inclusive), then prepend all Space-root instructions; archived instructions excluded.
8. Assignments (key = chat id): join with the chat row by id. Chat missing => `DanglingChat` (kept, inspectable). Chat `spaceId != S` or assignment `spaceId != S` => `Repair::AssignmentSpaceMismatch` and treated unassociated. Work item missing/cross-Space => chat shown unassociated + `DanglingWorkItem`. Projectless chats never assignable (write-time).
9. Execution configurations (valid-config rule 7.4), external references (dropped if item missing), action requests (dedupe rule 7.6), proposals (precedence 7.7), revisions (integrity 7.8), selected revision per chat (7.9).
10. Output: `{ available, synced, space, items (tree + flat), instructions, assignments, configs, references, actionRequests, proposals, revisions(selected only + ids), repairs, revision }`; repairs sorted.

**Write-time cycle validation must walk the RAW parent graph, not the repaired one.** Otherwise the user can move P under a former cycle victim, recreate a raw cycle whose greatest-clock edge is now the user's new move, and the repair silently undoes the user's move. Rule: if the raw ancestor chain of the destination contains a cycle, the mutation emits the persisted repair for that cycle in the same batch before the move.

## 9. Fractional order keys

Recommendation: a compact base-62 midpoint scheme with the record id as the tie-break (spec L90, L426), NOT the pin scheme.

- Alphabet `0-9A-Za-z` (ASCII order = digit order, so plain byte comparison orders keys; same alphabet as rocicorp `fractional-indexing`, public domain). New module `crates/proto/src/work_outline_order.rs` (next to `sidebar_pins.rs`); mirrored (for edge validation and shared vectors) by a regex in `registry-core.ts`.
- API: `valid_order_key(&str) -> bool` (1..=64 chars, base62, fractional part does not end in `0`, is not the minimum integer sentinel), `order_key_between(lower: Option<&str>, upper: Option<&str>) -> Result<String>` (strictly between, `None` = open end; first key `a0`), `order_keys_between(lower, upper, n) -> Vec<String>` (evenly spread for rebalance and multi-insert).
- Algorithm: the fractional-indexing construction (integer-part head char encodes length: `a..z` positive, `A..Z` negative; fraction digits via midpoint of the first differing digit, shared prefix stripped, recurse when adjacent). Appends/prepends bump the integer part so sequential creation stays about 1 char per 62 inserts rather than shrinking the gap each time (a pure midpoint scheme grows about 1 char per 5 appends at the tail).
- No nonce, no jitter: two devices inserting between the same neighbors may mint equal keys; sort is `(orderKey, id)` so the result is deterministic and both items keep stable relative order.
- Ties at insertion time: `placement` names a neighbor, not an index. If the neighbor's key equals the next sibling's key (a tie group), the writer assigns fresh distinct keys to the whole tie group in the same batch (group P writes), then inserts between.
- Length cap 64: if `order_key_between` would exceed it, the mutation rebalances that sibling group (`order_keys_between(None,None,n)`) in one batch; siblings in one parent are bounded (cap at 2,000 per parent, error beyond, so a rebalance fits two batches of 400 ops).
- Validity at read: invalid key => sort last by id plus `Repair::BadOrderKey`.
- Used for `workItems` (per parent) and `workItemInstructions` (per scope).
- Fallback if scope must shrink: reuse `pin_order_key_between` unchanged (hex, unique by nonce); costs about 300 B/key and needs a kind-aware edge regex.

## 10. Write-path rules (mandatory)

- **R1 Create-if-absent distinguishing tombstones.** Add `RegistryDoc::row_state(kind,id) -> Absent|Live|Tombstoned` (new; `overlay_row` hides tombstones, `registry.rs:676`). A retried create of a tombstoned id must not revive it unless the operation is explicitly a restore.
- **R2 Update-only mutations** (`OpKind::Update`) for everything after creation, never carrying immutable fields. Existing mutators already follow this (`rename_chat`, `set_chat_archived`: `registry.rs:1008-1032`), except sidebar code, which uses upsert and relies on soft-delete.
- **R3 Clock observation.** Before writing any outline row, bump `HlcClock` past the row's `max_clock` and the destination parent's, as `observe_sidebar_row` does (`sidebar_sections.rs:10-24`), because `HlcClock` has no receive rule. Otherwise a causally later edit from a slow-clock device is silently discarded and the UI snaps back (merged row is broadcast to the sender, `registry-room.ts:396-407`). Consider promoting this to a general receive-merge in `apply_rows` as a follow-up (blast radius: a far-future clock from any device).
- **R4 Size budget gate** before `enqueue_ops` (section 6), and byte-aware batch chunking (required change in `enqueue_ops`, `registry.rs:609-626`).
- **R5 Operation ids.** `operationId` is a client UUID. Derived row ids `derive_id(operationId, role) = hex(sha256(operationId NUL role))[..32]` (allowed charset, fixed length); every multi-step workflow is "replay all steps with create-if-absent", so a crash anywhere converges without duplicates. For the non-registry steps (queued first turn), derive the command id from the operation id so the existing `mark_processed` ledger (`store.rs:501`) dedupes execution. A small device-local workflow journal is only needed if steps cannot be re-derived from registry state; prefer re-derivation.
- **R6 One batch per logical mutation** where it fits in 400 ops and the byte budget (createChatForWorkItem rows, approve-proposal + instruction, move + repair). Cascades larger than a chunk are best-effort-atomic and must be safe to replay.
- **R7 Enum and optional leniency** on every `Raw*` (section 2.3).
- **R8 Chat-row writers stay untouched.** The outline never writes `chats`, `spaces`, or `sessions` rows; `claim_chat` and `upsert_chat` (`registry.rs:894-970`) stay the only writers, preserving old-client behavior.

## 11. Space tombstone suppression and cleanup

- Materialization requires a live Space row (algorithm step 0). `WatchWorkOutline(spaceId)` for a tombstoned/absent Space returns `available=false`. Suppression keys on "not live", not on seeing a tombstone, so it also covers Spaces whose tombstone was GC'd (C3) and the not-yet-synced case (`synced=false` tells the UI which).
- `delete_space` extension (`registry.rs:859-890`): also tombstone every outline row with `spaceId == space_id` in the same chunked batch (cost: one small delete op per row; 5,000 rows = 13 chunks). The orphan-chat pattern shows a cascade misses concurrent creates.
- Cleanup operation: add an outline pass to `sweep_orphans` (`spaces.rs:293-322`; runs every 2 min, `REPAIR_INTERVAL` :36): when `registry_synced()` and a Space id referenced by outline rows has no live Space row, tombstone those rows. Any device may do this (the tombstoned Space's `deviceId` is cleared, so ownership cannot be consulted). Make it idempotent by only emitting a delete for rows that are still live in the overlay; duplicate deletes from several devices just bump `delHlc`. Never run on an unsynced replica (`spaces.rs:293-301` incident precedent).
- Late offline writes: suppressed by step 0 even before cleanup. Recreating a folder yields a new Space uuid (`ui/src/shell/spaces.rs:5549`), so it never adopts the old outline (spec L691). Exception: C6 import.

## 12. Compatibility, rollout order, capabilities

1. **Deploy edge first.** Edge changes (`validateOp` additions, tests) ship and deploy before any client writes outline rows; deploy is automated on pushes to `main` touching `edge/` (`wrangler.jsonc` header comment). A client batch rejected by an older edge wedges the queue (section 6), and one oversize/bad op in a new kind would wedge all registry writes (sessions status, renames) for that device. Add a Rust-side kill switch `ZERON_WORK_OUTLINE_WRITES` default on only after the edge deploy is confirmed (cheap insurance).
2. **Capabilities.** Registry rows themselves need no gating (older clients ignore them). Runtime features advertise via the `device.capabilities` string array stamped on boot (`proto/src/workspace.rs:8-27`; `workspace_host.rs:286`): `work-outline-v1` (rows readable and writable), later `work-outline-instruction-envelope-v1`, `work-outline-multi-repo-v1` for hosts that can honor instructions/execution (M2/M3).
3. **Mixed fleets.** Old clients never see outline kinds, never damage them, and never cascade them; new clients repair by suppression and orphan cleanup. A downgraded engine reading the persisted `registry1` snapshot keeps unknown rows (`PersistedState.v == 1` unchanged, `registry.rs:417-447`); do not bump `PersistedState.v`.
4. **Snapshot version.** No change to `v` or `CURRENT_RESYNC_EPOCH` (`registry.rs:328`).

## 13. Test plan (shared vectors in Rust and TS)

- TS `registry-core.test.ts`: per-kind size/format validators (oversize op, bad orderKey, unknown field accepted), nothing rejects unknown fields.
- Rust `registry/tests.rs`: unknown-kind round trip through `apply_rows`, `to_bytes`/`from_bytes`, `apply_state` full replace, re-seed; tombstone then late upsert revive vs update no-revive for outline kinds; concurrent move versus reorder converges to a whole placement; slow-clock device edit not lost (R3); oversize op refused client-side; byte-aware chunking; `delete_space` cascade includes outline rows; Space suppression; lenient unknown `status`.
- Materialization property tests: random parent graphs with cycles (self-parent, 2-cycle, nested, disjoint multiple), cross-Space and missing parents; determinism under input permutation; repair idempotence after the persisted repair; raw-graph move validation.
- Order keys: `between` strictness, `n` keys monotone, 10^4 sequential appends stay under 8 chars, tie-group rebalance, mixed random inserts preserve `(key,id)` order, TS/Rust regex agreement.
- Live-edge: extend `crates/sync/tests/registry_edge.rs` (ignored test against `wrangler dev`) with two clients converging on outline rows.
- Large outline: 10k items; snapshot recompute time and `to_bytes` size; quadratic overlay case with 5k pending ops.
- Restart/offline: pending outline batches survive restart and re-push; stale cursor full resync; local-to-synced import copies outline rows (C6).

## 14. Open decisions and risks

1. **Revision size vs spec**: choose between "no stored rendered envelope" (recommended), or `instructionRevisionParts` rows, or raising the per-kind cap at the edge (deploy-order risk). Product must accept the 9,000-char effective-context budget.
2. **HLC receive rule**: scoped `observe_row` (recommended for MVP) versus a global receive-merge in `apply_rows`.
3. **Byte-aware batch chunking** changes shared code (`enqueue_ops`); small, but touches every writer. Required before large outline rows ship.
4. **30-day tombstone GC** resurrects stale-device deletions; accepted, documented (C3).
5. **Snapshot cost**: the registry snapshot rewrites every row each debounce; outline rows (thousands) increase it. Measure; if bad, split outline into its own local snapshot row (does not change sync).
6. **Recompute cost** of `WatchWorkOutline` on every registry change (including seen markers and presence-driven publishes); mitigated by coalescing and dedupe; measure with 10k chats.
7. **Local-to-synced import** (C6) is unowned in the spec.
8. **Unverified platform limits**: DO SQLite row size, WS send message size for a full `state` frame. Registry already ships multi-MB full states by design (`docs/registry-sync.md:19-20`), but outline growth should be load-tested against the live edge.
9. **Edge deploy ordering** is the highest-severity operational risk (batch wedge); gate client writes on it.
10. **Concurrent createSpace race** can yield two Space ids for one folder; each gets its own outline by design (spec L689); UI should warn only if it ever surfaces both.
