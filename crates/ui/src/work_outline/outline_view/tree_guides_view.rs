//! Painting the Outline's tree guides, the move-aware drop indicator, and
//! the keyboard-move flash. Geometry lives in `work_outline::tree_guides`;
//! this file only turns it into absolutely positioned 1px divs inside each
//! row's left padding (no row height change, no horizontal shift).
//!
//! Debug selectors (headless tests):
//! - `outline-guide-{key}-{column}`: a guide cell's hit area in row `key`.
//! - `outline-guide-arm-{key}`: the elbow arm into row `key`.
//! - `outline-guide-lit-{key}-{column}`: the cell's upper segment when lit
//!   (selection path, hovered subtree, or move flash).
//! - `outline-drop-label`: the drag label next to the drop indicator.
//! - `outline-move-flash-{key}`: the flashed row after a keyboard move.

use super::*;
use crate::work_outline::tree_guides::{
    self, CellHighlight, GuideCell, GuideHighlight, GuideKind, TreeGuides,
};

/// How long a keyboard-moved row and its new parent's guide flash.
pub(crate) const MOVE_FLASH: Duration = Duration::from_millis(400);

/// Guide state cached beside the flattened model.
#[derive(Debug, Clone, Default)]
pub(crate) struct GuideState {
    /// Recomputed with every flatten (O(rows)).
    guides: TreeGuides,
    /// Row key of the parent whose guide the pointer is over.
    hovered: Option<String>,
    /// Row key flashing after a keyboard move.
    flash: Option<String>,
    flash_seq: u64,
}

/// Where a selected row sat before a keyboard move: `(key, index, parent
/// key)`.
pub(super) type MoveAnchor = Option<(String, usize, Option<String>)>;

/// Which highlight lights a cell (strongest first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lit {
    Flash,
    Active,
    Hover,
}

fn row_title(row: &OutlineRow) -> Option<&str> {
    match &row.kind {
        OutlineRowKind::Item(item) => Some(&item.title),
        OutlineRowKind::Space { label, .. } | OutlineRowKind::Unassociated { label, .. } => {
            Some(label)
        }
        OutlineRowKind::Chat(chat) => Some(&chat.chat.title),
        OutlineRowKind::Notice { .. } => None,
    }
}

impl OutlineView {
    /// Recompute the guides for the current model (called on every flatten).
    pub(super) fn refresh_guides(&mut self) {
        self.guide_state.guides = TreeGuides::compute(&self.model);
    }

    /// The cached guides (tests).
    pub(crate) fn tree_guides(&self) -> &TreeGuides {
        &self.guide_state.guides
    }

    /// The row flashing after a keyboard move (tests).
    pub(crate) fn move_flash_key(&self) -> Option<&str> {
        self.guide_state.flash.as_deref()
    }

    fn hover_guide(&mut self, ancestor: &str, hovered: bool, cx: &mut Context<Self>) {
        let next = if hovered {
            Some(ancestor.to_owned())
        } else if self.guide_state.hovered.as_deref() == Some(ancestor) {
            None
        } else {
            return;
        };
        if self.guide_state.hovered != next {
            self.guide_state.hovered = next;
            cx.notify();
        }
    }

    /// The selected row before a keyboard move.
    pub(super) fn move_anchor(&self) -> MoveAnchor {
        let key = self.selected.clone()?;
        let ix = self.model.position(&key)?;
        let parent = self.model.rows[ix]
            .parent
            .map(|p| self.model.rows[p].key.clone());
        Some((key, ix, parent))
    }

