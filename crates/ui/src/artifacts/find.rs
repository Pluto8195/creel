//! Find in an open artifact (⌘F): the match model both renderers share, and
//! the small scripts that drive HTML artifacts.
//!
//! HTML artifacts render in a `sandbox="allow-scripts"` frame the host cannot
//! reach into, so the host talks to it through the outer document:
//!
//! ```text
//! Rust ─evaluateJavaScript→ outer doc (HOST_SCRIPT) ─postMessage→ frame (FRAME_SCRIPT)
//! Rust ←evaluateJavaScript(read state)─ outer doc ←postMessage─ frame
//! ```
//!
//! The frame finds text with the CSS Custom Highlight API (no DOM changes),
//! counts matches, scrolls, and reports the section in view. Everything with
//! logic (match counting for Markdown, next/previous, wraparound, labels,
//! script generation) stays here in Rust so it is testable without WebKit.
use std::ops::Range;

/// Where find stands. The renderer reports the match `count`; `current` is
/// the index (0-based) of the highlighted match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FindState {
    pub(crate) open: bool,
    pub(crate) query: String,
    count: usize,
    current: Option<usize>,
}

impl FindState {
    pub(crate) fn count(&self) -> usize {
        self.count
    }

    pub(crate) fn current(&self) -> Option<usize> {
        self.current
    }

    /// An empty query has nothing to find.
    pub(crate) fn is_searching(&self) -> bool {
        !self.query.is_empty()
    }

    /// A new query: matches are unknown until the renderer reports them.
    pub(crate) fn set_query(&mut self, query: &str) {
        self.query = query.to_owned();
        self.count = 0;
        self.current = None;
    }

    /// The renderer's match count for the current query. The first match
    /// becomes current; a recount keeps the current match when it still
    /// exists.
    pub(crate) fn set_count(&mut self, count: usize) {
        self.count = count;
        self.current = match (self.current, count) {
            (_, 0) => None,
            (Some(current), count) if current < count => Some(current),
            _ => Some(0),
        };
    }

    /// Move to the next (or previous) match, wrapping at both ends. `None`
    /// when there is nothing to move to.
    pub(crate) fn step(&mut self, forward: bool) -> Option<usize> {
        if self.count == 0 {
            return None;
        }
        let next = match (self.current, forward) {
            (None, true) => 0,
            (None, false) => self.count - 1,
            (Some(current), true) => (current + 1) % self.count,
            (Some(current), false) => (current + self.count - 1) % self.count,
        };
        self.current = Some(next);
        Some(next)
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.count = 0;
        self.current = None;
    }

    /// The text beside the find field: "3 of 12", "No matches", or nothing
    /// for an empty query.
    pub(crate) fn label(&self) -> String {
        match (self.is_searching(), self.current) {
            (false, _) => String::new(),
            (true, Some(current)) => format!("{} of {}", current + 1, self.count),
            (true, None) => "No matches".to_owned(),
        }
    }
}

/// Fold a char for case-insensitive comparison (its first lowercase char).
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Byte ranges of every case-insensitive, non-overlapping occurrence of
/// `query` in `text`. An empty query matches nothing.
pub(crate) fn find_ranges(text: &str, query: &str) -> Vec<Range<usize>> {
    let needle: Vec<char> = query.chars().map(fold).collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let hay: Vec<(char, usize, usize)> = text
        .char_indices()
        .map(|(start, c)| (fold(c), start, start + c.len_utf8()))
        .collect();
    let mut out = Vec::new();
    let mut at = 0;
    while at + needle.len() <= hay.len() {
        if hay[at..at + needle.len()]
            .iter()
            .map(|(c, _, _)| c)
            .eq(needle.iter())
        {
            out.push(hay[at].1..hay[at + needle.len() - 1].2);
            at += needle.len();
        } else {
            at += 1;
        }
    }
    out
}

/// How many matches `query` has across `texts` (one entry per rendered text
/// element, so a match never spans two).
pub(crate) fn count_matches<'a>(texts: impl IntoIterator<Item = &'a str>, query: &str) -> usize {
    texts
        .into_iter()
        .map(|text| find_ranges(text, query).len())
        .sum()
}

