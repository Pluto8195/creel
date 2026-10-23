# Work Outline — implementation handoff

Prepared 2026-10-14. Read this first in a new chat picking up the Work Outline work.

## Where things are

- **Repository:** `/Users/mikey/Projects/agent-mode-tools/zeron` (fork `Pluto8195/zeron`).
- **Integration branch:** `work-outline-implementation`, branched from `main` at `3801cb5b`. Pushed
  to `origin` as a backup. **Not merged to `main`; ask before merging.**
- **Parent repo:** `agent-mode-tools` branch `zeron-work-outline` pins the `zeron` submodule.
  Restore points: tag `pre-linear-integration` (`83f49b3d`) and parent commit `d9129f6`.
- **Original spec:** `work-outline-implementation-specification.md` (copy of Mikey's spec, plus an
  "Implementation contract amendments" section). The binding contract is `m0-contract.md` in this
  folder; it wins where it differs from the spec.
- **Installed build:** `/Applications/Zeron Dev.app`. Rollback binaries are in
  `~/zeron-dev-app-backups/` (one per install, named after the feature).

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
  - Launcher review chats are detected through the `pr_review` classification tag
    (`shell/work_context_ui.rs:28`). Removing Canvas classification has to replace this, e.g. with
    a launcher-sourced chat PR link.
  - Follow-ups: PR line counts / unresolved threads / conflicts; open-question counts in the
    planning strip; matching the chat's diff-target branch to PR heads; the strip in the overview's
    chat panel.
- **Known engine lib failures** (pre-existing on main `3801cb5b`, not regressions):
  `capture_source_context_reads_the_dispatch_checkout`,
  `local_opening_tail_arrives_before_full_mirror_and_keeps_all_history`.

## Working model (since 2026-10-20)

Zeron development is organised in the Work Outline under the root item "Zeron Dev"
(`48ef4b86c9967626ba1b6372e4cdfa66`, Space agent-mode-tools) with children Release, Bugs, Features,
Parked. A coordinator chat on that root starts child chats on the right items with
`create_chat { workItemId }`, polls `work_outline_read_subtree` for "Ready to merge" actions, and is
the only chat that merges, installs, edits this file, and pushes. The coordinator does the release
itself; there is no separate Release chat (Mikey, 2026-10-20). Feature and bug chats push their
branch and raise "Ready to merge: <branch>".

Coordinator tips:
- Never block on a child with a foreground `wait_for_turn`, because Mikey keeps giving new work
  while children run. A child that runs its tests in the background goes idle before it is done,
  so idle doesn't mean finished. The reliable "done" signal is its branch appearing on origin: in
  a background shell, run `until git ls-remote origin <branch> | grep -q .; do sleep 60; done`,
  then read the subtree for the Ready to merge action.
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
- **Remove Canvas auto-classification** (Mikey asked to start cutting it now that the outline is
  the deliberate structure). Proposal given, not started.
- **Chat header / side-panel revamp** based on the outline (item path, instructions state,
  execution root). Design proposed, waiting on Mikey's approval.

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
  `/Applications/Zeron Dev.app/Contents/MacOS/zeron`; `codesign --force --sign - --identifier
  sh.zeron.app.dev`; push `work-outline-implementation`.
- **Never restart Zeron automatically.** Install, then tell Mikey what to restart and test.
  Restarting kills agents running inside Zeron, so commit WIP first.
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
