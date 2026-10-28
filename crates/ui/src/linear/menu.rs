//! Linear entries of the Outline's right-click menus (Linear-enabled Spaces
//! only): an issue-linked item gets Open in Linear / Refresh from Linear /
//! Unlink Linear issue; a project-linked item gets Add Issues from
//! <project>…; a Space row gets Open Linear Panel, which shows the
//! Toggle Linear Panel binding. The Shell runs them
//! (`shell/linear_ui.rs`).
use zeron_proto::WorkOutlineSnapshot;

use crate::icons;
use crate::work_outline::WorkOutlineCommand;
use crate::work_outline::hints::HintKey;
use crate::work_outline::outline_view::context_menu::{MenuAction, MenuEntry, OutlineContextMenu};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinearMenuAction {
    /// Open the issue URL (web-link preference).
    OpenInLinear(String),
    /// `LinearRefreshLinkedItems` for the Space and a fresh fetch of the issue.
    RefreshFromLinear {
        space_id: String,
        identifier: String,
    },
    /// Remove the item's Linear issue reference (by reference id).
    UnlinkIssue(String),
    /// Open the Linear panel on this Space.
    OpenPanel(String),
    /// Open "Add issues from <project>…" on a project-linked item.
    AddProjectIssues { space_id: String, item_id: String },
    /// `setLinearAssignedDestination` to this item (`None` clears it).
    SetAssignedDestination {
        space_id: String,
        item_id: Option<String>,
    },
}

impl LinearMenuAction {
    pub(crate) fn hint(&self) -> HintKey {
        match self {
            Self::OpenPanel(_) => HintKey::Command(WorkOutlineCommand::OpenLinear),
            _ => HintKey::None,
        }
    }
}

/// The entries an item's menu adds (none outside Linear-enabled Spaces or
/// for items without a Linear issue or project).
pub(crate) fn item_entries(
    snapshot: Option<&WorkOutlineSnapshot>,
    item_id: &str,
) -> Vec<MenuEntry> {
    let Some(snapshot) = snapshot.filter(|s| super::space_enabled(Some(s))) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    if let Some(reference) = snapshot.linear_issue_reference(item_id) {
        entries.extend([
            MenuEntry::new(
                "linear-open",
                "Open in Linear",
                icons::ARROW_UP_RIGHT,
                MenuAction::Linear(LinearMenuAction::OpenInLinear(reference.url.clone())),
            ),
            MenuEntry::new(
                "linear-refresh",
                "Refresh from Linear",
                icons::REFRESH,
                MenuAction::Linear(LinearMenuAction::RefreshFromLinear {
                    space_id: snapshot.space_id.clone(),
                    identifier: reference.external_id.clone(),
                }),
            ),
            MenuEntry::new(
                "linear-unlink",
                "Unlink Linear Issue",
                icons::CLOSE_CIRCLE,
                MenuAction::Linear(LinearMenuAction::UnlinkIssue(reference.id.clone())),
            ),
        ]);
    }
    if let Some(reference) = snapshot.linear_project_reference(item_id) {
        let name = reference
            .cached_title
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "Project".into());
        entries.push(MenuEntry::new(
            "linear-add-issues",
            format!("Add Issues from {name}\u{2026}"),
            icons::CHECKLIST,
            MenuAction::Linear(LinearMenuAction::AddProjectIssues {
                space_id: snapshot.space_id.clone(),
                item_id: item_id.to_string(),
            }),
        ));
    }
    let live = snapshot
        .item(item_id)
        .is_some_and(|item| item.archived_at.is_none());
    let destination = snapshot.linear_assigned_destination() == Some(item_id);
    if live {
        entries.push(if destination {
            MenuEntry::new(
                "linear-clear-assigned",
                "Stop Adding My Unstarted Tickets Here",
                icons::CLOSE_CIRCLE,
                MenuAction::Linear(LinearMenuAction::SetAssignedDestination {
                    space_id: snapshot.space_id.clone(),
                    item_id: None,
                }),
            )
        } else {
            MenuEntry::new(
                "linear-use-assigned",
                "Auto-add My Unstarted Linear Tickets Here",
                icons::CHECKLIST,
                MenuAction::Linear(LinearMenuAction::SetAssignedDestination {
                    space_id: snapshot.space_id.clone(),
                    item_id: Some(item_id.to_string()),
                }),
            )
        });
    }
    if let Some(first) = entries.first_mut() {
        first.separator_before = true;
    }
    entries
}

