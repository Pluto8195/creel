//! Seam 1: the seventeen Work Outline commands and the confirmation request type.
//!
//! Every entry point (command palette, keymap actions, Outline toolbar) forwards
//! to `Shell::run_work_outline_command`, which routes to
//! `Overview::run_work_outline_command`. This file only names the commands; it
//! owns no behavior.
use gpui::{SharedString, Window};
use zeron_proto::DescendantArchivePolicy;

use crate::settings::ShortcutId;

/// The Work Outline commands, in palette order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WorkOutlineCommand {
    OpenOutline,
    /// Jump between a Chat and its Outline row: from the Chat, select its
    /// row and focus the tree; from the tree, back to the last Chat opened
    /// from the Outline with its composer focused.
    ToggleOutlineFocus,
    /// Leave the Overview for the full chat route of the Chat in play: the
    /// chat panel's Chat, or the selected Chat row (the panel's "Open full
    /// chat →").
    OpenFullChat,
    CreateWorkItem,
    CreateChildWorkItem,
    RenameWorkItem,
    MoveWorkItem,
    AssociateChatWithWorkItem,
    OpenEffectiveInstructions,
    CreateInstructionFromChat,
    OpenRequiredActions,
    /// Milestone 3: start a Chat in the selected work item.
    StartChatForWorkItem,
    /// Restructuring: before the previous sibling.
    MoveWorkItemUp,
    /// Restructuring: after the next sibling.
    MoveWorkItemDown,
    /// Restructuring: last child of the previous sibling.
    IndentWorkItem,
    /// Restructuring: after the former parent, one level up.
    OutdentWorkItem,
    /// Discoverability: the on-demand Outline shortcut reference.
    ShowOutlineShortcuts,
    /// Show/hide the Overview's left "Inbox" column (the Linear panel and
    /// the GitHub section) — open and focus it, or close it when focused.
    /// Listed in the palette only while some Space has Linear on or the PR
    /// review sync. The id stays `openLinear` (persisted keymap field).
    OpenLinear,
}

impl WorkOutlineCommand {
    pub(crate) const ALL: [WorkOutlineCommand; 18] = [
        Self::OpenOutline,
        Self::ToggleOutlineFocus,
        Self::OpenFullChat,
        Self::CreateWorkItem,
        Self::CreateChildWorkItem,
        Self::RenameWorkItem,
        Self::MoveWorkItem,
        Self::AssociateChatWithWorkItem,
        Self::OpenEffectiveInstructions,
        Self::CreateInstructionFromChat,
        Self::OpenRequiredActions,
        Self::StartChatForWorkItem,
        Self::MoveWorkItemUp,
        Self::MoveWorkItemDown,
        Self::IndentWorkItem,
        Self::OutdentWorkItem,
        Self::ShowOutlineShortcuts,
        Self::OpenLinear,
    ];

