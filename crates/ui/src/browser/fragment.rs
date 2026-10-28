//! Host documents for Codex `visualize` HTML fragments.
//!
//! Protocol notes and sources: docs/research/codex-visualize.md. A fragment
//! is untrusted model output. It is wrapped twice, like Codex's own host:
//!
//! - an inner frame document with a strict CSP (scripts are part of the
//!   protocol; network only for the skill's CDN allowlist, no `connect-src`
//!   beyond `blob:`/`data:`), Mast-themed base CSS, a local `window.openai`
//!   stub and a small runtime for tabs, tooltips and icons;
//! - an outer document whose only content is
//!   `<iframe sandbox="allow-scripts" srcdoc=…>` — an opaque origin with no
//!   storage, cookies, popups, forms, top navigation or parent access.
//!
//! The browser loads the outer document as a string with no base URL, so
//! neither level can address `file://`. No page-to-engine channel exists.
//! The base stylesheet and scripts here are Mast's own implementation of the
//! skill's documented contract; Codex's proprietary kit is not vendored.

use crate::files::client::FilesClientError;
use crate::theme::Theme;
use mast_proto::WorkspaceFileText;
use mast_proto::visualize::{MAX_FRAGMENT_BYTES, is_complete_document};

/// Static resource origins the visualize skill promises fragments.
pub const RESOURCE_SOURCES: &str = "blob: data: https://cdnjs.cloudflare.com \
https://cdn.jsdelivr.net https://esm.sh https://fonts.bunny.net \
https://fonts.googleapis.com https://fonts.gstatic.com https://unpkg.com";

/// Lucide build the skill's sandbox exposes as the global `lucide`.
const LUCIDE_SRC: &str = "https://unpkg.com/lucide@1.17.0/dist/umd/lucide.js";

pub const NORMAL_WIDTH: u32 = 736;
pub const WIDE_WIDTH: u32 = 1024;

/// CSP of the sandboxed frame (matches the skill's frame policy).
pub fn frame_csp() -> String {
    let r = RESOURCE_SOURCES;
    format!(
        "default-src 'none'; \
         script-src 'unsafe-inline' 'unsafe-eval' 'wasm-unsafe-eval' {r}; \
         style-src 'unsafe-inline' {r}; img-src {r}; font-src {r}; media-src {r}; \
         worker-src blob:; connect-src blob: data:; frame-src 'none'; \
         object-src 'none'; base-uri 'none'; form-action 'none'"
    )
}

/// CSP of the outer document. A srcdoc frame inherits it, so it must permit
/// what the frame may load; the outer document itself has no script.
pub fn host_csp() -> String {
    frame_csp().replace("frame-src 'none'", "frame-src 'self' about:")
}

