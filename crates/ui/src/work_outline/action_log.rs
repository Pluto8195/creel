//! The Work Outline action log: what the user changed from the UI this
//! session, as inverse-able operations, for ⌘Z / ⇧⌘Z.
//!
//! Each [`Entry`] carries the forward [`Step`] and its inverse. Undo and redo
//! send those steps through the same engine operations the UI uses (new
//! operation id each time, so they sync like any other change). Before
//! sending, the entry's [`Probe`] checks the live snapshot still looks the way
//! the entry left it; if somebody else (an agent, another device, the user in
//! another window) changed the target since, undo fails with a message rather
//! than overwriting that change.
//!
//! The log is in memory, per Space, capped at [`LOG_CAP`]. Changes by agents
//! and remote devices are never recorded: only UI call sites record. Linking
//! and unlinking are local registry rows; nothing here talks to Linear or
//! GitHub.

use std::collections::HashMap;

use mast_proto::{
    DescendantArchivePolicy, FieldPatch, InstructionPlacement, WorkItemChanges, WorkItemStatus,
    WorkOutlinePlacement, WorkOutlineSnapshot,
};

use super::actions::{ActionTask, WorkOutlineActions, new_operation_id};

/// Entries kept per Space.
pub(crate) const LOG_CAP: usize = 200;

/// Rows shown by "Recent changes".
pub(crate) const RECENT_LIMIT: usize = 12;

/// One engine operation, without its operation id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    Move {
        work_item_id: String,
        parent: Option<String>,
        placement: WorkOutlinePlacement,
    },
    Update {
        work_item_id: String,
        changes: WorkItemChanges,
    },
    Archive {
        work_item_id: String,
    },
    Restore {
        work_item_id: String,
    },
    Assign {
        chat_id: String,
        work_item_id: String,
    },
    RemoveChat {
        chat_id: String,
    },
    LinkReference(ReferenceData),
    UnlinkReference {
        reference_id: String,
    },
    UpdateInstruction {
        instruction_id: String,
        text: String,
    },
    MoveInstruction {
        instruction_id: String,
        placement: InstructionPlacement,
    },
    /// Several steps that undo / redo as one, sent in order ("Group into…").
    Batch(Vec<Step>),
}

/// What `createWorkItemExternalReference` needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReferenceData {
    pub work_item_id: String,
    pub provider: String,
    pub reference_type: String,
    pub external_id: String,
    pub url: String,
    pub cached_title: Option<String>,
}

impl ReferenceData {
    pub(crate) fn reference_id(&self) -> String {
        mast_proto::external_reference_id(
            &self.work_item_id,
            &self.provider,
            &self.reference_type,
            &self.external_id,
        )
    }
}

impl Step {
    /// The single-request steps to send, in order (a [`Step::Batch`] expands).
    pub(crate) fn leaves(&self) -> Vec<Step> {
        match self {
            Step::Batch(steps) => steps.iter().flat_map(Step::leaves).collect(),
            other => vec![other.clone()],
        }
    }

