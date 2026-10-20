//! Plan questions in HTML artifacts: the `data-zeron-question` form from
//! `docs/planning-artifact-conventions.md` §4.
//!
//! ```html
//! <div data-zeron-question="Q-3" data-status="open" data-blocking="true"
//!      data-example="…" data-blocks="…" data-decision="…">Question text</div>
//! ```
//!
//! The card is built inside the sandboxed frame: [`CSS`] styles it and
//! [`SCRIPT`] adds the header (id badge, status pill, Blocking flag) and
//! the labelled rows from the `data-*` attributes, matching the Markdown
//! card. The question's own content stays inside the element, so inline
//! markup in it survives. Values are set as text, never as markup. Resolved
//! cards collapse to the question and the decision, with a toggle.
//!
//! Only the documented attributes become rows (`data-example`,
//! `data-blocks`, `data-decision`); other `data-*` attributes are an
//! author's own and are left alone.

/// The frame stylesheet. Colors come from the visualize theme variables
/// (`--orange`, `--primary`, `--green`, …) with fallbacks for pages that
/// don't define them.
pub(crate) const CSS: &str = r##"
[data-zeron-question]{display:block;box-sizing:border-box;margin:12px 0;padding:10px 12px 10px 14px;border:1px solid var(--border,rgba(128,128,128,.28));border-left:3px solid var(--orange,#d97706);border-radius:6px;background:color-mix(in srgb,var(--foreground,#222) 4%,transparent);color:var(--muted-foreground,inherit);line-height:1.5}
[data-zeron-question][data-status="answered"]{border-left-color:var(--primary,#4f46e5)}
[data-zeron-question][data-status="resolved"]{border-left-color:var(--muted-foreground,#888)}
[data-zeron-question][data-zq-collapsed]{opacity:.75}
[data-zeron-question] .zq-head{display:flex;flex-wrap:wrap;align-items:center;gap:6px;margin-bottom:6px}
[data-zeron-question] .zq-id{padding:0 7px;border-radius:5px;background:color-mix(in srgb,var(--foreground,#222) 10%,transparent);color:var(--foreground,inherit);font-size:11px;font-weight:600;line-height:18px}
[data-zeron-question] .zq-pill{padding:0 7px;border:1px solid currentColor;border-radius:9px;font-size:11px;font-weight:500;line-height:16px}
[data-zeron-question] .zq-pill[data-zq-status="open"]{color:var(--orange,#d97706);background:color-mix(in srgb,var(--orange,#d97706) 14%,transparent)}
[data-zeron-question] .zq-pill[data-zq-status="answered"]{color:var(--primary,#4f46e5);background:color-mix(in srgb,var(--primary,#4f46e5) 14%,transparent)}
[data-zeron-question] .zq-pill[data-zq-status="resolved"]{color:var(--green,#16a34a);background:color-mix(in srgb,var(--green,#16a34a) 14%,transparent)}
[data-zeron-question] .zq-flag{padding:0 7px;border:1px solid var(--red,#dc2626);border-radius:9px;color:var(--red,#dc2626);font-size:11px;font-weight:500;line-height:16px}
[data-zeron-question] .zq-spacer{flex:1}
[data-zeron-question] .zq-toggle{padding:0 6px;border:0;border-radius:5px;background:none;color:inherit;font:inherit;font-size:11px;line-height:18px;cursor:pointer}
[data-zeron-question] .zq-toggle:hover{background:color-mix(in srgb,var(--foreground,#222) 8%,transparent)}
[data-zeron-question] .zq-text{color:var(--foreground,inherit);font-weight:600}
[data-zeron-question] .zq-row{display:flex;align-items:flex-start;gap:10px;margin-top:4px}
[data-zeron-question] .zq-label{flex:none;width:68px;font-size:11px;font-weight:600;line-height:22px}
[data-zeron-question] .zq-value{flex:1;min-width:0}
[data-zeron-question] .zq-row[data-zq-key="decision"]{padding:3px 8px;border-left:2px solid var(--green,#16a34a);border-radius:0 5px 5px 0;background:color-mix(in srgb,var(--green,#16a34a) 8%,transparent)}
[data-zeron-question][data-zq-collapsed] .zq-row:not([data-zq-key="decision"]){display:none}
html[data-zq-finding] [data-zeron-question] .zq-row:not([data-zq-key="decision"]){display:flex}
"##;

/// The frame script: builds each card once the document has parsed.
pub(crate) const SCRIPT: &str = r##"(() => {
  if (window.__zeronQuestionsInstalled) return;
  window.__zeronQuestionsInstalled = true;
  const STATUSES = ["open", "answered", "resolved"];
  const LABELS = { open: "Open", answered: "Answered", resolved: "Resolved" };
  const ROWS = [["example", "Example"], ["blocks", "Blocks"], ["decision", "Decision"]];
  const make = (tag, cls, text) => {
    const node = document.createElement(tag);
    if (cls) node.className = cls;
    if (text != null) node.textContent = text;
    return node;
  };
  const build = (card) => {
    if (card.hasAttribute("data-zq-ready")) return;
    card.setAttribute("data-zq-ready", "");
    const raw = (card.getAttribute("data-status") || "open").trim().toLowerCase();
    const status = STATUSES.includes(raw) ? raw : "open";
    const blocking = (card.getAttribute("data-blocking") || "").trim().toLowerCase() === "true";
    const id = (card.getAttribute("data-zeron-question") || "").trim();
    const question = make("div", "zq-text");
    while (card.firstChild) question.appendChild(card.firstChild);
    const head = make("div", "zq-head");
    if (id) head.appendChild(make("span", "zq-id", id));
    const pill = make("span", "zq-pill", LABELS[status]);
    pill.setAttribute("data-zq-status", status);
    head.appendChild(pill);
    if (blocking) head.appendChild(make("span", "zq-flag", "Blocking"));
    card.appendChild(head);
    if (question.textContent.trim() || question.children.length) card.appendChild(question);
    for (const [key, label] of ROWS) {
      const value = (card.getAttribute("data-" + key) || "").trim();
      if (!value) continue;
      const row = make("div", "zq-row");
      row.setAttribute("data-zq-key", key);
      row.appendChild(make("div", "zq-label", label));
      row.appendChild(make("div", "zq-value", value));
      card.appendChild(row);
    }
    if (status === "resolved") {
      card.setAttribute("data-zq-collapsed", "");
      const toggle = make("button", "zq-toggle", "Show details");
      toggle.type = "button";
      toggle.addEventListener("click", () => {
        const collapsed = card.toggleAttribute("data-zq-collapsed");
        toggle.textContent = collapsed ? "Show details" : "Hide details";
      });
      head.appendChild(make("span", "zq-spacer"));
      head.appendChild(toggle);
    }
  };
  const run = () => document.querySelectorAll("[data-zeron-question]").forEach(build);
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", run);
  else run();
  // While find is active, resolved cards show everything they match.
  window.addEventListener("message", (event) => {
    const data = event.data || {};
    if ("zeronFind" in data) document.documentElement.toggleAttribute("data-zq-finding", String(data.zeronFind) !== "");
    else if ("zeronClear" in data) document.documentElement.removeAttribute("data-zq-finding");
  });
})();
"##;

/// `<style>` and `<script>` for a frame that may hold questions.
pub(crate) fn head_markup() -> String {
    format!("<style>{CSS}</style>\n<script>{SCRIPT}</script>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_styles_every_status_and_the_decision_row() {
        for status in ["answered", "resolved"] {
            assert!(CSS.contains(&format!("[data-zeron-question][data-status=\"{status}\"]")));
        }
        for status in ["open", "answered", "resolved"] {
            assert!(CSS.contains(&format!(".zq-pill[data-zq-status=\"{status}\"]")));
        }
        assert!(CSS.contains(".zq-row[data-zq-key=\"decision\"]"));
        assert!(
            CSS.contains(
                "[data-zq-collapsed] .zq-row:not([data-zq-key=\"decision\"]){display:none}"
            )
        );
        assert!(CSS.contains("html[data-zq-finding]"));
    }

    #[test]
    fn script_builds_the_card_from_the_documented_attributes_as_text() {
        for attribute in [
            "data-zeron-question",
            "data-status",
            "data-blocking",
            "data-example",
            "data-blocks",
            "data-decision",
        ] {
            let name = attribute.trim_start_matches("data-");
            assert!(
                SCRIPT.contains(&format!("\"{attribute}\""))
                    || SCRIPT.contains(&format!("\"{name}\"")),
                "{attribute}"
            );
        }
        assert!(SCRIPT.contains("\"Blocking\""));
        assert!(SCRIPT.contains("zq-toggle"));
        // Attribute values are text, never markup.
        assert!(SCRIPT.contains("textContent"));
        assert!(!SCRIPT.contains("innerHTML"));
        assert!(!SCRIPT.contains("insertAdjacentHTML"));
    }

    #[test]
    fn head_markup_wraps_both_parts() {
        let markup = head_markup();
        assert!(markup.starts_with("<style>"));
        assert!(markup.contains("</style>\n<script>"));
        assert!(markup.ends_with("</script>"));
        assert!(!SCRIPT.contains("</script"), "would end the script early");
    }

    fn theme() -> crate::browser::fragment::FragmentTheme {
        crate::browser::fragment::FragmentTheme {
            dark: false,
            vars: Vec::new(),
            font_sans: "sans-serif".into(),
            font_mono: "monospace".into(),
        }
    }

    const FRAGMENT: &str = "<h2>Open questions</h2>\n\
<div data-zeron-question=\"Q-3\" data-status=\"open\" data-blocking=\"true\" \
data-example=\"two LM358 lines\" data-blocks=\"ENG-2690\">Merge by <b>MPN</b>?</div>\n\
<div data-zeron-question=\"Q-4\" data-status=\"resolved\" data-decision=\"by MPN\">Done?</div>";

    #[test]
    fn web_documents_carry_the_card_style_and_script_around_the_authors_markup() {
        use crate::artifacts::document::web_document;
        use crate::artifacts::model::WebKind;
        use crate::browser::fragment::escape_attr;
        let page = format!(
            "<!doctype html><html><head><title>p</title></head><body>{FRAGMENT}</body></html>"
        );
        for (kind, source) in [
            (WebKind::Fragment, FRAGMENT.to_owned()),
            (WebKind::Page, page),
        ] {
            for probe in [false, true] {
                let html = web_document(kind, &source, "t", "", &theme(), false, probe);
                // The frame is the escaped srcdoc attribute.
                assert!(html.contains(&escape_attr(CSS.trim())), "{kind:?} style");
                assert!(
                    html.contains("__zeronQuestionsInstalled"),
                    "{kind:?} script"
                );
                assert!(
                    html.contains(&escape_attr("<div data-zeron-question=\"Q-3\"")),
                    "{kind:?} author markup kept"
                );
                assert!(html.contains(&escape_attr("data-decision=\"by MPN\"")));
                assert_eq!(html.matches("<iframe").count(), 1);
            }
        }
        // A page without <head> still gets the style, ahead of its content.
        let bare = web_document(WebKind::Page, FRAGMENT, "t", "", &theme(), false, false);
        let style_at = bare
            .find(&escape_attr("<style>\n[data-zeron-question]"))
            .unwrap();
        let card_at = bare.find(&escape_attr("<div data-zeron-question")).unwrap();
        assert!(style_at < card_at);
    }

    #[test]
    fn compare_frames_render_cards_the_same_way() {
        use crate::artifacts::document::compare_frame;
        use crate::artifacts::model::WebKind;
        for kind in [WebKind::Fragment, WebKind::Page] {
            let frame = compare_frame(kind, FRAGMENT, "t", "", &theme(), false, "", "");
            assert!(
                frame.html.contains("<style>\n[data-zeron-question]"),
                "{kind:?}"
            );
            assert!(frame.html.contains("__zeronQuestionsInstalled"), "{kind:?}");
            assert!(
                frame.html.contains("data-zeron-question=\"Q-3\""),
                "{kind:?}"
            );
        }
        // Mermaid and image kinds carry no question style.
        let frame = compare_frame(
            WebKind::Mermaid,
            "graph TD; A-->B",
            "t",
            "",
            &theme(),
            false,
            "",
            "",
        );
        assert!(!frame.html.contains("<style>\n[data-zeron-question]"));
    }
}
