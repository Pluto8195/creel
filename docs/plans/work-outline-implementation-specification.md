# Zeron Work Outline — Implementation Specification

## Summary

Add a persistent work outline above Zeron's existing chats, execution sessions, repositories, and Git worktrees.

The goal is to let users organize work hierarchically and define instructions once at the appropriate level. Descendant work automatically receives those instructions, so users do not have to repeat project rules, workstream details, or decisions in every chat.

This is intentionally smaller than Tod. Zeron should gain Tod's useful organizational layer without becoming a workflow engine.

## Problem

Zeron already handles execution well:

- Users start chats against repositories and Git worktrees.
- Agents implement, investigate, review, test, and coordinate work.
- Skills and repository instructions provide reusable behavior.
- Work is commonly associated with a ticket, pull request, service, or product area.

The missing layer is persistent structure across chats. Related chats are currently independent, so users repeatedly provide the same instructions and must remember:

- Which chats belong to the same work item.
- Which repository, branch, and Git worktree the work uses.
- Which instructions apply to an entire project or workstream.
- Which active agents are blocked on a user decision.
- Which useful conclusions should survive after a Chat ends.

## Product outcome

A user can structure work like this:

```text
Cofactr
├── Flagship
│   ├── Flagship instructions
│   └── ENG-2967
│       ├── Linear ticket
│       ├── Branch and Git worktree
│       ├── Implementation chat
│       └── Review chat
└── UI
    ├── Frontend instructions
    └── Table redesign
```

Starting a chat under `ENG-2967` automatically supplies the effective instructions inherited from `Cofactr`, `Flagship`, and `ENG-2967`, then opens with the item's exact execution configuration.

## Scope

The minimum coherent product consists of five features:

1. Nested work items.
2. Inherited instructions.
3. Execution configuration.
4. A four-state user-action model.
5. User-approved creation of durable instructions from Chats.

## Zeron domain and implementation contracts

These contracts resolve terms that are otherwise easy to confuse with harness or storage concepts.

### Durable and transient identities

- **Chat** is the durable, user-facing conversation and the unit associated with a work item.
- **Execution Session** is a bounded harness context that shares a native resume identity, harness, execution root, and applied instruction revision. It is not the Work Outline ownership unit.
- **Run** is one local process or remote invocation within an Execution Session.
- **Turn** begins with one user prompt and ends when the corresponding work settles, requests input, or fails.
- **Execution State** is transient operational state such as Working, Awaiting Input, Idle, or Error. It is not a work-item lifecycle state.

Use **Chat** throughout user-facing Work Outline language. Use **Execution Session** only when describing native harness continuity or runtime delivery.

### Ownership and project boundary

For the MVP, a Work Outline is owned by an existing Zeron **Space**. A Space is the synced identity of a `(device_id, absolute folder)` pair; no hidden `Project` entity is introduced.

- The same repository opened at another path or on another device is a different Space and therefore has a different outline.
- A multi-Space overview renders each Space as a separate root.
- A projectless chat remains unassociated until it is moved into a Space through an existing supported flow.
- Associating a Chat with a work item never retargets the Chat's `space_id`, device, execution root, current working directory, repository focus, or checkout.
- Work items, parents, Chat associations, instructions, action requests, and execution configuration must remain inside one Space.
- Space-root instructions are instructions whose scope is the Space root; they do not require a synthetic root work item or another ownership entity.

### Persistence and convergence

Persist Work Outline entities as additive row kinds in Zeron's existing profile-scoped HLC/LWW registry. Do not introduce SQL tables or place outline metadata inside transcript Loro documents. Publish one composed `WatchWorkOutline` snapshot for a Space.

- IDs are stable, client-generated, and safe to retry.
- Archival uses durable archive fields; permanent deletion uses tombstones.
- Older clients ignore unknown row kinds without damaging them.
- Sibling order uses a stable fractional order key with the record ID as a deterministic tie-breaker; integer positions are not sufficient under concurrent edits.
- Parent changes validate Space ownership and cycles locally, but reads must also deterministically repair or quarantine cycles and cross-Space parents created by offline concurrent writes.
- Creating a Chat, association, instruction revision, and queued first turn is an idempotent multi-step operation with recoverable partial-failure states.

### Instruction delivery

Work-item instructions are runtime configuration, not visible prompt text. `RunRequest` must carry an instruction/context envelope separate from the user's transcript prompt. Each harness adapter must either deliver the exact immutable revision or reject the operation with a capability error; it must never silently omit instructions.

- A queued command captures the revision ID and content it will use.
- A fresh Execution Session receives the revision before its first turn.
- A warm or natively resumed Execution Session keeps its applied revision until the user explicitly applies another one.
- Applying a revision occurs at a turn boundary. If the harness cannot update runtime instructions safely, Zeron starts a compatible Execution Session rather than injecting the change as ordinary user text.
- Crash recovery and retries must not apply a revision twice or substitute the newest revision for the captured one.
- Remote hosts advertise Work Outline instruction-envelope capability; unsupported hosts are rejected with a clear upgrade path.

### Execution scope

Execution configuration separates:

- The execution root and writable workspace.
- An optional primary repository focus.
- Zero or more member repositories included in diff and repository context.
- An optional exact Git worktree or checkout for each configured repository.

This permits one Chat rooted at a workspace such as Cofactr to reason about both `ui` and `flagship` while retaining repo-specific branch and diff behavior. Missing, offline, moved, or pruned configured paths block launch or resume; Zeron must never silently fall back to another checkout.

## User-experience requirements

