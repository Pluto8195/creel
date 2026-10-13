//! Codex `visualize` content references and HTML fragment naming.
//!
//! Codex has no app-server item for inline visualizations. The agent writes
//! an HTML fragment file and puts a content reference into its final answer
//! (docs/research/codex-visualize.md). On the wire the reference is framed by
//! private-use tokens, `U+E200 visualize U+E202 {json} U+E201`; after
//! detokenization it is a whole `visualize{json}` line. Both forms stay in the
//! persisted assistant text, so every client and existing history carries
//! them. This module is the single parser for them and for the
//! `<name>.fragment.html` / `<name>.preview.html` naming convention.

use serde::Deserialize;

/// Envelope prefix exactly as Codex's own TUI matches it.
pub const ENVELOPE_PREFIX: &str = "\u{e200}visualize\u{e202}";
pub const ENVELOPE_SUFFIX: &str = "\u{e201}";
/// The detokenized whole-line form documented by the visualize skill.
pub const LINE_PREFIX: &str = "visualize{";
pub const MAX_PAYLOAD_BYTES: usize = 8 * 1024;
pub const MAX_PATH_BYTES: usize = 4 * 1024;
pub const MAX_TITLE_CHARS: usize = 200;
/// The skill's size limit (`render.py` `_MAX_FRAGMENT_BYTES`).
pub const MAX_FRAGMENT_BYTES: usize = 1_000_000;

pub const FRAGMENT_SUFFIX: &str = ".fragment.html";
pub const PREVIEW_SUFFIX: &str = ".preview.html";

/// One validated content reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisualizationRef {
    /// Absolute executor-side path of the fragment.
    pub path: String,
    pub title: String,
    /// `"mode":"wide"`: designed for 1,024px instead of 736px.
    pub wide: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    path: String,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

/// Parse and whitelist a reference payload. Anything unexpected is rejected:
/// the payload is model output and the path later drives file access.
pub fn parse_payload(payload: &str) -> Option<VisualizationRef> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return None;
    }
    let payload: Payload = serde_json::from_str(payload).ok()?;
    if payload.path.is_empty()
        || payload.path.len() > MAX_PATH_BYTES
        || !std::path::Path::new(&payload.path).has_root()
        || payload.path.chars().any(char::is_control)
        || !payload.path.to_ascii_lowercase().ends_with(".html")
        || payload.mode.as_deref().is_some_and(|mode| mode != "wide")
    {
        return None;
    }
    let title = payload
        .title
        .or_else(|| default_title(&payload.path))
        .unwrap_or_else(|| "Visualization".to_owned());
    let title = title.trim();
    if title.is_empty()
        || title.chars().count() > MAX_TITLE_CHARS
        || title.chars().any(char::is_control)
    {
        return None;
    }
    Some(VisualizationRef {
        path: payload.path,
        title: title.to_owned(),
        wide: payload.mode.as_deref() == Some("wide"),
    })
}

/// File-name title, without the `.fragment.html`/`.html` suffix.
pub fn default_title(path: &str) -> Option<String> {
    let name = path.rsplit(['/', '\\']).next()?;
    let lower = name.to_ascii_lowercase();
    let stem_len = [FRAGMENT_SUFFIX, PREVIEW_SUFFIX, ".html"]
        .iter()
        .find(|suffix| lower.ends_with(*suffix))
        .map_or(name.len(), |suffix| name.len() - suffix.len());
    let stem = name[..stem_len].trim();
    (!stem.is_empty()).then(|| stem.to_owned())
}

/// A piece of assistant text: prose, or a reference (`None` = malformed or
/// still streaming envelope; it must never render as raw control bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment<'a> {
    Text(&'a str),
    Reference(Option<VisualizationRef>),
}

/// Longest proper prefix of [`ENVELOPE_PREFIX`] that `text` ends with.
pub fn trailing_envelope_prefix_len(text: &str) -> usize {
    ENVELOPE_PREFIX
        .char_indices()
        .skip(1)
        .map(|(boundary, _)| boundary)
        .filter(|&boundary| text.ends_with(&ENVELOPE_PREFIX[..boundary]))
        .max()
        .unwrap_or(0)
}

