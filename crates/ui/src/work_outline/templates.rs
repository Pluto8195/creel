//! Project templates, the pure parts: turning a template into the ordered
//! mutation list a "New from template" sends (ids derived from ONE operation
//! key, every id <= 64 chars, retries replay), and the right-click entries.
//!
//! A template is a frozen copy (`mast_proto::WorkItemTemplateView`);
//! instantiation is a client composition of existing mutations
//! (`createWorkItem`, `createWorkItemInstruction`, `updateWorkItem`), sent in
//! order, each awaited, because later steps name items earlier ones create.
//! The view-side flow lives in `outline_view/templates.rs`.
use std::collections::HashMap;

use mast_proto::{
    CreateWorkItemInstructionParams, CreateWorkItemParams, FieldPatch, InstantiationStep,
    InstructionScope, UpdateWorkItemParams, WorkItemChanges, WorkItemTemplateLink,
    WorkItemTemplateView, WorkOutlineMutation, WorkOutlinePlacement, WorkOutlineSnapshot,
    derive_id, plan_template_instantiation, roles,
};

use super::hints::HintKey;
use super::outline_view::context_menu::{MenuAction, MenuEntry, OutlineContextMenu};
use crate::icons;

/// The ordered requests that create one project from a template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TemplateRun {
    /// In send order; await each before the next.
    pub mutations: Vec<WorkOutlineMutation>,
    /// The new project root's work item id.
    pub root_id: String,
    /// Every created item id in plan order (root first).
    pub item_ids: Vec<String>,
    /// The first request's operation id (the undo entry's key while pending).
    pub first_operation_id: String,
}

/// The operation id of plan step `index` under `operation_key`.
pub(crate) fn step_operation_id(operation_key: &str, index: usize) -> String {
    derive_id(operation_key, &format!("template-step:{index}"))
}

/// The mutations that instantiate `template` as a project titled `title`
/// under `root_parent` (`None` = a Space-level root). Pure and deterministic:
/// the same `operation_key` always yields the same operation and item ids
/// (32 hex characters each), so a retry converges on the same items.
pub(crate) fn plan_instantiation(
    space_id: &str,
    root_parent: Option<&str>,
    template: &WorkItemTemplateView,
    title: &str,
    operation_key: &str,
) -> TemplateRun {
    let steps = plan_template_instantiation(template, title);
    let ops: Vec<String> = (0..steps.len())
        .map(|ix| step_operation_id(operation_key, ix))
        .collect();
    // A path id's real item id: derived from its CreateItem step's operation.
    let mut ids: HashMap<&str, String> = HashMap::new();
    let mut item_ids = Vec::new();
    for (step, op) in steps.iter().zip(&ops) {
        if let InstantiationStep::CreateItem { path_id, .. } = step {
            let id = derive_id(op, roles::ITEM);
            item_ids.push(id.clone());
            ids.insert(path_id.as_str(), id);
        }
    }
    let item = |path: &str| ids.get(path).cloned().unwrap_or_default();
    let update = |work_item_id: String, changes: WorkItemChanges, operation_id: &String| {
        WorkOutlineMutation::UpdateWorkItem(UpdateWorkItemParams {
            work_item_id,
            changes,
            operation_id: operation_id.clone(),
        })
    };
    let mutations = steps
        .iter()
        .zip(&ops)
        .map(|(step, op)| match step {
            InstantiationStep::CreateItem {
                parent_path_id,
                title,
                description,
                ..
            } => WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
                space_id: space_id.to_owned(),
                parent_work_item_id: match parent_path_id {
                    Some(path) => Some(item(path)),
                    None => root_parent.map(str::to_owned),
                },
                placement: WorkOutlinePlacement::Last,
                title: title.clone(),
                description: description.clone(),
                status: None,
                operation_id: op.clone(),
            }),
            InstantiationStep::AddInstruction { path_id, text } => {
                WorkOutlineMutation::CreateWorkItemInstruction(CreateWorkItemInstructionParams {
                    space_id: space_id.to_owned(),
                    scope: InstructionScope::WorkItem {
                        work_item_id: item(path_id),
                    },
                    text: text.clone(),
                    placement: None,
                    source: None,
                    operation_id: op.clone(),
                })
            }
            InstantiationStep::MarkProject { path_id } => update(
                item(path_id),
                WorkItemChanges {
                    is_project: Some(true),
                    ..Default::default()
                },
                op,
            ),
            InstantiationStep::SetWorkerDestination {
                project_path_id,
                destination_path_id,
            } => update(
                item(project_path_id),
                WorkItemChanges {
                    worker_destination: FieldPatch::Set(item(destination_path_id)),
                    ..Default::default()
                },
                op,
            ),
            InstantiationStep::LinkTemplate {
                path_id,
                template_id,
                revision,
            } => update(
                item(path_id),
                WorkItemChanges {
                    template: FieldPatch::Set(WorkItemTemplateLink {
                        template_id: template_id.clone(),
                        revision: *revision,
                    }),
                    ..Default::default()
                },
                op,
            ),
        })
        .collect();
    TemplateRun {
        mutations,
        root_id: item_ids.first().cloned().unwrap_or_default(),
        item_ids,
        first_operation_id: ops.first().cloned().unwrap_or_default(),
    }
}

