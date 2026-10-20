//! Find in a Markdown preview: the pure counting side.
//!
//! The renderer paints a wash over every match in each text element it draws
//! (`markdown::render`, `FindHighlight`). This module lists, per top-level
//! block, the plain text of every element the renderer will draw — splitting
//! exactly where it splits — so match counts here equal what gets painted.
//! A match never spans two elements.
//!
//! Mirrored renderer rules (markdown/render.rs, preview options: media and
//! task UI present, no workspace root):
//! - paragraph / heading: one element per run group between inline images
//!   (image runs are drawn as media, not text);
//! - list item: its blocks recursively; a task item's first paragraph omits
//!   the leading `[ ] ` / `[x] ` marker run (a checkbox is drawn instead);
//! - block quote: children recursively;
//! - table: header cells, then body cells, row by row, each cell separately
//!   (image runs split a cell like a paragraph);
//! - code block: one element per line; mermaid fences are diagrams and have
//!   none;
//! - rule: none.

use crate::{
    artifacts::find::find_ranges,
    markdown::{
        parser::{Block, BlockTree, InlineRun},
        render::is_mermaid,
    },
};

/// Plain text of each rendered text element of `block`, in paint order.
pub(crate) fn block_texts(block: &Block) -> Vec<String> {
    let mut out = Vec::new();
    push_block(block, &mut out);
    out
}

/// Text elements for one run list: runs between images, concatenated.
fn push_runs(runs: &[InlineRun], out: &mut Vec<String>) {
    let mut current = String::new();
    let mut open = false;
    for run in runs {
        if run.style.image.is_some() {
            if open {
                out.push(std::mem::take(&mut current));
                open = false;
            }
        } else {
            current.push_str(&run.text);
            open = true;
        }
    }
    if open {
        out.push(current);
    }
}

fn push_block(block: &Block, out: &mut Vec<String>) {
    match block {
        Block::Paragraph { runs } | Block::Heading { runs, .. } => push_runs(runs, out),
        Block::CodeBlock { language, code } => {
            if !is_mermaid(language.as_deref()) {
                out.extend(code.split('\n').map(str::to_owned));
            }
        }
        Block::BlockQuote { children } => {
            for child in children {
                push_block(child, out);
            }
        }
        Block::List { items, .. } => {
            for item in items {
                for (ix, child) in item.iter().enumerate() {
                    match child {
                        Block::Paragraph { runs }
                            if ix == 0
                                && runs.first().is_some_and(|run| run.style.task.is_some()) =>
                        {
                            push_runs(&runs[1..], out);
                        }
                        _ => push_block(child, out),
                    }
                }
            }
        }
        Block::Table { header, rows, .. } => {
            let header = std::iter::once(header).filter(|header| !header.is_empty());
            for row in header.chain(rows.iter()) {
                for cell in row {
                    push_runs(cell, out);
                }
            }
        }
        Block::Rule => {}
    }
}

/// Matches of `query` in each top-level block, in document order. An empty
/// query matches nothing.
pub(crate) fn block_match_counts(tree: &BlockTree, query: &str) -> Vec<usize> {
    tree.blocks
        .iter()
        .map(|top| {
            if query.is_empty() {
                return 0;
            }
            block_texts(&top.block)
                .iter()
                .map(|text| find_ranges(text, query).len())
                .sum()
        })
        .collect()
}

/// Which block holds match number `n` (0-based, document order), and which
/// match within that block's row it is. `None` when `n` is past the last
/// match.
pub(crate) fn locate(counts: &[usize], n: usize) -> Option<(usize, usize)> {
    let mut before = 0;
    for (block, count) in counts.iter().enumerate() {
        if n < before + count {
            return Some((block, n - before));
        }
        before += count;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::parser::parse_full;

    fn counts(source: &str, query: &str) -> Vec<usize> {
        block_match_counts(&parse_full(source), query)
    }

    #[test]
    fn counts_every_block_kind_case_insensitively() {
        let source = "\
# Plan Alpha

Alpha and ALPHA, then alpha.

- alpha item
- [ ] task with Alpha
  - nested ALPHA

> quoted Alpha
>
> - listed alpha

| Alpha | Beta |
| --- | --- |
| alpha | none |
| x | ALPHA alpha |

```rust
let alpha = 1;
// Alpha
```

---

Last paragraph without it.
";
        let tree = parse_full(source);
        let counts = block_match_counts(&tree, "alpha");
        assert_eq!(counts.len(), tree.blocks.len());
        assert_eq!(counts, vec![1, 3, 3, 2, 4, 2, 0, 0]);
        assert_eq!(counts.iter().sum::<usize>(), 15);
    }

    #[test]
    fn empty_query_matches_nothing() {
        assert!(counts("# a\n\nb\n", "").iter().all(|count| *count == 0));
        assert_eq!(counts("# a\n\nb\n", "").len(), 2);
    }

    #[test]
    fn task_marker_text_is_not_searchable() {
        assert_eq!(counts("- [ ] done\n- [x] x\n", "[ ]"), vec![0]);
        assert_eq!(counts("- [x] done\n", "done"), vec![1]);
    }

    #[test]
    fn matches_do_not_span_elements() {
        // Table cells, code lines and the runs around an image each keep
        // their own text.
        assert_eq!(
            counts("| ab | cd |\n| --- | --- |\n| e | f |\n", "bc"),
            vec![0]
        );
        assert_eq!(counts("```\nab\ncd\n```\n", "bc"), vec![0]);
        assert_eq!(counts("a ![img](x.png) b\n", "a b"), vec![0]);
        assert_eq!(counts("a ![img](x.png) b\n", "img"), vec![0]);
        assert_eq!(counts("a ![img](x.png) b\n", "a"), vec![1]);
    }

    #[test]
    fn mermaid_fences_have_no_text() {
        assert_eq!(
            counts("```mermaid\ngraph TD; alpha-->b\n```\n", "alpha"),
            vec![0]
        );
        assert_eq!(counts("```text\nalpha\n```\n", "alpha"), vec![1]);
    }

    #[test]
    fn inline_formatting_joins_into_one_element() {
        assert_eq!(counts("al**ph**a `x`\n", "alpha"), vec![1]);
    }

    #[test]
    fn locate_maps_global_index_to_block_and_nth() {
        let counts = [0, 2, 0, 3, 1];
        assert_eq!(locate(&counts, 0), Some((1, 0)));
        assert_eq!(locate(&counts, 1), Some((1, 1)));
        assert_eq!(locate(&counts, 2), Some((3, 0)));
        assert_eq!(locate(&counts, 4), Some((3, 2)));
        assert_eq!(locate(&counts, 5), Some((4, 0)));
        assert_eq!(locate(&counts, 6), None);
        assert_eq!(locate(&[], 0), None);
        assert_eq!(locate(&[0, 0], 0), None);
    }
}