/// Split assistant text at visualization references. `streaming` hides an
/// unfinished trailing reference instead of flashing it as prose.
pub fn split_references(text: &str, streaming: bool) -> Vec<Segment<'_>> {
    let mut segments = Vec::new();
    let mut plain_start = 0;
    let mut search_start = 0;
    let mut suppressed_tail = false;
    while let Some(relative) = text[search_start..].find(ENVELOPE_PREFIX) {
        let start = search_start + relative;
        split_lines(&text[plain_start..start], false, &mut segments);
        let payload_start = start + ENVELOPE_PREFIX.len();
        let Some(relative_end) = text[payload_start..].find(ENVELOPE_SUFFIX) else {
            // Streaming may stop between the envelope and its terminator.
            segments.push(Segment::Reference(None));
            plain_start = text.len();
            suppressed_tail = true;
            break;
        };
        let end = payload_start + relative_end;
        segments.push(Segment::Reference(parse_payload(&text[payload_start..end])));
        plain_start = end + ENVELOPE_SUFFIX.len();
        search_start = plain_start;
    }
    let partial = if streaming && !suppressed_tail {
        trailing_envelope_prefix_len(&text[plain_start..])
    } else {
        0
    };
    let plain_end = text.len() - partial;
    if plain_start < plain_end {
        split_lines(&text[plain_start..plain_end], streaming, &mut segments);
    }
    if segments.is_empty() && partial == 0 {
        segments.push(Segment::Text(text));
    }
    segments
}

fn is_fence(line: &str) -> bool {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let line = line.trim_start_matches(' ');
    indent < 4 && (line.starts_with("```") || line.starts_with("~~~"))
}

/// Whole-line `visualize{…}` references outside fenced code. Unlike the
/// envelope, an invalid ASCII line stays prose: it may be quoted text.
fn split_lines<'a>(text: &'a str, streaming: bool, out: &mut Vec<Segment<'a>>) {
    let mut plain_start = 0;
    let mut offset = 0;
    let mut in_fence = false;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let body = line.trim_end_matches(['\n', '\r']);
        if is_fence(body) {
            in_fence = !in_fence;
            continue;
        }
        let trimmed = body.trim();
        if in_fence || !trimmed.starts_with(LINE_PREFIX) {
            continue;
        }
        let unterminated = !line.ends_with('\n') && offset == text.len();
        if streaming && unterminated {
            // The reference is still arriving; keep it out of the prose.
            if start > plain_start {
                out.push(Segment::Text(&text[plain_start..start]));
            }
            plain_start = text.len();
            break;
        }
        if let Some(reference) = parse_payload(&trimmed["visualize".len()..]) {
            if start > plain_start {
                out.push(Segment::Text(&text[plain_start..start]));
            }
            out.push(Segment::Reference(Some(reference)));
            plain_start = offset;
        }
    }
    if plain_start < text.len() {
        out.push(Segment::Text(&text[plain_start..]));
    }
}

/// Every reference in `text`, in order (valid ones only).
pub fn references(text: &str) -> Vec<VisualizationRef> {
    split_references(text, false)
        .into_iter()
        .filter_map(|segment| match segment {
            Segment::Reference(reference) => reference,
            Segment::Text(_) => None,
        })
        .collect()
}

/// The `.fragment.html` / `.preview.html` naming convention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HtmlArtifactKind {
    /// In-chat source: the host wraps and renders it.
    Fragment,
    /// Standalone export for a local browser.
    Preview,
}

