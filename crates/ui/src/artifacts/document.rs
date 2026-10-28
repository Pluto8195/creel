//! Web documents for visual artifacts. Every kind goes through the Codex
//! visualize host (`browser::fragment`): an outer document whose only content
//! is a `sandbox="allow-scripts"` srcdoc frame under the visualize CSP, so
//! fragments, pages, Mermaid diagrams, and render checks share one sandbox.
//!
//! - fragment: `fragment::host_document` exactly as the visualize card does;
//! - page: the same outer policy, the page as the frame's whole document,
//!   full width (pages bring their own layout);
//! - Mermaid: a fragment that loads Mermaid from the allowed jsDelivr CDN
//!   and renders the source with `securityLevel: "strict"`;
//! - image (render checks only): an `<img>` fragment.
//!
//! Render checks add a probe ([`PROBE_SCRIPT`]) inside the frame and a
//! collector in the outer document; the native side reads the collector's
//! `window.__mastProbe` once the probe has posted its report.
use crate::browser::fragment::{self, FragmentTheme, escape_attr, escape_text};

use super::bridge::HOST_SCRIPT as BRIDGE_HOST_SCRIPT;
use super::comments::FRAME_SCRIPT as COMMENT_FRAME_SCRIPT;
use super::find::{FRAME_SCRIPT as FIND_FRAME_SCRIPT, HOST_SCRIPT as FIND_HOST_SCRIPT};
use super::model::WebKind;
use super::question_html::{CSS as QUESTION_CSS, SCRIPT as QUESTION_FRAME_SCRIPT};

/// Mermaid build the diagram wrapper loads (on the visualize allowlist).
pub(crate) const MERMAID_SRC: &str = "https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.min.js";

/// How long the probe waits after `load` before measuring (fonts, icons,
/// first animation frames).
pub(crate) const SETTLE_MS: u32 = 600;

/// Upper bound the probe waits for Mermaid diagrams to finish.
pub(crate) const MERMAID_WAIT_MS: u32 = 4000;

/// The outer document for `kind` + `source`. `probe` adds the render-check
/// probe and collector.
pub(crate) fn web_document(
    kind: WebKind,
    source: &str,
    title: &str,
    mime_type: &str,
    theme: &FragmentTheme,
    reduced_motion: bool,
    probe: bool,
) -> String {
    // Every frame carries the link and find bridges; render checks add the
    // probe.
    let frame_scripts = if probe {
        format!(
            "{LINK_FRAME_SCRIPT}{FIND_FRAME_SCRIPT}{QUESTION_FRAME_SCRIPT}{}",
            probe_script()
        )
    } else {
        // Only the app's own view gets the comment + answer bridge; render
        // checks and compare frames stay passive.
        format!(
            "{LINK_FRAME_SCRIPT}{FIND_FRAME_SCRIPT}{QUESTION_FRAME_SCRIPT}{COMMENT_FRAME_SCRIPT}"
        )
    };
    let probe_script = Some(frame_scripts);
    let document = match kind {
        WebKind::Fragment => {
            let body = with_probe_prefix(&with_question_style(source), probe_script.as_deref());
            fragment::host_document(&body, title, false, theme, reduced_motion)
        }
        WebKind::Mermaid => {
            let body = with_probe_prefix(
                &mermaid_fragment(source, theme.dark),
                probe_script.as_deref(),
            );
            fragment::host_document(&body, title, true, theme, reduced_motion)
        }
        WebKind::Image => {
            let body = with_probe_prefix(
                &image_fragment(source, mime_type, title),
                probe_script.as_deref(),
            );
            fragment::host_document(&body, title, true, theme, reduced_motion)
        }
        WebKind::Page => page_host_document(
            &inject_head_markup(source, &question_style()),
            title,
            theme,
            probe_script.as_deref(),
        ),
    };
    let document = with_link_host(&document);
    if probe {
        with_collector(&document)
    } else {
        document
    }
}

