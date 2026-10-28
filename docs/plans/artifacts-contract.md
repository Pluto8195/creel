# Planning artifacts v1 — wire contract (phase 1: A + B)

Wire contract commit: `8e3bd0f1` (branch `artifacts-engine`). Code:
`crates/proto/src/artifacts.rs` (+ `artifacts/tests.rs`), tool names in
`crates/proto/src/agent.rs` (`agent_tools::tools::ARTIFACTS`), RPC names in
`crates/rpc/src/lib.rs` (`methods::*ARTIFACT*`). Capability: `artifacts-v1`
(`capabilities::ARTIFACTS_V1`). Agent conventions (binding for agents):
`docs/planning-artifact-conventions.md` v1. Design: `docs/plans/planning-artifacts-design.md`.

## Decisions

- **Separate stream, not a Work Outline snapshot field.** Artifacts change often (every publish,
  every watcher revision) and carry up to 50 revisions each; folding them into
  `WorkOutlineSnapshot` would re-emit the whole outline on every artifact write. `WatchArtifacts
  {spaceId}` emits `ArtifactsSnapshot {spaceId, revision, artifacts}` instead. The UI subscribes
  next to `WatchWorkOutline` and joins on `workItemId` / `chatId`.
- **Location.** Artifacts live in the work item's canonical checkout:
  `<canonical>/.zeron/artifacts/<folder>/<file>`.
  - Canonical checkout = the MAIN checkout (never a worktree) of the work item's resolved
    execution configuration's primary repository (`resolveExecutionConfiguration`; the primary
    binding, else the only binding). No binding: the Space folder's repository main checkout.
    A Space folder that is not a git checkout is used as is.
  - `<folder>` = `<slug>-<first 8 of work item id>` (sanitized title), stored on the artifact
    (`folder`) and reused by every later artifact of the item, so renames never move files.
    Chats without a work item: `_chats/<chatId>`. Registered with neither: `_space`.
  - `.zeron/` is appended once to the file named by `git rev-parse --git-path info/exclude`
    (submodules included). `.gitignore` and tracked files are never touched.
- **Identity.** `artifactId = derive_id("<scope>\0<normalized title>\0<role>", "artifact")` where
  scope is `item:<workItemId>`, else `chat:<chatId>`, else `space:<spaceId>`. Same (scope, title,
  role) → same artifact, new revision when the content changed; otherwise a new artifact.
- **Registry.** Row kind `artifacts`, fields exactly `ArtifactView` (metadata only). Revisions
  newest first, at most 50, trimmed oldest first until the op fits the 16 KiB cap. Notes are cut to
  200 characters, titles to 200. Only the owning device writes content-bearing fields; `ArtifactPin`
  writes `pinnedAsPlan` (LWW) from any device. The kind is part of the Work Outline kind set, so a
  deleted Space cascades and the orphan sweep covers it.
