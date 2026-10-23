# Zeron PR page and chat-PR linking audit

Repo: `/Users/mikey/Projects/agent-mode-tools/zeron`. All paths are relative to it unless absolute.
Registry evidence comes from a `.backup` copy at `/tmp/zeron-audit.db`. The snapshot was saved 2026-10-23 14:55 UTC.

## 0. Orientation

`crates/ui/src/shell/pr_review_ui.rs` (496 lines, about 100 of code) is only the "Start Review" glue. It does `Shell::start_pr_review`, `start_pr_review_in_item` and `open_review_pr_input`. The rest is tests. There is no single PR page. Four surfaces show PRs:

| Surface | Open with | Code |
|---|---|---|
| **A1. "My PRs" sidebar**, a left column of the Overview | **⌘P** (`OpenMyPrs`) or the toolbar toggle "My PRs (N)" | `crates/ui/src/overview.rs` |
| **A2. "PR review chats" page** (`Route::PrReviewChats`) | **⇧⌘P** (`OPEN_PR_REVIEW_CHATS_COMBO`), palette "Open PR review chats" or "Review PR…" | `crates/ui/src/chat_type_view.rs` and `chat_type_view/pr_destination.rs` |
| **A3. GitHub section** in the Overview's left (Inbox) column, plus per-item PR chips | always visible | `crates/ui/src/github/*` |
| **A4. Work-item PR chips and Details "Pull request"** | n/a | `crates/ui/src/github/{chip,row_icons}.rs`, fed by `workItemExternalReferences` |

Docs: `docs/pr-review.md`. It is accurate for the review launcher and the work-item PR links.

## A. PR page

### A.0 Queries (no GraphQL on the PR page; everything shells out to `gh`)

Two independent engine paths exist. They have separate caches and separate `gh` calls.

**Path 1: `PrTicketCache`** (`crates/engine/src/pr_ticket_cache.rs`). It feeds the ⌘P sidebar, ⇧⌘P "Review requested" and chat link status.
- Authored list, `fetch_my_open_prs` (1042-1047) calls `run_pr_search` (1060-1078). The command is `gh search prs --author=@me --state=open --json number,title,url,repository,isDraft,updatedAt --limit 50`.
- Review-requested list, `fetch_review_requested_prs` (1053-1058), is the same search with `--review-requested=@me` and the extra field `author`. It uses `--limit 50` and has no `reviewed-by` exclusion (comment 1049-1052).
- Per-PR detail, `fetch_github_pr_by_number` (1142-1160), runs `gh pr view <n> --repo <owner/repo> --json number,url,state,isDraft,reviewDecision,statusCheckRollup,reviews,title,headRefName,mergeable,reviewRequests,author` (`PR_VIEW_JSON_FIELDS`, line 50).
- A chat's linked PR (`fetch_github_pr_by_url`, 972) runs `gh pr view <url> --json …` with the same fields.
- A chat's branch guess (`fetch_github_pr`, 956) runs `gh pr view <branch>` in the chat's cwd. If that fails it tries each `.gitmodules` submodule dir.
- The CLI is resolved from the login-shell PATH (`gh_command`, 1165).

**Path 2: `PrReviewSyncService`** (`crates/engine/src/pr_review_sync.rs`). It feeds the GitHub section (A3), the PR chips and the review-sync loop.
- Search: `gh search prs <--review-requested=@me | --author=@me> --state=open --json number,title,url,repository,isDraft,updatedAt,author --limit 100` (`search` 611-643, `SEARCH_LIMIT` 65). It is cached for 60 s.
- Detail: `gh pr view <n> --repo … --json number,title,url,state,isDraft,author,reviewDecision,reviews,reviewRequests,statusCheckRollup,headRefName,baseRefName,updatedAt` (`PR_VIEW_FIELDS` 69, `detail` 646). It is cached for 2 min.
- Results are filtered to the Space's repo set (`space_list` 719-753, `repo_set` 776). The set is the Space checkout, submodules and nested repos, matched by `remote.origin.url`.
- Space lists are sorted by `updated_at` descending (751).

**GraphQL exists only for Linear attachments:** `crates/engine/src/linear/queries.rs:78`, `query ZeronLinearIssueAttachments($id: String!) { issue(id: $id) { identifier attachments(first: 50) { nodes { url title subtitle sourceType metadata updatedAt } } } }`.

### A.1 Data model

