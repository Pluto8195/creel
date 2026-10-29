//! "Convert to project…", the pure part: map an existing item onto a template
//! the user picked, and say exactly what would happen before anything is sent.
//!
//! Mast never decides a project's shape; the template does. Template nodes
//! are matched to the item's existing children by title path: a node that
//! already exists is reused, the rest are created (with the template's
//! instructions for the nodes created here). Existing children can be moved
//! under a template node (default: when the template has a worker destination,
//! children with an external link, i.e. tickets, go there; everything else
//! stays). The item is marked as a project, gets the template's worker
//! destination, and is linked to the template.
//!
//! Instructions are only written to items this conversion creates. Template
//! instructions the existing item or nodes lack are reported in the preview
//! and left to the template review: the link then says "not reviewed yet"
//! (revision 0) so the "Template changed" chip invites that review.
//!
//! Chats stay on the item they are on. The preview sends nothing; the
//! mutations of [`ConvertPlan`] go out in order on Apply, as one undo step
//! ([`super::action_log::convert_entry`]).
use std::collections::HashMap;

use mast_proto::{
    CreateWorkItemInstructionParams, CreateWorkItemParams, FieldPatch, InstructionScope,
    MoveWorkItemParams, TemplateNode, UpdateWorkItemParams, WorkItemChanges, WorkItemTemplateLink,
    WorkItemTemplateView, WorkOutlineMutation, WorkOutlinePlacement, WorkOutlineSnapshot,
    derive_id, roles,
};

use super::templates::step_operation_id;

/// A child's chosen destination: a template node path below the root
/// (`None` = stays where it is).
pub(crate) type MoveChoice = (String, Option<Vec<String>>);

/// One existing child that moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConvertMove {
    pub child_id: String,
    pub child_title: String,
    pub to_id: String,
    /// "Tickets", "Planning / Sub".
    pub to_label: String,
}

/// What a conversion does, for the preview and for Apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConvertPlan {
    pub item_id: String,
    pub template_id: String,
    pub template_name: String,
    /// Labels of the items it creates, in order.
    pub created: Vec<String>,
    /// Their ids, in order.
    pub created_ids: Vec<String>,
    /// Created items whose parent is not created here (archiving them takes
    /// their subtree along).
    pub created_roots: Vec<String>,
    pub moves: Vec<ConvertMove>,
    /// The item is not a project yet (it always becomes one).
    pub marks_project: bool,
    /// The worker destination it sets: `(item id, label)`.
    pub destination: Option<(String, String)>,
    /// Template instructions the existing nodes lack, as "label: n".
    pub unapplied: Vec<String>,
    /// The revision the link records (0 = not reviewed yet).
    pub link_revision: u32,
    /// Everything to send, in order.
    pub mutations: Vec<WorkOutlineMutation>,
    pub first_operation_id: String,
}

fn live_title_match<'a>(
    snapshot: &'a WorkOutlineSnapshot,
    parent: &str,
    title: &str,
) -> Option<&'a str> {
    snapshot
        .item(parent)?
        .child_ids
        .iter()
        .filter_map(|id| snapshot.item(id))
        .find(|child| child.archived_at.is_none() && child.title == title)
        .map(|child| child.id.as_str())
}

/// The path (below the root) of the first worker-destination node.
fn destination_path(node: &TemplateNode, path: &mut Vec<String>) -> Option<Vec<String>> {
    for child in &node.children {
        path.push(child.title.clone());
        if child.is_worker_destination {
            return Some(path.clone());
        }
        if let Some(found) = destination_path(child, path) {
            return Some(found);
        }
        path.pop();
    }
    None
}

/// Each live child of the item with its default destination: children with an
/// external link go to the template's worker destination (when it has one and
/// the child is not itself a template node); everything else stays.
pub(crate) fn default_moves(
    snapshot: &WorkOutlineSnapshot,
    item_id: &str,
    template: &WorkItemTemplateView,
) -> Vec<MoveChoice> {
    let Some(item) = snapshot.item(item_id) else {
        return Vec::new();
    };
    let destination = destination_path(&template.body, &mut Vec::new());
    item.child_ids
        .iter()
        .filter_map(|id| snapshot.item(id))
        .filter(|child| child.archived_at.is_none())
        .map(|child| {
            let is_template_node = template
                .body
                .children
                .iter()
                .any(|node| node.title == child.title);
            let target = (!child.external_reference_ids.is_empty() && !is_template_node)
                .then(|| destination.clone())
                .flatten();
            (child.id.clone(), target)
        })
        .collect()
}