/// Theme tokens the fragment contract names, resolved from Mast's theme.
#[derive(Clone, Debug, PartialEq)]
pub struct FragmentTheme {
    pub dark: bool,
    pub vars: Vec<(&'static str, String)>,
    pub font_sans: String,
    pub font_mono: String,
}

fn css(color: gpui::Hsla) -> String {
    let rgba = gpui::Rgba::from(color);
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    let alpha = rgba.a.clamp(0.0, 1.0);
    if alpha >= 0.999 {
        format!(
            "rgb({} {} {})",
            channel(rgba.r),
            channel(rgba.g),
            channel(rgba.b)
        )
    } else {
        format!(
            "rgb({} {} {} / {:.3})",
            channel(rgba.r),
            channel(rgba.g),
            channel(rgba.b),
            alpha
        )
    }
}

fn font_stack(primary: &str, fallback: &str, generic: &str) -> String {
    let mut families = Vec::new();
    for family in [primary, fallback] {
        let family = family.trim();
        if !family.is_empty() && family.chars().all(|c| !matches!(c, '"' | '\\' | '<' | ';')) {
            families.push(format!("\"{family}\""));
        }
    }
    families.push(generic.to_owned());
    families.join(", ")
}

impl FragmentTheme {
    pub fn from_theme(theme: &Theme) -> Self {
        let dark = theme.appearance.is_dark();
        // Fixed hues for the named colors; their lightness follows appearance.
        let pick = |light: &str, dark_value: &str| if dark { dark_value } else { light }.to_owned();
        let vars = vec![
            ("--background", css(theme.bg)),
            ("--foreground", css(theme.text)),
            ("--card", css(theme.surface_card)),
            ("--card-foreground", css(theme.text)),
            ("--popover", css(theme.surface_overlay)),
            ("--popover-foreground", css(theme.text)),
            ("--primary", css(theme.accent)),
            ("--primary-foreground", css(theme.on_accent)),
            ("--secondary", css(theme.surface_raised)),
            ("--secondary-foreground", css(theme.text)),
            ("--muted", css(theme.text.opacity(0.10))),
            ("--muted-foreground", css(theme.text_muted)),
            ("--accent", css(theme.accent_wash)),
            ("--accent-foreground", css(theme.accent)),
            ("--destructive", css(theme.danger)),
            ("--border", css(theme.border)),
            ("--input", css(theme.border_strong)),
            ("--ring", css(theme.accent)),
            ("--blue", pick("rgb(37 120 230)", "rgb(96 165 250)")),
            ("--orange", css(theme.warning)),
            ("--green", css(theme.success)),
            ("--red", css(theme.danger)),
            ("--purple", pick("rgb(138 80 230)", "rgb(176 132 250)")),
            ("--yellow", pick("rgb(214 160 0)", "rgb(250 204 21)")),
            ("--viz-series-1", "var(--primary)".to_owned()),
            ("--viz-series-2", pick("rgb(232 128 52)", "rgb(245 158 92)")),
            ("--viz-series-3", pick("rgb(60 170 90)", "rgb(110 210 135)")),
            (
                "--viz-series-4",
                pick("rgb(220 100 160)", "rgb(240 145 192)"),
            ),
            (
                "--viz-series-5",
                pick("rgb(130 100 220)", "rgb(170 145 240)"),
            ),
            ("--viz-series-6", pick("rgb(40 165 160)", "rgb(90 205 195)")),
            ("--font-size-base", "14px".to_owned()),
        ];
        Self {
            dark,
            vars,
            font_sans: font_stack(
                &theme.font_sans,
                &theme.font_sans_fallback,
                "-apple-system, system-ui, sans-serif",
            ),
            font_mono: font_stack(
                &theme.font_mono,
                &theme.font_mono_fallback,
                "ui-monospace, SFMono-Regular, Menlo, monospace",
            ),
        }
    }
}

pub fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn escape_attr(text: &str) -> String {
    escape_text(text).replace('"', "&quot;")
}

fn base_stylesheet(theme: &FragmentTheme, reduced_motion: bool) -> String {
    let mut out = String::from(":root {\n");
    out.push_str(if theme.dark {
        "  color-scheme: dark;\n"
    } else {
        "  color-scheme: light;\n"
    });
    for (name, value) in &theme.vars {
        out.push_str(&format!("  {name}: {value};\n"));
    }
    out.push_str(&format!(
        "  --font-sans: {};\n  --font-mono: {};\n}}\n",
        theme.font_sans, theme.font_mono
    ));
    out.push_str(BASE_CSS);
    let motion = "*, *::before, *::after { animation-duration: 0.01ms !important; \
animation-iteration-count: 1 !important; transition-duration: 0.01ms !important; \
scroll-behavior: auto !important; }\n";
    if reduced_motion {
        out.push_str(motion);
    } else {
        out.push_str(&format!(
            "@media (prefers-reduced-motion: reduce) {{ {motion}}}\n"
        ));
    }
    out
}

/// Utility classes from the skill's documented design system.
const BASE_CSS: &str = r#"*, *::before, *::after { box-sizing: border-box; }
html { background: transparent; }
html > body { margin: 0; padding: 0; color: var(--foreground); background: transparent;
  font-family: var(--font-sans); font-size: var(--font-size-base); font-weight: 400;
  line-height: 1.5; -webkit-font-smoothing: antialiased; }
h1, h2, h3, p { margin-block: 0; }
h1 { font-size: 1.7em; font-weight: 500; line-height: 1.25; }
h2 { font-size: 1.43em; font-weight: 500; line-height: 1.25; }
h3 { font-size: 1.29em; font-weight: 500; line-height: 1.3; }
strong, b { font-weight: 500; }
a { color: var(--primary); text-decoration: none; font-weight: 500; }
a:is(:hover, :focus-visible) { text-decoration: underline; }
code:not(pre code) { padding: 1px 6px; border-radius: 6px; background: var(--muted);
  font-family: var(--font-mono); font-size: 0.92em; overflow-wrap: anywhere; }
pre { margin: 0; overflow-x: auto; font-family: var(--font-mono); }
img, video, canvas { max-width: 100%; height: auto; }
hr { border: 0; border-top: 1px solid var(--border); margin: 8px 0; }
#widget { display: flex; flex-direction: column; gap: 12px; width: 100%; }
.card { min-width: 0; padding: 12px; overflow: hidden; overflow-wrap: break-word;
  border-radius: 16px; color: var(--card-foreground); background: var(--card); }
.viz-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(max(180px, 24%), 1fr)); gap: 10px; }
.viz-row { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; }
.viz-stat { display: flex; flex-direction: column; gap: 2px; }
.viz-stat-value { font-size: 1.43em; font-weight: 500; line-height: 1.25; }
.viz-badge { display: inline-block; padding: 3px 8px; border-radius: 9999px;
  color: var(--accent-foreground); background: var(--accent); font-weight: 500; }
