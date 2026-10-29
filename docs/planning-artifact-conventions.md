# Planning artifact conventions — v1.1

The contract between agents (the planning skill) and Mast for planning artifacts: plan documents,
diagrams, state machines, and option comparisons. Mast parses exactly what is described here;
anything else is shown as plain content. Design background: `plans/planning-artifacts-design.md`.

Version: **v1.1** (2026-10-14). v1.1 is additive over v1: `artifact_render_check` gained
`viewports`, `themes`, and layout warnings (section 3). Breaking changes bump the major version;
Mast keeps reading older versions.

## 1. Publishing an artifact

Everything the user should see is published; Mast never discovers artifacts by scanning folders.

- **Write the file anywhere you can write** (your worktree, a temp folder).
  - Any regular file on this device can be published (it is copied), except credential
    locations (`~/.ssh`, `~/.gnupg`, `~/.aws`, `~/.config/gh`, `~/Library/Keychains`, `~/.netrc`,
    `~/.docker/config.json`), `.env` / `.env.*` files, and files over 10 MB: refused with
    `artifact/denied`.
- **Publish it with the `mast-agent` tool:**

  ```
  artifact_publish {
    path:          "<file you wrote>",
    title:         "Receiving flow — plan",
    kind?:         "markdown" | "html-fragment" | "html-page" | "mermaid" | "image" | "options",
    role?:         "plan" | "diagram" | "state-machine" | "comparison" | "doc",
    note:          "<one line: what changed in this revision>",
    pinAsPlan?:    true,
    idempotencyKey:"<stable per change; reuse on retry>"
  }
  → { artifactId, revision, url: "mast-artifact:<artifactId>", storedPath }
  ```

  - `kind` defaults from the file extension.
  - Mast copies the file into the work item's artifact folder
    (`<canonical checkout>/.mast/artifacts/<work item>/`) and records a revision. Later publishes of
    the same `title` and `role` on the same work item add revisions to the same artifact.
  - `storedPath` is where the canonical copy lives; read it there in later chats instead of
    rebuilding the artifact.
- **Link it** in your message with the returned URL: `[Receiving flow — plan](mast-artifact:<artifactId>)`.
  Never link a bare file path. Mast still resolves the older `zeron-artifact:` links, the
  `data-zeron-question` / `data-zeron-anchor` attributes and artifacts stored under
  `<canonical checkout>/.zeron/artifacts/`, but write the `mast-` / `.mast/` forms.
- **Without the tool** (harnesses that have no Mast tools), link the file with
  `[title](mast-artifact:<path you wrote>)`; Mast registers it on first sight.
- **Existing files** (`work/`, `.local/`, earlier artifacts): publish them the same way; the first
  publish imports them as revision 1.

## 2. HTML

- Write `<name>.fragment.html` containing body content only: no `<html>`, `<head>`, or `<body>`.
  Mast wraps it with its theme variables, a sandbox, and a content security policy.
- Optionally also write `<name>.preview.html`, a standalone page for viewing in a browser.
- Publish the **fragment**. A link to either file opens the rendered fragment.
- Scripts are allowed inside the sandbox. Network access is limited to the CDNs allowed by Codex's
  visualize policy; anything else fails and is reported by the render check.

## 3. Checking what you made

After creating or changing any visual artifact (HTML, Mermaid, image, options):

```
artifact_render_check {
  artifactId | path,
  viewports?: [{ width, height, label? }],  // default: [{800,800,"narrow"}, {1440,900,"wide"}]
  viewport?:  { width, height },             // one viewport; replaces the default list
  themes?:    ["light" | "dark", ...],       // default: the user's current theme only
  theme?:     "light" | "dark"               // one theme
}
→ { screenshots: [{ viewport, width, height, theme, png }],   // one image per viewport x theme
    screenshot,                                               // the first image (v1 field)
    errors: [...], warnings: [...], layoutWarnings: [...] }
```

- Every viewport is rendered in every theme, at most **4 images per call**. If you ask for more,
  the extra ones are dropped and `warnings` says so. Over MCP each image arrives as an image item
  preceded by a text label (viewport, size, theme).
- For visuals, check **both themes at both default widths**: call
  `artifact_render_check { artifactId, themes: ["light", "dark"] }` (that is exactly four images).
- `errors`: JavaScript console errors, blocked resources, Mermaid parse errors, empty render.
- `warnings`: horizontal overflow, content outside the viewport, low-contrast text.
- `layoutWarnings` (v1.1): each is `{ kind, selector, textSample (at most 80 chars), viewport,
  theme }`, at most 50 per call:
  - `clippedText`: text in an element with hidden overflow or an ellipsis that is cut off.
  - `overlappingSiblings`: visible siblings whose boxes overlap by more than 20% of the smaller
    one (positioned overlays such as badges are skipped unless text overlaps text).
  - `narrowTextColumn`: a block of 3 or more lines averaging fewer than 2 words per line.
  - `widerThanParent`: a child wider than a parent that does not scroll.
  - `fixedWidth`: an authored fixed pixel width wider than the viewport.
- **Warnings are heuristics.** They miss real problems and flag fine layouts. Always look at every
  screenshot, whatever the warnings say.
- Fix every error and every warning that is a real problem, call the check again, and look at the
  screenshots before you say the artifact is done.
- If it returns `renderUnavailable` (no Mast window on that device), say so instead of claiming
  the artifact was checked.

## 4. Open questions

### Markdown

One block per question:

```markdown
> [!question] Q-3 · Should receiving lines merge by MPN or by internal part number?
> blocking: true
> example: two lines for LM358 from different suppliers
> blocks: ENG-2690 data model; §4 Receiving flow
> status: open
```

- **Id:** `Q-<number>`, unique within the artifact and never reused or renumbered.
- **Fields, one per line, in any order:**
  - `blocking: true|false` (default `false`)
  - `example:` and `blocks:` (optional)
  - `status: open|answered|resolved`
- **Resolving:** when a question is settled, keep the block and set `status: resolved` with a
  `decision: <one line>` field. Do not delete resolved questions.
- **Answers arrive as messages** like: `Re: "Q-3: Should receiving lines merge by MPN?": By MPN`
  (the same shape as any Required Action reply). Mast records the answer on the artifact but does
  not edit your file: apply the decision to the document, set the block to `status: resolved`
  with `decision:`, and publish a new revision. `artifact_get_plan` lists unreflected answers as
  `answeredQuestions: [{id, answer, at, revision}]`, so you also see answers given while you were
  not running.
- **In Mast:** blocking questions each become a Required Action on the work item. Other open
  questions are summarised as one "N open questions in <plan>" action per artifact. Questions with
  no `Q-n` id cannot become actions or be answered: always give each one an id.

### HTML

```html
<div data-mast-question="Q-3" data-status="open" data-blocking="true"
     data-example="two lines for LM358 from different suppliers"
     data-blocks="ENG-2690 data model">
  Should receiving lines merge by MPN or by internal part number?
</div>
```

Resolved questions add `data-status="resolved" data-decision="<one line>"`.

## 5. Citations and claim status

Every factual claim about the system cites a source and says how sure it is.

| Status | Meaning |
|---|---|
| `measured` | you ran, queried, or read it directly in this work |
| `claimed` | a source says so (a doc, ticket, comment, person) and you did not verify it |
| `inferred` | your reasoning from other facts |
| `unverified` | stated without checking |

### Markdown

- **With a source:** a normal link whose *title* is the status:
  - `[orders.py:120](flagship/orders.py:120 "measured")`
  - `[#6265](https://github.com/Cofactr/flagship/pull/6265 "claimed")`
  - `[ENG-2690](https://linear.app/cofactr/issue/ENG-2690 "claimed")`
  - `[p95 latency](https://app.datadoghq.com/… "measured")`
- **Without a source:** an inline marker right after the claim: `{unverified}` or `{inferred}`.
- **Code locations** are `path:line` or `path:line-line`, relative to the repository root (for the
  Cofactr workspace: `ui/src/…`, `flagship/…`).

Mast recognises sources by URL: GitHub PRs, commits and files; Linear issues, comments and
projects; Datadog, Sentry and Metabase; anything else is shown as a documentation link.

### HTML

`<a href="…" data-claim="measured">…</a>`, and for unsourced claims
`<span data-claim="unverified">…</span>`.

## 6. Comparing options

- Write one fragment per option, all rendering the **same records**.
- Write a manifest and publish it with `kind: "options"`:

  ```json
  {
    "title": "Receiving table layouts",
    "question": "Q-2",
    "options": [
      { "id": "A", "label": "Dense grid",    "path": "receiving.option-a.fragment.html" },
      { "id": "B", "label": "Grouped by PO", "path": "receiving.option-b.fragment.html" }
    ]
  }
  ```

  Option paths are relative to the manifest. Publishing the manifest publishes the fragments with
  it.
- **Set a design width.** "Side by side" lays each option out at a fixed width and scales it down
  to its column, so two-column mockups and tables keep their real layout. Say what width each
  option was designed for, in px (default 1100, clamped to 320–4000): per option with
  `"designWidth": 1280` in its manifest entry, for all options with a top-level `"designWidth"`, or
  in the fragment with `data-mast-design-width="1280"` on any element. Order of precedence: entry,
  fragment attribute, top-level, 1100. Don't rely on the narrow column to reflow your layout.
- The user's pick arrives as `Picked option B for "Receiving table layouts": Grouped by PO`. If
  `question` is set, that question is resolved with the pick.

## 7. The plan

- Publish the work item's plan with `role: "plan"` and `pinAsPlan: true`. There is one plan per
  work item; pinning a new one unpins the old.
- At the start of every chat under a work item, call `artifact_get_plan {}`:

  ```
  → { artifactId, title, revision, storedPath, content, openQuestions, unverifiedClaims,
      answeredQuestions }
  ```

  With no plan, `artifactId` is `null` and a `note` explains why. If a plan exists, work from it and update it (publish a new revision) rather than writing a new
  one.
- Mast shows the work item's linked Linear project next to the plan. Syncing plan content into
  Linear is the skill's job (for example with the `linear` CLI); Mast does not write to Linear.

## 8. Stable anchors

Give diagram nodes and major HTML sections an `id` or `data-mast-anchor="<name>"`. User comments
attach to these anchors and survive later revisions; elements without anchors attach by position
and may be marked outdated after a revision.

## Comments on HTML and Mermaid artifacts

Users can comment on parts of an HTML or Mermaid artifact in Mast. Give sections, headings, cards
and diagram nodes a stable `id` (or `data-mast-anchor="..."`) and keep it across revisions, so a
comment stays attached when the text around it changes; without one it falls back to the quoted
text and goes *outdated* once that text is gone. Call `artifact_get_plan` to read the open
`comments` (each has its anchor and quoted text), address them in the next revision, and pass their
ids as `addressedComments` (for example `["C-1", "C-3"]`) to `artifact_publish`; they are then
marked resolved.