/// A command for the frame.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FrameCommand<'a> {
    /// Highlight every match of the query and report the count.
    Find(&'a str),
    /// Mark match `n` current and scroll it into view.
    Goto(usize),
    /// Remove every highlight.
    Clear,
    /// Scroll to the `n`th `h1`–`h3` (document order, counting empty ones).
    Scroll(usize),
}

/// The script that sends `command` to the frame through the outer document.
pub(crate) fn command_script(command: &FrameCommand) -> String {
    let message = match command {
        FrameCommand::Find(query) => serde_json::json!({ "zeronFind": query }),
        FrameCommand::Goto(n) => serde_json::json!({ "zeronGoto": n }),
        FrameCommand::Clear => serde_json::json!({ "zeronClear": true }),
        FrameCommand::Scroll(n) => serde_json::json!({ "zeronScroll": n }),
    };
    // JSON is valid JS except U+2028 / U+2029 in old engines; escape them.
    let json = message
        .to_string()
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    format!("window.__zeronFrameCmd&&window.__zeronFrameCmd({json});")
}

/// The script that reads the frame's last report as a JSON string.
pub(crate) const READ_STATE_SCRIPT: &str = "JSON.stringify(window.__zeronFrame||null)";

/// What the frame last reported (see [`READ_STATE_SCRIPT`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct FrameState {
    /// The query the count belongs to (a stale reply is ignored).
    #[serde(default)]
    pub(crate) q: Option<String>,
    /// Match count for `q`; `-1` when the page cannot highlight.
    #[serde(default)]
    pub(crate) count: Option<i64>,
    /// Index (document order among `h1`–`h3`) of the section in view.
    #[serde(default)]
    pub(crate) active: Option<usize>,
    /// The page's live headings, `(level, text)`.
    #[serde(default)]
    pub(crate) heads: Vec<(u8, String)>,
}

impl FrameState {
    pub(crate) fn parse(json: &str) -> Option<FrameState> {
        serde_json::from_str::<Option<FrameState>>(json).ok().flatten()
    }

    /// The match count when the report answers `query`.
    pub(crate) fn count_for(&self, query: &str) -> Option<usize> {
        match (&self.q, self.count) {
            (Some(q), Some(count)) if q == query && count >= 0 => Some(count as usize),
            _ => None,
        }
    }
}

/// In the frame: answers the host's messages. Search uses the CSS Custom
/// Highlight API over text nodes (a match inside one text node; no DOM
/// changes), `::highlight` styles come from a constructed stylesheet (CSP
/// does not block those).
pub(crate) const FRAME_SCRIPT: &str = r##"(() => {
  if (window.__zeronFindInstalled) return;
  window.__zeronFindInstalled = true;
  const H = window.CSS && CSS.highlights;
  let ranges = [];
  const sheet = new CSSStyleSheet();
  sheet.replaceSync("::highlight(zeron-find){background:#ffe58a;color:#000}::highlight(zeron-find-cur){background:#ff9632;color:#000}");
  document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
  const heads = () => Array.from(document.querySelectorAll("h1,h2,h3"));
  const send = (o) => { try { parent.postMessage({ zeronReport: o }, "*"); } catch (_) {} };
  const clear = () => { ranges = []; if (H) { H.delete("zeron-find"); H.delete("zeron-find-cur"); } };
  const find = (q) => {
    clear();
    if (!H || !q) return send({ q, count: H ? 0 : -1 });
    const needle = q.toLowerCase();
    const walker = document.createTreeWalker(document.body || document.documentElement, NodeFilter.SHOW_TEXT);
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      if (n.parentElement && n.parentElement.closest("script,style,noscript")) continue;
      const text = n.data.toLowerCase();
      for (let i = text.indexOf(needle); i >= 0; i = text.indexOf(needle, i + needle.length)) {
        const r = new Range(); r.setStart(n, i); r.setEnd(n, i + needle.length); ranges.push(r);
      }
    }
    H.set("zeron-find", new Highlight(...ranges));
    send({ q, count: ranges.length });
  };
  const goto = (i) => {
    const r = ranges[i]; if (!r || !H) return;
    H.set("zeron-find-cur", new Highlight(r));
    const el = r.startContainer.parentElement; if (el) el.scrollIntoView({ block: "center" });
  };
  const report = () => {
    const hs = heads(); let active = null;
    hs.forEach((h, i) => { if (h.getBoundingClientRect().top <= 24) active = i; });
    send({ active, heads: hs.map((h) => [Number(h.tagName[1]), (h.textContent || "").trim().slice(0, 160)]) });
  };
  window.addEventListener("message", (e) => {
    const d = e.data || {};
    if ("zeronFind" in d) find(String(d.zeronFind));
    else if ("zeronGoto" in d) goto(d.zeronGoto);
    else if ("zeronClear" in d) clear();
    else if ("zeronScroll" in d) { const h = heads()[d.zeronScroll]; if (h) h.scrollIntoView({ block: "start" }); }
  });
  let t = 0;
  window.addEventListener("scroll", () => { clearTimeout(t); t = setTimeout(report, 80); }, { passive: true, capture: true });
  window.addEventListener("load", () => setTimeout(report, 50));
})();
"##;

