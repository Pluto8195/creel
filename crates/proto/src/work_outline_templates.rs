//! Project templates (contract amendment "project templates", design option A
//! "Template as a copy"): the wire types of a frozen project subtree, the two
//! mutations that save and delete one, and the pure helpers clients compose
//! into "new project from template" and "review template changes".
//!
//! A template is a FROZEN COPY: titles, descriptions, each item's live
//! instruction texts (in order), which item is a project and which is the
//! worker destination. Chats, tickets (the destination container's children),
//! items with external references, artifacts and archived items are dropped.

use serde::{Deserialize, Serialize};

use crate::work_outline::{WorkItemView, WorkOutlineSnapshot};

/// Template limits (the contract amendment lists them).
pub mod template_limits {
    /// Nodes of one template body.
    pub const MAX_TEMPLATE_NODES: usize = 64;
    /// Levels of one template body, the root counting as level 1.
    pub const MAX_TEMPLATE_DEPTH: usize = 6;
    /// Instruction entries of one node.
    pub const MAX_TEMPLATE_INSTRUCTIONS_PER_NODE: usize = 16;
    /// A template name, in Unicode scalar values (after trimming).
    pub const MAX_TEMPLATE_NAME_CHARS: usize = 80;
    /// Serialized body size, in UTF-16 code units: leaves room under
    /// `limits::MAX_OP_UTF16_UNITS` for the row's other fields and clocks.
    pub const MAX_TEMPLATE_BODY_UTF16_UNITS: usize = 13_000;
}

/// One node of a template body.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateNode {
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The item is a project (`isProject`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_project: bool,
    /// The item is the worker destination of its project.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_worker_destination: bool,
    /// Live instruction texts of the item, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instructions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TemplateNode>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// One template of a Space (registry kind `workItemTemplates`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemTemplateView {
    pub id: String,
    #[serde(default)]
    pub space_id: String,
    #[serde(default)]
    pub name: String,
    /// Starts at 1; every re-save adds 1.
    #[serde(default)]
    pub revision: u32,
    #[serde(default)]
    pub body: TemplateNode,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

/// `changes.template` of `updateWorkItem`: the template (and revision) a
/// project was created from or last reviewed against. Users and system only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemTemplateLink {
    pub template_id: String,
    pub revision: u32,
}

/// `saveWorkItemTemplate`: freeze the live subtree of a project into a
/// template. `templateId: null` creates a template (reply `templateId` =
/// `derive_id(operationId, "template")`); `Some(id)` replaces that template's
/// body and name and adds 1 to its revision. Also links the source project to
/// the saved template and revision. User action only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveWorkItemTemplateParams {
    pub space_id: String,
    pub project_work_item_id: String,
    pub name: String,
    #[serde(default)]
    pub template_id: Option<String>,
    pub operation_id: String,
}

/// `deleteWorkItemTemplate`: tombstone the template row. Projects keep their
/// link, which then reads as dangling. Reply `{}`. User action only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteWorkItemTemplateParams {
    pub template_id: String,
    pub operation_id: String,
}

impl TemplateNode {
    /// Nodes of this subtree, this node included.
    pub fn node_count(&self) -> usize {
        1 + self.children.iter().map(Self::node_count).sum::<usize>()
    }

    /// Levels of this subtree (a leaf is 1).
    pub fn depth(&self) -> usize {
        1 + self.children.iter().map(Self::depth).max().unwrap_or(0)
    }

    /// Check the size limits ([`template_limits`]). The message is
    /// user-presentable; the registry maps it to `limitExceeded`.
    pub fn check_limits(&self) -> Result<(), String> {
        use template_limits::*;
        if self.node_count() > MAX_TEMPLATE_NODES {
            return Err(format!(
                "A template holds at most {MAX_TEMPLATE_NODES} items"
            ));
        }
        if self.depth() > MAX_TEMPLATE_DEPTH {
            return Err(format!(
                "A template is at most {MAX_TEMPLATE_DEPTH} levels deep"
            ));
        }
        let mut stack = vec![self];
        while let Some(node) = stack.pop() {
            if node.instructions.len() > MAX_TEMPLATE_INSTRUCTIONS_PER_NODE {
                return Err(format!(
                    "A template item holds at most {MAX_TEMPLATE_INSTRUCTIONS_PER_NODE} instructions"
                ));
            }
            stack.extend(node.children.iter());
        }
        Ok(())
    }

    /// Preorder `(path of titles below the root, node)`; the root's path is
    /// empty.
    fn walk<'a>(&'a self, path: &mut Vec<String>, out: &mut Vec<(Vec<String>, &'a TemplateNode)>) {
        out.push((path.clone(), self));
        for child in &self.children {
            path.push(child.title.clone());
            child.walk(path, out);
            path.pop();
        }
    }
}

// --------------------------------------------------------------------------
// Freezing a live project
// --------------------------------------------------------------------------

