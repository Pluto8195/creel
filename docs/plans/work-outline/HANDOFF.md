# Work Outline — implementation handoff

Prepared 2026-10-14, last updated 2026-10-25. Read this first in a new chat picking up the Work Outline work.

## Where things are

- **Repository:** `/Users/mikey/Projects/agent-mode-tools/zeron`, on GitHub as `Pluto8195/mast`
  (renamed from the `Pluto8195/zeron` fork on 2026-10-25, then `creel` → `mast` on 2026-10-26; old
  URLs redirect). `zeronsh/zeron`
  stays as the `upstream` remote, but we no longer open PRs there. The folder and submodule path
  are still `zeron/`, and so are all code identifiers.
- **Integration branch:** `work-outline-implementation`, branched from `main` at `3801cb5b`. It is
  the GitHub default branch. `main` is upstream's code at the fork point. **Never commit to
  `main`.** `origin` carries only these two branches; delete each feature branch once it is merged.
- **Parent repo:** `agent-mode-tools` branch `zeron-work-outline` pins the `zeron` submodule.
  Restore points: tag `pre-linear-integration` (`83f49b3d`) and parent commit `d9129f6`.
- **Original spec:** `work-outline-implementation-specification.md` (copy of Mikey's spec, plus an
  "Implementation contract amendments" section). The binding contract is `m0-contract.md` in this
  folder; it wins where it differs from the spec.
- **Installed build:** `/Applications/Mast.app` (since 2026-10-26). It has the same bundle id
  `sh.zeron.app.dev` and data `~/.zeron` as the earlier `Creel.app` and `Zeron Dev.app`, which are
  rollbacks only (`Zeron Dev.app` is in `~/zeron-dev-app-backups/`); never run two at once. Rollback binaries are
  in `~/zeron-dev-app-backups/`, one per install, named after the feature. Keep only the last few.

## Built and installed

All milestones M0–M4 of the spec, then the follow-ups below. User docs: `docs/work-outline.md`,
`docs/pr-review.md`.

- **Outline (M1):** durable work items in the profile registry (row table, HLC last-writer-wins per
  field, 16 KiB op cap), Spaces as roots, chats under items, Canvas positions preserved.
- **Instructions (M2):** per-item instructions with revisions. Claude Code gets them via
  `--append-system-prompt-file` + `--system-prompt-snapshot off` on every spawn; Codex via
  `developerInstructions` with a fresh session on change (or "keep session"). Cursor/ACP refuse.
- **Execution (M3):** per-chat execution bindings, multi-repo roots, worktree assignments.
- **Required Actions and agent tools (M4):** `zeron-agent` MCP server with a per-Run token;
  `work_outline_*` tools; Reply & resolve sends the answer to the chat.
- **Keyboard and editing:** Tod-style keys (↑↓ in visual order, ⇧arrows move/indent, Tab/⇧Tab,
  F2/R rename incl. chats), drag and drop, right-click menus that show their keys, `?` reference,
  ⌘; outline⇄chat, ⇧⌘A archive, ⌘↩ Start Chat, ⇧⌘↩ Open Full Chat, ⌘F search, preview on select,
  Space root reorder/hide/rename/delete, tree guide lines, Undo toast for moves.
- **Latency fix:** the local profile never acked pending registry batches (67k queued); now
  `settle_pending_locally` when there is no edge, plus optimistic UI.
- **Editors:** skill autocomplete (always Claude Code + on-disk skills, regardless of the chat's
  harness), auto-growing instruction editors.
- **Review PR launcher (⇧⌘P):** matches the Cofactr workspace `launch.sh` flow.
- **Linear v1** (`linear-contract.md`): per-Space opt-in, key in keychain `sh.zeron.linear`, Inbox
  column on the left (⌥⌘L) with collapsible Issues/Projects. Nodes are created **manually only**
  (Mikey: "I don't want things changing out from under me"). Title/description follow Linear until
  edited; the full ticket goes to agents; project-linked item is the issue destination; "Add
  issues from project". Nothing is written to Linear. GraphQL is **not yet verified against the
  real API** (projects and attachments especially).
- **PR review sync** (`pr-sync-contract.md`): automatic items under one chosen destination item
  per Space (covers submodule repos), Required Actions "Review requested" / "Re-review
  requested", auto-archive on merge/close. PR links on ticket items come from Linear attachments
  first, chat-created PRs as fallback.