/// The entry a Space row's menu adds (Linear-enabled Spaces only).
pub(crate) fn space_entries(
    snapshot: Option<&WorkOutlineSnapshot>,
    space_id: &str,
) -> Vec<MenuEntry> {
    if !super::space_enabled(snapshot) {
        return Vec::new();
    }
    vec![MenuEntry::new(
        "linear-panel",
        "Open Linear Panel",
        icons::CHECKLIST,
        MenuAction::Linear(LinearMenuAction::OpenPanel(space_id.to_string())),
    )]
}

/// Put [`item_entries`] before Archive (the menu's destructive tail).
pub(crate) fn insert_item_entries(
    menu: &mut OutlineContextMenu,
    snapshot: Option<&WorkOutlineSnapshot>,
    item_id: &str,
) {
    let entries = item_entries(snapshot, item_id);
    let at = menu
        .entries
        .iter()
        .position(|entry| entry.id == "archive")
        .unwrap_or(menu.entries.len());
    menu.entries.splice(at..at, entries);
}

/// Put [`space_entries`] after Required Actions.
pub(crate) fn insert_space_entries(
    menu: &mut OutlineContextMenu,
    snapshot: Option<&WorkOutlineSnapshot>,
    space_id: &str,
) {
    let entries = space_entries(snapshot, space_id);
    let at = menu
        .entries
        .iter()
        .position(|entry| entry.id == "required-actions")
        .map_or(menu.entries.len(), |ix| ix + 1);
    menu.entries.splice(at..at, entries);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_outline::store::fixture_ids::*;

    #[test]
    fn entries_only_in_enabled_spaces_on_linked_items() {
        let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
        snapshot.external_references = vec![super::super::fixture::reference(
            "r1",
            "s1",
            API,
            "issue",
            "ENG-1",
            "https://linear.app/acme/issue/eng-1",
        )];
        assert!(item_entries(Some(&snapshot), API).is_empty(), "Linear off");
        assert!(space_entries(Some(&snapshot), "s1").is_empty());
        snapshot.space_settings.linear_enabled = true;
        let ids: Vec<_> = item_entries(Some(&snapshot), API)
            .iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(
            ids,
            [
                "linear-open",
                "linear-refresh",
                "linear-unlink",
                "linear-use-assigned"
            ]
        );
        let unlinked: Vec<_> = item_entries(Some(&snapshot), UI)
            .iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(unlinked, ["linear-use-assigned"], "not linked");
        assert!(
            item_entries(Some(&snapshot), RETIRED).is_empty(),
            "archived"
        );
        let mut project = super::super::fixture::reference(
            "r2",
            "s1",
            UI,
            "project",
            "p-1",
            "https://linear.app/acme/project/ui-abc",
        );
        project.cached_title = Some("ReceiveAI".into());
        snapshot.external_references.push(project);
        let entries = item_entries(Some(&snapshot), UI);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "linear-add-issues");
        assert_eq!(entries[0].label, "Add Issues from ReceiveAI\u{2026}");
        assert!(entries[0].separator_before);
        snapshot.space_settings.linear_assigned_work_item_id = Some(UI.into());
        let on_destination: Vec<_> = item_entries(Some(&snapshot), UI)
            .iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(
            on_destination,
            ["linear-add-issues", "linear-clear-assigned"]
        );
        snapshot.space_settings.linear_enabled = false;
        assert!(item_entries(Some(&snapshot), UI).is_empty(), "Linear off");
        snapshot.space_settings.linear_enabled = true;
        let space = space_entries(Some(&snapshot), "s1");
        assert_eq!(
            space[0].hint,
            HintKey::Command(WorkOutlineCommand::OpenLinear)
        );
    }
}