- `MyOpenPr` (`pr_ticket_cache.rs:221`): `number, title, url, repo ("owner/name"), is_draft, updated_at, author?`.
- `MyPrItem` (241) is `MyOpenPr` flattened plus `detail: Option<PrStatus>`. The review-requested list always has `detail = None` (588-599).
- `PrStatus` (77): `number, url, state (lowercased gh state), is_draft, review_decision, reviewers[] (non-bot, non-author review authors), checks: Option<Passing|Pending|Failing>, title, branch, mergeable (lowercased), has_reviewer_requested`.
- `PrAction` (67): `CiFailing, MergeConflict, ChangesRequested, NeedsReviewer, ReadyToMerge`.
- `ChatLinkStatus` (166): `pr, pr_links[{url, source?, detail?}], ticket, is_worktree, diff_stat, pr_source, ticket_source`.
- `PullRequestStatusSummary` (`crates/proto/src/pr_review_sync.rs:206`), used by A3 and A4: `ref, title, author, state (Draft|Merged|Closed|Open), ci_status, review_decision, my_review_state (Requested|ReRequested|Commented|Approved|ChangesRequested|None), requested_reviewers, updated_at, head_ref_name, base_ref_name`.

### A.2 "Needs you" and attention

- **PR-level action reasons**, `PrStatus::action_reasons` (`pr_ticket_cache.rs:97-122`). A draft gives none. The other rules:
  - `CiFailing` when checks are failing.
  - `MergeConflict` when mergeable is `conflicting`.
  - `ChangesRequested` when the review decision is `CHANGES_REQUESTED`.
  - `NeedsReviewer` when there is no reviewer request and no reviewers.
  - `ReadyToMerge` when there are no other reasons, the decision is `APPROVED`, checks are passing and mergeable is `mergeable`.
- **Severity sort**, `pr_action_severity` (`overview.rs:10624`): CiFailing and MergeConflict = 0, ChangesRequested = 1, NeedsReviewer = 2, ReadyToMerge = 3.
- **Toolbar badge** "My PRs (N)", `overview.rs:6497-6514`, with `actionable_pr_count` at 712. N is the number of authored PRs with at least one reason, excluding PRs whose only reason is ReadyToMerge, plus the length of the entire review-requested list. The button turns critical red and bold when N > 0.
- **Detail is only fetched for the authored list**, one PR per 20 s sweep tick (`sweep_one_my_pr_detail`, 728-758). This has a starvation problem; see C.9.

### A.3 A1: ⌘P "My PRs" sidebar (overview.rs)

Open mechanics:
- Shortcut id: `ShortcutId::OpenMyPrs`, default `"mod-p"`.
  - `crates/ui/src/settings.rs`: enum 880, ALL 917, label "Toggle My PRs" 967, default 1013, keymap field 1113, default 1196, get/set 1242/1284, settings JSON key `"openMyPrs"` 1696.
  - `crates/ui/src/settings/shortcuts.rs`: group "Panels" 488 and description 530. This file has no `ShortcutId` definition.
- Action `OpenMyPrs` is declared in `crates/ui/src/shell.rs:108`.
- Keybinding is `shell.rs:404` (`valid_or_default(&keymap.open_my_prs, "mod-p")`).
- Handler is `shell.rs:12379-12383`, which calls `Shell::toggle_my_prs`. That function is at `shell.rs:4689-4700` and is also called from the palette at `shell/command_palette.rs:240` (`Entry::MyPrs`, entries at 48 and 97).
  - Off the overview, it opens the overview and calls `Overview::open_pr_sidebar`.
  - On the overview, it calls `toggle_pr_sidebar`.
- Badge hint: `Shell::my_prs_shortcut_label` (`shell.rs:6669`).
- **⇧⌘P is a different shortcut.** `OPEN_PR_REVIEW_CHATS_COMBO = "mod-shift-p"` (`shell.rs:130`), a fixed, non-customizable binding (`shell.rs:458`). Its handler is `shell.rs:12361-12365` and it navigates to `Route::PrReviewChats` via `open_pr_review_chats` (`shell.rs:4758`).
- ⌘P is **not** handled in `overview.rs` itself. Only `Overview::{open_pr_sidebar, toggle_pr_sidebar}` are called by the shell.

