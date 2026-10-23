# pr-dash: port-ready spec for the two PR tabs

Source of truth: `/Users/mikey/Projects/cofactr/workspace/scripts/shell/bin/pr-dash` (Python 3; logic and the embedded HTML/JS template are both in this one file). Line refs below are to that file ("L123") unless a path is given. Skill doc: `.claude/skills/pr-dash/SKILL.md`. `tools/github-overlay/src/dash/{dashboard.js,frame.js,*.html}` are only a Chrome native-messaging relay (op `page`/`prs`/`ticket`); they contain NO PR logic. The extension starts `pr-dash --native-host` with no other flags (`tools/github-overlay/native/install.sh:70`), so extension defaults == CLI defaults.

I ran `pr-dash --json --no-cache --fast` (and `--incoming`) read-only to confirm bucket output shape. It matches the rules below. (`--fast` skips only the mergeable retry.)

## 0. Pipeline (what a pure function must do)

1. `me` = `gh api user --jq .login` (L4046). On failure it falls back to the literal string `"@me"` (then no login ever equals me; every "mine" comparison silently fails). With `--author` or the person picker, `me` for normalization = the chosen person's login (L4226-4229: `normalize(n, author, ...)`), while the page header/labels use the viewer.
2. Stage one: search -> list of PR ids. Stage two: batched read of full payload per PR (5 PRs per request alias batch, 8 concurrent). Both tabs fetched concurrently.
3. `normalize()` per PR (L1148-1412) -> classify (L1516) -> `resolve_files` (page files >100) -> `resolve_inert` (diff discount) -> `annotate_stacks` -> `resolve_mergeable` (retry UNKNOWN; reclassify).
4. Filters (`shape`, L4244) default to none (repo/no-drafts/drafts-only/needs/bucket all off). Python sort is overwritten by the page's JS sort.
5. Page-level: hide-drafts (default ON), group-by-bucket (default ON), sort updated_at desc, no bucket chips active.

## 1. The two tabs and their queries

Search qualifier strings (L4021-4043), with defaults `--state open`, no `--since`, no `--org`, no `--query`:

- Tab "Authored by me" (`role="author"`): `author:{me} is:pr sort:updated-desc is:open`
- Tab "Review requested" (`role="reviewer"`), TWO searches whose results are unioned (dedupe by PR url, first-seen wins, so the `review-requested` hits come first):
  - `review-requested:{me} is:pr sort:updated-desc is:open`
  - `reviewed-by:{me} -author:{me} is:pr sort:updated-desc is:open`
  (Reason: `review-requested:` drops you once you submit a review.)
- `{me}` is the resolved login, not `@me`. Order of terms: selector, `is:pr`, `sort:updated-desc`, `is:open`.
- No org filter (default `--org` None): includes any org/personal repo the user is involved in. No `archived:` filter (open PRs in archived repos would appear), no `draft:` filter (drafts are fetched, hidden client-side), no label filter. State is open only; MERGED/CLOSED bucket logic exists but only reachable via `--state`.
- Search: GraphQL, `search(query:$q, type: ISSUE, first: 100, after: $after)`, paginated via `pageInfo.hasNextPage/endCursor` until `limit` (default 200) reached. Per query truncated to 200; then union, then truncated to first 200 (`fetch`, L451-458). So with >200 incoming, the `reviewed-by` tail is dropped first. Null nodes skipped.

Stage-one GraphQL (L190-203):
```graphql
query($q: String!, $after: String) {
  search(query: $q, type: ISSUE, first: 100, after: $after) {
    issueCount
    pageInfo { hasNextPage endCursor }
    nodes { ... on PullRequest { number url updatedAt repository { nameWithOwner } } }
  }
}
```
Stage two: one request per 5 PRs, aliased `p{i}: repository(owner:"o", name:"n") { pullRequest(number:N) {FIELDS} }` (L206-215). A missing node is dropped silently.

## 2. Fields fetched per PR (PR_FIELDS, L119-160)

