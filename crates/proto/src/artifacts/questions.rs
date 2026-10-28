//! Plan questions as data (Artifacts phase 3 D): the engine and the UI both
//! read questions with [`extract_questions`], so a Required Action, a card
//! and a drift count always agree on which questions exist.
//!
//! Markdown: `> [!question] Q-3 · text` quotes with `blocking:` / `status:` /
//! `blocks:` / `decision:` lines. HTML: `data-zeron-question="Q-3"` elements
//! (`data-status`, `data-blocking`, `data-blocks`, `data-decision`). A
//! question without an id is skipped: it cannot be deduplicated or answered.
//! The UI's card parser (`markdown/question.rs`) stays the display reader.

use serde::{Deserialize, Serialize};

use super::ArtifactKind;
use crate::work_outline::WorkOutlineActor;

/// Longest recorded answer, in Unicode scalar values.
pub const MAX_ANSWER_CHARS: usize = 600;
/// Answers kept on one artifact row (newest dropped last; the row is capped).
pub const MAX_ANSWERS: usize = 24;

/// Status written in the document itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlanQuestionStatus {
    #[default]
    Open,
    Answered,
    Resolved,
}

impl PlanQuestionStatus {
    fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "answered" => Self::Answered,
            "resolved" => Self::Resolved,
            _ => Self::Open,
        }
    }
}

/// One question as written in a revision.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanQuestion {
    /// Upper-cased `Q-n`.
    pub id: String,
    pub text: String,
    pub blocking: bool,
    pub status: PlanQuestionStatus,
    pub blocks: Option<String>,
}

/// A user's answer recorded on the artifact row (not in the document: the
/// agent folds it into a later revision as `status: resolved` + `decision:`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactQuestionAnswer {
    /// Upper-cased `Q-n`.
    pub id: String,
    pub answer: String,
    /// Epoch milliseconds.
    pub at: i64,
    #[serde(default)]
    pub by: WorkOutlineActor,
    /// The revision that was current when it was answered.
    #[serde(default)]
    pub revision: u32,
}

/// `ArtifactAnswerQuestion`: record an answer from the plan's question card.
/// A registry write like `ArtifactPin`: local, not forwardable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactAnswerQuestionParams {
    pub artifact_id: String,
    pub question_id: String,
    pub answer: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactAnswerQuestionReply {
    pub artifact_id: String,
    pub question_id: String,
}

/// `ArtifactDrift`: how far the work has moved since the artifact's latest
/// revision, from the owning device's checkouts. Forwardable (owning device).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactDriftParams {
    pub artifact_id: String,
    /// Forward to this device (the artifact's owner), like `ArtifactContent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactDriftReply {
    pub artifact_id: String,
    /// Commits made after the latest revision on the checkouts the work
    /// item's chats work in (their bound worktrees / branches).
    #[serde(default)]
    pub commits_since: u32,
    /// Branch of the checkout that has the most such commits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Newest such commit, epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_commit_at: Option<i64>,
    /// Epoch milliseconds of the revision the commits are measured from.
    #[serde(default)]
    pub since: i64,
}

/// The questions in `text`, in document order (Markdown block quotes and
/// HTML `data-zeron-question` elements). Kinds without questions read empty.
pub fn extract_questions(kind: ArtifactKind, text: &str) -> Vec<PlanQuestion> {
    match kind {
        ArtifactKind::Markdown | ArtifactKind::Unknown => markdown_questions(text),
        ArtifactKind::HtmlFragment | ArtifactKind::HtmlPage => html_questions(text),
        _ => Vec::new(),
    }
}

fn is_separator(c: char) -> bool {
    c.is_whitespace() || c == '\u{b7}'
}

fn id_of(word: &str) -> Option<String> {
    (word.len() > 2 && word.get(..2).is_some_and(|p| p.eq_ignore_ascii_case("q-"))).then(|| {
        word.trim_end_matches(|c: char| !c.is_alphanumeric())
            .to_ascii_uppercase()
    })
}