    /// Send the step under a fresh operation id (≤ 64 chars: a UUID).
    pub(crate) fn run(&self, actions: &WorkOutlineActions) -> ActionTask {
        let operation_id = new_operation_id();
        match self.clone() {
            Step::Batch(_) => gpui::Task::ready(Err(mast_proto::WorkOutlineError::new(
                mast_proto::WorkOutlineErrorCode::Invalid,
                "a batch is sent step by step",
            ))),
            Step::Move {
                work_item_id,
                parent,
                placement,
            } => actions.move_work_item(mast_proto::MoveWorkItemParams {
                work_item_id,
                destination_parent_work_item_id: parent,
                placement,
                operation_id,
            }),
            Step::Update {
                work_item_id,
                changes,
            } => actions.update_work_item(mast_proto::UpdateWorkItemParams {
                work_item_id,
                changes,
                operation_id,
            }),
            Step::Archive { work_item_id } => {
                actions.archive_work_item(mast_proto::ArchiveWorkItemParams {
                    work_item_id,
                    descendant_archive_policy: DescendantArchivePolicy::ArchiveDescendants,
                    operation_id,
                })
            }
            Step::Restore { work_item_id } => {
                actions.restore_work_item(mast_proto::RestoreWorkItemParams {
                    work_item_id,
                    operation_id,
                })
            }
            Step::Assign {
                chat_id,
                work_item_id,
            } => actions.assign_chat(mast_proto::AssignChatToWorkItemParams {
                chat_id,
                work_item_id,
                operation_id,
            }),
            Step::RemoveChat { chat_id } => {
                actions.remove_chat(mast_proto::RemoveChatFromWorkItemParams {
                    chat_id,
                    operation_id,
                })
            }
            Step::LinkReference(data) => actions.create_external_reference(
                mast_proto::CreateWorkItemExternalReferenceParams {
                    work_item_id: data.work_item_id,
                    provider: data.provider,
                    reference_type: data.reference_type,
                    external_id: data.external_id,
                    url: data.url,
                    cached_title: data.cached_title,
                    operation_id,
                },
            ),
            Step::UnlinkReference { reference_id } => actions.remove_external_reference(
                mast_proto::RemoveWorkItemExternalReferenceParams {
                    reference_id,
                    operation_id,
                },
            ),
            Step::UpdateInstruction {
                instruction_id,
                text,
            } => actions.update_instruction(mast_proto::UpdateWorkItemInstructionParams {
                instruction_id,
                text,
                operation_id,
            }),
            Step::MoveInstruction {
                instruction_id,
                placement,
            } => actions.move_instruction(mast_proto::MoveWorkItemInstructionParams {
                instruction_id,
                placement,
                operation_id,
            }),
        }
    }
}

/// A condition on the live snapshot: the state an entry left behind (for
/// undo) or the state before it (for redo).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Probe {
    /// The item is live under `parent`.
    Parent {
        work_item_id: String,
        parent: Option<String>,
    },
    /// Each `Some` field of the item equals the value.
    Fields {
        work_item_id: String,
        title: Option<String>,
        status: Option<WorkItemStatus>,
        description: Option<Option<String>>,
    },
    /// The item is (not) archived.
    Archived {
        work_item_id: String,
        archived: bool,
    },
    /// The item is live and has no live children and no chats.
    Empty { work_item_id: String },
    /// The Chat belongs to `work_item_id` (`None` = unassociated).
    Assigned {
        chat_id: String,
        work_item_id: Option<String>,
    },
    /// The reference exists (or not).
    Reference { reference_id: String, present: bool },
    /// The instruction is live with this text.
    InstructionText {
        instruction_id: String,
        text: String,
    },
    /// The instruction is live.
    InstructionLive { instruction_id: String },
    /// Every probe holds.
    All(Vec<Probe>),
    /// The live children of `parent` are exactly `children` (any order) and
    /// it has no chats.
    OnlyChildren {
        parent: String,
        children: Vec<String>,
    },
}

