//! Right-click menus for Outline rows: work items, Chats, Space roots, and
//! the Unassociated Chats group.
//!
//! This half builds a row's menu from the Outline's own state. The Shell
//! renders it (`shell::work_outline_menu_ui`), owns its keyboard while it is
//! open, and runs the chosen entry. An entry that shows a key dispatches the
//! same action that key (or the palette command) dispatches, so drift
//! confirmation, operation-id reuse, and validation behave exactly as they
//! do from the keyboard. The remaining entries call the helpers their
//! buttons already use (Work Item Details, the Compare dialog, the sidebar
//! Chat menu).
//!
//! Opening: right-click a row (gpui's macOS platform layer delivers
//! Control-click as a right click), or press Shift+F10 / the Menu key while
//! the Outline has focus ([`OpenContextMenu`]).
//!
//! This is a child module of `outline_view` so it can read the Outline's
//! snapshots and scroll state without widening that view's API.
use gpui::{
    Action, App, Context, KeyBinding, MouseDownEvent, Pixels, Point, WeakEntity, Window, actions,
    point, px,
};
use zeron_proto::WorkItemStatus;

use super::super::chips::base_status_label;
use super::super::command::WorkOutlineCommand;
use super::super::details_panel::SPACE_ROOT_ITEM_ID;
use super::super::hints::HintKey;
use super::super::model::{self, ChatRow, ItemRow, OutlineRowKind, row_key};
use super::super::restructure::{self, RestructureOp};
use super::OutlineView;
use crate::icons;
use crate::shell::work_outline_ui::{
    OUTLINE_NAVIGATION_CONTEXT, OutlineNavKey, RunWorkOutlineCommand,
};

actions!(work_outline, [OpenContextMenu]);

/// Keys that open the selected row's menu from the keyboard (Outline focus
/// only): Shift+F10 and the Menu key, the Windows/Linux conventions.
pub(crate) const OPEN_MENU_KEYS: [&str; 2] = ["shift-f10", "menu"];

/// Bindings for [`OPEN_MENU_KEYS`], scoped like every bare Outline key.
pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    OPEN_MENU_KEYS
        .iter()
        .map(|keys| KeyBinding::new(keys, OpenContextMenu, Some(OUTLINE_NAVIGATION_CONTEXT)))
        .collect()
}

/// Which row a menu belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuKind {
    Item,
    Chat,
    SpaceRoot,
    Unassociated,
}

/// What an entry does when chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MenuAction {
    /// Dispatch the Outline key's own action (the key the entry shows).
    Outline(OutlineNavKey),
    /// Dispatch the palette / keymap command.
    Command(WorkOutlineCommand),
    /// Open the Set Status page.
    StatusPage,
    /// Back from the Set Status page.
    Back,
    SetStatus(WorkItemStatus),
    AddInstruction,
    /// Copy this path label to the clipboard.
    CopyPath(String),
    Archive,
    Restore,
    SpaceInstructions,
    ExecutionSetup,
    RemoveFromWorkItem(String),
    CompareInstructions(String),
    ApplyInstructions(String),
    PinChat {
        chat_id: String,
        pinned: bool,
    },
    CopyChatIdentity(String),
    CloseOutChat(String),
    ArchiveChat(String),
}

impl MenuAction {
    /// The gpui action a keyed entry dispatches: the very action its key
    /// fires. `None` for entries that call a helper instead.
    pub(crate) fn dispatch(&self) -> Option<Box<dyn Action>> {
        match self {
            Self::Outline(key) => Some(outline_action(*key)),
            Self::Command(command) => Some(Box::new(RunWorkOutlineCommand(*command))),
            _ => None,
        }
    }
}