These requirements are based on established Zeron interaction patterns and the configured interface behavior used for agent work. Implement them through Zeron's existing settings and presentation components rather than hard-coding one appearance.

### Preserve the Chat-first workflow

The Work Outline supplements Chats; it does not become the primary execution interface.

- Keep the existing compact Chat sidebar available and unchanged by default.
- Do not require a work item before starting a Chat.
- Automatically associate a Chat started from a work item.
- Keep Chats started elsewhere unassociated until the user or agent explicitly associates them.
- Display the associated work-item path as compact Chat metadata alongside the existing Space, harness, branch, and pull-request metadata.
- Opening an associated Chat must return directly to its transcript and execution environment.

### Integrate with the existing overview

> 2026-10-23: Canvas and chat classification were removed; stored positions and classification rows are kept but no longer read or written. See tag pre-canvas-removal. The Overview now has List and Outline only; a saved Canvas setting loads as Outline.

Add **Outline** as a third overview presentation beside the existing **List** and **Canvas** presentations.

- Persist the selected overview presentation as a tolerant enum or string and migrate the existing List/Canvas boolean without resetting user choice.
- Never modify saved Canvas positions when a Chat is associated with, moved between, or removed from a work item.
- Show the work-item path and `User Action Required` state on existing List rows and Canvas cards without changing their primary content.
- Allow filtering existing List and Canvas presentations by work item.
- In Outline, render Space roots, nested work items, and associated Chats in a compact hierarchy.
- When more than one Space is visible, render each Space as a separate collapsible root.
- Place Chats without an association under **Unassociated Chats**; do not hide them.

The Outline presentation is a different organization of existing Chats, not a separate task-management application.

### Use compact, information-dense rows

The configured Zeron sidebar favors compact rows with visible operational metadata. Outline rows should follow the same pattern.

- Use one primary line for the work-item title.
- Use indentation and disclosure controls to communicate hierarchy.
- Show only concise secondary indicators: status, unresolved user-action count, associated-Chat count, and external-reference presence.
- Reuse existing branch, pull-request, harness, Space, and attention indicators for Chat rows.
- Do not use large cards, decorative illustrations, progress rings, or permanent instructional text.
- Hide completed work items by default when stale Chats are hidden, with an explicit control to reveal them.

### Apply progressive disclosure

The transcript remains the dominant working surface. Work-item administration should consume space only when requested.

- Create and rename work items inline.
- Keep descriptions, instructions, external references, and execution configuration collapsed until opened.
- Open **Work Item Details** in an Overview-owned side surface rather than reusing the Chat-keyed transcript and diff pane.
- Restore the previous transcript layout when Work Item Details closes.
- Show **Effective Instructions** automatically only before starting a Chat when instructions changed, or when the user explicitly opens them.
- Require a focused drift confirmation when associating or moving a Chat, toggling inheritance, editing or archiving an ancestor instruction, moving an item, or changing execution configuration would alter current effective instructions. Existing Chats retain their applied revision until explicitly updated.
- Do not introduce a setup wizard or a multi-page item editor.

### Support keyboard operation

Every primary Work Outline operation must be available from the command palette and the configurable keybinding system.

Required commands:

- **Open Outline**
- **Create Work Item**
- **Create Child Work Item**
- **Rename Work Item**
- **Move Work Item**
- **Associate Chat with Work Item**
- **Open Effective Instructions**
- **Create Instruction from Chat**
- **Open Required Actions**

Support keyboard navigation among parent, child, and sibling work items. Register real command and shortcut identifiers, migrate settings tolerantly, account for platform modifiers and shortcut conflicts, and do not fire outline shortcuts while a text field owns focus.

### Require explicit persistence for durable instructions

Durable instructions affect future agent behavior and must not be saved implicitly.

- Provide explicit **Save** and **Cancel** actions when editing an instruction.
- Do not save instruction edits when focus changes or the details surface closes.
- Show the target work item and affected descendant count before saving a Chat-derived instruction.
- Keep agent proposals nonmodal until the user chooses to review them.

### Reuse existing attention behavior

User action requests should participate in Zeron's existing attention system.

- Respect the existing notification, background-notification, and attention-sound settings.
- Coalesce multiple requests from the same work item into one notification event.
- Keep the complete requests in **Required Actions** even when notifications are disabled.
- Use the existing attention visual treatment instead of introducing another alert color or badge system.

### Respect existing appearance and link preferences

- Inherit the active theme, accent, surface treatment, typography, font size, and motion settings.
- Maintain readable hierarchy in both compact and standard-density presentations.
- Open external references according to Zeron's existing web-link preference.
- Extract or adapt a generic text-comparison presentation for instruction revisions. The current Git Changes comparison is checkout-bound and must not be treated as a generic instruction diff.

### Differences from Tod

Zeron should adopt Tod's hierarchical instruction inheritance without reproducing Tod's product structure.

- Chats remain the center of the interface; the outline organizes them.
- Work-item creation is inline and immediate rather than a structured intake procedure.
- Repository, branch, Git worktree, harness, and pull-request information should be inferred from associated Chats whenever possible.
- Advanced lifecycle phases, evaluators, plans, obligation editors, and capability configuration are absent.
- The user sees operational state and required actions, not an additional workflow dashboard.

## Core concepts

### Work item

A work item is an entry in a Space's Work Outline. It may represent a product area, workstream, ticket, investigation, pull request, or any other useful scope. The outline is the hierarchical presentation of a Space's work items and is not a separately persisted entity in the MVP.

A work item contains:

- Title.
- Optional description.
- Parent item.
- Ordered children.
- Status.
- Instructions.
- External references, such as Linear tickets and GitHub pull requests.
- Optional execution configuration.
- Associated Zeron Chats.
- Open and resolved user action requests.

There is deliberately no separate `list` type. A work item may contain child items and may also have associated Chats.

### Work item instruction

A work item instruction is a durable directive or fact attached to a work item. Examples:

- "Use targeted tests before running the full suite."
- "Flagship is a Django service."
- "Do not change the public API in this ticket."
- "The user prefers a short implementation summary followed by risks."

Instructions retain provenance: who created them, when, and whether they were created from a Chat excerpt. Excerpt provenance includes the Chat and message identifiers plus a copy and hash of the selected source text, because transcript content may later be compacted or unavailable.

### Execution configuration

An execution configuration defines the environment in which a work item's Chats run:

- Writable execution root.
- Optional primary repository focus.
- Member repositories included in navigation, repository context, and diffs.
- Optional exact Git worktree identity and path per repository.
- Expected branch or revision information.
- Optional remote or environment identifier if Zeron already supports one.

### User action request

A user action request identifies a specific question, decision, approval, or action that an agent requires from the user. It is not a generic notification.

A user action request is durable and separate from the transient `Awaiting Input` Execution State. It contains:

- A concise question or requested decision.
- The originating work item and Chat, with optional message or tool-call identity.
- Optional supporting detail.
- A deduplication key, resolution provenance, and created and resolved timestamps.

## Functional requirements

### 1. Nested work items

Users can:

- Create a root item or child item.
- Rename and edit an item.
- Reorder siblings.
- Move an item to a different parent.
- Archive or complete an item.
- Associate and disassociate existing Chats.
- Start a new Chat from an item.
- Add external references.
- See breadcrumbs and immediate children.

The Work Outline is accessible through the **Outline** overview presentation. It does not replace the current Chat list; Chats appear beneath work items while remaining available through existing Chat navigation.

Moving an item can change inherited instructions. Before completing the move, the user interface must show the revision drift for the item and any descendants with associated Chats. Those Chats keep their applied revision until the user explicitly applies another one.

Deleting a parent must never silently delete descendants or Chats. The MVP should use archive, with an explicit choice to archive descendants or move them elsewhere. Completing or archiving a work item does not archive Chats, close worktrees, delete branches, or clean repositories.

Moves and reorders are expressed relative to a neighboring sibling or boundary, not an integer position. Concurrent offline writes must converge to one deterministic, acyclic, Space-local hierarchy.

### 2. Inherited instructions

When a Chat starts from a work item, runtime context has two layers. The existing harness resolves global preferences, repository instruction files, and applicable skills. Zeron separately resolves the immutable Work Outline instruction envelope in this order:

1. Space-root instructions.
2. Inherited work-item instructions from the active boundary down to the current item's parent.
3. Instructions defined directly on the current item.

The user's current prompt remains separate transcript content and follows both runtime-instruction layers.

Within the Work Outline envelope, later and more specific work-item instructions take precedence. Conflicts with global preferences, repository instructions, skills, or runtime protections follow the existing harness precedence rules; a Work Outline instruction cannot weaken them. The MVP does not attempt semantic conflict detection across plain-text instructions.

The item user interface must provide an **Effective Instructions** view that shows:

- The ordered instructions supplied to a Chat.
- The source item for every instruction.
- Which instructions are local versus inherited.
- The precedence rule applied to inherited and local instructions.

Each item may disable parent work-item inheritance. At that item, all ancestor work-item instructions are reset and local instructions begin a new boundary. Descendants inherit from that boundary unless they opt out again. This does not disable Space-root policy, global policy, repository safety instructions, or runtime protections.

Do not create a structured obligation or constraint language in the MVP. Instructions are plain text with provenance and ordering. Enforce configured limits for instructions per item, effective rendered context, and immutable revision size.

#### Instruction revisions

Resolved Work Outline instructions are recorded as an immutable revision when a Chat starts or when the user explicitly applies updated instructions. Each entry stores its instruction ID, source kind (`space_root` or `work_item`), owning scope ID, exact text, provenance, and order. The revision also stores the canonical rendered Work Outline envelope and its content hash. Global preferences, repository files, and skills remain authoritative harness inputs and are not copied into this revision. A later instruction edit or archive never rewrites an existing revision.

If inherited instructions change, an existing Chat displays:

> Instructions changed since this Chat's applied revision.

The user can inspect the diff and explicitly apply the new revision to subsequent Turns. New Chats use the latest revision captured at launch. Append-only revision-application records preserve exactly which revision reached which Chat, Turn, or Execution Session.

### 3. Execution configuration

A Space can define a default execution root. A work item may define a more specific execution configuration.

When starting a Chat:

- Resolve the nearest applicable execution root, primary repository focus, member repositories, and exact worktree configuration.
- Validate the saved device, Space, paths, repositories, and worktree identities before launch or resume.
- Never create or switch a branch silently.
- If no Git worktree is configured, offer to use the Space workspace or create or select a Git worktree through Zeron's existing flow.
- If configured resources are missing, offline, moved, or pruned, block with an actionable error rather than selecting another checkout.

All Chats launched from an item use the same execution configuration by default. The user may deliberately create a Chat with a different configuration, but Zeron should show that exception on the item. Association alone never mutates an existing Chat's configuration.

This feature should reuse Zeron's existing repository, branch, and Git worktree models rather than introduce a second Git implementation.

### 4. Status and user actions

Work items have exactly four user-facing states:

- **Pending** — recorded but not started.
- **Active** — work is in progress.
- **User Action Required** — at least one unresolved user action request blocks progress.
- **Completed** — work is complete.

Avoid additional lifecycle states in the MVP.

`User Action Required` is an effective state derived from unresolved user action requests. The stored base status remains `Pending`, `Active`, or `Completed`. While a request is unresolved, the interface presents `User Action Required`; resolving the final request reveals the unchanged base status. Completing an item with unresolved requests requires confirmation and records an explicit resolution for each request; changing the base status alone never erases request history.

The work outline shows descendant status totals, for example:

```text
Flagship   3 active · 1 user action required
```

Zeron provides Space-level and global **Required Actions** views. Each row must show the requested action, work item, originating Chat, and age. Opening a request takes the user directly to the relevant Chat when available, while requests whose Chat was archived or deleted remain inspectable. Notify on the transition from zero to one unresolved request and deduplicate retries.

### 5. Create instructions from Chats

From a Chat, the user can select content or accept a durable agent proposal and choose:

- Create instruction for this item.
- Create instruction for the parent workstream.
- Create instruction for the Space root.

Before saving, Zeron presents editable text, the target scope, and a preview of affected descendants.

Agents may propose durable instructions but may not silently add or alter them. Creating an instruction from a Chat requires user approval.

The saved entry records its originating Chat and message, the copied selected text, and a content hash so the user can inspect why it exists even if the source transcript later changes or disappears. Proposals have durable pending, accepted, and rejected outcomes.

## Agent capabilities

Agents operating inside a work item should receive origin-aware, idempotent, narrowly authorized tools to:

- Read the current item and effective instructions.
- Create a child item.
- Associate the current Chat with an item.
- Mark an item active or completed.
- Create and resolve a user action request.
- Propose a work item instruction for user approval.

Agents must not silently:

- Change inherited instructions.
- Move or archive a work-item hierarchy.
- Delete items or Chats.
- Reconfigure an item to use another repository or Git worktree.
- Create or switch Git branches outside the existing approved Zeron flow.

Agent-created work items should indicate which Chat created them. A Zeron-created child Chat inherits the originating work item and applied revision by default. Harness-native subagents remain transcript artifacts and do not become durable Zeron Chats unless Zeron explicitly creates them.

## Recommended data model

Persist Work Outline metadata as additive row kinds in the profile's existing `RegistryRoom`. Do not introduce SQL tables and do not store outline metadata in transcript Loro documents.

Registry rows use Zeron's existing HLC/LWW merge behavior, stable client-generated identifiers, and tombstones. Older clients must safely ignore unknown row kinds. A composed `WatchWorkOutline` snapshot materializes the hierarchy and related records for one Space.

The MVP owner is the existing Zeron Space: a synchronized `(device_id, absolute folder)` scope. `space_id` is immutable after creation. The same repository opened on another device is a different Space and therefore has a different outline. Projectless Chats remain unassociated.

### `work_item`

Each row contains:

- Stable `id` and immutable `space_id`.
- Nullable `parent_work_item_id`.
- Title and optional description.
- Base status: `Pending`, `Active`, or `Completed`.
- Nullable `archived_at` and archive provenance; archived items are excluded from default views but remain restorable and addressable.
- Stable `order_key` for sibling placement.
- `inherit_parent_instructions`, defaulting to true.
- Creator provenance and lifecycle timestamps.
- A tombstone reserved for permanent deletion and synchronization.

Use the row identifier as the deterministic tie-breaker when sibling order keys collide. Completing or archiving a work item must not archive associated Chats, close worktrees, change Chat classification, or alter Canvas positions.

Write-time validation rejects known cycles and cross-Space moves. Because offline concurrent LWW updates can still create invalid structures, materialization treats a missing or cross-Space parent as no parent and emits a repair marker. For each cycle, it removes the derived parent edge whose source row has the greatest `(updated_hlc, work_item_id)` tuple. This repair is deterministic, preserves immutable Space ownership, and remains derived until an explicit repair mutation is persisted.

### `work_item_instruction`

Each row contains:

- Stable `id` and immutable `space_id`.
- Nullable `work_item_id`; null identifies a Space-root instruction without introducing a separate Project entity.
- Instruction text and stable `order_key`.
- Creator provenance.
- Optional source `chat_id` and `message_id`.
- A copied source excerpt and content hash when created from a Chat.
- Lifecycle timestamps, nullable `archived_at`, and archive provenance.
- A tombstone reserved for permanent deletion.

Copied source content preserves provenance when transcript messages are compacted, removed, or unavailable. Per-item, effective-context, and revision-size limits must prevent unbounded registry rows or runtime prompts.

When `inherit_parent_instructions` is false, that work item begins a new inheritance boundary: ancestor work-item instructions are excluded, local instructions are applied, and descendants inherit from that boundary unless they establish another boundary. Space-root instructions always apply and are not disabled by a work-item inheritance boundary.

### `work_item_chat_assignment`

Use one LWW assignment row keyed by `chat_id`. It contains the immutable Chat `space_id`, nullable `work_item_id`, assignment provenance, and assignment time. A null work item represents explicit disassociation.

A Chat has at most one primary work item in the MVP. Assignment does not retarget the Chat's Space, device, execution root, current directory, worktree, harness, native resume identity, classification, or Canvas position.

The associated durable object is the Chat. An Execution Session is transient harness continuity for that Chat and must not be used as the Work Outline association key.

