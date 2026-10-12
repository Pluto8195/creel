//! Outliner-style restructuring: move up / down among siblings, indent,
//! outdent, and drag-and-drop targets. Pure planning from a snapshot to one
//! `moveWorkItem` request (destination parent + placement); the Outline view
//! sends it through the drift confirmation. No GUI types here.
//!
//! Siblings are the live (non-archived) items under the same effective
//! parent, in outline (materialization) order. Keyboard moves only step over
//! siblings the Outline currently shows (`shown`), so a press always moves
//! the item past a row the user can see; the request names that neighbor, so
//! hidden rows keep their place deterministically.
use std::collections::HashSet;

use super::model;
use super::store::{WorkItemView, WorkOutlinePlacement, WorkOutlineSnapshot};

/// A keyboard / palette restructuring of the selected work item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestructureOp {
    /// Before the previous sibling.
    MoveUp,
    /// After the next sibling.
    MoveDown,
    /// Last child of the previous sibling.
    Indent,
    /// To the parent's parent, right after the former parent.
    Outdent,
}

/// One `moveWorkItem` request. Descendants always move with the item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MovePlan {
    pub work_item_id: String,
    /// `None` = the Space root.
    pub destination_parent: Option<String>,
    pub placement: WorkOutlinePlacement,
}

impl MovePlan {
    pub(crate) fn params(&self, operation_id: String) -> zeron_proto::MoveWorkItemParams {
        zeron_proto::MoveWorkItemParams {
            work_item_id: self.work_item_id.clone(),
            destination_parent_work_item_id: self.destination_parent.clone(),
            placement: self.placement.clone(),
            operation_id,
        }
    }
}

/// Where on a target row a drag is released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DropZone {
    /// Upper edge band: before the target, same parent.
    Before,
    /// Middle: last child of the target.
    Into,
    /// Lower edge band: after the target, same parent.
    After,
}

/// Fraction of the row height that counts as the upper / lower edge band.
pub(crate) const EDGE_BAND: f32 = 0.25;

/// The drop zone for a pointer `offset_y` pixels below the top of a row
/// `height` pixels tall.
pub(crate) fn drop_zone(offset_y: f32, height: f32) -> DropZone {
    if height <= 0.0 {
        return DropZone::Into;
    }
    let ratio = offset_y / height;
    if ratio < EDGE_BAND {
        DropZone::Before
    } else if ratio > 1.0 - EDGE_BAND {
        DropZone::After
    } else {
        DropZone::Into
    }
}

fn live(item: &WorkItemView) -> bool {
    item.archived_at.is_none()
}

/// Live items under `parent`, in outline order (the order the Outline
/// renders: `root_work_item_ids` / `child_ids`).
fn live_children<'a>(
    snapshot: &'a WorkOutlineSnapshot,
    parent: Option<&str>,
) -> Vec<&'a WorkItemView> {
    let ids: &[String] = match parent {
        None => &snapshot.root_work_item_ids,
        Some(parent) => match snapshot.item(parent) {
            Some(item) => &item.child_ids,
            None => return Vec::new(),
        },
    };
    ids.iter()
        .filter_map(|id| snapshot.item(id))
        .filter(|item| live(item) && item.parent_work_item_id.as_deref() == parent)
        .collect()
}

/// The keyboard / palette move for `item_id`, or `None` when it is a no-op
/// (first / last sibling, nothing to indent under, already top level) or the
/// item is not a live item of `snapshot`. `shown(id)` says whether the
/// Outline currently shows a sibling row.
pub(crate) fn plan(
    snapshot: &WorkOutlineSnapshot,
    item_id: &str,
    op: RestructureOp,
    shown: impl Fn(&str) -> bool,
) -> Option<MovePlan> {
    let item = snapshot.item(item_id).filter(|item| live(item))?;
    let parent = item.parent_work_item_id.clone();
    let siblings: Vec<&WorkItemView> = live_children(snapshot, parent.as_deref())
        .into_iter()
        .filter(|sibling| sibling.id == item_id || shown(&sibling.id))
        .collect();
    let ix = siblings.iter().position(|sibling| sibling.id == item_id)?;
    let previous = ix.checked_sub(1).map(|i| siblings[i]);
    let next = siblings.get(ix + 1).copied();
    let plan = |destination_parent: Option<String>, placement| MovePlan {
        work_item_id: item_id.to_string(),
        destination_parent,
        placement,
    };
    match op {
        RestructureOp::MoveUp => previous.map(|sibling| {
            plan(
                parent.clone(),
                WorkOutlinePlacement::Before {
                    sibling_work_item_id: sibling.id.clone(),
                },
            )
        }),
        RestructureOp::MoveDown => next.map(|sibling| {
            plan(
                parent.clone(),
                WorkOutlinePlacement::After {
                    sibling_work_item_id: sibling.id.clone(),
                },
            )
        }),
        RestructureOp::Indent => {
            previous.map(|sibling| plan(Some(sibling.id.clone()), WorkOutlinePlacement::Last))
        }
        RestructureOp::Outdent => {
            let former = snapshot.item(parent.as_deref()?)?;
            Some(plan(
                former.parent_work_item_id.clone(),
                WorkOutlinePlacement::After {
                    sibling_work_item_id: former.id.clone(),
                },
            ))
        }
    }
}

