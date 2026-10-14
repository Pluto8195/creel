# Planning artifacts — design

Status: decisions recorded 2026-10-14 (see "Decisions"); phase 1 = A + B + conventions.
Conventions for the planning skill: `docs/planning-artifact-conventions.md` (versioned).

Planning chats already produce artifacts: plan documents, HTML diagrams, state machines, option
comparisons. Zeron is where the user reads, reviews, and hands them off. This design turns them
from loose files into first-class objects. Conventions for the agent-side planning skill are in
the last section.

## Principles

- **Files stay files, in a Zeron-owned place.** An artifact is a file under the work item's
  canonical checkout (`<canonical>/.zeron/artifacts/<work-item>/`, see Decisions). Agents publish
  files from wherever they wrote them; Zeron copies them there, keeps revision snapshots, and
  renders them.
- **Content never goes in the registry.** Registry operations are capped at 16 KiB, so the registry
  holds only artifact metadata. Content and revision snapshots live in the profile store on the
  owning device, addressed by content hash. Other devices read through the existing workspace file
  API, as they do for files today.
- **One mechanism, many surfaces.** Questions, citations, comments, and option picks are all parsed
  from the artifact itself, so the artifact remains readable outside Zeron (GitHub, a browser,
  Linear) and Zeron adds interaction on top.
- **Tool first, marker second.** Claude Code and Codex register artifacts through the
  `zeron-agent` MCP tools. Harnesses without tools use a link marker in the message, which Zeron
  detects and registers.

## A. Artifacts as first-class objects

**Registration.** New `zeron-agent` tool `artifact_publish {path, title, kind?, role?, note?,
pinAsPlan?, idempotencyKey}`:

- `path` is the file the agent wrote (anywhere it can write: its worktree or tmp); Zeron copies it into the work item's artifact folder. `kind` is inferred from the extension when omitted:
  `markdown`, `html-fragment`, `html-page`, `mermaid`, `image`, `options` (see F).
- `role` is free-form with known values `plan`, `diagram`, `state-machine`, `comparison`, `doc`.
- It returns `{artifactId, revision, url: "zeron-artifact:<artifactId>"}`. The agent puts that URL in
  its message; clicking it opens the rendered artifact.

Fallback for harnesses without tools: a markdown link `[title](zeron-artifact:path/to/file.md)`
anywhere in an assistant message registers that file on first sight.

**Storage.** Registry row kind `artifacts` (id derived from chat + path):
`{spaceId, workItemId?, chatId, path, kind, role, title, currentRevision, revisions: [{id, hash,
note, at, by}] (bounded, newest 50), pinnedAsPlan?, archivedAt?}`. Snapshots go in
`<profile>/artifacts/<artifactId>/<hash>`.

**HTML fragment and preview pairs.** Registration and link clicks both resolve the pair: for
`foo.fragment.html` or `foo.preview.html`, Zeron renders the fragment inside its host wrapper (the
Codex visualize wrapper already built: theme variables, sandbox, CSP). If only a page exists, it
renders the page sandboxed. The preview file stays available as "Open standalone". A link to
either file always opens something viewable.

**Surfaces.**

- **Artifact tab:** a pane tab with renderer, toolbar (revision picker, Diff, Questions, Comments,
  Open standalone, Reveal in Finder, View source), and the question and comment panels.
- **Work item:** Work Item Details gets an Artifacts section listing the item's artifacts (from all
  its chats) with role, title, current revision age, open-question count; the pinned plan sits on
  top. The Outline row shows a small artifact count.
- **Chat:** messages that reference artifacts show an artifact card (title, role, revision, open
  questions) instead of a bare path; the chat header context drawer lists the chat's artifacts.
- **Codex visualize** (`\u{e200}visualize…`) cards register automatically as artifacts.

## B. Render check for agents

New tool `artifact_render_check {artifactId | path, viewport?: {width, height}, theme?: light|dark}`
returns:

- a PNG screenshot of the artifact rendered exactly as Zeron shows it (same wrapper, sandbox, and
  theme), as image content the agent can see;
- render errors: JavaScript console errors, CSP violations, failed resource loads, Mermaid parse
  errors;
- layout warnings: horizontal overflow, elements outside the viewport, text below a minimum
  contrast, empty render.

Mechanism: the headed app renders into an offscreen web view and snapshots it. This is not screen
capture and needs no Screen Recording permission. A headless engine with no UI returns a
`renderUnavailable` capability error. The tool is rate-limited per run.

## C. Revisions and diffs