/// The viewport a compare render uses for each pane (and the offscreen
/// renders behind "Highlight changes" and onion skin), so both revisions lay
/// out at the same width and region boxes line up with the panes.
pub(crate) const COMPARE_VIEWPORT_WIDTH: u32 = 520;
/// Tall enough to capture most plans in one snapshot.
pub(crate) const COMPARE_VIEWPORT_HEIGHT: u32 = 1400;

/// The most CSS pixels tall a compare render grows to. Taller documents are
/// compared up to this height and the view says so
/// ([`compare_coverage`]). 16 000 keeps the offscreen view within common
/// GPU texture limits and, at 1x, within [`COMPARE_MAX_PIXELS`].
pub(crate) const COMPARE_MAX_HEIGHT: u32 = 16_000;
/// The most pixels in one compare snapshot (RGBA8: 4 bytes each, and the
/// diff keeps a few masks of the same size), so 20 M is 80 MB per image.
pub(crate) const COMPARE_MAX_PIXELS: u64 = 20_000_000;

/// How a compare render is taken for a document `content_height` CSS pixels
/// tall: the view height (CSS px, between the base viewport and
/// [`COMPARE_MAX_HEIGHT`]) and the snapshot width in pixels, `None` for the
/// native scale (`scale` is 2 on Retina). The snapshot is downscaled, never
/// below 1x, when height x scale would exceed [`COMPARE_MAX_PIXELS`].
pub(crate) fn compare_render_plan(content_height: u32, scale: f32) -> (u32, Option<u32>) {
    let scale = if scale.is_finite() && scale >= 1.0 {
        scale
    } else {
        1.0
    };
    let height = content_height.clamp(COMPARE_VIEWPORT_HEIGHT, COMPARE_MAX_HEIGHT);
    let area = f64::from(COMPARE_VIEWPORT_WIDTH) * f64::from(height);
    let affordable = (COMPARE_MAX_PIXELS as f64 / area).sqrt() as f32;
    let effective = affordable.clamp(1.0, scale);
    let width = (effective < scale).then(|| (COMPARE_VIEWPORT_WIDTH as f32 * effective) as u32);
    (height, width)
}

/// `Some((compared, total))` CSS pixels when a document `content_height`
/// tall is cut off at [`COMPARE_MAX_HEIGHT`], `None` when the render covers
/// all of it.
pub(crate) fn compare_coverage(content_height: u32) -> Option<(u32, u32)> {
    (content_height > COMPARE_MAX_HEIGHT).then_some((COMPARE_MAX_HEIGHT, content_height))
}

/// "Compared the first 16,000 px of 42,000" (digit groups of three).
pub(crate) fn coverage_notice((compared, total): (u32, u32)) -> String {
    fn grouped(n: u32) -> String {
        let digits = n.to_string();
        let mut out = String::new();
        for (i, c) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i) % 3 == 0 {
                out.push(',');
            }
            out.push(c);
        }
        out
    }
    format!(
        "Compared the first {} px of {}",
        grouped(compared),
        grouped(total)
    )
}

/// One pane's sandboxed frame document for the compare view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompareFrame {
    pub(crate) html: String,
    /// The frame's width in the host (what the tab's wrapper gives the
    /// frame at [`COMPARE_VIEWPORT_WIDTH`]).
    pub(crate) width: u32,
    /// Where the frame's top-left sits inside an offscreen render of the same
    /// document at the compare viewport (the host's padding and centering);
    /// region boxes are translated by it.
    pub(crate) origin: (u32, u32),
}