.viz-controls { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
.viz-controls > .form-label { display: grid; flex: 1 1 280px; min-width: min(100%, 260px);
  grid-template-columns: minmax(0, 1fr) auto; align-items: baseline; gap: 2px 12px; margin-bottom: 0; }
.viz-controls > .form-label > :is(.form-control, .form-range, .form-select) { grid-column: 1 / -1; }
.viz-dotted-background { background-image: radial-gradient(var(--border) 1px, transparent 1px);
  background-size: 16px 16px; }
small, .text-small, .viz-badge { font-size: max(11px, calc(var(--font-size-base) - 2px)); }
.text-muted { color: var(--muted-foreground); }
.text-destructive, .text-warning { color: var(--destructive); }
.tabular-nums { font-variant-numeric: tabular-nums; }
.text-end { text-align: end; } .text-center { text-align: center; } .text-nowrap { white-space: nowrap; }
.sr-only { position: absolute; width: 1px; height: 1px; padding: 0; overflow: hidden;
  clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }
.cursor-interaction { cursor: pointer; }
.btn { appearance: none; display: inline-flex; inline-size: fit-content; max-inline-size: 100%;
  min-height: 28px; align-items: center; justify-content: center; gap: 4px; margin: 0; padding: 0 8px;
  border: 1px solid var(--input); border-radius: 10px; color: var(--secondary-foreground);
  background: var(--secondary); font: inherit; cursor: pointer; white-space: nowrap; user-select: none; }
.btn:not(:disabled):hover { background: color-mix(in srgb, var(--foreground) 6%, var(--secondary)); }
.btn-primary { border-color: transparent; color: var(--background); background: var(--foreground); }
.btn-primary:not(:disabled):hover { background: color-mix(in srgb, var(--foreground) 80%, transparent); }
.btn-ghost { border-color: transparent; color: var(--muted-foreground); background: transparent; }
.btn:is(.btn-block, .viz-tile) { inline-size: 100%; }
.btn.viz-tile { min-width: 0; white-space: normal; overflow-wrap: anywhere; }
.btn:is([aria-pressed="true"], [aria-selected="true"], .is-selected) {
  border-color: transparent; color: var(--primary-foreground); background: var(--primary); }
.btn.viz-tile:is([aria-pressed="true"], [aria-selected="true"], .is-selected) {
  color: inherit; background: inherit; box-shadow: 0 0 0 2px var(--ring); }
.btn:disabled { cursor: not-allowed; opacity: 0.4; }
.btn:focus-visible, .nav-link:focus-visible { outline: 2px solid var(--ring); outline-offset: 2px; }
.form-label { display: block; margin-bottom: 6px; color: var(--foreground); }
.form-control, .form-select { display: block; width: 100%; min-height: 28px; padding: 0 8px;
  border: 1px solid var(--input); border-radius: 10px; color: var(--foreground);
  background: var(--secondary); font: inherit; }
textarea.form-control { min-height: 72px; padding: 8px 10px; resize: vertical; }
.form-control:focus-visible, .form-select:focus-visible { outline: none; border-color: var(--ring);
  box-shadow: inset 0 0 0 1px var(--ring); }
.form-control-color[type="color"] { width: 40px; height: 28px; padding: 3px; }
.form-range { display: block; width: 100%; accent-color: var(--primary); }
.form-check { display: flex; min-height: 20px; align-items: center; gap: 6px; }
.form-check-input { margin: 0; accent-color: var(--primary); }
.form-check-label { color: var(--foreground); }
.table-responsive { width: 100%; overflow-x: auto; }
.table { width: 100%; border-collapse: collapse; color: var(--foreground); font: inherit; text-align: start; }
.table :is(th, td) { padding-block: 10px; padding-inline: 0 24px; overflow-wrap: anywhere;
  border-bottom: 1px solid var(--border); text-align: start; vertical-align: top; }
.table :is(th, td):last-child { padding-inline-end: 0; }
.table :is(caption, thead th) { font-weight: 500; }
.table tbody tr:last-child :is(th, td) { border-bottom: 0; }
.table.table-sm :is(th, td) { padding-block: 6px; }
.table .text-end { font-variant-numeric: tabular-nums; }
.nav.nav-pills { display: flex; flex-wrap: wrap; gap: 4px; }
.nav-pills.nav-justified > .nav-link { flex: 1 1 0; }
.nav-link { appearance: none; min-height: 28px; padding: 0 10px; border: 0; border-radius: 9999px;
  color: var(--muted-foreground); background: transparent; font: inherit; cursor: pointer; }
.nav-link:is(.active, [aria-selected="true"]) { color: var(--foreground); background: var(--muted); }
.nav-link:disabled, .nav-link[aria-disabled="true"] { opacity: 0.4; cursor: not-allowed; }
.progress { height: 8px; overflow: hidden; border-radius: 9999px; background: var(--muted); }
.progress-bar { height: 100%; background: var(--primary); }
.tooltip { position: fixed; z-index: 50; top: 0; left: 0; max-width: min(20rem, calc(100vw - 10px));
  padding: 4px 8px; border: 1px solid var(--border); border-radius: 10px; color: var(--popover-foreground);
  background: var(--popover); font-size: calc(var(--font-size-base) - 1px); line-height: 1.4;
  overflow-wrap: break-word; pointer-events: none; user-select: none; }
svg.lucide { display: block; width: 16px !important; height: 16px !important; flex: none; stroke-width: 1.6; }
"#;

/// `window.openai` stub installed before the fragment runs. State lives in
/// memory only; follow-ups and external opens are deliberately inert.
fn bridge_script(dark: bool, reduced_motion: bool) -> String {
    let theme = if dark { "dark" } else { "light" };
    format!(
        r#"(() => {{
  const theme = "{theme}";
  document.documentElement.dataset.theme = theme;
  if ({reduced_motion}) {{
    const original = window.matchMedia.bind(window);
    window.matchMedia = (query) => /prefers-reduced-motion\s*:\s*reduce/.test(String(query))
      ? {{ matches: true, media: String(query), onchange: null,
          addListener() {{}}, removeListener() {{}},
          addEventListener() {{}}, removeEventListener() {{}}, dispatchEvent() {{ return false; }} }}
      : original(query);
  }}
  let widgetState = null;
  const publish = (globals) => window.dispatchEvent(
    new CustomEvent("openai:set_globals", {{ detail: {{ globals }} }}));
  window.openai = {{
    theme, visualizationTheme: theme, visualizationStyleVariables: {{}},
    widgetState, statePersistence: "none", stateModelContext: "none",
    async setWidgetState(next) {{
      const value = typeof next === "function" ? next(widgetState) : next;
      if (value == null || typeof value !== "object" || Array.isArray(value)) {{
        throw new TypeError("Widget state must be a JSON object");
      }}
      widgetState = {{ modelContent: null, privateContent: null, ...JSON.parse(JSON.stringify(value)) }};
      window.openai.widgetState = widgetState;
      publish({{ widgetState }});
    }},
    async sendFollowUpMessage() {{}},
    openExternal() {{}},
  }};
  publish(window.openai);
}})();"#
    )
}