impl Probe {
    /// `Ok` when the snapshot matches; otherwise why not, for the message.
    pub(crate) fn check(&self, snapshot: &WorkOutlineSnapshot) -> Result<(), String> {
        let gone = |what: &str| Err(format!("{what} no longer exists"));
        match self {
            Probe::Parent {
                work_item_id,
                parent,
            } => {
                let Some(item) = snapshot.item(work_item_id) else {
                    return gone("The item");
                };
                if item.archived_at.is_some() {
                    return Err("The item is archived".into());
                }
                if &item.parent_work_item_id != parent {
                    return Err("The item was moved again".into());
                }
                Ok(())
            }
            Probe::Fields {
                work_item_id,
                title,
                status,
                description,
            } => {
                let Some(item) = snapshot.item(work_item_id) else {
                    return gone("The item");
                };
                if title.as_ref().is_some_and(|title| &item.title != title)
                    || status.is_some_and(|status| item.status != status)
                    || description
                        .as_ref()
                        .is_some_and(|description| &item.description != description)
                {
                    return Err("The item was edited again".into());
                }
                Ok(())
            }
            Probe::Archived {
                work_item_id,
                archived,
            } => {
                let Some(item) = snapshot.item(work_item_id) else {
                    return gone("The item");
                };
                if item.archived_at.is_some() != *archived {
                    return Err(if *archived {
                        "The item is no longer archived".into()
                    } else {
                        "The item was archived".into()
                    });
                }
                Ok(())
            }
            Probe::Empty { work_item_id } => {
                let Some(item) = snapshot.item(work_item_id) else {
                    return gone("The item");
                };
                if item.archived_at.is_some() {
                    return Err("The item is archived".into());
                }
                let has_children = super::restructure::live_children(snapshot, Some(work_item_id))
                    .first()
                    .is_some();
                if has_children || !item.chat_ids.is_empty() {
                    return Err("The item has since gained children or chats".into());
                }
                Ok(())
            }
            Probe::Assigned {
                chat_id,
                work_item_id,
            } => {
                let current = super::instructions::assigned_chats(snapshot).remove(chat_id);
                if &current != work_item_id {
                    return Err("The chat was moved again".into());
                }
                Ok(())
            }
            Probe::Reference {
                reference_id,
                present,
            } => {
                let exists = snapshot
                    .external_references
                    .iter()
                    .any(|reference| &reference.id == reference_id);
                if exists != *present {
                    return Err(if *present {
                        "The link is gone".into()
                    } else {
                        "The link was added again".into()
                    });
                }
                Ok(())
            }
            Probe::InstructionText {
                instruction_id,
                text,
            } => match snapshot
                .instructions
                .iter()
                .find(|instruction| &instruction.id == instruction_id)
            {
                Some(instruction) if !instruction.archived() && &instruction.text == text => Ok(()),
                Some(instruction) if instruction.archived() => {
                    Err("The instruction was removed".into())
                }
                Some(_) => Err("The instruction was edited again".into()),
                None => gone("The instruction"),
            },
            Probe::All(probes) => probes.iter().try_for_each(|probe| probe.check(snapshot)),
            Probe::OnlyChildren { parent, children } => {
                let Some(item) = snapshot.item(parent) else {
                    return gone("The group");
                };
                if item.archived_at.is_some() {
                    return Err("The group is archived".into());
                }
                let mut live: Vec<&str> =
                    super::restructure::live_children(snapshot, Some(parent))
                        .iter()
                        .map(|child| child.id.as_str())
                        .collect();
                let mut want: Vec<&str> = children.iter().map(String::as_str).collect();
                live.sort_unstable();
                want.sort_unstable();
                if live != want || !item.chat_ids.is_empty() {
                    return Err("The group has since changed".into());
                }
                Ok(())
            }
            Probe::InstructionLive { instruction_id } => match snapshot
                .instructions
                .iter()
                .find(|instruction| &instruction.id == instruction_id)
            {
                Some(instruction) if !instruction.archived() => Ok(()),
                Some(_) => Err("The instruction was removed".into()),
                None => gone("The instruction"),
            },
        }
    }
}

/// One logged change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub id: u64,
    /// "moved 'X' under 'Y'": shown after "Undid: " / "Redid: ".
    pub label: String,
    pub forward: Step,
    pub inverse: Step,
    /// State the forward step left; undo requires it.
    pub after: Probe,
    /// State before the forward step; redo requires it.
    pub before: Probe,
    /// The forward request's operation id while its reply is pending, so a
    /// failure can drop the entry.
    pub operation_id: Option<String>,
}

/// An entry before the log numbers it.
pub(crate) type Draft = Entry;

#[derive(Debug, Default)]
struct SpaceLog {
    entries: Vec<Entry>,
    /// `entries[..applied]` can be undone; the rest can be redone.
    applied: usize,
}

/// What "Recent changes" lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Recent {
    pub id: u64,
    pub label: String,
    /// Already undone (redoable).
    pub undone: bool,
}

#[derive(Debug, Default)]
pub(crate) struct ActionLog {
    spaces: HashMap<String, SpaceLog>,
    next_id: u64,
    last_space: Option<String>,
}

impl ActionLog {
    /// Append `draft`; a new change forgets the redo tail.
    pub(crate) fn record(&mut self, space_id: &str, mut draft: Draft) -> u64 {
        self.next_id += 1;
        draft.id = self.next_id;
        let log = self.spaces.entry(space_id.to_owned()).or_default();
        log.entries.truncate(log.applied);
        log.entries.push(draft);
        if log.entries.len() > LOG_CAP {
            let excess = log.entries.len() - LOG_CAP;
            log.entries.drain(..excess);
        }
        log.applied = log.entries.len();
        self.last_space = Some(space_id.to_owned());
        self.next_id
    }

