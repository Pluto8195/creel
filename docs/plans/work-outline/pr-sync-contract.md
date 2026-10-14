# PR review sync v1 — wire contract

Wire contract commit: `e073523f` (branch `pr-review-sync-engine`). Code:
`crates/proto/src/pr_review_sync.rs` (+ tests), `crates/proto/src/work_outline.rs` and
`work_outline_user_actions.rs` (Work Outline additions), `crates/rpc/src/lib.rs`
(`methods::PR_*`), doc layer `crates/doc/src/registry/work_outline/pr_review_sync.rs`.
Capability: `pr-review-sync-v1` (`capabilities::PR_REVIEW_SYNC_V1`). Engine:
`crates/engine/src/pr_review_sync.rs` (`71f8b8dc`), tests `crates/engine/tests/pr_review_sync.rs`
(fake `gh`, `04378c08`).

## Decisions

- Automatic sync, OFF until the user picks ONE work item per Space as the destination. A Space
  without a destination makes no `gh` calls for the sync.
- Every open PR that requests the user's review (`gh search prs --review-requested=@me`) in ANY
  GitHub repository of the Space gets a child item under the destination. Repositories = the
  Space folder's own checkout, every submodule (recursively), and every nested repository under the
  folder (the repository topology discovery), matched by the `owner/repo` of each `origin` remote
  (https, ssh, scp spellings; case-insensitive). For the Cofactr workspace that is workspace, ui,
  flagship, kb, infra.