- **Planning artifacts phase 1** (`../planning-artifacts-design.md`, `../artifacts-contract.md`,
  agent conventions `docs/planning-artifact-conventions.md` v1.1):
  - Stored in `<canonical checkout>/.zeron/artifacts/<work item>/`, ignored via
    `.git/info/exclude` (never `.gitignore`); snapshots under `<profile>/artifacts/<id>/`.
  - `artifact_publish` / `artifact_get_plan` / `artifact_render_check` agent tools.
  - Render check uses an offscreen WKWebView snapshot (not screen capture), viewports × themes up
    to 4 images, and layout heuristics.
  - Links inside rendered artifacts route through Zeron. Files outside the workspace get "Open as
    artifact" (credential paths, `.env*`, and files over 10 MB are refused).
- **Visual compare for artifact revisions** (`crates/ui/src/artifacts/{compare,pixel_diff,
  markdown_diff}.rs`). Mikey's complaint: text diffs of charts are useless.
  - HTML, Mermaid and images: two revisions side by side with synced scroll. "Highlight changes"
    runs a pixel diff with region boxes; there is also onion skin and a Source tab for the line
    diff.
  - Markdown: rendered side by side, changed blocks tinted, section summary on top.
  - Options: compares two options of the current revision.
  - Verified only by headless tests.

- **Git uses the login-shell PATH** (2026-10-19): every engine `git` spawn goes through
  `zeron_harness::git_command()` / `git_command_async()`. Before this, worktrees in LFS repos
  (flagship) failed with "git-lfs: command not found" because the GUI app's PATH lacks
  `/opt/homebrew/bin`. New `git` spawns must use these helpers.
- **Error notices** (`crates/ui/src/error_notice.rs`): errors are a red card with full wrapped
  detail and a Copy button, and stay until dismissed. Used by the PR review launcher, Start Chat,
  composer send/worktree failures, and error toasts.

- **Coordinator agent tools** (2026-10-20): `create_chat { workItemId, prompt, … }` starts a
  child on the caller's item or a descendant (same launch path as Start Chat, so the target's
  instructions and execution binding apply; `cwd`/`branch` refused). `work_outline_read_subtree`
  lists the items, chats, and open Required Actions under the caller's item. Children cannot message
  their parent; coordinators collect results with `wait_for_turn` / `read_chat` / Required
  Actions.
  In the outline a child chat nests under its parent chat only when it has no item of its own or
  shares the parent's item; a coordinator's child shows under the item it was started on.

- **Outline fixes** (2026-10-20, first release by the coordinator):
  - Instructions longer than 4 lines collapse behind Show more / Show less. ↑ ↓ Edit Remove sit in
    the entry's header row. The instruction action "Archive" is now "Remove" and still has undo.
  - Synced PR and Linear chips keep their last data during a refresh or after a failed pull. They
    used to drop to "#42" / "ENG-1" on every 2-minute (PR) or 5-minute (Linear) refresh. A
    search-list row no longer overwrites a fully fetched PR status (`github/store.rs`,
    `linear/store.rs`).
  - An archived item with no visible child item is hidden even when it still has live chats; those
    chats show under Unassociated Chats. This used to leave archived synced PR items on screen,
    dimmed and immovable, because they keep their review chat (`work_outline/model.rs`).
- **Coordinator resolves its children's requests** (2026-10-20): `resolveUserActionRequest` is
  allowed when the request's origin Chat is the caller or a Chat whose `parentChatId` is the
  caller. This covers direct children only, not grandchildren or chats a person started
  (`m0-contract.md` § coordinator amendments, item 3).
- **Visual compare follow-ups** (2026-10-20):
  - The per-kind compare mode persists in `UiSettings` (`artifactCompareModes`).
  - Markdown panes scroll together by block position. Wheel and trackpad sync; dragging the
    scrollbar does not.
  - Highlight changes and onion skin cover the full render, up to 8000 CSS px and 20 M px per
    snapshot. Taller content is cut off silently. The agent render check is unchanged.
  - Options compare can follow one option across two revisions. The engine snapshots option files
    per revision, and `ArtifactContent` serves them by revision. Revisions recorded before this
    show "not found" until the artifact is republished.
  - The tall-snapshot path has only run in headless tests, never against a real WKWebView.
