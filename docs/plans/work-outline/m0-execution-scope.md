# M0 audit: multi-repository execution scope

Scope: spec "Execution scope", section 3 "Execution configuration", the `execution_configuration` data-model row, acceptance criteria 13-15.
Branch audited: `work-outline-implementation` (read-only). All citations are `path:line` against that tree. Git behaviour claims marked "(spike)" were reproduced in a throwaway repo with Apple git 2.39.3; the temp repo is deleted.

## 0. Verdict

"One execution root + primary focus + member repos + exact worktree per repo" is **feasible without a new Git implementation**, but not as a data-model-only change. Five engine/UI changes are required, and one spec gap (per-Chat captured binding) must be closed first.

1. The Chat has no execution-scope fields beyond `cwd`/`branch`/`checkout_id`/`source_context`, and `RunRequest` has a single `cwd` (crates/proto/src/agent.rs:96-129). A resolved binding must be captured per Chat and ride the Run command.
2. There is no validator today. Launch/resume has at least 9 silent-fallback paths (section 2.3).
3. Diff, branch and PR indicators are strictly one checkout per Chat (section 3). Per-member data needs member checkouts added to the existing checkout-keyed diff pipeline; the per-checkout machinery itself is reusable.
4. Harnesses are cwd-rooted with no additional-directory concept. Member worktrees outside the execution root are not covered by the workspace-write sandbox (section 3.4).
5. `Space.git_detected` is "inside a git work tree", so a workspace folder that is not itself a repo (sibling repos) is not recognised as git at all.

Layout verdicts:

| Layout | Verdict |
|---|---|
| A. Superproject with submodules (`ws/ui`, `ws/flagship`), root = superproject checkout, member worktrees under `ws/.worktrees/<label>/<task>` | Feasible. Member worktrees live under the root so sandbox containment holds. |
| A'. Root = a superproject worktree with initialised submodules (`git submodule update --init`, already exercised by tests at crates/engine/src/chat_workspace_plan.rs:1793-1840) | Feasible and the cleanest "all members under one root" shape, but a different creation flow than `create_chat_worktree`. |
| B. Plain non-git folder containing sibling repos | Feasible via the same binding (root kind = plain folder), but needs Space/diff/source-context changes (section 3.3). |

## 1. How a Chat's execution environment is represented today

### 1.1 Rows (registry, synced)

| Concept | Fields | Cite |
|---|---|---|
| Space | `id`, immutable `device_id`, `path`, owner-stamped `git_detected`, `checkout_id` | crates/proto/src/entities.rs:112-132 |
| Chat | `device_id` (host), `cwd`, `branch`, `checkout_id`, `source_context`, `config`, `harness_session_id`, `harness_session_cwd`, `space_id`, `parent_chat_id` | entities.rs:239-309 |
| `ChatConfig` | harness/model/reasoning/sandbox/auto_approve. No cwd, repo, or worktree. | entities.rs:154-167 |
| `ConversationSourceContext` | one `{checkout_id, repo_root, cwd, branch, head_sha}` captured immediately before each dispatch | entities.rs:220-235; doc_host.rs:4588-4630 |
| `Worktree` | `repo_path`, `path`, `branch`, `name`, `checkout_id` | entities.rs:472-482 |

There is no "repository" row. Repository registration is a per-device local `repos.json` list of path strings (crates/engine/src/repos.rs:222-247, `add` at 407). It is not synced and has no ids. `Repos::list` silently drops vanished paths and non-repos (repos.rs:387-404). The only synced identity above a path is the Space.

Space creation is idempotent on `(device_id, path)` (workspace_host.rs:998-1003), which matches the spec's "Space = synced (device, absolute folder)".

### 1.2 Worktrees: two incompatible flows

| | `Repos::create_worktree` via `RunRequest.worktree` (`WorktreeSpec`) | `create_chat_worktree` via `CREATE_CHAT_WORKTREE` |
|---|---|---|
| Location | `{worktrees_root}/<repoName>/<adjective-noun>`, default `~/.zeron/worktrees` (repos.rs:153-158, 1127-1187) | `<superproject>/.worktrees/<label>/<slug>` (chat_workspace_plan.rs:249-337) |
| Branch | new `zeron/<name>` off the picked base | new branch named `<slug>` off `default_branch` |
| When created | on the HOST at command-drain time (doc_host.rs:4308-4316, 4637-4667) | on the sender's LOCAL engine, before the Run is queued (composer.rs:7011-7033) |
| Remote Spaces | yes (host materialises) | **no**: method is IPC-only (rpc.rs:3895-3898), and the chip is gated to local targets (pickers.rs:2310-2322) |
| Extras | none | `.workspace-root` marker with the superproject path (317), `.claude` seeding (353), submodule-nesting guard (1089-1106) |
| Auto title rename | `zeron/*` branches are renamed after the chat title (titles.rs:131-148; repos.rs:1212-1249) | not renamed (no `zeron/` prefix) |