/// Why [`plan`] returned `None` for `item_id` (diagnostics only; no titles).
pub(crate) fn noop_reason(
    snapshot: &WorkOutlineSnapshot,
    item_id: &str,
    op: RestructureOp,
    shown: impl Fn(&str) -> bool,
) -> &'static str {
    let Some(item) = snapshot.item(item_id) else {
        return "item not in snapshot";
    };
    if !live(item) {
        return "item archived";
    }
    let parent = item.parent_work_item_id.as_deref();
    let all = live_children(snapshot, parent);
    if !all.iter().any(|sibling| sibling.id == item_id) {
        return "item missing from its parent's child list";
    }
    let siblings: Vec<&WorkItemView> = all
        .into_iter()
        .filter(|sibling| sibling.id == item_id || shown(&sibling.id))
        .collect();
    let ix = siblings
        .iter()
        .position(|sibling| sibling.id == item_id)
        .unwrap_or_default();
    match op {
        RestructureOp::MoveUp | RestructureOp::Indent if ix == 0 => {
            "first shown sibling (nothing above)"
        }
        RestructureOp::MoveDown if ix + 1 >= siblings.len() => "last shown sibling (nothing below)",
        RestructureOp::Outdent if parent.is_none() => "already top level",
        RestructureOp::Outdent => "parent not in snapshot",
        _ => "unknown",
    }
}

/// The move a drop of `moving` onto `target` in `zone` makes, or `None`
/// when the drop is refused: onto itself or one of its descendants, an
/// archived item on either side, an item of another Space, or a drop that
/// would leave the item where it is.
pub(crate) fn drop_plan(
    snapshot: &WorkOutlineSnapshot,
    moving: &str,
    target: &str,
    zone: DropZone,
) -> Option<MovePlan> {
    if moving == target {
        return None;
    }
    let item = snapshot.item(moving).filter(|item| live(item))?;
    let target_item = snapshot.item(target).filter(|item| live(item))?;
    let descendants: HashSet<String> = model::descendants(snapshot, moving);
    if descendants.contains(target) {
        return None;
    }
    let plan = match zone {
        DropZone::Into => MovePlan {
            work_item_id: moving.to_string(),
            destination_parent: Some(target.to_string()),
            placement: WorkOutlinePlacement::Last,
        },
        DropZone::Before => MovePlan {
            work_item_id: moving.to_string(),
            destination_parent: target_item.parent_work_item_id.clone(),
            placement: WorkOutlinePlacement::Before {
                sibling_work_item_id: target.to_string(),
            },
        },
        DropZone::After => MovePlan {
            work_item_id: moving.to_string(),
            destination_parent: target_item.parent_work_item_id.clone(),
            placement: WorkOutlinePlacement::After {
                sibling_work_item_id: target.to_string(),
            },
        },
    };
    (!is_noop(snapshot, item, &plan)).then_some(plan)
}

