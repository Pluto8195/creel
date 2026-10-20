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

- Visual compare: markdown panes don't scroll in sync; highlight and onion skin cover only the
  first 520×1400 px; options can't compare one option across revisions (`ArtifactContent` serves
  companion files for the current copy only); the per-kind mode memory doesn't survive a restart.
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