    /// The forward request `operation_id` failed: forget its entry.
    pub(crate) fn fail(&mut self, operation_id: &str) {
        for log in self.spaces.values_mut() {
            if let Some(ix) = log
                .entries
                .iter()
                .position(|entry| entry.operation_id.as_deref() == Some(operation_id))
            {
                log.entries.remove(ix);
                if ix < log.applied {
                    log.applied -= 1;
                }
            }
        }
    }

    /// The forward request was acknowledged.
    pub(crate) fn confirm(&mut self, operation_id: &str) {
        for log in self.spaces.values_mut() {
            for entry in &mut log.entries {
                if entry.operation_id.as_deref() == Some(operation_id) {
                    entry.operation_id = None;
                }
            }
        }
    }

    pub(crate) fn next_undo(&self, space_id: &str) -> Option<&Entry> {
        let log = self.spaces.get(space_id)?;
        log.applied.checked_sub(1).map(|ix| &log.entries[ix])
    }

    pub(crate) fn next_redo(&self, space_id: &str) -> Option<&Entry> {
        let log = self.spaces.get(space_id)?;
        log.entries.get(log.applied)
    }

    /// `entry_id` was just undone (it must be the next undo).
    pub(crate) fn mark_undone(&mut self, space_id: &str, entry_id: u64) {
        if let Some(log) = self.spaces.get_mut(space_id) {
            if log.applied > 0 && log.entries[log.applied - 1].id == entry_id {
                log.applied -= 1;
            }
        }
        self.last_space = Some(space_id.to_owned());
    }

    /// `entry_id` was just redone (it must be the next redo).
    pub(crate) fn mark_redone(&mut self, space_id: &str, entry_id: u64) {
        if let Some(log) = self.spaces.get_mut(space_id) {
            if log
                .entries
                .get(log.applied)
                .is_some_and(|e| e.id == entry_id)
            {
                log.applied += 1;
            }
        }
        self.last_space = Some(space_id.to_owned());
    }

    /// The Space most recently recorded in or undone/redone.
    pub(crate) fn last_space(&self) -> Option<&str> {
        self.last_space.as_deref()
    }

    /// Newest first, at most `limit`.
    pub(crate) fn recent(&self, space_id: &str, limit: usize) -> Vec<Recent> {
        let Some(log) = self.spaces.get(space_id) else {
            return Vec::new();
        };
        log.entries
            .iter()
            .enumerate()
            .rev()
            .take(limit)
            .map(|(ix, entry)| Recent {
                id: entry.id,
                label: entry.label.clone(),
                undone: ix >= log.applied,
            })
            .collect()
    }

    /// Entries to undo, newest first, to get back to just before `entry_id`
    /// (inclusive). `None` when `entry_id` is not an undoable entry.
    pub(crate) fn undo_chain(&self, space_id: &str, entry_id: u64) -> Option<Vec<u64>> {
        let log = self.spaces.get(space_id)?;
        let ix = log.entries[..log.applied]
            .iter()
            .position(|entry| entry.id == entry_id)?;
        Some(
            log.entries[ix..log.applied]
                .iter()
                .rev()
                .map(|entry| entry.id)
                .collect(),
        )
    }

    pub(crate) fn len(&self, space_id: &str) -> usize {
        self.spaces.get(space_id).map_or(0, |log| log.entries.len())
    }
}

// ---------------------------------------------------------------------------
// Entry builders. Each reads the snapshot as it is BEFORE the change.
// ---------------------------------------------------------------------------

fn shown(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        return "Untitled".into();
    }
    const MAX: usize = 40;
    if title.chars().count() > MAX {
        let cut: String = title.chars().take(MAX).collect();
        format!("{}…", cut.trim_end())
    } else {
        title.to_owned()
    }
}

fn quoted(title: &str) -> String {
    format!("\u{2018}{}\u{2019}", shown(title))
}

