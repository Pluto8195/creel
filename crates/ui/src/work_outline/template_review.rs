//! "Template changed" review, the pure parts: when a project's chip shows,
//! how each [`TemplateChange`] reads in the list, and the exact mutations
//! that accepting one sends.
//!
//! Nothing here writes. A change is applied only when the user presses its
//! Accept (or Accept all), as a client composition of existing mutations:
//! `createWorkItemInstruction`, `archiveWorkItemInstruction`, the
//! instantiation plan of an added subtree, and `updateWorkItem` for the
//! worker destination and for the template link. Removed items are reported
//! only (never deleted). The view side is `details_template_review.rs`.
use std::collections::HashMap;

use mast_proto::{
    ArchiveWorkItemInstructionParams, CreateWorkItemInstructionParams, FieldPatch,
    InstructionScope, TemplateChange, TemplateNode, UpdateWorkItemParams, WorkItemChanges,
    WorkItemTemplateLink, WorkItemTemplateView, WorkItemView, WorkOutlineMutation,
    WorkOutlineSnapshot, derive_id, template_changed,
};

use super::restructure::live_children;
use super::templates::plan_instantiation;

/// Item titles below the project root.
pub(crate) type Path = Vec<String>;

/// Items an earlier change of the same run creates, by path: the snapshot
/// does not show them yet when a later change (the worker destination) names
/// them.
pub(crate) type Overlay = HashMap<Path, String>;

/// `(linked revision, template revision)` when `project` links a live
/// template that moved on; `None` otherwise (the chip is hidden).
pub(crate) fn chip_revisions(
    snapshot: &WorkOutlineSnapshot,
    project: &WorkItemView,
) -> Option<(u32, u32)> {
    if !project.is_project || project.archived_at.is_some() {
        return None;
    }
    let template = snapshot
        .templates
        .iter()
        .find(|template| Some(&template.id) == project.template_id.as_ref())?;
    template_changed(project, template)
        .then(|| (project.template_revision.unwrap_or(0), template.revision))
}

/// "Template changed (rev 2 → 3)".
pub(crate) fn chip_text(from: u32, to: u32) -> String {
    format!("Template changed (rev {from} \u{2192} {to})")
}

/// Whether Accept applies `change` (a removal is information only).
pub(crate) fn applicable(change: &TemplateChange) -> bool {
    !matches!(change, TemplateChange::RemovedItem { .. })
}

fn path_text(path: &[String]) -> String {
    if path.is_empty() {
        "the project".to_owned()
    } else {
        path.join(" \u{203a} ")
    }
}

/// The list row's text: `("+" | "-" | "~" | "!", text)`.
pub(crate) fn describe(change: &TemplateChange) -> (&'static str, String) {
    match change {
        TemplateChange::InstructionAdded { path, text } => (
            "+",
            format!("Added instruction on {}: {text}", path_text(path)),
        ),
        TemplateChange::InstructionRemoved { path, text } => (
            "-",
            format!("Removed instruction on {}: {text}", path_text(path)),
        ),
        TemplateChange::AddedItem { path, node, .. } => {
            let count = node.node_count();
            (
                "+",
                format!(
                    "Added item {}{}",
                    path_text(path),
                    if count > 1 {
                        format!(" ({count} items)")
                    } else {
                        String::new()
                    }
                ),
            )
        }
        TemplateChange::RemovedItem { path } => (
            "!",
            format!(
                "{} exists in the project, not in the template. Left as is",
                path_text(path)
            ),
        ),
        TemplateChange::WorkerDestinationChanged { from, to } => (
            "~",
            format!(
                "Worker destination: {} \u{2192} {}",
                from.as_deref().map_or("none".to_owned(), path_text),
                to.as_deref().map_or("none".to_owned(), path_text),
            ),
        ),
    }
}

/// The live item at `path` below `project_id` (the overlay first). An item
/// with an external link is a ticket, not template structure: prefer the
/// others of the same title.
pub(crate) fn resolve_path(
    snapshot: &WorkOutlineSnapshot,
    project_id: &str,
    path: &[String],
    overlay: &Overlay,
) -> Option<String> {
    let mut current = project_id.to_owned();
    for depth in 0..path.len() {
        if let Some(id) = overlay.get(&path[..=depth]) {
            current = id.clone();
            continue;
        }
        let children = live_children(snapshot, Some(&current));
        let title = &path[depth];
        let found = children
            .iter()
            .find(|child| &child.title == title && child.external_reference_ids.is_empty())
            .or_else(|| children.iter().find(|child| &child.title == title))?;
        current = found.id.clone();
    }
    Some(current)
}

