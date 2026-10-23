# Review PR launcher

Zeron can check out someone's pull request and start a review Chat the same
way the Cofactr workspace's `tools/cursor-claude-launcher/launch.sh` does
(the GitHub overlay's **Claude ▾ → code-review / fix-ci** menu).

## Entry points

- **⌘P / ⇧⌘P (the PR page)**: ⌘P is `OpenMyPrs` (customizable in Settings →
  Shortcuts, default `mod-p`, "Open PR page"); ⇧⌘P is a fixed alias; the
  palette's **My PRs** opens it too. The page is a port of pr-dash with three
  tabs: **Authored by me** (default) and **Review requested**, both fed by
  `PrDashboard` (see [pr-dash spec](plans/work-outline/pr-dash-spec.md)), and
  **Review chats** (the destination list below, or the launcher review Chats). Each dash tab title carries its needs-you count (`urgent / total`,
  red when anything needs you, over rows that pass **hide drafts**); the page
  refetches the visible tab every minute and **Refresh** forces. A tab has
  bucket chips (multi-select), **hide drafts** (on), **group by status** (on)
  and a text filter, each kept per tab. Rows list repo, `#number` (opens the
  PR in the browser), title with tags, churn, reviewers (or author), CI, open
  comments and age, plus the Zeron chats linked to the PR as chips (click
  opens the chat). Review-requested rows have **Review** (`/code-review`) and
  **Fix CI** (`/fix-ci`). Authored rows offer **Fix CI** while CI fails.
  A PR's **review chat** is a live (not archived) chat that is assigned to the
  PR's review work item, or linked to the PR as `manual` or `review_launcher`
  (mention, branch, ticket, diff-target and created-in-chat links stay plain
  chips). A finished or idle review chat still counts. While a PR has one,
  **Open review** is the row's primary action, in the accent color with the
  chat's title and state (working, idle, awaiting input, finished); **Review**
  moves into the row's **⋯** menu as **Start another review**, which also lists
  the other review chats (the most recent opens from the button). Up / Down
  move the highlighted row and **Enter** opens its review chat, or starts a
  review when there is none. The same "being reviewed" marker shows on the
  PR's row in the Outline and in the chat's work-context strip.
  The box in the header (all tabs) takes a PR URL
  (`https://github.com/owner/repo/pull/123`) or `owner/repo#123`.
- **Command palette → Review PR…** opens the PR page with the PR box focused.
- **PR review sync** ([Work Outline](work-outline.md#pr-review-sync)): `⌘↩`
  on a PR review item, **Review** / **Fix CI** in its Details or right-click
  menu, the Inbox column's GitHub section, and ⇧⌘P's destination list send
  `StartPrReview` with the item's `spaceId` and `workItemId`, so the review
  Chat is filed under that item.

After a start Zeron opens the new chat. Refusals (fork, repository not in any
project, occupied worktree path, `gh` missing, device offline) stay inline on
the row or box with what to do next. A behind-origin warning shows in the
sidebar notice and is also part of the seeded prompt.

## `StartPrReview`

`{repoOwner, repoName, number, action: codeReview|fixCi, spaceId?,
workItemId?, config?, operationId}` → `{chatId, spaceId, deviceId, title,
prUrl, branch, checkoutPath, checkoutKind: existing|canonicalCheckout|
newWorktree, cwd, behindBy, prompt, notes[]}`. Errors are
`pr-review/<code>: <message>` with codes `invalid`, `fork`, `prNotFound`,
`ghUnavailable`, `repoNotFound`, `worktreePathExists`, `gitFailed`,
`deviceOffline`, `operationConflict`.

Steps, on the device that owns the Space:

1. **Space.** Among this device's Spaces (or the pinned `spaceId`), find a
   checkout of `github.com/<owner>/<name>`: the Space folder itself or one of
   its submodules (`.gitmodules`, up to three levels), matched by the raw
   `remote.origin.url` (https, ssh, and scp spellings), never by folder name.
   Main checkouts beat linked worktrees, then the outermost folder wins, so
   the Cofactr workspace superproject beats a Space opened on `flagship/`.
   When no local Space matches, the engine asks each other device that owns
   Spaces and advertises `pr-review-launch-v1`.
2. **PR.** `gh pr view <n> --repo <owner>/<name> --json
   headRefName,isCrossRepository,title,author`. Forks are refused.
3. **Fetch.** `git fetch origin +refs/heads/<b>:refs/remotes/origin/<b>`.
4. **Location**, in launch.sh's order:
   1. wherever `<b>` is already checked out (the canonical checkout or any
      worktree; the first `git worktree list` entry maps to the checkout);
   2. the canonical checkout, if it is on `main` with no tracked changes
      (`git status --porcelain --untracked-files=no --ignore-submodules=all`):
      `gh pr checkout <n>` there;
   3. otherwise a new worktree `<root>/<name>-pr-<n>` (`git worktree add
      <path> <b>` when the local branch exists, else `--track -b <b> <path>
      origin/<b>`). `<root>` is the first line of
      `<space>/.devcontainer/.current-worktrees-root`, else
      `$WORKSPACE_WORKTREES_DIR` (the engine's environment), else
      `<space>/.worktrees`. An existing path is refused with launch.sh's
      message.
5. **Behind.** `git rev-list --count HEAD..origin/<b>` in the chosen checkout.
6. **Chat.** Created at the Space root (launch.sh `cd "$ws"`) with the
   caller's agent config (the UI sends the composer's new-chat agent; the
   engine defaults to Claude Code), titled `Review <name>#<n>: <PR title>`
   (`Fix CI …` for fix-ci), linked to the PR (a `ChatPrLink` with source `review_launcher`),
   assigned to `workItemId` when given (the item must be in the same Space),
   and the seeded prompt is queued last. The `review_launcher` PR link is what
   marks the chat as a launcher-created review chat; there is no separate
   classification.

The prompts are launch.sh's, verbatim:

- code-review: ``/code-review <url> — someone else's PR; review it as a
  reviewer. Its branch `<b>` is checked out at <location>; review the PR's
  commits against its base, not local changes. Report findings to me here and
  don't post anything to GitHub unless I ask.``
- code-review of the viewer's own PR (its author is the engine's cached
  `PrViewer` login; an unknown login keeps the prompt above): ``/code-review
  <url> — my own PR; review it as its author, before or while I wait for
  reviewers: look for bugs, missing tests, risky changes and unclear diffs.
  Its branch `<b>` is checked out at <location>; review the PR's commits
  against its base, not local changes. Report findings to me here. Fix clear
  problems on the PR branch only if I ask, and don't post review comments or
  approve on GitHub.``
- fix-ci: ``/fix-ci <url> — my PR. Its branch `<b>` is checked out at
  <location>; do all the work there.``

`<location>` is `the canonical workspace checkout` (the Space root), ``the
worktree `<path>` (`/workspace/.worktrees/<dir>` in the devcontainer)`` (under
`<space>/.worktrees` or the worktrees root), ``the canonical checkout
`<path>` `` (elsewhere inside the Space), or `` `<path>` ``, followed by `, N
commit(s) behind origin/<b> — pull before you start` when behind. Zeron adds
the devcontainer note only when the Space has a `.devcontainer` folder
(Cofactr always does).

Nothing is posted to GitHub.

### Idempotency

Chat, message, and command ids derive from `operationId`. The inputs are
claimed in the processed ledger (`pr-review:launch:<op>`), and the finished
reply is kept in `<store>/pr_review_launches/`, so a retry returns the same
reply without running `gh` or `git` again; the same id with other inputs is
`operationConflict`. The UI keeps one id per PR + action + agent for the life
of the window, so double-clicks and retries land on the same chat.

### Routing

`StartPrReview` is forwardable. With `spaceId` on another device the engine
forwards to that device; a forwarded call (it carries `targetDeviceId`) never
fans out again.

## Differences from launch.sh

- Repositories are any GitHub repo found in a Space, not the fixed
  `flagship|kb|ui|infra|workspace` list; the worktree folder uses the GitHub
  repository name.
- `claude --permission-mode acceptEdits` becomes the chat's agent config from
  the composer (approval mode included).
- The devcontainer mount note is conditional on `.devcontainer` (above).
- Chats are filed under a work item only with an explicit `workItemId`.
  The PR review sync's destination (one work item per project) collects the
  requested PRs as items, and every sync surface sends the PR item's id.

## PR review sync

With a destination picked for a project (the Inbox column's GitHub section
**Auto-sync review requests into… Choose item ▾**, the Space-root details, or
**Auto-sync PR Review Requests Here** on an item), the engine on the project's device keeps one
child item per PR that requests your review, each with a Required Action per
review round, and resolves them when you review or the PR closes. Wire
contract: `docs/plans/work-outline/pr-sync-contract.md`; user guide:
[Work Outline → PR review sync](work-outline.md#pr-review-sync).

When a synced PR is **merged** (or **closed** without merging, where the review is moot), the sync
resolves its Required Action, marks the item completed, and archives it as the system actor; the
item's Chats are left as they are. The same applies to an item you already completed by reviewing
once its PR merges or closes. Restoring an archived item keeps it restored: the sync archives it
again only after a new round (the PR reopened and your review requested again) finishes.

| RPC | Used by |
| --- | --- |
| `PrStatus {owner, repo, number, forceRefresh?}` | PR chips (cached ~2 minutes; Details and ↻ force a refresh) |
| `PrReviewSyncNow {spaceId}` | **Sync now** in the GitHub section's sync bar and the Space-root details |
| `PrReviewRequested {spaceId}` / `PrMyOpen {spaceId}` | the Inbox column's GitHub section |
| `WorkOutlineMutate setPrReviewSyncDestination` | the sync bar (**Choose item ▾**, **Change ▾**, **Stop**, **Create "PR reviews" item** after its `createWorkItem`), the Space-root picker and **Stop**, the item menu |

Failures read `github/<code>: …` (`ghMissing`, `unauthorized`, `rateLimited`,
`notFound`, `network`, `invalid`); a host without the RPCs shows "PR review
sync isn't available on this host yet".

When the current project has a destination, the **Review chats** tab lists the
destination's PR items (synced PR items) with their review Chats nested.
Without one it lists the launcher review Chats: the Chats with a PR link whose
source is `review_launcher`. Chat classification no longer exists.

## PRs on work items

Besides the review sync, the engine links a work item to the PRs that belong
to it, so the item shows the PR icons (state, CI, your review; the Details chip has the
full text) and the
Details **Pull request** section. Code: `crates/engine/src/work_outline_pr_links.rs`.
Each link is a `github` / `pullRequest` reference (`externalId`
`<owner>/<repo>#<n>`) added by the system actor. Status comes from `PrStatus`
like every other PR chip; nothing is fetched twice.

| Source | Applies to | When |
| --- | --- | --- |
| **Linear** (primary): the issue's GitHub PR attachments (Linear's GitHub integration attaches a PR whose branch or title names the issue) | items linked to a Linear issue, in projects with Linear on | the linked-item Linear sync: every 15 minutes, **↻ Refresh**, turning Linear on |
| **Chats** (fallback): a PR the item's Chat created | any item with an associated Chat | every 3 minutes, with the PR review sync loop (no `gh` call) |

What counts as "the Chat created PR X" (high confidence only):

- the Chat's durable PR link with source `manual` (the user set it) or
  `created_in_chat` (the live tap saw a `gh pr create` / `gh pr new` /
  `create_pull_request` tool call print the URL). `mentioned` (a URL in text
  or another tool's output) never counts, and neither does branch inference
  (`gh pr view <branch>`, which at a superproject root resolves every Chat to
  the same PR);
- backfill: a Chat with no such link has its stored transcript re-read once
  (at most 50 Chats per pass, logged) for PR-creation tool output; a hit is
  stored as a `created_in_chat` Chat link (a `manual` link is never changed).

Rules for both sources:

- The PR's repository must be in the project (checkout, submodules, nested
  repositories, by `origin`), and the project must live on this device.
- Links are only added: never removed, never moved, never duplicated (one
  reference per item and PR; the operation id is a hash of item and PR). A
  reference you remove is not added back. An item can link several PRs (a
  ticket's UI and Flagship PRs); the Outline shows an icon for each PR (up to three, then
  "+N"), and Details lists the others under the first.
- A Chat's PR is linked once, to the item the Chat belongs to when the PR is
  first seen, and only if no item of the project links that PR yet. Moving
  the Chat to another item later keeps the existing link where it is (the
  new item does not get it automatically).
- Item status is left alone: a merged PR does not complete its item. (A
  "complete the item when its PRs merge" toggle is a possible later option.)
- These items are not PR review items: `⌘↩` opens the normal Start Chat, and
  the review sync never adopts them for a review request (a requested review
  of such a PR gets its own item under the destination).
- The Linear attachment fields (`attachments { nodes { url title subtitle
  sourceType metadata updatedAt } }`) were not verified against the real
  API. The query is separate from the issue query, parsed leniently, and a
  failure is logged (`linear: issue attachments could not be read`, target
  `linear`) without stopping the title sync.