fn draft(
    label: String,
    forward: Step,
    inverse: Step,
    before: Probe,
    after: Probe,
    operation_id: Option<String>,
) -> Draft {
    Entry {
        id: 0,
        label,
        forward,
        inverse,
        after,
        before,
        operation_id,
    }
}

fn item_title(snapshot: &WorkOutlineSnapshot, id: &str) -> String {
    snapshot
        .item(id)
        .map(|item| quoted(&item.title))
        .unwrap_or_else(|| "an item".into())
}

/// Where `item_id` sits now, as a placement that puts it back there.
fn placement_back(snapshot: &WorkOutlineSnapshot, item_id: &str) -> Option<WorkOutlinePlacement> {
    let item = snapshot.item(item_id)?;
    let siblings = super::restructure::live_children(snapshot, item.parent_work_item_id.as_deref());
    let ix = siblings.iter().position(|sibling| sibling.id == item.id)?;
    Some(match ix.checked_sub(1) {
        Some(previous) => WorkOutlinePlacement::After {
            sibling_work_item_id: siblings[previous].id.clone(),
        },
        None => WorkOutlinePlacement::First,
    })
}

/// A move, reorder, indent, outdent, or drag of one item.
pub(crate) fn move_entry(
    snapshot: &WorkOutlineSnapshot,
    work_item_id: &str,
    destination_parent: Option<&str>,
    placement: &WorkOutlinePlacement,
    operation_id: Option<String>,
) -> Option<Draft> {
    let item = snapshot.item(work_item_id)?;
    if item.archived_at.is_some() {
        return None;
    }
    let old_parent = item.parent_work_item_id.clone();
    let back = placement_back(snapshot, work_item_id)?;
    let label = if old_parent.as_deref() == destination_parent {
        format!("reordered {}", quoted(&item.title))
    } else {
        match destination_parent {
            Some(parent) => format!(
                "moved {} under {}",
                quoted(&item.title),
                item_title(snapshot, parent)
            ),
            None => format!("moved {} to the top level", quoted(&item.title)),
        }
    };
    let new_parent = destination_parent.map(str::to_owned);
    Some(draft(
        label,
        Step::Move {
            work_item_id: work_item_id.to_owned(),
            parent: new_parent.clone(),
            placement: placement.clone(),
        },
        Step::Move {
            work_item_id: work_item_id.to_owned(),
            parent: old_parent.clone(),
            placement: back,
        },
        Probe::Parent {
            work_item_id: work_item_id.to_owned(),
            parent: old_parent,
        },
        Probe::Parent {
            work_item_id: work_item_id.to_owned(),
            parent: new_parent,
        },
        operation_id,
    ))
}

/// Where "Group into…" puts the items.
pub(crate) enum GroupTarget<'a> {
    /// A container created by the same action.
    New { work_item_id: &'a str, title: &'a str },
    /// An existing work item; items are appended.
    Existing { work_item_id: &'a str },
}

/// "Group into…": `items` (top-most, in visible order) reparented under the
/// target as ONE entry. Undo moves each back (siblings restored in order) and
/// archives a container the action created; redo restores it and moves again.
pub(crate) fn group_entry(
    snapshot: &WorkOutlineSnapshot,
    target: &GroupTarget<'_>,
    items: &[String],
    operation_id: Option<String>,
) -> Option<Draft> {
    let (container, created) = match target {
        GroupTarget::New { work_item_id, .. } => (*work_item_id, true),
        GroupTarget::Existing { work_item_id } => (*work_item_id, false),
    };
    let mut forward = Vec::new();
    let mut inverse = Vec::new();
    let mut after = Vec::new();
    let mut before = Vec::new();
    if created {
        forward.push(Step::Restore {
            work_item_id: container.to_owned(),
        });
        after.push(Probe::OnlyChildren {
            parent: container.to_owned(),
            children: items.to_vec(),
        });
        before.push(Probe::Archived {
            work_item_id: container.to_owned(),
            archived: true,
        });
    }
    for id in items {
        let item = snapshot.item(id)?;
        let back = placement_back(snapshot, id)?;
        forward.push(Step::Move {
            work_item_id: id.clone(),
            parent: Some(container.to_owned()),
            placement: WorkOutlinePlacement::Last,
        });
        inverse.push(Step::Move {
            work_item_id: id.clone(),
            parent: item.parent_work_item_id.clone(),
            placement: back,
        });
        after.push(Probe::Parent {
            work_item_id: id.clone(),
            parent: Some(container.to_owned()),
        });
        before.push(Probe::Parent {
            work_item_id: id.clone(),
            parent: item.parent_work_item_id.clone(),
        });
    }
    if created {
        inverse.push(Step::Archive {
            work_item_id: container.to_owned(),
        });
    }
    let name = match target {
        GroupTarget::New { title, .. } => quoted(title),
        GroupTarget::Existing { work_item_id } => item_title(snapshot, work_item_id),
    };
    let what = match items {
        [only] => item_title(snapshot, only),
        many => format!("{} items", many.len()),
    };
    Some(draft(
        format!("grouped {what} into {name}"),
        Step::Batch(forward),
        Step::Batch(inverse),
        Probe::All(before),
        Probe::All(after),
        operation_id,
    ))
}

