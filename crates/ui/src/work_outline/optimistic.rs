//! Optimistic local application of the user's own Work Outline changes.
//!
//! A move, rename, create or Chat association is applied to a copy of the
//! Space's last snapshot the moment the user makes it, so the row moves (and
//! the selection follows) in the same frame instead of after the
//! `WorkOutlineMutate` reply and the next `WatchWorkOutline` frame. The
//! engine stays the authority: the Outline drops a change once a frame shows
//! it (or after a timeout), and rolls it back when the mutation fails.
//!
//! Pure and GUI-free. [`apply`] mirrors what the doc layer writes for the
//! tree shape the Outline renders (`root_work_item_ids` / `child_ids`,
//! parent ids, depths, `chat_ids`, assignments); derived fields the engine
//! recomputes (rollups, effective status) catch up with the next frame.
//! Canvas positions are never involved.
use zeron_proto::{
    WorkItemChatAssignmentState, WorkItemChatAssignmentView, WorkItemEffectiveStatus,
    WorkItemRollup, WorkItemStatus, WorkItemView, WorkOutlinePlacement, WorkOutlineSnapshot,
};

/// One user-initiated change, as sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LocalChange {
    /// `moveWorkItem` (keyboard, drag and drop, the move picker).
    Move {
        work_item_id: String,
        /// `None` = the Space root.
        destination_parent: Option<String>,
        placement: WorkOutlinePlacement,
    },
    /// `updateWorkItem` with a new title.
    Rename { work_item_id: String, title: String },
    /// `createWorkItem`; `work_item_id` is the id the engine derives from the
    /// operation id.
    Create {
        work_item_id: String,
        parent: Option<String>,
        placement: WorkOutlinePlacement,
        title: String,
        created_at: i64,
    },
    /// `assignChatToWorkItem`.
    AssignChat {
        chat_id: String,
        work_item_id: String,
    },
}

/// `snapshot` with `change` applied, or `None` when the snapshot already
/// shows it (a frame caught up) or it cannot apply (an id it names is gone).
pub(crate) fn apply(
    snapshot: &WorkOutlineSnapshot,
    change: &LocalChange,
) -> Option<WorkOutlineSnapshot> {
    match change {
        LocalChange::Move {
            work_item_id,
            destination_parent,
            placement,
        } => apply_move(
            snapshot,
            work_item_id,
            destination_parent.as_deref(),
            placement,
        ),
        LocalChange::Rename {
            work_item_id,
            title,
        } => {
            let item = snapshot.item(work_item_id)?;
            if item.title == *title {
                return None;
            }
            let mut out = snapshot.clone();
            item_mut(&mut out, work_item_id)?.title = title.clone();
            Some(out)
        }
        LocalChange::Create {
            work_item_id,
            parent,
            placement,
            title,
            created_at,
        } => apply_create(
            snapshot,
            work_item_id,
            parent.as_deref(),
            placement,
            title,
            *created_at,
        ),
        LocalChange::AssignChat {
            chat_id,
            work_item_id,
        } => apply_assign(snapshot, chat_id, work_item_id),
    }
}

fn item_mut<'a>(snapshot: &'a mut WorkOutlineSnapshot, id: &str) -> Option<&'a mut WorkItemView> {
    snapshot.items.iter_mut().find(|item| item.id == id)
}

/// The ordered children of `parent` (`None` = the Space root).
fn children<'a>(snapshot: &'a WorkOutlineSnapshot, parent: Option<&str>) -> Option<&'a [String]> {
    match parent {
        None => Some(&snapshot.root_work_item_ids),
        Some(id) => snapshot.item(id).map(|item| item.child_ids.as_slice()),
    }
}

fn children_mut<'a>(
    snapshot: &'a mut WorkOutlineSnapshot,
    parent: Option<&str>,
) -> Option<&'a mut Vec<String>> {
    match parent {
        None => Some(&mut snapshot.root_work_item_ids),
        Some(id) => item_mut(snapshot, id).map(|item| &mut item.child_ids),
    }
}

/// `siblings` (without the placed item) with `id` inserted per `placement`;
/// `None` when the placement's sibling is not there.
fn placed(siblings: &[String], id: &str, placement: &WorkOutlinePlacement) -> Option<Vec<String>> {
    let mut out: Vec<String> = siblings.iter().filter(|s| *s != id).cloned().collect();
    let position = |sibling: &str| out.iter().position(|s| s == sibling);
    let ix = match placement {
        WorkOutlinePlacement::First => 0,
        WorkOutlinePlacement::Last => out.len(),
        WorkOutlinePlacement::Before {
            sibling_work_item_id,
        } => position(sibling_work_item_id)?,
        WorkOutlinePlacement::After {
            sibling_work_item_id,
        } => position(sibling_work_item_id)? + 1,
    };
    out.insert(ix, id.to_owned());
    Some(out)
}