- **Snapshots.** Every revision's bytes are stored once at `<profile store>/artifacts/<artifactId>/
  <sha256>` on the owning device. Other devices read through the forwardable `ArtifactContent`.
- **Plan.** One pinned plan per scope (work item, else chat). Pinning unpins the others in one
  registry batch.

## RPCs

| Method | Params | Reply | Routing |
|---|---|---|---|
| `WatchArtifacts` | `{spaceId}` | stream of `ArtifactsSnapshot` | local replica, not forwardable |
| `ArtifactContent` | `{artifactId, revision?, companion?, targetDeviceId?}` | `ArtifactContentReply` | forwardable (owning device) |
| `ArtifactRegister` | `{spaceId, workItemId?, chatId?, path, title?, kind?, role?, targetDeviceId?}` | `ArtifactPublishReply` | forwardable (Space device) |
| `ArtifactPromote` | `{artifactId, targetDeviceId?}` | `{artifactId, promotedPath}` | forwardable (owning device) |
| `ArtifactPin` | `{artifactId, pinned}` | `{artifactId, pinned, unpinned[]}` | local |
| `WatchArtifactRenderRequests` | `{client?}` | stream of `ArtifactRenderRequest` | IPC-only (UI → own engine) |
| `ArtifactRenderResult` | `{requestId, result? \| error?}` | `{accepted}` | IPC-only |

- `ArtifactContent`: `revision` omitted = the current stored file (`revision` in the reply is the
  current revision number, or `0` if the file changed after it and the watcher has not recorded it
  yet). Text kinds come back `encoding: "utf8"`, images `"base64"`. `companion` names an entry of
  `companions` or the preview file name (current copy only). `wrapperHtml` is reserved and `null`
  in phase 1: the UI wraps fragments with its own visualize wrapper.
- `ArtifactRegister`: `path` absolute or relative to the Space folder; any regular file on the
  owning device's filesystem outside the source deny-list (below). Imported as revision 1 by the local user, `originPath` kept.
  The same (scope, title, role) again adds a revision when the content changed.
- `ArtifactPromote`: copies the current revision to `<canonical>/docs/plans/<file name>` (creates the
  directory). The same content already there is reused; a different file is never overwritten
  (`<stem>-2<ext>`, `-3`, …). Left uncommitted; `promotedPath` recorded on the artifact.
- Errors are `artifact/<code>: <message>`: `notFound`, `invalid`, `forbidden`, `denied` (source deny-list), `tooLarge` (10 MiB),
  `wrongDevice` (call the owning device), `renderUnavailable`, `renderTimeout`, `renderFailed`,
  `rateLimited`, `io`, unknown → `internal`.

## Types (`zeron_proto::artifacts`, camelCase JSON)

- `ArtifactKind`: `markdown | htmlFragment | htmlPage | mermaid | image | options`, unknown →
  `unknown`. Also accepts `html-fragment` / `html-page`. Default from the extension
  (`ArtifactKind::from_path`): `.fragment.html` / `.preview.html` → fragment, `.html` → page,
  `.md` → markdown, `.mmd` / `.mermaid` → mermaid, png/jpg/gif/webp/svg → image, `.options.json`
  → options, anything else → markdown.
- `ArtifactRevision {id (1-based number), hash (sha256 hex), note, at (ms), by (WorkOutlineActor),
  sizeBytes, op?}`.
- `ArtifactView {id, spaceId, deviceId, workItemId?, chatId?, title, kind, role?, folder,
  storedPath, originPath?, previewPath?, companions[], currentRevision, revisions (newest 50,
  newest first), pinnedAsPlan, promotedPath?, archivedAt?, createdAt, updatedAt}`.
  `url()` = `zeron-artifact:<id>`.
- `ArtifactsSnapshot {spaceId, revision, artifacts}` with `for_work_item` (plan first),
  `for_chat`, `plan_of`.
- Render: `ArtifactRenderRequest {requestId, artifactId?, title, kind, path, encoding, content,
  companions[{path, content}], viewport {width, height} (default 1280×800, clamped 200–4096),
  theme light|dark|system, viewports[{width, height, label?}]?, themes[]?}`. `viewports` /
  `themes` (v1.1) are the lists to render; empty reads as `[viewport]` / `[theme]`. The UI renders
  viewport × theme (viewport-major), at most `MAX_RENDER_IMAGES` = 4 (`plan_renders`), and
  answers `ArtifactRenderCheckResult {screenshotPngBase64? (the first image), screenshots[{viewport
  (label), width, height, theme, pngBase64}], errors[], warnings[], layoutWarnings[{kind, selector,
  textSample ≤ 80 chars, viewport, theme}]}`. Layout warnings come from the probe's per-element
  metrics classified in `crates/ui/src/artifacts/layout_heuristics.rs` (`clippedText`,
  `overlappingSiblings`, `narrowTextColumn`, `widerThanParent`, `fixedWidth`); at most 50 per
  call, identical errors across renders reported once.
- Helpers: `artifact_id`, `publish_operation_id` (`artifact-<32 hex>`, ≤ 64 chars),
  `artifact_links` (Markdown `[label](zeron-artifact:…)` links), `work_item_folder`,
  `chat_folder`, `OptionsManifest::parse`, `count_open_questions`, `count_unverified_claims`.

## Agent tools (`zeron-agent` MCP, token-scoped)

Names in `agent_tools::tools::ARTIFACTS`. Listed only with an engine-minted token; every call is
scoped to the token's Chat (and its work item).

- `artifact_publish {path, title, kind?, role?, note, pinAsPlan?, idempotencyKey}` →
  `{artifactId, revision, url, storedPath, changed}`.
  - `path`: absolute, or relative to the Chat's cwd. Source: any regular file on the owning
    device's local filesystem (it is copied into the artifact store). Symlinks are resolved first
    and the deny-list applies to the real path; refused with `artifact/denied`: anything under
    `~/.ssh`, `~/.gnupg`, `~/Library/Keychains`, `~/.aws`, `~/.config/gh`, `~/.netrc`,
    `~/.docker/config.json`, files named `.env` / `.env.*`, files over 10 MiB, and non-regular
    files (directories, fifos, devices). The same rule covers `ArtifactRegister`, the fragment's
    preview sibling, and options companions. Remote-device paths are never read (`wrongDevice`).
  - Scope: the Chat's work item, else the Chat itself (`_chats/<chatId>`).
  - `foo.fragment.html`: a sibling `foo.preview.html` is copied too (`previewPath`). Publishing a
    `foo.preview.html` whose fragment sibling exists publishes the fragment.
  - `options`: the manifest is parsed and every referenced fragment (relative path, no `..`) is
    copied next to it (`companions`). Revisions track the manifest bytes only in phase 1.
  - Unchanged content records no revision (`changed: false`). The same `idempotencyKey` returns the
    revision it recorded first (`publish_operation_id(chat, key)` is stored on the revision).
  - `pinAsPlan: true` pins it and unpins the scope's other plans.
  - A file name already used by another artifact in the folder gets `-<id6>` before its suffix.
- `artifact_get_plan {}` → `{artifactId, title, kind, revision, storedPath, url, content,
  openQuestions, unverifiedClaims}`, or `{artifactId: null, note}` when no plan is pinned (the RPC
  transport cannot carry a bare `null` result, so the conventions' `null` is this object). Phase 1 counts questions and unverified claims with
  the simple syntax counters above (the full parser is phase 3).
- `artifact_render_check {artifactId | path, viewports?, viewport?, themes?, theme?}` →
  `{artifactId?, errors, warnings, layoutWarnings, screenshots[{viewport, width, height, theme,
  png}], screenshot (the first image, v1 compatibility)}`. `viewports` defaults to
  `[{800,800,"narrow"}, {1440,900,"wide"}]`; a single `viewport` replaces that list; `themes`
  defaults to the current theme only (a single `theme` is `themes: [theme]`). At most 4 images per
  call; a truncation is reported in `warnings`. MCP content: a text block with the JSON (image
  bytes elided), then for each screenshot a text label (`Screenshot: <viewport> (<w>x<h>), <theme>
  theme`) followed by an `image` block (PNG). Warnings are heuristics; agents look at every image.
  An `artifactId` must belong to
  the Chat's scope (`forbidden` otherwise). Rate limit: 20 per Run per minute (`rateLimited`).
  No headed UI subscribed → `artifact/renderUnavailable`; no answer in 30 s →
  `artifact/renderTimeout`.

## Engine behavior

- **Marker fallback.** Every completed assistant segment is scanned (the same choke point that
  mines PR links): `[t](zeron-artifact:<path>)` imports the file once per (chat, path) into the
  chat's scope with title `t`; `zeron-artifact:<id>` links are left alone (they resolve to the
  existing artifact); Codex `visualize` references register as `htmlFragment` artifacts once per
  (chat, path). Best effort; failures are logged.
- **Watcher.** The owning device polls stored files every 2 s; a content change not made by Zeron
  becomes a revision noted `edited without a note` by the system actor.
- **Render routing.** The headed UI keeps `WatchArtifactRenderRequests` open; the engine sends each
  request to the newest live subscriber, waits up to 30 s for `ArtifactRenderResult`, and returns
  the screenshot to the agent as MCP image content.

## UI notes

- Open `zeron-artifact:<id>` links from `ArtifactsSnapshot` (any device) and read content with
  `ArtifactContent` (forward to `deviceId`). `zeron-artifact:<path>` links: match `storedPath`,
  `originPath`, or `previewPath`; otherwise show the file as before.
- Pinned plan: `ArtifactsSnapshot::plan_of(itemId)`; pin/unpin with `ArtifactPin`.
- Render requests: render exactly as the artifact tab (fragment wrapper, sandbox, theme), snapshot
  offscreen (no screen capture), answer every request, including failures (`error`).

## Links inside rendered artifacts

Classified by `artifacts::links::classify`; HTML documents and Markdown share it.

- `path`, `path:line[:col]`, `path:line-line`, `path#L12`, `zeron-file:` open in the file viewer at
  that line. Resolution: canonical checkout (parent of `.zeron/`), then the chat's checkout
  (absolute paths under either root); a relative path names the same file in both. `..`, `./`,
  queries, and unresolvable paths are blocked. `zeron-artifact:` opens that tab; `http(s)` follows
  the web-link preference; `mailto:` is external; `#id` stays in the page.