    /// Palette / menu label (sentence-style, matches the existing palette).
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::OpenOutline => "Open Outline",
            Self::ToggleOutlineFocus => "Toggle Outline Focus",
            Self::OpenFullChat => "Open Full Chat",
            Self::CreateWorkItem => "Create Work Item",
            Self::CreateChildWorkItem => "Create Child Work Item",
            Self::RenameWorkItem => "Rename Work Item",
            Self::MoveWorkItem => "Move Work Item",
            Self::AssociateChatWithWorkItem => "Associate Chat with Work Item",
            Self::OpenEffectiveInstructions => "Open Effective Instructions",
            Self::CreateInstructionFromChat => "Create Instruction from Chat",
            Self::OpenRequiredActions => "Open Required Actions",
            Self::StartChatForWorkItem => "Start Chat in Work Item",
            Self::MoveWorkItemUp => "Move Up",
            Self::MoveWorkItemDown => "Move Down",
            Self::IndentWorkItem => "Indent",
            Self::OutdentWorkItem => "Outdent",
            Self::ShowOutlineShortcuts => "Show Outline Shortcuts",
            Self::OpenLinear => "Toggle Inbox Column",
        }
    }

    /// Palette glyph. Names come from `crate::icons`.
    pub(crate) fn icon(self) -> &'static str {
        use crate::icons;
        match self {
            Self::OpenOutline => icons::LIST,
            Self::ToggleOutlineFocus => icons::SPLIT_COLUMNS,
            Self::OpenFullChat => icons::WINDOW_MAXIMIZE,
            Self::CreateWorkItem | Self::CreateChildWorkItem => icons::ADD_CIRCLE,
            Self::RenameWorkItem => icons::PEN,
            Self::MoveWorkItem => icons::SORT_VERTICAL,
            Self::AssociateChatWithWorkItem => icons::CHAT_ROUND_LINE,
            Self::OpenEffectiveInstructions | Self::CreateInstructionFromChat => icons::DOCUMENT,
            Self::OpenRequiredActions => icons::BELL,
            Self::StartChatForWorkItem => icons::ACTION_PLAY,
            Self::MoveWorkItemUp => icons::ARROW_UP,
            Self::MoveWorkItemDown => icons::ARROW_DOWN,
            Self::IndentWorkItem => icons::ARROW_RIGHT,
            Self::OutdentWorkItem => icons::ARROW_LEFT,
            Self::ShowOutlineShortcuts => icons::KEYBOARD,
            Self::OpenLinear => icons::CHECKLIST,
        }
    }

    /// Stable id: camelCase, never renamed. It is the persisted/keymap field
    /// name (`KeymapConfig`) and the test selector suffix, so it must not
    /// change once shipped.
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::OpenOutline => "openOutline",
            Self::ToggleOutlineFocus => "toggleOutlineFocus",
            Self::OpenFullChat => "openFullChat",
            Self::CreateWorkItem => "createWorkItem",
            Self::CreateChildWorkItem => "createChildWorkItem",
            Self::RenameWorkItem => "renameWorkItem",
            Self::MoveWorkItem => "moveWorkItem",
            Self::AssociateChatWithWorkItem => "associateChatWithWorkItem",
            Self::OpenEffectiveInstructions => "openEffectiveInstructions",
            Self::CreateInstructionFromChat => "createInstructionFromChat",
            Self::OpenRequiredActions => "openRequiredActions",
            Self::StartChatForWorkItem => "startChatForWorkItem",
            Self::MoveWorkItemUp => "moveWorkItemUp",
            Self::MoveWorkItemDown => "moveWorkItemDown",
            Self::IndentWorkItem => "indentWorkItem",
            Self::OutdentWorkItem => "outdentWorkItem",
            Self::ShowOutlineShortcuts => "showOutlineShortcuts",
            Self::OpenLinear => "openLinear",
        }
    }

    /// The rebindable shortcut registered for this command. Every command has
    /// one, including those whose palette entry waits for a later milestone.
    pub(crate) fn shortcut_id(self) -> ShortcutId {
        match self {
            Self::OpenOutline => ShortcutId::OpenOutline,
            Self::ToggleOutlineFocus => ShortcutId::ToggleOutlineFocus,
            Self::OpenFullChat => ShortcutId::OpenFullChat,
            Self::CreateWorkItem => ShortcutId::CreateWorkItem,
            Self::CreateChildWorkItem => ShortcutId::CreateChildWorkItem,
            Self::RenameWorkItem => ShortcutId::RenameWorkItem,
            Self::MoveWorkItem => ShortcutId::MoveWorkItem,
            Self::AssociateChatWithWorkItem => ShortcutId::AssociateChatWithWorkItem,
            Self::OpenEffectiveInstructions => ShortcutId::OpenEffectiveInstructions,
            Self::CreateInstructionFromChat => ShortcutId::CreateInstructionFromChat,
            Self::OpenRequiredActions => ShortcutId::OpenRequiredActions,
            Self::StartChatForWorkItem => ShortcutId::StartChatForWorkItem,
            Self::MoveWorkItemUp => ShortcutId::MoveWorkItemUp,
            Self::MoveWorkItemDown => ShortcutId::MoveWorkItemDown,
            Self::IndentWorkItem => ShortcutId::IndentWorkItem,
            Self::OutdentWorkItem => ShortcutId::OutdentWorkItem,
            Self::ShowOutlineShortcuts => ShortcutId::ShowOutlineShortcuts,
            Self::OpenLinear => ShortcutId::OpenLinear,
        }
    }

    /// The command a shortcut id fires, if it is a Work Outline shortcut.
    pub(crate) fn from_shortcut_id(id: ShortcutId) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|command| command.shortcut_id() == id)
    }

    /// Whether the command appears in the command palette unconditionally:
    /// every command since Milestone 4 enabled Open Required Actions, except
    /// Toggle Linear Panel, which the palette adds only while some Space has Linear
    /// on (`linear::panel::palette_visible`).
    pub(crate) fn in_palette(self) -> bool {
        self != Self::OpenLinear
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
                | Self::StartChatForWorkItem
                | Self::MoveWorkItemUp
                | Self::MoveWorkItemDown
                | Self::IndentWorkItem
                | Self::OutdentWorkItem
        )
    }

    /// The restructuring a command performs, if it is one.
    pub(crate) fn restructure_op(self) -> Option<super::restructure::RestructureOp> {
        use super::restructure::RestructureOp;
        match self {
            Self::MoveWorkItemUp => Some(RestructureOp::MoveUp),
            Self::MoveWorkItemDown => Some(RestructureOp::MoveDown),
            Self::IndentWorkItem => Some(RestructureOp::Indent),
            Self::OutdentWorkItem => Some(RestructureOp::Outdent),
            _ => None,
        }
    }

    /// Commands that act on the active or selected Chat.
    pub(crate) fn needs_chat(self) -> bool {
        matches!(
            self,
            Self::AssociateChatWithWorkItem | Self::CreateInstructionFromChat
        )
    }
}