/// The frame document of `kind` + `source` exactly as [`web_document`] puts
/// it in its sandbox frame, plus `script` (scroll sync) and `overlay`
/// (positioned change boxes) inside the frame.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compare_frame(
    kind: WebKind,
    source: &str,
    title: &str,
    mime_type: &str,
    theme: &FragmentTheme,
    reduced_motion: bool,
    script: &str,
    overlay: &str,
) -> CompareFrame {
    let scripts = format!("{LINK_FRAME_SCRIPT}{QUESTION_FRAME_SCRIPT}{script}");
    let body = |fragment: &str| format!("<script>{scripts}</script>\n{fragment}\n{overlay}");
    let (html, wide) = match kind {
        WebKind::Fragment => (
            fragment::frame_document(
                &body(&with_question_style(source)),
                title,
                theme,
                reduced_motion,
            ),
            false,
        ),
        WebKind::Mermaid => (
            fragment::frame_document(
                &body(&mermaid_fragment(source, theme.dark)),
                title,
                theme,
                reduced_motion,
            ),
            true,
        ),
        WebKind::Image => (
            fragment::frame_document(
                &body(&image_fragment(source, mime_type, title)),
                title,
                theme,
                reduced_motion,
            ),
            true,
        ),
        WebKind::Page => {
            let page = inject_head_script(&inject_head_markup(source, &question_style()), &scripts);
            let lower = page.to_ascii_lowercase();
            let page = match lower.rfind("</body>") {
                Some(at) => format!("{}{overlay}{}", &page[..at], &page[at..]),
                None => format!("{page}{overlay}"),
            };
            (page, false)
        }
    };
    let (width, origin) = if kind == WebKind::Page {
        (COMPARE_VIEWPORT_WIDTH, (0, 0))
    } else {
        let max = if wide {
            fragment::WIDE_WIDTH
        } else {
            fragment::NORMAL_WIDTH
        };
        (
            (COMPARE_VIEWPORT_WIDTH - 32).min(max),
            (
                16 + (COMPARE_VIEWPORT_WIDTH - 32).saturating_sub(max) / 2,
                16,
            ),
        )
    };
    CompareFrame {
        html,
        width,
        origin,
    }
}

/// In the frame: clicks on links (other than same-document `#id`) post the
/// link's raw `href` to the outer document instead of navigating. The frame
/// is `sandbox="allow-scripts"` (no popups, no top navigation) and its
/// `about:srcdoc` base cannot resolve relative links like `ui/foo.tsx:120`,
/// so the page's own `href` text is what the host needs.
pub(crate) const LINK_FRAME_SCRIPT: &str = r##"(() => {
  if (window.__mastLinksInstalled) return;
  window.__mastLinksInstalled = true;
  const handle = (event) => {
    const node = event.target;
    const a = node && node.closest ? node.closest("a, area") : null;
    if (!a) return;
    const href = (a.getAttribute("href") || a.getAttribute("xlink:href") || "").trim();
    if (href.startsWith("#")) return;
    event.preventDefault();
    event.stopPropagation();
    if (event.type === "click" && href) {
      try { parent.postMessage({ mastLink: href }, "*"); } catch (_) {}
    }
  };
  document.addEventListener("click", handle, true);
  document.addEventListener("auxclick", handle, true);
})();
"##;

/// In the outer document: turns the frame's link message into a navigation
/// the native delegate recognises (`mast-link:<encoded href>`, see
/// `browser::document_view`), and cancels.
pub(crate) const LINK_HOST_SCRIPT: &str = r##"window.addEventListener("message", (event) => {
  const frame = document.querySelector("iframe");
  if (!frame || event.source !== frame.contentWindow) return;
  const href = event.data && event.data.mastLink;
  if (typeof href !== "string" || href.length > 4096) return;
  const a = document.createElement("a");
  a.href = "mast-link:" + encodeURIComponent(href);
  a.rel = "noreferrer";
  a.style.display = "none";
  document.body.appendChild(a);
  a.click();
  a.remove();
});"##;

fn with_link_host(document: &str) -> String {
    document.replacen(
        "</head>",
        &format!(
            "<script>{LINK_HOST_SCRIPT}{FIND_HOST_SCRIPT}{BRIDGE_HOST_SCRIPT}</script>\n</head>"
        ),
        1,
    )
}

fn with_probe_prefix(body: &str, probe: Option<&str>) -> String {
    match probe {
        Some(probe) => format!("<script>{probe}</script>\n{body}"),
        None => body.to_owned(),
    }
}