/// Whether `plan` would leave `item` exactly where it is.
fn is_noop(snapshot: &WorkOutlineSnapshot, item: &WorkItemView, plan: &MovePlan) -> bool {
    if plan.destination_parent != item.parent_work_item_id {
        return false;
    }
    let siblings = live_children(snapshot, item.parent_work_item_id.as_deref());
    let Some(ix) = siblings.iter().position(|sibling| sibling.id == item.id) else {
        return false;
    };
    let id_at = |i: Option<usize>| i.and_then(|i| siblings.get(i)).map(|s| s.id.as_str());
    match &plan.placement {
        WorkOutlinePlacement::First => ix == 0,
        WorkOutlinePlacement::Last => ix + 1 == siblings.len(),
        WorkOutlinePlacement::Before {
            sibling_work_item_id,
        } => id_at(Some(ix + 1)) == Some(sibling_work_item_id.as_str()),
        WorkOutlinePlacement::After {
            sibling_work_item_id,
        } => id_at(ix.checked_sub(1)) == Some(sibling_work_item_id.as_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::store::fixture_ids::*;
    use super::super::store::fixture_snapshot;
    use super::*;

    fn all(_: &str) -> bool {
        true
    }

    fn before(id: &str) -> WorkOutlinePlacement {
        WorkOutlinePlacement::Before {
            sibling_work_item_id: id.into(),
        }
    }

    fn after(id: &str) -> WorkOutlinePlacement {
        WorkOutlinePlacement::After {
            sibling_work_item_id: id.into(),
        }
    }

    fn moved(item: &str, parent: Option<&str>, placement: WorkOutlinePlacement) -> MovePlan {
        MovePlan {
            work_item_id: item.into(),
            destination_parent: parent.map(Into::into),
            placement,
        }
    }

    #[test]
    fn move_up_and_down_name_the_neighbor_and_stop_at_the_ends() {
        let s = fixture_snapshot("s1");
        // Launch v2 > [API, UI]; roots [Launch v2, Housekeeping] (+ archived).
        assert_eq!(
            plan(&s, UI, RestructureOp::MoveUp, all),
            Some(moved(UI, Some(LAUNCH), before(API)))
        );
        assert_eq!(
            plan(&s, API, RestructureOp::MoveDown, all),
            Some(moved(API, Some(LAUNCH), after(UI)))
        );
        assert_eq!(plan(&s, API, RestructureOp::MoveUp, all), None);
        assert_eq!(plan(&s, UI, RestructureOp::MoveDown, all), None);
        // The archived root is not a sibling: Housekeeping is last.
        assert_eq!(plan(&s, HOUSEKEEPING, RestructureOp::MoveDown, all), None);
        assert_eq!(
            plan(&s, HOUSEKEEPING, RestructureOp::MoveUp, all),
            Some(moved(HOUSEKEEPING, None, before(LAUNCH)))
        );
        // Archived items never move from the keyboard.
        assert_eq!(plan(&s, RETIRED, RestructureOp::MoveUp, all), None);
    }

    #[test]
    fn hidden_siblings_are_stepped_over() {
        let s = fixture_snapshot("s1");
        // UI hidden (for example by a filter): API has no visible next sibling.
        let shown = |id: &str| id != UI;
        assert_eq!(plan(&s, API, RestructureOp::MoveDown, shown), None);
    }

    #[test]
    fn indent_makes_the_last_child_of_the_previous_sibling() {
        let s = fixture_snapshot("s1");
        assert_eq!(
            plan(&s, UI, RestructureOp::Indent, all),
            Some(moved(UI, Some(API), WorkOutlinePlacement::Last))
        );
        assert_eq!(
            plan(&s, HOUSEKEEPING, RestructureOp::Indent, all),
            Some(moved(
                HOUSEKEEPING,
                Some(LAUNCH),
                WorkOutlinePlacement::Last
            ))
        );
        assert_eq!(plan(&s, API, RestructureOp::Indent, all), None);
        assert_eq!(plan(&s, LAUNCH, RestructureOp::Indent, all), None);
    }

    #[test]
    fn outdent_lands_after_the_former_parent() {
        let s = fixture_snapshot("s1");
        assert_eq!(
            plan(&s, API_AUTH, RestructureOp::Outdent, all),
            Some(moved(API_AUTH, Some(LAUNCH), after(API)))
        );
        assert_eq!(
            plan(&s, API, RestructureOp::Outdent, all),
            Some(moved(API, None, after(LAUNCH)))
        );
        assert_eq!(plan(&s, LAUNCH, RestructureOp::Outdent, all), None);
    }

    #[test]
    fn drop_zones_split_the_row_into_edge_bands() {
        assert_eq!(drop_zone(1.0, 24.0), DropZone::Before);
        assert_eq!(drop_zone(12.0, 24.0), DropZone::Into);
        assert_eq!(drop_zone(23.0, 24.0), DropZone::After);
        assert_eq!(drop_zone(5.0, 0.0), DropZone::Into);
    }

    #[test]
    fn drops_onto_items_and_edges_plan_moves_and_refuse_cycles() {
        let s = fixture_snapshot("s1");
        assert_eq!(
            drop_plan(&s, HOUSEKEEPING, API, DropZone::Into),
            Some(moved(HOUSEKEEPING, Some(API), WorkOutlinePlacement::Last))
        );
        assert_eq!(
            drop_plan(&s, HOUSEKEEPING, UI, DropZone::Before),
            Some(moved(HOUSEKEEPING, Some(LAUNCH), before(UI)))
        );
        assert_eq!(
            drop_plan(&s, HOUSEKEEPING, API, DropZone::After),
            Some(moved(HOUSEKEEPING, Some(LAUNCH), after(API)))
        );
        // Self and descendants are refused.
        assert_eq!(drop_plan(&s, LAUNCH, LAUNCH, DropZone::Into), None);
        assert_eq!(drop_plan(&s, LAUNCH, API_AUTH, DropZone::Into), None);
        assert_eq!(drop_plan(&s, LAUNCH, API, DropZone::Before), None);
        // Archived either side.
        assert_eq!(drop_plan(&s, HOUSEKEEPING, RETIRED, DropZone::Into), None);
        assert_eq!(drop_plan(&s, RETIRED, API, DropZone::Into), None);
        // Drops that leave the item in place.
        assert_eq!(drop_plan(&s, API, UI, DropZone::Before), None);
        assert_eq!(drop_plan(&s, UI, API, DropZone::After), None);
        assert_eq!(drop_plan(&s, API_AUTH, API, DropZone::Into), None);
        assert_eq!(
            drop_plan(&s, UI, API, DropZone::Before),
            Some(moved(UI, Some(LAUNCH), before(API)))
        );
    }
}