- Blocked (`file:`, `javascript:`, `data:`, unknown schemes, other local files): navigation is
  cancelled and the sidebar notice says so.
- HTML mechanism: the sandboxed srcdoc frame cannot navigate the host and cannot resolve relative
  hrefs, so an injected click handler posts the raw `href` to the outer document, which clicks a
  hidden `zeron-link:<encoded href>` anchor. The WKWebView navigation delegate
  (`browser::document_view::document_decision`) cancels every non-`about:` navigation and reports
  link clicks as `BrowserEvent::DocumentLink`; the Shell routes them. Markdown uses the preview's
  file-link handler, which routes the same classes and keeps `#anchor`/doc-relative behavior.
- Tests: resolver, delegate decision (fake navigation per scheme), and Markdown routing unit tests;
  no real-click test (headless only).

## Amendment: phase 3, questions → Required Actions, revision diffs and drift (2026-10-27)

Code: `crates/proto/src/artifacts/questions.rs`, `crates/engine/src/artifact_questions.rs`,
`ArtifactsService::{answer_question, drift, plan_questions}`, UI in `crates/ui/src/artifacts/`.

- **Questions as data.** `extract_questions(kind, text)` reads Markdown `> [!question]` blocks and
  HTML `data-zeron-question` elements into `PlanQuestion {id, text, blocking, status, blocks}`.
  The engine and the UI cards use it for counts; a question without a `Q-n` id is skipped. It
  replaces nothing: `count_open_questions` stays the cheap counter for `artifact_get_plan`.