/// In the outer document: forwards host commands to the frame and keeps the
/// frame's reports where the host can read them (`window.__zeronFrame`).
pub(crate) const HOST_SCRIPT: &str = r##"window.__zeronFrame = {};
window.__zeronFrameCmd = (o) => {
  const f = document.querySelector("iframe");
  if (f && f.contentWindow) f.contentWindow.postMessage(o, "*");
};
window.addEventListener("message", (e) => {
  const f = document.querySelector("iframe");
  if (!f || e.source !== f.contentWindow) return;
  const r = e.data && e.data.zeronReport;
  if (r && typeof r === "object") window.__zeronFrame = Object.assign({}, window.__zeronFrame, r);
});"##;

#[cfg(test)]
mod tests {
    use super::*;

    fn state(count: usize) -> FindState {
        let mut state = FindState::default();
        state.set_query("plan");
        state.set_count(count);
        state
    }

    #[test]
    fn ranges_are_case_insensitive_and_byte_accurate() {
        assert_eq!(find_ranges("Plan plan PLAN", "plan"), vec![0..4, 5..9, 10..14]);
        // Multi-byte text before the match keeps byte offsets.
        let text = "café Plan";
        let ranges = find_ranges(text, "PLAN");
        assert_eq!(ranges.len(), 1);
        assert_eq!(&text[ranges[0].clone()], "Plan");
        // The accented letter folds too.
        assert_eq!(find_ranges("CAFÉ", "café"), vec![0..5]);
    }

    #[test]
    fn matches_do_not_overlap() {
        assert_eq!(find_ranges("aaaa", "aa"), vec![0..2, 2..4]);
        assert_eq!(find_ranges("abab", "ab"), vec![0..2, 2..4]);
    }

    #[test]
    fn an_empty_query_matches_nothing() {
        assert!(find_ranges("anything", "").is_empty());
        assert_eq!(count_matches(["a b", "c"], ""), 0);
        let mut state = FindState::default();
        state.set_query("");
        assert_eq!(state.label(), "");
        assert!(!state.is_searching());
        assert_eq!(state.step(true), None);
    }

    #[test]
    fn counts_sum_over_elements_and_never_span_two() {
        assert_eq!(count_matches(["ab", "ab ab"], "ab"), 3);
        assert_eq!(count_matches(["a", "b"], "ab"), 0);
    }

    #[test]
    fn the_first_match_is_current_and_the_label_counts() {
        let state = state(12);
        assert_eq!(state.current(), Some(0));
        assert_eq!(state.label(), "1 of 12");
    }

