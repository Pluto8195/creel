# Review PR launcher

Zeron can check out someone's pull request and start a review Chat the same
way the Cofactr workspace's `tools/cursor-claude-launcher/launch.sh` does
(the GitHub overlay's **Claude ▾ → code-review / fix-ci** menu).

## Entry points

- **⇧⌘P (PR review chats)**: a **Review requested** section above the review
  chats lists every PR where your review is requested (`ReviewRequestedPrs`)
  with **Review** (`/code-review`) and **Fix CI** (`/fix-ci`), plus **Open
  chat** when a chat already links the PR. The box next to it takes a PR URL
  (`https://github.com/owner/repo/pull/123`) or `owner/repo#123`.
- **Overview → My PRs → Needs my review**: rows without a linked chat get a
  **Review** button; rows with one still jump to that chat.
- **Command palette → Review PR…** opens ⇧⌘P with the PR box focused.
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
   headRefName,isCrossRepository,title`. Forks are refused.
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
   (`Fix CI …` for fix-ci), linked to the PR (manual `ChatPrLink`),
   classified `pr_review` (manual), assigned to `workItemId` when given (the
   item must be in the same Space), and the seeded prompt is queued last.

The prompts are launch.sh's, verbatim:

- code-review: ``/code-review <url> — someone else's PR; review it as a
  reviewer. Its branch `<b>` is checked out at <location>; review the PR's
  commits against its base, not local changes. Report findings to me here and
  don't post anything to GitHub unless I ask.``
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
fans out again. The caller also writes the `pr_review` classification into its
own sidecar once the chat row replicates, so ⇧⌘P on the calling device lists
it.

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

With a destination picked for a project (Space-root details, or **Use as PR
Review Destination** on an item), the engine on the project's device keeps one
child item per PR that requests your review, each with a Required Action per
review round, and resolves them when you review or the PR closes. Wire
contract: `docs/plans/work-outline/pr-sync-contract.md`; user guide:
[Work Outline → PR review sync](work-outline.md#pr-review-sync).

| RPC | Used by |
| --- | --- |
| `PrStatus {owner, repo, number, forceRefresh?}` | PR chips (cached ~2 minutes; Details and ↻ force a refresh) |
| `PrReviewSyncNow {spaceId}` | **↻ Sync now** in the Space-root details |
| `PrReviewRequested {spaceId}` / `PrMyOpen {spaceId}` | the Inbox column's GitHub section |
| `WorkOutlineMutate setPrReviewSyncDestination` | the destination picker, **Clear**, the item menu |

Failures read `github/<code>: …` (`ghMissing`, `unauthorized`, `rateLimited`,
`notFound`, `network`, `invalid`); a host without the RPCs shows "PR review
sync isn't available on this host yet".

When the current project has a destination, ⇧⌘P lists the destination's PR
items with their review Chats nested instead of the `pr_review`
classification; without one the classified list stays (classification is
being retired).