- **Where state lives.** Answers are a new `ArtifactView.answers: [{id, answer, at, by,
  revision}]` (≤ 24 entries, 600 chars each, written alone by `set_artifact_answers` so a device
  that does not own the content never rewrites revisions). The document itself is never edited:
  the agent folds the answer in with `status: resolved` + `decision:`. This replaces the design
  doc's "Zeron writes the answer into the artifact" (no write races with the watcher, no
  surprise diffs).
- **Required Actions.** The engine (`ArtifactQuestionSync`, every 2 s on the artifact's owning
  device, and immediately after `ArtifactAnswerQuestion`) reconciles pinned plans (or
  `role: plan`) that belong to a work item:
  - blocking open question → its own request, dedupe key `artq:<artifactId>:<Q-n>`, text
    `Q-n: <question>`, info = plan title/revision, `Blocks:` and the artifact link;
  - every other open question → one request, key `artq:<artifactId>:rollup`, text `N open
    question(s) in <title>`, info lists `Q-n: text` per line. Requests cannot be edited, so a
    changed list resolves the old request (system actor, note "Superseded: …") and raises a new
    one;
  - dedupe is by key across revisions; operation ids derive from (artifact, question, revision),
    so republishing never duplicates. A question removed, resolved in the document, answered, or
    no longer blocking resolves its request (system actor, note says why);
  - origin: the Chat that published the latest revision (else the artifact's first publisher)
    when live and in the Space, so Reply & resolve reaches it; otherwise a system-origin request.
- **Resolution both ways.** Card answer → `ArtifactAnswerQuestion {artifactId, questionId,
  answer}` (local registry write, not forwardable; the UI also sends `Re: "Q-n: text": answer` to
  the publishing Chat) → the next reconcile resolves the request ("Answered in Mast: …"). A
  request resolved by a person (Resolve, or Reply & resolve) records an answer: the note, else
  "Resolved in Required Actions". The roll-up answers every question its info lists. A resolution
  by the system actor never counts as an answer. Plain Resolve does not message the Chat; only
  Reply & resolve (existing behaviour) and the card do.
- **Agents.** `artifact_get_plan` adds `answeredQuestions: [{id, answer, at, revision}]`.
  Conventions: answers arrive as `Re: "Q-n: …": <answer>` messages.
- **Drift (C).** `ArtifactDrift {artifactId, targetDeviceId?}` → `{artifactId, commitsSince,
  branch?, latestCommitAt?, since}` (forwardable, owning device). Rule: commits whose committer
  date is at or after the latest revision, on the checkouts the item's chats work in (chat cwd
  plus bound diff targets), skipping checkouts on `main` / `master` / detached HEAD; the checkout
  with the most commits wins. Second signal, computed in the UI from the row: answered
  questions the document does not yet mark resolved and no revision followed
  (`unreflected_answers`). Shown as a badge with a tooltip; no LLM.
- **Known limits.** Answers written from two devices at once are last-writer-wins; the checkout
  scan is limited to the owning device; unrelated commits on a shared feature branch count.

## Amendment: phase 4, comments on HTML and Mermaid artifacts (2026-10-28)

- **Storage.** Comments live on the artifact row as `comments` (at most 40, oldest resolved
  dropped first, else oldest), never in the file and never a revision. Written with the `comments`
  field only, from any device, like `answers`. Only HTML (fragment/page) and Mermaid artifacts
  take comments; Markdown, images and options are refused.
