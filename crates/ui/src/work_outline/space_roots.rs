//! Space-root management in a multi-Space Outline: the order of the Space
//! roots and the Spaces hidden from the Outline.
//!
//! Both are device-local view state, never registry writes: the order is an
//! ordered list of Space ids ([`overview_positions::OUTLINE_SPACE_ORDER_FILE`])
//! and the hidden set a string set
//! ([`overview_positions::OUTLINE_HIDDEN_SPACES_FILE`]), saved the way the
//! collapse set is. Changes apply at once (nothing to wait for).
//!
//! - Order: Spaces in the list come first, in list order; any other Space
//!   (a new one) follows alphabetically. Every write stores the full current
//!   order, which drops Spaces that no longer exist.
//! - Hidden: a hidden Space renders nothing in the Outline (List, Canvas and
//!   the sidebar still show it and its Chats). The toolbar's "Hidden
//!   projects (N)" shows hidden Spaces as dimmed root rows with Unhide.
//! - Only with more than one Space: a single-Space Outline has no root rows.
//!
//! A child module of `outline_view` so it can update the Outline's state.
use std::collections::BTreeSet;

use gpui::Context;

use super::super::model::{OutlineRowKind, OutlineSpace, row_key};
use super::super::restructure::DropZone;
use super::{DropHint, OutlineDrag, OutlineEvent, OutlineView};
use crate::overview_positions;

/// `spaces` (alphabetical, as the Overview delivers them) in the user's
/// order, with the hidden flags applied. Ids in `order` that are not in
/// `spaces` are ignored.
pub(crate) fn arrange(
    spaces: &[OutlineSpace],
    order: &[String],
    hidden: &BTreeSet<String>,
) -> Vec<OutlineSpace> {
    let mut out: Vec<OutlineSpace> = spaces
        .iter()
        .cloned()
        .map(|mut space| {
            space.hidden = hidden.contains(&space.id);
            space
        })
        .collect();
    // Stable: unlisted Spaces keep their alphabetical order, after the rest.
    out.sort_by_key(|space| {
        order
            .iter()
            .position(|id| *id == space.id)
            .unwrap_or(usize::MAX)
    });
    out
}

/// The full order after moving `moving` before (`after == false`) or after
/// `target`; `None` when that changes nothing or either id is unknown.
pub(crate) fn place(
    all: &[String],
    moving: &str,
    target: &str,
    after: bool,
) -> Option<Vec<String>> {
    if moving == target || !all.iter().any(|id| id == moving) {
        return None;
    }
    let mut out: Vec<String> = all.iter().filter(|id| *id != moving).cloned().collect();
    let at = out.iter().position(|id| id == target)? + usize::from(after);
    out.insert(at, moving.to_string());
    (out != all).then_some(out)
}

/// The full order after moving `moving` one step up or down past the
/// neighbouring *shown* Space root (`shown`: the Space rows on screen, in
/// order); `None` at either end.
pub(crate) fn step(
    all: &[String],
    shown: &[String],
    moving: &str,
    up: bool,
) -> Option<Vec<String>> {
    let ix = shown.iter().position(|id| id == moving)?;
    let neighbour = if up {
        shown.get(ix.checked_sub(1)?)?
    } else {
        shown.get(ix + 1)?
    };
    place(all, moving, neighbour, !up)
}

impl OutlineView {
    /// Load the saved order and hidden set (with the collapse set).
    pub(super) fn load_space_prefs(&mut self, data_dir: &std::path::Path) {
        self.space_order = overview_positions::load_string_list(
            data_dir,
            overview_positions::OUTLINE_SPACE_ORDER_FILE,
        );
        self.hidden_spaces = overview_positions::load_string_set(
            data_dir,
            overview_positions::OUTLINE_HIDDEN_SPACES_FILE,
        );
    }

    /// The Spaces as the Outline shows them: the user's order, hidden flags.
    pub(super) fn arranged_spaces(&self) -> Vec<OutlineSpace> {
        arrange(&self.inputs.spaces, &self.space_order, &self.hidden_spaces)
    }

    /// Every Space id in display order (hidden ones included).
    fn space_ids_in_order(&self) -> Vec<String> {
        self.arranged_spaces()
            .into_iter()
            .map(|space| space.id)
            .collect()
    }