`overview.rs` ranges:
- Doc comment on the sidebar, lines 13-15 and 54.
- `MY_PRS_REFRESH = 30s`, line 1990.
- Constants: `PR_SIDEBAR_W = 340`, 2362-2364; `PR_SIDEBAR_FLAG = "prSidebarOpen"`, 2366-2368; `PR_SECTION_COLLAPSED_PREFIX = "prSectionCollapsed:"`, 2371; `PR_SECTION_NEEDS_REVIEW = "needs-review"`, 2374; `PR_SECTION_NEEDS_ACTION = "needs-action"`, 2376.
- Fields: `my_prs`, `review_prs` and their pending/fetched_at fields (~2525-2540); `pr_sidebar_open`, `pr_sidebar_scroll`, `pr_search`, `pr_collapsed_sections` (2541-2552).
- Pure helpers: `pr_matches_search` 723 (repo, title, `#n`, head branch, author), `pr_sidebar_empty_message` 738, `pr_sidebar_open_from_flags` 756, `pr_repo_section_key` 762 (`repo:<owner/name>`), `pr_section_flag` 767, `pr_collapsed_sections_from_flags` 773, `pr_section_expanded` 963.
- **UI_FLAGS_FILE entry:** the file is `overview-ui-flags.json`, a string set. `UI_FLAGS_FILE` is defined at `crates/ui/src/overview_positions.rs:60`. In `overview.rs` it is used for:
  - Load, `ensure_prefs_loaded` (3152-3170). It only ever turns the sidebar on.
  - Write via `persist_ui_flag` (3279-3293), `set_pr_sidebar_open` (3240-3256) and `toggle_pr_section` (3268).
  - Entries are `prSidebarOpen` plus `prSectionCollapsed:<key>`.
- `set_pr_sidebar_open` (3240) zeroes both fetched_at stamps on open to force an immediate refetch.
- Fetch: `ensure_my_prs` (4477-4507, RPC `MyOpenPrs`) calls `ensure_review_prs` (4512-4541, RPC `ReviewRequestedPrs`). Both are TTL-gated by `MY_PRS_REFRESH` (30 s). They are driven from every overview render and from `refresh_tick` (3374, `IDLE_REFRESH` = 15 s, line 315), even with the sidebar closed, because of the toolbar badge.
- Engine side: `MY_OPEN_PRS` (`rpc.rs:3501`) waits up to 5 s (`MY_OPEN_PRS_INITIAL_WAIT`, `rpc.rs:85`) for the first authored search. `REVIEW_REQUESTED_PRS` (`rpc.rs:3513`) is a pure cache read.
- Render: `render_pr_sidebar` 8887-9099. It takes the section order and counts from the code:
  1. **"Needs my review (N)"** (`needs-review`, only if non-empty), from `review_prs`, in `gh search` order.
  2. **"⚡ Needs your action (N)"** (`needs-action`), authored PRs with a non-empty `action_reasons()`, sorted by `pr_action_severity` of the first reason (stable sort).
  3. **One section per repo** (`repo:<owner/name>`), a `BTreeMap` so repos are alphabetical. Within a repo the order is `gh search` order.
  - A PR can appear in both "needs action" and its repo section.
  - Header "My PRs <total>" or "<shown> of <total>" (8918-8922). Search box `pr_search`. Section fold persists.
- Row, `render_pr_item` (9165-9347):
  - Title (or `#n`).
  - Action badge, authored rows only, first `action_reason`, `pr_action_badge` (1927).
  - Second line: `#<n> · <state or "draft"/"open"> · by <author>` (review rows).
  - Linked chat line: `→ <chat title>` (via `chat_for_pr_url`, with a Manual pin and a provenance hover), and a click calls `jump_to_chat_from_pr` (3352, which opens the chat panel at right).
  - With no linked chat: "No open chat for this PR — click to open on GitHub". Review rows without a chat also get a **Review** button (`start_pr_review_from_sidebar`, 3308).
- Columns shown per PR: title, number, state, author (review rows), action badge, linked-chat title. There are no CI or review-decision columns in this sidebar. The detailed chips live in A3 and A4.
- Jump mapping `chat_for_pr_url` (4548-4564) scans `self.link_status` (a HashMap) with `status_has_pr_url` (1779, `link.url == url`).

### A.4 A2: ⇧⌘P "PR review chats" page (chat_type_view.rs)

- `ChatTypeView` (183). `ChatViewKind::PrReview` has category `"pr_review"` (26-48).
- **"REVIEW REQUESTED (N)" block**, `render_review_requested` (810-886). It is fed by `REVIEW_REQUESTED_PRS`, refreshed by `ensure_review_prs` (339-368) every 30 s (`REVIEW_PRS_REFRESH`, 212) while the page renders. Each row (`render_review_row`, 888-1000):
  - title;
  - subtitle `<repo>#<n> · by <author> · draft`;
  - **Open chat** (only if `existing_review_chat` matches);
  - **Review** and **Fix CI** buttons;
  - plus the URL / `owner/repo#n` box (`submit_review_input` 435).