`<label>` is the toplevel directory name only when `git rev-parse --show-superproject-working-tree` is non-empty (chat_workspace_plan.rs:266-275, 1039-1045), else the literal `"workspace"`. Consequences:
- An embedded (non-submodule) repo gets label `workspace` and a worktree under its own `.worktrees/workspace/`.
- Called with a *submodule worktree* path, `--show-superproject-working-tree` is empty (spike), so the label logic only works from the submodule main checkout.
- A worktree of the superproject has **uninitialised** submodule directories (spike: `ls ws/.worktrees/workspace/t/ui` empty, `submodule status` shows `-`). The `.worktrees/ui/<task>` and `.worktrees/flagship/<task>` worktrees are therefore siblings of, not inside, `.worktrees/workspace/<task>`.
- `CREATE_CHAT_WORKTREE` also calls `set_chat_target(chatId, worktree, checkout_id)` (rpc.rs:2669-2697), retargeting the Chat's `cwd` as a side effect. That conflicts with a binding where `cwd` must stay the execution root.
- It is not idempotent: an existing target returns "worktree path already exists" (chat_workspace_plan.rs:286-291).

`.workspace-root` is a plain-text marker (first line = superproject path). It is consumed only for overview repo grouping (ui/src/overview.rs:456-521), close-out ownership (ui/src/chat_closeout.rs:11,39,52) and dirty-ignore (engine/src/repository_topology.rs:348,374-381). It records no membership, is not validated, and must not be used as identity.

### 1.3 Harness cwd choice

New chat (composer.rs:6853-6908): `space.path` -> or `"~"` for projectless -> or `"."` (6853-6860); `ReuseWorktree{path}` overrides `cwd` (6877-6881); `NewWorktree` leaves `cwd` as the repo and attaches `WorktreeSpec` (6882-6906); the chip's `CREATE_CHAT_WORKTREE` result overrides `cwd` after `createChat` (7025-7028). `createChat` stores `cwd.unwrap_or(space.path)` (workspace_host.rs:953-958).

Dispatch (`SessionsEngine::dispatch_inner`): `resolve_run_cwd` only rewrites `~`/HOME to a per-chat scratch dir (sessions.rs:423, 1650-1670). It does not check that the path exists. Warm runtimes are keyed by cwd (`RuntimeConfig.cwd`, sessions.rs:75-103), so a changed cwd replaces the runtime.

Resume: `resume_for(chat, cwd)` injects the stored harness session id only if `session_cwd.is_empty() || session_cwd == cwd` (string compare, sessions.rs:1245-1261). A mismatch silently starts a fresh harness session. This contradicts the spec's definition of an Execution Session as sharing "native resume identity, harness, execution root".

Resume has three different cwd sources depending on the caller:

| Caller | cwd source | Cite |
|---|---|---|
| Queue promotion | chat row (`request_from_chat_row`) | doc_host.rs:3256-3287 |
| Steer / input fallback | in-memory `last_request`, else chat row | doc_host.rs:4449-4451, 4516-4518 |
| Crash auto-resume | `last_request`, else chat row, else journal cwd | sessions.rs:834-852 |
| Orphaned steer | `last_request` | sessions.rs:2757 |

All of them funnel through `DocHost::dispatch_with_source_context` (doc_host.rs:4613-4630; callers at 3287, 4378, 4467, 4532, sessions.rs:866, 2771). That is the single chokepoint for a validator hook.

### 1.4 Remaining pieces

- `claim_chat` / `space_for_path`: a Run for a chat with no row claims the chat under the host and, if no Space matches the cwd, **mints a Space at that path** (workspace_host.rs:784-832). Linked-worktree collapsing uses `linked_worktree_root` (1623-1645), a pure `.git`-file string heuristic that returns `None` for submodule worktrees (`gitdir: .../.git/modules/ui/worktrees/<w>`, spike), so a submodule-worktree cwd would mint a phantom Space.
- `SetChatCwd` mutate: any device may retarget any Chat (rpc.rs:1287-1318). It resolves the identity, writes `cwd` + `checkoutId`, and the next run starts a fresh harness session (workspace_host.rs:1306-1310).
- MCP `create_chat` accepts an arbitrary unvalidated `cwd` (mcp/src/tools.rs:622-624); MCP `send` falls back `chat.cwd -> space.path -> "~"` (tools.rs:910-914).

## 2. Identity, drift detection, fallbacks

### 2.1 What stable identity exists

| Thing | Identity | Derivation | Survives | Does not survive |
|---|---|---|---|---|
| Space | `Space.id` (uuid) plus `(device_id, path)` | synced row | rename | n/a (immutable) |
| Checkout / worktree | `CheckoutIdentity.id` | `sha256(device_id NUL canonical --git-dir)` (repos.rs:62-69, 348-371) | `git worktree move` (git-dir under `.git/worktrees/<name>` unchanged) | move of a main checkout (git-dir moves); **remove + re-add of a worktree with the same basename yields the same id** (path-derived) |
| Repository | `RepositoryTopologyRepository.id` = `"repo:" + sha256(device_id NUL canonical --git-common-dir)` | repository_topology.rs:121-125, 530-536 | worktrees of the repo | repo moved; **a different repo cloned to the same path yields the same id** |
| Plain folder | `folder-<sha256(device, "plain-folder", path)>` | workspace_files.rs:207-215 | n/a | any move |
| Chat -> checkout | `Chat.cwd` is authoritative (rpc.rs:1288); `checkout_id` is a cache stamped asynchronously by diff-sync (diff_sync.rs:416-424; rpc.rs:945) | | | stale until reconcile |

