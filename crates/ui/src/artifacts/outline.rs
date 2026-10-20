//! The section index: a table of contents from a plan's `h1`–`h3` headings.
//!
//! Pure: extraction from HTML source and from a Markdown block tree, entry
//! ids for duplicate titles, the "fewer than three headings" rule, and which
//! section is in view. The tab renders it and drives the renderers.
use crate::markdown::parser::{Block, BlockTree};

/// Plans with fewer headings than this get no index.
pub(crate) const MIN_HEADINGS: usize = 3;

/// The deepest heading level the index lists.
pub(crate) const MAX_LEVEL: u8 = 3;

/// A heading as found in a document: every `h1`–`h3`, in document order,
/// empty ones included (renderers number headings by position, so an empty
/// heading still takes a slot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawHeading {
    pub(crate) level: u8,
    pub(crate) text: String,
    /// Where the renderer finds it: the top-level block index (Markdown) or
    /// the heading's position among `h1`–`h3` (HTML).
    pub(crate) target: usize,
}

/// One row of the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Section {
    /// Unique within the document (duplicate titles get `-1`, `-2`, …).
    pub(crate) id: String,
    pub(crate) level: u8,
    pub(crate) title: String,
    /// See [`RawHeading::target`].
    pub(crate) target: usize,
}

/// A keyboard move through the sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SectionStep {
    Previous,
    Next,
    First,
    Last,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SectionIndex {
    pub(crate) sections: Vec<Section>,
}

impl SectionIndex {
    /// Index the non-empty headings, with unique ids.
    pub(crate) fn from_headings(headings: &[RawHeading]) -> SectionIndex {
        let mut sections: Vec<Section> = Vec::new();
        for heading in headings {
            if heading.text.is_empty() || heading.level == 0 || heading.level > MAX_LEVEL {
                continue;
            }
            let slug = slug(&heading.text);
            let mut id = slug.clone();
            let mut n = 1;
            while sections.iter().any(|section| section.id == id) {
                id = format!("{slug}-{n}");
                n += 1;
            }
            sections.push(Section {
                id,
                level: heading.level,
                title: heading.text.clone(),
                target: heading.target,
            });
        }
        SectionIndex { sections }
    }

    /// Whether the plan has enough structure to be worth an index.
    pub(crate) fn is_shown(&self) -> bool {
        self.sections.len() >= MIN_HEADINGS
    }

    /// The shallowest level present, so a plan that starts at `h2` is not
    /// indented.
    pub(crate) fn base_level(&self) -> u8 {
        self.sections
            .iter()
            .map(|section| section.level)
            .min()
            .unwrap_or(1)
    }

    /// The section in view when the renderer reports `position`: the last
    /// section whose target is at or before it. Before the first heading,
    /// the first section.
    pub(crate) fn active_at(&self, position: usize) -> Option<usize> {
        if self.sections.is_empty() {
            return None;
        }
        Some(
            self.sections
                .iter()
                .rposition(|section| section.target <= position)
                .unwrap_or(0),
        )
    }

    /// Where a keyboard step lands. `from` is the section in view, `None`
    /// before the first heading. `None` out when there is nowhere to go
    /// (the ends do not wrap).
    pub(crate) fn step_target(&self, from: Option<usize>, step: SectionStep) -> Option<usize> {
        let last = self.sections.len().checked_sub(1)?;
        match step {
            SectionStep::First => Some(0),
            SectionStep::Last => Some(last),
            SectionStep::Next => match from {
                None => Some(0),
                Some(at) => (at < last).then_some(at + 1),
            },
            SectionStep::Previous => from?.checked_sub(1),
        }
    }

    /// The section whose target is exactly `target` (the page reports the
    /// heading index it is at).
    pub(crate) fn section_for_target(&self, target: usize) -> Option<usize> {
        self.sections
            .iter()
            .rposition(|section| section.target <= target)
            .or_else(|| (!self.sections.is_empty()).then_some(0))
    }
}

fn slug(text: &str) -> String {
    let slug: String = text
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        .map(|c| if c == ' ' { '-' } else { c })
        .collect();
    if slug.is_empty() {
        "section".to_owned()
    } else {
        slug
    }
}