- Every `artifact_publish` of a changed file is a revision with the agent's `note`. Zeron also
  watches registered files: an edit outside `artifact_publish` becomes a revision noted "edited
  without a note", so drift is never invisible.
- **Diff any two revisions:**
  - **Markdown:** a section-level summary ("changed: §3 Data model, §5 Rollout; added: Q-7"), then a
    line diff reusing the generic revision-diff presenter built for instructions.
  - **HTML and Mermaid:** source diff plus the two renders side by side.
- **Decision drift.** When a question is answered (D) but the artifact has no newer revision, the
  artifact shows "1 answered question not reflected in the doc", which directly addresses "u only
  changed the one section right?".

## D. Structured open questions

Agents mark questions in the artifact (syntax in Conventions). Zeron parses them per revision.

- **Questions panel** in the artifact tab: each question with id, text, example, what it blocks, and
  status. An answer box sends the answer to the chat that owns the artifact as a normal message
  ("Answer to Q-3 in *plan.md*: …"), using the same send path as Reply & resolve.
- **Required Actions:** each open question is a system-origin Required Action on the work item
  ("Q-3: …"), deduplicated by artifact and question id, so plan questions show up in Required
  Actions, roll up to parents, and notify once.
- **Resolution:** the question shows "answered" as soon as you answer, and "resolved" when a later
  revision of the artifact marks it `status: resolved` (with a `decision:` line). Answered but not
  yet resolved in the doc is the drift signal from C.

## E. Typed citations

Rendered as source chips with hover previews, for both Markdown and HTML artifacts:

| Source | Recognized from | Chip and hover |
|---|---|---|
| Code | `path:line` / `path:line-line`, or a `zeron-file:` link | file name and line; hover shows the snippet; click opens the file at that line |
| GitHub | PR, commit, file-at-commit URLs | PR number, state, title (existing PR cache) |
| Linear | issue / comment / project URLs | identifier, state, title (Linear client) |
| Observability | Datadog, Sentry, Metabase URLs | product icon, query or issue title from the URL |
| Vendor docs | any other https URL | domain and page title |

**Claim status.** Each citation, or a bare claim, carries `measured`, `claimed`, `inferred`, or
`unverified`. Measured and claimed are plain chips; inferred is muted; unverified is flagged with
the warning color and counted in the artifact header ("4 unverified claims"), so "did we confirm
that" is visible at a glance.

## F. Side-by-side options

An `options` artifact is a small manifest pointing at N fragments, each rendering the same records.
Zeron renders them in synchronized columns (shared scroll, the same viewport width each), with
**Pick this** on each column. Picking sends "Picked option C for *<title>*: <label>" to the owning
chat and records the decision on the artifact (and resolves a linked question if the manifest names
one).

## G. Commenting on HTML artifacts

Extend the Markdown preview's block-comment-to-composer flow to rendered HTML, Mermaid, and images:
hover highlights the element (or diagram node); click and comment captures the artifact, revision,
a stable selector (`id` or `data-zeron-anchor` first, CSS path as fallback), the element's text,
and a cropped screenshot. The composer gets a comment chip; sending produces a structured block
the agent can act on ("Comment on *flow.fragment.html* r4 · node `receiving` ('Receiving'): …").
Comments are kept per revision and shown as pins; a comment whose anchor disappears in a later
revision is listed as outdated.

## H. A canonical plan on the work item

- One artifact per work item can be pinned as **the plan** (by the agent with `pinAsPlan`, or by you
  from the Artifacts section).
- **New and child chats under the item** get a fixed pointer in their Work Outline instructions:
  "This work item's plan: *<title>* (`zeron-artifact:<id>`). Read it with `artifact_get_plan`
  before starting." The pointer names the artifact, not a revision, so publishing new revisions
  does not create instruction drift.
- New tool `artifact_get_plan {}` returns the current plan's content, revision, open questions, and
  unverified-claim count for the chat's work item.
- Other copies of the plan registered on the item are labelled "not the plan" so a new chat never
  has to guess which is current.
- **Optional Linear mirror:** with Linear on, "Mirror to Linear project document" pushes the
  Markdown plan to the linked Linear project's document on publish. That is Zeron's first write to
  Linear, so it is off by default and explicit per item.

## Phasing