/// Tabs, `[data-tooltip]` and Lucide placeholders, run after the fragment.
const RUNTIME_SCRIPT: &str = r#"(() => {
  document.addEventListener("click", (event) => {
    const tab = event.target instanceof Element ? event.target.closest('[role="tab"]') : null;
    if (!tab || tab.disabled || tab.getAttribute("aria-disabled") === "true") return;
    const list = tab.closest('[role="tablist"]');
    if (!list) return;
    for (const other of list.querySelectorAll('[role="tab"]')) {
      const selected = other === tab;
      other.classList.toggle("active", selected);
      other.setAttribute("aria-selected", String(selected));
      const panelId = other.getAttribute("aria-controls");
      const panel = panelId ? document.getElementById(panelId) : null;
      if (panel && panelId !== tab.getAttribute("aria-controls")) panel.hidden = true;
    }
    const target = document.getElementById(tab.getAttribute("aria-controls") || "");
    if (target) target.hidden = false;
  });
  let tip = null;
  const hide = () => { tip?.remove(); tip = null; };
  const show = (trigger) => {
    const text = trigger.getAttribute("data-tooltip")?.trim();
    if (!text) return;
    hide();
    tip = document.createElement("div");
    tip.className = "tooltip";
    tip.setAttribute("role", "tooltip");
    tip.textContent = text;
    document.body.appendChild(tip);
    const box = trigger.getBoundingClientRect();
    const own = tip.getBoundingClientRect();
    const placement = trigger.getAttribute("data-tooltip-placement") || "top";
    let x = box.left + box.width / 2 - own.width / 2;
    let y = box.top - own.height - 5;
    if (placement === "bottom" || y < 5) y = box.bottom + 5;
    if (placement === "left") { x = box.left - own.width - 5; y = box.top + box.height / 2 - own.height / 2; }
    if (placement === "right") { x = box.right + 5; y = box.top + box.height / 2 - own.height / 2; }
    x = Math.max(5, Math.min(x, innerWidth - own.width - 5));
    y = Math.max(5, Math.min(y, innerHeight - own.height - 5));
    tip.style.transform = `translate(${Math.round(x)}px, ${Math.round(y)}px)`;
  };
  const triggerOf = (event) => event.target instanceof Element ? event.target.closest("[data-tooltip]") : null;
  document.addEventListener("pointerover", (event) => { const t = triggerOf(event); if (t) show(t); });
  document.addEventListener("pointerout", (event) => { if (triggerOf(event)) hide(); });
  document.addEventListener("focusin", (event) => { const t = triggerOf(event); if (t) show(t); });
  document.addEventListener("focusout", hide);
  document.addEventListener("keydown", (event) => { if (event.key === "Escape") hide(); });
  const icons = () => globalThis.lucide?.createIcons({ attrs: { width: 16, height: 16 } });
  if (globalThis.lucide) icons();
  else document.getElementById("zeron-visualization-lucide")?.addEventListener("load", icons, { once: true });
})();"#;