/// A candidate new parent in [`WorkOutlineArchiveRequest`]. `work_item_id:
/// None` is the Space root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkOutlineParentChoice {
    pub work_item_id: Option<String>,
    /// Path label shown in the picker (for example `Launch v2 / API`).
    pub label: SharedString,
}

/// The archive confirmation for an item that has descendants: the user must
/// choose explicitly between archiving the whole subtree and moving the
/// descendants to a parent first (never an implicit default). The Overview
/// supplies the valid destinations (live items outside the archived subtree,
/// plus the Space root); the first entry is preselected.
///
/// `on_decision` receives the chosen policy, or `None` when the dialog was
/// cancelled (Cancel, Escape). An item without descendants needs no choice:
/// the dialog degrades to a plain confirmation and reports
/// `DescendantArchivePolicy::ArchiveDescendants`.
pub(crate) struct WorkOutlineArchiveRequest {
    pub item_title: SharedString,
    pub descendant_count: usize,
    pub parents: Vec<WorkOutlineParentChoice>,
    pub on_decision:
        Box<dyn FnOnce(Option<DescendantArchivePolicy>, &mut Window, &mut gpui::App) + 'static>,
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

/// The drift confirmation (Milestone 2): shown before a change that alters
/// the current effective instructions of at least one assigned Chat
/// (toggle inheritance, edit or archive an inherited instruction, associate
/// a Chat from Work Item Details). Outline moves and associations apply at
/// once with an Undo toast instead (`outline_view::move_toast`). `impact` is computed client-side
/// (`instructions::drift_impact`). `on_decision` gets `true` = confirmed.
pub(crate) struct WorkOutlineDriftRequest {
    pub title: SharedString,
    pub confirm_label: SharedString,
    pub impact: super::instructions::DriftImpact,
    pub on_decision: Box<dyn FnOnce(bool, &mut Window, &mut gpui::App) + 'static>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn eighteen_commands_with_unique_labels_and_ids() {
        assert_eq!(WorkOutlineCommand::ALL.len(), 18);
        let ids: HashSet<_> = WorkOutlineCommand::ALL.iter().map(|c| c.id()).collect();
        let labels: HashSet<_> = WorkOutlineCommand::ALL.iter().map(|c| c.label()).collect();
        assert_eq!(ids.len(), 18);
        assert_eq!(labels.len(), 18);
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
                "toggleOutlineFocus",
                "openFullChat",
                "createWorkItem",
                "createChildWorkItem",
                "renameWorkItem",
                "moveWorkItem",
                "associateChatWithWorkItem",
                "openEffectiveInstructions",
                "createInstructionFromChat",
                "openRequiredActions",
                "startChatForWorkItem",
                "moveWorkItemUp",
                "moveWorkItemDown",
                "indentWorkItem",
                "outdentWorkItem",
                "showOutlineShortcuts",
                "openLinear",
            ]
        );
    }

    #[test]
    fn every_command_has_a_distinct_shortcut_that_maps_back() {
        let mut seen = HashSet::new();
        for command in WorkOutlineCommand::ALL {
            let id = command.shortcut_id();
            assert!(seen.insert(id), "{command:?} shares a shortcut id");
            assert_eq!(WorkOutlineCommand::from_shortcut_id(id), Some(command));
        }
        assert_eq!(
            WorkOutlineCommand::from_shortcut_id(ShortcutId::NewSession),
            None
        );
    }

    #[test]
    fn every_command_but_open_linear_is_always_in_the_palette() {
        let listed: Vec<_> = WorkOutlineCommand::ALL
            .into_iter()
            .filter(|c| c.in_palette())
            .collect();
        assert_eq!(listed, WorkOutlineCommand::ALL[..17]);
        assert!(!WorkOutlineCommand::OpenLinear.in_palette());
        assert!(WorkOutlineCommand::OpenRequiredActions.in_palette());
        // Global: no selection needed.
        assert!(!WorkOutlineCommand::OpenRequiredActions.needs_selected_work_item());
    }

    #[test]
    fn selection_requirements_are_disjoint_and_exclude_open_and_create() {
        for command in WorkOutlineCommand::ALL {
            assert!(!(command.needs_selected_work_item() && command.needs_chat()));
        }
        assert!(!WorkOutlineCommand::OpenOutline.needs_selected_work_item());
        assert!(!WorkOutlineCommand::ToggleOutlineFocus.needs_selected_work_item());
        assert!(!WorkOutlineCommand::ToggleOutlineFocus.needs_chat());
        // Open Full Chat resolves its own Chat (panel or selected row).
        assert!(!WorkOutlineCommand::OpenFullChat.needs_selected_work_item());
        assert!(!WorkOutlineCommand::OpenFullChat.needs_chat());
        assert!(!WorkOutlineCommand::CreateWorkItem.needs_selected_work_item());
        assert!(WorkOutlineCommand::CreateChildWorkItem.needs_selected_work_item());
        assert!(WorkOutlineCommand::AssociateChatWithWorkItem.needs_chat());
    }
}