- **Plan reading: ⌘F find and section index** (2026-10-20; `artifacts/{find,outline}.rs`,
  `artifacts/tab/find_view.rs`, `files/markdown_find.rs`):
  - ⌘F opens a find bar while focus is in an Artifact tab. It shows a match count and highlights
    every match. Enter / ⇧Enter and ⌘G / ⇧⌘G step through matches with wraparound, and Esc closes
    the bar. Outside an Artifact tab, the shell's ⌘F and ⇧⌘G handlers `propagate()` when their own
    route doesn't apply. ⌘F is still outline search everywhere else.
  - A collapsible section rail lists h1–h3 headings: click to jump, and the section in view is
    highlighted. It's hidden under 3 headings and for options, source, compare, images and Mermaid.
  - HTML find runs over postMessage into the sandboxed frame, and WKWebView forwards ⌘F, ⌘G, ⇧⌘G
    and Esc to the app. Highlights need macOS 14.2+. Matches that span inline tags aren't found.
    Find and the index are off for HTML on Linux.
  - Only headless tests have run. Key forwarding, the frame script under the CSP, and active-section
    tracking haven't been seen in a real WKWebView.
- **Plan section keys** (2026-10-20):
  - With the section rail focused (click it, or press ⌥⌘S), ↑ / ↓ / Home / End move through
    sections and scroll the document. Enter or Esc returns focus to the document.
  - ⌥↑ / ⌥↓ anywhere in the plan jump to the previous / next section, with no wraparound. These keys
    are bound only in the ArtifactTab key context.
  - `browser/macos.rs` now maps AppKit's special-key characters (arrows, Home / End, Esc) to
    combos. Before this, Esc forwarding from WKWebView probably never worked.
  - The `?` reference doesn't list these keys yet.
- **Compact icon PR / Linear rows** (2026-10-20, `github/row_icons.rs`):
  - PR rows end in fixed slots, in this order: author, needs (attention dot + count), your review
    (eye / refresh / check / close-circle), CI dot, and the PR icon colored by state. Hovering the
    group shows the full line, the details panel keeps the full text, and `?` has a legend.
  - On narrow screens the slots drop CI, then review, then author. The title keeps about 120 px.
  - "You" for the viewer's own PRs isn't shown, because the UI doesn't know the viewer's GitHub
    login (only the engine does, via `gh api user`). Adding it needs an RPC.
  - Items with several PRs show icons only for the first one.
  - Linear rows get a state dot plus a status icon, but keep their totals text and don't drop slots.
- **Question cards** (2026-10-20; first slice of Artifacts phase 3 D, rendering only):
  - `markdown/question.rs` `parse_question` turns a top-level `> [!question] Q-n · …` quote into
    `Block::Question`. `markdown_diff` reads ids from it, and its old private parser is removed.
  - Each card shows an id badge, a status pill, a Blocking flag, the question in bold, and Example /
    Blocks / unknown fields / Decision rows. Decision is tinted. Resolved cards are dimmed and
    collapsed, with a Show/Hide toggle, and expand while find is active.
  - Transcripts render question cards too.
  - The HTML `data-zeron-question` form gets CSS plus a frame script (`artifacts/question_html.rs`).
    It's only been checked headlessly, never in a real WKWebView.
  - Still to do for D: the Questions panel, Required Actions for questions, and answering from Zeron.
- **Diff the agent's worktree** (2026-10-21; `m0-contract.md` "Amendment: chat diff targets"):
  - Cofactr chats start at the workspace root, and agents then `git worktree add` into a gitignored
    `.worktrees/<submodule>/<slug>`, so the Changes pane used to show the root's unrelated diff.
  - A new mutable registry row, `chatDiffTargets`, is written by two things: the agent tool
    `bind_chat_worktree {path}` (validated against the chat repository's worktree family), and the
    Changes pane's "Diff checkout…" picker, which suggests ticket matches first.
  - The selected target drives the Working tree, Branch and Latest turn scopes, as well as the member
    pills and the composer branch label. It is display-only and never blocks a turn; execution
    bindings were deliberately not reused.
  - Not done: remote-device chats, PR lookup from a target, and automatic binding without the agent.
    If it is added later, tag auto binds `source: detected` and never take the selection away from
    a manual pick.
- **zeron-agent MCP instructions fit the truncation limit** (2026-10-21):
  - Claude Code cuts MCP server instructions at about 2000 characters. `INSTRUCTIONS` in
    `crates/mcp/src/jsonrpc.rs` was about 2500, so Claude agents never saw the Planning artifacts
    paragraph.
  - It is now 1886 characters, with a test enforcing ≤ 1900 and checking the key phrases. It tells
    agents to call `bind_chat_worktree` right after `git worktree add`.
  - Keep new guidance short and near the top, or put it in the tool descriptions.
- **Work-context strip in the full chat view** (2026-10-23, `chat_work_context{,/strip}.rs`,
  `shell/work_context_ui.rs`):
  - It's one line, expandable. Variants by precedence: PR review item (own item, ancestor, or a
    launcher review chat) > Linear issue (the ticket plus its PRs, or "No PR yet") > Linear project
    or pinned plan (plan title, revision, age, "+N artifacts", or "No plan yet"). No strip otherwise.
  - Launcher review chats are detected by their `review_launcher` chat-PR link
    (`ChatPr::launched_review` in `chat_work_context.rs`).
  - Follow-ups: PR line counts / unresolved threads / conflicts; open-question counts in the
    planning strip; matching the chat's diff-target branch to PR heads; the strip in the overview's
    chat panel.