/// The action an Outline key fires (mirrors
/// `shell::work_outline_ui::bind_work_outline_navigation`).
pub(crate) fn outline_action(key: OutlineNavKey) -> Box<dyn Action> {
    use super::{
        Collapse, CreateChild, CreateSibling, Expand, IndentItem, MoveItemDown, MoveItemUp,
        OpenSelected, OutdentItem, RenameSelected, SelectFirst, SelectLast, SelectNext,
        SelectNextSibling, SelectPrevious, SelectPreviousSibling,
    };
    match key {
        OutlineNavKey::SelectNext => Box::new(SelectNext),
        OutlineNavKey::SelectPrevious => Box::new(SelectPrevious),
        OutlineNavKey::SelectFirst => Box::new(SelectFirst),
        OutlineNavKey::SelectLast => Box::new(SelectLast),
        OutlineNavKey::SelectNextSibling => Box::new(SelectNextSibling),
        OutlineNavKey::SelectPreviousSibling => Box::new(SelectPreviousSibling),
        OutlineNavKey::Expand => Box::new(Expand),
        OutlineNavKey::Collapse => Box::new(Collapse),
        OutlineNavKey::OpenSelected => Box::new(OpenSelected),
        OutlineNavKey::RenameSelected => Box::new(RenameSelected),
        OutlineNavKey::CreateSibling => Box::new(CreateSibling),
        OutlineNavKey::CreateChild => Box::new(CreateChild),
        OutlineNavKey::MoveItemUp => Box::new(MoveItemUp),
        OutlineNavKey::MoveItemDown => Box::new(MoveItemDown),
        OutlineNavKey::IndentItem => Box::new(IndentItem),
        OutlineNavKey::OutdentItem => Box::new(OutdentItem),
        OutlineNavKey::ShowShortcuts => Box::new(RunWorkOutlineCommand(
            WorkOutlineCommand::ShowOutlineShortcuts,
        )),
    }
}

/// One menu entry. The key it shows is resolved from the live keymap when
/// the menu renders (`hints::hint_keys`), so a rebind shows immediately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MenuEntry {
    /// Stable id: test selector suffix (`outline-menu-{id}`).
    pub id: &'static str,
    pub label: String,
    /// `""`: no glyph (the slot stays, for alignment).
    pub icon: &'static str,
    pub hint: HintKey,
    /// `Some(reason)`: greyed out, the reason in its tooltip.
    pub disabled: Option<String>,
    /// A check mark (the current status on the Set Status page).
    pub checked: bool,
    /// Draw a separator above this entry.
    pub separator_before: bool,
    pub action: MenuAction,
}

impl MenuEntry {
    pub(crate) fn new(
        id: &'static str,
        label: impl Into<String>,
        icon: &'static str,
        action: MenuAction,
    ) -> Self {
        let hint = match &action {
            MenuAction::Outline(key) => HintKey::Outline(*key),
            MenuAction::Command(command) => HintKey::Command(*command),
            _ => HintKey::None,
        };
        Self {
            id,
            label: label.into(),
            icon,
            hint,
            disabled: None,
            checked: false,
            separator_before: false,
            action,
        }
    }

    pub(crate) fn disabled(mut self, reason: Option<impl Into<String>>) -> Self {
        self.disabled = reason.map(Into::into);
        self
    }

    pub(crate) fn separated(mut self) -> Self {
        self.separator_before = true;
        self
    }

    fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    /// Opens a page (shows a chevron instead of a key).
    pub(crate) fn opens_page(&self) -> bool {
        self.action == MenuAction::StatusPage
    }
}

/// A row's menu as the Outline describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutlineContextMenu {
    pub kind: MenuKind,
    /// The row the menu belongs to; re-selected before an entry runs so the
    /// dispatched action acts on it. `None` for the single-Space root entry
    /// (there is no row).
    pub row_key: Option<String>,
    pub space_id: Option<String>,
    /// Target of the details-backed entries (`""` = the Space root).
    pub item_id: Option<String>,
    /// The Chat of a Chat row.
    pub chat_id: Option<String>,
    pub entries: Vec<MenuEntry>,
    /// The Set Status page (work items only), headed by Back.
    pub status_entries: Vec<MenuEntry>,
}

