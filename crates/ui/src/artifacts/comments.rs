//! Comments on HTML and Mermaid artifacts: the frame script and the
//! host-side model (Artifacts phase 4 G).
//!
//! The frame script does three DOM things the host cannot: it reports the
//! reader's text selection (as a text-quote anchor), picks an element in
//! comment mode (hover outline, click to capture), and draws markers for the
//! open comments (CSS Custom Highlight for quotes, an outline attribute for
//! element anchors). Everything with decisions (status, outdated, ids,
//! storage) lives in Rust: `zeron_proto::resolve_anchor` decides found /
//! outdated from the revision source, so the list never depends on the page
//! having laid out. The script's text-index rules mirror
//! `zeron_proto::visible_text` / `find_quote`; where they differ the marker is
//! simply missing, never wrong.
//!
//! Messages in: `zeronComments {items: [{id, anchor}]}`, `zeronCommentMode`,
//! `zeronCommentFocus`. Messages out (see `bridge::FrameMessage`): `ready`,
//! `selection`, `selection-clear`, `draft`, `focus`.

use serde_json::json;
use zeron_proto::{ArtifactComment, ArtifactKind, ArtifactView, AnchorStatus, CommentStatus};

use super::bridge::frame_command_script;

/// What the list shows for one comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommentState {
    Open,
    /// Open, but its anchor is not in the revision on screen.
    Outdated,
    Resolved,
}

impl CommentState {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            CommentState::Open => "Open",
            CommentState::Outdated => "Outdated",
            CommentState::Resolved => "Resolved",
        }
    }
}

/// One row of the comments list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommentRow {
    pub(crate) comment: ArtifactComment,
    pub(crate) state: CommentState,
}

/// The rows for `comments` against the source on screen, oldest first
/// (comment ids order). Resolved comments keep their state; open ones are
/// outdated when [`zeron_proto::resolve_anchor`] cannot find them. With no
/// source (still loading), open comments count as open.
pub(crate) fn rows(
    kind: ArtifactKind,
    source: Option<&str>,
    comments: &[ArtifactComment],
) -> Vec<CommentRow> {
    comments
        .iter()
        .map(|comment| {
            let state = match comment.status {
                CommentStatus::Resolved => CommentState::Resolved,
                CommentStatus::Open => match source {
                    Some(source)
                        if zeron_proto::resolve_anchor(kind, source, &comment.anchor)
                            == AnchorStatus::Outdated =>
                    {
                        CommentState::Outdated
                    }
                    _ => CommentState::Open,
                },
            };
            CommentRow {
                comment: comment.clone(),
                state,
            }
        })
        .collect()
}

/// Open comments, outdated first so they are not missed, then the rest.
pub(crate) fn open_count(rows: &[CommentRow]) -> usize {
    rows.iter()
        .filter(|row| row.state != CommentState::Resolved)
        .count()
}

/// The script that hands the frame the markers to draw: the comments that
/// are open and found (outdated and resolved ones have no place on the
/// page).
pub(crate) fn markers_script(rows: &[CommentRow]) -> String {
    let items: Vec<_> = rows
        .iter()
        .filter(|row| row.state == CommentState::Open)
        .map(|row| json!({ "id": row.comment.id, "anchor": row.comment.anchor }))
        .collect();
    frame_command_script(&json!({ "zeronComments": { "items": items } }))
}

pub(crate) fn mode_script(on: bool) -> String {
    frame_command_script(&json!({ "zeronCommentMode": on }))
}

pub(crate) fn focus_script(id: &str) -> String {
    frame_command_script(&json!({ "zeronCommentFocus": id }))
}

/// Whether the tab can show the comments affordances for this artifact.
pub(crate) fn applies(artifact: &ArtifactView) -> bool {
    zeron_proto::kind_supports_comments(artifact.kind)
}