/// The sandboxed frame's document: policy, base styles, bridge, fragment,
/// runtime.
pub fn frame_document(
    fragment: &str,
    title: &str,
    theme: &FragmentTheme,
    reduced_motion: bool,
) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\" data-theme=\"{scheme}\">\n<head>\n<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<meta name=\"referrer\" content=\"no-referrer\">\n\
<meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">\n\
<title>{title}</title>\n<style>\n{style}</style>\n</head>\n<body>\n\
<script>{bridge}</script>\n{fragment}\n\
<script id=\"zeron-visualization-lucide\" async src=\"{LUCIDE_SRC}\"></script>\n\
<script>{RUNTIME_SCRIPT}</script>\n</body>\n</html>\n",
        scheme = if theme.dark { "dark" } else { "light" },
        csp = escape_attr(&frame_csp()),
        title = escape_text(title),
        style = base_stylesheet(theme, reduced_motion),
        bridge = bridge_script(theme.dark, reduced_motion),
    )
}

/// The document the browser loads: one sandboxed frame, centered at the
/// skill's design width, filling the pane's height.
pub fn host_document(
    fragment: &str,
    title: &str,
    wide: bool,
    theme: &FragmentTheme,
    reduced_motion: bool,
) -> String {
    let background = theme
        .vars
        .iter()
        .find(|(name, _)| *name == "--background")
        .map_or("transparent", |(_, value)| value.as_str());
    let width = if wide { WIDE_WIDTH } else { NORMAL_WIDTH };
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<meta name=\"referrer\" content=\"no-referrer\">\n\
<meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">\n\
<title>{title}</title>\n<style>:root{{color-scheme:{scheme};background:{background}}}\
html,body{{margin:0;height:100%}}body{{box-sizing:border-box;padding:16px}}\
iframe{{display:block;width:100%;max-width:{width}px;height:calc(100vh - 32px);margin:0 auto;border:0;background:transparent}}\
</style>\n</head>\n<body>\n<iframe sandbox=\"allow-scripts\" referrerpolicy=\"no-referrer\" \
title=\"{title_attr}\" srcdoc=\"{srcdoc}\"></iframe>\n</body>\n</html>\n",
        csp = escape_attr(&host_csp()),
        title = escape_text(title),
        title_attr = escape_attr(title),
        scheme = if theme.dark { "dark" } else { "light" },
        srcdoc = escape_attr(&frame_document(fragment, title, theme, reduced_motion)),
    )
}