Both repo and checkout ids are device-scoped and path-derived, so neither can detect "same path, different repository". The binding needs a content fingerprint in addition (section 4.1).

### 2.2 How missing/moved/pruned is detected today

Not as a launch gate anywhere.
- Topology reads `prunable`/`locked` from `git worktree list --porcelain -z` and only displays them (repository_topology.rs:431-462, 183-184).
- Close-out requires a registered linked worktree (chat_workspace_plan.rs:567-579).
- Diff-sync drops an identity when the path is gone and logs at debug only (diff_sync.rs:374-379); the Changes pane shows "Preparing" indefinitely (changes.rs:920-926).
- `capture_source_context` returns `None` on any failure and the caller ignores it (doc_host.rs:4588-4606, 4621-4626).
- `workspace_checkout` (the file-surface and project-action path jail) authorises only the Space root plus worktrees *that have a branch* in `git worktree list` of the Space repo (repos.rs:540-574, 1053-1073). Detached worktrees and worktrees of a submodule when the Space is the superproject are rejected (spike: superproject `git worktree list` shows only itself).
- Spike: in a submodule, `git worktree list` reports the **main** worktree path as `.git/modules/<n>` (core.worktree quirk). Topology's `is_main = (index == 0)` (repository_topology.rs:182) and cwd matching (231-335) will mis-place a submodule's main checkout. The validator must identify the main checkout by `git-dir == git-common-dir`, not by list index.

### 2.3 Every silent fallback to another directory

| # | Where | Behaviour |
|---|---|---|
| F1 | composer.rs:6853-6860 | missing Space path -> `"~"` -> `"."` (process cwd) |
| F2 | composer.rs:6898-6899; pickers.rs:136-137 | `NewWorktree` with no base -> `HEAD`; old host ignores the spec and runs in the main checkout (6884-6887) |
| F3 | composer.rs:6968-6971, 7029-7031 | chip worktree creation failure -> "dispatching in the base checkout" (outcome is badged, send proceeds) |
| F4 | doc_host.rs:4775 (`request_from_chat_row`) | `cwd: chat.cwd.unwrap_or_default()` -> empty string |
| F5 | claude/mod.rs:233, codex/mod.rs:709, cursor/mod.rs:321, opencode/mod.rs:418, acp/mod.rs:1281 | empty cwd -> harness inherits the engine process cwd (no `current_dir`) |
| F6 | sessions.rs:1245-1261 | resume cwd mismatch -> fresh session without error; empty stored cwd matches anything |
| F7 | workspace_host.rs:784-832 | missing chat row -> claim under cwd, mint a Space for an arbitrary path |
| F8 | mcp/src/tools.rs:910-914 | `chat.cwd` -> `space.path` -> `"~"` |
| F9 | sessions.rs:834-852; doc_host.rs:4449-4451, 4516-4518 | stale in-memory `last_request.cwd` preferred over the current chat row in steer/crash paths, while queue promotion prefers the row (inconsistent source of truth) |
| F10 | doc_host.rs:4642-4651 | `materialize_worktree` reuse guard uses the `.git`-file heuristic (None for submodule worktrees) and so can mint a second worktree |

A nonexistent cwd given to a harness spawn is an OS spawn error surfaced as an Errored session. That is a failure, not a fallback, but the message is not actionable.

## 3. Diff / branch presentation

### 3.1 Single checkout per Chat everywhere

- Diff-sync groups this device's chats by `chat.cwd -> checkout_identity` (diff_sync.rs:391-430) and publishes one `CheckoutDiff` per checkout (673-692). Entries are chat-agnostic except the edge sidecar, which posts to each chat's slot (698-711): a Chat can own one sidecar slot, so multiple checkouts per Chat would overwrite each other.
- Changes pane: `ChangesTarget` carries one `checkout_id`/`cwd`/`branch` (changes.rs:68-98); `resolve_diff` picks one diff by checkout id, then device+cwd (896-909).
- Branch chip and ref picker: single Space repo (`ListRefs`/`SwitchRef`, pickers.rs:1638, 1757); `CheckoutPlan` is one repo (128-139).
- PR/ticket status keys on `(chat cwd, chat branch)` and runs `gh pr view <branch>` from that cwd (pr_ticket_cache.rs:400-405, 555). With root = superproject and branch = `ui`'s branch this queries the wrong repository.
- Topology is Space-scoped and does join chats to worktrees (attach_chats, repository_topology.rs:231-335) but by cwd prefix: a Chat rooted at the superproject lands under the superproject worktree only. The nested scan skips `.worktrees` (487) and only finds members through `.gitmodules` or `.git` markers.
- `WorkspaceTarget` (chat XOR space + optional checkoutPath) and the file-surface jail are single-checkout (workspace_files.rs:231-343; entities.rs:532-538).

### 3.2 What is already reusable