    /// After a keyboard move: when the selected row landed somewhere else,
    /// flash it and its new parent's guide for [`MOVE_FLASH`] (skipped with
    /// reduced motion).
    pub(super) fn flash_if_moved(&mut self, before: MoveAnchor, cx: &mut Context<Self>) {
        let Some((key, ix, parent)) = before else {
            return;
        };
        let Some((now_key, now_ix, now_parent)) = self.move_anchor() else {
            return;
        };
        if now_key != key || (now_ix == ix && now_parent == parent) {
            return;
        }
        if crate::motion::reduced_motion(cx) {
            return;
        }
        self.guide_state.flash_seq += 1;
        let seq = self.guide_state.flash_seq;
        self.guide_state.flash = Some(key);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MOVE_FLASH).await;
            this.update(cx, |this, cx| {
                if this.guide_state.flash_seq == seq {
                    this.guide_state.flash = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The label an in-progress drag shows next to its indicator.
    pub(crate) fn drop_label(&self) -> Option<String> {
        let hint = self.drop_hint.as_ref()?;
        let row = self.model.row(&hint.key)?;
        let title = row_title(row).unwrap_or_default();
        let parent = match row.kind {
            OutlineRowKind::Space { .. } => None,
            _ => Some(
                row.parent
                    .and_then(|p| row_title(&self.model.rows[p]))
                    .unwrap_or("top level"),
            ),
        };
        Some(tree_guides::drop_label(hint.zone, title, parent))
    }

    /// The highlight sources for this paint, strongest first.
    fn guide_highlights(&self) -> Vec<(Lit, GuideHighlight)> {
        let guides = &self.guide_state.guides;
        let mut out = Vec::new();
        let ix_of = |key: &Option<String>| key.as_deref().and_then(|k| self.model.position(k));
        if let Some(flash) = ix_of(&self.guide_state.flash) {
            if let Some(parent) = self.model.rows[flash].parent {
                out.push((Lit::Flash, GuideHighlight::subtree(guides, parent)));
            }
        }
        if let Some(selected) = ix_of(&self.selected) {
            out.push((
                Lit::Active,
                GuideHighlight::active_path(&self.model, guides, selected),
            ));
        }
        if let Some(hovered) = ix_of(&self.guide_state.hovered) {
            out.push((Lit::Hover, GuideHighlight::subtree(guides, hovered)));
        }
        out
    }

    /// The guides painted in row `ix`'s left padding.
    pub(super) fn guide_layer(
        &self,
        ix: usize,
        compact: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let row = self.model.rows.get(ix)?;
        let cells = self.guide_state.guides.cells(ix);
        if cells.is_empty() {
            return None;
        }
        let highlights = self.guide_highlights();
        // The row's content box (inside its 1px border).
        let height = Self::row_height(compact) - 2.0;
        let mid = (height / 2.0).floor();
        let base = if tree_guides::dim_guides(row) {
            theme.border.opacity(0.55)
        } else {
            theme.border
        };
        let lit_color = |lit: Lit| match lit {
            Lit::Flash => theme.accent,
            Lit::Active => theme.accent.opacity(0.55),
            Lit::Hover => theme.accent.opacity(0.35),
        };
        let key = &row.key;
        // The arm stops short of a disclosure triangle, or reaches into an
        // empty disclosure slot.
        let arm_end = tree_guides::indent(row.depth) + if row.has_children { -1.0 } else { 4.0 };
        let mut layer = div().absolute().top_0().left_0().size_0();
        for cell in cells {
            let lit =
                highlights
                    .iter()
                    .find_map(|(lit, highlight)| match highlight.cell(ix, cell) {
                        CellHighlight::None => None,
                        level => Some((*lit, level)),
                    });
            let upper = lit.map_or(base, |(lit, _)| lit_color(lit));
            let lower = match lit {
                Some((lit, CellHighlight::Full)) => lit_color(lit),
                _ => base,
            };
            layer = layer.child(self.guide_cell(
                key,
                cell,
                mid,
                height,
                arm_end,
                upper,
                lower,
                lit.is_some(),
                compact,
                cx,
            ));
        }
        Some(layer.into_any_element())
    }

    #[allow(clippy::too_many_arguments)]
    fn guide_cell(
        &self,
        key: &str,
        cell: &GuideCell,
        mid: f32,
        height: f32,
        arm_end: f32,
        upper: gpui::Hsla,
        lower: gpui::Hsla,
        lit: bool,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let x = tree_guides::guide_x(cell.column);
        let selector = format!("outline-guide-{key}-{}", cell.column);
        let ancestor = self.model.rows[cell.ancestor].key.clone();
        // The hit area is 7px wide, centred on the 1px line, and runs across
        // the rows' 1px borders so lines join up. Child y = row y + 1.
        let full = height + 2.0;
        let elbow = mid + 1.0;
        let segment = |top: f32, bottom: f32, color: gpui::Hsla| {
            div()
                .absolute()
                .left(px(3.0))
                .top(px(top))
                .w(px(1.0))
                .h(px(bottom - top))
                .bg(color)
        };
        let upper_end = match cell.kind {
            GuideKind::Continue => full,
            GuideKind::Tee | GuideKind::Last => elbow + 1.0,
        };
        let mut upper_line = segment(0.0, upper_end, upper);
        if lit {
            let lit_selector = format!("outline-guide-lit-{key}-{}", cell.column);
            upper_line = upper_line.debug_selector(move || lit_selector.clone());
        }
        let lower_line = (cell.kind == GuideKind::Tee).then(|| segment(elbow + 1.0, full, lower));
        let arm = (cell.kind != GuideKind::Continue).then(|| {
            let arm_selector = format!("outline-guide-arm-{key}");
            let min = if compact { 3.0 } else { 4.0 };
            div()
                .debug_selector(move || arm_selector.clone())
                .absolute()
                .left(px(3.0))
                .top(px(elbow))
                .w(px((arm_end - x).max(min)))
                .h(px(1.0))
                .bg(upper)
        });
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .absolute()
            .left(px(x - 3.0))
            .top(px(-1.0))
            .w(px(7.0))
            .h(px(full))
            .child(upper_line)
            .children(lower_line)
            .children(arm)
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.hover_guide(&ancestor, *hovered, cx);
            }))
            .into_any_element()
    }