```
number title url state isDraft
additions deletions changedFiles
createdAt updatedAt mergedAt closedAt
mergeable mergeStateStatus reviewDecision
repository { nameWithOwner }
headRefName baseRefName headRefOid
author { login }
labels(first: 15) { nodes { name color } }
assignees(first: 10) { nodes { login } }
reviewRequests(first: 20) { nodes { requestedReviewer { __typename ... on User { login } ... on Team { name } } } }
reviews(first: 100) { nodes { author { __typename login } state submittedAt bodyText } }     # OLDEST 100
comments(last: 40) { totalCount nodes { author { __typename login } createdAt } }           # issue comments, NEWEST 40
files(first: 100) { totalCount pageInfo { hasNextPage endCursor } nodes { path additions deletions changeType } }
reviewThreads(first: 100) { nodes { isResolved isOutdated comments(last: 20) { nodes { author { __typename login } } } } }
commits(last: 1) { nodes { commit { statusCheckRollup { state
   contexts(first: 100) { nodes { __typename
      ... on CheckRun { name conclusion status detailsUrl }
      ... on StatusContext { context state targetUrl } } } } } } }
```
Extras: files beyond 100 are paged with `files(first:100, after:$cursor)` (cap 900 extra files; L950-987). `contexts` MUST be selected: `statusCheckRollup.state` is wrong without it (L181-186). `mergeable` UNKNOWN retry (L344-372): up to 3 attempts, first 60 UNKNOWN PRs per attempt, sleep 1.2s before each re-query of `mergeable mergeStateStatus`; if the new `mergeable` is truthy, overwrite `mergeable` + `merge_state` and re-classify. May remain UNKNOWN. Volatile fields (state, isDraft, mergeable, mergeStateStatus, reviewDecision, commits/rollup) are re-read even on a cache hit; a pure function can just read everything fresh.

Optional REST: `GET repos/{o}/{r}/pulls/{n}` with `Accept: application/vnd.github.v3.diff` per PR with `code_churn > 0` (for the inert-line discount, section 4.1).

## 3. Per-PR derived fields (normalize, L1148-1412)

Definitions: `author` = `author.login` or "". `is_bot(login, typename)` (L866): `typename=="Bot"` OR lower(login) ends with `[bot]` OR in BOT_LOGINS OR ends with `-bot`.
BOT_LOGINS = {greptile-apps, macroscope-app, macroscope-dev, coderabbitai, github-actions, dependabot, sentry-io, codecov, vercel, renovate, claude, cursor, devin-ai-integration} (exact lowercase match; L218-232). `--bots` flag (default off) un-hides bots only in `reviewers`.

### 3.1 my_review / my_review_at (L1153-1162)
Iterate `reviews.nodes` in API order (chronological), only those whose author login == me:
- state in {APPROVED, CHANGES_REQUESTED, DISMISSED}: overwrite `my_review`, `my_review_at`.
- state COMMENTED: set only if `my_review` is still None.
- PENDING etc. ignored.
So APPROVED then COMMENTED stays APPROVED; COMMENTED then APPROVED becomes APPROVED.

### 3.2 reviewers (L1164-1206)
`states: login -> state` over reviews in order; skip if login empty, == author, == me, or (bot and not --bots). State in {APPROVED, CHANGES_REQUESTED, DISMISSED} overwrites; COMMENTED sets only if login not already in `states`. (A later COMMENTED never downgrades an earlier verdict; a later APPROVED replaces an earlier CHANGES_REQUESTED.) PENDING/other states ignored. Not time-filtered, not commit-staleness aware.

`requested` (pending review requests): for each `reviewRequests.nodes[].requestedReviewer`: `login = rev.login or rev.name` (Team gives its `name`); skip if empty; if login == me: `requested_from_me = True` and skip; skip bots (unless --bots); else append. (Team requests are listed as reviewers by team name; they never set `requested_from_me`.)