/// "name · rev N · K items", the Templates list's line.
pub(crate) fn summary_line(template: &WorkItemTemplateView) -> String {
    let count = template.body.node_count();
    format!(
        "rev {} \u{b7} {} item{}",
        template.revision,
        count,
        if count == 1 { "" } else { "s" }
    )
}

// --------------------------------------------------------------------------
// Menu entries
// --------------------------------------------------------------------------

/// The template entries of the Outline's right-click menus (the Shell runs
/// them, `shell/work_outline_menu_ui.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TemplateMenuAction {
    /// Open the save-as-template prompt in the item's details.
    SaveAsTemplate { space_id: String, item_id: String },
    /// "New from template…": under `parent` (`None` = the Space's top level).
    NewFromTemplate {
        space_id: String,
        parent: Option<String>,
    },
}

/// Why "Save as Template…" is unavailable for `item_id`, if it is.
pub(crate) fn save_blocked(
    snapshot: Option<&WorkOutlineSnapshot>,
    item_id: &str,
) -> Option<&'static str> {
    let Some(item) = snapshot.and_then(|snapshot| snapshot.item(item_id)) else {
        return Some("This project's outline is not loaded");
    };
    if item.archived_at.is_some() {
        return Some("Restore this work item first");
    }
    (!item.is_project).then_some("Mark it as a project first")
}

/// Put "Save as Template…" and "New from Template…" after "Group into…".
pub(crate) fn insert_item_entries(
    menu: &mut OutlineContextMenu,
    snapshot: Option<&WorkOutlineSnapshot>,
    space_id: &str,
    item_id: &str,
) {
    let archived = snapshot
        .and_then(|snapshot| snapshot.item(item_id))
        .is_some_and(|item| item.archived_at.is_some());
    let entries = [
        MenuEntry::new(
            "save-as-template",
            "Save as Template\u{2026}",
            icons::DOCUMENT_ADD,
            MenuAction::Template(TemplateMenuAction::SaveAsTemplate {
                space_id: space_id.to_owned(),
                item_id: item_id.to_owned(),
            }),
        )
        .disabled(save_blocked(snapshot, item_id))
        .separated(),
        MenuEntry::new(
            "new-from-template",
            "New from Template\u{2026}",
            icons::ADD_CIRCLE,
            MenuAction::Template(TemplateMenuAction::NewFromTemplate {
                space_id: space_id.to_owned(),
                parent: Some(item_id.to_owned()),
            }),
        )
        .disabled(archived.then_some("Restore this work item to add a project under it")),
    ];
    let at = menu
        .entries
        .iter()
        .position(|entry| entry.id == "group-into")
        .map_or(menu.entries.len(), |ix| ix + 1);
    menu.entries.splice(at..at, entries);
}