fn markdown_questions(text: &str) -> Vec<PlanQuestion> {
    let mut out = Vec::new();
    let mut quote: Vec<&str> = Vec::new();
    let flush = |quote: &mut Vec<&str>, out: &mut Vec<PlanQuestion>| {
        if let Some(question) = markdown_question(quote) {
            out.push(question);
        }
        quote.clear();
    };
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix('>') {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            // A new `[!question]` header starts a new block even without a
            // blank line between two quotes.
            if rest.trim_start().to_ascii_lowercase().starts_with("[!question]") {
                flush(&mut quote, &mut out);
            }
            quote.push(rest);
        } else {
            flush(&mut quote, &mut out);
        }
    }
    flush(&mut quote, &mut out);
    out
}

fn markdown_question(lines: &[&str]) -> Option<PlanQuestion> {
    const TAG: &str = "[!question]";
    let header = lines.first()?.trim_start();
    if !header.get(..TAG.len())?.eq_ignore_ascii_case(TAG) {
        return None;
    }
    let rest = header[TAG.len()..].trim_matches(is_separator);
    let first_end = rest.find(is_separator).unwrap_or(rest.len());
    let (id, text) = match id_of(&rest[..first_end]) {
        Some(id) => (id, rest[first_end..].trim_matches(is_separator)),
        None => (rest.split(is_separator).find_map(id_of)?, rest),
    };
    let mut question = PlanQuestion {
        id,
        text: text.to_owned(),
        ..PlanQuestion::default()
    };
    let mut field: Option<&'static str> = None;
    for line in &lines[1..] {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let kv = line.split_once(':').filter(|(key, _)| {
            key.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        });
        match kv {
            Some((key, value)) => {
                let value = value.trim();
                field = None;
                match key.to_ascii_lowercase().as_str() {
                    "status" => question.status = PlanQuestionStatus::parse(value),
                    "blocking" => {
                        question.blocking =
                            matches!(value.to_ascii_lowercase().as_str(), "true" | "yes")
                    }
                    "blocks" if !value.is_empty() => {
                        question.blocks = Some(value.to_owned());
                        field = Some("blocks");
                    }
                    _ => {}
                }
            }
            None => match field {
                Some("blocks") => {
                    if let Some(blocks) = question.blocks.as_mut() {
                        blocks.push(' ');
                        blocks.push_str(line);
                    }
                }
                _ => {
                    if question.text.is_empty() {
                        question.text = line.to_owned();
                    } else if question.status == PlanQuestionStatus::Open
                        && !question.blocking
                        && question.blocks.is_none()
                    {
                        question.text.push(' ');
                        question.text.push_str(line);
                    }
                }
            },
        }
    }
    Some(question)
}

fn html_questions(text: &str) -> Vec<PlanQuestion> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = text[from..].find("data-zeron-question") {
        let at = from + rel;
        let tag_start = text[..at].rfind('<').unwrap_or(at);
        let tag_end = text[at..].find('>').map_or(text.len(), |e| at + e);
        let tag = &text[tag_start..tag_end];
        from = tag_end.max(at + 1);
        let Some(id) = attr(tag, "data-zeron-question").and_then(|v| id_of(v.trim())) else {
            continue;
        };
        let name: String = tag[1.min(tag.len())..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        let close = format!("</{name}");
        let body_end = text[tag_end..]
            .find(&close)
            .map_or(text.len(), |e| tag_end + e);
        let body = strip_tags(text.get(tag_end + 1..body_end).unwrap_or(""));
        out.push(PlanQuestion {
            id,
            text: body.split_whitespace().collect::<Vec<_>>().join(" "),
            blocking: attr(tag, "data-blocking")
                .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "yes")),
            status: attr(tag, "data-status")
                .map(PlanQuestionStatus::parse)
                .unwrap_or_default(),
            blocks: attr(tag, "data-blocks").filter(|v| !v.is_empty()).map(str::to_owned),
        });
    }
    out
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("{name}=");
    let mut search = 0;
    while let Some(rel) = tag[search..].find(&needle) {
        let at = search + rel;
        search = at + needle.len();
        // A whole attribute name, not the tail of a longer one.
        if tag[..at]
            .chars()
            .next_back()
            .is_some_and(|c| !c.is_whitespace())
        {
            continue;
        }
        let rest = &tag[at + needle.len()..];
        let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let inner = &rest[1..];
        return inner.find(quote).map(|end| &inner[..end]);
    }
    None
}

fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