/// Why a fragment can't be shown, phrased as what the user can do next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FragmentError {
    Missing { path: String },
    OutsideWorkspace { path: String },
    TooLarge { bytes: u64 },
    CompleteDocument { path: String },
    NotText { path: String },
    Unavailable { detail: String },
    Unsupported,
}

impl FragmentError {
    pub fn title(&self) -> &'static str {
        match self {
            Self::Missing { .. } => "Visualization file not found",
            Self::OutsideWorkspace { .. } => "Visualization is outside this workspace",
            Self::TooLarge { .. } => "Visualization is too large",
            Self::CompleteDocument { .. } => "Not an HTML fragment",
            Self::NotText { .. } => "Visualization isn’t readable text",
            Self::Unavailable { .. } => "Couldn’t read the visualization",
            Self::Unsupported => "Rendered visualizations aren’t available here",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Missing { path } => format!(
                "{path} no longer exists in this chat’s workspace. Ask the agent to recreate it, then try again."
            ),
            Self::OutsideWorkspace { path } => format!(
                "{path} isn’t inside this chat’s checkout. Ask the agent to save the fragment inside the project."
            ),
            Self::TooLarge { bytes } => format!(
                "The fragment is {} KB; visualizations are limited to 1 MB. Ask the agent to downsample its data.",
                bytes.div_ceil(1024)
            ),
            Self::CompleteDocument { path } => format!(
                "{path} contains <html>, <head> or <body>. Open its .preview.html in your browser, or view the source."
            ),
            Self::NotText { path } => {
                format!("{path} isn’t UTF-8 text. View the source to inspect it.")
            }
            Self::Unavailable { detail } => {
                format!("{detail} Check that the chat’s device is online, then try again.")
            }
            Self::Unsupported => {
                "This platform has no embedded renderer. View the source, or open the preview in your browser.".into()
            }
        }
    }
}