/// In the frame. Installs once.
pub(crate) const FRAME_SCRIPT: &str = r##"(() => {
  if (window.__zeronCommentsInstalled) return;
  window.__zeronCommentsInstalled = true;
  window.__zeronAnswerable = true;
  const H = window.CSS && CSS.highlights;
  const post = (o) => { try { parent.postMessage({ zeronMsg: o }, "*"); } catch (_) {} };
  const sheet = new CSSStyleSheet();
  sheet.replaceSync(
    "::highlight(zeron-comment){background:rgba(79,70,229,.22);background:color-mix(in srgb,var(--primary,#4f46e5) 26%,transparent);text-decoration:underline dotted}" +
    "::highlight(zeron-comment-cur){background:rgba(217,119,6,.35);background:color-mix(in srgb,var(--orange,#d97706) 38%,transparent)}" +
    "[data-zeron-commented]{outline:2px dotted var(--primary,#4f46e5)!important;outline-offset:2px}" +
    "[data-zeron-commented-cur]{outline:2px solid var(--orange,#d97706)!important}" +
    "[data-zeron-pick]{outline:2px solid var(--primary,#4f46e5)!important;outline-offset:2px;cursor:crosshair!important}"
  );
  document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
  const norm = (s) => String(s || "").replace(/\s+/g, " ").trim();
  const SKIP = "script,style,noscript,template";
  // The page's visible text with whitespace collapsed, and where each
  // character came from (text node + offset). A space separates blocks.
  let idx = null;
  const index = () => {
    if (idx) return idx;
    const chars = [], nodes = [], offs = [], first = new Map();
    let lastBlock = null;
    const walker = document.createTreeWalker(document.body || document.documentElement, NodeFilter.SHOW_TEXT);
    const block = (el) => {
      for (let n = el; n; n = n.parentElement) {
        const d = getComputedStyle(n).display;
        if (!d.startsWith("inline") || d === "inline-block" || d === "inline-flex") return n;
      }
      return null;
    };
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      const el = n.parentElement;
      if (!el || el.closest(SKIP)) continue;
      const b = block(el);
      if (b !== lastBlock && chars.length && chars[chars.length - 1] !== " ") { chars.push(" "); nodes.push(n); offs.push(0); }
      lastBlock = b;
      first.set(n, chars.length);
      const d = n.data;
      for (let i = 0; i < d.length; i++) {
        const ws = /\s/.test(d[i]);
        if (ws && (!chars.length || chars[chars.length - 1] === " ")) continue;
        chars.push(ws ? " " : d[i]); nodes.push(n); offs.push(i);
      }
    }
    return (idx = { text: chars.join(""), nodes, offs, first });
  };
  const invalidate = () => { idx = null; };
  const countChars = (node, upto) => { const x = index(); let i = x.first.get(node), c = 0; while (i + c < x.nodes.length && x.nodes[i + c] === node && x.offs[i + c] < upto) c++; return c; };
  // Index of a DOM boundary point in the text. A start boundary maps to the
  // first character at or after it, an end boundary to one past the last
  // character before it.
  const posOf = (node, offset, end) => {
    const x = index();
    if (node.nodeType === 3) return x.first.has(node) ? x.first.get(node) + countChars(node, offset) : null;
    const b = new Range(); b.setStart(node, offset); b.collapse(true);
    if (!end) { for (const n of x.first.keys()) if (b.comparePoint(n, 0) >= 0) return x.first.get(n); return x.text.length; }
    let last = null;
    for (const n of x.first.keys()) { if (b.comparePoint(n, n.data.length) <= 0) last = n; else break; }
    return last ? x.first.get(last) + countChars(last, last.data.length) : 0;
  };
  const rangeFromTo = (i0, i1) => {
    const x = index();
    if (i0 == null || i1 == null || i1 <= i0 || i0 >= x.nodes.length) return null;
    const r = new Range();
    r.setStart(x.nodes[i0], x.offs[i0]);
    r.setEnd(x.nodes[i1 - 1], x.offs[i1 - 1] + 1);
    return r;
  };
  const anchorFor = (i0, i1, el) => {
    const x = index();
    const exact = x.text.slice(i0, i1).trim().slice(0, 240);
    const start = x.text.indexOf(exact, i0);
    const a = start < 0 ? i0 : start;
    return {
      id: idOf(el) || undefined,
      selector: pathOf(el) || undefined,
      prefix: x.text.slice(Math.max(0, a - 40), a),
      exact,
      suffix: x.text.slice(a + exact.length, a + exact.length + 40),
    };
  };
  const nodeId = (el) => {
    const m = /^(?:flowchart|state|class|er|requirement)-(.+)-\d+$/.exec(el.id || "");
    return m ? "mermaid:" + m[1] : null;
  };
  const idOf = (el) => {
    for (let n = el && el.nodeType === 1 ? el : el && el.parentElement; n && n !== document.documentElement; n = n.parentElement) {
      const d = n.getAttribute && n.getAttribute("data-zeron-anchor");
      if (d) return d;
      const m = nodeId(n);
      if (m) return m;
      if (n.id && !/^mermaid-|^zeron-|^zq-/.test(n.id)) return n.id;
    }
    return null;
  };
  const pathOf = (el) => {
    const parts = [];
    for (let n = el && el.nodeType === 1 ? el : el && el.parentElement; n && n !== document.body && parts.length < 6; n = n.parentElement) {
      const same = n.parentElement ? Array.from(n.parentElement.children).filter((c) => c.localName === n.localName) : [n];
      parts.unshift(n.localName + (same.length > 1 ? ":nth-of-type(" + (same.indexOf(n) + 1) + ")" : ""));
    }
    return parts.join(" > ");
  };
  const byId = (id) => {
    if (!id) return null;
    if (id.startsWith("mermaid:")) {
      const n = id.slice(8), e = CSS.escape(n);
      return document.querySelector('[id^="flowchart-' + e + '-"],[id^="state-' + e + '-"],[id^="class-' + e + '-"],[id="' + e + '"]');
    }
    return document.getElementById(id) || document.querySelector('[data-zeron-anchor="' + CSS.escape(id) + '"]');
  };
  const rangeForQuote = (a) => {
    if (!a || !a.exact) return null;
    const x = index();
    let best = -1, bestScore = -1;
    for (let at = x.text.indexOf(a.exact); at >= 0; at = x.text.indexOf(a.exact, at + Math.max(1, a.exact.length))) {
      let s = 0;
      const before = x.text.slice(0, at).trimEnd(), after = x.text.slice(at + a.exact.length).trimStart();
      const p = norm(a.prefix), q = norm(a.suffix);
      while (s < p.length && before[before.length - 1 - s] === p[p.length - 1 - s]) s++;
      let t = 0;
      while (t < q.length && after[t] === q[t]) t++;
      if (s + t > bestScore) { best = at; bestScore = s + t; }
    }
    return best < 0 ? null : rangeFromTo(best, best + a.exact.length);
  };

  // ---- markers --------------------------------------------------------
  let items = [], marked = [], hits = [];
  const clearMarks = () => {
    if (H) { H.delete("zeron-comment"); H.delete("zeron-comment-cur"); }
    marked.forEach((e) => { e.removeAttribute("data-zeron-commented"); e.removeAttribute("data-zeron-commented-cur"); e.removeAttribute("data-zeron-comment-ids"); });
    marked = []; hits = [];
  };
  const draw = () => {
    clearMarks(); invalidate();
    const ranges = [];
    for (const it of items) {
      const r = rangeForQuote(it.anchor);
      if (r) { ranges.push(r); hits.push({ id: it.id, range: r }); continue; }
      const el = byId(it.anchor && it.anchor.id);
      if (el) {
        el.setAttribute("data-zeron-commented", "");
        el.setAttribute("data-zeron-comment-ids", ((el.getAttribute("data-zeron-comment-ids") || "") + " " + it.id).trim());
        marked.push(el); hits.push({ id: it.id, el });
      }
    }
    if (H && ranges.length) H.set("zeron-comment", new Highlight(...ranges));
  };
  const focus = (id) => {
    const h = hits.find((x) => x.id === id);
    if (!h) return;
    marked.forEach((e) => e.removeAttribute("data-zeron-commented-cur"));
    if (H) H.delete("zeron-comment-cur");
    if (h.range) { if (H) H.set("zeron-comment-cur", new Highlight(h.range)); const e = h.range.startContainer.parentElement; if (e) e.scrollIntoView({ block: "center" }); }
    else if (h.el) { h.el.setAttribute("data-zeron-commented-cur", ""); h.el.scrollIntoView({ block: "center" }); }
  };

  // ---- reporting the selection ---------------------------------------
  let sent = "", timer = 0;
  const reportSelection = () => {
    const s = getSelection();
    if (!s || s.isCollapsed || !s.rangeCount) { if (sent) { sent = ""; post({ t: "selection-clear" }); } return; }
    const r = s.getRangeAt(0);
    const host = r.commonAncestorContainer.nodeType === 1 ? r.commonAncestorContainer : r.commonAncestorContainer.parentElement;
    if (!host || host.closest(SKIP + ",textarea,input,[data-zeron-question] .zq-answer")) return;
    invalidate();
    const i0 = posOf(r.startContainer, r.startOffset, false), i1 = posOf(r.endContainer, r.endOffset, true);
    if (i0 == null || i1 == null || i1 - i0 < 2) { if (sent) { sent = ""; post({ t: "selection-clear" }); } return; }
    const anchor = anchorFor(i0, i1, host);
    if (!anchor.exact) return;
    const key = anchor.exact + "|" + anchor.prefix;
    if (key === sent) return;
    sent = key;
    post({ t: "selection", anchor, quote: anchor.exact });
  };
  document.addEventListener("selectionchange", () => { clearTimeout(timer); timer = setTimeout(reportSelection, 180); });

  // ---- comment mode: hover an element, click to capture ---------------
  let mode = false, hot = null;
  const COMMENTABLE = /^(h[1-6]|p|li|tr|td|th|pre|blockquote|figure|figcaption|section|article|dt|dd|summary|button|img|svg|table)$/;
  const target = (e) => {
    for (let n = e; n && n !== document.body && n !== document.documentElement; n = n.parentElement) {
      if (n.closest && n.closest("[data-zeron-question]") && n.matches && !n.hasAttribute("data-zeron-question")) continue;
      const cls = n.classList;
      if (n.id || (n.hasAttribute && n.hasAttribute("data-zeron-anchor")) || COMMENTABLE.test(n.localName) || (cls && (cls.contains("node") || cls.contains("cluster") || cls.contains("actor") || cls.contains("note")))) return n;
    }
    return null;
  };
  const unhot = () => { if (hot) { hot.removeAttribute("data-zeron-pick"); hot = null; } };
  window.addEventListener("mouseover", (e) => {
    if (!mode) return;
    const t = target(e.target);
    if (t === hot) return;
    unhot();
    if (t) { hot = t; t.setAttribute("data-zeron-pick", ""); }
  }, true);
  window.addEventListener("mouseleave", unhot);
  window.addEventListener("click", (e) => {
    if (mode) {
      const t = target(e.target);
      e.preventDefault(); e.stopPropagation();
      if (!t) return;
      unhot();
      invalidate();
      const x = index();
      const w = document.createTreeWalker(t, NodeFilter.SHOW_TEXT);
      let a = null, z = null;
      for (let n = w.nextNode(); n; n = w.nextNode()) if (x.first.has(n)) { if (a == null) a = n; z = n; }
      let anchor;
      if (a) anchor = anchorFor(x.first.get(a), x.first.get(z) + countChars(z, z.data.length), t);
      else anchor = { id: idOf(t) || undefined, selector: pathOf(t) || undefined, exact: norm(t.textContent).slice(0, 240), prefix: "", suffix: "" };
      anchor.id = (t.getAttribute("data-zeron-anchor") || nodeId(t) || (t.id && !/^mermaid-/.test(t.id) ? t.id : "")) || idOf(t) || undefined;
      if (!anchor.exact && !anchor.id) return;
      post({ t: "draft", anchor, quote: anchor.exact });
      return;
    }
    const sel = getSelection();
    if (sel && !sel.isCollapsed) return;
    const hit = hits.find((h) => h.el ? h.el.contains(e.target) : Array.from(h.range.getClientRects()).some((r) => e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom));
    if (hit) post({ t: "focus", id: hit.id });
  }, true);

  window.addEventListener("message", (e) => {
    const d = e.data || {};
    if ("zeronComments" in d) { items = (d.zeronComments && d.zeronComments.items) || []; draw(); }
    else if ("zeronCommentMode" in d) { mode = !!d.zeronCommentMode; if (!mode) unhot(); document.documentElement.style.cursor = mode ? "crosshair" : ""; }
    else if ("zeronCommentFocus" in d) focus(String(d.zeronCommentFocus));
  });
  window.addEventListener("load", () => setTimeout(() => post({ t: "ready" }), 60));
  if (document.readyState === "complete") setTimeout(() => post({ t: "ready" }), 60);
})();
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use zeron_proto::CommentAnchor;

    fn comment(id: &str, exact: &str, status: CommentStatus) -> ArtifactComment {
        ArtifactComment {
            id: id.into(),
            anchor: CommentAnchor {
                exact: exact.into(),
                ..Default::default()
            },
            text: "why".into(),
            status,
            ..Default::default()
        }
    }

    #[test]
    fn rows_mark_unfindable_open_comments_outdated() {
        let comments = vec![
            comment("C-1", "Receiving", CommentStatus::Open),
            comment("C-2", "Gone words", CommentStatus::Open),
            comment("C-3", "Gone words", CommentStatus::Resolved),
        ];
        let rows = rows(ArtifactKind::HtmlFragment, Some("<h2>Receiving</h2>"), &comments);
        let states: Vec<_> = rows.iter().map(|r| r.state.clone()).collect();
        assert_eq!(
            states,
            vec![CommentState::Open, CommentState::Outdated, CommentState::Resolved]
        );
        assert_eq!(open_count(&rows), 2);
        // Still loading: nothing is called outdated yet.
        let loading = super::rows(ArtifactKind::HtmlFragment, None, &comments);
        assert_eq!(loading[1].state, CommentState::Open);
    }

    #[test]
    fn markers_cover_only_open_found_comments() {
        let comments = vec![
            comment("C-1", "Receiving", CommentStatus::Open),
            comment("C-2", "Gone words", CommentStatus::Open),
            comment("C-3", "Receiving", CommentStatus::Resolved),
        ];
        let rows = rows(ArtifactKind::HtmlFragment, Some("<h2>Receiving</h2>"), &comments);
        let script = markers_script(&rows);
        assert!(script.contains("\"C-1\""));
        assert!(!script.contains("\"C-2\""));
        assert!(!script.contains("\"C-3\""));
        assert!(script.starts_with("window.__zeronFrameCmd&&window.__zeronFrameCmd({\"zeronComments\""));
        assert!(mode_script(true).contains("\"zeronCommentMode\":true"));
        assert!(focus_script("C-1").contains("\"zeronCommentFocus\":\"C-1\""));
    }

    #[test]
    fn frame_script_is_self_contained_and_guarded() {
        assert!(FRAME_SCRIPT.contains("__zeronCommentsInstalled"));
        assert!(!FRAME_SCRIPT.contains("</script"), "would end the inline script");
        // Messages out match `bridge::FrameMessage`.
        for kind in ["ready", "selection", "selection-clear", "draft", "focus"] {
            assert!(FRAME_SCRIPT.contains(&format!("t: \"{kind}\"")), "{kind}");
        }
        // No network or navigation from the page.
        for banned in ["fetch(", "XMLHttpRequest", "location.href", "window.open"] {
            assert!(!FRAME_SCRIPT.contains(banned), "{banned}");
        }
    }
}