/// Put "New from Template…" after "New Work Item" on a Space root's menu.
pub(crate) fn insert_space_entries(
    menu: &mut OutlineContextMenu,
    space_id: &str,
    unavailable: Option<&'static str>,
) {
    let entry = MenuEntry::new(
        "new-from-template",
        "New from Template\u{2026}",
        icons::ADD_CIRCLE,
        MenuAction::Template(TemplateMenuAction::NewFromTemplate {
            space_id: space_id.to_owned(),
            parent: None,
        }),
    )
    .disabled(unavailable);
    let at = menu
        .entries
        .iter()
        .position(|entry| entry.id == "new-item")
        .map_or(menu.entries.len(), |ix| ix + 1);
    menu.entries.insert(at, entry);
}

impl TemplateMenuAction {
    pub(crate) fn hint(&self) -> HintKey {
        HintKey::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::TemplateNode;

    pub(crate) fn node(title: &str, children: Vec<TemplateNode>) -> TemplateNode {
        TemplateNode {
            title: title.into(),
            description: None,
            is_project: false,
            is_worker_destination: false,
            instructions: Vec::new(),
            children,
        }
    }

    pub(crate) fn sample_template() -> WorkItemTemplateView {
        let mut tickets = node("Tickets", vec![]);
        tickets.is_worker_destination = true;
        let mut docs = node("Docs", vec![]);
        docs.description = Some("Write the docs".into());
        docs.instructions = vec!["Keep it short.".into()];
        let mut root = node("Launch", vec![tickets, docs]);
        root.is_project = true;
        WorkItemTemplateView {
            id: "t-1".into(),
            space_id: "s1".into(),
            name: "Launch kit".into(),
            revision: 3,
            body: root,
            created_at: 1,
            updated_at: 2,
        }
    }

    #[test]
    fn instantiation_ids_are_deterministic_and_short() {
        let template = sample_template();
        let a = plan_instantiation("s1", Some("p"), &template, "Q3 launch", "key-1");
        let b = plan_instantiation("s1", Some("p"), &template, "Q3 launch", "key-1");
        assert_eq!(a, b, "same key, same ids");
        let c = plan_instantiation("s1", Some("p"), &template, "Q3 launch", "key-2");
        assert_ne!(a.root_id, c.root_id);
        let long_key = "k".repeat(500);
        let d = plan_instantiation("s1", None, &template, "x", &long_key);
        for mutation in a.mutations.iter().chain(&d.mutations) {
            let id = mutation.operation_id();
            assert!(!id.is_empty() && id.len() <= 64, "{id}");
        }
        for id in a.item_ids.iter().chain(&d.item_ids) {
            assert!(id.len() <= 64, "{id}");
        }
        let mut unique = a.item_ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 3, "one id per node");
    }

    #[test]
    fn mutations_follow_the_plan_in_order() {
        let template = sample_template();
        let run = plan_instantiation("s1", Some("parent"), &template, "  Q3 launch ", "k");
        let ops: Vec<&str> = run.mutations.iter().map(|m| m.op_name()).collect();
        assert_eq!(
            ops,
            [
                "createWorkItem",
                "createWorkItem",
                "createWorkItem",
                "createWorkItemInstruction",
                "updateWorkItem",
                "updateWorkItem",
                "updateWorkItem",
            ]
        );
        let WorkOutlineMutation::CreateWorkItem(root) = &run.mutations[0] else {
            panic!("root first");
        };
        assert_eq!(root.title, "Q3 launch");
        assert_eq!(root.parent_work_item_id.as_deref(), Some("parent"));
        let WorkOutlineMutation::CreateWorkItem(child) = &run.mutations[1] else {
            panic!();
        };
        assert_eq!(child.parent_work_item_id.as_deref(), Some(run.root_id.as_str()));
        let top = plan_instantiation("s1", None, &template, "x", "k");
        let WorkOutlineMutation::CreateWorkItem(root) = &top.mutations[0] else {
            panic!();
        };
        assert_eq!(root.parent_work_item_id, None);
    }
}
