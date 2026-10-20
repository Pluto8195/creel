//! Block-level diff of two Markdown revisions (the rendered compare view).
//!
//! Both sources are split into the Markdown preview's own top-level blocks
//! (`markdown::parser::parse_full`: headings, paragraphs, lists, code blocks,
//! block quotes, tables, rules). Question blocks (`> [!question] Q-3 · …`) are
//! `Block::Question`, read by `markdown::question::parse_question`. Blocks are aligned with an
//! LCS over their parsed content (so whitespace-only edits are not changes),
//! and a gap between two matches pairs up like-kinded blocks as *changed*; the
//! rest are *added* / *removed*.
//!
//! Because the index of a block here is its index in the preview, the per-side
//! statuses can tint the preview's rows directly.
use crate::markdown::parser::{self, Block};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockStatus {
    Same,
    Added,
    Removed,
    Changed,
}

/// What kind of block it is, for pairing and for the summary.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tag {
    Heading,
    Question(Option<String>),
    Code,
    List,
    Paragraph,
    Other,
}

#[derive(Debug, Clone)]
struct Item {
    block: Block,
    tag: Tag,
    /// The heading's plain text.
    heading: Option<String>,
}

fn split(source: &str) -> Vec<Item> {
    parser::parse_full(source)
        .blocks
        .iter()
        .map(|top| {
            let (tag, heading) = match &top.block {
                Block::Heading { runs, .. } => (
                    Tag::Heading,
                    Some(runs.iter().map(|run| run.text.as_str()).collect::<String>()),
                ),
                Block::Question(card) => (Tag::Question(card.id.clone()), None),
                Block::CodeBlock { .. } => (Tag::Code, None),
                Block::List { .. } => (Tag::List, None),
                Block::Paragraph { .. } => (Tag::Paragraph, None),
                _ => (Tag::Other, None),
            };
            Item {
                block: top.block.clone(),
                tag,
                heading,
            }
        })
        .collect()
}

/// A row of the aligned diff: `old` / `new` are block indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlockRow {
    pub(crate) old: Option<usize>,
    pub(crate) new: Option<usize>,
    pub(crate) status: BlockStatus,
}

/// The one-line summary above the rendered compare.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SectionSummary {
    pub(crate) changed: Vec<String>,
    pub(crate) added: Vec<String>,
    pub(crate) removed: Vec<String>,
}