- **Anchors.** In order of stability: an element `id` or `data-zeron-anchor` value; for Mermaid
  `mermaid:<node id>`; a text-quote selector (`prefix` / `exact` / `suffix` over the visible text,
  whitespace collapsed); a CSS `selector` as a last resort, evaluated by the frame only.
- **Outdated.** A comment whose anchor is not found in the current revision is *outdated*. This
  is computed (`proto::resolve_anchor`) from the current revision's source, never stored; outdated
  comments stay listed. `artifact_get_plan` reports `anchorStatus` `found`, `outdated`, or
  `unknown` (source unreadable, e.g. owned by another device).
- **RPCs.** `ArtifactAddComment {artifactId, anchor, quote, text}` and `ArtifactResolveComment
  {artifactId, commentId, resolved}` (`resolved: false` reopens). Both are local registry writes,
  not forwardable. Text is clipped to 600 chars, quote to 240.
- **Resolution.** By a person (RPC, `resolvedBy: "user"`), or by the agent: `artifact_publish`
  takes `addressedComments: ["C-1", ...]`; after the publish those open comments become Resolved
  with `resolvedBy: "agent"` and `addressedIn` = the resulting revision. Unknown ids are ignored.
- **Agent view.** `artifact_get_plan.comments` lists the open comments only: `{id, text, quote,
  anchor, revision, at, anchorStatus}`.
- **Chat message.** The UI messages the publishing chat with `proto::comment_message`:
  ``Comment C-n on *title* rN · `id` ("quote"): text``.

## Amendment: phase 5, option picks and the canonical plan (2026-10-28)

Code: `crates/proto/src/artifacts/picks.rs`, `ArtifactsService::pick_option`,
`ArtifactQuestionSync::after_pick`, `LinearService::project_detail`; UI in
`crates/ui/src/artifacts/{plan_linear.rs, tab/options_view.rs, tab/plan_view.rs}`.

- **F: picks live on the row.** `ArtifactView.picked: {optionId, label, note?, at, by,
  revision}` (an `options` artifact only), written alone by `set_artifact_pick` like answers
  (`ArtifactPickOption {artifactId, optionId?, label, note?}`, local, not forwardable; no
  `optionId` clears the pick). The manifest and fragments are never edited, and a new revision of
  the options keeps the pick (it records the revision it was made on). The UI sends the label (it
  has the manifest); the engine caps label/note at 400 chars, id at 80.
- **Message.** The UI then messages the publishing chat, same target and path as a question answer:
  `Picked option B for "<title>": <label>` + `\n\nNote: <note>`; `Changed my pick to option C …`
  when changing; `Withdrew my pick of option B for "<title>": I haven't chosen yet.` on un-pick.
- **Required Actions.** A manifest `question` that is a plan question id (`Q-2`) also records the
  pick as that question's answer on the item's pinned plan (`Picked option B: Grouped`), so the
  question card and its Required Action resolve through the phase-3 path; changing the pick
  rewrites the answer, clearing removes it (the Required Action comes back with the plan's next
  revision, because create ids are per revision). Open requests on the item with dedupe key
  `artopt:<artifactId>` (agents may raise one for an unanswered choice) resolve on a pick.
- **Agents.** `artifact_get_plan` adds `pickedOptions: [{artifactId, title, optionId, label, note,
  at, revision}]` for every options artifact in the caller's scope that has a pick, with or without
  a pinned plan. Agents continue with the picked option.
- **H: the canonical plan is the pinned plan** (`pinnedAsPlan`, one per work item; pin/unpin from
  the Artifacts section or the tab). No second marker was added. With Linear on in the Space and a
  linked Linear project (`linearProject` reference), the plan's tab shows a project panel under the
  header (collapsible), and the item details show it under the pinned plan plus an "Open plan" link
  in the header.
- **Linear project detail** (read-only): `LinearProjectDetail {projectId, force?, cachedOnly?}` →
  `{id, name, url, state?, progress?, targetDate?, lead?, updates[≤3 {id, body (≤600), health?,
  author?, createdAt}], updatedAt}`, device-local. Served through an `SwrStore` (same TTL as the
  lists, persisted in `linear-project-details.json`); the UI reads the stored copy first
  (`cachedOnly`), then the cached/fresh read, and keeps data through a failed refresh. No mutation
  or mirroring into Linear exists. The GraphQL (`project { progress targetDate projectUpdates }`)
  is unverified against the real API, like the rest of the Linear projects/attachments queries.
- **Not done.** Option picking from inside an HTML frame (needs phase 4's frame ↔ host bridge);
  multi-device picks are last-writer-wins.
