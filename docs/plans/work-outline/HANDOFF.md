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
- **Visual compare for artifact revisions:** see "In flight" for status at the time of writing.

## In flight

- Visual compare (branch `artifacts-visual-compare`): kind-aware compare. Visual kinds render side
  by side with synced scroll, "Highlight changes" pixel diff, onion skin, and a Source tab.
  Markdown renders side by side with changed blocks and a section summary. Options compare one
  option at a time.

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
