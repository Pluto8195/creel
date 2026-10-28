//! Comments on HTML and Mermaid artifacts (Artifacts phase 4 G).
//!
//! A comment is stored on the artifact row (like phase 3's answers) and never
//! edits the document. It is anchored to something in the rendered page:
//! an `id` / `data-mast-anchor` (legacy `data-zeron-anchor`) value first (stable across revisions), then
//! a text-quote selector (prefix / exact / suffix over the page's visible
//! text). [`resolve_anchor`] re-finds the anchor in any revision's source; a
//! comment it cannot find is *outdated*, shown in a list rather than dropped.
//!
//! The frame script (`ui/src/artifacts/comments.rs`) runs the same rules over
//! the live DOM to draw markers; keep the two in step (the Rust side is the
//! one the engine and the comments list use, and the one with fixtures).

use serde::{Deserialize, Serialize};

use super::ArtifactKind;
use crate::work_outline::WorkOutlineActor;

/// Longest comment, in Unicode scalar values.
pub const MAX_COMMENT_CHARS: usize = 600;
/// Longest quoted text kept with a comment.
pub const MAX_QUOTE_CHARS: usize = 240;
/// Longest prefix / suffix context kept in a text-quote selector.
pub const MAX_CONTEXT_CHARS: usize = 40;
/// Comments kept on one artifact row (oldest dropped first).
pub const MAX_COMMENTS: usize = 40;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CommentStatus {
    #[default]
    Open,
    /// Resolved by a person in Mast, or addressed by a published revision.
    Resolved,
}

/// Where a comment points. At least one of `id` and `exact` is set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentAnchor {
    /// An element's `id` or `data-mast-anchor` (legacy `data-zeron-anchor`) value. Mermaid nodes use
    /// `mermaid:<node id>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// CSS path (last resort, used by the frame only; the Rust resolver does
    /// not evaluate it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    /// Text-quote selector over the visible text, whitespace collapsed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub exact: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub suffix: String,
}

impl CommentAnchor {
    pub fn is_empty(&self) -> bool {
        self.id.as_deref().is_none_or(str::is_empty) && self.exact.is_empty()
    }

