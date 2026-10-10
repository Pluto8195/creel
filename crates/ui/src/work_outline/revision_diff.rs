//! Generic text comparison presenter for instruction revisions (spec
//! "Respect existing appearance": the Git Changes diff is checkout-bound and
//! is not reused). It renders any `TextDiffLine` list (the
//! `compareInstructionRevisions` `lineDiff`) with the Changes pane's visual
//! vocabulary — theme `diff_add` / `diff_del` tints, an accent bar, dual line
//! numbers, a `+`/`−` marker, the code font and size — plus the entry-level
//! summary (changed / added / removed / reordered), which leads because entry
//! numbers in the rendered envelope shift when entries are inserted.
use gpui::{AnyElement, SharedString, div, prelude::*, px};
use zeron_proto::{InstructionRevisionComparison, TextDiffKind, TextDiffLine};

use crate::theme::Theme;

/// Unchanged lines kept around each change.
pub(crate) const CONTEXT_LINES: usize = 3;
/// Presented lines rendered at most (one stacked element; the rest is
/// summarized).
pub(crate) const MAX_PRESENTED_LINES: usize = 400;

/// One presented row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PresentedLine {
    Line {
        kind: TextDiffKind,
        old_no: Option<u32>,
        new_no: Option<u32>,
        text: String,
    },
    /// `n` unchanged lines folded away.
    Folded(usize),
}

/// Number the lines (old/new) and fold unchanged runs longer than
/// `2 * CONTEXT_LINES` down to their context. Pure.
pub(crate) fn present(lines: &[TextDiffLine]) -> Vec<PresentedLine> {
    let mut numbered = Vec::with_capacity(lines.len());
    let (mut old_no, mut new_no) = (0u32, 0u32);
    for line in lines {
        let (old, new) = match line.kind {
            TextDiffKind::Same => {
                old_no += 1;
                new_no += 1;
                (Some(old_no), Some(new_no))
            }
            TextDiffKind::Removed => {
                old_no += 1;
                (Some(old_no), None)
            }
            TextDiffKind::Added => {
                new_no += 1;
                (None, Some(new_no))
            }
        };
        numbered.push(PresentedLine::Line {
            kind: line.kind,
            old_no: old,
            new_no: new,
            text: line.text.clone(),
        });
    }
    let changed: Vec<bool> = lines.iter().map(|l| l.kind != TextDiffKind::Same).collect();
    if !changed.iter().any(|c| *c) {
        // Identical: show everything (it is the whole envelope).
        return numbered;
    }
    let keep: Vec<bool> = (0..lines.len())
        .map(|ix| {
            let lo = ix.saturating_sub(CONTEXT_LINES);
            let hi = (ix + CONTEXT_LINES).min(lines.len() - 1);
            changed[lo..=hi].iter().any(|c| *c)
        })
        .collect();
    let mut out = Vec::new();
    let mut folded = 0;
    for (line, keep) in numbered.into_iter().zip(keep) {
        if keep {
            if folded > 0 {
                out.push(PresentedLine::Folded(folded));
                folded = 0;
            }
            out.push(line);
        } else {
            folded += 1;
        }
    }
    if folded > 0 {
        out.push(PresentedLine::Folded(folded));
    }
    out
}

fn code_size(theme: &Theme) -> f32 {
    crate::typography::clamp_font_size(theme.code_font_size * 0.92)
}

fn line_row(line: &PresentedLine, theme: &Theme) -> AnyElement {
    let size = code_size(theme);
    let height = (size * 1.55).round();
    let PresentedLine::Line {
        kind,
        old_no,
        new_no,
        text,
    } = line
    else {
        let PresentedLine::Folded(count) = line else {
            unreachable!()
        };
        return div()
            .h(px(height))
            .pl(px(12.0))
            .flex()
            .items_center()
            .text_size(crate::typography::ui_rems(10.5))
            .text_color(theme.text_faint)
            .child(SharedString::from(format!(
                "\u{22ef} {count} unchanged line{}",
                if *count == 1 { "" } else { "s" }
            )))
            .into_any_element();
    };
    let (marker, color) = match kind {
        TextDiffKind::Added => ("+", Some(theme.diff_add)),
        TextDiffKind::Removed => ("\u{2212}", Some(theme.diff_del)),
        TextDiffKind::Same => ("\u{b7}", None),
    };
    let gutter = |no: Option<u32>| {
        div()
            .w(px(26.0))
            .flex_none()
            .flex()
            .justify_end()
            .pr(px(6.0))
            .text_color(color.map_or(theme.text_faint.opacity(0.8), |c| c.opacity(0.9)))
            .child(SharedString::from(
                no.map(|n| n.to_string()).unwrap_or_default(),
            ))
    };
    div()
        .min_h(px(height))
        .w_full()
        .flex()
        .items_start()
        .font_family(theme.font_mono.clone())
        .text_size(px(size))
        .line_height(px(height))
        .when_some(color, |el, color| {
            let mut wash = color;
            wash.a = 0.055;
            el.bg(wash)
        })
        .child(
            div()
                .w(px(2.0))
                .h(px(height))
                .flex_none()
                .when_some(color, |el, color| el.bg(color.opacity(0.55))),
        )
        .child(gutter(*old_no))
        .child(gutter(*new_no))
        .child(
            div()
                .w(px(12.0))
                .flex_none()
                .text_color(color.unwrap_or(theme.text_faint.opacity(0.5)))
                .child(marker),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(theme.text.opacity(0.92))
                .child(SharedString::from(text.clone())),
        )
        .into_any_element()
}