- Item title `"<repo>#<number> · <PR title>"` (`pr_item_title`), status `active`, placed last under
  the destination, linked by an external reference provider `github`, referenceType `pullRequest`,
  externalId `<owner>/<repo>#<number>` (GitHub's spelling), `url`, `cachedTitle` = PR title.
- Items are tracked by reference, never by position: an item the user moved stays where it is and
  is still updated; an archived item is never touched (except one the sync itself archived, below); nothing is ever deleted or moved by the sync.
- Nothing is written to GitHub.

## RPCs (local/IPC, never forwarded)

| Method | Params | Reply |
|---|---|---|
| `PrStatus` | `{owner, repo, number, forceRefresh?}` | `PullRequestStatusSummary` (~2 min per-PR cache) |
| `PrReviewSyncNow` | `{spaceId}` | `{created, updated, resolved}` (bypasses caches) |
| `PrMyOpen` | `{spaceId}` | `[PullRequestStatusSummary]` the user's open PRs in the Space's repos |
| `PrReviewRequested` | `{spaceId}` | `[PullRequestStatusSummary]` open PRs in the Space's repos requesting the user's review |

`PrMyOpen` / `PrReviewRequested` come from one `gh search prs` each (~1 min cache) filtered to the
Space's repositories, most recently updated first; CI, review decision, and requested reviewers are
filled from the `PrStatus` cache when it holds the PR (else `ciStatus: none`, no decision; call
`PrStatus` per row for live chips). In `PrReviewRequested`, `myReviewState` is `requested` or
`reRequested` (the latter when the user has reviewed before, as far as the cache knows).

Errors are strings `github/<code>: <message>` (`GithubError::from_wire_message`), codes
`ghMissing` (install gh), `unauthorized` (run `gh auth login`), `rateLimited`, `notFound`, `network`,
`invalid` (bad params; Space-scoped calls: no destination, destination missing/archived, or the
Space lives on another device — the sync and the Space-scoped RPCs run on the Space's own device,
which has its checkouts).

## Types (`zeron_proto::pr_review_sync`, camelCase JSON)

- `PullRequestRef {owner, repo, number, url}`; `external_id()`, `key()` (lowercased),
  `parse_external_id`.
- `PullRequestStatusSummary {ref, title, author, state, ciStatus, reviewDecision?, myReviewState,
  requestedReviewers, updatedAt, headRefName?, baseRefName?}`.
  - `state`: `open|draft|merged|closed` (unknown → `open`).
  - `ciStatus`: `passing|failing|pending|none` (unknown → `none`).
  - `reviewDecision`: `approved|changesRequested|reviewRequired` (unknown → `reviewRequired`).
  - `myReviewState`: `requested|reRequested|commented|approved|changesRequested|none`
    (unknown → `none`). `requested` = requested and never reviewed; `reRequested` = requested
    after a review; otherwise the latest review's state.
- `WorkItemExternalSyncState {provider, externalId, round, requestState, syncedStatus,
  roundStartedAt, syncedAt, archivedBySync?}`; `requestState`:
  `requested|submitted|withdrawn|merged|closed` (unknown → `requested`). `archivedBySync`
  (`merged|closed`, absent otherwise): the sync archived the item for that PR state (see below).

## Work Outline additions

- `workOutlineSpaceSettings.prReviewSyncWorkItemId` (LWW string/null). Snapshot:
  `spaceSettings.prReviewSyncWorkItemId` (omitted when unset); helper
  `WorkOutlineSnapshot::pr_review_sync_destination`.
- `WorkOutlineMutate` op `setPrReviewSyncDestination {spaceId, workItemId?, operationId}` → `{}`.
  The item must be live, unarchived, and in the Space (`crossSpace`, `archived`, `notFound`);
  `workItemId: null` turns the sync off (synced items stay). User only (agents refused). Setting a
  destination triggers a sync of the Space.
- `WorkItemView.externalSyncState` (omitted when absent): the sync's per-item record. Written only
  by the system actor through `updateWorkItem.changes.externalSyncState` (user/agent →
  `unauthorized`).
- External reference provider `github` / referenceType `pullRequest`
  (`GITHUB_PROVIDER`, `GITHUB_PULL_REQUEST_REFERENCE`); helper
  `WorkOutlineSnapshot::github_pull_request_reference`.
- Required Actions: `UserActionRequestOrigin.kind` `chat|system` (default `chat`, omitted) and
  `UserActionRequestView.originKind` (omitted for `chat`). A `system` request has an empty `chatId`,
  `originChatAvailable: false`, and only the system actor may create it. Row field
  `originKind: "system"`.

## Engine behavior

- Runs on the device that owns the Space, every 3 minutes (first pass 60 s after boot), on
  `PrReviewSyncNow`, and when a destination is set. `gh` comes from the login-shell PATH with
  `GH_PROMPT_DISABLED=1`.
- One pass: `gh api user` (viewer login, cached per engine run) and one
  `gh search prs --review-requested=@me --state=open` (shared by every Space) are the steady-state
  cost; `gh pr view <n> --repo <o/r>` runs only for a new item, a new review round, or a tracked PR
  that left the requested list. A `rateLimited`/`unauthorized`/`ghMissing` failure stops the pass
  before any write.
- Ids: item `derive_id("pr-review-sync:<spaceId>:<key>", "item")`, Required Action
  `derive_id("pr-review-sync:<spaceId>:<key>:round:<n>", "request")` (`<key>` = lowercased
  `<owner>/<repo>#<n>`), so repeated passes and other devices converge on one item and one request
  per round. Every write is the `system` actor.
- Per PR (requested now):
  - not tracked → create item + reference + record (`round 1`, `requested`, status `active`) +
    Required Action.
  - tracked, record `requested` → nothing (the status is the user's to change).
  - tracked, record `submitted|withdrawn|merged|closed` (a new request) → `round + 1`,
    `requested`, status `active`, new Required Action.
- Per tracked item whose record is `requested` but whose PR left the list: `gh pr view` decides
  `merged` ("PR merged"), `closed` ("PR closed"), `submitted` (the user reviewed during this round;
  "Review submitted"), else `withdrawn` ("Review request withdrawn"); the round's Required Action is
  resolved with that note and the item set `completed`.
- Archive on finish: when the PR is `merged` or `closed` (closed without merge: the review is moot,
  so it archives like a merge), after resolving the Required Action and completing the item the sync
  archives it with `archiveWorkItem` as the system actor (`archiveDescendants`; synced items have
  none; Chats are never archived or unassigned) and then records `archivedBySync` = that state.
  This also covers a reviewed/withdrawn (completed) item whose PR later merges or closes: each pass
  checks such items' PRs (`gh pr view`, through the status cache) until they finish. The archive
  operation id is hashed from (Space, PR key, round, finished state) like the other derived ids.
- Never archived twice: a record with `archivedBySync` is skipped, so an item the user restores
  stays restored. A new round (the PR reopened and the review requested again) clears the marker, so
  a later merge/close archives again. A `merged`/`closed` record without the marker (an earlier
  archive failed, or a record from before this rule) is archived on the next pass. An item the user
  archived is never touched; an item the *sync* archived (marker set, system actor) is restored
  when GitHub shows the PR open again and requests the review, then starts the new round.
- The status is written only on those transitions, so a status the user set stands until GitHub's
  review-request state changes again (`syncedStatus` records what the sync last wrote).
- Required Action text `Review requested: <repo>#<n> <title>` (or `Re-review requested: …` when the
  user reviewed before); detail lines `Author: @<login>`, `Requested: <PR updatedAt>`,
  `CI: <status>`, `<url>`; `dedupeKey` `pr-review:<owner>/<repo>#<n>:<round>`
  (`pr_review_dedupe_key`). A request the user resolved is never recreated for the same round.
- `StartPrReview` with `workItemId` of a synced item starts the review Chat assigned to that item.
- Tests and embedders swap the tools with `EngineCore.pr_review_sync.reconfigure(
  PrReviewSyncConfig::for_tests(<fake gh>))`; `background: false` turns off the loop and the
  destination-triggered pass (`PrReviewSyncNow` still runs).

## UI notes

- New fields break exhaustive literals: `WorkItemView` (add `external_sync_state: None`) in
  `crates/ui/src/work_outline/optimistic.rs` and `store.rs`, `UserActionRequestView` (add
  `origin_kind: Default::default()`) in `crates/ui/src/work_outline/required_actions.rs`.
- A system request has no Chat: hide "Open chat" when `originKind == system`.