impl super::ArtifactView {
    /// The recorded answer to question `id` (case-insensitive).
    pub fn answer_of(&self, id: &str) -> Option<&ArtifactQuestionAnswer> {
        self.answers.iter().find(|a| a.id.eq_ignore_ascii_case(id))
    }
}

/// Questions of `questions` that still need an answer: open in the document
/// and not answered on the row.
pub fn open_unanswered<'a>(
    questions: &'a [PlanQuestion],
    answers: &[ArtifactQuestionAnswer],
) -> Vec<&'a PlanQuestion> {
    questions
        .iter()
        .filter(|q| q.status == PlanQuestionStatus::Open)
        .filter(|q| !answers.iter().any(|a| a.id.eq_ignore_ascii_case(&q.id)))
        .collect()
}

/// Answers whose question the document has not yet marked resolved and that
/// no later revision followed: the "answered but not reflected in the doc"
/// decision drift (design doc, C). `latest_revision_at` is the newest
/// revision's time.
pub fn unreflected_answers<'a>(
    questions: &[PlanQuestion],
    answers: &'a [ArtifactQuestionAnswer],
    latest_revision_at: i64,
) -> Vec<&'a ArtifactQuestionAnswer> {
    answers
        .iter()
        .filter(|a| a.at >= latest_revision_at)
        .filter(|a| {
            questions
                .iter()
                .find(|q| q.id.eq_ignore_ascii_case(&a.id))
                .is_some_and(|q| q.status != PlanQuestionStatus::Resolved)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MD: &str = "# Plan\n\n> [!question] Q-1 · Merge by MPN?\n> blocking: true\n> blocks: ENG-1; §4\n> status: open\n\n> [!question] Q-2 · Which\n> timezone applies?\n> status: resolved\n> decision: UTC\n\n> [!question] no id here\n\n> [!question] q-3 Third\n> blocking: no\n";

    #[test]
    fn markdown_questions_in_order() {
        let qs = extract_questions(ArtifactKind::Markdown, MD);
        let ids: Vec<_> = qs.iter().map(|q| q.id.as_str()).collect();
        assert_eq!(ids, ["Q-1", "Q-2", "Q-3"]);
        assert!(qs[0].blocking);
        assert_eq!(qs[0].blocks.as_deref(), Some("ENG-1; §4"));
        assert_eq!(qs[0].text, "Merge by MPN?");
        assert_eq!(qs[1].status, PlanQuestionStatus::Resolved);
        assert_eq!(qs[1].text, "Which timezone applies?");
        assert!(!qs[2].blocking && qs[2].status == PlanQuestionStatus::Open);
    }

    #[test]
    fn html_questions_read_attributes() {
        let html = r#"<p>x</p><div data-zeron-question="Q-4" data-status="open" data-blocking="true" data-blocks="A">Pick <b>one</b>?</div><div data-zeron-question="Q-5" data-status="resolved">Done</div>"#;
        let qs = extract_questions(ArtifactKind::HtmlFragment, html);
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].id, "Q-4");
        assert!(qs[0].blocking);
        assert_eq!(qs[0].text, "Pick one?");
        assert_eq!(qs[1].status, PlanQuestionStatus::Resolved);
    }

    #[test]
    fn open_unanswered_skips_answered_and_resolved() {
        let qs = extract_questions(ArtifactKind::Markdown, MD);
        let answers = vec![ArtifactQuestionAnswer {
            id: "Q-1".into(),
            answer: "MPN".into(),
            at: 5,
            ..Default::default()
        }];
        let open: Vec<_> = open_unanswered(&qs, &answers).iter().map(|q| q.id.clone()).collect();
        assert_eq!(open, ["Q-3"]);
    }

    #[test]
    fn unreflected_answers_need_no_newer_revision() {
        let qs = extract_questions(ArtifactKind::Markdown, MD);
        let answers = vec![
            ArtifactQuestionAnswer { id: "Q-1".into(), at: 10, ..Default::default() },
            ArtifactQuestionAnswer { id: "Q-2".into(), at: 10, ..Default::default() },
        ];
        // Q-2 is resolved in the doc; Q-1 answered after the latest revision.
        assert_eq!(unreflected_answers(&qs, &answers, 5).len(), 1);
        assert!(unreflected_answers(&qs, &answers, 11).is_empty());
    }
}