Archived Chats retain their assignment and follow existing stale-content filters. If a Chat is deleted or unavailable, Work Item Details renders an inspectable dangling assignment until the user explicitly removes it. Chat parent-child relationships do not imply Work Outline hierarchy; only a Zeron-created child Chat inherits its creator Chat's assignment by default. Projectless Chats cannot be assigned.

### `execution_configuration`

Each configuration row has exactly one scope: either a `space_id` for the Space default or a `work_item_id` plus its immutable owning `space_id` for an override. A scope has at most one live configuration row. The row contains a writable execution-root path, repository bindings, update provenance, and an optional tombstone. Every repository binding contains repository identity and path, membership, whether it is primary, an optional exact Git worktree identity and path, and expected branch or revision. At most one binding is primary.

Execution root, repository focus, member repositories, and worktree identity are separate concerns. This supports a workspace repository containing repositories such as `ui` and `flagship` without forcing separate Chats.

Starting or continuing work must validate that the Space, paths, repositories, and exact worktree still exist. Missing, offline, moved, or pruned resources block execution; Zeron must never silently fall back to another checkout.

### `work_item_external_reference`

Each row stores the work item, provider, reference type, external identifier, URL, optional cached title, provenance, and tombstone.

Reuse existing PR and ticket metadata, caching, provenance, and web-link preferences where possible. Work-item references remain independent from manual Chat PR links such as `ChatPrLink` and must not overwrite them.

### `user_action_request`

Each durable request contains:

- Stable `id`, `space_id`, and `work_item_id`.
- Originating `chat_id` and optional message or tool-call identifier.
- Concise requested action and optional supporting information.
- Optional deduplication key.
- Open or resolved state, resolution provenance, and timestamps.

These rows represent durable work-item actions and are distinct from transient Execution State such as Awaiting Input. Notification behavior should react to the transition from zero to one unresolved request, not repeatedly notify for equivalent open requests. Requests remain inspectable if their originating Chat is later archived or unavailable.

### `work_item_instruction_proposal`

Each durable proposal contains its target scope, proposed text, proposing Chat or agent provenance, optional source message or tool-call identifier, review state, reviewer provenance, and timestamps.

Approval creates a new instruction from the final reviewed text and records its identifier. Supporting modification proposals later requires an explicit target instruction ID and expected version. Rejection preserves the proposal audit record.

### `instruction_revision`

An instruction revision is immutable and contains:

- Stable `id`, `space_id`, and `work_item_id`.
- The exact ordered instruction snapshot.
- For every entry: instruction identifier, source kind, owning scope identifier, exact text, provenance, and order.
- The canonically rendered Work Outline instruction envelope.
- A hash over the canonical representation.
- Creation provenance and timestamp.

Global preferences, repository files, and skills are authoritative harness inputs but are not copied into the Work Outline revision.

Create a revision when a Chat is launched for a work item or when the user explicitly applies updated instructions. Editing source instructions never mutates an existing revision.

### `instruction_revision_application`

Use immutable application rows to preserve history rather than storing one revision pointer on the Chat assignment. Each application row stores the Chat, revision, target Execution Session, effective Turn boundary, application reason, idempotency key, result, HLC timestamp, and stable ID.

The Chat's selected revision is the latest successful launch or explicit application by `(applied_hlc, application_id)` and is the default for future Execution Sessions. Every active Execution Session separately reports the revision it actually received; the UI must not infer session delivery solely from the Chat selection. Existing Chats retain their selected revision until the user explicitly applies another one.

### Materialization and compatibility

`WatchWorkOutline(space_id)` should return one composed snapshot containing the repaired hierarchy, effective ordering, Chat assignments, execution configurations, external references, unresolved action counts, instruction proposals, and current revision state.

All multi-row workflows use client-generated operation identifiers so retries after crashes or partial synchronization converge without duplicate Chats, revisions, requests, or initial prompts. Tombstones must converge across devices and profiles must remain isolated.

## Service operations

The implementation should expose equivalent operations through Zeron's existing command and application-service boundaries:

- `watchWorkOutline(spaceId)`
- `createWorkItem(spaceId, parentWorkItemId, placement, definition, operationId)`
- `updateWorkItem(workItemId, changes, operationId)`
- `moveWorkItem(workItemId, destinationParentWorkItemId, placement, operationId)`
- `archiveWorkItem(workItemId, descendantArchivePolicy, operationId)`
- `createWorkItemInstruction(scope, instructionDefinition, operationId)`
- `updateWorkItemInstruction(instructionId, changes, operationId)`
- `archiveWorkItemInstruction(instructionId, operationId)`
- `resolveEffectiveInstructions(workItemId)`
- `createInstructionRevision(workItemId, operationId)`
- `compareInstructionRevisions(sourceRevisionId, targetRevisionId)`
- `applyInstructionRevision(chatId, revisionId, operationId)`
- `assignChatToWorkItem(chatId, workItemId, operationId)`
- `removeChatFromWorkItem(chatId, operationId)`
- `resolveExecutionConfiguration(scope)`
- `configureExecution(scope, executionConfiguration, operationId)`
- `createChatForWorkItem(workItemId, initialPrompt, operationId)`
- `continueChatForWorkItem(chatId, prompt, operationId)`
- `createWorkItemExternalReference(workItemId, reference, operationId)`
- `removeWorkItemExternalReference(referenceId, operationId)`
- `createUserActionRequest(workItemId, origin, requestDefinition, operationId)`
- `resolveUserActionRequest(userActionRequestId, resolution, operationId)`
- `proposeWorkItemInstruction(targetScope, origin, instructionText, operationId)`
- `approveWorkItemInstructionProposal(proposalId, approvedInstructionText, operationId)`
- `rejectWorkItemInstructionProposal(proposalId, reason, operationId)`