fn preorder_paths(node: &TemplateNode, prefix: &mut Path, out: &mut Vec<Path>) {
    out.push(prefix.clone());
    for child in &node.children {
        prefix.push(child.title.clone());
        preorder_paths(child, prefix, out);
        prefix.pop();
    }
}

fn op(key: &str, index: usize) -> String {
    derive_id(key, &format!("review-step:{index}"))
}

/// The mutations that accept `change` on `project_id`, in send order, ids
/// derived from `key` (<= 64 chars). `overlay` gains the items an added
/// subtree creates. `Err` says why it cannot apply now.
pub(crate) fn plan_change(
    snapshot: &WorkOutlineSnapshot,
    project_id: &str,
    change: &TemplateChange,
    key: &str,
    overlay: &mut Overlay,
) -> Result<Vec<WorkOutlineMutation>, String> {
    let space_id = snapshot.space_id.clone();
    let missing = |path: &[String]| format!("{} is no longer in the project", path_text(path));
    match change {
        TemplateChange::RemovedItem { .. } => {
            Err("Removed items are only reported; nothing is deleted".into())
        }
        TemplateChange::InstructionAdded { path, text } => {
            let item = resolve_path(snapshot, project_id, path, overlay)
                .ok_or_else(|| missing(path))?;
            Ok(vec![WorkOutlineMutation::CreateWorkItemInstruction(
                CreateWorkItemInstructionParams {
                    space_id,
                    scope: InstructionScope::WorkItem { work_item_id: item },
                    text: text.clone(),
                    placement: None,
                    source: None,
                    operation_id: op(key, 0),
                },
            )])
        }
        TemplateChange::InstructionRemoved { path, text } => {
            let item = resolve_path(snapshot, project_id, path, overlay)
                .ok_or_else(|| missing(path))?;
            let instruction = snapshot
                .instructions
                .iter()
                .find(|instruction| {
                    !instruction.archived()
                        && instruction.work_item_id.as_deref() == Some(item.as_str())
                        && &instruction.text == text
                })
                .ok_or("That instruction is already gone")?;
            Ok(vec![WorkOutlineMutation::ArchiveWorkItemInstruction(
                ArchiveWorkItemInstructionParams {
                    instruction_id: instruction.id.clone(),
                    operation_id: op(key, 0),
                },
            )])
        }
        TemplateChange::AddedItem { path, title, node } => {
            let parent_path = &path[..path.len().saturating_sub(1)];
            let parent = resolve_path(snapshot, project_id, parent_path, overlay)
                .ok_or_else(|| missing(parent_path))?;
            let template = WorkItemTemplateView {
                id: String::new(),
                space_id: space_id.clone(),
                name: String::new(),
                revision: 0,
                body: node.clone(),
                created_at: 0,
                updated_at: 0,
            };
            let run = plan_instantiation(&space_id, Some(&parent), &template, title, key);
            let mut paths = Vec::new();
            preorder_paths(node, &mut path.clone(), &mut paths);
            for (path, id) in paths.into_iter().zip(&run.item_ids) {
                overlay.insert(path, id.clone());
            }
            // The plan ends by linking the (blank) template to the new root:
            // a review never links a subtree.
            Ok(run
                .mutations
                .into_iter()
                .filter(|mutation| {
                    !matches!(mutation, WorkOutlineMutation::UpdateWorkItem(params)
                        if !params.changes.template.is_unchanged())
                })
                .collect())
        }
        TemplateChange::WorkerDestinationChanged { to, .. } => {
            let worker_destination = match to {
                Some(path) => FieldPatch::Set(
                    resolve_path(snapshot, project_id, path, overlay)
                        .ok_or("Accept the added item first, then the destination")?,
                ),
                None => FieldPatch::Clear,
            };
            Ok(vec![WorkOutlineMutation::UpdateWorkItem(
                UpdateWorkItemParams {
                    work_item_id: project_id.to_owned(),
                    changes: WorkItemChanges {
                        worker_destination,
                        ..Default::default()
                    },
                    operation_id: op(key, 0),
                },
            )])
        }
    }
}

/// The write that records `template` as reviewed: the link at its current
/// revision, nothing else. Never an undo step.
pub(crate) fn link_mutation(
    project_id: &str,
    template: &WorkItemTemplateView,
    key: &str,
) -> WorkOutlineMutation {
    WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
        work_item_id: project_id.to_owned(),
        changes: WorkItemChanges {
            template: FieldPatch::Set(WorkItemTemplateLink {
                template_id: template.id.clone(),
                revision: template.revision,
            }),
            ..Default::default()
        },
        operation_id: derive_id(key, "review-link"),
    })
}