- **Chat table**, shown when the Space has no PR-review-sync destination. The columns (1104-1126) are `CHAT / PR TITLE` (span 2), `REPOSITORY`, `PR`, `BRANCH`, `STATUS`, `UPDATED`.
  - Membership, `chat_visible` (137): no parent chat, not archived (unless Show archived is on) and `CHAT_CLASSIFICATION == "pr_review"`.
  - Sort, `sort_rows` (148): `last_message_at` or `created_at` descending, then title.
  - PR cell, `PrSummary::from_status` (67): only `status.pr_links.first()` or `status.pr`. It uses one PR per chat.
- **Destination mode**, `pr_destination.rs`. With a PR-review-sync destination in the current Space, the page lists the destination item's PR children (live chip plus Review / Fix CI filed under the item) with their chats nested. The chats come from the item's chat assignments (`item.chat_ids`, line 43-56), not from PR links.
- Opening: `Shell::open_pr_review_chats` (`shell.rs:4758`), `ensure_pr_review_chats_page` (4778), shortcut `shell.rs:458`, handler `shell.rs:12361`. The palette entries are in `shell/command_palette.rs` (46, 63, 95, 238, 309).

### A.5 A3/A4: GitHub section and chips

- `crates/ui/src/github/panel.rs`: lists "Review requested" and "My open PRs" (`prefs.rs:21-45`). It is refreshed **on open and by ↻ only**. Data comes from `PrReviewRequested`/`PrMyOpen` (client.rs 199-218).
- Chips use `PrStatus` with `STATUS_TTL = 2 min` and `STATUS_RETRY = 60 s` (`github/store.rs:14-16`).
- Per-row sync state: "Synced ✓" / "Pending first sync" / "Linked".
- Sync loop (engine): `SYNC_INTERVAL = 3 min`, `FIRST_SYNC_DELAY = 60 s` (`pr_review_sync.rs:53-55`, loop at 1524-1536). Each pass runs `sync_all()` (review-request items) and then `link_all_chat_pull_requests()` (B.4).
- Sweep cadence in `PrTicketCache`: `SWEEP_INTERVAL = 20 s`, `KEYS_PER_SWEEP = 3`, `CACHE_TTL = 180 s`, `MY_PRS_LIST_TTL = 60 s`, `SEARCH_FAILURE_BACKOFF = 90 s` (34-46, 208-214). Authored and review searches never run on the same tick (686-691).

### A.6 Review launcher (A, but also relevant to B)

- UI: `crates/ui/src/pr_review_launch.rs`. `PrReviewOperations` (22-73) keeps a **window-lifetime in-memory** `operationId` per (PR, action, agent config, work item). The ids are `pr-review-<uuid>`.
- Engine: `PrReviewLauncher::start` (`crates/engine/src/pr_review_launch.rs:693-907`). The chat id is `derive_id(operationId, CHAT)` (714). The stored reply (`<store>/pr_review_launches/`) makes a retry with the same op id idempotent (718-726).

## B. Chat to PR linking

### B.1 Row schema

Registry kind `chatPrLinks` (`KIND_CHAT_PR_LINKS`, `crates/doc/src/registry.rs:35`).
- **Row id** = hex SHA-256 of `chat_id \0 url` (`chat_pr_link_row_id`, 1861-1867).
- **Fields**, written by `upsert_chat_pr_link` (1522-1542): `chatId`, `url`, `source`. No timestamps and no PR number or repo columns.
- **`source`** is the snake_case `ChatLinkSource` (`crates/proto/src/entities.rs:180-186`): `"manual"`, `"created_in_chat"`, `"mentioned"`. Rank is mentioned 0 < created_in_chat 1 < manual 2 (198-217). `can_overwrite` is `new.rank >= current.rank`, so equal ranks overwrite.
- **URL is canonicalized** by `canonicalize_pr_url` (`crates/engine/src/chat_links.rs:67-79`) to `https://github.com/<owner>/<repo>/pull/<n>` and **lowercased entirely**. Real example in the registry: `https://github.com/cofactr/flagship/pull/6487`, while GitHub's own URL is `https://github.com/Cofactr/flagship/pull/6487`.
- **Legacy mirror on the chat row:** `chats.linkedPrUrl` + `linkedPrSource` (proto `Chat` 295-300). It holds the primary link (highest rank, then URL order), via `mirror_primary_pr_link` (`workspace_host.rs:1891`). `chats.linkedTicketId`/`linkedTicketSource` is a single slot (302-308).
- **Reads:** `WorkspaceHost::chat_pr_links` (`workspace_host.rs:715`) calls `effective_chat_pr_links` (1858-1889). It merges the rows with the legacy slot, takes the best source per canonical URL, and sorts by rank descending then URL.

### B.2 Writers and triggers