    /// The Space root rows on screen, in order.
    fn shown_space_roots(&self) -> Vec<String> {
        self.model
            .rows
            .iter()
            .filter_map(|row| match &row.kind {
                OutlineRowKind::Space { space_id, .. } => Some(space_id.clone()),
                _ => None,
            })
            .collect()
    }

    /// The Space of the selected Space root row.
    pub(super) fn selected_space_root(&self) -> Option<String> {
        match &self.selected_row()?.kind {
            OutlineRowKind::Space { space_id, .. } => Some(space_id.clone()),
            _ => None,
        }
    }

    /// Why Move Up / Move Down would do nothing for Space root `space_id`.
    pub(crate) fn space_move_blocked(&self, space_id: &str, up: bool) -> Option<&'static str> {
        step(
            &self.space_ids_in_order(),
            &self.shown_space_roots(),
            space_id,
            up,
        )
        .is_none()
        .then_some(if up {
            "Already the first project"
        } else {
            "Already the last project"
        })
    }

    /// ⇧↑ / ⇧↓ (and the menu's Move Up / Down) on a Space root: swap it
    /// with the neighbouring shown root. Nothing at either end.
    pub(crate) fn move_space(&mut self, space_id: &str, up: bool, cx: &mut Context<Self>) {
        let order = step(
            &self.space_ids_in_order(),
            &self.shown_space_roots(),
            space_id,
            up,
        );
        tracing::info!(
            target: "work_outline_keys",
            %space_id, up, moved = order.is_some(),
            "space root move"
        );
        if let Some(order) = order {
            self.set_space_order(order, cx);
        }
    }

    /// Store `order` (the full order of every current Space) and show it.
    fn set_space_order(&mut self, order: Vec<String>, cx: &mut Context<Self>) {
        self.space_order = order;
        if let Some(data_dir) = self.state.read(cx).data_dir.clone() {
            if let Err(err) = overview_positions::save_string_list(
                &data_dir,
                overview_positions::OUTLINE_SPACE_ORDER_FILE,
                &self.space_order,
            ) {
                tracing::warn!(error = %err, "saving outline space order failed");
            }
        }
        self.reflatten();
        if let Some(ix) = self
            .selected
            .as_deref()
            .and_then(|key| self.model.position(key))
        {
            self.scroll
                .scroll_to_item(ix, gpui::ScrollStrategy::Nearest);
        }
        cx.emit(OutlineEvent::SpacesArranged);
        cx.notify();
    }

    /// Hide (or unhide) Space `space_id` in the Outline.
    pub(crate) fn set_space_hidden(
        &mut self,
        space_id: &str,
        hidden: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = if hidden {
            self.hidden_spaces.insert(space_id.to_string())
        } else {
            self.hidden_spaces.remove(space_id)
        };
        if !changed {
            return;
        }
        // Prune Spaces that no longer exist.
        let present: BTreeSet<&str> = self.inputs.spaces.iter().map(|s| s.id.as_str()).collect();
        self.hidden_spaces
            .retain(|id| present.is_empty() || present.contains(id.as_str()));
        if let Some(data_dir) = self.state.read(cx).data_dir.clone() {
            if let Err(err) = overview_positions::save_string_set(
                &data_dir,
                overview_positions::OUTLINE_HIDDEN_SPACES_FILE,
                &self.hidden_spaces,
            ) {
                tracing::warn!(error = %err, "saving hidden outline spaces failed");
            }
        }
        let label = self
            .inputs
            .spaces
            .iter()
            .find(|space| space.id == space_id)
            .map(|space| space.label.clone())
            .unwrap_or_else(|| "Project".into());
        if hidden && self.selected.as_deref() == Some(row_key::space(space_id).as_str()) {
            // Keep the keyboard somewhere useful when the row goes away.
            self.select_past(&row_key::space(space_id), cx);
        }
        self.reflatten();
        if self.model.hidden_spaces == 0 {
            self.show_hidden_spaces = false;
            self.reflatten();
        }
        cx.emit(OutlineEvent::SpacesArranged);
        if hidden {
            self.show_notice(
                format!(
                    "Hid \u{201c}{label}\u{201d} from the Outline. Hidden projects shows it again."
                ),
                cx,
            );
        } else {
            cx.notify();
        }
    }

    /// Spaces hidden from the Outline (the toolbar count; 0 with one Space).
    pub(crate) fn hidden_space_count(&self) -> usize {
        self.model.hidden_spaces
    }

    pub(crate) fn showing_hidden_spaces(&self) -> bool {
        self.show_hidden_spaces
    }

    /// The toolbar's "Hidden projects (N)": show hidden Spaces as dimmed root
    /// rows (with Unhide), or stop showing them.
    pub(crate) fn set_show_hidden_spaces(&mut self, show: bool, cx: &mut Context<Self>) {
        if self.show_hidden_spaces == show {
            return;
        }
        self.show_hidden_spaces = show;
        self.reflatten();
        cx.emit(OutlineEvent::SpacesArranged);
        cx.notify();
    }

    /// The pointer moved during a drag over Space root row `key`: a Space
    /// drag shows a before / after line (upper / lower half); anything else
    /// is refused here.
    pub(super) fn drag_over_space(
        &mut self,
        key: &str,
        target: &str,
        drag: &OutlineDrag,
        position: gpui::Point<gpui::Pixels>,
        bounds: gpui::Bounds<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let hint = match drag {
            OutlineDrag::Space { space_id, .. } if bounds.contains(&position) => {
                let after =
                    f32::from(position.y - bounds.top()) > f32::from(bounds.size.height) / 2.0;
                place(&self.space_ids_in_order(), space_id, target, after).map(|_| DropHint {
                    key: key.to_string(),
                    zone: if after {
                        DropZone::After
                    } else {
                        DropZone::Before
                    },
                })
            }
            _ if self.drop_hint.as_ref().is_some_and(|hint| hint.key != key) => return,
            _ => None,
        };
        if self.drop_hint != hint {
            self.drop_hint = hint;
            cx.notify();
        }
    }

    /// A drag was released over Space root row `key`: place the dragged
    /// Space where the indicator showed.
    pub(super) fn drop_on_space(
        &mut self,
        key: &str,
        target: &str,
        drag: &OutlineDrag,
        cx: &mut Context<Self>,
    ) {
        let hint = self.drop_hint.take();
        cx.notify();
        let (Some(hint), OutlineDrag::Space { space_id, .. }) =
            (hint.filter(|hint| hint.key == key), drag)
        else {
            return;
        };
        let after = hint.zone == DropZone::After;
        if let Some(order) = place(&self.space_ids_in_order(), space_id, target, after) {
            self.set_space_order(order, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_outline::store::WorkOutlineLoad;

    fn space(id: &str) -> OutlineSpace {
        OutlineSpace {
            id: id.into(),
            label: id.to_uppercase(),
            load: WorkOutlineLoad::Loading,
            hidden: false,
        }
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn arrange_puts_listed_spaces_first_and_new_ones_alphabetically_after() {
        let spaces = [space("a"), space("b"), space("c"), space("d")];
        let hidden: BTreeSet<String> = ["b".to_string()].into();
        let out = arrange(&spaces, &ids(&["c", "gone", "a"]), &hidden);
        let order: Vec<&str> = out.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(order, ["c", "a", "b", "d"]);
        assert!(out.iter().find(|s| s.id == "b").unwrap().hidden);
        assert!(!out.iter().find(|s| s.id == "a").unwrap().hidden);
    }

    #[test]
    fn place_and_step_reorder_and_refuse_noops() {
        let all = ids(&["a", "b", "c"]);
        assert_eq!(place(&all, "c", "a", false), Some(ids(&["c", "a", "b"])));
        assert_eq!(place(&all, "a", "c", true), Some(ids(&["b", "c", "a"])));
        assert_eq!(place(&all, "a", "b", false), None, "already there");
        assert_eq!(place(&all, "a", "a", true), None);
        assert_eq!(place(&all, "x", "a", true), None);

        // Steps skip Spaces not on screen (hidden ones).
        let shown = ids(&["a", "c"]);
        assert_eq!(step(&all, &shown, "c", true), Some(ids(&["c", "a", "b"])));
        assert_eq!(step(&all, &shown, "a", false), Some(ids(&["b", "c", "a"])));
        assert_eq!(step(&all, &shown, "a", true), None);
        assert_eq!(step(&all, &shown, "c", false), None);
    }
}
