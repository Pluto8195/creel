# Work Outline — parked proposals

Proposals investigated and deliberately parked (2026-10-13). Revisit on request.

## Active checkout tracking for workspace-root chats

Finding: 113 of 117 chats in the Cofactr workspace Space start at the superproject root.
`capture_source_context(request.cwd)` (`crates/engine/src/doc_host.rs`) rewrites each chat's
branch to the superproject branch before every turn, so every chip shows the same branch while
agents actually work in submodules or mid-chat worktrees (`.worktrees/<repo>/<task>`) that Zeron
never attaches. Branch-based PR inference then resolves every root chat to the same superproject
PR (`pr_ticket_cache.rs` `fetch_github_pr`).

Proposed, in order:
1. Track an "active checkout" per chat from tool-call cwd, `cd`, `git -C`, and `git worktree add`;
   drive the branch chip, Changes pane, and PR lookup from it. Never change `cwd` or a binding's
   execution root.
2. Show differing submodule branches for superproject-root chats; skip root-branch PR inference
   and match PRs by submodule remote.
3. Start Chat and Review PR create the primary repository's worktree up front without retargeting
   the Chat, tell the agent to work there, and record it as the chat's active checkout.
4. Later: per-member worktrees for multi-repository bindings.

## Linear integration (modelled on Tod, `~/Projects/tod`; no license, so reimplement)

Reviewed with the user 2026-10-13. Principle: nothing in the Outline is created or changed
automatically; Linear is opt-in per Space. A more automatic mode (project-linked items whose
children track the project's issues, Tod "generators") is deferred until the user asks.

Version 1 (read-only, per-Space opt-in, off by default):
- Connection: Linear personal API key in the macOS keychain (bare `Authorization` header,
  `viewer` check before saving, prompt on first Linear URL in a Linear-enabled Space, clear and
  re-prompt on 401, retry-after capped at 30 s). Never exposed to agents. Spaces with Linear off
  make no calls, show no Linear UI, and keep pasted Linear URLs as plain links.
- Linear panel (on demand, Overview right-side slot): My issues (assigned, open); My projects
  (lead/member) expandable to their open issues; search; already-linked markers with Reveal in
  Outline; refresh on open and manual only.
- User-initiated node creation only: Create work item (under the selected item or a chosen
  parent), drag a row onto an Outline item, Link to selected item (issue or project), and
  Create nodes for selected issues of a project.
- Linked nodes: live chip (state, assignee, priority, labels) is informational only. Title and
  description changes in Linear surface as "Updated in Linear · Update"; nothing changes until
  the user applies it (pending decision: prompt vs. automatic-until-edited, Tod's
  `ticket_modified_fields`). Status, children, placement, and archive state are never changed
  by Linear.
- Item title renames must not count as instruction drift (only instruction text should).
- Chat ticket chips use the same client in Linear-enabled Spaces; chats whose branch or messages
  name a ticket get a dismissible "Move under ENG-…" suggestion, never an automatic move.
- `work_outline_read_current` returns the linked ticket summary when the Space has Linear on.
- Nothing is written to Linear.

Open decisions: title format ("ENG-123 · title" vs. title); Linear text changes as prompt vs.
automatic-until-edited; agent ticket context (full description vs. id/title/status/url).

Later: automatic project sync (Tod generators), optional per-Space forward-only state write-back
(Tod `linear_sync.rs`), filter presets.
Study notes: `/tmp/tod-linear-study.md` (not versioned).
