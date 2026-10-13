# Codex `visualize` fragments

Research notes for rendering Codex inline visualizations in Zeron chats.
Sources were inspected read-only on 2026-10-13 against `codex-cli 0.154.0`
and the bundled `visualize` plugin `1.0.45`.

## Sources

| Ref | Location |
| --- | --- |
| S1 | `codex app-server generate-json-schema --experimental --out /tmp/codex-schema` → `codex_app_server_protocol.v2.schemas.json` |
| S2 | `~/.codex/plugins/cache/openai-bundled/visualize/1.0.45/skills/visualize/SKILL.md` |
| S3 | same plugin, `skills/visualize/scripts/render.py` |
| S4 | same plugin, `skills/visualize/assets/standalone-host-bridge.js`, `standalone-shell.js`, `visualize.html`, `visualize.css` |
| S5 | string table of `~/.codex/packages/standalone/releases/0.154.0-aarch64-apple-darwin/bin/codex` (the TUI's own viewer) |

The plugin is licensed "Proprietary" (`.codex-plugin/plugin.json`). Zeron
does not vendor its CSS, kit HTML or scripts; it implements the documented
agent-facing contract independently.

## There is no app-server item for visualizations

`ThreadItem` (S1, `definitions.ThreadItem.oneOf`) has `userMessage`,
`hookPrompt`, `agentMessage`, `functionCallOutput`, `plan`, `reasoning`,
`commandExecution`, `fileChange`, `mcpToolCall`, `dynamicToolCall`,
`collabAgentToolCall`, `subAgentActivity`, `webSearch`, `imageView`,
`sleep`, `imageGeneration`, review-mode items and `contextCompaction`. None
carries a visualization, HTML, or artifact. The only related schema entry is
`ThreadRealtimeBemItemPresentation.inlineVisualization {index}` (S1), an
experimental realtime-voice presentation hint that points at an existing item
by index; it is not emitted for ordinary turns.

Visualizations are therefore a **convention between the skill and the host**:

1. The agent writes an HTML **fragment** file (S2 "Inline HTML output
   contract → File/Fragment").
2. The final answer contains a content reference on its own line (S2
   "Content and response"):

   ```text
   visualize{"path":"<absolute-path>/<title>.html"}
   visualize{"path":"…","mode":"wide"}      // full-screen mockups, ≤1,024px
   ```

   The JSON may also carry `title`. On the wire the model emits the prefix
   and terminator as private-use tokens: the agent message text contains
   `U+E200 "visualize" U+E202 {json} U+E201`. The TUI matches exactly the
   bytes `\u{e200}visualize\u{e202}` and rewrites them into a
   `::codex-inline-vis{…}` directive (S5, next to `tui/src/assistant_directives.rs`).
   Zeron's transcript already split this envelope (`crates/ui/src/transcript.rs`,
   `assistant_text_fragments`). A plain ASCII `visualize{…}` line is the same
   reference after detokenization and is accepted only as a whole line outside
   code fences.
3. The host reads the fragment, wraps it, and renders it inline. The TUI,
   which cannot render HTML, materializes a viewer document under
   `$CODEX_HOME/generated/visualization-viewers/` and offers "visualization in
   the browser"; on failure it prints "Visualization unavailable on this
   device." (S5).

The file name is chosen by the agent (`<title>.html` per S2). Users' agents
also use `<name>.fragment.html` for the inline source and `<name>.preview.html`
for a standalone export produced by `render.py <fragment> <destination>.html`
(S2 "Exporting an existing visualization"; S3 `export_html`).

## Fragment rules (S2, S3)

- Fragment only: no `<!doctype>`, `<html>`, `<head>`, `<body>`. `render.py`
  rejects documents with `<!doctype\s|<\s*(?:html|head|body)(?:\s|>)` (S3
  `_DOCUMENT_MARKUP`).
- At most 1 MB (S3 `_MAX_FRAGMENT_BYTES = 1_000_000`); regular file, not a
  symlink (S3 `_read_fragment`).
- **Scripts are allowed** and expected (D3, tabs, simulations). The fragment
  must not use `fetch`, XHR, WebSocket or other API calls.
- Static resources only from the CDN allowlist: `cdnjs.cloudflare.com`,
  `esm.sh`, `cdn.jsdelivr.net`, `unpkg.com`, `fonts.googleapis.com`,
  `fonts.gstatic.com`, `fonts.bunny.net`. "Other origins are blocked and fail
  silently" (S2 "External resources").
- Design widths 736px (normal) and 1,024px (`mode: "wide"`), down to 320px.
  "The host sizes the frame to its content" — avoid viewport-height layouts
  and `position: fixed` (S2 "Layout and accessibility").
- Honor `prefers-reduced-motion`; never loop motion (S2 "Charts").

## Host wrapper (S3, S4, S5)

Both `render.py` and the TUI build the same two-level structure:

- **Inner frame document**: `<meta name="referrer" content="no-referrer">`,
  a CSP meta tag, a title, the base stylesheet, a host bridge script, then the
  fragment followed by the runtime kit (tooltip positioning for
  `[data-tooltip]`, tab behaviour, Lucide icon initialisation).
- **Outer shell**: `<iframe sandbox="allow-scripts" referrerpolicy="no-referrer" srcdoc="…">`
  — no `allow-same-origin`, so the fragment runs in an opaque origin with no
  storage, cookies, popups, forms or top navigation.

CSP for the frame (S3 `_FRAME_CSP`, identical string in S5):

```text
default-src 'none';
script-src 'unsafe-inline' 'unsafe-eval' 'wasm-unsafe-eval' <R>;
style-src 'unsafe-inline' <R>;  img-src <R>;  font-src <R>;  media-src <R>;
worker-src blob:;  connect-src blob: data:;
frame-src 'none';  object-src 'none';  base-uri 'none';  form-action 'none'
<R> = blob: data: https://cdnjs.cloudflare.com https://cdn.jsdelivr.net https://esm.sh
      https://fonts.bunny.net https://fonts.googleapis.com https://fonts.gstatic.com https://unpkg.com
```

The shell repeats it with `frame-src 'self'` because a srcdoc frame inherits
the shell policy (S3 comment above `_SHELL_CSP`).

Theme contract (S2 "Runtime styles → Color"; S5 `:root` block "Agent-facing
contract; keep in sync with SKILL.md"): `--background`, `--foreground`,
`--card`, `--card-foreground`, `--popover`, `--popover-foreground`,
`--primary`, `--primary-foreground`, `--secondary`, `--secondary-foreground`,
`--muted`, `--muted-foreground`, `--accent`, `--accent-foreground`,
`--destructive`, `--border`, `--input`, `--ring`, `--blue`, `--orange`,
`--green`, `--red`, `--purple`, `--yellow`, `--viz-series-1…6`,
`--font-size-base`. `:root[data-theme="light"|"dark"]` selects the color
scheme. Utility classes: `.card`, `.viz-stat`, `.viz-stat-value`, `.viz-grid`,
`.viz-row`, `.viz-badge`, `.viz-controls`, `.viz-tile`, `.btn`
(`-primary`, `-ghost`, `-block`), `.form-*`, `.table` (`-sm`,
`-responsive`), `.nav.nav-pills`/`.nav-link`, `.progress`, `.text-small`,
`.text-muted`, `.text-destructive`, `.tabular-nums`, `.sr-only`,
`.viz-dotted-background`, `.cursor-interaction`.

Host bridge (S4 `standalone-host-bridge.js`): defines `window.openai` with
`theme`, `widgetState`, `setWidgetState()`, `sendFollowUpMessage()`,
`openExternal()`, and dispatches `openai:set_globals`. Height and other host
messages travel over a `MessageChannel` handed to the frame with a
`codex-visualization-initialize` postMessage: `height` (0–10,000px),
`scroll-to`, `open-external` (https only, user activation required),
`widget-state-write` (≤16 KiB), `follow-up`. Fragments must treat
`setWidgetState` failures as non-fatal (`.catch(() => {})`, S2).

## Zeron decisions

- **Transport.** The content reference stays in the persisted assistant text
  (as Codex sends it). The transcript turns it into a dedicated visualization
  row. No new session-doc part kind was added: the text already reaches every
  client and existing history, a second part would duplicate it, and a new
  kind would need edge/iOS schema work. Shared parsing lives in
  `zeron_proto::visualize`.
- **Detection.** Envelopes, whole-line ASCII references, and `*.fragment.html`
  paths mentioned in assistant text (links, inline code, bare paths) become
  visualization cards when the path resolves inside the chat's checkout.
- **Rendering.** A fragment opens in a right-pane tab backed by the existing
  embedded browser (`crates/ui/src/browser`, WKWebView via wry, nonpersistent
  data store, no IPC handler). Zeron reads the file through the workspace RPC
  (checkout-jailed, works for remote devices), wraps it in its own host
  document with the CSP above and `sandbox="allow-scripts"`, and loads it with
  `loadHTMLString` and no base URL, so the page has no `file://` access at
  all (tighter than "the fragment's directory"). In document mode the
  navigation delegate allows only `about:blank`/`about:srcdoc`; a clicked
  http(s) link opens a regular Browser tab instead of replacing the
  visualization.
- **Theme and motion.** The base stylesheet is generated from the active
  Zeron theme (`--background` = panel, `--primary` = accent, …) and
  re-rendered when the theme changes. Zeron's reduced-motion setting forces
  `prefers-reduced-motion: reduce` behaviour in the frame.
- **Bridge.** `window.openai` is a local stub: widget state lives in memory
  only, `sendFollowUpMessage`/`openExternal` are no-ops. No page-to-engine
  channel exists.
- **Sizing.** The pane is the expanded view; the frame fills it, capped at
  736px or 1,024px for `wide`. Inline auto-height rendering inside the
  transcript is not implemented.
- **Previews.** `*.preview.html` links open in the system browser through a
  `file://` URL on local chats. They are not loaded in the embedded browser,
  which permits only http(s).

## Sharing outside Zeron (Linear)

Linear cannot read local files, and a fragment is not a standalone page, so
neither a fragment path nor a `file://` link works there. Use the standalone
export (`<name>.preview.html`, produced by `render.py <fragment> <dest>.html`)
and publish or attach it where viewers can reach it (for example a hosted
site, or an attachment). The export loads its libraries from the public CDNs,
so it works when opened from that hosted copy.