/// Plan the conversion of `item_id` onto `template`. `Err` is a sentence for
/// the user (nothing is planned).
pub(crate) fn plan_conversion(
    snapshot: &WorkOutlineSnapshot,
    item_id: &str,
    template: &WorkItemTemplateView,
    choices: &[MoveChoice],
    operation_key: &str,
) -> Result<ConvertPlan, String> {
    let item = snapshot
        .item(item_id)
        .ok_or("This work item is not loaded")?;
    if item.archived_at.is_some() {
        return Err("Restore this work item first".into());
    }
    if item.is_project {
        return Err("This item is already a project".into());
    }
    let live_texts = |id: &str| -> Vec<&str> {
        snapshot
            .instructions
            .iter()
            .filter(|i| i.work_item_id.as_deref() == Some(id) && !i.archived())
            .map(|i| i.text.as_str())
            .collect()
    };

    let mut counter = 0usize;
    let mut next_op = || {
        let op = step_operation_id(operation_key, counter);
        counter += 1;
        op
    };
    let mut mutations = Vec::new();
    let mut created = Vec::new();
    let mut created_ids: Vec<String> = Vec::new();
    let mut created_roots = Vec::new();
    let mut unapplied = Vec::new();
    // Template path -> (item id, created here).
    let mut resolved: HashMap<Vec<String>, (String, bool)> = HashMap::new();
    resolved.insert(Vec::new(), (item.id.clone(), false));

    // Pre-order: every node's parent is resolved before it.
    let mut stack: Vec<(Vec<String>, &TemplateNode)> = vec![(Vec::new(), &template.body)];
    let mut order: Vec<(Vec<String>, &TemplateNode)> = Vec::new();
    while let Some((path, node)) = stack.pop() {
        order.push((path.clone(), node));
        for child in node.children.iter().rev() {
            let mut child_path = path.clone();
            child_path.push(child.title.clone());
            stack.push((child_path, child));
        }
    }
    for (path, node) in &order {
        let label = if path.is_empty() {
            item.title.clone()
        } else {
            path.join(" / ")
        };
        if path.is_empty() {
            let lacking = node
                .instructions
                .iter()
                .filter(|text| !live_texts(&item.id).contains(&text.as_str()))
                .count();
            if lacking > 0 {
                unapplied.push(format!("{label}: {lacking}"));
            }
            continue;
        }
        let parent_path = path[..path.len() - 1].to_vec();
        let (parent_id, parent_created) = resolved
            .get(&parent_path)
            .cloned()
            .ok_or("The template is malformed")?;
        let title = &node.title;
        // An existing child of an existing parent is reused.
        let existing = (!parent_created)
            .then(|| live_title_match(snapshot, &parent_id, title))
            .flatten();
        if let Some(existing) = existing {
            let lacking = node
                .instructions
                .iter()
                .filter(|text| !live_texts(existing).contains(&text.as_str()))
                .count();
            if lacking > 0 {
                unapplied.push(format!("{label}: {lacking}"));
            }
            resolved.insert(path.clone(), (existing.to_owned(), false));
            continue;
        }
        let op = next_op();
        let id = derive_id(&op, roles::ITEM);
        mutations.push(WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: item.space_id.clone(),
            parent_work_item_id: Some(parent_id),
            placement: WorkOutlinePlacement::Last,
            title: title.clone(),
            description: node.description.clone(),
            status: None,
            operation_id: op,
        }));
        for text in &node.instructions {
            mutations.push(WorkOutlineMutation::CreateWorkItemInstruction(
                CreateWorkItemInstructionParams {
                    space_id: item.space_id.clone(),
                    scope: InstructionScope::WorkItem {
                        work_item_id: id.clone(),
                    },
                    text: text.clone(),
                    placement: None,
                    source: None,
                    operation_id: next_op(),
                },
            ));
        }
        if !parent_created {
            created_roots.push(id.clone());
        }
        created.push(label);
        created_ids.push(id.clone());
        resolved.insert(path.clone(), (id, true));
    }

    // Existing children that move.
    let matched_ids: Vec<&String> = resolved
        .values()
        .filter(|(_, was_created)| !was_created)
        .map(|(id, _)| id)
        .collect();
    let mut moves = Vec::new();
    for (child_id, target) in choices {
        let Some(path) = target else { continue };
        let child = snapshot
            .item(child_id)
            .filter(|child| child.archived_at.is_none() && child.parent_work_item_id.as_deref() == Some(item_id))
            .ok_or("A child to move is no longer there")?;
        let (to_id, _) = resolved
            .get(path)
            .ok_or_else(|| format!("The template has no \u{201c}{}\u{201d}", path.join(" / ")))?;
        // Template nodes that already exist stay; nothing moves into itself.
        if matched_ids.contains(&&child.id) || &child.id == to_id {
            continue;
        }
        mutations.push(WorkOutlineMutation::MoveWorkItem(MoveWorkItemParams {
            work_item_id: child.id.clone(),
            destination_parent_work_item_id: Some(to_id.clone()),
            placement: WorkOutlinePlacement::Last,
            operation_id: next_op(),
        }));
        moves.push(ConvertMove {
            child_id: child.id.clone(),
            child_title: child.title.clone(),
            to_id: to_id.clone(),
            to_label: path.join(" / "),
        });
    }

    // The item itself: project, destination, template link.
    let update = |changes: WorkItemChanges, operation_id: String| {
        WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id: item.id.clone(),
            changes,
            operation_id,
        })
    };
    mutations.push(update(
        WorkItemChanges {
            is_project: Some(true),
            ..Default::default()
        },
        next_op(),
    ));
    let destination = destination_path(&template.body, &mut Vec::new())
        .and_then(|path| resolved.get(&path).map(|(id, _)| (id.clone(), path.join(" / "))));
    if let Some((id, _)) = &destination {
        mutations.push(update(
            WorkItemChanges {
                worker_destination: FieldPatch::Set(id.clone()),
                ..Default::default()
            },
            next_op(),
        ));
    }
    let link_revision = if unapplied.is_empty() { template.revision } else { 0 };
    mutations.push(update(
        WorkItemChanges {
            template: FieldPatch::Set(WorkItemTemplateLink {
                template_id: template.id.clone(),
                revision: link_revision,
            }),
            ..Default::default()
        },
        next_op(),
    ));
    Ok(ConvertPlan {
        item_id: item.id.clone(),
        template_id: template.id.clone(),
        template_name: template.name.clone(),
        created,
        created_ids,
        created_roots,
        moves,
        marks_project: true,
        destination,
        unapplied,
        link_revision,
        first_operation_id: step_operation_id(operation_key, 0),
        mutations,
    })
}

/// The preview's lines, in reading order. Nothing here is sent.
pub(crate) fn preview_lines(plan: &ConvertPlan) -> Vec<String> {
    let mut lines = Vec::new();
    for label in &plan.created {
        lines.push(format!("Create \u{201c}{label}\u{201d}"));
    }
    for mv in &plan.moves {
        lines.push(format!(
            "Move \u{201c}{}\u{201d} under \u{201c}{}\u{201d}",
            mv.child_title, mv.to_label
        ));
    }
    lines.push("Mark as a project".into());
    if let Some((_, label)) = &plan.destination {
        lines.push(format!("Worker destination: \u{201c}{label}\u{201d}"));
    }
    if plan.unapplied.is_empty() {
        lines.push(format!(
            "Link template \u{201c}{}\u{201d} (up to date)",
            plan.template_name
        ));
    } else {
        lines.push(format!(
            "Link template \u{201c}{}\u{201d} as not reviewed yet: its instructions differ on {} (use Review changes\u{2026})",
            plan.template_name,
            plan.unapplied.join(", ")
        ));
    }
    lines.push("Chats stay where they are".into());
    lines
}
