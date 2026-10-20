//! Plan question blocks (`docs/planning-artifact-conventions.md` §4):
//!
//! ```text
//! > [!question] Q-3 · Should receiving lines merge by MPN or by internal part number?
//! > blocking: true
//! > example: two lines for LM358 from different suppliers
//! > blocks: ENG-2690 data model; §4 Receiving flow
//! > status: open
//! ```
//!
//! [`parse_question`] is the one pure parser for that shape. The Markdown
//! parser turns a matching block quote into `Block::Question`, and the
//! compare view reads the id from that block, so there is a single reader.
//! pulldown-cmark joins the quote's lines into one paragraph, which is why
//! this works on the quote's source text instead of its parsed children.
//!
//! Nothing here fails: a header without an id, a quote without fields, and
//! unknown fields all give a card (`[!question]` alone is an empty card).
use super::parser::{self, Block, InlineRun, InlineStyle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionStatus {
    Open,
    Answered,
    Resolved,
}

impl QuestionStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Answered => "Answered",
            Self::Resolved => "Resolved",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "open" => Some(Self::Open),
            "answered" => Some(Self::Answered),
            "resolved" => Some(Self::Resolved),
            _ => None,
        }
    }
}

/// One labelled row of the card. `key` is the lowercase field name.
#[derive(Debug, Clone, PartialEq)]
pub struct QuestionField {
    pub key: String,
    pub label: String,
    pub runs: Vec<InlineRun>,
}