/// A new item. `work_item_id` is `derive_id(operationId, "item")`.
pub(crate) fn create_entry(work_item_id: &str, title: &str, operation_id: Option<String>) -> Draft {
    draft(
        format!("created {}", quoted(title)),
        Step::Restore {
            work_item_id: work_item_id.to_owned(),
        },
        Step::Archive {
            work_item_id: work_item_id.to_owned(),
        },
        Probe::Archived {
            work_item_id: work_item_id.to_owned(),
            archived: true,
        },
        // Undo refuses once the item has children or chats of its own.
        Probe::Empty {
            work_item_id: work_item_id.to_owned(),
        },
        operation_id,
    )
}

/// A rename, status change, or description edit. `None` for changes that
/// cannot be undone cleanly (they carry a request resolution, inheritance, or
/// sync state) or that change nothing.
pub(crate) fn update_entry(
    snapshot: &WorkOutlineSnapshot,
    work_item_id: &str,
    changes: &WorkItemChanges,
    operation_id: Option<String>,
) -> Option<Draft> {
    if changes.resolve_open_requests
        || changes.inherit_parent_instructions.is_some()
        || changes.external_sync_state.is_some()
        || changes.is_project.is_some()
        || !changes.worker_destination.is_unchanged()
    {
        return None;
    }
    let item = snapshot.item(work_item_id)?;
    let mut inverse = WorkItemChanges::default();
    let mut before = (None, None, None);
    let mut after = (None, None, None);
    let mut parts = Vec::new();
    if let Some(title) = changes.title.as_ref().filter(|title| **title != item.title) {
        inverse.title = Some(item.title.clone());
        before.0 = Some(item.title.clone());
        after.0 = Some(title.clone());
        parts.push(format!(
            "renamed {} to {}",
            quoted(&item.title),
            quoted(title)
        ));
    }
    if let Some(status) = changes.status.filter(|status| *status != item.status) {
        inverse.status = Some(item.status);
        before.1 = Some(item.status);
        after.1 = Some(status);
        let word = match status {
            WorkItemStatus::Pending => "pending",
            WorkItemStatus::Active => "active",
            WorkItemStatus::Completed => "completed",
        };
        parts.push(format!("marked {} {word}", quoted(&item.title)));
    }
    let new_description = match &changes.description {
        FieldPatch::Unchanged => None,
        FieldPatch::Clear => Some(None),
        FieldPatch::Set(text) => Some(Some(text.clone())),
    };
    if let Some(new_description) = new_description.filter(|new| *new != item.description) {
        inverse.description = match item.description.clone() {
            Some(text) => FieldPatch::Set(text),
            None => FieldPatch::Clear,
        };
        before.2 = Some(item.description.clone());
        after.2 = Some(new_description);
        parts.push(format!("edited the description of {}", quoted(&item.title)));
    }
    if parts.is_empty() {
        return None;
    }
    let label = if parts.len() == 1 {
        parts.remove(0)
    } else {
        format!("edited {}", quoted(&item.title))
    };
    let forward = Step::Update {
        work_item_id: work_item_id.to_owned(),
        changes: changes.clone(),
    };
    Some(draft(
        label,
        forward,
        Step::Update {
            work_item_id: work_item_id.to_owned(),
            changes: inverse,
        },
        Probe::Fields {
            work_item_id: work_item_id.to_owned(),
            title: before.0,
            status: before.1,
            description: before.2,
        },
        Probe::Fields {
            work_item_id: work_item_id.to_owned(),
            title: after.0,
            status: after.1,
            description: after.2,
        },
        operation_id,
    ))
}