/// The Space root's menu (a multi-Space Space row, or the single-Space
/// "Space root instructions" entry).
fn space_root_menu(
    row_key: Option<String>,
    space_id: &str,
    unavailable: Option<&'static str>,
) -> OutlineContextMenu {
    OutlineContextMenu {
        kind: MenuKind::SpaceRoot,
        row_key,
        space_id: Some(space_id.to_string()),
        item_id: Some(SPACE_ROOT_ITEM_ID.to_string()),
        chat_id: None,
        entries: vec![
            MenuEntry::new(
                "new-item",
                "New Work Item",
                icons::ADD_CIRCLE,
                MenuAction::Command(WorkOutlineCommand::CreateWorkItem),
            )
            .disabled(unavailable),
            MenuEntry::new(
                "space-instructions",
                "Space Instructions",
                icons::DOCUMENT,
                MenuAction::SpaceInstructions,
            )
            .disabled(unavailable),
            MenuEntry::new(
                "execution",
                "Execution Setup",
                icons::FOLDER,
                MenuAction::ExecutionSetup,
            )
            .disabled(unavailable),
            MenuEntry::new(
                "required-actions",
                "Required Actions",
                icons::BELL,
                MenuAction::Command(WorkOutlineCommand::OpenRequiredActions),
            )
            .separated(),
            MenuEntry::new(
                "shortcuts",
                "Show Outline Shortcuts",
                icons::KEYBOARD,
                MenuAction::Outline(OutlineNavKey::ShowShortcuts),
            ),
        ],
        status_entries: Vec::new(),
    }
}

/// Why a restructuring key would do nothing for this item.
fn restructure_blocked(op: RestructureOp) -> &'static str {
    match op {
        RestructureOp::MoveUp => "Already the first item at this level",
        RestructureOp::MoveDown => "Already the last item at this level",
        RestructureOp::Indent => "No item above to indent under",
        RestructureOp::Outdent => "Already at the top level",
    }
}

impl OutlineView {
    /// The menu of row `key`; `None` for rows without one (notices, and the
    /// Unassociated group while no Chat is selected).
    pub(crate) fn context_menu(&self, key: &str, cx: &App) -> Option<OutlineContextMenu> {
        let row = self.model.row(key)?;
        match &row.kind {
            OutlineRowKind::Item(item) => Some(self.item_context_menu(key, item)),
            OutlineRowKind::Chat(chat) => Some(self.chat_context_menu(key, chat)),
            OutlineRowKind::Space { space_id, .. } => Some(space_root_menu(
                Some(key.to_string()),
                space_id,
                self.space_unavailable(space_id),
            )),
            OutlineRowKind::Unassociated { .. } => self.unassociated_context_menu(key, cx),
            OutlineRowKind::Notice { .. } => None,
        }
    }

    /// The selected row's menu and where to show it (keyboard open).
    pub(crate) fn context_menu_for_selection(
        &self,
        cx: &App,
    ) -> Option<(OutlineContextMenu, Option<Point<Pixels>>)> {
        let key = self.selected.as_deref()?;
        let menu = self.context_menu(key, cx)?;
        Some((menu, self.row_anchor(key)))
    }

