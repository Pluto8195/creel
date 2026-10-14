# Linear integration v1 — wire contract

Wire contract commit: `82112ea8` (branch `linear-engine`). Plan: `parked-proposals.md`
("Linear integration"). Code: `crates/proto/src/linear.rs` (+ tests),
`crates/proto/src/work_outline.rs` (Work Outline additions), `crates/rpc/src/lib.rs`
(`methods::LINEAR_*`). Capability: `linear-v1` (`capabilities::LINEAR_V1`).

## Decisions

- Linear is OFF by default and enabled per Space. A Space with Linear off makes no Linear calls
  for its items and shows no Linear UI; pasted Linear URLs stay plain links.
- Nothing in the Outline is created automatically. Items are created only by user actions.
- Issue-linked item title: `"ENG-123 · <issue title>"` (`linear_item_title`); description =
  Linear's description cut to the description limit (`linear_item_description`).
- Title and description of an issue-linked item FOLLOW Linear until the user edits that field
  (Tod's modified-fields rule). Status, parent, order, archive state, and children never change
  because of Linear.
- Agents get the full ticket (including description) in `work_outline_read_current` when the
  Space has Linear on. The key never reaches clients, agents, MCP, or harness environments.
- Nothing is written to Linear.

## RPCs (local/IPC, never forwarded)

| Method | Params | Reply |
|---|---|---|
| `LinearStatus` | `{forceRefresh?}` | `LinearConnectionStatus {configured, source: keychain\|environment\|none, viewer?, error?}` |
| `LinearSetApiKey` | `{apiKey}` | `LinearConnectionStatus` (validates with `viewer {id name email}` first; stores in Keychain and reads back) |
| `LinearClearApiKey` | `{}` | `LinearConnectionStatus` |
| `LinearIssue` | `{identifier, forceRefresh?}` | `LinearIssueSummary` (5 min cache per identifier) |
| `LinearMyIssues` | `{}` | `[LinearIssueSummary]` assigned to the viewer, state type not completed/canceled, max 250 |
| `LinearMyProjects` | `{}` | `[LinearProjectSummary]` viewer is lead or member, not completed/canceled |
| `LinearProjectIssues` | `{projectId}` | `[LinearIssueSummary]` open issues of the project, max 250 |
| `LinearRefreshLinkedItems` | `{spaceId}` | `{updated}` (manual refresh of linked items; bypasses cache) |

Errors are strings `linear/<code>: <message>` (`LinearError::from_wire_message`), codes
`missingKey`, `unauthorized` (the host already deleted the stored key: prompt again),
`rateLimited`, `notFound`, `network`, `disabledForSpace`, `invalid`.

Account-wide methods (`Status`, `SetApiKey`, `ClearApiKey`, `Issue`, `MyIssues`, `MyProjects`,
`ProjectIssues`) work in any Space; the UI only shows them in Linear-enabled Spaces.

## Types (`zeron_proto::linear`, camelCase JSON)

- `LinearUser {id, name, displayName?, email?}`
- `LinearState {id, name, type, color}`; `type` is `triage|backlog|unstarted|started|completed|
  canceled`, unknown reads as `unknown`.
- `LinearLabel {name, color}`, `LinearTeamRef {id, key, name}`, `LinearProjectRef {id, name, url}`
- `LinearIssueSummary {id, identifier, title, description?, url, state, assignee?, priority (0-4),
  priorityLabel, team, project?, labels, branchName?, updatedAt}`
- `LinearProjectSummary {id, name, url, state?, lead?, openIssueCount?, updatedAt}`
- `LinearIssueLink {id, identifier, title, description?, url}` (`From<&LinearIssueSummary>`)

## Work Outline additions

- Registry kind `workOutlineSpaceSettings`: row id = Space id, field `linearEnabled` (bool,
  default false, LWW, clock-observed). Snapshot: `spaceSettings: {linearEnabled}` (defaults when
  absent).
- `WorkOutlineMutate` ops:
  - `setSpaceLinearEnabled {spaceId, enabled, operationId}` → `{}`. Enabling triggers a sync of
    the Space's linked items.
  - `createWorkItemFromLinear {spaceId, parentWorkItemId?, placement, issue: LinearIssueLink,
    operationId}` → `{workItemId, referenceId}`. Creates the item and its `linear`/`issue`
    reference in one batch; idempotent by `operationId`; `linear/disabledForSpace` when off.
  - `linkLinearProject {workItemId, project: {id, name, url}, operationId}` → `{referenceId}`;
    `linear/disabledForSpace` when off.
  - Link an issue to an existing item with the ordinary `createWorkItemExternalReference`
    (`provider: "linear"`, `referenceType: "issue"`, `externalId: "ENG-123"`, `url`).
- External references: provider `linear` with `referenceType` `issue` (externalId = identifier)
  or `project` (externalId = project id, `cachedTitle` = project name). Helpers:
  `WorkOutlineSnapshot::linear_issue_reference`, `linear_project_reference`,
  `nearest_linear_reference`.
- `WorkItemView.externalSyncModifiedFields: ["title" | "description"]` (omitted when empty): the
  user edited that field of an issue-linked item, so Linear no longer updates it. A user
  `updateWorkItem` that changes title or description of an issue-linked item adds the field in
  the same batch; removing the item's last Linear issue reference clears the list. Linking an
  issue to an existing item that already has a description marks `description` (the user's text
  wins); its title follows Linear. `WorkItemView::follows_external_sync(field)`.
- Agents cannot perform any of the Linear ops (agent policy refuses them).

## Engine behavior

- Key: macOS Keychain (`security`, service `sh.zeron.linear`, account `api-key`), then
  `LINEAR_API_KEY`. Debug/test builds use an injectable store. Any 401 deletes the stored key.
- GraphQL `https://api.linear.app/graphql` with `Authorization: <key>` (no `Bearer`); 429 honors
  `Retry-After` capped at 30 s with bounded retries; request timeout; identical in-flight
  requests are coalesced.
- Background sync of linked items every 15 minutes, on `LinearRefreshLinkedItems`, and when a
  Space turns Linear on: only title/description, only fields not in
  `externalSyncModifiedFields`, written as a system actor.
- Chat ticket chips in Linear-enabled Spaces use the API client when a key is set; the `linear`
  CLI remains the fallback.
- Instruction drift ignores item title renames (compares instruction ids, text, order, and source
  kind); stored revision hashes and the envelope are unchanged.

## UI notes

- `crates/ui/src/work_outline/launch.rs` `instructions_changed_since_last_launch` compares
  `selectedRevisionHash` with `currentEffectiveHash` directly; use `ChatInstructionState.drift`
  instead so title renames do not count there either.
- New fields break exhaustive `WorkItemView` literals (add
  `external_sync_modified_fields: Vec::new()`).