/// Freeze the live subtree of `project_id` into a template body (pure; the
/// registry's save and the diff both use it). Drops archived items, items
/// with external references (with their subtrees), and the children of every
/// worker destination (tickets). Instructions are the item's live ones in
/// order. `None` when the item is unknown. Limits are NOT checked here
/// ([`TemplateNode::check_limits`]).
pub fn freeze_project_subtree(
    snapshot: &WorkOutlineSnapshot,
    project_id: &str,
) -> Option<TemplateNode> {
    let root = snapshot.item(project_id)?;
    let mut destinations: Vec<&str> = Vec::new();
    let mut stack = vec![root];
    while let Some(item) = stack.pop() {
        if item.is_project
            && let Some(destination) = item.worker_destination_id.as_deref()
        {
            destinations.push(destination);
        }
        stack.extend(item.child_ids.iter().filter_map(|id| snapshot.item(id)));
    }
    Some(freeze_node(snapshot, root, &destinations, 0))
}

fn freeze_node(
    snapshot: &WorkOutlineSnapshot,
    item: &WorkItemView,
    destinations: &[&str],
    depth: usize,
) -> TemplateNode {
    let is_destination = destinations.contains(&item.id.as_str());
    let children = if is_destination || depth > 64 {
        Vec::new()
    } else {
        item.child_ids
            .iter()
            .filter_map(|id| snapshot.item(id))
            .filter(|child| child.archived_at.is_none() && child.external_reference_ids.is_empty())
            .map(|child| freeze_node(snapshot, child, destinations, depth + 1))
            .collect()
    };
    TemplateNode {
        title: item.title.clone(),
        description: item.description.clone().filter(|d| !d.is_empty()),
        is_project: item.is_project,
        is_worker_destination: is_destination,
        instructions: {
            let mut own: Vec<_> = snapshot
                .instructions
                .iter()
                .filter(|i| i.work_item_id.as_deref() == Some(item.id.as_str()) && !i.archived())
                .collect();
            own.sort_by(|a, b| (&a.order_key, &a.id).cmp(&(&b.order_key, &b.id)));
            own.into_iter().map(|i| i.text.clone()).collect()
        },
        children,
    }
}

// --------------------------------------------------------------------------
// Instantiation plan
// --------------------------------------------------------------------------

/// One step of a template instantiation. `path_id`s are plan-local ids
/// (`"n0"` = the root, then preorder); the caller maps each to the real item
/// id its `createWorkItem` returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum InstantiationStep {
    /// `createWorkItem` (last placement) under `parentPathId` (`None` = the
    /// parent the user picked for the new project).
    #[serde(rename_all = "camelCase")]
    CreateItem {
        path_id: String,
        parent_path_id: Option<String>,
        title: String,
        description: Option<String>,
    },
    /// `createWorkItemInstruction` on the item.
    #[serde(rename_all = "camelCase")]
    AddInstruction { path_id: String, text: String },
    /// `updateWorkItem { isProject: true }`.
    #[serde(rename_all = "camelCase")]
    MarkProject { path_id: String },
    /// `updateWorkItem { workerDestination: <destination item> }` on the
    /// project (after `MarkProject`).
    #[serde(rename_all = "camelCase")]
    SetWorkerDestination {
        project_path_id: String,
        destination_path_id: String,
    },
    /// `updateWorkItem { template: {templateId, revision} }` on the new root.
    #[serde(rename_all = "camelCase")]
    LinkTemplate {
        path_id: String,
        template_id: String,
        revision: u32,
    },
}

/// The ordered, flat plan that instantiates `template` as a new project
/// titled `new_title` (pure, no I/O). Order: every `CreateItem` with its
/// `AddInstruction`s in preorder (a parent before its children), then a
/// `MarkProject` per project node, then each project's `SetWorkerDestination`
/// (its nearest-project destination node), finally `LinkTemplate` on the root.
pub fn plan_template_instantiation(
    template: &WorkItemTemplateView,
    new_title: &str,
) -> Vec<InstantiationStep> {
    struct Flat<'a> {
        id: String,
        parent: Option<usize>,
        node: &'a TemplateNode,
    }
    fn flatten<'a>(node: &'a TemplateNode, parent: Option<usize>, out: &mut Vec<Flat<'a>>) {
        let index = out.len();
        out.push(Flat {
            id: format!("n{index}"),
            parent,
            node,
        });
        for child in &node.children {
            flatten(child, Some(index), out);
        }
    }
    let mut flat = Vec::new();
    flatten(&template.body, None, &mut flat);

    let mut steps = Vec::new();
    for (index, entry) in flat.iter().enumerate() {
        steps.push(InstantiationStep::CreateItem {
            path_id: entry.id.clone(),
            parent_path_id: entry.parent.map(|p| flat[p].id.clone()),
            title: if index == 0 {
                new_title.trim().to_owned()
            } else {
                entry.node.title.clone()
            },
            description: entry.node.description.clone(),
        });
        for text in &entry.node.instructions {
            steps.push(InstantiationStep::AddInstruction {
                path_id: entry.id.clone(),
                text: text.clone(),
            });
        }
    }
    for entry in &flat {
        if entry.node.is_project {
            steps.push(InstantiationStep::MarkProject {
                path_id: entry.id.clone(),
            });
        }
    }
    for (index, entry) in flat.iter().enumerate() {
        if !entry.node.is_worker_destination {
            continue;
        }
        // The destination belongs to the nearest project above it.
        let mut cursor = entry.parent;
        while let Some(at) = cursor {
            if flat[at].node.is_project {
                steps.push(InstantiationStep::SetWorkerDestination {
                    project_path_id: flat[at].id.clone(),
                    destination_path_id: flat[index].id.clone(),
                });
                break;
            }
            cursor = flat[at].parent;
        }
    }
    steps.push(InstantiationStep::LinkTemplate {
        path_id: flat[0].id.clone(),
        template_id: template.id.clone(),
        revision: template.revision,
    });
    steps
}