- `WATCH_CHECKOUT_DIFFS` already accepts `{cwd, checkoutId}` scoping with an id-vs-canonical check (rpc.rs:2770-2800), and `GET_CHECKOUT_DIFF` does the same (2855-2883). A per-member watch/capture works today for any checkout that has a diff-sync entry.
- `CheckoutDiff.submodules` already embeds expanded diffs for initialised gitlinks (depth <= 2, <= 8 repos; diff_sync.rs:50-64, 1187+) for the **in-tree** submodules of the root checkout. It does not carry per-submodule branch and does not cover member worktrees outside the tree.
- Per-repo branch/status/ahead-behind already exists in topology (`RepositoryTopologyWorktree`, repository_topology.rs:164-188, 337-366) and `Repos::current_branch`/`head_sha` (repos.rs:307-328).

### 3.3 What it takes to show branch + diff per member repo for one Chat rooted at a superproject

1. Persist the resolved member list on the Chat (section 4.3); `Chat.cwd` stays the execution root.
2. Diff-sync `reconcile`: for every Chat with a binding, also resolve each member's checkout (exact worktree or main checkout) and add the checkout entries to `groups` (identity memo and orphan grace already handle churn, diff_sync.rs:337-389, 436-458). Skip the edge sidecar for member entries, or key it by `(chat, checkout)`.
3. `ChangesTarget`: add a selected-repo dimension (default = primary); resolve by member `checkout_id`. The pane, scopes (working tree / branch / latest turn) and submodule folds are checkout-scoped already.
4. Branch indicator per member: from validated live state (section 4.4), not the stamped `chat.branch`. Keep `chat.branch` mirroring the primary member so PR inference and existing UI stay correct; PR lookup must use the primary member's path.
5. Layout B (no git at the root): `Space.git_detected` is false (spaces.rs:247-280; gates at pickers.rs:1597, shell.rs:2542), `checkout_identity(root)` fails, and diff-sync/source-context produce nothing. Needs a Space flag (for example `has_member_repos`) or per-binding gating independent of `git_detected`.
6. File surfaces for member worktrees: `workspace_checkout` must be generalised to accept bound checkouts (branchless and submodule worktrees included).

### 3.4 Sandbox / writable workspace

`RunRequest` has one `cwd` and no additional directories; no harness adapter passes `--add-dir` or writable roots (grep for add_dir/writable_roots in crates/: no hits). `SandboxLevel::WorkspaceWrite` is rooted at cwd (codex/mod.rs:32-41, 943, 1049). Member worktrees under the execution root are writable; ones outside (`~/.zeron/worktrees`, `WORKSPACE_WORKTREES_DIR`, root = a superproject *worktree* with members at `ws/.worktrees/...`) need escalation approvals or `auto_approve`/full access. Spec "writable execution root" therefore means "harness cwd", and the validator should report member paths outside the root. The git-dir of a linked worktree is outside the worktree for every existing worktree chat, so that part is not new, but a spike (S2 below) should confirm commits in member worktrees under each supported harness sandbox.

## 4. Remote / device

- Ownership is `Chat.device_id` = host and `Space.device_id` immutable (entities.rs:112-117; workspace_host.rs:931-946). Host-only writes are enforced by `is_host` (761-770) and owner-only stamps (1041-1060).
- Paths are only ever validated by the device that owns them. Examples of the existing pattern, which the validator should follow: `local_project_action_space` (rpc.rs:998-1010), `RUN_PROJECT_ACTION` (3557+: chat device == host, chat space == space, `workspace_checkout`), `WorkspaceFiles::resolve_target` (workspace_files.rs:253-290: chat device, space device, broad-root rejection, jail).
- Routing: `targetDeviceId` forwarding for the `forwardable` list (rpc.rs:1425+, 1810-1815). Repo/worktree/diff/file methods forward; `PLAN_CHAT_WORKSPACE` and `CREATE_CHAT_WORKTREE` are IPC-only.
- A client cannot validate a remote path. It knows only owner-stamped `Space.git_detected`/`checkout_id`, device capabilities (`Device.capabilities`, `capabilities::*`, `device_supports`, workspace.rs:8-25; ui/state.rs:1168-1173), and heartbeat liveness (`peer_liveness`, workspace_host.rs:520-570; `device_online`, ui/state.rs:1698-1706, which treats unknown devices as online).
- Remote sends are durable by design: the composer queues and tells the user "Messages will send once the connection recovers" (composer.rs:7905-7931). A Run reaching the host is executed with no cwd check; execution errors become `SessionCommandStatus::Rejected` with a resolution string (doc_host.rs:4157-4161). This is the hook for host-side authoritative rejection.
- Command plane outruns the registry channel (workspace_host.rs:775-779), so the host's replica of a work-item configuration may lag the sender. Validation must therefore use a snapshot carried in the command, not a registry read.

## 5. Proposal

### 5.1 Binding shape (mapped to existing identities)

New additive registry row kind (crates/doc/src/registry.rs, alongside `KIND_CHAT_PR_LINKS`) plus proto types. No new Git code: every field is produced by existing `Repos::checkout_identity`, topology `stable_id("repo", ...)`, `git worktree list`, `current_branch`, `head_sha`.