fn ends_with_ignore_case(path: &str, suffix: &str) -> bool {
    path.len() >= suffix.len()
        && path.is_char_boundary(path.len() - suffix.len())
        && path[path.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

pub fn html_artifact_kind(path: &str) -> Option<HtmlArtifactKind> {
    if ends_with_ignore_case(path, FRAGMENT_SUFFIX) {
        Some(HtmlArtifactKind::Fragment)
    } else if ends_with_ignore_case(path, PREVIEW_SUFFIX) {
        Some(HtmlArtifactKind::Preview)
    } else {
        None
    }
}

/// `x.fragment.html` (or a plain `x.html` reference) → `x.preview.html`.
pub fn preview_sibling(fragment: &str) -> Option<String> {
    let stem = if ends_with_ignore_case(fragment, FRAGMENT_SUFFIX) {
        &fragment[..fragment.len() - FRAGMENT_SUFFIX.len()]
    } else if ends_with_ignore_case(fragment, PREVIEW_SUFFIX) {
        return None;
    } else if ends_with_ignore_case(fragment, ".html") {
        &fragment[..fragment.len() - ".html".len()]
    } else {
        return None;
    };
    (!stem.is_empty() && !stem.ends_with('/')).then(|| format!("{stem}{PREVIEW_SUFFIX}"))
}

/// `x.preview.html` → `x.fragment.html`.
pub fn fragment_sibling(preview: &str) -> Option<String> {
    ends_with_ignore_case(preview, PREVIEW_SUFFIX)
        .then(|| &preview[..preview.len() - PREVIEW_SUFFIX.len()])
        .filter(|stem| !stem.is_empty() && !stem.ends_with('/'))
        .map(|stem| format!("{stem}{FRAGMENT_SUFFIX}"))
}

fn path_char(c: char) -> bool {
    !(c.is_whitespace() || c.is_control() || "()[]<>`\"'|*,;".contains(c))
}

/// `*.fragment.html` paths mentioned in prose or tool output: Markdown link
/// targets, inline code and bare paths. Order-preserving and de-duplicated;
/// `file://` prefixes and `:line` suffixes are removed.
pub fn fragment_mentions(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(relative) = lower[from..].find(FRAGMENT_SUFFIX) {
        let suffix_start = from + relative;
        let end = suffix_start + FRAGMENT_SUFFIX.len();
        from = end;
        // The suffix must end the token (`x.fragment.html5` is something else).
        if text[end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'))
        {
            continue;
        }
        let start = text[..suffix_start]
            .char_indices()
            .rev()
            .find(|(_, c)| !path_char(*c))
            .map_or(0, |(ix, c)| ix + c.len_utf8());
        let mut token = &text[start..end];
        if let Some(rest) = token.strip_prefix("file://") {
            token = rest;
        }
        if token.len() <= FRAGMENT_SUFFIX.len()
            || token.contains("://")
            || token.len() > MAX_PATH_BYTES
        {
            continue;
        }
        if !found.iter().any(|seen| seen == token) {
            found.push(token.to_owned());
        }
    }
    found
}

/// `render.py` rejects complete documents: a fragment has no doctype,
/// `<html>`, `<head>` or `<body>`.
pub fn is_complete_document(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    if lower
        .match_indices("<!doctype")
        .any(|(ix, m)| lower[ix + m.len()..].starts_with(|c: char| c.is_whitespace()))
    {
        return true;
    }
    lower.match_indices('<').any(|(ix, _)| {
        let rest = lower[ix + 1..].trim_start();
        ["html", "head", "body"].iter().any(|tag| {
            rest.strip_prefix(tag)
                .is_some_and(|after| after.starts_with(|c: char| c.is_whitespace() || c == '>'))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(payload: &str) -> String {
        format!("{ENVELOPE_PREFIX}{payload}{ENVELOPE_SUFFIX}")
    }

    #[test]
    fn envelope_and_line_references_split_out_of_prose() {
        let text = format!(
            "Before\n\n{}\n\nMiddle\nvisualize{{\"path\":\"/w/b.html\",\"mode\":\"wide\"}}\nAfter",
            envelope(r#"{"path":"/w/a.fragment.html","title":"A"}"#)
        );
        let segments = split_references(&text, false);
        let refs = references(&text);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].title, "A");
        assert!(!refs[0].wide);
        assert_eq!(refs[1].path, "/w/b.html");
        assert_eq!(refs[1].title, "b");
        assert!(refs[1].wide);
        let prose: String = segments
            .iter()
            .filter_map(|s| match s {
                Segment::Text(t) => Some(*t),
                _ => None,
            })
            .collect();
        assert_eq!(prose, "Before\n\n\n\nMiddle\nAfter");
    }

    #[test]
    fn line_references_in_code_fences_or_invalid_stay_prose() {
        for text in [
            "```text\nvisualize{\"path\":\"/w/a.html\"}\n```\n",
            "visualize{\"path\":\"relative.html\"}\n",
            "see visualize{\"path\":\"/w/a.html\"} inline\n",
        ] {
            assert_eq!(split_references(text, false), vec![Segment::Text(text)]);
        }
    }

    #[test]
    fn streaming_hides_unfinished_references() {
        assert_eq!(
            split_references("Hi\nvisualize{\"pa", true),
            vec![Segment::Text("Hi\n")]
        );
        let partial = format!("Hi{}", &ENVELOPE_PREFIX[..4]);
        assert_eq!(split_references(&partial, true), vec![Segment::Text("Hi")]);
        let open = format!("Hi {ENVELOPE_PREFIX}{{\"path\"");
        assert_eq!(
            split_references(&open, true),
            vec![Segment::Text("Hi "), Segment::Reference(None)]
        );
    }

    #[test]
    fn payloads_are_whitelisted() {
        for payload in [
            r#"{"path":"relative/chart.html"}"#,
            r#"{"path":"/w/chart.svg"}"#,
            r#"{"path":"/w/chart.html","mode":"normal"}"#,
            r#"{"path":"/w/chart.html","unknown":true}"#,
            "{\"path\":\"/w/chart.html\",\"title\":\"bad\\nline\"}",
        ] {
            assert!(parse_payload(payload).is_none(), "{payload}");
        }
        assert_eq!(
            parse_payload(r#"{"path":"/w/kit.fragment.html"}"#)
                .unwrap()
                .title,
            "kit"
        );
    }

    #[test]
    fn naming_convention_and_siblings() {
        assert_eq!(
            html_artifact_kind("/w/A.Fragment.HTML"),
            Some(HtmlArtifactKind::Fragment)
        );
        assert_eq!(
            html_artifact_kind("w/a.preview.html"),
            Some(HtmlArtifactKind::Preview)
        );
        assert_eq!(html_artifact_kind("w/a.html"), None);
        assert_eq!(
            preview_sibling("w/a.fragment.html").as_deref(),
            Some("w/a.preview.html")
        );
        assert_eq!(
            preview_sibling("w/a.html").as_deref(),
            Some("w/a.preview.html")
        );
        assert_eq!(preview_sibling("w/a.preview.html"), None);
        assert_eq!(
            fragment_sibling("w/a.preview.html").as_deref(),
            Some("w/a.fragment.html")
        );
        assert_eq!(fragment_sibling("w/.preview.html"), None);
    }

    #[test]
    fn fragment_mentions_cover_links_code_and_bare_paths() {
        let text = "Open [mockup](work/mockup.fragment.html), or `work/b.fragment.html`.\n\
                    Bare: /repo/c.fragment.html and file:///repo/d.fragment.html:3 \
                    again work/mockup.fragment.html; not work/e.fragment.html5 or \
                    https://x.test/f.fragment.html";
        assert_eq!(
            fragment_mentions(text),
            vec![
                "work/mockup.fragment.html",
                "work/b.fragment.html",
                "/repo/c.fragment.html",
                "/repo/d.fragment.html",
            ]
        );
    }

    #[test]
    fn complete_documents_are_detected() {
        assert!(is_complete_document("<!DOCTYPE html><p>x</p>"));
        assert!(is_complete_document("<p>x</p><BODY class=a>"));
        assert!(is_complete_document("< html>"));
        assert!(!is_complete_document("<div id=\"hd\"><header>x</header></div>"));
        assert!(!is_complete_document("<p>body text</p><htmlish>"));
    }
}