/// A Mermaid source rendered in the fragment wrapper. The source is text
/// content of a `<pre>`, never markup.
pub(crate) fn mermaid_fragment(source: &str, dark: bool) -> String {
    let theme = if dark { "dark" } else { "default" };
    format!(
        "<div class=\"mast-mermaid\" style=\"display:flex;justify-content:center\">\
<pre class=\"mermaid\">{source}</pre></div>\n\
<script src=\"{MERMAID_SRC}\"></script>\n\
<script>(() => {{\n\
  const fail = (message) => {{\n\
    console.error(\"Mermaid: \" + message);\n\
    document.querySelectorAll(\"pre.mermaid\").forEach((el) => el.setAttribute(\"data-mast-error\", message));\n\
  }};\n\
  if (!globalThis.mermaid) {{ fail(\"the Mermaid library could not load\"); return; }}\n\
  mermaid.initialize({{ startOnLoad: false, securityLevel: \"strict\", theme: \"{theme}\" }});\n\
  mermaid.run({{ querySelector: \"pre.mermaid\" }}).catch((error) => fail(error && error.message ? error.message : String(error)));\n\
}})();</script>",
        source = escape_text(source),
    )
}

fn image_fragment(base64: &str, mime_type: &str, title: &str) -> String {
    let mime = if mime_type.starts_with("image/") {
        mime_type
    } else {
        "image/png"
    };
    format!(
        "<img alt=\"{alt}\" src=\"data:{mime};base64,{data}\" style=\"display:block;max-width:100%;height:auto;margin:0 auto\">",
        alt = escape_attr(title),
        data = base64.trim(),
    )
}

/// A standalone page in the visualize sandbox: the outer document carries
/// the visualize host policy (a srcdoc frame inherits it), the page is the
/// frame's whole document.
pub(crate) fn page_host_document(
    page: &str,
    title: &str,
    theme: &FragmentTheme,
    probe: Option<&str>,
) -> String {
    let background = theme
        .vars
        .iter()
        .find(|(name, _)| *name == "--background")
        .map_or("transparent", |(_, value)| value.as_str());
    let page = match probe {
        Some(probe) => inject_head_script(page, probe),
        None => page.to_owned(),
    };
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<meta name=\"referrer\" content=\"no-referrer\">\n\
<meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">\n\
<title>{title}</title>\n<style>:root{{color-scheme:{scheme};background:{background}}}\
html,body{{margin:0;height:100%}}\
iframe{{display:block;width:100%;height:100vh;border:0;background:transparent}}\
</style>\n</head>\n<body>\n<iframe sandbox=\"allow-scripts\" referrerpolicy=\"no-referrer\" \
title=\"{title_attr}\" srcdoc=\"{srcdoc}\"></iframe>\n</body>\n</html>\n",
        csp = escape_attr(&fragment::host_csp()),
        title = escape_text(title),
        title_attr = escape_attr(title),
        scheme = if theme.dark { "dark" } else { "light" },
        srcdoc = escape_attr(&page),
    )
}

/// `script` as the first thing a page runs: after `<head …>` when there is
/// one, else after `<html …>`, else at the very start.
pub(crate) fn inject_head_script(page: &str, script: &str) -> String {
    inject_head_markup(page, &format!("<script>{script}</script>"))
}

/// The stylesheet for `data-mast-question` cards, as a `<style>` element.
fn question_style() -> String {
    format!("<style>{QUESTION_CSS}</style>")
}

/// A fragment with the question card styles in front of it.
fn with_question_style(fragment: &str) -> String {
    format!("{}\n{fragment}", question_style())
}

/// `tag` (markup) as the first thing in a page: after `<head …>` when there
/// is one, else after `<html …>`, else at the very start.
fn inject_head_markup(page: &str, tag: &str) -> String {
    let lower = page.to_ascii_lowercase();
    for open in ["<head", "<html"] {
        if let Some(start) = lower.find(open)
            && let Some(end) = lower[start..].find('>')
        {
            let at = start + end + 1;
            return format!("{}{tag}{}", &page[..at], &page[at..]);
        }
    }
    format!("{tag}{page}")
}