Merge into `reviewers = [{login, state}]`:
- each `states` entry: state becomes `RE_REQUESTED` if login in requested set, else the stored state;
- each `requested` login not in `states`: `{login, state:"REQUESTED"}`;
- sort key `(state != "CHANGES_REQUESTED", login.lower())` (blockers first, then alphabetical).
`blocking_changes = any(r.state == "CHANGES_REQUESTED")` (so a re-requested reviewer, state RE_REQUESTED, no longer blocks; `reviewDecision` is NOT used for blocking because GitHub keeps CHANGES_REQUESTED after re-request).
Note: `reviewers` excludes me and the author, so in the incoming tab it is everyone else; my own verdict is only in `my_review`.

### 3.3 CI (L1211-1257)
`rollup = commits.nodes[0].commit.statusCheckRollup`; if absent (no commits or no CI) `ci = None`.
Else `ci = rollup.state` (one of SUCCESS FAILURE ERROR PENDING EXPECTED) and, over `contexts` (first 100 only):
```
live = { c.name for CheckRun c if c.conclusion != "CANCELLED" }
saw_cancel = False; pending = False
for c in contexts:
  CheckRun:
    if conclusion == "CANCELLED":
        saw_cancel = True
        if c.name not in live: cancelled.append({name, detailsUrl})
        continue                       # never counted failing
    if status != "COMPLETED": pending = True
    if conclusion in (FAILURE, TIMED_OUT, STARTUP_FAILURE, ACTION_REQUIRED): failing.append({name, detailsUrl})
  StatusContext:
    if state in (PENDING, EXPECTED): pending = True
    if state in (FAILURE, ERROR): failing.append({name: context, url: targetUrl})
if saw_cancel:   # ONLY then is the rollup state overridden
    ci = "FAILURE" if failing else "PENDING" if pending else "CANCELLED" if cancelled else "SUCCESS"
```
No check is ignored by name; there is no required-vs-optional distinction (any non-required failing check makes rollup FAILURE). NEUTRAL/SKIPPED/STALE/SUCCESS conclusions are non-failing. When `saw_cancel` is false `ci` is GitHub's raw rollup state even if `failing` is empty (reason text then "CI failing"). `ci` can be `CANCELLED` only through the override.

