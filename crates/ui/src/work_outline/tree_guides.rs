//! Pure tree-guide geometry for the Outline rows.
//!
//! The Outline is a virtualized `uniform_list`, so nesting lines are drawn
//! per row rather than as one overlay. [`TreeGuides::compute`] walks the
//! flattened [`OutlineModel`] once (O(rows)) and records, for every row, the
//! guide cells to paint in that row:
//!
//! - one cell per ancestor *column* (an ancestor's depth), aligned under that
//!   ancestor's disclosure triangle;
//! - the parent's column holds an elbow: `Tee` (├) when the row has a later
//!   sibling, `Last` (└) when it is the last child;
//! - every older ancestor's column holds a `Continue` line (│) only while the
//!   path child of that ancestor still has later siblings below.
//!
//! Highlights (selection's active path, hovered guide) are resolved per
//! painted row by [`GuideHighlight`], which is O(depth) to build and O(1) per
//! cell, so selection changes never recompute the guides.
use std::ops::Range;

use super::model::{OutlineModel, OutlineRow, OutlineRowKind};
use super::restructure::DropZone;

/// How long a keyboard-moved row and its new parent's guide flash.
pub(crate) const MOVE_FLASH: std::time::Duration = std::time::Duration::from_millis(400);

/// Horizontal step per depth level, in px (matches the row indent).
pub(crate) const INDENT_STEP: f32 = 14.0;
/// Left padding before depth 0, in px.
pub(crate) const INDENT_BASE: f32 = 8.0;
/// Width of the disclosure triangle slot that leads each row, in px.
pub(crate) const DISCLOSURE_WIDTH: f32 = 14.0;

/// Left edge of a row's content (its disclosure slot) at `depth`.
pub(crate) fn indent(depth: u16) -> f32 {
    INDENT_BASE + depth as f32 * INDENT_STEP
}