impl SectionSummary {
    pub(crate) fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.added.is_empty() && self.removed.is_empty()
    }

    /// "changed: §3 Data model, §5 Rollout; added: Q-7" (or "No changes").
    pub(crate) fn text(&self) -> String {
        let parts: Vec<String> = [
            ("changed", &self.changed),
            ("added", &self.added),
            ("removed", &self.removed),
        ]
        .into_iter()
        .filter(|(_, items)| !items.is_empty())
        .map(|(label, items)| format!("{label}: {}", items.join(", ")))
        .collect();
        if parts.is_empty() {
            "No changes".to_owned()
        } else {
            parts.join("; ")
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BlockDiff {
    pub(crate) rows: Vec<BlockRow>,
    /// Status of each old block (never `Added`).
    pub(crate) old_status: Vec<BlockStatus>,
    /// Status of each new block (never `Removed`).
    pub(crate) new_status: Vec<BlockStatus>,
    pub(crate) summary: SectionSummary,
}

impl BlockDiff {
    pub(crate) fn changed_blocks(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| row.status != BlockStatus::Same)
            .count()
    }
}

/// Past this many blocks per side the LCS table is skipped and blocks are
/// matched only by their common prefix and suffix.
const LCS_LIMIT: usize = 3000;

pub(crate) fn diff_blocks(old: &str, new: &str) -> BlockDiff {
    let old_items = split(old);
    let new_items = split(new);
    let rows = align(&old_items, &new_items);
    let mut old_status = vec![BlockStatus::Same; old_items.len()];
    let mut new_status = vec![BlockStatus::Same; new_items.len()];
    for row in &rows {
        if let Some(ix) = row.old {
            old_status[ix] = row.status;
        }
        if let Some(ix) = row.new {
            new_status[ix] = row.status;
        }
    }
    let summary = summarize(&rows, &old_items, &new_items);
    BlockDiff {
        rows,
        old_status,
        new_status,
        summary,
    }
}

fn align(old: &[Item], new: &[Item]) -> Vec<BlockRow> {
    let same = |a: usize, b: usize| old[a].block == new[b].block;
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && same(prefix, prefix) {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old.len() - prefix
        && suffix < new.len() - prefix
        && same(old.len() - 1 - suffix, new.len() - 1 - suffix)
    {
        suffix += 1;
    }
    let (o_end, n_end) = (old.len() - suffix, new.len() - suffix);
    let mut rows: Vec<BlockRow> = (0..prefix)
        .map(|ix| BlockRow {
            old: Some(ix),
            new: Some(ix),
            status: BlockStatus::Same,
        })
        .collect();
    // Matched pairs of the middle, by LCS.
    let (om, nm) = (o_end - prefix, n_end - prefix);
    let mut matches: Vec<(usize, usize)> = Vec::new();
    if om > 0 && nm > 0 && om <= LCS_LIMIT && nm <= LCS_LIMIT {
        let width = nm + 1;
        let mut table = vec![0u32; (om + 1) * width];
        for i in (0..om).rev() {
            for j in (0..nm).rev() {
                table[i * width + j] = if same(prefix + i, prefix + j) {
                    table[(i + 1) * width + j + 1] + 1
                } else {
                    table[(i + 1) * width + j].max(table[i * width + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < om && j < nm {
            if same(prefix + i, prefix + j) {
                matches.push((prefix + i, prefix + j));
                i += 1;
                j += 1;
            } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
    }
    let (mut o, mut n) = (prefix, prefix);
    for (mo, mn) in matches.into_iter().chain([(o_end, n_end)]) {
        gap(old, new, o..mo, n..mn, &mut rows);
        if mo < o_end || mn < n_end {
            rows.push(BlockRow {
                old: Some(mo),
                new: Some(mn),
                status: BlockStatus::Same,
            });
        }
        o = mo + 1;
        n = mn + 1;
    }
    for ix in 0..suffix {
        rows.push(BlockRow {
            old: Some(o_end + ix),
            new: Some(n_end + ix),
            status: BlockStatus::Same,
        });
    }
    rows
}

/// Pair like-kinded blocks of an unmatched gap, in order, as changed.
fn gap(
    old: &[Item],
    new: &[Item],
    o: std::ops::Range<usize>,
    n: std::ops::Range<usize>,
    rows: &mut Vec<BlockRow>,
) {
    let pairable = |a: &Item, b: &Item| match (&a.tag, &b.tag) {
        (Tag::Question(x), Tag::Question(y)) => x == y,
        (x, y) => x == y,
    };
    let mut next_new = n.start;
    for oi in o {
        let found = (next_new..n.end).find(|&ni| pairable(&old[oi], &new[ni]));
        match found {
            Some(ni) => {
                for added in next_new..ni {
                    rows.push(BlockRow {
                        old: None,
                        new: Some(added),
                        status: BlockStatus::Added,
                    });
                }
                rows.push(BlockRow {
                    old: Some(oi),
                    new: Some(ni),
                    status: BlockStatus::Changed,
                });
                next_new = ni + 1;
            }
            None => rows.push(BlockRow {
                old: Some(oi),
                new: None,
                status: BlockStatus::Removed,
            }),
        }
    }
    for added in next_new..n.end {
        rows.push(BlockRow {
            old: None,
            new: Some(added),
            status: BlockStatus::Added,
        });
    }
}

/// "§3 Data model" for a numbered heading ("3. Data model", "3.2 Fields"),
/// else the heading text.
pub(crate) fn section_label(heading: &str) -> String {
    let text = heading.trim();
    let number: String = text
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let numbered = !number.is_empty()
        && number
            .split('.')
            .all(|part| part.len() <= 2 && part.chars().all(|c| c.is_ascii_digit()));
    let rest = text[number.len()..].trim_start_matches([')', ':', '.', ' ', '\u{2013}', '-']);
    let number = number.trim_end_matches('.');
    if numbered && !number.is_empty() && !rest.is_empty() {
        format!("\u{a7}{number} {}", rest.trim())
    } else {
        text.to_owned()
    }
}

fn summarize(rows: &[BlockRow], old: &[Item], new: &[Item]) -> SectionSummary {
    struct Section {
        label: String,
        status: BlockStatus,
        /// Content (non-heading, non-question) blocks changed.
        dirty: bool,
    }
    let mut sections: Vec<Section> = Vec::new();
    let mut intro_dirty = false;
    // Questions are reported by id, apart from the section they sit in.
    let mut questions: [Vec<String>; 3] = Default::default();
    for row in rows {
        let item = match (row.new, row.old) {
            (Some(ix), _) => &new[ix],
            (None, Some(ix)) => &old[ix],
            (None, None) => continue,
        };
        match &item.tag {
            Tag::Heading => sections.push(Section {
                // A renamed heading shows its new name.
                label: section_label(item.heading.as_deref().unwrap_or("")),
                status: row.status,
                dirty: false,
            }),
            Tag::Question(id) => {
                let name = id.clone().unwrap_or_else(|| "question".to_owned());
                match row.status {
                    BlockStatus::Same => {}
                    BlockStatus::Changed => questions[0].push(name),
                    BlockStatus::Added => questions[1].push(name),
                    BlockStatus::Removed => questions[2].push(name),
                }
            }
            _ if row.status != BlockStatus::Same => match sections.last_mut() {
                Some(section) => section.dirty = true,
                None => intro_dirty = true,
            },
            _ => {}
        }
    }
    let mut summary = SectionSummary::default();
    if intro_dirty {
        summary.changed.push("(intro)".to_owned());
    }
    for section in sections {
        match section.status {
            BlockStatus::Added => summary.added.push(section.label),
            BlockStatus::Removed => summary.removed.push(section.label),
            BlockStatus::Changed => summary.changed.push(section.label),
            BlockStatus::Same if section.dirty => summary.changed.push(section.label),
            BlockStatus::Same => {}
        }
    }
    let [changed, added, removed] = questions;
    summary.changed.extend(changed);
    summary.added.extend(added);
    summary.removed.extend(removed);
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "\
# Plan

Intro paragraph.

## 1. Goals

Ship the thing.

## 2. Data model

Tables: parts, lots.

## 3. Rollout

Week one.

> [!question] Q-3 \u{b7} Merge by MPN?
> status: open
";

    #[test]
    fn identical_documents_have_no_changes() {
        let diff = diff_blocks(OLD, OLD);
        assert_eq!(diff.changed_blocks(), 0);
        assert!(diff.summary.is_empty());
        assert_eq!(diff.summary.text(), "No changes");
        assert!(diff.new_status.iter().all(|s| *s == BlockStatus::Same));
    }

    #[test]
    fn whitespace_only_edits_are_not_changes() {
        let reflowed = OLD.replace("Ship the thing.\n", "Ship the thing.\n\n\n\n");
        assert_eq!(diff_blocks(OLD, &reflowed).changed_blocks(), 0);
    }

    #[test]
    fn changed_sections_and_added_questions_are_summarised() {
        let new = OLD
            .replace("Tables: parts, lots.", "Tables: parts, lots, bins.")
            .replace("Week one.", "Week one, then week two.")
            + "\n> [!question] Q-7 \u{b7} Who owns the backfill?\n> status: open\n";
        let diff = diff_blocks(OLD, &new);
        assert_eq!(
            diff.summary.text(),
            "changed: \u{a7}2 Data model, \u{a7}3 Rollout; added: Q-7"
        );
        // Two paragraphs changed + one question added.
        assert_eq!(diff.changed_blocks(), 3);
        assert_eq!(
            diff.new_status
                .iter()
                .filter(|s| **s == BlockStatus::Added)
                .count(),
            1
        );
        assert_eq!(
            diff.old_status
                .iter()
                .filter(|s| **s == BlockStatus::Changed)
                .count(),
            2
        );
    }

    #[test]
    fn added_removed_sections_and_changed_questions() {
        let new = OLD
            .replace("## 1. Goals\n\nShip the thing.\n\n", "")
            .replace("status: open", "status: resolved")
            .replace(
                "## 3. Rollout",
                "## 2.5 Risks\n\nSlow backfill.\n\n## 3. Rollout",
            );
        let diff = diff_blocks(OLD, &new);
        assert_eq!(
            diff.summary.text(),
            "changed: Q-3; added: \u{a7}2.5 Risks; removed: \u{a7}1 Goals"
        );
        assert!(diff.old_status.contains(&BlockStatus::Removed));
        assert!(diff.new_status.contains(&BlockStatus::Added));
    }

    #[test]
    fn section_labels_number_headings() {
        assert_eq!(section_label("3. Data model"), "\u{a7}3 Data model");
        assert_eq!(section_label("3.2 Fields"), "\u{a7}3.2 Fields");
        assert_eq!(section_label("Rollout"), "Rollout");
        assert_eq!(section_label("2024 plan"), "2024 plan");
    }

    #[test]
    fn preview_indices_line_up() {
        let diff = diff_blocks(OLD, OLD);
        assert_eq!(diff.old_status.len(), parser::parse_full(OLD).blocks.len());
        assert_eq!(diff.rows.len(), diff.old_status.len());
    }
}