// --------------------------------------------------------------------------
// Diff
// --------------------------------------------------------------------------

/// One difference between a template and a project created from it. Paths are
/// item titles below the project root (the root's own path is empty), so a
/// renamed project still matches. "Added" means the template has it and the
/// project lacks it; "removed" means the project has it and the template does
/// not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TemplateChange {
    /// The template has an item the project lacks. `path` ends with `title`;
    /// `node` is the template subtree (to create it).
    AddedItem {
        path: Vec<String>,
        title: String,
        node: TemplateNode,
    },
    /// The project has an item the template lacks. Reported only; never
    /// deleted automatically.
    RemovedItem { path: Vec<String> },
    /// The template has an instruction the project item lacks.
    InstructionAdded { path: Vec<String>, text: String },
    /// The project item has an instruction the template lacks.
    InstructionRemoved { path: Vec<String>, text: String },
    /// The worker destination differs: `from` is the project's (path of the
    /// destination item), `to` the template's.
    WorkerDestinationChanged {
        from: Option<Vec<String>>,
        to: Option<Vec<String>>,
    },
}

/// Compare `template` (its body at `template.revision`) against the live
/// project subtree `project_item_id`, matching nodes by their path of titles
/// and instructions by text. An empty result means "in sync". The live side
/// is frozen with the same rules as saving, so tickets, archived items and
/// externally linked items never show up.
pub fn diff_template_against_project(
    snapshot: &WorkOutlineSnapshot,
    template: &WorkItemTemplateView,
    project_item_id: &str,
) -> Vec<TemplateChange> {
    let Some(live) = freeze_project_subtree(snapshot, project_item_id) else {
        return Vec::new();
    };
    let mut changes = Vec::new();
    diff_nodes(&template.body, &live, &mut Vec::new(), &mut changes);
    let destination = |root: &TemplateNode| {
        let mut flat = Vec::new();
        root.walk(&mut Vec::new(), &mut flat);
        flat.into_iter()
            .find(|(_, node)| node.is_worker_destination)
            .map(|(path, _)| path)
    };
    let (from, to) = (destination(&live), destination(&template.body));
    if from != to {
        changes.push(TemplateChange::WorkerDestinationChanged { from, to });
    }
    changes
}

fn diff_nodes(
    template: &TemplateNode,
    live: &TemplateNode,
    path: &mut Vec<String>,
    out: &mut Vec<TemplateChange>,
) {
    // Instructions as multisets of text.
    let mut live_texts: Vec<&String> = live.instructions.iter().collect();
    for text in &template.instructions {
        match live_texts.iter().position(|t| *t == text) {
            Some(at) => {
                live_texts.remove(at);
            }
            None => out.push(TemplateChange::InstructionAdded {
                path: path.clone(),
                text: text.clone(),
            }),
        }
    }
    for text in live_texts {
        out.push(TemplateChange::InstructionRemoved {
            path: path.clone(),
            text: text.clone(),
        });
    }
    // Children by title, duplicates matched in order.
    let mut unmatched: Vec<Option<&TemplateNode>> = live.children.iter().map(Some).collect();
    for child in &template.children {
        path.push(child.title.clone());
        let found = unmatched
            .iter()
            .position(|c| c.is_some_and(|c| c.title == child.title));
        match found {
            Some(at) => {
                let live_child = unmatched[at].take().expect("position found a node");
                diff_nodes(child, live_child, path, out);
            }
            None => out.push(TemplateChange::AddedItem {
                path: path.clone(),
                title: child.title.clone(),
                node: child.clone(),
            }),
        }
        path.pop();
    }
    for rest in unmatched.into_iter().flatten() {
        path.push(rest.title.clone());
        out.push(TemplateChange::RemovedItem { path: path.clone() });
        path.pop();
    }
}

/// Whether the template moved on since `project` was created from or last
/// reviewed against it: the project links this template and its recorded
/// revision (a missing one counts as 0) is older.
pub fn template_changed(project: &WorkItemView, template: &WorkItemTemplateView) -> bool {
    project.template_id.as_deref() == Some(template.id.as_str())
        && project.template_revision.unwrap_or(0) < template.revision
}

#[cfg(test)]
mod tests;