All write paths funnel through `WorkspaceHost::set_chat_link` (`workspace_host.rs:1619-1646`) to `add_chat_pr_link` (1650-1692). Clearing is manual-only. Mined writers can only add.

| Writer | Source | When | Code |
|---|---|---|---|
| Live tap, PR-create tool | `created_in_chat` | A tool call whose invocation text contains `gh pr create` / `gh pr new` or normalises to `createpullrequest` (`looks_like_pr_create`, `chat_links.rs:111-121`; text from `tool_call_invocation_text`, 127-143), then its `ToolResult.output`. Only `extract_pr_url(output)`, the FIRST URL. | `sessions.rs:2974-3005` (in `drive_run`) |
| Live tap, any other tool result | `mentioned` | Every distinct PR URL in any other tool result's output. | `sessions.rs:3000` |
| Live tap, message text | `mentioned` | `note_message` (`sessions.rs:1588-1597`) runs on user prompts and each completed assistant segment. It mines every distinct PR URL (`mine_pr_mention`, `chat_links.rs:159`) and one ticket id. | `sessions.rs:1594-1595` |
| Boot repair | `created_in_chat` or `mentioned` | `ExternalSessionImporter::repair_missing_links` runs once at engine start (`lib.rs:501-514`). | `external_import.rs:1497-1557`, `mine_links_from_entries` 2170-2204 |
| Work-item backfill | `created_in_chat` | Re-reads the stored transcript once per assigned chat (`created_pr_urls`, `work_outline_pr_links.rs:113`). | `work_outline_pr_links.rs:345-376` |
| Review launcher | `manual` | At launch, the PR URL on the new review chat. | `pr_review_launch.rs:843-850` |
| UI link editor / `SET_CHAT_LINK` | `manual` | User add/remove/clear. | `rpc.rs:3299-3363` |
| Ticket-from-PR stamp | (ticket only) `mentioned` | Inside `CHAT_LINK_STATUS`, when the chat has no ticket signal. | `rpc.rs:3248-3295` |

Chat title generation (`crates/engine/src/titles.rs:173-210`) only **reads** `chat_pr_links`, `linked_ticket_id` and the branch, to prefix the title with a ticket or PR number. It writes no links. It runs once, when the chat has no title yet.

### B.3 Match rules today

1. **PR URL anywhere in text.** The extractor is `extract_pr_urls` (`chat_links.rs:35-62`), a case-insensitive scan for `github.com/` followed by `<owner>/<repo>/pull/<digits>`. `/files`, `?x`, `#...` suffixes are dropped. Applied to:
   - every user and assistant message text (live), every tool result output (live);
   - transcript parts only in the boot repair, where `extract_pr_url` (first URL only, 28-30) is used per part.
2. **`created_in_chat`:** the invocation text is `gh pr create` / `gh pr new` / `create_pull_request`-shaped AND the result has a URL. Only the first URL of that output counts (`sessions.rs:2996`). No `isError` check in the live tap. The work-item backfill does check `is_error: false`.
3. **Branch inference** (display only, never persisted): `status_for_links` (`pr_ticket_cache.rs:433-546`) runs `gh pr view <chat.branch>` from `chat.cwd` (plus submodules). It is skipped when the cwd is access-blocked (home or protected folder). It never queues the lookup when durable links exist (505). A resolved PR whose URL differs is appended as an `source: None` entry. This is not written to `chatPrLinks`, and `plan_chat_links` ignores it.
4. **`chatDiffTargets` (`crates/proto/src/chat_diff_target.rs`, registry kind `chatDiffTargets`, row id = chat id):** fields `chatId`, `targets` (a JSON **string** of `[{path, source: agent|manual, repoId, worktreeId, boundAt}]`), `selected`, `ops`, `updatedAt`, `v`. It is **not used by PR linking at all**. No linking code reads it. It only drives the Changes pane and diff sync (`crates/engine/src/diff_targets.rs`, `diff_sync.rs`, `agent_tools.rs bind_chat_worktree`).
5. **Ticket rule** (for the chat to ticket link): the first `[A-Za-z]{2,6}-\d+` in text (`extract_ticket_id`, `pr_ticket_cache.rs:777-808`), most recent mention wins, `mentioned`. Fallbacks are the chat branch regex, then the PR's title and head branch (`rpc.rs:3248-3295`).
6. **Work-item attach** (`work_outline_pr_links.rs`): the chat's PR is linked to the item the chat is assigned to only if:
   - its source is `manual` or `created_in_chat` (`is_high_confidence`, 102);
   - the PR's repo is in the Space repo set;
   - no item in the Space links the PR yet;
   - the item is live;
   - the pass has not already linked this (chat, PR) (ledger marker).
   `mentioned` and branch inference never count. Limits: 50 transcript scans per pass (`MAX_TRANSCRIPT_SCANS_PER_PASS`, 45). The scan marker is set even when nothing is found (373).
