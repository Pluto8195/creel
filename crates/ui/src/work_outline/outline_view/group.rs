//! Multi-selection and "Group into…" for the Outline.
//!
//! The primary selection (`selected`) stays a single row. ⌘-click toggles a
//! row in an additive set (`multi`), ⇧-click adds every item row between the
//! primary row and the clicked one; a plain click, Esc, or moving the cursor
//! with the keys clears the set. "Group into…" (⌘⇧G, item menu) acts on the
//! set (or the primary row when the set is empty): a picker lists "New
//! container…" and the Space's other work items.
//!
//! The effect is one action and one undo step: an optional `createWorkItem`
//! plus a `moveWorkItem` per top-most selected item, in visible order, logged
//! as a single [`action_log::group_entry`].

use super::super::action_log::GroupTarget;
use super::super::model::{self, descendants};
use super::*;

/// Where grouped items go.
pub(crate) enum GroupDest {
    /// A new container titled this, where the first item was.
    New(String),
    /// An existing work item (items are appended under it).
    Existing(String),
}

impl OutlineView {
    // ----------------------------------------------------------------------
    // Multi-selection
    // ----------------------------------------------------------------------

    /// Whether `key` is in the multi-selection.
    pub(crate) fn is_multi_selected(&self, key: &str) -> bool {
        self.multi.contains(key)
    }

    /// The `(space id, item id)` of every item the group commands act on, in
    /// visible order: the multi-selection, else the selected item.
    pub(crate) fn group_selection(&self) -> Vec<(String, String)> {
        if self.multi.is_empty() {
            return self.selected_item().into_iter().collect();
        }
        self.model
            .rows
            .iter()
            .filter(|row| self.multi.contains(&row.key))
            .filter_map(OutlineRow::item)
            .filter(|item| !item.archived)
            .map(|item| (item.space_id.clone(), item.item_id.clone()))
            .collect()
    }

    /// The Space the current selection (primary or set) belongs to.
    fn selection_space(&self) -> Option<String> {
        self.group_selection().first().map(|(space, _)| space.clone())
    }

