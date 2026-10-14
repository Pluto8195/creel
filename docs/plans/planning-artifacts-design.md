# Planning artifacts — design

Status: proposal for review (2026-10-14). Nothing here is built yet.

Planning chats already produce artifacts: plan documents, HTML diagrams, state machines, option
comparisons. Zeron is where the user reads, reviews, and hands them off. This design turns them
from loose files into first-class objects. Conventions for the agent-side planning skill are in
the last section.

## Principles

- **Files stay files.** An artifact is a file in the chat's checkout that the agent wrote and
  registered. Zeron never asks agents to write somewhere special to be viewable; it records which
  file, keeps revision snapshots, and renders it.
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

- `path` is a file inside the chat's checkout; `kind` is inferred from the extension when omitted:
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

## Conventions for the agent-side planning skill

1. **Publish every artifact you want the user to see:**
   - call `artifact_publish` after writing or changing it, with a one-line `note` saying what
     changed;
   - link the returned `zeron-artifact:` URL in your message, never a bare path;
   - without the tool, use `[title](zeron-artifact:relative/path)`.
2. **HTML:** write `<name>.fragment.html` (body content only, no `<html>`/`<body>`) and, if useful,
   `<name>.preview.html` for standalone viewing. Publish the fragment.
3. **Check what you made:** after any visual artifact, call `artifact_render_check`, fix every
   reported error and warning, and look at the screenshot before saying it is done.
4. **Questions (Markdown):** one block per question:

   ```markdown
   > [!question] Q-3 · Should receiving lines merge by MPN or by internal part number?
   > example: two lines for LM358 from different suppliers
   > blocks: ENG-2690 data model; §4 Receiving flow
   > status: open
   ```

   When answered, keep the block and update it: `status: resolved` and `decision: <one line>`.
   Question ids are stable for the life of the artifact. In HTML, use
   `<div data-zeron-question="Q-3" data-status="open">` with `data-blocks` and `data-example`
   attributes.
5. **Citations:** every factual claim about the system cites a source with a status:
   - links carry the status as the link title: `[orders.py:120](flagship/orders.py:120 "measured")`,
     `[#6265](https://github.com/Cofactr/flagship/pull/6265 "claimed")`;
   - a claim with no source gets an inline marker: `{unverified}` or `{inferred}`;
   - statuses: `measured` (you ran or read it), `claimed` (a source says so), `inferred` (your
     reasoning), `unverified` (not checked);
   - in HTML, use `data-claim="<status>"` on the link or span.
6. **Options:** for N design options, write one fragment per option rendering the same records and
   an options manifest, then publish the manifest with `kind: options`:

   ```json
   {"title": "Receiving table layouts", "question": "Q-2",
    "options": [{"id": "A", "label": "Dense grid", "path": "receiving.option-a.fragment.html"},
                {"id": "B", "label": "Grouped by PO", "path": "receiving.option-b.fragment.html"}]}
   ```

7. **The plan:** publish the work item's plan with `role: plan, pinAsPlan: true`. At the start of
   any chat under a work item, call `artifact_get_plan`; if a plan exists, work from it and update
   it rather than creating a new one.
8. **Stable anchors:** give diagram nodes and major HTML sections `id` or `data-zeron-anchor`
   attributes so comments survive revisions.

## Open decisions

1. **Default location for artifact files:** a gitignored `.zeron/artifacts/<work-item>/` in the
   chat's checkout (recommended for drafts), or anywhere the agent chooses (registration works
   either way). Plans meant for the team can still be committed under `docs/plans/`.
2. **Should questions become Required Actions automatically** (recommended, as in D), or only
   appear in the artifact's Questions panel?
3. **Linear plan mirror (H):** confirm it is wanted, since it is the first write to Linear.