/// The outer document's collector: keeps the frame's report where the
/// native side can read it (`window.__mastProbe`).
pub(crate) const COLLECTOR_SCRIPT: &str = "window.__mastProbe = null;\
window.addEventListener(\"message\", (event) => {\
 if (event.data && event.data.mastRenderProbe) window.__mastProbe = event.data.mastRenderProbe;\
});";

fn with_collector(document: &str) -> String {
    document.replacen(
        "</head>",
        &format!("<script>{COLLECTOR_SCRIPT}</script>\n</head>"),
        1,
    )
}

/// What the native side evaluates to read the report (`null` until ready).
pub(crate) const READ_PROBE_SCRIPT: &str =
    "window.__mastProbe ? JSON.stringify(window.__mastProbe) : null";

/// The in-frame probe: console errors, uncaught errors and rejections, CSP
/// violations, failed resource loads, Mermaid errors, empty render, and
/// layout warnings (horizontal overflow, elements outside the viewport, low
/// contrast text), plus per-element metrics the native side classifies into
/// the layout heuristics ([`super::layout_heuristics`]). Posts
/// `{mastRenderProbe: {errors, warnings, metrics}}` to the parent once the
/// page has loaded and settled.
pub(crate) fn probe_script() -> String {
    PROBE_TEMPLATE
        .replace("__SETTLE_MS__", &SETTLE_MS.to_string())
        .replace("__MERMAID_WAIT_MS__", &MERMAID_WAIT_MS.to_string())
}