/// The stacked line diff (capped at [`MAX_PRESENTED_LINES`]).
pub(crate) fn render_text_diff(
    selector: &'static str,
    lines: &[TextDiffLine],
    theme: &Theme,
) -> AnyElement {
    let presented = present(lines);
    let hidden = presented.len().saturating_sub(MAX_PRESENTED_LINES);
    div()
        .debug_selector(move || selector.into())
        .w_full()
        .flex()
        .flex_col()
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.border)
        .overflow_hidden()
        .children(
            presented
                .iter()
                .take(MAX_PRESENTED_LINES)
                .map(|line| line_row(line, theme)),
        )
        .when(hidden > 0, |el| {
            el.child(
                div()
                    .px(px(12.0))
                    .py(px(2.0))
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(format!("{hidden} more lines not shown"))),
            )
        })
        .into_any_element()
}

fn summary_group(
    selector: String,
    label: String,
    rows: Vec<(Option<TextDiffKind>, String)>,
    theme: &Theme,
) -> AnyElement {
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .text_size(crate::typography::ui_rems(11.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text_muted)
                .child(SharedString::from(label)),
        )
        .children(rows.into_iter().map(|(kind, text)| {
            let color = match kind {
                Some(TextDiffKind::Added) => theme.diff_add,
                Some(TextDiffKind::Removed) => theme.diff_del,
                _ => theme.text,
            };
            div()
                .pl(px(8.0))
                .text_size(crate::typography::ui_rems(11.5))
                .text_color(color)
                .child(SharedString::from(text))
        }))
        .into_any_element()
}

/// Entry-level summary then the line diff of the rendered envelopes.
pub(crate) fn render_comparison(
    comparison: &InstructionRevisionComparison,
    theme: &Theme,
) -> AnyElement {
    let mut root = div()
        .debug_selector(|| "instruction-comparison".into())
        .flex()
        .flex_col()
        .gap(px(10.0));
    if comparison.identical {
        return root
            .child(
                div()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child("No differences"),
            )
            .into_any_element();
    }
    if !comparison.changed.is_empty() {
        root = root.child(summary_group(
            "instruction-comparison-changed".into(),
            format!("Changed ({})", comparison.changed.len()),
            comparison
                .changed
                .iter()
                .flat_map(|change| {
                    [
                        (Some(TextDiffKind::Removed), change.before_text.clone()),
                        (Some(TextDiffKind::Added), change.after_text.clone()),
                    ]
                })
                .collect(),
            theme,
        ));
    }
    if !comparison.added.is_empty() {
        root = root.child(summary_group(
            "instruction-comparison-added".into(),
            format!("Added ({})", comparison.added.len()),
            comparison
                .added
                .iter()
                .map(|entry| {
                    (
                        Some(TextDiffKind::Added),
                        format!("{} \u{b7} {}", entry.scope_title, entry.text),
                    )
                })
                .collect(),
            theme,
        ));
    }
    if !comparison.removed.is_empty() {
        root = root.child(summary_group(
            "instruction-comparison-removed".into(),
            format!("Removed ({})", comparison.removed.len()),
            comparison
                .removed
                .iter()
                .map(|entry| {
                    (
                        Some(TextDiffKind::Removed),
                        format!("{} \u{b7} {}", entry.scope_title, entry.text),
                    )
                })
                .collect(),
            theme,
        ));
    }
    if !comparison.reordered.is_empty() {
        root = root.child(
            div()
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_muted)
                .child(SharedString::from(format!(
                    "Reordered ({})",
                    comparison.reordered.len()
                ))),
        );
    }
    root.child(render_text_diff(
        "instruction-comparison-diff",
        &comparison.line_diff,
        theme,
    ))
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: TextDiffKind, text: &str) -> TextDiffLine {
        TextDiffLine {
            kind,
            text: text.into(),
        }
    }

    #[test]
    fn numbers_lines_and_folds_long_unchanged_runs() {
        let mut lines: Vec<TextDiffLine> = (0..10)
            .map(|i| line(TextDiffKind::Same, &format!("s{i}")))
            .collect();
        lines.push(line(TextDiffKind::Removed, "old"));
        lines.push(line(TextDiffKind::Added, "new"));
        lines.push(line(TextDiffKind::Same, "tail"));
        let presented = present(&lines);
        assert_eq!(presented[0], PresentedLine::Folded(7));
        assert_eq!(
            presented[4],
            PresentedLine::Line {
                kind: TextDiffKind::Removed,
                old_no: Some(11),
                new_no: None,
                text: "old".into()
            }
        );
        assert_eq!(
            presented[5],
            PresentedLine::Line {
                kind: TextDiffKind::Added,
                old_no: None,
                new_no: Some(11),
                text: "new".into()
            }
        );
        assert_eq!(presented.len(), 7);
    }

    #[test]
    fn identical_text_is_shown_unfolded() {
        let lines: Vec<_> = (0..12)
            .map(|i| line(TextDiffKind::Same, &format!("{i}")))
            .collect();
        assert_eq!(present(&lines).len(), 12);
        assert!(present(&[]).is_empty());
    }
}
