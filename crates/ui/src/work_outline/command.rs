//! Seam 1: the nine Work Outline commands and the confirmation request type.
//!
//! Every entry point (command palette, keymap actions, Outline toolbar) forwards
//! to `Shell::run_work_outline_command`, which routes to
//! `Overview::run_work_outline_command`. This file only names the commands; it
//! owns no behavior.
use gpui::{SharedString, Window};

/// The Work Outline commands, in palette order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WorkOutlineCommand {
    OpenOutline,
    CreateWorkItem,
    CreateChildWorkItem,
    RenameWorkItem,
    MoveWorkItem,
    AssociateChatWithWorkItem,
    OpenEffectiveInstructions,
    CreateInstructionFromChat,
    OpenRequiredActions,
}

impl WorkOutlineCommand {
    pub(crate) const ALL: [WorkOutlineCommand; 9] = [
        Self::OpenOutline,
        Self::CreateWorkItem,
        Self::CreateChildWorkItem,
        Self::RenameWorkItem,
        Self::MoveWorkItem,
        Self::AssociateChatWithWorkItem,
        Self::OpenEffectiveInstructions,
        Self::CreateInstructionFromChat,
        Self::OpenRequiredActions,
    ];

    /// Palette / menu label (sentence-style, matches the existing palette).
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::OpenOutline => "Open Outline",
            Self::CreateWorkItem => "Create Work Item",
            Self::CreateChildWorkItem => "Create Child Work Item",
            Self::RenameWorkItem => "Rename Work Item",
            Self::MoveWorkItem => "Move Work Item",
            Self::AssociateChatWithWorkItem => "Associate Chat with Work Item",
            Self::OpenEffectiveInstructions => "Open Effective Instructions",
            Self::CreateInstructionFromChat => "Create Instruction from Chat",
            Self::OpenRequiredActions => "Open Required Actions",
        }
    }

    /// Stable id: camelCase, never renamed. It is the persisted/keymap field
    /// name (`KeymapConfig`) and the test selector suffix, so it must not
    /// change once shipped.
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::OpenOutline => "openOutline",
            Self::CreateWorkItem => "createWorkItem",
            Self::CreateChildWorkItem => "createChildWorkItem",
            Self::RenameWorkItem => "renameWorkItem",
            Self::MoveWorkItem => "moveWorkItem",
            Self::AssociateChatWithWorkItem => "associateChatWithWorkItem",
            Self::OpenEffectiveInstructions => "openEffectiveInstructions",
            Self::CreateInstructionFromChat => "createInstructionFromChat",
            Self::OpenRequiredActions => "openRequiredActions",
        }
    }

    pub(crate) fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|command| command.id() == id)
    }

    /// Commands that act on the selected work item. With none selected they
    /// open the Outline and show a short notice; they are never hidden
    /// (contract section 7).
    pub(crate) fn needs_selected_work_item(self) -> bool {
        matches!(
            self,
            Self::CreateChildWorkItem
                | Self::RenameWorkItem
                | Self::MoveWorkItem
                | Self::OpenEffectiveInstructions
                | Self::OpenRequiredActions
        )
    }

    /// Commands that act on the active or selected Chat.
    pub(crate) fn needs_chat(self) -> bool {
        matches!(
            self,
            Self::AssociateChatWithWorkItem | Self::CreateInstructionFromChat
        )
    }
}

/// A confirmation the Overview asks the Shell to show as a window modal
/// (discard unsaved draft, archive with descendants, move with descendants).
/// The decision comes back through `on_decision` with `true` = confirmed;
/// closing the dialog any other way reports `false`.
pub(crate) struct WorkOutlineConfirmRequest {
    pub title: SharedString,
    pub body: SharedString,
    pub confirm_label: SharedString,
    pub cancel_label: SharedString,
    /// Render the confirm button as destructive.
    pub destructive: bool,
    pub on_decision: Box<dyn FnOnce(bool, &mut Window, &mut gpui::App) + 'static>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn nine_commands_with_unique_labels_and_ids() {
        assert_eq!(WorkOutlineCommand::ALL.len(), 9);
        let ids: HashSet<_> = WorkOutlineCommand::ALL.iter().map(|c| c.id()).collect();
        let labels: HashSet<_> = WorkOutlineCommand::ALL.iter().map(|c| c.label()).collect();
        assert_eq!(ids.len(), 9);
        assert_eq!(labels.len(), 9);
        assert!(
            WorkOutlineCommand::ALL
                .iter()
                .all(|c| !c.label().is_empty())
        );
    }

    #[test]
    fn ids_round_trip_and_are_pinned() {
        for command in WorkOutlineCommand::ALL {
            assert_eq!(WorkOutlineCommand::from_id(command.id()), Some(command));
        }
        assert_eq!(WorkOutlineCommand::from_id("nope"), None);
        let ids: Vec<_> = WorkOutlineCommand::ALL.iter().map(|c| c.id()).collect();
        assert_eq!(
            ids,
            [
                "openOutline",
                "createWorkItem",
                "createChildWorkItem",
                "renameWorkItem",
                "moveWorkItem",
                "associateChatWithWorkItem",
                "openEffectiveInstructions",
                "createInstructionFromChat",
                "openRequiredActions",
            ]
        );
    }

    #[test]
    fn selection_requirements_are_disjoint_and_exclude_open_and_create() {
        for command in WorkOutlineCommand::ALL {
            assert!(!(command.needs_selected_work_item() && command.needs_chat()));
        }
        assert!(!WorkOutlineCommand::OpenOutline.needs_selected_work_item());
        assert!(!WorkOutlineCommand::CreateWorkItem.needs_selected_work_item());
        assert!(WorkOutlineCommand::CreateChildWorkItem.needs_selected_work_item());
        assert!(WorkOutlineCommand::AssociateChatWithWorkItem.needs_chat());
    }
}