    /// Clamp every field to its limit (what the engine stores).
    pub fn cleaned(&self) -> CommentAnchor {
        CommentAnchor {
            id: self
                .id
                .as_deref()
                .map(|v| clip(v.trim(), 160))
                .filter(|v| !v.is_empty()),
            selector: self
                .selector
                .as_deref()
                .map(|v| clip(v.trim(), 300))
                .filter(|v| !v.is_empty()),
            prefix: clip(&collapse(&self.prefix), MAX_CONTEXT_CHARS),
            exact: clip(&collapse(&self.exact), MAX_QUOTE_CHARS),
            suffix: clip(&collapse(&self.suffix), MAX_CONTEXT_CHARS),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactComment {
    /// `C-n`, unique per artifact.
    pub id: String,
    pub anchor: CommentAnchor,
    /// The text the comment was made on (the anchor's quote, or the
    /// element's text), for the list and the agent.
    #[serde(default)]
    pub quote: String,
    pub text: String,
    #[serde(default)]
    pub status: CommentStatus,
    /// Revision the comment was made on.
    #[serde(default)]
    pub revision: u32,
    /// Epoch milliseconds.
    #[serde(default)]
    pub at: i64,
    #[serde(default)]
    pub by: WorkOutlineActor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<i64>,
    /// `"user"` (resolved in Mast) or `"agent"` (addressed by a revision).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
    /// The revision that addressed it (agent resolution).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addressed_in: Option<u32>,
}

impl ArtifactComment {
    pub fn is_open(&self) -> bool {
        self.status == CommentStatus::Open
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorStatus {
    /// Found by its `id` / `data-mast-anchor` (legacy `data-zeron-anchor`).
    FoundById,
    /// Found by its quoted text.
    FoundByQuote,
    /// Not in this revision.
    Outdated,
}

impl AnchorStatus {
    pub fn is_found(self) -> bool {
        self != AnchorStatus::Outdated
    }
}

/// `ArtifactAddCommentParams` → `ArtifactAddCommentReply`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactAddCommentParams {
    pub artifact_id: String,
    pub anchor: CommentAnchor,
    #[serde(default)]
    pub quote: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactAddCommentReply {
    pub artifact_id: String,
    pub comment: ArtifactComment,
}

/// `ArtifactResolveComment`: a person resolves (or reopens) a comment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactResolveCommentParams {
    pub artifact_id: String,
    pub comment_id: String,
    /// `true` resolves, `false` reopens.
    pub resolved: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactResolveCommentReply {
    pub artifact_id: String,
    pub comment_id: String,
    pub resolved: bool,
}

/// The next free `C-n` id.
pub fn next_comment_id(comments: &[ArtifactComment]) -> String {
    let max = comments
        .iter()
        .filter_map(|c| c.id.get(2..)?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("C-{}", max + 1)
}

/// Normalise a comment id typed by a person or an agent (`c-3` → `C-3`).
pub fn normalize_comment_id(id: &str) -> Option<String> {
    let id = id.trim().to_ascii_uppercase();
    let n = id.strip_prefix("C-")?;
    (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).then_some(id)
}

/// Whether comments make sense on this kind of artifact.
pub fn kind_supports_comments(kind: ArtifactKind) -> bool {
    matches!(
        kind,
        ArtifactKind::HtmlFragment | ArtifactKind::HtmlPage | ArtifactKind::Mermaid
    )
}

/// The message the publishing chat receives for a new comment.
pub fn comment_message(title: &str, revision: u32, comment: &ArtifactComment) -> String {
    let quote = if comment.quote.is_empty() {
        comment.anchor.exact.as_str()
    } else {
        comment.quote.as_str()
    };
    let at = match comment.anchor.id.as_deref().filter(|v| !v.is_empty()) {
        Some(id) => format!(" · `{id}`"),
        None => String::new(),
    };
    format!(
        "Comment {} on *{title}* r{revision}{at} (\"{}\"): {}",
        comment.id,
        clip(quote, 120),
        comment.text
    )
}

/// Resolve `anchor` against one revision's source.
pub fn resolve_anchor(kind: ArtifactKind, source: &str, anchor: &CommentAnchor) -> AnchorStatus {
    if let Some(id) = anchor.id.as_deref().filter(|v| !v.is_empty()) {
        let found = match id.strip_prefix("mermaid:") {
            Some(node) => kind == ArtifactKind::Mermaid && contains_word(source, node),
            None => kind != ArtifactKind::Mermaid && has_anchor_attr(source, id),
        };
        if found {
            return AnchorStatus::FoundById;
        }
    }
    if anchor.exact.is_empty() {
        return AnchorStatus::Outdated;
    }
    let text = match kind {
        ArtifactKind::Mermaid => collapse(source),
        _ => visible_text(source),
    };
    if find_quote(&text, anchor).is_some() {
        AnchorStatus::FoundByQuote
    } else {
        AnchorStatus::Outdated
    }
}

/// Byte offset of the best occurrence of the anchor's quote in `text` (the
/// whitespace-collapsed visible text): the one whose context matches most.
pub fn find_quote(text: &str, anchor: &CommentAnchor) -> Option<usize> {
    let exact = collapse(&anchor.exact);
    if exact.is_empty() {
        return None;
    }
    let (prefix, suffix) = (collapse(&anchor.prefix), collapse(&anchor.suffix));
    let mut best: Option<(usize, usize)> = None;
    let mut from = 0;
    while let Some(rel) = text[from..].find(&exact) {
        let at = from + rel;
        let end = at + exact.len();
        let score = common_suffix(text[..at].trim_end(), &prefix) + common_prefix(text[end..].trim_start(), &suffix);
        if best.is_none_or(|(_, s)| score > s) {
            best = Some((at, score));
        }
        from = at + exact.len().max(1);
        while !text.is_char_boundary(from) {
            from += 1;
        }
    }
    best.map(|(at, _)| at)
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

fn common_suffix(a: &str, b: &str) -> usize {
    a.chars()
        .rev()
        .zip(b.chars().rev())
        .take_while(|(x, y)| x == y)
        .count()
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => text[..cut].trim_end().to_owned(),
        None => text.to_owned(),
    }
}

fn contains_word(source: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    let mut from = 0;
    while let Some(rel) = source[from..].find(word) {
        let at = from + rel;
        let end = at + word.len();
        let before = source[..at].chars().next_back();
        let after = source[end..].chars().next();
        if !before.is_some_and(ident) && !after.is_some_and(ident) {
            return true;
        }
        from = end;
    }
    false
}

/// Whether some element carries `id="<value>"` or `data-mast-anchor="<value>"`.
fn has_anchor_attr(html: &str, value: &str) -> bool {
    ["id", "data-mast-anchor", "data-zeron-anchor"].iter().any(|name| {
        ['"', '\''].iter().any(|quote| {
            let needle = format!("{name}={quote}{value}{quote}");
            let mut from = 0;
            while let Some(rel) = html[from..].find(&needle) {
                let at = from + rel;
                from = at + needle.len();
                if html[..at].chars().next_back().is_some_and(char::is_whitespace) {
                    return true;
                }
            }
            false
        })
    })
}

/// The page's visible text: tags, comments, `<script>` and `<style>` bodies
/// removed, a few entities decoded, whitespace collapsed. Block boundaries
/// become spaces so adjacent headings and paragraphs do not fuse.
pub fn visible_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let tail = &rest[lt..];
        if let Some(after) = tail.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |e| &after[e + 3..]);
            continue;
        }
        let end = tail.find('>').map_or(tail.len(), |e| e + 1);
        let tag = &tail[..end];
        let name: String = tag[1..]
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        rest = &tail[end..];
        if !tag.starts_with("</") && matches!(name.as_str(), "script" | "style") {
            let close = format!("</{name}");
            let lower = rest.to_ascii_lowercase();
            rest = lower.find(&close).map_or("", |e| {
                let after = &rest[e..];
                after.find('>').map_or("", |g| &after[g + 1..])
            });
        }
        if !matches!(
            name.as_str(),
            "a" | "b" | "i" | "em" | "strong" | "span" | "code" | "small" | "mark" | "sub" | "sup" | "tspan"
        ) {
            out.push(' ');
        }
    }
    out.push_str(rest);
    collapse(&decode_entities(&out))
}

fn decode_entities(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    const R1: &str = r#"<html><head><style>.a{}</style></head><body>
<h2 id="intake">Receiving intake</h2>
<p>Parts arrive at the <b>dock</b> and are scanned.</p>
<p data-mast-anchor="qa-step">Inspection happens next.</p>
<script>var hidden = "Inspection happens next.";</script></body></html>"#;

    fn quote(prefix: &str, exact: &str, suffix: &str) -> CommentAnchor {
        CommentAnchor {
            prefix: prefix.into(),
            exact: exact.into(),
            suffix: suffix.into(),
            ..Default::default()
        }
    }

    #[test]
    fn id_and_data_anchor_are_found_across_revisions() {
        let by_id = CommentAnchor {
            id: Some("intake".into()),
            exact: "Receiving intake".into(),
            ..Default::default()
        };
        assert_eq!(resolve_anchor(ArtifactKind::HtmlPage, R1, &by_id), AnchorStatus::FoundById);
        let by_data = CommentAnchor { id: Some("qa-step".into()), ..Default::default() };
        assert_eq!(resolve_anchor(ArtifactKind::HtmlFragment, R1, &by_data), AnchorStatus::FoundById);
        // The pre-rename attribute still resolves.
        let legacy = R1.replace("data-mast-anchor", "data-zeron-anchor");
        assert_eq!(resolve_anchor(ArtifactKind::HtmlFragment, &legacy, &by_data), AnchorStatus::FoundById);
        // The id is gone but the text survived: found by quote.
        let r2 = R1.replace("id=\"intake\"", "");
        assert_eq!(resolve_anchor(ArtifactKind::HtmlPage, &r2, &by_id), AnchorStatus::FoundByQuote);
        // Both gone: outdated.
        let r3 = r2.replace("Receiving intake", "Dock arrivals");
        assert_eq!(resolve_anchor(ArtifactKind::HtmlPage, &r3, &by_id), AnchorStatus::Outdated);
        assert!(!AnchorStatus::Outdated.is_found());
    }

    #[test]
    fn id_is_a_whole_attribute_value() {
        let html = r#"<h2 id="intake-2" data-id="intake">x</h2>"#;
        let anchor = CommentAnchor { id: Some("intake".into()), ..Default::default() };
        assert_eq!(resolve_anchor(ArtifactKind::HtmlPage, html, &anchor), AnchorStatus::Outdated);
    }

    #[test]
    fn quote_matches_across_inline_tags_and_skips_scripts() {
        let anchor = quote("", "arrive at the dock and are scanned", "");
        assert_eq!(resolve_anchor(ArtifactKind::HtmlPage, R1, &anchor), AnchorStatus::FoundByQuote);
        let only_script = r#"<script>var s="Secret words";</script><p>x</p>"#;
        assert_eq!(
            resolve_anchor(ArtifactKind::HtmlPage, only_script, &quote("", "Secret words", "")),
            AnchorStatus::Outdated
        );
    }

    #[test]
    fn repeated_quote_prefers_the_matching_context() {
        let text = "alpha item beta item gamma item delta";
        let a = quote("beta ", "item", " gamma");
        assert_eq!(find_quote(text, &a), Some(text.find("item gamma").unwrap()));
        let b = quote("alpha ", "item", " beta");
        assert_eq!(find_quote(text, &b), Some(text.find("item").unwrap()));
    }

    #[test]
    fn entities_and_block_boundaries() {
        let html = "<h1>Tom &amp; Jerry</h1><p>Next</p>";
        assert_eq!(visible_text(html), "Tom & Jerry Next");
        assert_eq!(visible_text("a<br>b<i>c</i>d"), "a bcd");
    }

    #[test]
    fn mermaid_nodes_resolve_by_node_id_then_label() {
        let src = "flowchart LR\n  receiving[Receiving] --> qa[Inspection]\n";
        let by_node = CommentAnchor { id: Some("mermaid:receiving".into()), ..Default::default() };
        assert_eq!(resolve_anchor(ArtifactKind::Mermaid, src, &by_node), AnchorStatus::FoundById);
        let renamed = src.replace("receiving", "intake");
        let with_label = CommentAnchor {
            id: Some("mermaid:receiving".into()),
            exact: "Receiving".into(),
            ..Default::default()
        };
        assert_eq!(resolve_anchor(ArtifactKind::Mermaid, &renamed, &with_label), AnchorStatus::FoundByQuote);
        let gone = renamed.replace("Receiving", "Dock");
        assert_eq!(resolve_anchor(ArtifactKind::Mermaid, &gone, &with_label), AnchorStatus::Outdated);
        // `receiving2` is a different node.
        assert_eq!(
            resolve_anchor(ArtifactKind::Mermaid, "A --> receiving2", &by_node),
            AnchorStatus::Outdated
        );
    }

    #[test]
    fn ids_and_messages() {
        let mut c = ArtifactComment { id: "C-2".into(), text: "Why?".into(), quote: "Inspection".into(), ..Default::default() };
        assert_eq!(next_comment_id(&[c.clone()]), "C-3");
        assert_eq!(next_comment_id(&[]), "C-1");
        assert_eq!(normalize_comment_id(" c-12 ").as_deref(), Some("C-12"));
        assert_eq!(normalize_comment_id("Q-1"), None);
        assert_eq!(normalize_comment_id("C-"), None);
        c.anchor.id = Some("qa-step".into());
        assert_eq!(
            comment_message("Flow", 4, &c),
            "Comment C-2 on *Flow* r4 · `qa-step` (\"Inspection\"): Why?"
        );
    }

    #[test]
    fn cleaned_anchor_clamps_and_collapses() {
        let long = "word ".repeat(100);
        let a = CommentAnchor { exact: long, prefix: " a \n b ".into(), ..Default::default() }.cleaned();
        assert!(a.exact.chars().count() <= MAX_QUOTE_CHARS);
        assert_eq!(a.prefix, "a b");
        assert!(CommentAnchor::default().is_empty());
    }
}