impl QuestionField {
    pub fn is_decision(&self) -> bool {
        self.key == "decision"
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuestionCard {
    /// Upper-cased `Q-n`; `None` when the header carries no id.
    pub id: Option<String>,
    /// The question, from the header line (and any unlabelled lines that
    /// follow it).
    pub text: Vec<InlineRun>,
    pub status: QuestionStatus,
    pub blocking: bool,
    /// Rows in display order: example, blocks, unknown fields in source
    /// order, then decision.
    pub fields: Vec<QuestionField>,
}

impl QuestionCard {
    /// Resolved questions collapse to the question and its decision.
    pub fn collapsible(&self) -> bool {
        self.status == QuestionStatus::Resolved
    }
}

/// `Some(card)` when `quote` (the block quote's source, `>` prefixes
/// included) opens with `[!question]`.
pub fn parse_question(quote: &str) -> Option<QuestionCard> {
    let mut lines = quote.lines().map(strip_quote_marker);
    let header = lines.next()?;
    let rest = header.trim_start().get(..QUESTION_TAG.len())?;
    if !rest.eq_ignore_ascii_case(QUESTION_TAG) {
        return None;
    }
    let rest = &header.trim_start()[QUESTION_TAG.len()..];
    let (id, text) = split_header(rest);

    let mut text_lines = vec![text.to_owned()];
    let mut raw: Vec<(String, String)> = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match field_line(line) {
            Some((key, value)) => raw.push((key, value)),
            // A line that is not `key: value` continues the previous field,
            // or the question when no field has started.
            None => match raw.last_mut() {
                Some((_, value)) => append_words(value, line),
                None => text_lines.push(line.to_owned()),
            },
        }
    }

    let mut status = QuestionStatus::Open;
    let mut blocking = false;
    let mut example = None;
    let mut blocks = None;
    let mut decision = None;
    let mut others: Vec<QuestionField> = Vec::new();
    for (key, value) in raw {
        if value.is_empty() {
            continue;
        }
        match key.as_str() {
            "status" => match QuestionStatus::parse(&value) {
                Some(parsed) => status = parsed,
                // An unrecognised status stays visible as a plain row.
                None => others.push(field(&key, &value)),
            },
            "blocking" => blocking = matches!(value.to_ascii_lowercase().as_str(), "true" | "yes"),
            "example" => example = Some(field(&key, &value)),
            "blocks" => blocks = Some(field(&key, &value)),
            "decision" => decision = Some(field(&key, &value)),
            _ => others.push(field(&key, &value)),
        }
    }
    let fields = example
        .into_iter()
        .chain(blocks)
        .chain(others)
        .chain(decision)
        .collect();

    Some(QuestionCard {
        id,
        text: inline_runs(&text_lines.join(" ")),
        status,
        blocking,
        fields,
    })
}

const QUESTION_TAG: &str = "[!question]";

/// `> text` → `text` (one marker and one following space).
fn strip_quote_marker(line: &str) -> &str {
    let line = line.trim_start();
    let line = line.strip_prefix('>').unwrap_or(line);
    line.strip_prefix(' ').unwrap_or(line)
}

fn is_separator(c: char) -> bool {
    c.is_whitespace() || c == '\u{b7}'
}

/// The `Q-n` id in a header line and the question text after it. An id that
/// is not the first word is still found (and left in the text).
fn split_header(rest: &str) -> (Option<String>, &str) {
    let rest = rest.trim_matches(is_separator);
    let id_of = |word: &str| {
        (word.len() > 2 && word.get(..2).is_some_and(|p| p.eq_ignore_ascii_case("q-"))).then(|| {
            word.trim_end_matches(|c: char| !c.is_alphanumeric())
                .to_ascii_uppercase()
        })
    };
    let first_end = rest.find(is_separator).unwrap_or(rest.len());
    if let Some(id) = id_of(&rest[..first_end]) {
        let text = rest[first_end..].trim_matches(is_separator);
        return (Some(id), text);
    }
    (rest.split(is_separator).find_map(id_of), rest)
}

/// `key: value` where the key is a single word (letters, digits, `-`, `_`).
fn field_line(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once(':')?;
    let valid = key.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    valid.then(|| (key.to_ascii_lowercase(), value.trim().to_owned()))
}

fn append_words(value: &mut String, more: &str) {
    if !value.is_empty() {
        value.push(' ');
    }
    value.push_str(more);
}

fn field(key: &str, value: &str) -> QuestionField {
    QuestionField {
        key: key.to_owned(),
        label: label(key),
        runs: inline_runs(value),
    }
}

/// `example` → `Example`, `follow-up` → `Follow up`.
fn label(key: &str) -> String {
    let spaced = key.replace(['-', '_'], " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `text` as inline runs (code, links, emphasis). Text that parses as
/// anything but one paragraph stays plain.
fn inline_runs(text: &str) -> Vec<InlineRun> {
    if text.is_empty() {
        return Vec::new();
    }
    if let [top] = parser::parse_full(text).blocks.as_slice()
        && let Block::Paragraph { runs } = &top.block
    {
        return runs.clone();
    }
    vec![InlineRun {
        text: text.to_owned(),
        style: InlineStyle::default(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(runs: &[InlineRun]) -> String {
        runs.iter().map(|run| run.text.as_str()).collect()
    }

    const EXAMPLE: &str = "\
> [!question] Q-1 · Where is the retrospective SLA report, and does it set the 1/5-day rule, the 3pm cutoff and the facility timezones?
> blocking: false
> example: exactly 100 lines with no reeling gets 1 day in the UI (> 100), 5 days in the 13 Oct plan (≥ 100)…
> blocks: D-7 thresholds; ticket 1
> status: resolved
> decision: the SLA report definition: ship-as-kit 1 business day; …
";

    #[test]
    fn parses_the_real_example() {
        let card = parse_question(EXAMPLE).unwrap();
        assert_eq!(card.id.as_deref(), Some("Q-1"));
        assert_eq!(
            plain(&card.text),
            "Where is the retrospective SLA report, and does it set the 1/5-day rule, the 3pm cutoff and the facility timezones?"
        );
        assert_eq!(card.status, QuestionStatus::Resolved);
        assert!(!card.blocking);
        assert!(card.collapsible());
        let rows: Vec<_> = card
            .fields
            .iter()
            .map(|f| (f.label.as_str(), plain(&f.runs)))
            .collect();
        assert_eq!(
            rows,
            [
                (
                    "Example",
                    "exactly 100 lines with no reeling gets 1 day in the UI (> 100), 5 days in the 13 Oct plan (≥ 100)…".to_owned()
                ),
                ("Blocks", "D-7 thresholds; ticket 1".to_owned()),
                (
                    "Decision",
                    "the SLA report definition: ship-as-kit 1 business day; …".to_owned()
                ),
            ]
        );
        assert!(card.fields[2].is_decision());
    }

    #[test]
    fn all_statuses_and_blocking_values() {
        for (status, want) in [
            ("open", QuestionStatus::Open),
            ("answered", QuestionStatus::Answered),
            ("RESOLVED", QuestionStatus::Resolved),
        ] {
            for (blocking, want_blocking) in [("true", true), ("false", false), ("TRUE", true)] {
                let card = parse_question(&format!(
                    "> [!question] Q-2 · Why?\n> blocking: {blocking}\n> status: {status}\n"
                ))
                .unwrap();
                assert_eq!(card.status, want);
                assert_eq!(card.blocking, want_blocking);
                assert!(card.fields.is_empty(), "status/blocking are not rows");
            }
        }
        let defaults = parse_question("> [!question] Q-2 · Why?\n").unwrap();
        assert_eq!(defaults.status, QuestionStatus::Open);
        assert!(!defaults.blocking);
    }

    #[test]
    fn unknown_fields_and_bad_status_are_kept_in_order() {
        let card = parse_question(
            "> [!question] Q-4 · Which?\n> owner: Dana\n> decision: A\n> status: pending\n> follow-up: ask Lee\n> example: e\n",
        )
        .unwrap();
        assert_eq!(card.status, QuestionStatus::Open);
        let rows: Vec<_> = card
            .fields
            .iter()
            .map(|f| (f.label.as_str(), plain(&f.runs)))
            .collect();
        assert_eq!(
            rows,
            [
                ("Example", "e".to_owned()),
                ("Owner", "Dana".to_owned()),
                ("Status", "pending".to_owned()),
                ("Follow up", "ask Lee".to_owned()),
                ("Decision", "A".to_owned()),
            ]
        );
    }

    #[test]
    fn inline_markdown_in_values_survives() {
        let card = parse_question(
            "> [!question] Q-5 · Is `MPN` **unique**?\n> example: see [spec](https://example.com/s) and `LM358`\n",
        )
        .unwrap();
        assert!(card.text.iter().any(|r| r.style.code && r.text == "MPN"));
        assert!(card.text.iter().any(|r| r.style.bold && r.text == "unique"));
        let runs = &card.fields[0].runs;
        assert!(
            runs.iter()
                .any(|r| r.style.link.as_deref() == Some("https://example.com/s"))
        );
        assert!(runs.iter().any(|r| r.style.code && r.text == "LM358"));
    }

    #[test]
    fn malformed_blocks_give_cards_or_nothing_never_a_panic() {
        // Not a question.
        assert!(parse_question("> just a quote\n> blocking: true\n").is_none());
        assert!(parse_question("> [!note] Q-1 · hi\n").is_none());
        assert!(parse_question("").is_none());
        assert!(parse_question("é").is_none());
        // No id.
        let card = parse_question("> [!question] Who owns it?\n> status: answered\n").unwrap();
        assert_eq!(card.id, None);
        assert_eq!(plain(&card.text), "Who owns it?");
        assert_eq!(card.status, QuestionStatus::Answered);
        // No fields, no text.
        let card = parse_question("> [!question]\n").unwrap();
        assert_eq!(card.id, None);
        assert!(card.text.is_empty() && card.fields.is_empty());
        // Id only; multi-byte text next to an id-shaped word.
        let card = parse_question("> [!question] Q-9\n").unwrap();
        assert_eq!(card.id.as_deref(), Some("Q-9"));
        assert!(
            parse_question("> [!question] éé Q-1 · ok")
                .unwrap()
                .id
                .is_some()
        );
        assert_eq!(parse_question("> [!question] é\n").unwrap().id, None);
        // Empty values are dropped; stray lines continue the previous field.
        let card = parse_question(
            "> [!question] q-3 · Why\n> that matters?\n> example:\n> blocks: A;\n> B\n",
        )
        .unwrap();
        assert_eq!(card.id.as_deref(), Some("Q-3"));
        assert_eq!(plain(&card.text), "Why that matters?");
        assert_eq!(card.fields.len(), 1);
        assert_eq!(plain(&card.fields[0].runs), "A; B");
    }

    #[test]
    fn parse_full_turns_question_quotes_into_cards_and_leaves_other_quotes() {
        let source = format!(
            "# Plan\n\n{EXAMPLE}\n> [!note] heads up\n> blocking: true\n\n> plain quote\n\n\
             > [!warning] careful\n\n- > [!question] Q-8 · nested stays a quote\n"
        );
        let tree = parser::parse_full(&source);
        let kinds: Vec<&str> = tree
            .blocks
            .iter()
            .map(|top| match &top.block {
                Block::Heading { .. } => "heading",
                Block::Question(_) => "question",
                Block::BlockQuote { .. } => "quote",
                Block::List { .. } => "list",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            ["heading", "question", "quote", "quote", "quote", "list"]
        );
        // The question block keeps its source range.
        let Block::Question(card) = &tree.blocks[1].block else {
            unreachable!()
        };
        assert_eq!(card.id.as_deref(), Some("Q-1"));
        assert!(source[tree.blocks[1].range.clone()].starts_with("> [!question] Q-1"));
    }

    #[test]
    fn streaming_parse_matches_the_full_parse_with_question_cards() {
        let source = format!("intro\n\n{EXAMPLE}\n> [!question] Q-2 · Next?\n> status: open\n");
        let mut stream = parser::IncrementalParser::new();
        for ch in source.chars() {
            stream.append(&ch.to_string());
            assert_eq!(stream.tree(), &parser::parse_full(stream.source()));
        }
        assert!(
            stream
                .tree()
                .blocks
                .iter()
                .filter(|top| matches!(top.block, Block::Question(_)))
                .count()
                >= 1
        );
    }
}