/// Validate a workspace read for rendering.
pub fn fragment_text(
    result: Result<WorkspaceFileText, FilesClientError>,
    path: &str,
) -> Result<String, FragmentError> {
    let file = result.map_err(|error| {
        let detail = error.to_string();
        let lower = detail.to_ascii_lowercase();
        if lower.contains("not found")
            || lower.contains("no such file")
            || lower.contains("does not exist")
        {
            FragmentError::Missing { path: path.into() }
        } else if lower.contains("outside") || lower.contains("escape") {
            FragmentError::OutsideWorkspace { path: path.into() }
        } else {
            FragmentError::Unavailable {
                detail: format!("{path} couldn’t be read ({detail})."),
            }
        }
    })?;
    if file.size > MAX_FRAGMENT_BYTES as u64 {
        return Err(FragmentError::TooLarge { bytes: file.size });
    }
    let text = match file.text {
        Some(text) if !file.truncated => text,
        Some(_) => return Err(FragmentError::TooLarge { bytes: file.size }),
        None => return Err(FragmentError::NotText { path: path.into() }),
    };
    if text.len() > MAX_FRAGMENT_BYTES {
        return Err(FragmentError::TooLarge {
            bytes: text.len() as u64,
        });
    }
    if is_complete_document(&text) {
        return Err(FragmentError::CompleteDocument { path: path.into() });
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(text: Option<&str>, size: u64, truncated: bool) -> WorkspaceFileText {
        WorkspaceFileText {
            checkout_id: "c".into(),
            path: "work/a.fragment.html".into(),
            text: text.map(str::to_owned),
            content_hash: None,
            size,
            modified_at: None,
            encoding: mast_proto::WorkspaceTextEncoding::Utf8,
            line_ending: None,
            read_only_reason: None,
            truncated,
        }
    }

    #[test]
    fn host_document_sandboxes_the_fragment_and_injects_theme_variables() {
        let theme = FragmentTheme::from_theme(&Theme::default());
        let fragment =
            r#"<div id="chart" class="card">Q3 "sales" & more</div><script>draw()</script>"#;
        let document = host_document(fragment, "Sales <Q3>", true, &theme, false);
        // Exactly one frame, sandboxed to scripts only.
        assert_eq!(document.matches("<iframe").count(), 1);
        assert!(
            document.contains(r#"<iframe sandbox="allow-scripts" referrerpolicy="no-referrer""#)
        );
        for forbidden in [
            "allow-same-origin",
            "allow-top-navigation",
            "allow-popups",
            "allow-forms",
        ] {
            assert!(!document.contains(forbidden), "{forbidden}");
        }
        // The outer document carries no script of its own and never sees the
        // fragment as markup.
        let srcdoc_start = document.find("srcdoc=\"").unwrap();
        let outer = &document[..srcdoc_start];
        assert!(!outer.contains("<script"));
        assert!(!document.contains("<div id=\"chart\""));
        assert!(document.contains("max-width:1024px"));
        assert!(document.contains("Sales &lt;Q3&gt;"));
        // Policies: outer and frame both deny everything outside the CDN list.
        assert!(
            outer.contains("default-src &#39;none&#39;") || outer.contains("default-src 'none'")
        );
        assert!(outer.contains("connect-src blob: data:"));
        assert!(outer.contains("frame-src 'self' about:"));

        let frame = frame_document(fragment, "Sales", &theme, false);
        assert!(frame.contains(fragment));
        assert!(frame.contains("Content-Security-Policy"));
        assert!(frame.contains("frame-src 'none'"));
        assert!(frame.contains("form-action 'none'"));
        assert!(frame.contains("https://cdn.jsdelivr.net"));
        assert!(!frame.contains("file:"));
        assert!(frame.contains("<meta name=\"referrer\" content=\"no-referrer\">"));
        let default = Theme::default();
        for (name, value) in [
            ("--background", css(default.bg)),
            ("--foreground", css(default.text)),
            ("--primary", css(default.accent)),
            ("--border", css(default.border)),
        ] {
            assert!(
                frame.contains(&format!("{name}: {value};")),
                "{name} missing"
            );
        }
        assert!(frame.contains("window.openai"));
        assert!(frame.find("window.openai").unwrap() < frame.find(fragment).unwrap());
    }

    #[test]
    fn reduced_motion_is_forced_or_left_to_the_media_query() {
        let theme = FragmentTheme::from_theme(&Theme::default());
        let forced = frame_document("<p>x</p>", "x", &theme, true);
        let normal = frame_document("<p>x</p>", "x", &theme, false);
        assert!(forced.contains("if (true)"));
        assert!(!forced.contains("@media (prefers-reduced-motion: reduce)"));
        assert!(forced.contains("animation-duration: 0.01ms"));
        assert!(normal.contains("if (false)"));
        assert!(normal.contains("@media (prefers-reduced-motion: reduce)"));
    }

    #[test]
    fn theme_appearance_selects_the_color_scheme() {
        let mut light = Theme::default();
        light.appearance = crate::theme::Appearance::Light;
        let dark_flag = Theme::default().appearance.is_dark();
        let fragment_theme = FragmentTheme::from_theme(&light);
        assert!(!fragment_theme.dark);
        assert!(
            frame_document("<p/>", "x", &fragment_theme, false).contains("data-theme=\"light\"")
        );
        assert_eq!(FragmentTheme::from_theme(&Theme::default()).dark, dark_flag);
    }

    #[test]
    fn workspace_reads_map_to_actionable_errors() {
        let path = "work/a.fragment.html";
        assert_eq!(
            fragment_text(Ok(file(Some("<div>ok</div>"), 13, false)), path),
            Ok("<div>ok</div>".into())
        );
        let missing = fragment_text(
            Err(FilesClientError::Request(
                "file not found: work/a.fragment.html".into(),
            )),
            path,
        )
        .unwrap_err();
        assert_eq!(missing, FragmentError::Missing { path: path.into() });
        assert!(missing.message().contains("Ask the agent to recreate it"));
        assert!(matches!(
            fragment_text(Err(FilesClientError::Transport("closed".into())), path),
            Err(FragmentError::Unavailable { .. })
        ));
        assert!(matches!(
            fragment_text(Ok(file(Some("x"), 2_000_000, false)), path),
            Err(FragmentError::TooLarge { .. })
        ));
        assert!(matches!(
            fragment_text(Ok(file(Some("x"), 10, true)), path),
            Err(FragmentError::TooLarge { .. })
        ));
        assert!(matches!(
            fragment_text(Ok(file(None, 10, false)), path),
            Err(FragmentError::NotText { .. })
        ));
        let document =
            fragment_text(Ok(file(Some("<!doctype html><body>x"), 22, false)), path).unwrap_err();
        assert!(document.message().contains(".preview.html"));
    }
}