### 3.4 Inline threads (L1259-1302)
`open = threads where !isResolved`; `live = open where !isOutdated`; `outdated_threads = len(open) - len(live)`.
For each live thread, `comments` = its (last 20) comments; `last_mine` = index of the last comment whose author login == me (else -1):
- `last_mine < 0`: `unreplied_threads += 1`; and **only if role == "author"** also `unresolved += 1` (so on your own PR every never-answered live thread counts, bot-authored or not; on someone else's PR it does not count).
- else if any comment after `last_mine` is by someone who is not me AND not a bot (`is_bot(login, __typename)`): `unresolved += 1; reopened_threads += 1`.
- else `answered_threads += 1` (you had the last word, or only bots replied after you).
(Bot filter here applies regardless of `--bots`.)

### 3.5 Top-level unanswered comments (L1304-1343)
Build `timeline = [(ts, login, typename)]`:
- every issue comment (`comments.nodes`: createdAt, author login or "", typename);
- every review with a `submittedAt`: if login == me -> add (any state/body, means "you spoke"); else only if `state == "COMMENTED"` AND `bodyText.strip()` non-empty (APPROVED / CHANGES_REQUESTED / empty-body reviews skipped).
Sort tuples (ISO string compare). `last_mine_at = max(ts where login == me)` or `""`.
For each entry with `ts > last_mine_at`, login non-empty, not a bot: `addressed_to_me = (login == author)` if role == reviewer else `(login != me)`; if so push login.
`unanswered_comments = len(list)` (count of entries, not distinct people); `unanswered_comment_from = sorted(set(list))`. Note `my_last_comment_at = last_mine_at`.

### 3.6 Diff (L894-933, L950-1100, SKILL.md "Diff counting")
Per file `classify_path(path)` on lowercased path: first of test, gen (gen patterns + lockfile patterns), docs match wins, else code. Patterns live in `/Users/mikey/Projects/cofactr/workspace/.claude/skills/pr-risk/scripts/diff_kinds.json` (copy them verbatim; regexes with search semantics):
- test: `(^|/)tests?/`, `(^|/)__tests__/`, `(^|/)test_[^/]+\.py$`, `(^|/)tests?\.py$`, `(^|/)conftest\.py$`, `\.(test|spec)\.[jt]sx?$`, `(^|/)(e2e|cypress|playwright)/`, `(^|/)factories?\.py$`, `(^|/)(test)?fixtures?/`, `(^|/)test(setup|utils?|helpers?|harness|support|fixtures?)[a-z0-9]*\.[jt]sx?$`, `(^|/)setuptests?\.[jt]sx?$`, `(^|/)(vitest|jest|playwright|cypress|karma)[.-][^/]*$`, `(^|/)(pytest\.ini|tox\.ini)$`
- gen: `(^|/)migrations/`, `(^|/)versions/[0-9a-f]{8,}_`, `(^|/)__snapshots__/`, `\.snap$`, `\.generated\.[^/]+$`, `(^|/)generated/`, `(^|/)\.(basedpyright|quality)/baseline\.json$`, `\.(svg|png|jpe?g|gif|ico|woff2?|ttf|pdf)$`, plus lockfile `(^|/)(package-lock\.json|pnpm-lock\.yaml|yarn\.lock|uv\.lock|poetry\.lock|cargo\.lock|pipfile\.lock)$`
- docs: `\.(md|mdx|rst|txt|adoc)$`, `(^|/)docs?/`, `(^|/)\.claude/`
`split[kind] = {additions, deletions, files}` summed from `files.nodes` (+ extra pages). For kind==code: if `changeType == "ADDED"` then `code_new_lines += additions`; else (MODIFIED/DELETED/RENAMED/COPIED/CHANGED/absent) `code_churn += additions+deletions` and path goes in `code_churn_files` (capped 50), `churn_by_file[path]`. `pure_addition = code_new_lines>0 and code_churn==0`.

#### 3.6a Inert discount (`resolve_inert`, L1054-1100; diff_kinds.py `parse_patch`/`inert_churn`)
For each PR with `code_churn > 0`, fetch the unified diff (REST, above). Per path with a head-side `+++` path: count changed lines (every +/- content line) and "inert" lines = blank, OR starts (after strip) with the language line-comment marker (by extension: `#` for py pyi sh bash zsh yaml yml toml tf tfvars ini cfg conf mk, basenames `makefile`/`dockerfile*`; `//` for ts tsx js jsx mjs cjs scss less go rs java kt swift c h cpp hpp cs; `--` for sql), OR inside a Python docstring (py/pyi) or `/* */`/JSDoc block (ts tsx js jsx mjs cjs), evaluated per hunk, each side separately (`_docstring_flags`, `_block_comment_flags`, diff_kinds.py L197-291). Only code-kind paths that are not new files (`--- /dev/null`) count. `code_inert_lines = sum`; `code_churn_raw = code_churn`; `code_churn = max(0, code_churn - inert)`; files whose inert count >= their churn are dropped from `code_churn_files`; `pure_addition` recomputed. If the diff request fails, churn stays raw. A port may skip this pass (churn then reads high on comment-heavy diffs); say so explicitly if skipped. This pass is skipped with `--fast`.

### 3.7 Other row fields
`repo` = name part of nameWithOwner; `draft = isDraft`; `labels` = names (HTML shows first 3); `ticket` = first match of `\b((?:ENG|COF|FND|ASK|OPS|BIZ)-\d+)\b` (case-insens) in title, else headRefName, uppercased. `stacked = baseRefName not in {main, master, develop, dev}`. Stacks (L1103-1145): PRs whose base == another fetched PR's head (same repo) form chains; roots numbered 1..n bottom to top, `stack_pos/stack_size/stack_id` (root url); computed before any filtering. `comments` = comments.totalCount. Linear project annotation is cosmetic.

## 4. Buckets

Bucket order = array order. Priority = first match. Labels/glyphs from L236-258.

### 4.1 Authored tab, `BUCKETS` (chip/group order)
| key | label | glyph |
|---|---|---|
| needs-you | Needs you | ● |
| comments | Comments to address | ○ |
| ready | Ready to merge | ✓ |
| merged | Merged | ◆ |
| no-reviewer | No reviewer | ∅ |
| in-review | In review | · |
| ci-running | CI running | ◔ |
| draft | Draft | ✎ |
| closed | Closed | ✕ |

(Note `merged` sits 4th, before no-reviewer; irrelevant for open-only.) Predicate, evaluated in this order (`classify`, L1516-1563):
```
if state == MERGED: merged          # unreachable with is:open
if state == CLOSED: closed          # unreachable
reasons = []
if !draft && review_decision == "APPROVED" && mergeable == "CONFLICTING": reasons += "approved, but has a merge conflict"
if ci in ("FAILURE","ERROR"): reasons += "{n} check(s) failing" (n = len(failing_checks)) or "CI failing" if n==0
if blocking_changes: reasons += "changes requested"
if reasons: -> needs-you          # drafts included (no draft guard on CI/changes)
if unresolved > 0 or unanswered_comments > 0: -> comments
if !draft && review_decision == "APPROVED" && ci in ("SUCCESS", None) && mergeable == "MERGEABLE": -> ready
if draft: -> draft
if reviewers is empty: -> no-reviewer
if ci in ("PENDING","EXPECTED"): -> ci-running
else -> in-review
```
Consequences: unapproved conflicts are NOT needs-you; `mergeStateStatus` (BLOCKED/BEHIND) never affects bucket; `ready` ignores required-checks/branch state; `ci` of CANCELLED or PENDING blocks `ready`; `draft` always beats `no-reviewer`; `mergeable == UNKNOWN` (after retries) is neither CONFLICTING nor MERGEABLE, so never conflicts and never ready. `ready` does not test `reviewers` (only GitHub's `reviewDecision`), while `no-reviewer` does: an approved PR that fails the ready test (e.g. CI pending or mergeable UNKNOWN) whose only approver is a hidden bot lands in `no-reviewer`.

### 4.2 Review-requested tab, `REVIEW_BUCKETS` (chip/group order)
| key | label | glyph |
|---|---|---|
| to-review | Waiting on you | ● |
| re-review | Re-review asked | ◑ |
| changes-requested | Changes requested | ○ |
| approved | Approved | ✓ |
| commented-by-you | You commented | · |
| draft-in | Draft | ✎ |
| merged | Merged | ◆ |
| closed | Closed | ✕ |

(The module docstring L23-29 and the python docstring names `changes-by-you`/`approved-by-you` are stale; code and SKILL.md use the keys above.) Predicate (`classify_incoming`, L1455-1513):
```
MERGED -> merged ; CLOSED -> closed
if draft: -> draft-in                       # before any verdict logic
mine = my_review                            # None | APPROVED | CHANGES_REQUESTED | DISMISSED | COMMENTED
replies_pending = unresolved>0 and answered_threads==0   # (truthy int/bool)
back_with_me   = replies_pending or unanswered_comments>0
decision = review_decision
if decision is None:                        # no required review
    states = {r.state for r in reviewers} ∪ {mine if mine}
    decision = "CHANGES_REQUESTED" if "CHANGES_REQUESTED" in states
               else "APPROVED" if "APPROVED" in states else None
blocking = blocking_changes or (mine == "CHANGES_REQUESTED" and !requested_from_me)
if blocking: -> changes-requested  ("changes requested by <logins in reviewers with CR, + 'you' if mine==CR>")
if decision == "APPROVED": -> approved     ("approved by <reviewer logins with APPROVED, + 'you'>")
if requested_from_me:
    if mine in (APPROVED, CHANGES_REQUESTED, COMMENTED): -> re-review
    if my_last_comment_at (non-empty) and !back_with_me:  -> commented-by-you
    -> to-review ("asked <rel(updated_at)> ago")
if back_with_me: -> to-review
if mine == "COMMENTED" or unresolved>0: -> commented-by-you
else -> to-review ("waiting on you")
```
Notes: only `decision == "APPROVED"` is ever tested after the blocking test (the `decision=="CHANGES_REQUESTED"` fallback is effectively dead). GitHub's `reviewDecision` is REVIEW_REQUIRED on most repos (observed), in which case another person's approval does NOT move the PR to `approved` unless GitHub says APPROVED. `blocking`/`approved` outrank "someone asked you". `my_review == DISMISSED` + requested_from_me falls to the comment checks -> `to-review`. A PR matched only via a Team request has `requested_from_me=False` and can only land in `to-review` ("waiting on you") / `commented-by-you` / `changes-requested` / `approved` / `re-review` is impossible (needs requested_from_me).
In this role `unresolved` only counts threads you have spoken on that a non-bot, non-you person replied to (3.4); `unanswered_comments` only counts the PR author's remarks after your last timeline event.

### 4.3 UI behaviour
- Default tab: mine. Tab 3 `reviews`. Per-tab bucket filter sets (`activeByTab`); default empty = show all. Chips (L2530): one per bucket in that tab's bucket order that has `count > 0` among rows admitted by the draft toggle (counts are over `admitted(DATA)`, not over the text filter); click toggles membership (multi-select, OR semantics); no "all" chip. Chip text = label + count. Active set is kept across refresh.
- "hide drafts" checkbox: default CHECKED (unchecked only when `--drafts-only`). `admitted(rows)` drops `draft==true` rows; chips, tab badges, window title, and the "N of M" counter all use admitted rows. This hides draft rows even in needs-you/comments (they still bucket normally).
- "group by status": default CHECKED. Grouped view: for each bucket of the tab's bucket list, in order, a header row `label · count` then that bucket's rows (rows already filtered/sorted). Ungrouped: single sorted list.
- Row filter order (render, L3359): stack isolation, project isolation, hide-drafts, active bucket chips, then text search: lowercased haystack `title repo head base ticket project.name "#number" author reviewer-logins... labels...` joined by space; every whitespace-separated term must be a substring.
- Summary text: `"{shown} of {admitted total} · +{add} −{del}"` plus `" · {excl} excluded"` if excl>0; add/del summed over the rows shown for the counted kinds (see 5).
- Tab labels: viewer: "Authored by me" / "Review requested"; other person: "Authored by {name|@login}" / "Review requested of {name|@login}".
- Diff mode selector (default `code only`): code | code+tests | everything.

## 5. Columns per row (L2951-2989, HTML columns: status dot, Repo, PR, Title, Diff/Churn, Reviewers|Author, CI, Open, Updated)

- Status: bucket dot (title = bucket label).
- Repo: `repo` name; PR: `#number` link.
- Title with meta tags in order: `draft` tag; stack badge `pos/size` (else `↵ base` if stacked); ticket tag (project > ticket); `all new files` if pure_addition; for bucket needs-you or comments ONLY, each `reasons` string as a warning tag; `conflict` tag if (`mergeable=="CONFLICTING"` or `merge_state=="DIRTY"`) and no shown reason contains "conflict" (so it appears on non-approved conflicted PRs); first 3 `failing_checks` (name linked to url); `"{n} cancelled"` tag if cancelled_checks; first 3 label names.
- Diff/Churn column. Code-only mode: header "Churn". If pure_addition: `⊕{compact(code_new_lines)} new`; elif code_new_lines>0: `{compact(churn)} ⊕{compact(new)}`; else `{compact(churn)}`. Other modes: `+{adds} −{dels}` summed over counted kinds. Then superscript `+{compact(excl)}` where `excl` = (additions+deletions) of kinds NOT counted (in code-only: test+gen+docs). Plus a segmented bar (churn, inert, new, test, gen, docs widths = lines/total, total = sum over all four kinds). `compact(n)`: <1000 -> n; <10000 -> `n/1000` one decimal with trailing `.0` dropped + "k"; else `round(n/1000)k`.
- Reviewers (authored tab): `reviewers` list rendered `glyph login`: APPROVED ✓ ok, CHANGES_REQUESTED ✗ no, COMMENTED … cm, DISMISSED ◌ rq, REQUESTED ○ rq, RE_REQUESTED ↻ cm; empty -> "—". Incoming tab: `@{author}` instead.
- CI: SUCCESS ✓, FAILURE/ERROR ✗, PENDING/EXPECTED ◔, CANCELLED ⊘ (gray), anything else/None "—". Tooltip lists failing check names (or "cancelled: ..." names).
- Open: `unresolved + unanswered_comments` (hot badge, else `·`); `+{answered_threads + outdated_threads}` superscript when >0. Tooltip text itemizes inline threads, top-level comments + who, answered, outdated.
- Updated: `rel(updated_at)` = `now|{m}m|{h}h|{d}d|{n}mo` with thresholds 60s/3600s/86400s/30d (months = 30d), JS clamps negative to 0. Created-at is fetched but not shown in the HTML table.

## 6. "Needs you" counts (L2492, L2572-2583)
```
ALERT_BUCKETS = { mine: ['needs-you'], incoming: ['to-review','re-review'] }
rows = admitted(DATASETS[tab])        # hide-drafts applied
urgent = count(rows where bucket in ALERT_BUCKETS[tab])
badge text = urgent ? "{urgent} / {rows.length}" : "{rows.length}"     # class "alert" when urgent
window title = (total ? "({total}) " : "") + "Pull requests · " + WHO    where total = urgent(mine) + urgent(incoming)
favicon red when total>0
```
NOT counted: `comments`, `no-reviewer`, `commented-by-you`. (The CLI `--needs` filter uses a different set: needs-you, comments, no-reviewer, to-review, re-review; L4253.) Counts ignore the text search and bucket chips.

## 7. Sorting

- Default sort: `updated_at` (parsed time) descending. Tie-break always `updated_at` desc (`render`, L3383-3388): comparator `x<y -> -dir; x>y -> dir; else b.updated_at - a.updated_at`; JS sort is stable so equal keys keep API/search order (search order = `sort:updated-desc` for the authored tab).
- Across buckets: with grouping ON the bucket order is the tab's bucket array order (4.1/4.2), within a group the active sort applies. Ungrouped: the sort applies globally (sorting by the status column sorts by bucket index).
- Sortable header keys and values (`val`, L2991): `bucket` -> ORDER index; `repo`,`title` -> lowercase string; `number`; `size` -> code_churn in code-only mode else sum(add+del) over counted kinds; `reviewers` -> first reviewer login lowercase (incoming: author lowercase), no reviewers -> "￿" (sorts last when asc); `ci` -> FAILURE/ERROR 0, PENDING/EXPECTED 1, CANCELLED 2, SUCCESS 3, other/None 4; `unresolved` -> unresolved+unanswered_comments; `updated_at` -> epoch ms. Clicking a header: same key flips direction, new key starts descending for updated_at/size/unresolved/number, ascending for others.
- Stack isolation: rows of that stack only, sorted by `stack_pos` asc (no grouping).

## 8. Edge cases and surprising or ambiguous behaviour

1. `me` fallback: if `gh api user` fails the string `"@me"` is used both in the search (which GitHub resolves) and in normalization comparisons (which then never match; every `my_*`/`requested_from_me` is empty/false).
2. `reviews(first: 100)` takes the OLDEST 100; PRs with more than 100 reviews miss the latest state. `comments(last: 40)`, `reviewThreads(first: 100)` with `comments(last: 20)`, `contexts(first: 100)`, `reviewRequests(first: 20)`, `labels(first: 15)`, `files(first: 100)`+paging: all silently truncate.
3. Review staleness: reviews are never invalidated by new commits (no head-SHA comparison); DISMISSED state replaces an earlier approval (still counts the reviewer as a reviewer); `reviewDecision` (GitHub) drives `approved/ready`; reviewers (per-user verdicts) drive `needs-you` changes-requested.
4. Re-request semantics: a re-requested reviewer shows `RE_REQUESTED` and stops blocking; GitHub's `reviewDecision` may still say CHANGES_REQUESTED.
5. Bots: excluded from `reviewers`/`requested` (unless `--bots`), from thread-reopen detection, and from unanswered comments; but a thread you never answered counts even if a bot started it; a bot reply after you does not reopen it. Bot-authored PRs (dependabot etc.) get no special handling in buckets; `author` shown as the GraphQL login (no `[bot]` suffix).
6. Self reviews: the PR author and `me` are excluded from `reviewers` on both tabs. Your own review submissions count as "you spoke" in the timeline regardless of body.
7. Drafts: authored drafts can be `needs-you` (CI fail/changes requested) or `comments`, otherwise `draft`; the approved+conflict reason and `ready` require not-draft. Incoming drafts are always `draft-in`. All drafts are hidden in the page by default and not counted anywhere while hidden.
8. Team review requests: shown as reviewer entries by team `name`; never mark `requested_from_me`; whether the `review-requested:{login}` search matches team-only requests is NOT verified here (GitHub's own behavior). CODEOWNERS only matters through `reviewDecision`.
9. `mergeable`: lazily computed; up to 3 retries for at most 60 PRs per round; may stay UNKNOWN (then neither conflict nor ready). `mergeStateStatus` is display-only (DIRTY -> "conflict" tag).
10. CI: GitHub's rollup state is trusted unless a CANCELLED check run exists, in which case state is recomputed from contexts (cancelled runs superseded by a same-named non-cancelled run are ignored; unreplaced cancelled runs are listed as `cancelled_checks`, not failing). Only first 100 contexts seen. No required/optional distinction, no per-check ignore list.
11. `ready` ignores branch protection details (BLOCKED, BEHIND, unresolved required conversations, stale approvals); it is only approved + green-or-no-CI + mergeable + non-draft, after no unanswered conversation.
12. `no-reviewer` is "reviewers list empty" after hiding bots/self/author, not "no reviewRequests". An approved PR whose only approver is a bot can be `no-reviewer` if it does not qualify as `ready`.
13. Incoming `approved`/`changes-requested` outrank a live request for you (PR disappears from `to-review`). `my_review == CHANGES_REQUESTED` blocks only while you are not re-requested.
14. `unresolved` has two roles: author role counts all never-answered live threads; reviewer role counts only reopened threads. `unanswered_comments` counts entries (not distinct commenters).
15. Counts exclude outdated threads (`isOutdated`) and answered-by-you threads (shown only as `+n` superscript).
16. Cache: node cache keyed on `updatedAt` with 1h TTL (staleness risk if a field changes without bumping updatedAt, which is why volatile fields are re-read). Irrelevant for a fresh pure fetch.
17. Search limits: 200 per query/union; GitHub search caps at 1000. Archived-repo and forks PRs are not excluded. Result order for the union is review-requested hits then reviewed-by hits.
18. The module docstring (L11-29) and `--needs` help text disagree with the HTML alert set (see section 6); SKILL.md bucket table matches code.
19. Dead/unused: `closedAt`, `assignees`, `headRefOid` (used only as inert cache key), `mergedAt` (reason text only).

## 9. Tab 3 and the person picker (5 lines)
- Review leaderboard: always `org:Cofactr`, 13 weeks (Mon start, local time), searches `org:Cofactr is:pr updated:{lo}..{hi}` week by week, per PR `reviews(last:100){author{__typename login} state submittedAt}`; a PR counts once per person per week per repo; excludes bots, the PR author, PENDING and unsubmitted reviews; `--repo` substring filter; rows ranked by total then name (L507-591, L3253-3322).
- Picker: `GET /api/prs?author=<login>` (op `prs` with `author`) re-runs both tabs for that login (`me` in normalize = that login); options = viewer first ("(you)"), then leaderboard people sorted by display name; login must match `[A-Za-z0-9](?:[A-Za-z0-9-]{0,38})`.