fn depth_under(snapshot: &WorkOutlineSnapshot, parent: Option<&str>) -> u32 {
    parent
        .and_then(|id| snapshot.item(id))
        .map_or(0, |item| item.depth + 1)
}

/// Whether `candidate` is `ancestor` or sits below it.
fn within(snapshot: &WorkOutlineSnapshot, ancestor: &str, candidate: &str) -> bool {
    let mut cursor = Some(candidate.to_owned());
    let mut hops = 0;
    while let Some(id) = cursor {
        if id == ancestor {
            return true;
        }
        hops += 1;
        if hops > snapshot.items.len() {
            return false;
        }
        cursor = snapshot
            .item(&id)
            .and_then(|item| item.parent_work_item_id.clone());
    }
    false
}

fn apply_move(
    snapshot: &WorkOutlineSnapshot,
    id: &str,
    destination: Option<&str>,
    placement: &WorkOutlinePlacement,
) -> Option<WorkOutlineSnapshot> {
    let item = snapshot.item(id)?;
    if destination.is_some_and(|parent| within(snapshot, id, parent)) {
        return None;
    }
    let order = placed(children(snapshot, destination)?, id, placement)?;
    let source = item.parent_work_item_id.clone();
    if source.as_deref() == destination && children(snapshot, destination)? == order.as_slice() {
        return None;
    }
    let mut out = snapshot.clone();
    children_mut(&mut out, source.as_deref())?.retain(|child| child != id);
    *children_mut(&mut out, destination)? = order;
    let depth = depth_under(&out, destination);
    let moved = item_mut(&mut out, id)?;
    moved.parent_work_item_id = destination.map(str::to_owned);
    moved.raw_parent_work_item_id = destination.map(str::to_owned);
    // Re-depth the moved subtree.
    let mut stack = vec![(id.to_owned(), depth)];
    while let Some((next, depth)) = stack.pop() {
        let Some(view) = item_mut(&mut out, &next) else {
            continue;
        };
        view.depth = depth;
        stack.extend(
            view.child_ids
                .iter()
                .map(|child| (child.clone(), depth + 1)),
        );
    }
    Some(out)
}

fn apply_create(
    snapshot: &WorkOutlineSnapshot,
    id: &str,
    parent: Option<&str>,
    placement: &WorkOutlinePlacement,
    title: &str,
    created_at: i64,
) -> Option<WorkOutlineSnapshot> {
    if snapshot.item(id).is_some() {
        return None;
    }
    let order = placed(children(snapshot, parent)?, id, placement)?;
    let mut out = snapshot.clone();
    *children_mut(&mut out, parent)? = order;
    let status = WorkItemStatus::default();
    out.items.push(WorkItemView {
        id: id.to_owned(),
        space_id: snapshot.space_id.clone(),
        parent_work_item_id: parent.map(str::to_owned),
        raw_parent_work_item_id: parent.map(str::to_owned),
        order_key: String::new(),
        title: title.to_owned(),
        description: None,
        status,
        effective_status: match status {
            WorkItemStatus::Active => WorkItemEffectiveStatus::Active,
            WorkItemStatus::Completed => WorkItemEffectiveStatus::Completed,
            WorkItemStatus::Pending => WorkItemEffectiveStatus::Pending,
        },
        archived_at: None,
        archived_by: None,
        archive_cause: None,
        inherit_parent_instructions: true,
        created_by: None,
        created_at,
        updated_at: created_at,
        child_ids: Vec::new(),
        depth: depth_under(snapshot, parent),
        chat_ids: Vec::new(),
        external_reference_ids: Vec::new(),
        rollup: WorkItemRollup::default(),
    });
    Some(out)
}