/// Top-level `h1`–`h3` headings of a Markdown tree, targeted by block index
/// (the preview scrolls its block list).
pub(crate) fn markdown_headings(tree: &BlockTree) -> Vec<RawHeading> {
    tree.blocks
        .iter()
        .enumerate()
        .filter_map(|(ix, top)| match &top.block {
            Block::Heading { level, runs } if (1..=MAX_LEVEL).contains(level) => Some(RawHeading {
                level: *level,
                text: collapse(&runs.iter().map(|run| run.text.as_str()).collect::<String>()),
                target: ix,
            }),
            _ => None,
        })
        .collect()
}

/// `h1`–`h3` elements of an HTML document or fragment, in document order.
/// Comments, `<script>`, `<style>`, `<template>` and `<noscript>` content is
/// skipped; tags inside a heading are dropped from its text.
pub(crate) fn html_headings(source: &str) -> Vec<RawHeading> {
    const SKIPPED: [&str; 4] = ["script", "style", "template", "noscript"];
    let mut out = Vec::new();
    let mut rest = source;
    while let Some(lt) = rest.find('<') {
        rest = &rest[lt..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |end| &after[end + 3..]);
            continue;
        }
        let Some(gt) = tag_end(rest) else { break };
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];
        let name: String = tag
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if tag.starts_with('/') {
            continue;
        }
        if SKIPPED.contains(&name.as_str()) {
            let close = format!("</{name}");
            rest = find_ci(rest, &close).map_or("", |at| &rest[at..]);
            continue;
        }
        let level = match name.as_str() {
            "h1" => 1,
            "h2" => 2,
            "h3" => 3,
            _ => continue,
        };
        let close = format!("</h{level}");
        let end = find_ci(rest, &close).unwrap_or(rest.len());
        out.push(RawHeading {
            level,
            text: collapse(&decode_entities(&strip_tags(&rest[..end]))),
            target: out.len(),
        });
        rest = &rest[end..];
    }
    out
}

/// Byte offset of the `>` closing the tag that starts at `s[0] == '<'`,
/// honouring quoted attribute values.
fn tag_end(s: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices().skip(1) {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => return Some(i),
            _ => {}
        }
    }
    None
}

