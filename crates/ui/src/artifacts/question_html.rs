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
[data-zeron-question] .zq-row[data-zq-key="answer"]{padding:3px 8px;border-left:2px solid var(--primary,#4f46e5);border-radius:0 5px 5px 0;background:color-mix(in srgb,var(--primary,#4f46e5) 8%,transparent)}
[data-zeron-question][data-zq-collapsed] .zq-row:not([data-zq-key="decision"]){display:none}
html[data-zq-finding] [data-zeron-question] .zq-row:not([data-zq-key="decision"]){display:flex}
"##;

/// The frame script: builds each card once the document has parsed.
pub(crate) const SCRIPT: &str = r##"(() => {
  if (window.__zeronQuestionsInstalled) return;
  window.__zeronQuestionsInstalled = true;
  const STATUSES = ["open", "answered", "resolved"];
  const LABELS = { open: "Open", answered: "Answered", resolved: "Resolved" };
  const ROWS = [["example", "Example"], ["blocks", "Blocks"], ["answer", "Your answer"], ["decision", "Decision"]];
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

/// Mark the open `data-zeron-question` cards of `source` that have a
/// recorded answer (`answers`: upper-cased id to text) as answered, with the
/// text in `data-answer` (the frame script shows it as a "Your answer" row).
/// Resolved and already-answered cards are left alone. Pure string work: the
/// answer is attribute-escaped, the rest of the markup is untouched.
pub(crate) fn with_answers(
    source: &str,
    answers: &std::collections::HashMap<String, String>,
) -> String {
    const NEEDLE: &str = "data-zeron-question";
    if answers.is_empty() || !source.contains(NEEDLE) {
        return source.to_owned();
    }
    let mut out = String::with_capacity(source.len() + 64);
    let mut rest = source;
    while let Some(at) = rest.find(NEEDLE) {
        // The tag this attribute sits in: back to its `<`, on to its `>`.
        let tag_start = rest[..at].rfind('<').unwrap_or(0);
        let Some(tag_len) = rest[at..].find('>') else {
            break;
        };
        let tag_end = at + tag_len;
        let tag = &rest[tag_start..tag_end];
        out.push_str(&rest[..tag_start]);
        out.push_str(&answered_tag(tag, answers));
        rest = &rest[tag_end..];
    }
    out.push_str(rest);
    out
}

/// The value of attribute `name` in `tag` and the byte range of
/// `name="value"` (either quote style).
fn attribute<'a>(tag: &'a str, name: &str) -> Option<(&'a str, std::ops::Range<usize>)> {
    let mut from = 0;
    while let Some(found) = tag[from..].find(name) {
        let start = from + found;
        from = start + name.len();
        let before_ok = tag[..start]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace());
        let after = &tag[from..];
        let Some(after) = after.strip_prefix('=') else {
            continue;
        };
        let quote = after.chars().next().filter(|c| matches!(c, '"' | '\''));
        let Some(quote) = quote.filter(|_| before_ok) else {
            continue;
        };
        let value_start = from + 2;
        let len = tag[value_start..].find(quote)?;
        return Some((
            &tag[value_start..value_start + len],
            start..value_start + len + 1,
        ));
    }
    None
}

fn answered_tag(tag: &str, answers: &std::collections::HashMap<String, String>) -> String {
    let Some((id, _)) = attribute(tag, "data-zeron-question") else {
        return tag.to_owned();
    };
    let Some(answer) = answers.get(&id.trim().to_ascii_uppercase()) else {
        return tag.to_owned();
    };
    let escaped = crate::browser::fragment::escape_attr(answer);
    match attribute(tag, "data-status") {
        Some((status, range)) if status.trim().eq_ignore_ascii_case("open") => format!(
            "{}data-status=\"answered\" data-answer=\"{escaped}\"{}",
            &tag[..range.start],
            &tag[range.end..]
        ),
        Some(_) => tag.to_owned(),
        None => format!("{tag} data-status=\"answered\" data-answer=\"{escaped}\""),
    }
}

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
    fn answers_mark_open_cards_answered_and_leave_others_alone() {
        let answers = std::collections::HashMap::from([
            ("Q-3".to_owned(), "by \"MPN\"".to_owned()),
            ("Q-4".to_owned(), "ignored".to_owned()),
            ("Q-5".to_owned(), "no status".to_owned()),
        ]);
        let source = "<p>x</p><div data-zeron-question=\"Q-3\" data-status=\"open\" data-blocking=\"true\">A?</div>\
<div data-zeron-question=\"Q-4\" data-status=\"resolved\">B?</div>\
<div data-zeron-question='Q-5'>C?</div><div data-zeron-question=\"Q-9\">D?</div>";
        let out = with_answers(source, &answers);
        assert!(out.contains(
            "<div data-zeron-question=\"Q-3\" data-status=\"answered\" data-answer=\"by &quot;MPN&quot;\" data-blocking=\"true\">A?</div>"
        ));
        assert!(out.contains("<div data-zeron-question=\"Q-4\" data-status=\"resolved\">B?</div>"));
        assert!(out.contains(
            "data-zeron-question='Q-5' data-status=\"answered\" data-answer=\"no status\">C?"
        ));
        assert!(out.contains("<div data-zeron-question=\"Q-9\">D?</div>"));
        assert!(out.starts_with("<p>x</p>"));
        assert_eq!(with_answers(source, &Default::default()), source);
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