fn apply_assign(
    snapshot: &WorkOutlineSnapshot,
    chat_id: &str,
    work_item_id: &str,
) -> Option<WorkOutlineSnapshot> {
    let target = snapshot.item(work_item_id)?;
    let shown = target.chat_ids.iter().any(|id| id == chat_id)
        && !snapshot
            .unassociated_chat_ids
            .iter()
            .any(|id| id == chat_id);
    if shown {
        return None;
    }
    let mut out = snapshot.clone();
    for item in &mut out.items {
        item.chat_ids.retain(|id| id != chat_id);
    }
    item_mut(&mut out, work_item_id)?
        .chat_ids
        .push(chat_id.to_owned());
    out.unassociated_chat_ids.retain(|id| id != chat_id);
    match out.assignments.iter_mut().find(|a| a.chat_id == chat_id) {
        Some(assignment) => {
            assignment.work_item_id = Some(work_item_id.to_owned());
            assignment.state = WorkItemChatAssignmentState::Associated;
        }
        None => out.assignments.push(WorkItemChatAssignmentView {
            chat_id: chat_id.to_owned(),
            space_id: snapshot.space_id.clone(),
            work_item_id: Some(work_item_id.to_owned()),
            assigned_by: None,
            assigned_at: 0,
            state: WorkItemChatAssignmentState::Associated,
        }),
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_outline::store::fixture_ids::*;
    use crate::work_outline::store::fixture_snapshot;

    fn moved(
        snapshot: &WorkOutlineSnapshot,
        id: &str,
        parent: Option<&str>,
        placement: WorkOutlinePlacement,
    ) -> Option<WorkOutlineSnapshot> {
        apply(
            snapshot,
            &LocalChange::Move {
                work_item_id: id.into(),
                destination_parent: parent.map(Into::into),
                placement,
            },
        )
    }

    fn before(sibling: &str) -> WorkOutlinePlacement {
        WorkOutlinePlacement::Before {
            sibling_work_item_id: sibling.into(),
        }
    }

    #[test]
    fn a_reorder_moves_the_item_among_its_siblings() {
        let base = fixture_snapshot("s1");
        let out = moved(&base, UI, Some(LAUNCH), before(API)).unwrap();
        assert_eq!(out.item(LAUNCH).unwrap().child_ids, [UI, API]);
        // Applying it again changes nothing: the frame "already shows it".
        assert!(moved(&out, UI, Some(LAUNCH), before(API)).is_none());
    }

    #[test]
    fn a_reparent_moves_the_subtree_and_its_depths() {
        let base = fixture_snapshot("s1");
        // API (with Auth endpoints below it) to the top level, first.
        let out = moved(&base, API, None, WorkOutlinePlacement::First).unwrap();
        assert_eq!(out.root_work_item_ids[0], API);
        assert_eq!(out.item(LAUNCH).unwrap().child_ids, [UI]);
        let api = out.item(API).unwrap();
        assert_eq!(api.parent_work_item_id, None);
        assert_eq!(api.depth, 0);
        assert_eq!(out.item(API_AUTH).unwrap().depth, 1);
        // Into its own subtree: refused.
        assert!(moved(&base, LAUNCH, Some(API), WorkOutlinePlacement::Last).is_none());
        // A sibling that is gone: refused.
        assert!(moved(&base, UI, Some(LAUNCH), before("w-gone")).is_none());
    }

    #[test]
    fn rename_create_and_assign_show_up_once() {
        let base = fixture_snapshot("s1");
        let rename = LocalChange::Rename {
            work_item_id: UI.into(),
            title: "Frontend".into(),
        };
        let out = apply(&base, &rename).unwrap();
        assert_eq!(out.item(UI).unwrap().title, "Frontend");
        assert!(apply(&out, &rename).is_none());

        let create = LocalChange::Create {
            work_item_id: "w-new".into(),
            parent: Some(LAUNCH.into()),
            placement: WorkOutlinePlacement::After {
                sibling_work_item_id: API.into(),
            },
            title: "Docs".into(),
            created_at: 1,
        };
        let out = apply(&base, &create).unwrap();
        assert_eq!(out.item(LAUNCH).unwrap().child_ids, [API, "w-new", UI]);
        assert_eq!(out.item("w-new").unwrap().depth, 1);
        assert!(apply(&out, &create).is_none());

        let assign = LocalChange::AssignChat {
            chat_id: CHAT_LOOSE_A.into(),
            work_item_id: HOUSEKEEPING.into(),
        };
        let out = apply(&base, &assign).unwrap();
        assert!(
            out.item(HOUSEKEEPING)
                .unwrap()
                .chat_ids
                .contains(&CHAT_LOOSE_A.to_owned())
        );
        assert!(!out.unassociated_chat_ids.contains(&CHAT_LOOSE_A.to_owned()));
        assert!(apply(&out, &assign).is_none());
        // Re-associating moves the Chat off its old item.
        let out = apply(
            &base,
            &LocalChange::AssignChat {
                chat_id: CHAT_UI.into(),
                work_item_id: HOUSEKEEPING.into(),
            },
        )
        .unwrap();
        assert!(!out.item(UI).unwrap().chat_ids.contains(&CHAT_UI.to_owned()));
    }
}