7. **Which chats are scanned:**
   - Live mining runs for every chat Zeron drives, for the chat's whole life (not only the first N messages or the last turn).
   - Boot repair runs once per chat that has neither `linkedPrUrl` nor `linkedTicketId` and no marker file (`link_scan_markers/<chatId>`).
   - Imported or adopted external sessions are re-synced by `sync_stale_imports`, but that sync does not call the miners.
   - The work-item pass runs for chats assigned to a live item.
   - There is no "open PRs only" or "created by Zeron only" filter.

### B.4 Review launcher and existing chats

- `PrReviewLauncher::start` **never looks for an existing chat**. It always creates one: `chat_id = derive_id(operation_id, CHAT)` (`pr_review_launch.rs:714`, 821-833).
- The operation id is a fresh UUID per (PR, action, config, item) per window lifetime (`crates/ui/src/pr_review_launch.rs:55-59`). After a restart, or from another surface or window, a second review chat is made for the same PR.
- The only dedupe in the UI is the **Open chat** button (`existing_review_chat`, `chat_type_view.rs:222-232`). It matches `chat.linked_pr_url` (the primary mirror only) by exact string. The sidebar uses `chat_for_pr_url`. Neither does a case-insensitive match (C.2).
- The launcher writes the PR as a `manual` link on the new chat, and assigns the chat to the `workItemId` if given (`ensure_assignment`, 909). The `pr_review` classification is written to the sidecar (`rpc.rs:1685-1695`), not to `chatPrLinks`.

### B.5 How the UI reads links today

- `CHAT_LINK_STATUS` (`rpc.rs:3219-3298`). The Overview fetches it per **visible row** (`ensure_link_status`, `overview.rs:3927`, TTL `LINK_STATUS_REFRESH` = 45 s, line 112; rows come from `rows_cache`, 3390).
- Used for tile and row badges (`badges_for`, 4572; `effective_pr_links`, 1738), the chat header (`chat_metadata.rs`), and the detail link editor.
- ⌘P sidebar to chat: `chat_for_pr_url` (A.3).
- ⇧⌘P "Open chat": `existing_review_chat` (A.4).
- Chat table PR cell: first link only.
- Destination view: item chat assignments, not links.
- Repository topology: all link URLs per chat (`rpc.rs:4080-4098`).
- Work items: `workItemExternalReferences` (A4).

## C. Root causes of misses