    fn space_unavailable(&self, space_id: &str) -> Option<&'static str> {
        self.snapshot(space_id)
            .is_none_or(|snapshot| !snapshot.available)
            .then_some("This project's outline is not available yet")
    }

    fn item_context_menu(&self, key: &str, item: &ItemRow) -> OutlineContextMenu {
        let snapshot = self.snapshot(&item.space_id);
        let archived = item.archived;
        let restore_first = |what: &'static str| archived.then_some(what);
        let restructure = |op: RestructureOp| -> Option<&'static str> {
            if archived {
                return Some("Restore this work item before moving it");
            }
            let Some(snapshot) = snapshot else {
                return Some("This project's outline is not loaded");
            };
            let shown = |id: &str| {
                self.model
                    .position(&row_key::item(&item.space_id, id))
                    .is_some()
            };
            restructure::plan(snapshot, &item.item_id, op, shown)
                .is_none()
                .then(|| restructure_blocked(op))
        };
        let path = snapshot
            .map(|snapshot| {
                model::path_label(
                    model::item_path(snapshot, &item.item_id)
                        .iter()
                        .map(|item| item.title.as_str()),
                )
            })
            .unwrap_or_else(|| item.title.clone());
        let entries = vec![
            MenuEntry::new(
                "open-details",
                "Open Details",
                icons::EYE,
                MenuAction::Outline(OutlineNavKey::OpenSelected),
            ),
            MenuEntry::new(
                "start-chat",
                "Start Chat",
                icons::ACTION_PLAY,
                MenuAction::Command(WorkOutlineCommand::StartChatForWorkItem),
            )
            .disabled(restore_first("Restore the work item to start a Chat in it")),
            MenuEntry::new(
                "rename",
                "Rename",
                icons::PEN,
                MenuAction::Outline(OutlineNavKey::RenameSelected),
            )
            .separated(),
            MenuEntry::new(
                "new-sibling",
                "New Sibling",
                icons::ADD_CIRCLE,
                MenuAction::Outline(OutlineNavKey::CreateSibling),
            ),
            MenuEntry::new(
                "new-child",
                "New Child",
                icons::ADD_CIRCLE,
                MenuAction::Outline(OutlineNavKey::CreateChild),
            )
            .disabled(restore_first("Restore this work item to add children")),
            MenuEntry::new(
                "move-up",
                "Move Up",
                icons::ARROW_UP,
                MenuAction::Outline(OutlineNavKey::MoveItemUp),
            )
            .disabled(restructure(RestructureOp::MoveUp))
            .separated(),
            MenuEntry::new(
                "move-down",
                "Move Down",
                icons::ARROW_DOWN,
                MenuAction::Outline(OutlineNavKey::MoveItemDown),
            )
            .disabled(restructure(RestructureOp::MoveDown)),
            MenuEntry::new(
                "indent",
                "Indent",
                icons::ARROW_RIGHT,
                MenuAction::Outline(OutlineNavKey::IndentItem),
            )
            .disabled(restructure(RestructureOp::Indent)),
            MenuEntry::new(
                "outdent",
                "Outdent",
                icons::ARROW_LEFT,
                MenuAction::Outline(OutlineNavKey::OutdentItem),
            )
            .disabled(restructure(RestructureOp::Outdent)),
            MenuEntry::new(
                "move-to",
                "Move To\u{2026}",
                icons::SORT_VERTICAL,
                MenuAction::Command(WorkOutlineCommand::MoveWorkItem),
            )
            .disabled(restore_first("Restore this work item before moving it")),
            MenuEntry::new(
                "set-status",
                "Set Status",
                icons::CHECKLIST,
                MenuAction::StatusPage,
            )
            .disabled(restore_first("Restore this work item to change its status"))
            .separated(),
            MenuEntry::new(
                "associate",
                "Associate Chat\u{2026}",
                icons::CHAT_ROUND_LINE,
                MenuAction::Command(WorkOutlineCommand::AssociateChatWithWorkItem),
            )
            .disabled(restore_first("Restore this work item to associate Chats")),
            MenuEntry::new(
                "effective-instructions",
                "Open Effective Instructions",
                icons::DOCUMENT,
                MenuAction::Command(WorkOutlineCommand::OpenEffectiveInstructions),
            )
            .separated(),
            MenuEntry::new(
                "add-instruction",
                "Add Instruction",
                icons::DOCUMENT_ADD,
                MenuAction::AddInstruction,
            )
            .disabled(restore_first("Restore this work item to add instructions")),
            MenuEntry::new(
                "copy-path",
                "Copy Path",
                icons::COPY,
                MenuAction::CopyPath(path),
            )
            .separated(),
            MenuEntry::new(
                "archive",
                "Archive\u{2026}",
                icons::ARCHIVE_MINIMALISTIC,
                MenuAction::Archive,
            )
            .disabled(archived.then_some("Already archived")),
            MenuEntry::new(
                "restore",
                "Restore",
                icons::ARCHIVE_UP_MINIMALISTIC,
                MenuAction::Restore,
            )
            .disabled((!archived).then_some("This work item is not archived")),
        ];
        let status = |id: &'static str, status: WorkItemStatus| {
            // No glyph: the current status carries the check mark.
            MenuEntry::new(
                id,
                base_status_label(status),
                "",
                MenuAction::SetStatus(status),
            )
            .checked(item.status == status)
        };
        let status_entries = vec![
            MenuEntry::new(
                "status-back",
                "Set Status",
                icons::ALT_ARROW_LEFT,
                MenuAction::Back,
            ),
            status("status-pending", WorkItemStatus::Pending).separated(),
            status("status-active", WorkItemStatus::Active),
            status("status-completed", WorkItemStatus::Completed),
        ];
        OutlineContextMenu {
            kind: MenuKind::Item,
            row_key: Some(key.to_string()),
            space_id: Some(item.space_id.clone()),
            item_id: Some(item.item_id.clone()),
            chat_id: None,
            entries,
            status_entries,
        }
    }

    fn chat_context_menu(&self, key: &str, row: &ChatRow) -> OutlineContextMenu {
        let chat_id = row.chat.id.clone();
        let gone = row
            .dangling
            .then_some("This chat no longer exists on any device");
        let associated = row.work_item.is_some();
        let drift = !row.dangling
            && row
                .work_item
                .as_ref()
                .and_then(|(space, _)| self.snapshot(space))
                .and_then(|snapshot| snapshot.chat_instruction_state(&chat_id))
                .is_some_and(|state| state.drift);
        let mut entries = vec![
            MenuEntry::new(
                "open-chat",
                "Open Chat",
                icons::CHAT_ROUND_LINE,
                MenuAction::Outline(OutlineNavKey::OpenSelected),
            )
            .disabled(gone),
            MenuEntry::new(
                "rename",
                "Rename\u{2026}",
                icons::PEN,
                MenuAction::Outline(OutlineNavKey::RenameSelected),
            )
            .disabled(gone),
            MenuEntry::new(
                "associate",
                if associated {
                    "Move to Work Item\u{2026}"
                } else {
                    "Associate with Work Item\u{2026}"
                },
                icons::SORT_VERTICAL,
                MenuAction::Command(WorkOutlineCommand::AssociateChatWithWorkItem),
            )
            .disabled(gone.or_else(|| {
                row.chat
                    .space_id
                    .is_none()
                    .then_some("Chats without a project cannot be associated")
            }))
            .separated(),
        ];
        entries.extend(self.chat_move_entries(row, gone));
        if associated {
            entries.push(MenuEntry::new(
                "remove-from-item",
                "Remove from Work Item",
                icons::CLOSE_CIRCLE,
                MenuAction::RemoveFromWorkItem(chat_id.clone()),
            ));
        }
        if drift {
            entries.push(
                MenuEntry::new(
                    "compare-instructions",
                    "Compare Instructions\u{2026}",
                    icons::DOCUMENT,
                    MenuAction::CompareInstructions(chat_id.clone()),
                )
                .separated(),
            );
            entries.push(MenuEntry::new(
                "apply-instructions",
                "Apply Current Instructions",
                icons::REFRESH,
                MenuAction::ApplyInstructions(chat_id.clone()),
            ));
        }
        let (space_id, item_id) = row.work_item.clone().unzip();
        OutlineContextMenu {
            kind: MenuKind::Chat,
            row_key: Some(key.to_string()),
            space_id: space_id.or_else(|| row.chat.space_id.clone()),
            item_id,
            chat_id: Some(chat_id),
            entries,
            status_entries: Vec::new(),
        }
    }

    /// Move to Previous / Next / Parent Item: the Chat-row form of the
    /// restructuring keys (`restructure::chat_plan`), greyed out with the
    /// reason when the key would do nothing.
    fn chat_move_entries(&self, row: &ChatRow, gone: Option<&'static str>) -> [MenuEntry; 3] {
        let chat_id = row.chat.id.as_str();
        let space_id = row.chat.space_id.as_deref();
        let snapshot = space_id.and_then(|space| self.snapshot(space));
        let blocked_by = gone.or_else(|| {
            space_id
                .is_none()
                .then_some("Chats without a project cannot be associated")
                .or_else(|| {
                    snapshot
                        .is_none()
                        .then_some("This project's outline is not loaded")
                })
        });
        let blocked = |op: RestructureOp| -> Option<&'static str> {
            if blocked_by.is_some() {
                return blocked_by;
            }
            let (Some(space_id), Some(snapshot)) = (space_id, snapshot) else {
                return blocked_by;
            };
            let shown = |id: &str| self.model.position(&row_key::item(space_id, id)).is_some();
            restructure::chat_plan(snapshot, chat_id, op, shown)
                .is_none()
                .then_some(match op {
                    RestructureOp::MoveUp => "No work item above",
                    RestructureOp::MoveDown => "No work item below",
                    RestructureOp::Outdent | RestructureOp::Indent => "Not in a work item",
                })
        };
        let top_level = snapshot.is_some_and(|snapshot| {
            restructure::chat_item(snapshot, chat_id)
                .and_then(|item| snapshot.item(item))
                .is_some_and(|item| item.parent_work_item_id.is_none())
        });
        [
            MenuEntry::new(
                "move-to-previous-item",
                "Move to Previous Item",
                icons::ARROW_UP,
                MenuAction::Outline(OutlineNavKey::MoveItemUp),
            )
            .disabled(blocked(RestructureOp::MoveUp)),
            MenuEntry::new(
                "move-to-next-item",
                "Move to Next Item",
                icons::ARROW_DOWN,
                MenuAction::Outline(OutlineNavKey::MoveItemDown),
            )
            .disabled(blocked(RestructureOp::MoveDown)),
            MenuEntry::new(
                "move-to-parent-item",
                if top_level {
                    "Move to Unassociated Chats"
                } else {
                    "Move to Parent Item"
                },
                icons::ARROW_LEFT,
                MenuAction::Outline(OutlineNavKey::OutdentItem),
            )
            .disabled(blocked(RestructureOp::Outdent)),
        ]
    }

    /// "Associate Chat…" for the app's selected Chat, the target the
    /// command picks when the group header is selected.
    fn unassociated_context_menu(&self, key: &str, cx: &App) -> Option<OutlineContextMenu> {
        let chat_id = self.state.read(cx).selected_chat.clone()?;
        let chat = self.inputs.chats.iter().find(|chat| chat.id == chat_id)?;
        let entry = MenuEntry::new(
            "associate",
            format!("Associate \u{201c}{}\u{201d}\u{2026}", chat.title),
            icons::CHAT_ROUND_LINE,
            MenuAction::Command(WorkOutlineCommand::AssociateChatWithWorkItem),
        )
        .disabled(
            chat.space_id
                .is_none()
                .then_some("Chats without a project cannot be associated"),
        );
        Some(OutlineContextMenu {
            kind: MenuKind::Unassociated,
            row_key: Some(key.to_string()),
            space_id: chat.space_id.clone(),
            item_id: None,
            chat_id: Some(chat_id),
            entries: vec![entry],
            status_entries: Vec::new(),
        })
    }

    /// Window position just below row `key`'s label, for a menu opened from
    /// the keyboard. `None` before the list has been laid out.
    fn row_anchor(&self, key: &str) -> Option<Point<Pixels>> {
        let ix = self.model.position(key)?;
        let state = self.scroll.0.borrow();
        let bounds = state.base_handle.bounds();
        let offset = state.base_handle.offset();
        let rows = self.model.len().max(1);
        let height = state
            .last_item_size
            .map(|size| size.contents.height / rows as f32)
            .filter(|height| *height > px(0.0))?;
        let depth = self.model.rows[ix].depth;
        let below = bounds.top() + offset.y + height * (ix + 1) as f32;
        let y = if below < bounds.top() {
            bounds.top()
        } else if below > bounds.bottom() {
            bounds.bottom()
        } else {
            below
        };
        let x = bounds.left() + px(Self::indent(depth) + 24.0);
        Some(point(x, y))
    }

    /// Select row `key` and ask the Shell to show its menu at `position`.
    fn open_row_context_menu(
        &mut self,
        key: &str,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_editing() {
            return;
        }
        cx.stop_propagation();
        window.focus(&self.focus, cx);
        self.select(key, cx);
        if let Some(menu) = self.context_menu(key, cx) {
            self.show_context_menu(menu, position, cx);
        }
    }

    fn show_context_menu(
        &mut self,
        menu: OutlineContextMenu,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(shell) = self.shell.as_ref().and_then(WeakEntity::upgrade) else {
            return;
        };
        let outline = cx.entity().downgrade();
        let focus = self.focus.clone();
        shell.update(cx, |shell, cx| {
            shell.open_work_outline_menu(menu, position, outline, focus, false, cx)
        });
    }
}