/// The x (row-local, px) of the vertical guide for an ancestor at `depth`:
/// centred under that ancestor's disclosure triangle.
pub(crate) fn guide_x(depth: u16) -> f32 {
    (indent(depth) + DISCLOSURE_WIDTH / 2.0).floor()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuideKind {
    /// │ — an older ancestor's line passing through.
    Continue,
    /// ├ — elbow into a row that has later siblings.
    Tee,
    /// └ — elbow into the last child.
    Last,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GuideCell {
    /// The ancestor row this guide belongs to.
    pub ancestor: usize,
    /// The ancestor's depth (the guide's column).
    pub column: u16,
    pub kind: GuideKind,
}

/// How a highlight applies to one guide cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CellHighlight {
    None,
    /// The whole cell (vertical line, plus the elbow arm if any).
    Full,
    /// Only the upper half and the elbow arm: the path turns into this row,
    /// so the line continuing below is not part of it.
    Elbow,
}

/// Per-row guide cells for one flattened model, plus subtree extents.
#[derive(Debug, Clone, Default)]
pub(crate) struct TreeGuides {
    /// `cells[offsets[ix]..offsets[ix + 1]]` are row `ix`'s cells, ordered by
    /// column (outermost first).
    cells: Vec<GuideCell>,
    offsets: Vec<u32>,
    /// Exclusive end index of each row's visible subtree.
    subtree_end: Vec<u32>,
    /// Whether each row has a later visible sibling.
    has_next_sibling: Vec<bool>,
}

impl TreeGuides {
    /// One reverse pass (later siblings, subtree ends) and one forward pass
    /// (ancestor stack) over the rows: O(rows + total cells).
    pub(crate) fn compute(model: &OutlineModel) -> Self {
        Self::from_rows(&model.rows)
    }

    pub(crate) fn from_rows(rows: &[OutlineRow]) -> Self {
        let n = rows.len();
        let mut has_next_sibling = vec![false; n];
        let mut subtree_end = vec![0u32; n];
        // Reverse pass. `child_seen[p]`: some later row already has parent p.
        let mut child_seen = vec![false; n];
        let mut root_seen = false;
        for ix in (0..n).rev() {
            let seen = match rows[ix].parent {
                Some(p) if p < n => &mut child_seen[p],
                _ => &mut root_seen,
            };
            has_next_sibling[ix] = *seen;
            *seen = true;
        }
        // Subtree ends: a row's subtree ends where the next row that is not
        // its descendant starts. Walk forward with an ancestor stack.
        let mut stack: Vec<usize> = Vec::new();
        let mut cells = Vec::new();
        let mut offsets = Vec::with_capacity(n + 1);
        for ix in 0..n {
            let parent = rows[ix].parent.filter(|p| *p < ix);
            // Pop finished subtrees.
            while let Some(&top) = stack.last() {
                if Some(top) == parent {
                    break;
                }
                subtree_end[top] = ix as u32;
                stack.pop();
            }
            if parent.is_none() {
                for top in stack.drain(..) {
                    subtree_end[top] = ix as u32;
                }
            }
            offsets.push(cells.len() as u32);
            // Stack now holds the ancestor chain, root first, parent last.
            if let Some((&parent_ix, older)) = stack.split_last() {
                for (pos, &ancestor) in older.iter().enumerate() {
                    let path_child = stack[pos + 1];
                    if has_next_sibling[path_child] {
                        cells.push(GuideCell {
                            ancestor,
                            column: rows[ancestor].depth,
                            kind: GuideKind::Continue,
                        });
                    }
                }
                cells.push(GuideCell {
                    ancestor: parent_ix,
                    column: rows[parent_ix].depth,
                    kind: if has_next_sibling[ix] {
                        GuideKind::Tee
                    } else {
                        GuideKind::Last
                    },
                });
            }
            stack.push(ix);
        }
        for top in stack {
            subtree_end[top] = n as u32;
        }
        offsets.push(cells.len() as u32);
        Self {
            cells,
            offsets,
            subtree_end,
            has_next_sibling,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.subtree_end.len()
    }

    /// Row `ix`'s guide cells, outermost column first.
    pub(crate) fn cells(&self, ix: usize) -> &[GuideCell] {
        match (self.offsets.get(ix), self.offsets.get(ix + 1)) {
            (Some(&start), Some(&end)) => &self.cells[start as usize..end as usize],
            _ => &[],
        }
    }

    /// Rows strictly below `ix` that belong to its visible subtree.
    pub(crate) fn descendants(&self, ix: usize) -> Range<usize> {
        match self.subtree_end.get(ix) {
            Some(&end) => ix + 1..(end as usize).max(ix + 1),
            None => 0..0,
        }
    }

    pub(crate) fn has_next_sibling(&self, ix: usize) -> bool {
        self.has_next_sibling.get(ix).copied().unwrap_or(false)
    }
}

/// The label beside a drop indicator: "Into X", or "Before X (under Y)" /
/// "After X (under Y)"; `parent` is `None` where no parent applies (Space
/// roots reorder among themselves).
pub(crate) fn drop_label(zone: DropZone, target: &str, parent: Option<&str>) -> String {
    let edge = match zone {
        DropZone::Into => return format!("Into {target}"),
        DropZone::Before => "Before",
        DropZone::After => "After",
    };
    match parent {
        Some(parent) => format!("{edge} {target} (under {parent})"),
        None => format!("{edge} {target}"),
    }
}

/// Whether a row's guides render dimmer (Chat rows and their children).
pub(crate) fn dim_guides(row: &OutlineRow) -> bool {
    matches!(row.kind, OutlineRowKind::Chat(_))
}

/// The ancestor chain of a focus row, resolved once per paint.
#[derive(Debug, Clone, Default)]
pub(crate) struct GuideHighlight {
    /// `(ancestor, path child)`: guides of `ancestor` are lit from the
    /// ancestor down to the elbow into `path child`.
    path: Vec<(usize, usize)>,
    /// The focus row's own subtree span (its guide column is fully lit).
    subtree_root: Option<usize>,
    subtree: Range<usize>,
}

impl GuideHighlight {
    /// The selection's active path: every ancestor's guide down to the
    /// selected row, plus the selected row's own subtree span.
    pub(crate) fn active_path(model: &OutlineModel, guides: &TreeGuides, focus: usize) -> Self {
        let mut path = Vec::new();
        if focus >= model.rows.len() {
            return Self::default();
        }
        let mut child = focus;
        while let Some(parent) = model.rows[child].parent {
            if parent >= child {
                break;
            }
            path.push((parent, child));
            child = parent;
        }
        Self {
            path,
            subtree_root: Some(focus),
            subtree: guides.descendants(focus),
        }
    }

    /// Only `root`'s subtree span (the hovered guide's parent).
    pub(crate) fn subtree(guides: &TreeGuides, root: usize) -> Self {
        Self {
            path: Vec::new(),
            subtree_root: Some(root),
            subtree: guides.descendants(root),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.path.is_empty() && self.subtree_root.is_none()
    }

    /// How row `row`'s `cell` is lit.
    pub(crate) fn cell(&self, row: usize, cell: &GuideCell) -> CellHighlight {
        if self.subtree_root == Some(cell.ancestor) && self.subtree.contains(&row) {
            return CellHighlight::Full;
        }
        for &(ancestor, path_child) in &self.path {
            if ancestor != cell.ancestor {
                continue;
            }
            if row < path_child {
                return CellHighlight::Full;
            }
            if row == path_child {
                return match cell.kind {
                    GuideKind::Last => CellHighlight::Full,
                    _ => CellHighlight::Elbow,
                };
            }
        }
        CellHighlight::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_outline::model::OutlineRowKind;
    use std::time::Instant;

    fn row(key: &str, depth: u16, parent: Option<usize>, kind: OutlineRowKind) -> OutlineRow {
        OutlineRow {
            key: key.to_string(),
            depth,
            parent,
            has_children: false,
            collapsed: false,
            child_count: 0,
            kind,
        }
    }

    fn unassoc(key: &str, depth: u16, parent: Option<usize>) -> OutlineRow {
        row(
            key,
            depth,
            parent,
            OutlineRowKind::Unassociated {
                space_id: None,
                label: key.to_string(),
            },
        )
    }

    /// Builds rows from `(depth, key)` pairs, deriving parents from depth.
    fn tree(spec: &[(u16, &str)]) -> Vec<OutlineRow> {
        let mut rows: Vec<OutlineRow> = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for (ix, &(depth, key)) in spec.iter().enumerate() {
            stack.truncate(depth as usize);
            let parent = stack.last().copied();
            rows.push(unassoc(key, depth, parent));
            stack.push(ix);
        }
        for ix in 0..rows.len() {
            if let Some(p) = rows[ix].parent {
                rows[p].has_children = true;
                rows[p].child_count += 1;
            }
        }
        rows
    }

    fn render(guides: &TreeGuides, rows: &[OutlineRow]) -> Vec<String> {
        (0..rows.len())
            .map(|ix| {
                let mut line = vec![' '; rows[ix].depth as usize];
                for cell in guides.cells(ix) {
                    line[cell.column as usize] = match cell.kind {
                        GuideKind::Continue => '|',
                        GuideKind::Tee => '+',
                        GuideKind::Last => '`',
                    };
                }
                format!("{}{}", line.into_iter().collect::<String>(), rows[ix].key)
            })
            .collect()
    }

    fn three_level() -> Vec<OutlineRow> {
        tree(&[
            (0, "A"),
            (1, "A1"),
            (2, "A1a"),
            (2, "A1b"),
            (1, "A2"),
            (2, "A2a"),
            (0, "B"),
            (1, "B1"),
        ])
    }

    #[test]
    fn three_level_tree_tees_lasts_and_continuations() {
        let rows = three_level();
        let guides = TreeGuides::from_rows(&rows);
        assert_eq!(
            render(&guides, &rows),
            vec![
                "A", "+A1", "|+A1a", "|`A1b", "`A2", " `A2a", // A2 is last: no line
                "B", "`B1",
            ]
        );
        assert!(guides.has_next_sibling(0), "A has later root sibling B");
        assert!(!guides.has_next_sibling(6));
        assert_eq!(guides.descendants(0), 1..6);
        assert_eq!(guides.descendants(1), 2..4);
        assert_eq!(guides.descendants(5), 6..6);
        assert_eq!(guides.descendants(6), 7..8);
        // Continuation cells name the right ancestor.
        let cont = guides.cells(2)[0];
        assert_eq!((cont.ancestor, cont.column), (0, 0));
        assert_eq!(cont.kind, GuideKind::Continue);
    }

    #[test]
    fn chats_get_guides_and_dim() {
        let chat = |key: &str, depth, parent| {
            row(
                key,
                depth,
                Some(parent),
                OutlineRowKind::Chat(crate::work_outline::model::ChatRow {
                    chat: crate::work_outline::model::OutlineChat {
                        id: key.to_string(),
                        space_id: None,
                        parent_chat_id: None,
                        title: key.to_string(),
                        status: zeron_proto::ChatIndicator::Idle,
                        archived: false,
                        stale: false,
                        branch: None,
                        harness: None,
                        pr_number: None,
                        visible: true,
                        activity: 0,
                    },
                    work_item: None,
                    dangling: false,
                }),
            )
        };
        let rows = vec![
            unassoc("Unassociated", 0, None),
            chat("c1", 1, 0),
            chat("c1-child", 2, 1),
            chat("c2", 1, 0),
        ];
        let guides = TreeGuides::from_rows(&rows);
        assert_eq!(
            render(&guides, &rows),
            vec!["Unassociated", "+c1", "|`c1-child", "`c2"]
        );
        assert!(dim_guides(&rows[1]));
        assert!(!dim_guides(&rows[0]));
    }

    #[test]
    fn selection_highlights_ancestor_chain_and_subtree() {
        let rows = three_level();
        let mut model = OutlineModel::default();
        model.rows = rows;
        let guides = TreeGuides::compute(&model);
        // Select A1 (ix 1): A's column lit down to the elbow into A1 (upper
        // half only, it is a Tee); A1's own column lit over A1a/A1b.
        let lit = GuideHighlight::active_path(&model, &guides, 1);
        let at = |row: usize, column: u16| {
            let cell = guides
                .cells(row)
                .iter()
                .find(|c| c.column == column)
                .copied()
                .unwrap();
            lit.cell(row, &cell)
        };
        assert_eq!(at(1, 0), CellHighlight::Elbow);
        assert_eq!(at(2, 1), CellHighlight::Full);
        assert_eq!(at(3, 1), CellHighlight::Full);
        // A's continuation past A1 (rows 2..4) and the elbow into A2 are not
        // on the path.
        assert_eq!(at(2, 0), CellHighlight::None);
        assert_eq!(at(4, 0), CellHighlight::None);
        assert_eq!(at(5, 1), CellHighlight::None);

        // Select A2a (ix 5): A's column lit from A1 through the └ into A2,
        // A2's └ into A2a fully lit.
        let lit = GuideHighlight::active_path(&model, &guides, 5);
        let at = |row: usize, column: u16| {
            let cell = guides
                .cells(row)
                .iter()
                .find(|c| c.column == column)
                .copied()
                .unwrap();
            lit.cell(row, &cell)
        };
        assert_eq!(at(1, 0), CellHighlight::Full);
        assert_eq!(at(2, 0), CellHighlight::Full);
        assert_eq!(at(4, 0), CellHighlight::Full);
        assert_eq!(at(5, 1), CellHighlight::Full);
        assert_eq!(at(2, 1), CellHighlight::None);
        assert_eq!(at(7, 0), CellHighlight::None);

        // Hovering A's guide lights A's whole subtree span in column 0.
        let hover = GuideHighlight::subtree(&guides, 0);
        let cell = guides.cells(3)[0];
        assert_eq!(hover.cell(3, &cell), CellHighlight::Full);
        let cell = guides.cells(3)[1];
        assert_eq!(hover.cell(3, &cell), CellHighlight::None);
    }

    #[test]
    fn drop_labels_name_target_and_parent() {
        assert_eq!(
            drop_label(DropZone::Into, "API", Some("Launch")),
            "Into API"
        );
        assert_eq!(
            drop_label(DropZone::Before, "UI", Some("Launch v2")),
            "Before UI (under Launch v2)"
        );
        assert_eq!(
            drop_label(DropZone::After, "API", Some("top level")),
            "After API (under top level)"
        );
        assert_eq!(
            drop_label(DropZone::After, "Space B", None),
            "After Space B"
        );
    }

    #[test]
    fn guide_x_sits_under_parent_disclosure() {
        assert_eq!(guide_x(0), 15.0);
        assert_eq!(guide_x(1) - guide_x(0), INDENT_STEP);
        assert!(
            guide_x(2) < indent(3),
            "guide stays left of the child's content"
        );
    }

    #[test]
    fn twenty_thousand_rows_within_budget() {
        // 20k rows: 400 roots × (10 children × (4 grandchildren)).
        let mut spec: Vec<(u16, String)> = Vec::new();
        for r in 0..400 {
            spec.push((0, format!("r{r}")));
            for c in 0..10 {
                spec.push((1, format!("r{r}c{c}")));
                for g in 0..4 {
                    spec.push((2, format!("r{r}c{c}g{g}")));
                }
            }
        }
        spec.truncate(20_000);
        let spec_ref: Vec<(u16, &str)> = spec.iter().map(|(d, k)| (*d, k.as_str())).collect();
        let rows = tree(&spec_ref);
        let start = Instant::now();
        let guides = TreeGuides::from_rows(&rows);
        let elapsed = start.elapsed();
        assert_eq!(guides.len(), 20_000);
        assert!(
            elapsed.as_millis() < 50,
            "guides for 20k rows took {elapsed:?}"
        );
    }

    /// 20k work items through the real flatten, then the guides: both stay
    /// well inside a frame budget (debug build, generous bound).
    #[test]
    fn twenty_thousand_item_flatten_and_guides_within_budget() {
        use crate::work_outline::model::{OutlineOptions, OutlineSpace, flatten};
        use crate::work_outline::store::{WorkOutlineLoad, fixture_snapshot};
        use std::collections::HashSet;
        use std::sync::Arc;

        let mut snapshot = fixture_snapshot("s1");
        let mut template = snapshot.items[0].clone();
        template.chat_ids.clear();
        template.external_reference_ids.clear();
        template.archived_at = None;
        let item = |id: String, parent: Option<String>, children: Vec<String>, depth| {
            let mut item = template.clone();
            item.id = id.clone();
            item.title = id;
            item.parent_work_item_id = parent.clone();
            item.raw_parent_work_item_id = parent;
            item.child_ids = children;
            item.depth = depth;
            item
        };
        let mut items = Vec::new();
        let mut roots = Vec::new();
        for r in 0..400 {
            let root = format!("r{r}");
            let kids: Vec<String> = (0..10).map(|c| format!("{root}c{c}")).collect();
            items.push(item(root.clone(), None, kids.clone(), 0));
            for kid in kids {
                let grand: Vec<String> = (0..4).map(|g| format!("{kid}g{g}")).collect();
                items.push(item(kid.clone(), Some(root.clone()), grand.clone(), 1));
                for g in grand {
                    items.push(item(g, Some(kid.clone()), Vec::new(), 2));
                }
            }
            roots.push(root);
        }
        snapshot.items = items;
        snapshot.root_work_item_ids = roots;
        snapshot.assignments.clear();
        let spaces = vec![OutlineSpace {
            id: "s1".into(),
            label: "Space".into(),
            load: WorkOutlineLoad::Ready(Arc::new(snapshot)),
            hidden: false,
        }];
        let start = Instant::now();
        let model = flatten(&spaces, &[], &HashSet::new(), &OutlineOptions::default());
        let flattened = start.elapsed();
        let guides = TreeGuides::compute(&model);
        let total = start.elapsed();
        assert!(model.len() >= 20_000, "{} rows", model.len());
        assert_eq!(guides.len(), model.len());
        assert!(
            (total - flattened).as_millis() < 50,
            "guides took {:?}",
            total - flattened
        );
        assert!(total.as_millis() < 1_000, "flatten + guides took {total:?}");
    }
}