/// Case-insensitive ASCII `find`.
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        rest = &rest[lt..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |end| &after[end + 3..]);
            continue;
        }
        match tag_end(rest) {
            Some(gt) => rest = &rest[gt + 1..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

fn decode_entities(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest.find(';').filter(|end| *end <= 10).and_then(|end| {
            let name = &rest[1..end];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" => Some(' '),
                "ndash" => Some('\u{2013}'),
                "mdash" => Some('\u{2014}'),
                "hellip" => Some('\u{2026}'),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|n| n.parse().ok()))
                    .and_then(char::from_u32),
            };
            c.map(|c| (c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Trim and collapse runs of whitespace.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The headings to index for an HTML artifact: the source's, unless the live
/// page disagrees on how many there are (script-built headings), in which
/// case the page's own list is the truth.
pub(crate) fn reconcile_html(source: Vec<RawHeading>, live: &[(u8, String)]) -> Vec<RawHeading> {
    if live.is_empty() || live.len() == source.len() {
        return source;
    }
    live.iter()
        .enumerate()
        .map(|(target, (level, text))| RawHeading {
            level: *level,
            text: collapse(text),
            target,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::parser::parse_full;

    fn titles(index: &SectionIndex) -> Vec<(u8, &str)> {
        index
            .sections
            .iter()
            .map(|section| (section.level, section.title.as_str()))
            .collect()
    }

    #[test]
    fn html_headings_nest_h1_to_h3_and_skip_deeper_levels() {
        let html = "<h1>Plan</h1><p>x</p><h2 class=\"a\">Steps</h2><h3>One</h3><h4>Detail</h4><h2>Risks</h2>";
        let headings = html_headings(html);
        let index = SectionIndex::from_headings(&headings);
        assert_eq!(
            titles(&index),
            vec![(1, "Plan"), (2, "Steps"), (3, "One"), (2, "Risks")]
        );
        // Targets are positions among h1-h3 (the h4 does not count).
        assert_eq!(
            index.sections.iter().map(|s| s.target).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
    }

    #[test]
    fn html_headings_read_text_through_tags_and_entities() {
        let html = "<H2 id=x>Fish &amp; <em>Chips</em>\n   &#8212; &lt;now&gt;</H2>";
        let headings = html_headings(html);
        assert_eq!(headings[0].text, "Fish & Chips \u{2014} <now>");
    }

    #[test]
    fn html_headings_skip_comments_scripts_and_templates() {
        let html = "<!-- <h1>no</h1> --><script>var s='<h1>no</h1>'</script>\
<style>h1{}</style><template><h2>no</h2></template><h1>Yes</h1>\
<div title=\"a > b\"><h2>Also</h2></div>";
        let headings = html_headings(html);
        assert_eq!(
            headings.iter().map(|h| h.text.as_str()).collect::<Vec<_>>(),
            vec!["Yes", "Also"]
        );
    }

    #[test]
    fn html_headings_ignore_lookalike_tags() {
        // <header>, <hr>, <h> are not headings.
        let headings = html_headings("<header>x</header><hr><h>y</h><h2>Real</h2>");
        assert_eq!(headings.len(), 1);
        assert_eq!(headings[0].text, "Real");
    }

    #[test]
    fn empty_headings_take_a_slot_but_get_no_row() {
        let headings = html_headings("<h1>A</h1><h2></h2><h2>B</h2><h2> </h2><h2>C</h2>");
        assert_eq!(headings.len(), 5);
        let index = SectionIndex::from_headings(&headings);
        assert_eq!(titles(&index), vec![(1, "A"), (2, "B"), (2, "C")]);
        assert_eq!(
            index.sections.iter().map(|s| s.target).collect::<Vec<_>>(),
            vec![0, 2, 4]
        );
    }

    #[test]
    fn markdown_headings_use_top_level_blocks_and_levels_one_to_three() {
        let source = "# Plan\n\nIntro.\n\n## Steps\n\n```\n# not a heading\n```\n\n#### Deep\n\n### One\n\n> ## quoted\n";
        let tree = parse_full(source);
        let headings = markdown_headings(&tree);
        let index = SectionIndex::from_headings(&headings);
        assert_eq!(titles(&index), vec![(1, "Plan"), (2, "Steps"), (3, "One")]);
        // Targets are block indexes: heading, paragraph, heading, code, h4, h3.
        assert_eq!(
            index.sections.iter().map(|s| s.target).collect::<Vec<_>>(),
            vec![0, 2, 5]
        );
    }

    #[test]
    fn markdown_heading_text_flattens_inline_styles() {
        let tree = parse_full("## The `plan` is **bold**\n");
        let headings = markdown_headings(&tree);
        assert_eq!(headings[0].text, "The plan is bold");
    }

    #[test]
    fn duplicate_titles_get_unique_ids_and_both_rows() {
        let headings =
            html_headings("<h2>Risks</h2><h2>Risks</h2><h3>Risks</h3><h2>Next steps</h2>");
        let index = SectionIndex::from_headings(&headings);
        assert_eq!(index.sections.len(), 4);
        let ids: Vec<_> = index.sections.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["risks", "risks-1", "risks-2", "next-steps"]);
        // Each row still points at its own heading.
        assert_eq!(
            index.sections.iter().map(|s| s.target).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
    }

    #[test]
    fn symbol_only_titles_still_get_ids() {
        let index = SectionIndex::from_headings(&html_headings("<h2>???</h2><h2>!!!</h2>"));
        let ids: Vec<_> = index.sections.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["section", "section-1"]);
    }

    #[test]
    fn the_index_needs_three_headings() {
        let two = SectionIndex::from_headings(&html_headings("<h1>A</h1><h2>B</h2>"));
        assert!(!two.is_shown());
        let three = SectionIndex::from_headings(&html_headings("<h1>A</h1><h2>B</h2><h2>C</h2>"));
        assert!(three.is_shown());
        // Empty and h4+ headings do not count toward the three.
        let padded =
            SectionIndex::from_headings(&html_headings("<h1>A</h1><h2></h2><h4>x</h4><h2>B</h2>"));
        assert!(!padded.is_shown());
        assert!(!SectionIndex::default().is_shown());
        let md = SectionIndex::from_headings(&markdown_headings(&parse_full("# A\n\n## B\n")));
        assert!(!md.is_shown());
    }

    #[test]
    fn base_level_is_the_shallowest_present() {
        let index = SectionIndex::from_headings(&html_headings("<h2>A</h2><h3>B</h3><h2>C</h2>"));
        assert_eq!(index.base_level(), 2);
        assert_eq!(SectionIndex::default().base_level(), 1);
    }

    #[test]
    fn the_active_section_is_the_last_one_at_or_before_the_position() {
        let tree = parse_full("# A\n\ntext\n\n## B\n\ntext\n\ntext\n\n## C\n\ntext\n");
        let index = SectionIndex::from_headings(&markdown_headings(&tree));
        // Blocks: 0 A, 1 text, 2 B, 3 text, 4 text, 5 C, 6 text.
        assert_eq!(index.active_at(0), Some(0));
        assert_eq!(index.active_at(1), Some(0));
        assert_eq!(index.active_at(2), Some(1));
        assert_eq!(index.active_at(4), Some(1));
        assert_eq!(index.active_at(5), Some(2));
        assert_eq!(index.active_at(99), Some(2));
        assert_eq!(SectionIndex::default().active_at(3), None);
    }

    #[test]
    fn content_before_the_first_heading_highlights_the_first_section() {
        let tree = parse_full("intro\n\n# A\n\n## B\n\n## C\n");
        let index = SectionIndex::from_headings(&markdown_headings(&tree));
        assert_eq!(index.active_at(0), Some(0));
    }

    #[test]
    fn the_page_reports_a_heading_position_and_empty_ones_resolve_to_the_previous_row() {
        let headings = html_headings("<h1>A</h1><h2></h2><h2>B</h2>");
        let index = SectionIndex::from_headings(&headings);
        assert_eq!(index.section_for_target(0), Some(0));
        // The empty heading (position 1) is still inside section A.
        assert_eq!(index.section_for_target(1), Some(0));
        assert_eq!(index.section_for_target(2), Some(1));
    }

    #[test]
    fn live_headings_win_only_when_their_count_differs_from_the_source() {
        let source = html_headings("<h1>A</h1><h2>B</h2>");
        let live = vec![(1u8, "A".to_owned()), (2, "B".to_owned())];
        assert_eq!(reconcile_html(source.clone(), &live), source);
        // The page built a third heading with script.
        let live = vec![
            (1, "A".to_owned()),
            (2, "B".to_owned()),
            (2, " C ".to_owned()),
        ];
        let reconciled = reconcile_html(source.clone(), &live);
        assert_eq!(reconciled.len(), 3);
        assert_eq!(reconciled[2].text, "C");
        assert_eq!(reconciled[2].target, 2);
        // Nothing reported yet: the source stands.
        assert_eq!(reconcile_html(source.clone(), &[]), source);
    }

    #[test]
    fn steps_stop_at_both_ends_and_start_from_before_the_first_heading() {
        let index = SectionIndex::from_headings(&html_headings("<h1>A</h1><h2>B</h2><h2>C</h2>"));
        use SectionStep::*;
        assert_eq!(index.step_target(Some(1), Previous), Some(0));
        assert_eq!(index.step_target(Some(1), Next), Some(2));
        assert_eq!(index.step_target(Some(0), Previous), None);
        assert_eq!(index.step_target(Some(2), Next), None);
        assert_eq!(index.step_target(None, Next), Some(0));
        assert_eq!(index.step_target(None, Previous), None);
        assert_eq!(index.step_target(Some(1), First), Some(0));
        assert_eq!(index.step_target(Some(1), Last), Some(2));
        let empty = SectionIndex::default();
        assert_eq!(empty.step_target(None, Next), None);
        assert_eq!(empty.step_target(None, Last), None);
    }
}