- **Coordinator resolve fix** (2026-10-23):
  - Bug: coordinator resolves of a child's request silently wrote nothing when the request had a
    `dedupeKey`, which agents always pass. The dedupe group was filtered down to the caller's own
    requests, yet the tool still replied `resolved:true`.
  - Fix: `agent_may_resolve_origin` (the caller's own chat or a direct child) is shared by
    authorization and the group filter. The tool reads the row back and returns `operationConflict`
    if nothing changed.
- **PR page parity with pr-dash** (2026-10-23; `proto/src/pr_dash.rs`, `engine/src/pr_dash.rs`,
  `engine/src/pr_chat_links.rs`, `ui/src/pr_page/`):
  - ⌘P (`OpenMyPrs`) and ⇧⌘P open the PR page. It has pr-dash's two tabs: Authored, and Review
    requested (`review-requested:@me` + `reviewed-by:@me -author:@me`), fetched through the
    `PrDashboard` GraphQL RPC. The 9 authored and 8 incoming bucket rules are ported, with
    needs-you counts and churn, reviewer, CI, thread and conflict fields. The old chat table is
    kept as a third "Review chats" tab.
  - Chat-PR links gained the sources `branch`, `diff_target`, `ticket` and `review_launcher`. URLs
    are compared in canonical form, and every PR URL from one `gh pr create` is linked. The
    launcher reuses an existing chat for the same PR and action. A launcher review chat is
    identified only by its `review_launcher` link.
  - The old overview "My PRs" sidebar is unreachable; deleting it is a follow-up after
    remove-canvas.
  - Not done: the leaderboard tab, the person picker, the churn discount for blank and comment
    lines, the diff bar, and file paging beyond 100 files.
- **Diff a branch or PR without a checkout** (2026-10-23; `m0-contract.md` "Amendment: diff a
  branch or PR without a checkout"):
  - "Diff branch / PR…" sits next to "Diff checkout…". It suggests linked PRs first, then
    ticket-matching branches, and accepts a PR number, URL or branch name as free text.
  - The repo is picked from the PR's owner/repo across the checkout and its submodules. Refs are
    fetched read-only, falling back to `refs/pull/<n>/head`. The base is explicit, else the PR's
    base, else `origin/main`.
  - The diff is `merge-base...head`, via the `range` mode of `GetCheckoutDiff` /
    `GetCheckoutFileDiffText`.
  - Stored as `refTargets` / `selectedRef` on the `chatDiffTargets` row.
  - Not done: auto-refetch, and an unbind control in the UI.
- **Row icon follow-ups** (2026-10-23):
  - A `PrViewer` RPC returns the cached `gh api user` login, so rows and the chat strip show "you"
    for your own PRs.
  - Linear rows are icon clusters (assignee · needs · status) with narrow-width dropping, and their
    totals text moved to the tooltip.
  - `?` has a Plan group and "Open the PR page".
  - Items with several PRs show up to 3 PR icons plus "+N".
- **Canvas view and chat auto-classification removed** (2026-10-23; restore point: tag
  `pre-canvas-removal` at `8bdce21d`; 44 files, −10,869 / +1,278):
  - `OverviewMode` is List / Outline. A saved `viewMode:canvas` loads as Outline.
  - Classification is gone: the RPCs, the classifier, the startup repair and reclassify passes, the
    header category/origin chips and override menu, and Reclassify.
  - List grouping is Repo / Ticket. Expanded List rows lost "launched via" and the tools/skills
    tallies.
  - The generic TypeSafe/Jev client in `typesafe.rs` is kept for reuse.
  - The PR page "Review chats" tab lists chats with a `review_launcher` link when no destination is
    set.
  - Stored classification and position files are left on disk, unread.
- **Forgiving agent tools** (2026-10-23; `agent_tools::normalize_arguments`, `proto/src/agent.rs`):
  - Agents often call zeron-agent tools before their schema loads. These tools now derive an
    idempotency key when it's omitted: `bind_chat_worktree`, `set_status`,
    `associate_current_chat`, `resolve_user_action_request`, and `create_user_action_request`
    when it has a `dedupeKey`.
  - Aliases are accepted: text/title/question → request; id/requestId → userActionRequestId;
    itemId/workItem → workItemId; snake_case → camelCase.
  - Argument errors list the tool's parameters with an example call.
  - Keys stay required for create_chat, create_child, propose_instruction, artifact_publish and
    create_user_action_request without a `dedupeKey`.
- **Review chat replaces Review; My PRs sidebar deleted** (2026-10-23; `ui/src/review_chats.rs`):
  - A PR's review chat is a live chat assigned to its review item, or one linked with source
    `manual` or `review_launcher`. When one exists, the PR page row's primary action is "Open
    review" (title plus live state), and Review moves to the row menu as "Start another review".
  - Enter on a PR row opens the review chat, else starts a review; ↑↓ move the row highlight.
  - The same "being reviewed" marker shows on the outline row and in the chat strip.
  - The overview "My PRs" sidebar code is deleted; old UI-flag keys still load.
  - Authored rows get the same Review / Open review actions, with Fix CI kept when CI fails. When
    the viewer authored the PR (the cached `gh api user` login matches the `gh pr view` author),
    the launcher seeds a self-review prompt; otherwise it keeps the reviewer prompt.
- **Links open in Zeron's side browser** (2026-10-23; Shell `open_web_reference`, pure `reference_open`):
  - With `openWebLinksInZeron` on, every web and PR link (PR page, outline icons, chat strip, chips,
    transcript and artifact links) opens in the side browser. A tab already on that page is focused
    instead of opening a duplicate.
  - The side pane exists only on the Chat route. From the PR page, Overview or Outline, a link
    switches to the PR's review chat, else the selected chat, with the browser beside it; with
    neither, it opens externally.
  - ⌘-click, or "Open in External Browser" in the outline PR menu, always opens externally.
- **Renamed to Creel, user-facing** (2026-10-23; decisions under Parked → "Rename the Zeron fork"):
  - Name: about 190 visible strings, both plists ("Creel", `creel.icns`), packaging (`Creel.app`;
    the dev script builds `Creel Dev.app`), user docs and the agent instructions' prose. The guard
    test `crates/ui/tests/brand_name.rs` fails if a user-facing "Zeron" comes back.
  - Logo: `dist/brand/` (generator, SVGs, PNGs) and `dist/macos/creel.icns`, with a simplified mark
    at 16 and 32 px. The in-app logo is `icons::logo(appearance)`: steel on light, moss on dark.
  - Left as Zeron on purpose: release file names (the update manifest matches on them), the
    stored-instruction preamble (hashed), and every identifier, environment variable, data path,
    bundle id, keychain entry, `zeron-artifact:` link and `zeron-agent` tool.
  - The wordmark is live Copperplate text; outlining it waits on a licence check.
- **Renamed to Mast** (2026-10-26; Mikey disliked Creel because it sounds like "creole"):
  - Name: about 330 more strings, both plists ("Mast", `mast.icns`), packaging (`Mast.app`, dev
    script `Mast Dev.app`), docs and READMEs. `brand_name.rs` now fails on a user-facing "Creel" too.
    The same identifiers as above stay "zeron".
  - Mark: a masthead light on a pole with two shrouds, a crow's-nest ring and three agent nodes.
    The light is a thin-rim glass lens with a moss → clover → cobalt gradient; the app icon has a
    metal finish (silicon, magnesium, fir, moss) on a steel tile. Only Cofactr brand colours.
    Generator: `dist/brand/generate.py` then `build-assets.sh`; the in-app marks are
    `crates/ui/assets/icons/mast-logo-{light,dark}.svg`, rendered in colour through `img()`.
  - `dist/brand/status/` holds the five lamp colours (idle, working, ready, input, error). Exploration pages and generators are in the
    Zeron Dev item's artifacts (`sources/mast-final/`).
- **Status light** (2026-10-26; `crates/ui/src/status_light.rs`):
  - The masthead light shows the most urgent state across chats: error (cadmium) > needs you
    (bromine; an awaiting-input chat or an open Required Action other than "Ready to merge") >
    ready (cobalt; an open "Ready to merge") > working (clover) > idle (clear glass). The pure
    `app_light` picks it; `Shell::sync_status_light` runs on state changes and the 1 s ticker.
  - Dock: icon swapped via `applicationIconImage` (embedded 512 px PNGs in
    `crates/ui/assets/dock/`) and a badge counting awaiting + errored chats + open Required
    Actions; debounced about 1 s, only changed parts touch AppKit. In-app logos use
    `Shell::logo`, with ten per-state marks generated by `dist/brand/generate.py`.
  - Settings → Notifications → "Live app icon" (default on); off restores the bundle icon, clears
    the badge and shows the plain logo. No pulse while working (a repeating animation redraws
    every frame).
- **Known test failures** (pre-existing, not regressions):
  - Engine lib, on main `3801cb5b`: `capture_source_context_reads_the_dispatch_checkout`,
    `local_opening_tail_arrives_before_full_mirror_and_keeps_all_history`.
  - Engine lib, flaky while Zeron itself is running (likely the live instance lock):
    `instance_lock::holder_probe_reports_pid_without_disturbing_the_lock`.
  - Engine integration, flaky under parallel load (passes alone):
    `local_first::headless_sign_out_closes_joined_edge_rooms_and_stops_daemon`,
    `message_queue::restart_restores_the_queue_frozen_until_an_explicit_send` (sometimes passes).
  - Engine integration, verified failing on `a3651a9d` before remove-canvas:
    - `e2e::generated_image_is_materialized_before_publication_and_survives_reopen`
    - `external_import_sync::sync_picks_up_a_closed_message_but_defers_the_new_open_reply`
    - `local_first::online_runtime_shutdown_stops_edge_workers_and_retires_the_graph`
    - `m5c_accounts_uploads_titles::titling_e2e_names_chat_and_renames_worktree_branch`
    - `message_queue::restart_restores_the_queue_frozen_until_an_explicit_send`

## Housekeeping (2026-10-25)

- All 80 feature worktrees under `.worktrees/zeron/` were removed and their local branches deleted.
- The 26 merged feature branches and the 193 copies of upstream branches were deleted from
  `origin`. Upstream's tags are still there, and so are ours (`pre-linear-integration`,
  `pre-canvas-removal`).
- Leftovers were saved to `~/zeron-dev-app-backups/leftover-patches-2026-10-25/`:
  - uncommitted edits found in the `work-outline-boundary-moves` and `pr-sync-create-fix`
    worktrees (both branches were already merged, so the edits are probably abandoned);
  - the unpushed `pr-sidebar-removal-work` commit, which `d7a30add` already contains.
- GitHub Actions are turned off on the GitHub repo. The workflows are upstream's release, deploy
  and TestFlight jobs, which would fail without upstream's secrets.
- The README (en and zh-CN) now describes the app (Creel then, Mast now), with the lockup from `docs/media/readme/`. The
  screenshot block is commented out until Mikey adds `outline.png`, `pr-page.png` and `plan.png`
  there. `.github/FUNDING.yml` is gone.
- `/Applications/Zeron.app` (upstream 0.2.59, bundle id `sh.zeron.app`) went to the Trash. Old
  backup binaries were pruned to the last three installs.

## Working model (since 2026-10-20)

Mast development is organised in the Work Outline under the root item "Zeron Dev"
(`48ef4b86c9967626ba1b6372e4cdfa66`, Space agent-mode-tools) with children Release, Bugs, Features,
Parked. A coordinator chat on that root starts child chats on the right items with
`create_chat { workItemId }`, polls `work_outline_read_subtree` for "Ready to merge" actions, and is
the only chat that merges, installs, edits this file, and pushes. The coordinator does the release
itself; there is no separate Release chat (Mikey, 2026-10-20). Feature and bug chats push their
branch and raise "Ready to merge: <branch>".

Coordinator tips:
- Never block on a child with a foreground `wait_for_turn`, because Mikey keeps giving new work
  while children run. A child that runs its tests in the background goes idle before it is done,
  so idle doesn't mean finished. Children also push early checkpoints, so a push doesn't mean
  finished either. In a background shell, watch the branch's sha on origin (`until git ls-remote
  origin <branch> | grep -q -v <last-sha>; do sleep 60; done`), and on each change read the subtree
  for the "Ready to merge" action. That action is the real done signal.
- After a release, delete the merged branch from `origin`, then remove its worktree and local
  branch.
- `work_outline_read_subtree` cuts long action `detail`s short (`detailTruncated`). For the full
  text, `read_chat` the `originChatId`.

The peon tray (`peon_tray.py`) was retired on 2026-10-20. Zeron's own banners and chimes replace it.

## Known gaps

- Visual compare:
  - Dragging the Markdown scrollbar doesn't sync the panes.
  - Diffs of renders taller than 8000 CSS px are cut off without a notice.
  - Option revisions recorded before 2026-10-20 can't be compared across revisions.
- Linear GraphQL (projects, attachments) unverified against the real API.

## Next, agreed but not started

- **Artifacts phase 3:** D (structured questions → Required Actions: blocking ones each own an
  action, the rest roll up into "N open questions in <plan>", dedupe by question id, resolution
  both ways) + C (revision diffs and a drift signal).
- **Artifacts phase 4:** G (comments on HTML anchors).
- **Artifacts phase 5:** F (side-by-side options + pick) and H (canonical plan with the linked
  Linear project shown next to it; no mirroring into Linear).
- **Chat header / side-panel revamp** based on the outline (item path, instructions state,
  execution root). The work-context strip shipped; the rest is waiting on Mikey's approval.

## Parked (do not start unasked)

See `parked-proposals.md`: active-checkout branch tracking, Linear v2 automatic project sync and
status write-back, a "Waiting for input" group (declined), Tod-style action log / undo.

## How we work

- **Worktrees:** `/Users/mikey/Projects/agent-mode-tools/.worktrees/zeron/<branch>`, branched from
  the integration head. Seed the build cache with
  `cp -c -R target/debug/{deps,build,.fingerprint}` (APFS clones), add a `.workspace-root` file.
  Merge with `--no-ff` into `work-outline-implementation`. **Delete each worktree's `target/` after
  merging**; keep at least 100 GB free.
- **Subagents:** one narrow task per agent with clear file ownership; read files in ≤200-line
  slices (they stall on huge files like `overview.rs` / `shell.rs`); commit early and often.
- **Tests:** UI tests need `--test-threads=1`. Time-limit with `perl -e 'alarm N; exec @ARGV' cargo
  test …` (macOS has no `timeout`). Known pre-existing failures are listed in the memory notes and
  `m0-contract.md`; they are not regressions.
- **Install:** `cargo build -p zeron`; back up the current binary to `~/zeron-dev-app-backups/`;
  `cp target/debug/zeron "$M/.zeron.installing" && mv -f` it to
  `/Applications/Mast.app/Contents/MacOS/zeron`; `codesign --force --sign - --identifier
  sh.zeron.app.dev` (sign the bundle path); push `work-outline-implementation`. Earlier bundles:
  `Zeron Dev.app` (until 2026-10-23) and `Creel.app` (until 2026-10-26), kept only as rollbacks.
- **Never restart Mast automatically.** Install, then tell Mikey what to restart and test.
  Restarting kills agents running inside Mast, so commit WIP first.
- **Xcode license prompts** after Xcode updates block git/cargo: Mikey accepts, or use
  `DEVELOPER_DIR=/Library/Developer/CommandLineTools`.

## Hard rules for agents

- No GUI launches, example binaries, or screen capture; verify UI with headless gpui tests.
- Never print secrets; never touch `~/.claude` / `~/.codex` config or sessions, or Codex
  computer-use.
- Never write to live `~/.zeron` stores; inspect with read-only sqlite `.backup` copies.
- No real Linear/GitHub API calls in tests. Nothing is ever written to Linear.
- Never commit or push to `main`.
- Registry operation ids must be ≤ 64 characters; hash long keys with `derive_id`.

## Diagnostics

- Outline key handling: grep `work_outline_keys` in `~/.zeron/logs/zeron-headed.log`.
- Linear PR attachments: log target `linear`.
- Pre-settle registry DB backup:
  `~/zeron-dev-app-backups/local-profile-docs-2026-10-12-pre-settle.sqlite3`.