```text
ExecutionConfiguration            // row id = scope key, one live row per scope
  scope: Space{space_id} | WorkItem{space_id, work_item_id}
  device_id                       // must equal Space.device_id (immutable copy, validated)
  execution_root: RootRef
  repositories: [RepositoryBinding]   // at most one primary; no duplicate repo_id
  provenance, updated_hlc, tombstone

RootRef { path, kind: Git | PlainFolder,
          checkout_id? }          // CheckoutIdentity.id when Git; plain_folder_identity when PlainFolder

RepositoryBinding {
  repo_id        // topology "repo:<sha256(device|common-dir)>"
  repo_path      // canonical main-checkout path (locator, display)
  relative_path? // under execution_root (matches topology relative_path)
  member: bool   // included in diff + repository context
  primary: bool  // focus; implies a binding exists
  checkout: Unset | MainCheckout | Worktree{ checkout_id, path }
  fingerprint?   // root-commit sha(s): detects "different repo at same path"
  expected_branch?, expected_head?   // informational/drift, never auto-switched
}
```

Notes:
- `repo_id`/`checkout_id` are path-derived (section 2.1), so `fingerprint` (first root commit via `git rev-list --max-parents=0`) is required to detect replaced repos. Residual: a re-created worktree with the same basename and same base commit is indistinguishable; `expected_branch`/`expected_head` mitigate.
- `checkout: Unset` is the "no worktree configured" state of spec section 3 and produces `WorktreeChoiceRequired`; `MainCheckout` is an explicit user choice and is validated as exactly that checkout (never "whatever is at the path").
- Remote/environment identifier: Zeron has no environment concept beyond the device; use `Space.device_id` (copied for snapshotting), do not add a second field.
- Space default with no row: implicit `execution_root = Space.path`, no bindings (today's behaviour), reported as `ResolvedFrom::ImplicitSpaceRoot`.

**Per-Chat snapshot (spec gap, see C5).** Persist `ChatExecutionBinding { chat_id, source_row, config_revision (row id + updated_hlc), resolved: ExecutionConfiguration body, captured_at, op_id }` as an additive row keyed by `chat_id`, written by the idempotent start operation before the first Run. Existing Chats keep it when the work-item configuration changes; `Chat.cwd` = `resolved.execution_root.path`; `Chat.branch` mirrors the primary member. Resume and every dispatch validate this snapshot. A newer configuration is reported as `ConfigurationChanged` (informational) with an explicit "adopt" action that begins a new Execution Session.

**Wire.** Add `RunRequest.execution: Option<ExecutionBindingSnapshot>` (serde default, like `worktree`, agent.rs:123-128) and capability `execution-scope-v1` in `capabilities::CURRENT` (proto/src/workspace.rs:8-25). The client refuses a bound launch/queue to a host without the capability, using the existing pattern at composer.rs:6470-6483. A bound request must not also carry `worktree` (reject).

### 5.2 Resolve algorithm (nearest work item -> Space default)

Input: repaired work-item hierarchy (cycle/cross-Space edges already derived away per spec), live configuration rows, Space.

1. Start at the Chat's work item (null -> skip to step 3). Walk `item -> parent -> ... -> root` using the *repaired* parent edges, stopping at the first node with a live (non-tombstoned) `ExecutionConfiguration` row for that scope. If two rows exist for one scope (offline writes), pick the greatest `(updated_hlc, row_id)`, the same tie-break the spec uses elsewhere.
2. That row wins **as a whole**. Do not merge field-wise: a worktree identity is only meaningful with its root and repo set, and the spec does not define partial inheritance (flagged C6). Record `ResolvedFrom::Item(id) | Ancestor(id)`.
3. Otherwise the Space default row (`ResolvedFrom::SpaceDefault`).
4. Otherwise the implicit Space-root default (`ResolvedFrom::ImplicitSpaceRoot`).
5. Return `Resolved { config, from, revision }`. Resolution is pure (no filesystem, no git) so it is testable without repos and safe on any device, including the client.

A Chat with a persisted snapshot never re-resolves on resume; re-resolution happens only for a new launch or an explicit adopt. An exception Chat (deliberately created with another configuration) stores its own snapshot and is flagged on the work item by comparing `config_revision`/`source_row` with the item's resolved row.

### 5.3 Validation function

```text
validate_execution_binding(ctx, binding, intent: Launch | Resume | Dispatch)
    -> Result<ValidatedExecution, ExecutionScopeFailure>   // aggregates all errors + warnings
```

`ctx` = `{ local_device_id, WorkspaceHost (space/chat read), Repos, Sessions liveness }`. Read-only: no git mutation, no worktree creation, no branch switch, no registry writes (asserted by tests). Steps, collecting every failure rather than returning on the first:

1. **Device**: `binding.device_id == space.device_id == chat.device_id`; on the host also `== local_device_id`. Client-side pre-flight adds: device known, `execution-scope-v1` supported, liveness not Dark/offline.
2. **Space**: row exists; `chat.space_id == binding.scope.space_id`; for a work-item scope the item's Space matches.
3. **Root**: not broad (`is_broad_workspace_root_resolved`, repos.rs:98-108); exists (bounded `path_exists`-style probe, repos.rs:290-296); canonical path equals recorded; lies under `Space.path` or is a registered linked worktree of a bound repo (precedent: `workspace_checkout`, repos.rs:1053); if `kind=Git`, `checkout_identity(root).id == recorded`, else for `PlainFolder` it must not have become a repo silently (warn).
4. **Each repository binding**: path exists; `git-common-dir` hash == `repo_id`; `fingerprint` matches (absent on unborn repos -> warning); `relative_path` consistent with root; location under root (else `OutsideExecutionRoot`, severity by sandbox level).
5. **Checkout**: `Unset` -> `WorktreeChoiceRequired`. `MainCheckout` -> `checkout_identity(repo_path)` is the main checkout (`git-dir == common-dir`). `Worktree{id,path}` -> path exists; listed (and not `prunable`) in `git worktree list --porcelain -z` from the repo (reuse `parse_worktree_porcelain_z`, repository_topology.rs:431-462); `checkout_identity(path).id == id`; same common-dir as the repo. If the id is registered at a different path -> `WorktreeMoved{expected, found}` (report, do not follow).
6. **Branch/revision (drift, non-blocking)**: `current_branch`/`head_sha` vs `expected_*` -> `BranchDrift`/`RevisionDrift` warnings per member. The spec's blocking list (device, Space, paths, repositories, worktree) does not include branch, and agents legitimately create branches. Zeron never switches branches.
7. **Structure**: duplicate `repo_id`, more than one primary, primary without a binding -> blocking.

Return value `ValidatedExecution { cwd (canonical root), root_checkout, members: [{binding, canonical_path, checkout_id, live_branch, head_sha, locked}], warnings }`. This is the single source for `RunRequest.cwd`, `ConversationSourceContext` (generalised to per-member), diff targets and per-repo branch indicators.

Error variants (each carries expected/found/path and a `remediation()`):

| Variant | Remediation offered |
|---|---|
| `WrongDevice`, `DeviceOffline`, `HostLacksCapability` | open on owning device / wait / update engine |
| `SpaceMissing`, `SpaceMismatch` | re-associate or recreate Space |
| `RootMissing`, `RootTooBroad`, `RootOutsideSpace`, `RootIdentityChanged` | pick a root / update configuration |
| `RepositoryMissing`, `RepositoryFingerprintMismatch`, `RepositoryOutsideRoot` | locate/re-bind repository |
| `WorktreeChoiceRequired` | use Space workspace, select existing worktree, or create one via the existing flow |
| `WorktreeMissing`, `WorktreePruned`, `WorktreeNotRegistered`, `WorktreeBelongsToOtherRepository`, `WorktreeIdentityMismatch`, `WorktreeMoved{expected,found}` | re-bind to found path / recreate worktree / choose main checkout |
| `ResumeRootMismatch` | start a new Execution Session explicitly |
| `InvalidConfiguration` (duplicate repo, multiple primaries) | fix configuration |

Warnings: `BranchDrift`, `RevisionDrift`, `WorktreeOutsideExecutionRoot`, `RepositoryUnborn`, `ConfigurationChanged`.

No variant has an automatic alternative. Failure encoding: host returns `Err(EngineError)` from `execute`, which already becomes `SessionCommandStatus::Rejected` with the message (doc_host.rs:4157-4161); the message carries a machine code prefix (`execution-scope:<code>`) and the UI maps it to the remediation actions.

### 5.4 Hook points

1. **Client start-from-work-item** (idempotent op, spec criterion 16): resolve -> client pre-flight (device/Space/capability/liveness) -> write Chat row (`cwd = root`) + `ChatExecutionBinding` + assignment -> queue `Run` with `request.execution = snapshot`, `worktree = None`, `cwd = root`. Offline host: spec says block; the code currently queues durably (C4). Recommended resolution: client pre-flight blocks a *fresh bound launch* to a Dark/offline device with `DeviceOffline`, while the host re-validates authoritatively at drain for everything that does get queued (resume, queue promotion).
2. **Host `execute(Run)`** (doc_host.rs:4291): validate immediately after `resolve_request_attachments` and before `take()` of the worktree spec (4308) and before `claim_chat` (4319). A bound request must never reach `claim_chat`'s Space minting (F7) or `materialize_worktree`.
3. **All dispatches**: validate in `DocHost::dispatch_with_source_context` (doc_host.rs:4613) for any chat that has a snapshot, then overwrite `request.cwd` with `validated.cwd`. This collapses the four cwd sources (F9) and covers Run, steer fallback, input fallback, queue promotion, crash recovery, orphan-steer re-dispatch. Cost is a handful of git processes per fresh dispatch; skip when a live run already holds an identical `RuntimeConfig` and `config_revision`.
4. **Resume** (sessions.rs:1245): for bound chats `resume_for` returns a three-way result: resume / fresh (no prior session) / `ResumeRootMismatch` (stored `harness_session_cwd` differs from `validated.cwd`), instead of the silent `None`. Treat empty stored cwd as mismatch for bound chats.
5. **Guards for the existing fallbacks** when a snapshot exists: `request_from_chat_row` returns `None` rather than an empty cwd (F4) (and harness adapters reject an empty cwd, F5); `SetChatCwd` rejects or updates the snapshot explicitly; MCP `create_chat`/`send` route through the same validation and ignore a `cwd` override for bound chats.
6. **UI**: hide/disable the worktree chip and checkout picker for bound launches (chip eligibility, pickers.rs:2310-2322); replace F1-F3 for bound launches with a blocking error card carrying the remediation actions.
7. **Worktree creation for bindings** (M3): needs an idempotent, non-retargeting variant of `create_chat_worktree` (adopt if path exists, registered, and branch/identity match; no `set_chat_target`), callable for remote Spaces (forwardable) or done via `WorktreeSpec`-style host materialisation. The two existing flows must be reconciled first (C1).

### 5.5 Feasibility summary for the required capability set

| Capability | Today | Needed |
|---|---|---|
| One execution root | `Chat.cwd` | binding root + snapshot; validated |
| Primary repo focus | none | `primary` flag; drives default diff target, `Chat.branch` mirror, PR lookup path |
| Member repos for diff/context | in-tree submodule sections only | per-member checkout entries in diff-sync; per-member selector in Changes pane |
| Exact worktree per repo | single `cwd` | per-binding checkout identity + validator |
| Branch per member | one `chat.branch` | live per-member branch from validator/topology |
| Remote | queue + host execution | snapshot in command, host validation, capability gate |
| Writable member worktrees | cwd-rooted sandbox | place under root or accept escalation; surface `WorktreeOutsideExecutionRoot` |

## 6. Spec contracts that conflict with the code

- **C1.** Spec s3: "create or select a Git worktree through Zeron's existing flow." There are two flows with different locations, branch names, host (local vs remote), and idempotency (section 1.2). One must be designated canonical for bound creation. `.worktrees/<label>/<task>` keeps member worktrees under a superproject root but is local-only and non-idempotent; `Repos::create_worktree` works remotely but lands outside the root.
- **C2.** Spec "Execution Session shares ... execution root": code resumes only on exact cwd-string match and otherwise silently starts a fresh session (F6). A resume mismatch must become an error for bound chats.
- **C3.** Spec "never silently fall back": ten fallbacks exist (section 2.3), several deliberate and documented as such (composer.rs:6968-6971 "Never fails the send: every error path falls back to the base checkout"). They are product behaviour for non-bound chats today; they must be disabled for bound ones.
- **C4.** Spec "offline ... block launch or resume" vs durable queued sends by design (composer.rs:7905-7931) and host-only path visibility. Needs an explicit rule (section 5.4.1).
- **C5.** The data model has no per-Chat captured configuration. Without it, editing a work item's configuration silently retargets existing Chats and breaks `harness_session_cwd` continuity, and "Association alone never mutates an existing Chat's configuration" cannot be honoured. Add a `ChatExecutionBinding` row (or additive Chat fields).
- **C6.** Spec says "resolve the nearest applicable execution root, primary focus, member repositories, and exact worktree" without saying whether fields inherit independently. Propose whole-row replacement.
- **C7.** Spec "A Space can define a default execution root." `Space.path` is today simultaneously identity, default `Chat.cwd` (workspace_host.rs:953-958), and Space inference key (`space_for_path`, 803-832). A Space default that differs from `Space.path` needs those three consumers reconciled; absent a row, the root must equal `Space.path`.
- **C8.** Spec "never create or switch a branch silently": `Repos::create_worktree` mints `zeron/<name>` at drain time (repos.rs:1166-1178), the chip's Auto path decides via a first-message classifier (chat_workspace_plan.rs:123-140; composer.rs:6990-7010), and `rename_worktree_branch` later renames `zeron/*` branches (titles.rs:131-148). All three must be off for bound chats; auto-rename would also invalidate `expected_branch`.
- **C9.** Spec "optional remote or environment identifier if Zeron already supports one": only the device does.
- **C10.** Spec "writable execution root": no enforceable write-root concept; sandbox is cwd-rooted (section 3.4).
- **C11.** Spec criterion 15 "branch and diff state per member repository": the pipeline is checkout-keyed but Chat-to-checkout is 1:1 (section 3).
- **C12.** `repos.json` registration (path list, per-device, unsynced, silently pruned on read) is not an identity source for the spec's "repository identity"; use topology repo id + fingerprint.

## 7. Test plan (temp git repos)

Fixtures to reuse: `init_repo` and `fake_superproject_with_submodule` (chat_workspace_plan.rs:1393-1417), `repo_with_initialized_submodule_worktree` (1793-1840, real `submodule add` with `-c protocol.file.allow=always`), `repo_with_claude_config`, the `WORKSPACE_WORKTREES_DIR` env lock (1446), `Repos::with_worktrees_root` (repos.rs:201), the mock harness (`HarnessId::Mock`) as used in crates/engine/tests (restart_resume.rs, chat_workspace_plan_rpc.rs, diff_sync_churn.rs, device_routing.rs). Place the validator in a new `crates/engine/src/execution_scope.rs` with a `#[cfg(test)]` module; integration tests under crates/engine/tests.

**A. Resolver (pure, no git)**
- R1 nearest wins: item row beats ancestor row beats Space row beats implicit root, with `ResolvedFrom`.
- R2 tombstoned row is skipped; R3 repaired parents (cross-Space parent = none, cycle edge removed) never loop.
- R4 whole-row replacement: no field merge between item and ancestor.
- R5 two live rows for one scope: greatest `(updated_hlc, id)` wins, deterministic.
- R6 persisted Chat snapshot is not affected by editing the configuration; `ConfigurationChanged` reported.

**B. Validator (real temp repos)**
1. Happy path: plain repo, explicit main checkout.
2. Superproject with real submodules `ui` and `flagship`, root = superproject, primary = `ui`, members both; distinct ids for root/ui/flagship.
3. Exact worktrees `ws/.worktrees/ui/t` and `ws/.worktrees/flagship/t` (`git -C ui worktree add`); assert four distinct checkout ids and identification of each main checkout by `git-dir == common-dir`, including the submodule `worktree list` quirk (assert the test would fail if it used list index 0).
4. Root: missing dir, broad root (HOME alias), outside Space, git-vs-plain kind change.
5. Worktree `rm -rf` without prune -> `WorktreePruned`; after `git worktree prune` -> `WorktreeMissing`; in neither case is the main checkout returned.
6. `git worktree move` -> same checkout id (assert), `WorktreeMoved{expected,found}`, no auto-follow.
7. Remove + re-add a worktree with the same basename -> same id (assert); detected only via `expected_branch`/`expected_head` (documents the residual risk).
8. Repo at path deleted and `git init` anew with a different history -> `RepositoryFingerprintMismatch` (assert repo_id equal, fingerprint differs).
9. Detached worktree: validator OK, while `workspace_checkout` rejects it (regression guard motivating the jail change).
10. Branch drift: HEAD moved to another branch -> `BranchDrift` warning only; assert HEAD, refs, worktree list and `git status` byte-identical before/after validation (read-only guarantee).
11. Member outside execution root -> `OutsideExecutionRoot` (warning or block per sandbox).
12. Structure: duplicate `repo_id`, two primaries, primary without binding.
13. Device: binding device != Space device != host device each -> distinct `WrongDevice`/`SpaceMismatch`.
14. `checkout: Unset` -> `WorktreeChoiceRequired`; explicit `MainCheckout` validated.
15. Layout B: plain folder root with sibling repos (`PlainFolder` root, two repo bindings).
16. No-fallback property: for every failing fixture above, assert `Err`, registry rows (chats, spaces, count) unchanged, `Chat.cwd` unchanged, no new worktree/branch on disk, and zero harness runs.

**C. Integration (engine/tests with mock harness)**
- Run with `execution` snapshot and a deleted worktree -> command `Rejected` with `execution-scope:<code>`; no session started; no Space minted (F7 guard); `worktree` + `execution` together rejected.
- Parametrised over every dispatch path (Run, queue promotion, steer fallback, input fallback, `recover_stale` auto-resume, orphan-steer): remove the worktree first; each path rejects, none runs in another directory.
- Resume: stored `harness_session_cwd` != root -> `ResumeRootMismatch`, not a fresh session; empty stored cwd on a bound chat treated as mismatch.
- Empty/absent cwd with a snapshot never reaches harness spawn (F4/F5).
- Idempotent retry: same op id twice -> one Chat, one snapshot, one Run; binding-aware worktree creation adopts an existing matching worktree and does not retarget `Chat.cwd`.
- Diff-sync: a Chat with root + two member worktrees -> `WatchCheckoutDiffs` has three entries; editing a file in the `ui` worktree changes only that entry's checksum; scoped watch (`{cwd, checkoutId}`) works per member; member removed -> entry orphaned after grace and surfaced as a validation failure rather than "Preparing" forever.
- Topology: member worktrees under `.worktrees` appear under the correct repo; Chat joined by binding, not cwd prefix.
- Capability: host without `execution-scope-v1` -> client pre-flight refuses (pure function test).

**D. Contract**
- Serde round-trip and old-host ignore for `RunRequest.execution` (pattern at proto/src/agent.rs:559-576); unknown registry row kinds ignored by an older reader.
- `CREATE_CHAT_WORKTREE`-style creation has a non-retargeting mode (params with no chat id leave the row untouched).

**M0 spikes to close before committing to M3**
- S1: prototype `validate_execution_binding` against the section B fixtures (proves "no new Git implementation").
- S2: run Claude Code and Codex with cwd = superproject root and a bound member worktree under `.worktrees/`: confirm write + commit succeed under workspace-write and what escalation appears when the worktree is outside the root.
- S3: diff-sync with three checkout entries for one Chat (CPU/watcher cost with `MAX_WATCH_DIRS`, diff_sync.rs:51-64, 490-514).
- S4: confirm the submodule `git worktree list` main-path quirk on the minimum supported git version; use `git-dir == common-dir` regardless.

## 8. Open decisions for the spec owner

1. Offline policy (C4): block bound launches at the client when the owning device is dark, or queue and let the host reject.
2. Canonical worktree flow for bound creation (C1), including whether `CREATE_CHAT_WORKTREE` becomes forwardable.
3. Whole-row vs field-wise inheritance (C6).
4. Whether `OutsideExecutionRoot` is a blocking error or a warning, and whether bound launches may implicitly raise the sandbox.
5. Where the per-Chat snapshot lives (new row kind vs additive Chat fields) (C5).
6. Whether Layout B (non-git workspace root) is in the MVP; it needs a Space-level flag beyond `git_detected`.