| Phase | Scope | Why first |
|---|---|---|
| 1 | A (registry, publish tool, marker fallback, fragment/preview resolution, artifact tab, Artifacts section, message cards) | everything else hangs off artifacts |
| 2 | B render check; E citations and claim status | high priority, independent of each other once A exists |
| 3 | D questions panel and Required Actions; C revision diffs (D's drift signal needs C's revisions) | high and medium priority, built together |
| 4 | G HTML comments | medium |
| 5 | F options; H canonical plan and Linear mirror | later |

## UI behavior (phase 1, as built)

Code: `crates/ui/src/artifacts/` (+ `shell/artifacts_ui.rs`, `work_outline/details_artifacts.rs`).

- **Data.** `WatchArtifacts` follows the Work Outline watch set (visible Spaces + the selected
  chat's Space). A host answering `unknown method` shows "Artifacts aren't available on this host
  yet" everywhere instead of failing.
- **Links.** `zeron-artifact:<id>` opens the Artifact tab in the right pane (reusing an open tab).
  `zeron-artifact:<path>` resolves against `storedPath` / `originPath` / `previewPath` (relative
  paths from the chat's checkout); an unknown path is registered into the chat's scope
  (`ArtifactRegister`, its work item when assigned) and then opened. Works from transcripts, Details
  references, and Required Actions (chips for links in the request text or detail). From the
  Overview the tab opens in the artifact's chat, else a chat of its work item.
- **Tab.** Markdown renders in the native preview (`zeron-file:` and `path:line` links open files,
  resolved from the canonical checkout root). Fragments, pages, and Mermaid render through the
  visualize sandbox in an embedded web view (Mermaid from jsDelivr, `securityLevel: strict`);
  images natively; options as a list whose entries open their fragment. The revision picker reads
  `ArtifactContent {revision}`; "Compare with…" shows a line diff (`revision_diff`); Open
  standalone, Reveal in Finder, and Promote use the owning device (Reveal and standalone only when
  it is this device).
- **Details.** The Artifacts section lists the pinned plan first (with the item's Linear project
  beside it), then newest first; right-click offers Pin/Unpin, Promote, Reveal, Copy link.
  "Register existing file…" opens a native file picker for a Space on this device (a path field for
  a remote one) and refuses paths outside the Space folder. Outline rows show an artifact count.
- **Transcript.** Settled assistant messages show a card per artifact link (the inline link stays
  clickable); a Codex visualize card opens the registered artifact when one covers its file.
- **Render checks.** The headed UI subscribes to `WatchArtifactRenderRequests` as `zeron-ui` and
  answers every request within 25 s with at most two offscreen web views. Markdown answers with a
  "renders natively" warning and no screenshot; options render every option fragment stacked;
  non-macOS answers `renderUnavailable`.

## Conventions for the agent-side planning skill

Moved to `docs/planning-artifact-conventions.md` (v1), which is the binding version.

## Decisions (2026-10-14)

1. **Location: Zeron-owned and keyed by work item, never the chat checkout.** Planning chats run in
   worktrees that agents clean up, so artifacts live under the work item's *canonical* checkout:
   `<canonical>/.zeron/artifacts/<work-item>/`. The canonical checkout is the main checkout of the
   item's resolved repository, never a worktree. Chats without a work item use
   `<canonical>/.zeron/artifacts/_chats/<chat-id>/`.
   - **Publishing copies.** `artifact_publish` copies the agent's file, wherever the agent could write
     it (worktree, tmp), into that location and snapshots a revision. Agents never need write access
     outside their sandbox.
   - **Existing paths** (`work/`, `.local/`, old Claude artifacts) can be registered: the file is
     imported as revision 1 and its origin path is kept for provenance.
   - **Promote to docs/plans/** copies the current revision into the canonical repo's `docs/plans/`
     as an uncommitted file for the team, and records the link.
   - **Ignored with `.git/info/exclude`**, resolved with `git rev-parse --git-path info/exclude`
     (which also covers submodules), never `.gitignore`. Zeron does not modify tracked repo files.
2. **Questions: only blocking ones become their own Required Action.** Non-blocking open questions
   roll up into one "N open questions in <artifact>" action per artifact. The dedupe key is the
   question id, so revisions never duplicate. Resolution syncs both ways: answering in Zeron
   writes the answer into the artifact (new revision, `status: resolved`, `decision:`) and sends it
   to the chat; editing the artifact to `status: resolved` resolves the action.
3. **No Linear mirror.** Show the work item's linked Linear project next to the pinned plan instead.
   Agents keep pushing to Linear with the `linear` CLI; the planning skill owns that sync.
4. **Phase 1 = A + B + the conventions doc**, so visuals are never shown unchecked and the skill can
   be built in parallel.