Test case: chat `cb4e21587cb30673589bb3f52acd9d22` ("Begin workspace environment initialization"). The chat is assigned to work item `71e462b0…` (ENG-3012), has `branch: main` and `cwd: /Users/mikey/Projects/cofactr/workspace` (the superproject root), and `chatDiffTargets` bound manually to `/Users/mikey/Projects/cofactr/workspace/flagship`. The flagship checkout is currently on `michael/eng-3012-kit-line-estimate-calculation` (the head branch of #6488). Nothing in the linking code reads that diff target.

### Facts from the registry for this chat

- `chatPrLinks` rows:
  - `…/pull/6487`, `created_in_chat`.
  - `…/pull/6488`, `mentioned`.
- `chats` row: `linkedPrUrl` = 6487 (`created_in_chat`), `linkedTicketId` = **ENG-3103** (`mentioned`), **not ENG-3012**.
- Transcript message 13 (2026-10-21 15:25 UTC): a single `exec` ran `PR1=$(gh pr create …) && echo "$PR1" && PR2=$(gh pr create …) && echo "$PR2"`. Its output was `https://github.com/Cofactr/flagship/pull/6487\nhttps://github.com/Cofactr/flagship/pull/6488`. Both PRs were created in this one tool result.
- The ENG-3012 item (`71e462b0…`) links both PRs:
  - #6487 at 15:27:49 UTC, no `cachedTitle`, id casing `cofactr/…`. That is the chat route, 2m48s after creation (3-minute loop).
  - #6488 at 15:42:54 UTC, with `cachedTitle` "Calculate kit line ship estimates", id casing `Cofactr/…`. That is the Linear attachment route (15-minute loop).
- So the item-level links exist, but #6488 got there through Linear, not through the chat.
- 4 other chats also hold `mentioned` rows for these PRs.

### Which rule would have linked it

- **#6487:** the live PR-create tap (`sessions.rs:2995-2997`), `created_in_chat`. This worked.
- **#6488:** no rule produces `created_in_chat`. The create tap takes only the first URL (C.1). The `mentioned` row came from assistant text at 15:41.
- **Branch inference:** cannot match. `chat.branch` is `main` at the superproject root, and `gh pr view main` does not resolve to either PR.
- **Diff target:** would match, but it is unused (C.4).

### Ranked causes

**C.1 Multi-PR output keeps only the first URL (hit by the test case).** `sessions.rs:2996` calls `extract_pr_url` (`chat_links.rs:28-30`), which is `extract_pr_urls(..).into_iter().next()`. A tool call that creates two PRs (stacked PRs, a loop, `gh pr create` twice) yields one `created_in_chat` row. The rest degrade to `mentioned` at best. The same single-URL truncation exists in the boot repair (`external_import.rs:2182`, 2195), where `MinedLinks` holds only one `created_pr` or `mentioned_pr` per chat (2151-2159) and writes exactly one URL (1527-1541). `created_pr_urls` in `work_outline_pr_links.rs:131` also uses the single-URL extractor per tool part. Fix: use `extract_pr_urls` at all three sites.

**C.2 Case-sensitive URL matching in UI (affects every `Cofactr/...` repo).**
- Stored links are lowercased (`canonicalize_pr_url`). `gh search prs` and `gh pr view` return the real casing; I confirmed `https://github.com/Cofactr/flagship/pull/6487`.
- `status_has_pr_url` (`overview.rs:1779-1783`) does `link.url == url`. `existing_review_chat` (`chat_type_view.rs:226-230`) compares trimmed strings. Neither lowercases.
- Result: the ⌘P sidebar shows "No open chat for this PR" and ⇧⌘P never offers **Open chat** for any PR in an org or repo with an uppercase character, even when the link exists.
- Unit tests use all-lowercase `a/b`, which is why this was never caught. Fix: compare through `canonicalize_pr_url`, or compare by `(owner, repo, number)` lowercase.

**C.3 Sidebar jump only searches `link_status`, which only covers visible rows.**
- `chat_for_pr_url` iterates `self.link_status` (4548-4564), a `HashMap` filled by `ensure_link_status` for rows in `rows_cache` (3390, 4152). Archived, hidden (repo filter, hide stale), or never-rendered chats have no entry, so they cannot match.
- With several chats linked to the same PR (6487 has 4 chats), `find_map` over a `HashMap` returns an arbitrary one, so the "→ chat" target is unstable. Nothing prefers `created_in_chat`/`manual` over `mentioned`, newest, or non-archived.
- Fix: build an index in the engine or state keyed by canonical PR URL (`chatPrLinks` already has everything), with a deterministic preference order.

**C.4 `chatDiffTargets` and branches are ignored.**
- This chat's `branch` is `main`. It works from the superproject root and binds the flagship checkout as a diff target. 151 of 625 chats have branch `main` and 416 have no branch at all, so the branch rule rarely applies.
- `status_for_links` branch inference (the only branch rule) uses `chat.branch` and `chat.cwd`, never the diff-target checkout or its HEAD. A rule "diff target HEAD branch → `gh pr list --head <branch>` / `gh pr view <branch>` in that checkout" would link #6488, and via the branch name would give ENG-3012 (it contains `eng-3012`).
- Branch inference is also not persisted, so it never feeds `plan_chat_links`, the sidebar index or work items.

**C.5 Only `created_in_chat` / `manual` counts for work items, and #6488 was only `mentioned`.**
- `plan_chat_links` (`work_outline_pr_links.rs:204`) filters with `is_high_confidence`. Combined with C.1, the second PR was ignored for the chat route. It only reached the item through Linear.
- An item without Linear (or with a PR whose branch or title does not name the issue) loses the PR entirely.

**C.6 The boot repair is one-shot and heavily guarded.**
- `repair_missing_links` skips a chat that has `linkedPrUrl` or `linkedTicketId` (`external_import.rs:1503`). Because ticket mining sets `linkedTicketId` on almost any chat, a chat with an early ticket mention never gets a PR scan.
- It sets a marker file even when only the ticket matched (1550-1551), and it runs only at startup (`lib.rs:501-514`).
- Transcript sync of imported or adopted sessions never mines (`sync_stale_imports` has no miner), so a PR created after import is only seen if the chat is later driven by Zeron.

**C.7 The work-item backfill reads truncated tool output.**
- `store_transcript_reader` reads `SessionDoc` entries. Their tool `output` is the 160-char single-line summary (`crates/doc/src/parts.rs:18-66`, first non-empty line of outputs over 160 chars; the full text is in a sidecar `output_ref`).
- A `gh pr create` result whose first line is "Creating pull request for <long-branch> into main in Org/repo" loses the URL. The backfill only works if the output is short. In this chat both URLs fit (91 bytes), which is luck.
- The scan marker (`work_outline_pr_links.rs:373`) is set after the first scan, even if nothing was found. A PR created after that scan is only recovered by the live tap.

**C.8 Detection of "created" is heuristic.**
- `looks_like_pr_create` (`chat_links.rs:111`) matches only `gh pr create`/`gh pr new` substrings or a tool named like `createpullrequest`.
- Missed: `gh api repos/.../pulls -X POST`, a `gh` alias or wrapper (the repo's `gh-wait` style helpers), a web UI create, or a PR made by a subagent or another chat. Subagent tool results are not on the `drive_run` tap path (not verified in this pass).
- The live tap does not check `is_error`, so `gh pr create` that fails with "a pull request … already exists: <url>" is still stamped `created_in_chat` for a PR that already existed.

**C.9 Authored-list detail starvation (affects "needs action" accuracy, not linking).**
- `sweep_one_my_pr_detail` (`pr_ticket_cache.rs:728-758`) refreshes one PR per 20 s tick and always picks the first stale entry in list order, not the stalest. `CACHE_TTL` is 180 s.
- With more than about 9 authored PRs, early entries expire before the tail is reached, so tail PRs may never get `detail`. They then never show a badge and never count toward "Needs your action" or the toolbar count.
- `--limit 50` also silently drops PRs beyond 50, merged PRs vanish immediately from `--state=open`, and the review list never has detail.

**C.10 Mention noise degrades precision (the opposite failure).**
- 444 of the 490 `chatPrLinks` rows are `mentioned`. One chat has 103 links, and PR #6265 is linked by 9 chats. Any `gh pr view`/`gh api` listing output stamps links.
- Primary mirror and header show the highest-ranked link, then URL order, so a chat can display an unrelated PR.

**C.11 Ticket mismatch for the same chat.**
- Ticket mining is "first ID in each text, latest write wins" (`chat_links.rs:173-184`, `pr_ticket_cache.rs:777`). The chat is for ENG-3012 but its last messages discuss ENG-3103, so `linkedTicketId` became `ENG-3103` (`mentioned`).
- The assigned work item (ENG-3012), the PR head branch (`eng-3012`) and the PR titles are not consulted when a mention exists, because the PR-title and branch fallback only runs when no ticket exists at all (`rpc.rs:3252-3263`).

**C.12 The review launcher cannot reuse a chat.** See B.4. Starting a review from a different surface, or after restart, makes a new chat instead of opening the existing one.

**C.13 Link pass preconditions.**
- `link_chat_pull_requests` returns early if the outline is unavailable, the chat has no live assignment, or the Space repo set is empty. `space_repo_slugs` fails for a "broad workspace root" with "PR review sync needs a specific project folder", and `is_automatic_access_blocked` skips the Space.
- A PR already linked to any item in the Space is never added elsewhere, and moving a chat later does not move the link (`plan_chat_links` 198-208, 388-402).
- The first chat id wins a PR that two chats claim (`BTreeMap` order, 193-197).

## D. Pointers for changing this

| Goal | Start at |
|---|---|
| Multi-URL PR create | `chat_links.rs:28-30`, `sessions.rs:2995-2998`, `external_import.rs:2170-2204`, `work_outline_pr_links.rs:131` |
| Stable chat to PR index for the sidebar | New engine RPC over `effective_chat_pr_links` (`workspace_host.rs:1858`), replace `chat_for_pr_url` (`overview.rs:4548`) and `existing_review_chat` (`chat_type_view.rs:222`) |
| Case-insensitive match | `status_has_pr_url` (`overview.rs:1779`), `existing_review_chat` |
| Diff-target branch rule | `status_for_links` (`pr_ticket_cache.rs:433`) or a new persisted rule; inputs from `chat_diff_targets` (`workspace_host.rs:720`) |
| ⌘P handling | Shortcut `settings.rs:880..1696`, binding `shell.rs:404`, handler `shell.rs:12379`, toggle `shell.rs:4689`, sidebar `overview.rs:3240`, `8887` |
| Refresh cadences | `MY_PRS_REFRESH` (`overview.rs:1990`), `SWEEP_INTERVAL`/`CACHE_TTL`/`KEYS_PER_SWEEP` (`pr_ticket_cache.rs:34-46`), `SYNC_INTERVAL` (`pr_review_sync.rs:53`), `REVIEW_PRS_REFRESH` (`chat_type_view.rs:212`) |

Note: the Linear attachments GraphQL (`linear/queries.rs:78`) is not verified against the real API; its parser is lenient. The #6488 attachment did arrive in practice (cachedTitle present), so the query works at least here.