    /// `key` as a groupable row: a live item row, in the selection's Space.
    fn groupable(&self, key: &str) -> Result<(), &'static str> {
        let item = self
            .model
            .row(key)
            .and_then(OutlineRow::item)
            .ok_or("Only work items can be grouped")?;
        if item.archived {
            return Err("Restore this work item before grouping it");
        }
        if self
            .selection_space()
            .is_some_and(|space| space != item.space_id)
        {
            return Err("Group items from one project at a time");
        }
        Ok(())
    }

    /// ⌘-click: add or remove `key`. The primary row joins the set first, so
    /// the gesture is additive.
    pub(crate) fn toggle_multi(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Err(notice) = self.groupable(key) {
            self.show_notice(notice, cx);
            return;
        }
        if self.multi.is_empty() {
            if let Some(primary) = self
                .selected
                .clone()
                .filter(|primary| self.model.row(primary).is_some_and(|r| r.item().is_some()))
            {
                self.multi.insert(primary);
            }
        }
        if !self.multi.remove(key) {
            self.multi.insert(key.to_owned());
            self.select(key, cx);
        }
        cx.notify();
    }

    /// ⇧-click: add every item row from the primary row to `key`.
    pub(crate) fn extend_multi_to(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Err(notice) = self.groupable(key) {
            self.show_notice(notice, cx);
            return;
        }
        let Some(to) = self.model.position(key) else {
            return;
        };
        let from = self
            .selected
            .as_deref()
            .and_then(|primary| self.model.position(primary))
            .unwrap_or(to);
        let (lo, hi) = (from.min(to), from.max(to));
        let space = self
            .model
            .row(key)
            .and_then(OutlineRow::item)
            .map(|item| item.space_id.clone());
        let range: Vec<String> = self.model.rows[lo..=hi]
            .iter()
            .filter(|row| {
                row.item()
                    .is_some_and(|item| !item.archived && Some(&item.space_id) == space.as_ref())
            })
            .map(|row| row.key.clone())
            .collect();
        self.multi.extend(range);
        cx.notify();
    }

    /// Clear the multi-selection; `true` when there was one.
    pub(crate) fn clear_multi(&mut self, cx: &mut Context<Self>) -> bool {
        if self.multi.is_empty() {
            return false;
        }
        self.multi.clear();
        cx.notify();
        true
    }

    /// Keep only rows that are still shown.
    pub(super) fn prune_multi(&mut self) {
        let model = &self.model;
        self.multi.retain(|key| model.position(key).is_some());
    }

    // ----------------------------------------------------------------------
    // Group into…
    // ----------------------------------------------------------------------

    /// The top-most of `item_ids` (none is inside another's subtree), in
    /// visible order.
    fn top_most(
        &self,
        snapshot: &WorkOutlineSnapshot,
        space_id: &str,
        item_ids: &[String],
    ) -> Vec<String> {
        let mut inside: HashSet<String> = HashSet::new();
        for id in item_ids {
            inside.extend(descendants(snapshot, id));
        }
        let mut top: Vec<String> = item_ids
            .iter()
            .filter(|id| {
                !inside.contains(*id)
                    && snapshot
                        .item(id)
                        .is_some_and(|item| item.archived_at.is_none())
            })
            .cloned()
            .collect();
        top.dedup();
        top.sort_by_key(|id| {
            self.model
                .position(&row_key::item(space_id, id))
                .unwrap_or(usize::MAX)
        });
        top
    }

    /// "Group into…": the picker (⌘⇧G, item menu).
    pub(crate) fn open_group_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        if self.group_selection().is_empty() {
            return Err("Select a work item first");
        }
        let space_id = self
            .selection_space()
            .ok_or("Select a work item first")?;
        if self.group_selection().iter().any(|(space, _)| *space != space_id) {
            return Err("Group items from one project at a time");
        }
        let ids: Vec<String> = self.group_selection().into_iter().map(|(_, id)| id).collect();
        let snapshot = self
            .snapshot(&space_id)
            .ok_or("This project's outline is not loaded")?;
        let top = self.top_most(snapshot, &space_id, &ids);
        if top.is_empty() {
            return Err("Select a work item first");
        }
        let mut excluded: HashSet<String> = top.iter().cloned().collect();
        for id in &top {
            excluded.extend(descendants(snapshot, id));
        }
        let mut entries = vec![PickerEntry {
            label: "New container\u{2026}".to_string(),
            depth: 0,
            value: None,
        }];
        entries.extend(
            model::destinations(snapshot, None)
                .into_iter()
                .filter(|dest| dest.item_id.as_ref().is_some_and(|id| !excluded.contains(id)))
                .map(|dest| PickerEntry {
                    label: dest.title,
                    depth: dest.depth,
                    value: dest.item_id,
                }),
        );
        let title = match top.as_slice() {
            [only] => snapshot
                .item(only)
                .map(|item| format!("Group \u{201c}{}\u{201d} into\u{2026}", item.title))
                .unwrap_or_else(|| "Group into\u{2026}".to_string()),
            many => format!("Group {} items into\u{2026}", many.len()),
        };
        self.open_picker(
            title,
            entries,
            PickerPurpose::Group {
                space_id,
                item_ids: top,
            },
            window,
            cx,
        );
        Ok(())
    }

    /// An entry of the Group picker was chosen: an existing container groups
    /// at once; "New container…" asks for a title in the inline row.
    pub(super) fn choose_group_entry(
        &mut self,
        space_id: String,
        item_ids: Vec<String>,
        entry: PickerEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_picker(window, cx);
        let Some(target) = entry.value else {
            if let Err(notice) = self.begin_group_title(&space_id, item_ids, window, cx) {
                self.show_notice(notice, cx);
            }
            return;
        };
        if let Err(notice) = self.group_items(&space_id, &item_ids, GroupDest::Existing(target), cx)
        {
            self.show_notice(notice, cx);
        }
    }

    /// Show the inline title row where the new container will go.
    fn begin_group_title(
        &mut self,
        space_id: &str,
        item_ids: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let snapshot = self
            .snapshot(space_id)
            .ok_or("This project's outline is not loaded")?;
        let top = self.top_most(snapshot, space_id, &item_ids);
        let first = top.first().ok_or("Select a work item first")?.clone();
        let parent = snapshot
            .item(&first)
            .and_then(|item| item.parent_work_item_id.clone());
        self.cancel_edit(cx);
        self.editing = Some(InlineEdit {
            kind: EditKind::Create {
                space_id: space_id.to_owned(),
                parent,
                placement: WorkOutlinePlacement::Before {
                    sibling_work_item_id: first.clone(),
                },
                anchor: CreateAnchor::After(row_key::item(space_id, &first)),
                group: Some(top),
                template: None,
            },
            operation_id: new_operation_id(),
            submitting: false,
        });
        self.start_editor("", window, cx);
        Ok(())
    }

    /// Reparent `item_ids` under `dest` as ONE action and ONE undo step.
    /// `Err` is the refusal to show; nothing was sent or changed then.
    pub(crate) fn group_items(
        &mut self,
        space_id: &str,
        item_ids: &[String],
        dest: GroupDest,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        const BUSY: &str = "Still applying the previous change. Try again in a moment";
        if self.restructuring_busy() || self.log_busy {
            return Err(BUSY.into());
        }
        let snapshot = self
            .snapshot(space_id)
            .ok_or("This project's outline is not loaded")?;
        let top = self.top_most(snapshot, space_id, item_ids);
        let Some(first) = top.first().cloned() else {
            return Err("Select a work item first".into());
        };
        if let GroupDest::Existing(target) = &dest {
            let live = snapshot
                .item(target)
                .is_some_and(|item| item.archived_at.is_none());
            if !live {
                return Err("That work item is not available".into());
            }
            if top.contains(target)
                || top
                    .iter()
                    .any(|id| descendants(snapshot, id).contains(target))
            {
                return Err(
                    "Can\u{2019}t group an item into itself or something inside it. Nothing was changed"
                        .into(),
                );
            }
        }
        let first_parent = snapshot
            .item(&first)
            .and_then(|item| item.parent_work_item_id.clone());

        // The container: created here, or the chosen item.
        let create_op = new_operation_id();
        let (container, title) = match &dest {
            GroupDest::New(title) => {
                let title: String = title
                    .trim()
                    .chars()
                    .take(mast_proto::limits::MAX_TITLE_CHARS)
                    .collect();
                if title.is_empty() {
                    return Err("Give the new container a title".into());
                }
                (
                    mast_proto::derive_id(&create_op, mast_proto::roles::ITEM),
                    Some(title),
                )
            }
            GroupDest::Existing(target) => (target.clone(), None),
        };
        let create_placement = WorkOutlinePlacement::Before {
            sibling_work_item_id: first.clone(),
        };
        let move_ops: Vec<String> = top.iter().map(|_| new_operation_id()).collect();
        let first_op = title
            .as_ref()
            .map(|_| create_op.clone())
            .or_else(|| move_ops.first().cloned());
        let target = match &title {
            Some(title) => GroupTarget::New {
                work_item_id: &container,
                title,
            },
            None => GroupTarget::Existing {
                work_item_id: &container,
            },
        };
        let draft = action_log::group_entry(snapshot, &target, &top, first_op.clone())
            .ok_or("Those items can no longer be grouped")?;

        // Show the result now; the replies only confirm (or roll back).
        let mut shown: Vec<String> = Vec::new();
        let container_key = row_key::item(space_id, &container);
        if let Some(title) = &title {
            self.show_optimistic(
                space_id,
                &create_op,
                LocalChange::Create {
                    work_item_id: container.clone(),
                    parent: first_parent.clone(),
                    placement: create_placement.clone(),
                    title: title.clone(),
                    created_at: chrono::Utc::now().timestamp_millis(),
                },
                None,
                cx,
            );
            shown.push(create_op.clone());
        }
        for (id, op) in top.iter().zip(&move_ops) {
            self.show_optimistic(
                space_id,
                op,
                LocalChange::Move {
                    work_item_id: id.clone(),
                    destination_parent: Some(container.clone()),
                    placement: WorkOutlinePlacement::Last,
                },
                None,
                cx,
            );
            shown.push(op.clone());
        }
        self.set_collapsed(&container_key, false, cx);
        self.multi.clear();
        if title.is_some() {
            if self.model.position(&container_key).is_some() {
                self.select(&container_key, cx);
            } else {
                self.pending_select = Some(container_key);
            }
        }
        self.record_change(space_id, Some(draft), cx);

        let actions = self.state.read(cx).work_outline_actions();
        let create = title.map(|title| mast_proto::CreateWorkItemParams {
            space_id: space_id.to_owned(),
            parent_work_item_id: first_parent,
            placement: create_placement,
            title,
            description: None,
            status: None,
            operation_id: create_op,
        });
        let moves: Vec<mast_proto::MoveWorkItemParams> = top
            .into_iter()
            .zip(move_ops)
            .map(|(id, operation_id)| mast_proto::MoveWorkItemParams {
                work_item_id: id,
                destination_parent_work_item_id: Some(container.clone()),
                placement: WorkOutlinePlacement::Last,
                operation_id,
            })
            .collect();
        cx.spawn(async move |this, cx| {
            // One request at a time, in order: the container exists before
            // anything moves into it.
            let mut done: Vec<String> = Vec::new();
            let mut failure = None;
            if let Some(params) = create {
                let op = params.operation_id.clone();
                match actions.create_work_item(params).await {
                    Ok(_) => done.push(op),
                    Err(error) => failure = Some(error),
                }
            }
            if failure.is_none() {
                for params in moves {
                    let op = params.operation_id.clone();
                    match actions.move_work_item(params).await {
                        Ok(_) => done.push(op),
                        Err(error) => {
                            failure = Some(error);
                            break;
                        }
                    }
                }
            }
            this.update(cx, |this, cx| {
                for op in &shown {
                    if done.contains(op) {
                        this.acknowledge_optimistic(op, cx);
                    } else {
                        this.drop_optimistic(op, cx);
                    }
                }
                match (failure, first_op) {
                    (None, Some(op)) => this.confirm_change(&op, cx),
                    (Some(error), op) => {
                        if let Some(op) = op {
                            this.forget_change(&op, cx);
                        }
                        this.show_error(error.message.clone(), cx);
                    }
                    (None, None) => {}
                }
            })
            .ok();
        })
        .detach();
        Ok(())
    }

    /// ⌘⇧G / the menu entry.
    pub(crate) fn group_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_editing() {
            cx.propagate();
            return;
        }
        if let Err(notice) = self.open_group_picker(window, cx) {
            self.show_notice(notice, cx);
        }
    }
}