`placement` should identify a neighboring sibling or boundary rather than expose an incrementing integer sort position.

`createChatForWorkItem` is one recoverable, idempotent workflow: validate execution configuration and runtime capabilities, create the Chat, assign it, create the immutable instruction revision, deliver it with a successful launch application row, and queue the initial Turn. A retry must complete missing steps without creating duplicate durable objects or prompts.

Instruction context travels in a runtime instruction envelope separate from the visible transcript prompt. The queued command captures the exact revision identifier before dispatch. Each harness adapter must support fresh execution, native resume, crash recovery, and explicit revision application without silently converting instructions into user text.

Applying a revision while an Execution Session is active takes effect at a defined Turn boundary or requires an explicit restart. Remote hosts that do not advertise the required instruction-envelope or multi-repository capabilities must reject the operation with an actionable error instead of omitting configuration.

All mutations enforce authorization from their originating Chat or agent, Space ownership, idempotency, and immutable-field rules. Agent-created child Chats inherit the parent work item and applied revision by default. Harness-native subagents remain transcript artifacts unless Zeron explicitly creates a durable Chat for them.

## Primary user-interface flows

### Create and start work

1. User opens a Space.
2. User creates a child item or starts from a Pending item.
3. User optionally adds a Linear or GitHub external reference.
4. User selects an existing Git worktree or creates one through the existing flow.
5. User reviews the effective instructions.
6. User starts a Chat.
7. Zeron idempotently records the association and exact instruction revision before queueing the first Turn.

### Resume work

1. User selects the item from the work outline.
2. Zeron shows associated Chats, execution configuration, external references, status, and effective instructions.
3. User reopens an existing Chat or starts another with the validated execution configuration.

### Agent requests user action

1. Agent creates a user action request containing one concrete action or question.
2. The item and its ancestors display a `User Action Required` status total.
3. User opens the request and lands in the originating Chat when it is available.
4. User answers or resolves it.
5. The item returns to its stored base status when no unresolved user action requests remain.

### Create an instruction from a Chat

1. User selects a conclusion in a Chat, or the agent creates a durable proposal.
2. User chooses the target item.
3. Zeron shows editable text and affected descendants.
4. User approves.
5. Zeron saves a provenance-linked work-item instruction. Existing Chats keep their applied revision; a new immutable revision is created only at Chat launch or explicit apply.

## Acceptance criteria

The MVP is complete when all of the following are true:

1. A user can create, rename, archive, move, and deterministically reorder a three-level Work Outline within a Space.
2. Concurrent offline moves and reorders converge without data loss; materialization deterministically repairs cycles and cross-Space parents.
3. Multiple Chats can be associated with one work item, and a Chat can be associated, disassociated, or moved without changing its `space_id`, device, execution root, checkout, harness resume identity, classification, or Canvas position.
4. Projectless, archived, deleted, parent-child, and dangling Chats have explicit behavior and never disappear silently.
5. Effective Instructions are composed in root-to-child order and identify each instruction's source and provenance.
6. Setting `inherit_parent_instructions` to false removes all ancestor work-item instructions at that boundary; descendants inherit from the boundary unless they also opt out.
7. Creating a Chat or explicitly applying updated instructions records an immutable, ordered revision containing Work Outline instruction IDs, owning scope IDs, exact text, provenance, rendered context, and a content hash.
8. Editing or archiving an instruction does not mutate an existing Chat's applied revision. The user can compare the applied and current revisions and explicitly apply the newer revision.
9. Instruction delivery is separate from the visible prompt and is verified for a fresh Run, a subsequent Turn in a warm Execution Session, queued Turn, crash recovery, native resume, and explicit revision application.
10. An instruction revision captured when work is queued is the exact revision delivered when that work starts.
11. Applying a revision while a Chat is working follows an explicit turn-boundary policy; it is never silently inserted into an active Turn.
12. Every supported harness has an instruction-delivery adapter and capability declaration. An incompatible local or remote host rejects the operation instead of silently omitting context.
13. A work item can define an execution root, optional primary repository focus, member repositories used for context and diffs, and an optional exact Git worktree for each configured repository.
14. Starting or resuming work validates the saved device, Space, execution root, member repositories, and worktree. Missing, offline, moved, or pruned targets block with an actionable error and never fall back to another checkout.
15. Multi-repository work can keep one execution root while selecting a primary repository and showing branch and diff state per member repository.
16. Chat creation from a work item is idempotent across Chat creation, association, revision capture, and prompt queueing; retrying after partial failure does not duplicate records or Runs.
17. Durable work-item action requests are distinct from transient Execution State, retain origin Chat/message/tool-call provenance, deduplicate retries, preserve resolution history, and handle a missing originating Chat.
18. Required Action counts propagate to ancestors, and notifications fire only on the zero-to-nonzero transition while respecting existing notification and sound settings.
19. Agent mutation tools are origin-aware, narrowly authorized, and idempotent. Agent-created child Chats inherit the originating work item and applied revision by default.
20. Harness-native subagents remain transcript artifacts and are not promoted to Zeron Chats.
21. A user can create an instruction from a selected Chat excerpt or approve an agent proposal. Persisted provenance includes copied source text and a content hash in addition to Chat and message identifiers.
22. Agent proposals are durable records with explicit pending, accepted, and rejected outcomes; agents cannot directly persist instructions.
23. External references reuse existing pull-request, ticket, provenance, cache, metadata, and web-link-preference infrastructure without overwriting manually linked Chat metadata.
24. Existing unassociated Chats continue to work and remain available in the compact sidebar and under Unassociated Chats in Outline.
25. Outline is available beside List and Canvas through a tolerant overview-mode enum migration, and the selected presentation persists across launches.
26. Work Item Details is an Overview-owned side surface. Opening and closing it does not replace or corrupt the Chat-scoped transcript and diff pane.
27. Every primary outline operation is available through the command palette and configurable keybindings, including conflict handling, platform modifiers, and text-input focus behavior.
28. Instruction edits require explicit Save; Cancel or closing with an unresolved draft follows a tested discard-confirmation policy.
29. Personal multi-device sync converges across restarts, preserves tombstones, remains profile-isolated, and handles Space deletion and orphaned records deterministically.
30. Large outlines remain usable through virtualization, keyboard navigation, stable focus, and configured instruction and revision size limits.
31. Outline, List, Canvas, Work Item Details, revision comparison, and Required Actions use the active Zeron theme, typography, density, and attention indicators.