/// Right-click handler for row `key` (the Outline's only hook).
pub(crate) fn row_listener(
    key: String,
    cx: &mut Context<OutlineView>,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
        this.open_row_context_menu(&key, event.position, window, cx)
    })
}

/// Right-click handler for the single-Space "Space root instructions"
/// entry (there is no Space row to select).
pub(crate) fn space_root_listener(
    space_id: String,
    cx: &mut Context<OutlineView>,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
        if this.is_editing() {
            return;
        }
        cx.stop_propagation();
        window.focus(&this.focus, cx);
        let menu = space_root_menu(None, &space_id, this.space_unavailable(&space_id));
        this.show_context_menu(menu, event.position, cx);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyed_entries_dispatch_the_action_their_key_fires() {
        let entry = MenuEntry::new(
            "move-up",
            "Move Up",
            icons::ARROW_UP,
            MenuAction::Outline(OutlineNavKey::MoveItemUp),
        );
        assert_eq!(entry.hint, HintKey::Outline(OutlineNavKey::MoveItemUp));
        let action = entry.action.dispatch().unwrap();
        assert!(action.partial_eq(&super::super::MoveItemUp));

        let entry = MenuEntry::new(
            "start-chat",
            "Start Chat",
            icons::ACTION_PLAY,
            MenuAction::Command(WorkOutlineCommand::StartChatForWorkItem),
        );
        assert_eq!(
            entry.hint,
            HintKey::Command(WorkOutlineCommand::StartChatForWorkItem)
        );
        assert!(
            entry
                .action
                .dispatch()
                .unwrap()
                .partial_eq(&RunWorkOutlineCommand(
                    WorkOutlineCommand::StartChatForWorkItem
                ))
        );
        // `?` fires the shortcut command; so does its menu entry.
        assert!(
            outline_action(OutlineNavKey::ShowShortcuts).partial_eq(&RunWorkOutlineCommand(
                WorkOutlineCommand::ShowOutlineShortcuts
            ))
        );
        let entry = MenuEntry::new("copy-path", "Copy Path", icons::COPY, MenuAction::Archive);
        assert_eq!(entry.hint, HintKey::None);
        assert!(entry.action.dispatch().is_none());
    }

    #[test]
    fn open_menu_keys_are_scoped_to_the_outline() {
        let bindings = key_bindings();
        assert_eq!(bindings.len(), OPEN_MENU_KEYS.len());
        for binding in bindings {
            let predicate = binding.predicate().expect("scoped").to_string();
            assert!(predicate.contains(super::super::KEY_CONTEXT), "{predicate}");
            assert!(predicate.contains("!PaletteSearch"), "{predicate}");
        }
    }

    #[test]
    fn space_root_menu_lists_its_entries_and_greys_out_when_unavailable() {
        let menu = space_root_menu(None, "s1", None);
        let ids: Vec<_> = menu.entries.iter().map(|entry| entry.id).collect();
        assert_eq!(
            ids,
            [
                "new-item",
                "space-instructions",
                "execution",
                "required-actions",
                "shortcuts"
            ]
        );
        assert_eq!(menu.item_id.as_deref(), Some(SPACE_ROOT_ITEM_ID));
        assert!(menu.entries.iter().all(|entry| entry.disabled.is_none()));
        let menu = space_root_menu(None, "s1", Some("not yet"));
        assert_eq!(menu.entries[0].disabled.as_deref(), Some("not yet"));
        // Global commands stay available.
        assert!(menu.entries[3].disabled.is_none());
    }
}