    #[test]
    fn next_and_previous_wrap_around() {
        let mut state = state(3);
        assert_eq!(state.step(true), Some(1));
        assert_eq!(state.step(true), Some(2));
        assert_eq!(state.step(true), Some(0));
        assert_eq!(state.step(false), Some(2));
        assert_eq!(state.step(false), Some(1));
        assert_eq!(state.label(), "2 of 3");
    }

    #[test]
    fn previous_before_any_current_starts_at_the_last_match() {
        let mut state = FindState::default();
        state.set_query("x");
        state.count = 4;
        assert_eq!(state.step(false), Some(3));
    }

    #[test]
    fn no_matches_says_so_and_cannot_step() {
        let mut state = state(0);
        assert_eq!(state.label(), "No matches");
        assert_eq!(state.step(true), None);
        assert_eq!(state.step(false), None);
        assert_eq!(state.current(), None);
    }

    #[test]
    fn a_recount_keeps_the_current_match_when_it_still_exists() {
        let mut state = state(5);
        state.step(true);
        state.step(true);
        state.set_count(8);
        assert_eq!(state.current(), Some(2));
        state.set_count(2);
        assert_eq!(state.current(), Some(0));
        state.set_count(0);
        assert_eq!(state.current(), None);
    }

    #[test]
    fn a_new_query_forgets_the_old_matches() {
        let mut state = state(5);
        state.set_query("other");
        assert_eq!((state.count(), state.current()), (0, None));
        assert_eq!(state.label(), "No matches");
    }

    #[test]
    fn close_resets_everything() {
        let mut state = state(5);
        state.open = true;
        state.close();
        assert_eq!(state, FindState::default());
    }

    #[test]
    fn command_scripts_escape_the_query() {
        let script = command_script(&FrameCommand::Find("a\"b</script>\u{2028}\\"));
        assert_eq!(
            script,
            "window.__zeronFrameCmd&&window.__zeronFrameCmd({\"zeronFind\":\"a\\\"b</script>\\u2028\\\\\"});"
        );
        // One statement: no raw newline or line separator can end it early.
        assert!(!script.contains('\n') && !script.contains('\u{2028}'));
    }

    #[test]
    fn command_scripts_carry_indexes() {
        assert_eq!(
            command_script(&FrameCommand::Goto(7)),
            "window.__zeronFrameCmd&&window.__zeronFrameCmd({\"zeronGoto\":7});"
        );
        assert_eq!(
            command_script(&FrameCommand::Scroll(2)),
            "window.__zeronFrameCmd&&window.__zeronFrameCmd({\"zeronScroll\":2});"
        );
        assert_eq!(
            command_script(&FrameCommand::Clear),
            "window.__zeronFrameCmd&&window.__zeronFrameCmd({\"zeronClear\":true});"
        );
    }

    #[test]
    fn frame_state_parses_and_ignores_stale_counts() {
        let state = FrameState::parse(
            r#"{"q":"plan","count":4,"active":2,"heads":[[1,"Plan"],[2,"Steps"]]}"#,
        )
        .unwrap();
        assert_eq!(state.count_for("plan"), Some(4));
        assert_eq!(state.count_for("other"), None);
        assert_eq!(state.active, Some(2));
        assert_eq!(state.heads, vec![(1, "Plan".to_owned()), (2, "Steps".to_owned())]);
        // The page cannot highlight: no count.
        let unsupported = FrameState::parse(r#"{"q":"x","count":-1}"#).unwrap();
        assert_eq!(unsupported.count_for("x"), None);
        // Nothing reported yet.
        assert_eq!(FrameState::parse("null"), None);
        assert_eq!(FrameState::parse("not json"), None);
        assert_eq!(FrameState::parse("{}").unwrap(), FrameState::default());
    }

    #[test]
    fn injected_scripts_stay_small_and_cannot_close_their_tag() {
        for script in [FRAME_SCRIPT, HOST_SCRIPT] {
            assert!(!script.contains("</script"));
        }
        assert!(FRAME_SCRIPT.lines().count() < 60, "keep the injected JS small");
        assert!(HOST_SCRIPT.lines().count() < 15);
    }
}
