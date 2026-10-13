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

Version 1 (read-only): Linear personal API key in the macOS keychain (bare `Authorization`
header, prompt on first Linear URL, re-prompt on 401); live issue chips (title, state, assignee,
priority) on work-item references and chat ticket chips, replacing the `linear` CLI; New Work
Item from Linear URL or "My open issues" (local field edits win over refresh); linked issue
summary in `work_outline_read_current`.

Version 2: a Linear inbox work item mirroring a simple filter with an accept-into action
(Tod "generators"); optional per-Space forward-only state write-back (Tod `linear_sync.rs`).
Study notes: `/tmp/tod-linear-study.md` (not versioned).