    /// The before / after insertion line for row `ix`: it starts at the
    /// destination parent's guide (the target's parent), not the row edge.
    pub(super) fn drop_line(&self, ix: usize, zone: DropZone, theme: &Theme) -> AnyElement {
        let row = &self.model.rows[ix];
        let left = match row.parent {
            Some(parent) => tree_guides::guide_x(self.model.rows[parent].depth),
            None => tree_guides::indent(row.depth),
        };
        let edge = if zone == DropZone::Before {
            "before"
        } else {
            "after"
        };
        let selector = format!("outline-drop-{edge}-{}", row.key);
        div()
            .debug_selector(move || selector.clone())
            .absolute()
            .left(px(left))
            .right(px(4.0))
            .h(px(2.0))
            .rounded(px(1.0))
            .bg(theme.accent)
            .when(zone == DropZone::Before, |el| el.top(px(-1.0)))
            .when(zone == DropZone::After, |el| el.bottom(px(-1.0)))
            .into_any_element()
    }

    /// "Into X" / "Before X (under Y)" beside the indicator on row `ix`.
    pub(super) fn drop_label_element(&self, theme: &Theme) -> Option<AnyElement> {
        let text = self.drop_label()?;
        Some(
            div()
                .debug_selector(|| "outline-drop-label".into())
                .absolute()
                .right(px(6.0))
                .top(px(2.0))
                .max_w(px(220.0))
                .px(px(5.0))
                .rounded(px(3.0))
                .border_1()
                .border_color(theme.accent)
                .bg(theme.surface_raised)
                .text_size(crate::typography::ui_rems(10.0))
                .text_color(theme.accent)
                .truncate()
                .child(SharedString::from(text))
                .into_any_element(),
        )
    }

    /// The flashed row's wash after a keyboard move.
    pub(super) fn move_flash_overlay(&self, key: &str, theme: &Theme) -> Option<AnyElement> {
        (self.guide_state.flash.as_deref() == Some(key)).then(|| {
            let selector = format!("outline-move-flash-{key}");
            div()
                .debug_selector(move || selector.clone())
                .absolute()
                .inset_0()
                .rounded(px(4.0))
                .bg(theme.accent_wash)
                .into_any_element()
        })
    }
}