const PROBE_TEMPLATE: &str = r##"(() => {
  if (window.__mastProbeInstalled) return;
  window.__mastProbeInstalled = true;
  const errors = [], warnings = [];
  const push = (list, text) => {
    text = String(text).slice(0, 400);
    if (list.length < 40 && !list.includes(text)) list.push(text);
  };
  const show = (value) => {
    if (value instanceof Error) return value.message;
    if (typeof value === "string") return value;
    try { return JSON.stringify(value); } catch (_) { return String(value); }
  };
  const original = console.error.bind(console);
  console.error = (...args) => { push(errors, "Console error: " + args.map(show).join(" ")); original(...args); };
  window.addEventListener("error", (event) => {
    const target = event.target;
    if (target && target !== window && (target.src || target.href)) {
      push(errors, "Failed to load " + String(target.tagName || "resource").toLowerCase() + ": " + (target.src || target.href));
      return;
    }
    push(errors, "JavaScript error: " + (event.message || "unknown error") + (event.lineno ? " (line " + event.lineno + ")" : ""));
  }, true);
  window.addEventListener("unhandledrejection", (event) => {
    const reason = event.reason;
    push(errors, "Unhandled promise rejection: " + (reason && reason.message ? reason.message : show(reason)));
  });
  document.addEventListener("securitypolicyviolation", (event) => {
    push(errors, "Blocked by the content security policy (" + event.violatedDirective + "): " + (event.blockedURI || "inline"));
  });
  const describe = (el) => {
    let text = el.tagName.toLowerCase();
    if (el.id) text += "#" + el.id;
    else if (el.classList && el.classList.length) text += "." + Array.from(el.classList).slice(0, 2).join(".");
    return text;
  };
  const rgb = (value) => {
    const m = String(value).match(/rgba?\(([^)]+)\)/);
    if (!m) return null;
    const parts = m[1].split(/[ ,\/]+/).filter(Boolean).map(Number);
    return { r: parts[0], g: parts[1], b: parts[2], a: parts.length > 3 ? parts[3] : 1 };
  };
  const luminance = (c) => {
    const ch = (v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); };
    return 0.2126 * ch(c.r) + 0.7152 * ch(c.g) + 0.0722 * ch(c.b);
  };
  const background = (el) => {
    for (let node = el; node; node = node.parentElement) {
      const c = rgb(getComputedStyle(node).backgroundColor);
      if (c && c.a > 0.5) return c;
    }
    return rgb(getComputedStyle(document.documentElement).backgroundColor) ||
      (matchMedia("(prefers-color-scheme: dark)").matches ? { r: 0, g: 0, b: 0, a: 1 } : { r: 255, g: 255, b: 255, a: 1 });
  };
  const report = () => {
    document.querySelectorAll("pre.mermaid, div.mermaid").forEach((el) => {
      const failed = el.getAttribute("data-mast-error");
      if (failed) push(errors, "Mermaid: " + failed);
      else if (/syntax error|parse error/i.test(el.textContent || "")) push(errors, "Mermaid: " + (el.textContent || "").trim().slice(0, 200));
    });
    const body = document.body;
    const text = body ? (body.innerText || "").trim() : "";
    const media = body ? Array.from(body.querySelectorAll("img, svg, canvas, video, iframe, object")).some((el) => {
      const r = el.getBoundingClientRect();
      return r.width > 4 && r.height > 4;
    }) : false;
    if (!text && !media) push(errors, "Empty render: nothing visible was drawn");
    const root = document.scrollingElement || document.documentElement;
    if (root.scrollWidth > root.clientWidth + 1) {
      push(warnings, "Horizontal overflow: content is " + root.scrollWidth + "px wide in a " + root.clientWidth + "px viewport");
    }
    let outside = 0, lowContrast = 0;
    const examples = [], contrastExamples = [];
    const all = body ? body.querySelectorAll("*") : [];
    for (let i = 0; i < all.length && i < 4000; i++) {
      const el = all[i];
      const r = el.getBoundingClientRect();
      if (r.width === 0 || r.height === 0) continue;
      if (r.right > window.innerWidth + 1 || r.left < -1) {
        outside++;
        if (examples.length < 3) examples.push(describe(el));
      }
      const own = Array.from(el.childNodes).some((n) => n.nodeType === 3 && n.textContent.trim());
      if (own && lowContrast < 50) {
        const style = getComputedStyle(el);
        const fg = rgb(style.color);
        if (fg && fg.a > 0.5 && style.visibility !== "hidden" && Number(style.opacity) > 0.3) {
          const bg = background(el);
          const a = luminance(fg), b = luminance(bg);
          const ratio = (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
          if (ratio < 3) { lowContrast++; if (contrastExamples.length < 3) contrastExamples.push(describe(el) + " " + ratio.toFixed(1) + ":1"); }
        }
      }
    }
    if (outside) push(warnings, outside + " element(s) extend outside the viewport horizontally (" + examples.join(", ") + ")");
    if (lowContrast) push(warnings, lowContrast + " text element(s) have contrast below 3:1 (" + contrastExamples.join(", ") + ")");
    const contentHeight = Math.ceil(Math.max(
      document.documentElement ? document.documentElement.scrollHeight : 0,
      document.body ? document.body.scrollHeight : 0));
    try { parent.postMessage({ mastRenderProbe: { errors, warnings, metrics, contentHeight } }, "*"); } catch (_) {}
  };
  const waitMermaid = (deadline) => {
    const pending = Array.from(document.querySelectorAll("pre.mermaid, div.mermaid")).some((el) =>
      !el.querySelector("svg") && !el.getAttribute("data-mast-error") && !el.getAttribute("data-processed"));
    if (pending && Date.now() < deadline) { setTimeout(() => waitMermaid(deadline), 100); return; }
    report();
  };
  const settle = () => setTimeout(() => waitMermaid(Date.now() + __MERMAID_WAIT_MS__), __SETTLE_MS__);
  if (document.readyState === "complete") settle();
  else window.addEventListener("load", settle, { once: true });
})();"##;

/// The probe's report as the native side reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct ProbeReport {
    #[serde(default)]
    pub(crate) errors: Vec<String>,
    #[serde(default)]
    pub(crate) warnings: Vec<String>,
    /// Per-element measurements for [`super::layout_heuristics::classify`].
    #[serde(default)]
    pub(crate) metrics: Vec<super::layout_heuristics::ElementMetrics>,
    /// The frame document's full scroll height in CSS pixels.
    #[serde(default, rename = "contentHeight")]
    pub(crate) content_height: Option<u32>,
}