/// Archive. Undoable unless the descendants were moved out (restore would not
/// bring them back).
pub(crate) fn archive_entry(
    snapshot: &WorkOutlineSnapshot,
    work_item_id: &str,
    policy: &DescendantArchivePolicy,
    operation_id: Option<String>,
) -> Option<Draft> {
    let item = snapshot.item(work_item_id)?;
    if item.archived_at.is_some() {
        return None;
    }
    let has_children = !super::restructure::live_children(snapshot, Some(work_item_id)).is_empty();
    if has_children && !matches!(policy, DescendantArchivePolicy::ArchiveDescendants) {
        return None;
    }
    Some(draft(
        format!("archived {}", quoted(&item.title)),
        Step::Archive {
            work_item_id: work_item_id.to_owned(),
        },
        Step::Restore {
            work_item_id: work_item_id.to_owned(),
        },
        Probe::Archived {
            work_item_id: work_item_id.to_owned(),
            archived: false,
        },
        Probe::Archived {
            work_item_id: work_item_id.to_owned(),
            archived: true,
        },
        operation_id,
    ))
}

pub(crate) fn restore_entry(
    snapshot: &WorkOutlineSnapshot,
    work_item_id: &str,
    operation_id: Option<String>,
) -> Option<Draft> {
    let item = snapshot.item(work_item_id)?;
    item.archived_at?;
    Some(draft(
        format!("restored {}", quoted(&item.title)),
        Step::Restore {
            work_item_id: work_item_id.to_owned(),
        },
        Step::Archive {
            work_item_id: work_item_id.to_owned(),
        },
        Probe::Archived {
            work_item_id: work_item_id.to_owned(),
            archived: true,
        },
        Probe::Archived {
            work_item_id: work_item_id.to_owned(),
            archived: false,
        },
        operation_id,
    ))
}

/// Moving a Chat to an item (`Some`) or off its item (`None`).
pub(crate) fn chat_entry(
    snapshot: &WorkOutlineSnapshot,
    chat_id: &str,
    chat_title: &str,
    destination: Option<&str>,
    operation_id: Option<String>,
) -> Draft {
    let previous = super::instructions::assigned_chats(snapshot).remove(chat_id);
    let step_for = |item: &Option<String>| match item {
        Some(work_item_id) => Step::Assign {
            chat_id: chat_id.to_owned(),
            work_item_id: work_item_id.clone(),
        },
        None => Step::RemoveChat {
            chat_id: chat_id.to_owned(),
        },
    };
    let destination = destination.map(str::to_owned);
    let label = match &destination {
        Some(item) => format!(
            "moved chat {} to {}",
            quoted(chat_title),
            item_title(snapshot, item)
        ),
        None => format!("removed chat {} from its item", quoted(chat_title)),
    };
    draft(
        label,
        step_for(&destination),
        step_for(&previous),
        Probe::Assigned {
            chat_id: chat_id.to_owned(),
            work_item_id: previous.clone(),
        },
        Probe::Assigned {
            chat_id: chat_id.to_owned(),
            work_item_id: destination,
        },
        operation_id,
    )
}

/// Linking a ticket or PR to an item (local registry row only).
pub(crate) fn link_entry(
    snapshot: &WorkOutlineSnapshot,
    data: ReferenceData,
    operation_id: Option<String>,
) -> Draft {
    let reference_id = data.reference_id();
    let name = data
        .cached_title
        .clone()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| data.external_id.clone());
    draft(
        format!(
            "linked {} to {}",
            quoted(&name),
            item_title(snapshot, &data.work_item_id)
        ),
        Step::LinkReference(data),
        Step::UnlinkReference {
            reference_id: reference_id.clone(),
        },
        Probe::Reference {
            reference_id: reference_id.clone(),
            present: false,
        },
        Probe::Reference {
            reference_id,
            present: true,
        },
        operation_id,
    )
}