## Rollout plan

### Milestone 0 — Contracts and feasibility

- Fix the product vocabulary: Chat, Execution Session, Run, Turn, and Execution State.
- Confirm that a Space is the MVP ownership boundary and define projectless behavior.
- Specify registry row kinds, immutable ownership, tombstones, ordering keys, conflict repair, profile isolation, and capability negotiation.
- Prove instruction-envelope delivery and native-resume behavior for each supported harness.
- Prove multi-repository execution with a shared root, primary repository focus, member repositories, and exact worktrees per configured repository.
- Specify idempotency and recovery for Chat creation, association, revision capture, and prompt queueing.

No runtime-instruction promise ships until these contracts and spikes pass.

### Milestone 1 — Durable outline

- Additive work-item, association, reference, and settings records in the existing registry model.
- A composed `WatchWorkOutline` snapshot.
- Outline beside List and Canvas, with tolerant migration from the existing Canvas preference.
- Compact hierarchy with Space roots, work items, associated Chats, and Unassociated Chats.
- Work Item Details as an Overview-owned side surface.
- Create, rename, archive, move, reorder, associate, and disassociate operations.
- Command-palette actions and configurable keybindings.
- Concurrency, tombstone, restart, profile-isolation, and large-outline tests.

This release provides organization without claiming runtime instruction delivery.

### Milestone 2 — Instruction correctness

- Space-root and work-item instructions with explicit inheritance boundaries.
- Provenance from user edits and Chat excerpts.
- Exact immutable instruction revisions and append-only revision-application history.
- Per-harness instruction envelopes, capability checks, turn-boundary application, crash recovery, and native-resume tests.
- Effective Instructions and a generic instruction-revision comparison surface.
- Drift confirmation for association changes, inheritance changes, ancestor edits or archives, and Chat moves.
- Instruction and revision size budgets.

### Milestone 3 — Execution continuity

- Execution root, primary repository focus, member repositories, and exact-worktree configuration.
- Idempotent start and resume from a work item.
- Strict validation for device, Space, checkout, and worktree availability with no silent fallback.
- Reuse of existing pull-request, ticket, repository, cache, and web-link infrastructure.
- Work-item metadata and filters in List and Canvas.
- Explicit multi-repository branch and diff presentation.
- Drift confirmation for execution-configuration changes.

### Milestone 4 — Required Actions and restricted agent tools

- Durable work-item action requests and the global Required Actions view.
- Ancestor totals, notification deduplication, and resolution audit history.
- Durable instruction proposals and approval flow.
- Origin-aware, narrowly authorized, idempotent tools for child creation, status updates, action requests, and proposals.
- Default inheritance for Zeron-created child Chats.
- Clear separation between Zeron Chats and harness-native subagents.

## Migration and compatibility

- Store the feature as additive HLC/LWW registry row kinds; do not introduce a separate SQL store or place outline metadata in transcript Loro documents.
- Older clients ignore unknown row kinds. New clients tolerate missing fields and materialize deterministic defaults.
- Expose one composed `WatchWorkOutline` snapshot so callers do not reconstruct cross-row consistency independently.
- Migrate the persisted List/Canvas boolean to a tolerant overview-mode enum that supports List, Canvas, and Outline.
- Existing Chats remain unassociated and continue to work without migration.
- Users can manually associate prior Chats. Heuristic suggestions based on title, repository, branch, worktree, ticket, or pull request may come later but are not required for MVP.
- A Space is the ownership boundary: the same repository folder synced from another device remains a distinct Space and outline.
- Personal multi-device synchronization remains supported; multi-user collaborative editing does not.
- Registry data remains profile-isolated. When a Space is tombstoned, materialization suppresses all owned Work Outline rows. An idempotent cleanup operation emits tombstones for those rows; late offline writes remain suppressed by the owning Space tombstone. Recreating the folder with a new `space_id` never adopts the deleted Space's outline.
- Local and remote hosts advertise feature capabilities. Unsupported hosts reject instruction or execution operations with an actionable compatibility error.
- Repository instructions and skills remain authoritative runtime inputs; Work Outline instructions supplement them through the explicit instruction envelope.

## Explicit non-goals

Do not include these in the initial implementation:

- A hidden Project entity separate from the existing Space ownership boundary.
- Separate list and item object types.
- A multi-stage task lifecycle beyond the four states above.
- Plans, obligations, constraints, or evaluations as separate durable entity families.
- Autonomous instruction persistence without user approval.
- A new agent runtime, daemon, process manager, sandbox, transcript store, or Git worktree implementation.
- Multi-user real-time collaboration or shared-team permissions.
- Full Linear or GitHub issue synchronization.
- Dependencies between arbitrary work items.
- Time estimates, calendars, reminders, or recurring tasks.
- General-purpose automation rules.
- Automatic parent completion based on descendants.
- Automatic Chat archival, worktree closure, branch deletion, or repository cleanup when a work item is completed or archived.
- Promotion of harness-native subagents into standalone Zeron Chats.
- Automatic retargeting of an existing Chat to another device, Space, execution root, checkout, or Execution Session.

## Product guardrails

- Optimize for fewer repeated instructions and faster resumption, not more task administration.
- Keep creating a child item or associated Chat as fast as starting a Chat today.
- Treat Chats as durable user-facing conversations and Execution Sessions as bounded harness continuity; do not use the terms interchangeably in UI, storage, or APIs.
- Keep work-item association orthogonal to Chat classification, execution scope, worktree ownership, and Canvas layout.
- Make inheritance, provenance, rendered context, revision drift, and delivery state inspectable.
- Preserve user control over durable memory: edits require Save, agent proposals require approval, and existing Chats retain their applied revision until explicitly updated.
- Never silently change execution root, repository focus, checkout, worktree, device, or instruction revision.
- Never silently degrade when a host lacks required capabilities.
- Reuse Zeron's existing registry, Chats, skills, repository metadata, pull-request links, ticket links, Git worktrees, notifications, and web-link preferences.
- Bound instruction count, effective rendered size, revision size, and outline rendering cost.
- Completion and archival organize work; they do not archive Chats, close worktrees, delete branches, or clean repositories.
- If a feature does not strengthen organization, inspectable instruction reuse, execution continuity, or user-action routing, it is outside this scope.

## Success signals

After rollout, validate that:

- Users start multiple related Chats from the same work item.
- Space-root and workstream instructions are reused across descendant Chats.
- The amount of repeated setup text in first messages decreases.
- Users can resume ticket-oriented work from the Work Outline without locating the correct Chat manually.
- User action requests are resolved from the Required Actions view.
- Chat-derived instructions remain concise, reviewed, and actively reused.

## Implementation contract amendments

Milestone 0 resolved this specification against the existing code. The binding contract is
`work-outline/m0-contract.md`; it takes precedence where it differs from the text above. Summary of
amendments:

- Execution Session: any change to the applied revision starts a new Execution Session, even when
  the harness continues the same native transcript. The host reports the delivered revision on its
  session-status row; there is no separate Execution Session entity.
- `instruction_revision` stores ordered entries, renderer version, and hashes, not the rendered
  envelope text (registry ops are capped at 16 KiB). Delivery re-renders deterministically and
  verifies the hash. Budget: 9,000 effective characters and 32 entries per revision.
- `instruction_revision_application` rows record selection (written once by the client); actual
  delivery is reported separately by the host.
- Queued commands carry only the revision id and hash; the host waits for the exact revision and
  never substitutes another.
- Revision changes never swap instructions inside a live harness thread. Claude Code resumes with
  a replaced system-prompt file; Codex requires a proven compaction swap or a confirmed context
  reset.
- Cursor and ACP harnesses reject Work Outline instruction delivery with a capability error.
- A Work Outline instruction cannot be mechanically prevented from outranking repository
  instructions in Claude Code; the envelope states the precedence rule in a fixed preamble.
- A per-Chat `chat_execution_binding` row captures the resolved execution configuration at launch,
  so later configuration edits never retarget existing Chats. Resolution is whole-row, not
  field-wise.
- Cycle repair uses the HLC of the parent field, because registry rows have no row-level HLC.
- Non-Git workspace roots were initially out of scope; Milestone 3 allows a plain-folder execution
  root while still requiring every configured repository to be a Git checkout (see below).

Amendments from Milestones 1–4 (details in the discoveries sections of `work-outline/m0-contract.md`):

- Operation replay protection is durable per device: completed operation ids are recorded in the
  existing device-local processed-command ledger in the same transaction as the registry change.
- Codex cannot swap instructions inside a thread reliably (compaction swap passed 4 of 6 trials), so
  a Codex revision change requires the user to confirm a fresh session. Claude Code resumes the same
  conversation with the new revision.
- The engine pins a Chat's selected revision when a plain send is ingested, so every
  instruction-bearing send uses the instruction command kind and hosts without the capability reject
  it before anything is written.
- A work item with no effective instructions launches without a revision, so harnesses without
  instruction support can still start Chats from it.
- A worktree chosen at launch for a binding that pins none is not an execution exception.
- The user's home directory is refused as an execution root. Execution roots may be plain folders;
  every configured repository must be a Git checkout.
- A malformed Chat execution binding is treated as bound and invalid; it never reverts the Chat to
  unbound behavior.
- Agent tools are served by an injected `zeron-agent` MCP server authenticated by a per-Run token.
  With a token, the pre-existing Zeron MCP tools are scoped to the caller's Space, device, and
  Zeron-created children, never exceed the caller's sandbox, and never enable auto-approve.
- An agent may associate its own Chat only while unassociated or within its current item's subtree,
  never for an execution-bound Chat, and resolving a deduplicated request group closes only the
  requests its own Chat raised.
- Residual risks accepted for the MVP: the loopback engine IPC remains unauthenticated for its
  general methods; registry tombstones are garbage-collected after 30 days; outline rows are not
  copied by local-to-synced profile import; the edge should be deployed before a synced profile
  writes outline rows.