/// Unlinking a reference; `None` when the snapshot does not have it.
pub(crate) fn unlink_entry(
    snapshot: &WorkOutlineSnapshot,
    reference_id: &str,
    operation_id: Option<String>,
) -> Option<Draft> {
    let reference = snapshot
        .external_references
        .iter()
        .find(|reference| reference.id == reference_id)?;
    let data = ReferenceData {
        work_item_id: reference.work_item_id.clone(),
        provider: reference.provider.clone(),
        reference_type: reference.reference_type.clone(),
        external_id: reference.external_id.clone(),
        url: reference.url.clone(),
        cached_title: reference.cached_title.clone(),
    };
    let name = data
        .cached_title
        .clone()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| data.external_id.clone());
    // The link's id is content-derived, so relinking restores the same row.
    Some(draft(
        format!(
            "unlinked {} from {}",
            quoted(&name),
            item_title(snapshot, &data.work_item_id)
        ),
        Step::UnlinkReference {
            reference_id: reference_id.to_owned(),
        },
        Step::LinkReference(data),
        Probe::Reference {
            reference_id: reference_id.to_owned(),
            present: true,
        },
        Probe::Reference {
            reference_id: reference_id.to_owned(),
            present: false,
        },
        operation_id,
    ))
}

fn instruction_owner(snapshot: &WorkOutlineSnapshot, instruction_id: &str) -> String {
    match snapshot
        .instructions
        .iter()
        .find(|instruction| instruction.id == instruction_id)
        .and_then(|instruction| instruction.work_item_id.as_deref())
    {
        Some(item) => item_title(snapshot, item),
        None => "the project".into(),
    }
}

/// An instruction's text edit.
pub(crate) fn instruction_edit_entry(
    snapshot: &WorkOutlineSnapshot,
    instruction_id: &str,
    text: &str,
    operation_id: Option<String>,
) -> Option<Draft> {
    let old = snapshot
        .instructions
        .iter()
        .find(|instruction| instruction.id == instruction_id && !instruction.archived())?
        .text
        .clone();
    if old == text {
        return None;
    }
    Some(draft(
        format!(
            "edited an instruction on {}",
            instruction_owner(snapshot, instruction_id)
        ),
        Step::UpdateInstruction {
            instruction_id: instruction_id.to_owned(),
            text: text.to_owned(),
        },
        Step::UpdateInstruction {
            instruction_id: instruction_id.to_owned(),
            text: old.clone(),
        },
        Probe::InstructionText {
            instruction_id: instruction_id.to_owned(),
            text: old,
        },
        Probe::InstructionText {
            instruction_id: instruction_id.to_owned(),
            text: text.to_owned(),
        },
        operation_id,
    ))
}

/// Reordering an instruction one step. `scope` is the live instructions of
/// its scope in order, before the move.
pub(crate) fn instruction_move_entry(
    snapshot: &WorkOutlineSnapshot,
    scope: &[String],
    instruction_id: &str,
    placement: InstructionPlacement,
    operation_id: Option<String>,
) -> Option<Draft> {
    let ix = scope.iter().position(|id| id == instruction_id)?;
    let back = match (ix.checked_sub(1), scope.get(ix + 1)) {
        (Some(previous), _) => InstructionPlacement::After {
            sibling_instruction_id: scope[previous].clone(),
        },
        (None, Some(next)) => InstructionPlacement::Before {
            sibling_instruction_id: next.clone(),
        },
        (None, None) => return None,
    };
    let live = Probe::InstructionLive {
        instruction_id: instruction_id.to_owned(),
    };
    Some(draft(
        format!(
            "reordered an instruction on {}",
            instruction_owner(snapshot, instruction_id)
        ),
        Step::MoveInstruction {
            instruction_id: instruction_id.to_owned(),
            placement,
        },
        Step::MoveInstruction {
            instruction_id: instruction_id.to_owned(),
            placement: back,
        },
        live.clone(),
        live,
        operation_id,
    ))
}

#[cfg(test)]
#[path = "action_log_tests.rs"]
mod tests;
