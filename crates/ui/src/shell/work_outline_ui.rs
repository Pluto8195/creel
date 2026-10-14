//! Shell-owned Work Outline surfaces: the confirmation modals (discard an
//! unsaved draft, archive with an explicit descendant choice), the external
//! reference opener, the command router, and the keymap bindings.
//!
//! The Overview never owns a window modal; it calls through its
//! `WeakEntity<Shell>`. Decisions are delivered through the request's
//! `on_decision` closure, deferred to after the current Shell update so a
//! callback can update any entity without re-entering the Shell.
//!
//! Escape: `Shell::on_key_down_capture` cancels the modal (reporting a
//! cancelled decision); `overlay_owns_keyboard` and `capture_escape_surface`
//! also know about it, like the other shell dialogs.
use super::*;

use crate::markdown::links::LinkTarget;
use crate::overview::OutlineChatReturn;
use crate::work_outline::instructions::DriftImpact;
use crate::work_outline::{
    WorkOutlineArchiveRequest, WorkOutlineCommand, WorkOutlineConfirmRequest,
    WorkOutlineDriftRequest, WorkOutlineParentChoice,
};
use zeron_proto::DescendantArchivePolicy;

/// Fire one Work Outline command from a keymap binding. One action carrying
/// the command, like `JumpSession`, rather than nine action types.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = shell, no_json)]
pub struct RunWorkOutlineCommand(pub WorkOutlineCommand);

type ConfirmCallback = Box<dyn FnOnce(bool, &mut Window, &mut App) + 'static>;
type ArchiveCallback =
    Box<dyn FnOnce(Option<DescendantArchivePolicy>, &mut Window, &mut App) + 'static>;

pub(super) struct ConfirmDialog {
    title: SharedString,
    body: SharedString,
    confirm_label: SharedString,
    cancel_label: SharedString,
    destructive: bool,
    on_decision: Option<ConfirmCallback>,
}

/// The explicit archive choice. There is no default: the confirm button stays
/// disabled until the user picks one (unless there are no descendants).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ArchiveChoice {
    ArchiveDescendants,
    MoveDescendants,
}

pub(super) struct ArchiveDialog {
    item_title: SharedString,
    descendant_count: usize,
    parents: Vec<WorkOutlineParentChoice>,
    choice: Option<ArchiveChoice>,
    parent_ix: usize,
    on_decision: Option<ArchiveCallback>,
}

impl ArchiveDialog {
    fn new(request: WorkOutlineArchiveRequest) -> Self {
        Self {
            choice: (request.descendant_count == 0).then_some(ArchiveChoice::ArchiveDescendants),
            item_title: request.item_title,
            descendant_count: request.descendant_count,
            parents: request.parents,
            parent_ix: 0,
            on_decision: Some(request.on_decision),
        }
    }

    /// The policy the current selection means; `None` while no valid choice
    /// has been made (confirm disabled).
    pub(super) fn policy(&self) -> Option<DescendantArchivePolicy> {
        match self.choice? {
            ArchiveChoice::ArchiveDescendants => Some(DescendantArchivePolicy::ArchiveDescendants),
            ArchiveChoice::MoveDescendants => {
                let parent = self.parents.get(self.parent_ix)?;
                Some(DescendantArchivePolicy::MoveDescendants {
                    destination_parent_work_item_id: parent.work_item_id.clone(),
                })
            }
        }
    }
}

/// The drift confirmation: counts of affected Chats and items, and the
/// statement that existing Chats keep their applied revision.
pub(super) struct DriftDialog {
    title: SharedString,
    confirm_label: SharedString,
    impact: DriftImpact,
    on_decision: Option<ConfirmCallback>,
}

pub(super) enum WorkOutlineModal {
    Confirm(ConfirmDialog),
    Archive(ArchiveDialog),
    Drift(DriftDialog),
    /// Revision comparison with Apply (`instructions_ui`).
    Compare(super::instructions_ui::CompareDialog),
    /// Create Instruction from Chat (`instructions_ui`).
    FromChat(super::instructions_ui::FromChatDialog),
    /// Start Chat from a work item (`start_chat_ui`).
    StartChat(super::start_chat_ui::StartChatDialog),
    /// The on-demand Outline shortcut reference (read-only; Esc closes).
    Shortcuts,
}

/// How a work item reference link should open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ReferenceOpen {
    /// The internal browser surface of the selected chat.
    Internal(String),
    External(String),
    Rejected,
}

/// Pure routing for [`Shell::open_work_item_reference`]. Only http(s) URLs
/// pass (`LinkTarget` validation). The internal browser exists only on the
/// Chat route with a chat selected and the web-link preference on; from
/// Overview-hosted details there is no source chat, so the external browser
/// is used (m0-contract section 7).
pub(super) fn reference_open(
    url: &str,
    on_chat_route_with_chat: bool,
    open_web_links_in_zeron: bool,
    embedded_platform: bool,
) -> ReferenceOpen {
    match LinkTarget::new(url, url).navigation {
        Err(_) => ReferenceOpen::Rejected,
        Ok(navigation) => {
            if on_chat_route_with_chat && open_web_links_in_zeron && embedded_platform {
                ReferenceOpen::Internal(navigation)
            } else {
                ReferenceOpen::External(navigation)
            }
        }
    }
}

/// Key contexts that hold a text input. Outline navigation keys must never
/// fire while one of these owns focus.
pub(crate) const TEXT_INPUT_CONTEXTS: [&str; 3] = ["Composer", "MessageComposer", "PaletteSearch"];

/// The context predicate every bare-key outline binding uses.
pub(crate) const OUTLINE_NAVIGATION_CONTEXT: &str =
    "WorkOutline && !Composer && !MessageComposer && !PaletteSearch";

/// Outline navigation, one entry per action defined in
/// `work_outline::outline_view`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutlineNavKey {
    SelectNext,
    SelectPrevious,
    SelectFirst,
    SelectLast,
    SelectNextSibling,
    SelectPreviousSibling,
    Expand,
    Collapse,
    OpenSelected,
    /// Start a Chat in the selected work item (or a Chat row's work item).
    StartChat,
    RenameSelected,
    CreateSibling,
    CreateChild,
    MoveItemUp,
    MoveItemDown,
    IndentItem,
    OutdentItem,
    /// Open the shortcut reference.
    ShowShortcuts,
}

/// The shortcut reference's groups for the Outline keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutlineKeyGroup {
    Navigate,
    Edit,
    Restructure,
}

impl OutlineNavKey {
    /// Short label (shortcut reference, tooltips).
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::SelectNext => "Next item",
            Self::SelectPrevious => "Previous item",
            Self::SelectFirst => "First item",
            Self::SelectLast => "Last item",
            Self::SelectNextSibling => "Next sibling",
            Self::SelectPreviousSibling => "Previous sibling",
            Self::Expand => "Expand",
            Self::Collapse => "Collapse",
            Self::OpenSelected => "Open details",
            Self::StartChat => "Start Chat",
            Self::RenameSelected => "Rename",
            Self::CreateSibling => "New sibling",
            Self::CreateChild => "New child",
            Self::MoveItemUp => "Move up",
            Self::MoveItemDown => "Move down",
            Self::IndentItem => "Indent",
            Self::OutdentItem => "Outdent",
            Self::ShowShortcuts => "Show shortcuts",
        }
    }

    pub(crate) fn group(self) -> OutlineKeyGroup {
        match self {
            Self::SelectNext
            | Self::SelectPrevious
            | Self::SelectFirst
            | Self::SelectLast
            | Self::SelectNextSibling
            | Self::SelectPreviousSibling
            | Self::Expand
            | Self::Collapse
            | Self::OpenSelected
            | Self::ShowShortcuts => OutlineKeyGroup::Navigate,
            Self::StartChat | Self::RenameSelected | Self::CreateSibling | Self::CreateChild => {
                OutlineKeyGroup::Edit
            }
            Self::MoveItemUp | Self::MoveItemDown | Self::IndentItem | Self::OutdentItem => {
                OutlineKeyGroup::Restructure
            }
        }
    }
}

/// Default outline keys (identical on every platform: no primary modifier is
/// involved, and Alt is Option on macOS). Tod-style outliner conventions:
/// plain arrows move through the rows as displayed, Alt jumps over subtrees,
/// Shift moves the item.
///
/// - Up / Down: previous / next row in display order (Down from an expanded
///   item enters its first child; Up from a first child selects the parent)
/// - Home / End: first / last row
/// - Left / Right: collapse / expand (on a collapsed item or a leaf, Left
///   selects the parent; on an expanded item, Right selects the first child)
/// - Alt+Up / Alt+Down: previous / next sibling, jumping over subtrees
/// - Shift+Up / Shift+Down: move the selected work item before its previous /
///   after its next sibling
/// - Shift+Right / Shift+Left: indent (last child of the previous sibling) /
///   outdent (after its former parent). Tab / Shift+Tab are aliases; with no
///   work item or Chat selected they propagate, so Tab still walks focus out
///   of the Outline.
/// - On a Chat row the same keys move the Chat between work items:
///   Shift+Up / Shift+Down to the previous / next shown work item of its
///   Space in row order, Shift+Left (Shift+Tab) to its item's parent (out of
///   its item from the top level); Shift+Right (Tab) does nothing.
/// - On a project (Space root) row of a multi-project Outline, Shift+Up /
///   Shift+Down reorder the project roots (device-local view state);
///   Shift+Right / Shift+Left (Tab / Shift+Tab) propagate.
/// - Enter: open the selected item (Work Item Details)
/// - Mod+Enter (Cmd on macOS, Ctrl elsewhere): Start Chat in the selected
///   work item, or in a Chat row's work item. Stored as `mod-enter` and bound
///   through `platform_combo`; scoped like every key here, so it can never
///   reach the composer's own Mod+Enter send. The rebindable Start Chat
///   command stays unbound globally: `mod-enter` is reserved for the
///   composer (`send_combo_is_reserved`).
/// - F2 / R: rename in place (a Chat row opens the Chat rename dialog). R is
///   the laptop-friendly alias: F2 needs Fn there.
/// - N / Shift+N: create a sibling / child
/// - ? (Shift+/ on US layouts): the shortcut reference. Bound to the typed
///   character, so it follows the keyboard layout.
///
/// When an action has several keys, the first is the one tooltips show.
pub(crate) const OUTLINE_NAVIGATION_KEYS: [(&str, OutlineNavKey); 21] = [
    ("up", OutlineNavKey::SelectPrevious),
    ("down", OutlineNavKey::SelectNext),
    ("home", OutlineNavKey::SelectFirst),
    ("end", OutlineNavKey::SelectLast),
    ("left", OutlineNavKey::Collapse),
    ("right", OutlineNavKey::Expand),
    ("alt-up", OutlineNavKey::SelectPreviousSibling),
    ("alt-down", OutlineNavKey::SelectNextSibling),
    ("enter", OutlineNavKey::OpenSelected),
    ("mod-enter", OutlineNavKey::StartChat),
    ("f2", OutlineNavKey::RenameSelected),
    ("r", OutlineNavKey::RenameSelected),
    ("n", OutlineNavKey::CreateSibling),
    ("shift-n", OutlineNavKey::CreateChild),
    ("shift-up", OutlineNavKey::MoveItemUp),
    ("shift-down", OutlineNavKey::MoveItemDown),
    ("shift-right", OutlineNavKey::IndentItem),
    ("shift-left", OutlineNavKey::OutdentItem),
    ("tab", OutlineNavKey::IndentItem),
    ("shift-tab", OutlineNavKey::OutdentItem),
    ("?", OutlineNavKey::ShowShortcuts),
];

/// Bindings for the configurable Work Outline commands. An empty combo is
/// unbound (the user cleared it, or the command ships without a default); an
/// unparseable one falls back to the default, or to unbound when there is
/// none.
pub(crate) fn work_outline_bindings(keymap: &KeymapConfig) -> Vec<KeyBinding> {
    WorkOutlineCommand::ALL
        .into_iter()
        .filter_map(|command| {
            let combo = crate::work_outline::hints::command_combo(keymap, command)?;
            Some(KeyBinding::new(
                &platform_combo(&combo),
                RunWorkOutlineCommand(command),
                None,
            ))
        })
        .collect()
}

/// Diagnostics (`work_outline_keys` target): a Shift+Arrow / Tab keystroke
/// on the Overview with the focused element's key context stack and the
/// bindings it matches, so a press that never reaches the Outline shows why.
/// `phase` is `capture` (before bindings) or `unhandled` (no action took it).
pub(crate) fn log_restructure_keystroke(
    phase: &'static str,
    event: &gpui::KeyDownEvent,
    window: &gpui::Window,
    cx: &App,
) {
    let keystroke = &event.keystroke;
    let restructure_key = matches!(
        keystroke.key.as_str(),
        "up" | "down" | "left" | "right" | "tab"
    ) && keystroke.modifiers.shift
        || keystroke.key == "tab";
    if !restructure_key {
        return;
    }
    let context_stack = window.context_stack();
    let (matched, _) = cx
        .key_bindings()
        .borrow()
        .bindings_for_input(std::slice::from_ref(keystroke), &context_stack);
    let bindings: Vec<String> = matched
        .iter()
        .map(|binding| {
            format!(
                "{} ctx={:?}",
                binding.action().name(),
                binding.predicate().map(|p| p.to_string())
            )
        })
        .collect();
    tracing::info!(
        target: "work_outline_keys",
        phase,
        keystroke = %keystroke.unparse(),
        ?context_stack,
        ?bindings,
        "outline keystroke"
    );
}

/// Binds [`OUTLINE_NAVIGATION_KEYS`] under [`OUTLINE_NAVIGATION_CONTEXT`] to
/// the actions of `work_outline::outline_view`.
pub(crate) fn bind_work_outline_navigation(cx: &mut App) {
    use crate::work_outline::outline_view::{
        Collapse, CreateChild, CreateSibling, Expand, IndentItem, MoveItemDown, MoveItemUp,
        OpenSelected, OutdentItem, RenameSelected, SelectFirst, SelectLast, SelectNext,
        SelectNextSibling, SelectPrevious, SelectPreviousSibling, StartChatSelected,
    };
    let context = Some(OUTLINE_NAVIGATION_CONTEXT);
    cx.bind_keys(OUTLINE_NAVIGATION_KEYS.iter().map(|(keys, key)| {
        let keys = platform_combo(keys);
        let keys = keys.as_str();
        match key {
            OutlineNavKey::SelectNext => KeyBinding::new(keys, SelectNext, context),
            OutlineNavKey::SelectPrevious => KeyBinding::new(keys, SelectPrevious, context),
            OutlineNavKey::SelectFirst => KeyBinding::new(keys, SelectFirst, context),
            OutlineNavKey::SelectLast => KeyBinding::new(keys, SelectLast, context),
            OutlineNavKey::SelectNextSibling => KeyBinding::new(keys, SelectNextSibling, context),
            OutlineNavKey::SelectPreviousSibling => {
                KeyBinding::new(keys, SelectPreviousSibling, context)
            }
            OutlineNavKey::Expand => KeyBinding::new(keys, Expand, context),
            OutlineNavKey::Collapse => KeyBinding::new(keys, Collapse, context),
            OutlineNavKey::OpenSelected => KeyBinding::new(keys, OpenSelected, context),
            OutlineNavKey::StartChat => KeyBinding::new(keys, StartChatSelected, context),
            OutlineNavKey::RenameSelected => KeyBinding::new(keys, RenameSelected, context),
            OutlineNavKey::CreateSibling => KeyBinding::new(keys, CreateSibling, context),
            OutlineNavKey::CreateChild => KeyBinding::new(keys, CreateChild, context),
            OutlineNavKey::MoveItemUp => KeyBinding::new(keys, MoveItemUp, context),
            OutlineNavKey::MoveItemDown => KeyBinding::new(keys, MoveItemDown, context),
            OutlineNavKey::IndentItem => KeyBinding::new(keys, IndentItem, context),
            OutlineNavKey::OutdentItem => KeyBinding::new(keys, OutdentItem, context),
            OutlineNavKey::ShowShortcuts => KeyBinding::new(
                keys,
                RunWorkOutlineCommand(WorkOutlineCommand::ShowOutlineShortcuts),
                context,
            ),
        }
    }));
}

/// The shortcut reference card: Outline keys (Navigate, Edit, Restructure)
/// beside the Work Outline commands, every binding read from the live
/// keymap. Read-only; Esc or Close dismisses it.
fn shortcuts_card(theme: &Theme, cx: &mut Context<Shell>) -> AnyElement {
    let keymap = crate::settings::current(cx).keymap;
    let groups = crate::work_outline::hints::shortcut_reference(&keymap);
    let group_el = |group: &crate::work_outline::hints::ShortcutGroup| {
        let selector = format!("work-outline-shortcuts-{}", group.title.to_lowercase());
        div()
            .debug_selector(move || selector.clone())
            .flex()
            .flex_col()
            .gap(px(1.0))
            .child(
                div()
                    .pb(px(2.0))
                    .text_size(crate::typography::ui_rems(10.5))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text_muted)
                    .child(group.title),
            )
            .children(group.rows.iter().map(|row| {
                div()
                    .h(px(20.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.text)
                    .child(div().min_w_0().truncate().child(row.label))
                    .when_some(row.keys.clone(), |el, keys| {
                        el.child(popover::kbd_hint(theme, &keys))
                    })
            }))
    };
    let (outline_groups, command_groups) = groups.split_at(3);
    popover::dialog_card(theme)
        .w(px(600.0))
        .debug_selector(|| "work-outline-shortcuts".into())
        .child(popover::dialog_title(theme, "Outline shortcuts"))
        .child(
            div()
                .mt(px(12.0))
                .flex()
                .gap(px(24.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .children(outline_groups.iter().map(group_el)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .children(command_groups.iter().map(group_el)),
                ),
        )
        .child(
            div()
                .mt(px(14.0))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.0))
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_faint)
                        .child(crate::work_outline::hints::shortcuts_footer()),
                )
                .child(
                    popover::btn_ghost(theme, "Close", "work-outline-shortcuts-close")
                        .id("work-outline-shortcuts-close")
                        .debug_selector(|| "work-outline-shortcuts-close".into())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.resolve_work_outline_modal(false, window, cx);
                        })),
                ),
        )
        .into_any_element()
}

/// Run the modal's decision callback (at most once).
fn deliver_decision(modal: WorkOutlineModal, accept: bool, window: &mut Window, cx: &mut App) {
    match modal {
        WorkOutlineModal::Confirm(mut dialog) => {
            if let Some(callback) = dialog.on_decision.take() {
                callback(accept, window, cx);
            }
        }
        WorkOutlineModal::Archive(mut dialog) => {
            let policy = if accept { dialog.policy() } else { None };
            if let Some(callback) = dialog.on_decision.take() {
                callback(policy, window, cx);
            }
        }
        WorkOutlineModal::Drift(mut dialog) => {
            if let Some(callback) = dialog.on_decision.take() {
                callback(accept, window, cx);
            }
        }
        WorkOutlineModal::Compare(_)
        | WorkOutlineModal::FromChat(_)
        | WorkOutlineModal::StartChat(_)
        | WorkOutlineModal::Shortcuts => {}
    }
}

impl Shell {
    /// Whether a Work Outline modal is open.
    pub(super) fn work_outline_modal_open(&self) -> bool {
        self.work_outline_modal.is_some()
    }

    /// Escape inside a modal closes its open `/` skill popup first (the
    /// shell's capture-phase Escape would otherwise cancel the whole modal
    /// before the input sees the key). `true` when a popup was closed.
    pub(super) fn dismiss_work_outline_modal_skills(&mut self, cx: &mut Context<Self>) -> bool {
        let skills = match self.work_outline_modal.as_ref() {
            Some(WorkOutlineModal::StartChat(dialog)) => dialog.skills.clone(),
            Some(WorkOutlineModal::FromChat(dialog)) => dialog.skills.clone(),
            _ => return false,
        };
        if !skills.read(cx).is_open() {
            return false;
        }
        skills.update(cx, |skills, cx| skills.dismiss(cx));
        true
    }

    /// The harness whose skills the Work Outline editors offer: the one a
    /// new Chat would use (the composer's resolved harness), else Claude Code.
    pub(crate) fn skill_harness(&self, cx: &App) -> zeron_proto::HarnessId {
        self.composer
            .read(cx)
            .pickers()
            .read(cx)
            .resolved(cx)
            .harness
            .unwrap_or(zeron_proto::HarnessId::ClaudeCode)
    }

    /// Open the external reference of a work item per the web-link
    /// preference. See [`reference_open`]. Never touches manual PR/ticket
    /// links: it only opens a URL.
    pub(crate) fn open_work_item_reference(
        &mut self,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let on_chat = matches!(self.route, Route::Chat) && !self.active_chat.is_empty();
        let in_app = crate::settings::current(cx).open_web_links_in_zeron;
        match reference_open(
            url,
            on_chat,
            in_app,
            cfg!(any(target_os = "macos", target_os = "linux")),
        ) {
            ReferenceOpen::Internal(navigation) => {
                self.set_surfaces_open(true, cx);
                self.add_browser_surface(Some(navigation), window, cx);
            }
            ReferenceOpen::External(navigation) => cx.open_url(&navigation),
            ReferenceOpen::Rejected => {
                self.sidebar_notice = Some("That reference link can\u{2019}t be opened".into());
                cx.notify();
            }
        }
    }

    /// Show a window modal (discard draft, move with descendants, ...) and
    /// deliver the decision through `request.on_decision` (`true` =
    /// confirmed). Any other way of closing reports `false`.
    pub(crate) fn open_work_outline_confirm(
        &mut self,
        request: WorkOutlineConfirmRequest,
        cx: &mut Context<Self>,
    ) {
        self.begin_work_outline_modal(
            WorkOutlineModal::Confirm(ConfirmDialog {
                title: request.title,
                body: request.body,
                confirm_label: request.confirm_label,
                cancel_label: request.cancel_label,
                destructive: request.destructive,
                on_decision: Some(request.on_decision),
            }),
            cx,
        );
    }

    /// Show the drift confirmation and deliver the decision through
    /// `request.on_decision` (`true` = confirmed; any other close is `false`).
    pub(crate) fn open_work_outline_drift_confirm(
        &mut self,
        request: WorkOutlineDriftRequest,
        cx: &mut Context<Self>,
    ) {
        self.begin_work_outline_modal(
            WorkOutlineModal::Drift(DriftDialog {
                title: request.title,
                confirm_label: request.confirm_label,
                impact: request.impact,
                on_decision: Some(request.on_decision),
            }),
            cx,
        );
    }

    /// Open the Outline shortcut reference (palette, `?` in the Outline,
    /// the Outline toolbar). Rides the modal plumbing: it takes focus, Esc
    /// closes it, and focus returns to where it was.
    pub(crate) fn open_outline_shortcuts(&mut self, cx: &mut Context<Self>) {
        if matches!(self.work_outline_modal, Some(WorkOutlineModal::Shortcuts)) {
            return;
        }
        self.begin_work_outline_modal(WorkOutlineModal::Shortcuts, cx);
    }

    /// Show the archive dialog with its explicit descendant choice.
    pub(crate) fn open_work_outline_archive_confirm(
        &mut self,
        request: WorkOutlineArchiveRequest,
        cx: &mut Context<Self>,
    ) {
        self.begin_work_outline_modal(WorkOutlineModal::Archive(ArchiveDialog::new(request)), cx);
    }

    pub(super) fn begin_work_outline_modal(
        &mut self,
        modal: WorkOutlineModal,
        cx: &mut Context<Self>,
    ) {
        // A newer request supersedes an open one, which reports a cancel. No
        // window is at hand here, so the report is delivered on the next
        // render (`render_work_outline_modal`).
        if let Some(previous) = self.work_outline_modal.take() {
            self.work_outline_superseded.push(previous);
        }
        self.command_palette = None;
        self.work_outline_modal = Some(modal);
        self.work_outline_modal_focus_pending = true;
        cx.notify();
    }

    /// Close the modal as cancelled. Returns whether one was open.
    pub(super) fn cancel_work_outline_modal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.resolve_work_outline_modal(false, window, cx)
    }

    pub(super) fn resolve_work_outline_modal(
        &mut self,
        accept: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(modal) = self.work_outline_modal.take() else {
            return false;
        };
        self.work_outline_modal_focus_pending = false;
        if let Some(prior) = self.work_outline_prior_focus.take() {
            window.focus(&prior, cx);
        }
        cx.notify();
        window.defer(cx, move |window, cx| {
            deliver_decision(modal, accept, window, cx)
        });
        true
    }

    /// Single entry point for the Work Outline commands (palette, keymap):
    /// open the Overview route if needed, then let the Overview act.
    pub(crate) fn run_work_outline_command(
        &mut self,
        command: WorkOutlineCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.command_palette = None;
        // The shortcut reference is an overlay over whatever is showing; it
        // never switches route.
        if command == WorkOutlineCommand::ShowOutlineShortcuts {
            self.open_outline_shortcuts(cx);
            return;
        }
        // Open Outline from a Chat the Outline opened is the toggle's
        // Chat → tree half: it lands on that Chat's row, not wherever the
        // selection was.
        if command == WorkOutlineCommand::ToggleOutlineFocus
            || (command == WorkOutlineCommand::OpenOutline
                && self.focused_outline_opened_chat(window, cx))
        {
            self.toggle_outline_focus(window, cx);
            return;
        }
        if command == WorkOutlineCommand::OpenFullChat {
            self.open_full_chat(window, cx);
            return;
        }
        // Create Instruction from Chat acts where the Chat is; only when that
        // is impossible does it fall through to the Outline (with a notice).
        if command == WorkOutlineCommand::CreateInstructionFromChat
            && self.open_instruction_from_chat(window, cx).is_ok()
        {
            return;
        }
        // The Linear toggle is a no-op (with a notice) while no Space has
        // Linear on: it never leaves the current route just to explain.
        if command == WorkOutlineCommand::OpenLinear
            && !matches!(self.route, Route::Overview)
            && !crate::linear::panel::palette_visible(self.state.read(cx))
        {
            self.sidebar_notice = Some(crate::linear::LINEAR_OFF_NOTICE.into());
            cx.notify();
            return;
        }
        if !matches!(self.route, Route::Overview) {
            self.open_overview(cx);
        }
        let page = self.ensure_overview_page(cx);
        page.update(cx, |overview, cx| {
            overview.run_work_outline_command(command, window, cx)
        });
    }

    /// The Chat on screen and how: the full chat route's selected Chat, or
    /// the Overview chat panel's.
    fn shown_chat(&self, cx: &App) -> Option<OutlineChatReturn> {
        match self.route {
            Route::Chat => {
                self.state
                    .read(cx)
                    .selected_chat
                    .clone()
                    .map(|chat_id| OutlineChatReturn {
                        chat_id,
                        full_route: true,
                    })
            }
            Route::Overview => self
                .overview_page
                .as_ref()
                .and_then(|page| page.read(cx).shown_panel_chat(cx))
                .map(|chat_id| OutlineChatReturn {
                    chat_id,
                    full_route: false,
                }),
            _ => None,
        }
    }

    /// The composer of the Chat the Outline last opened has focus (where
    /// Enter on a Chat row lands it).
    fn focused_outline_opened_chat(&self, window: &Window, cx: &App) -> bool {
        if !self.composer.focus_handle(cx).contains_focused(window, cx) {
            return false;
        }
        let opened = self
            .overview_page
            .as_ref()
            .and_then(|page| page.read(cx).outline_chat_return().cloned());
        match (opened, self.shown_chat(cx)) {
            (Some(opened), Some(shown)) => opened.chat_id == shown.chat_id,
            _ => false,
        }
    }

    /// Toggle Outline Focus. From the Outline tree: back to the last Chat
    /// opened from the Outline, in the layout it was shown in, composer
    /// focused (a short notice when there is none). From anywhere else: the
    /// Outline with the shown Chat's row selected (ancestors expanded,
    /// scrolled into view) and the tree focused. The Chat's selection and
    /// composer draft are never touched on the way out.
    pub(crate) fn toggle_outline_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.command_palette = None;
        let tree_focused = matches!(self.route, Route::Overview)
            && self
                .overview_page
                .as_ref()
                .is_some_and(|page| page.read(cx).outline_tree_focused(window, cx));
        if tree_focused {
            self.return_to_outline_chat(cx);
            return;
        }
        let chat = self.shown_chat(cx);
        if !matches!(self.route, Route::Overview) {
            self.open_overview(cx);
        }
        let page = self.ensure_overview_page(cx);
        page.update(cx, |overview, cx| {
            overview.focus_outline_on_chat(chat, window, cx)
        });
    }

    /// Open Full Chat: on the Overview, switch to the full chat route of the
    /// chat panel's Chat or the selected Chat row
    /// (`Overview::full_chat_target`), exactly like the panel's "Open full
    /// chat →": the panel closes, the Chat is selected and its composer
    /// focused. The composer is the Shell's own, so a draft typed in the
    /// panel is still there. Anywhere else it does nothing (the full chat is
    /// already showing, or no Overview Chat is in play).
    pub(crate) fn open_full_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.command_palette = None;
        if !matches!(self.route, Route::Overview) {
            return;
        }
        let Some(page) = self.overview_page.clone() else {
            return;
        };
        let Some(chat_id) = page.read(cx).full_chat_target(window, cx) else {
            page.update(cx, |overview, cx| overview.show_full_chat_notice(cx));
            return;
        };
        page.update(cx, |overview, cx| overview.prepare_full_chat(&chat_id, cx));
        self.open_chat(chat_id, cx);
    }

    fn return_to_outline_chat(&mut self, cx: &mut Context<Self>) {
        let page = self.ensure_overview_page(cx);
        let target = page.read(cx).outline_chat_return().cloned();
        let target = target.filter(|target| {
            self.state
                .read(cx)
                .chats
                .iter()
                .any(|chat| chat.id == target.chat_id)
        });
        let Some(target) = target else {
            page.update(cx, |overview, cx| {
                overview.show_outline_notice("Open a Chat from the Outline first", cx)
            });
            return;
        };
        if target.full_route {
            self.open_chat(target.chat_id, cx);
        } else {
            // Deferred: reopening the panel calls back into this Shell.
            cx.defer(move |cx| {
                page.update(cx, |overview, cx| {
                    overview.return_to_outline_chat_panel(target.chat_id, cx)
                });
            });
        }
    }

    pub(super) fn render_work_outline_modal(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        for superseded in std::mem::take(&mut self.work_outline_superseded) {
            window.defer(cx, move |window, cx| {
                deliver_decision(superseded, false, window, cx)
            });
        }
        let theme = Theme::of(cx).for_popup();
        let modal = self.work_outline_modal.as_ref()?;
        // Move keyboard focus into the modal (remembering where it was), so
        // the outline's key actions (Enter, n, ...) cannot fire underneath it.
        if std::mem::take(&mut self.work_outline_modal_focus_pending) {
            if self.work_outline_prior_focus.is_none() {
                self.work_outline_prior_focus = window
                    .focused(cx)
                    .filter(|focused| *focused != self.work_outline_modal_focus);
            }
            match modal {
                WorkOutlineModal::FromChat(dialog) => {
                    let handle = dialog.input_focus(cx);
                    window.focus(&handle, cx);
                }
                WorkOutlineModal::StartChat(dialog) => {
                    let handle = dialog.input_focus(cx);
                    window.focus(&handle, cx);
                }
                _ => window.focus(&self.work_outline_modal_focus, cx),
            }
        }
        let card = match modal {
            WorkOutlineModal::Drift(dialog) => {
                let impact_line = format!(
                    "{} \u{b7} {}",
                    crate::work_outline::instructions::plural(
                        dialog.impact.chat_ids.len(),
                        "Chat",
                        "Chats"
                    ),
                    crate::work_outline::instructions::plural(
                        dialog.impact.item_ids.len(),
                        "work item",
                        "work items"
                    ),
                );
                popover::dialog_card(&theme)
                    .debug_selector(|| "work-outline-drift".into())
                    .child(popover::dialog_title(&theme, &dialog.title))
                    .child(
                        div()
                            .mt(px(6.0))
                            .child(popover::dialog_body(&theme, dialog.impact.body())),
                    )
                    .child(
                        div()
                            .debug_selector(|| "work-outline-drift-counts".into())
                            .mt(px(8.0))
                            .text_size(crate::typography::ui_rems(11.5))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(impact_line)),
                    )
                    .child(
                        div()
                            .mt(px(16.0))
                            .flex()
                            .flex_row()
                            .justify_end()
                            .gap(px(8.0))
                            .child(
                                popover::btn_ghost(&theme, "Cancel", "work-outline-drift-cancel")
                                    .id("work-outline-drift-cancel")
                                    .debug_selector(|| "work-outline-drift-cancel".into())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.resolve_work_outline_modal(false, window, cx);
                                    })),
                            )
                            .child(
                                popover::btn_primary(&theme, &dialog.confirm_label)
                                    .id("work-outline-drift-confirm")
                                    .debug_selector(|| "work-outline-drift-confirm".into())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.resolve_work_outline_modal(true, window, cx);
                                    })),
                            ),
                    )
                    .into_any_element()
            }
            WorkOutlineModal::Compare(dialog) => {
                super::instructions_ui::compare_card(dialog, self, &theme, cx)
            }
            WorkOutlineModal::FromChat(dialog) => {
                super::instructions_ui::from_chat_card(dialog, &theme, cx)
            }
            WorkOutlineModal::StartChat(dialog) => super::start_chat_ui::start_chat_card(
                dialog,
                self,
                f32::from(viewport.height),
                &theme,
                cx,
            ),
            WorkOutlineModal::Shortcuts => shortcuts_card(&theme, cx),
            WorkOutlineModal::Confirm(dialog) => {
                let accept = if dialog.destructive {
                    popover::btn_danger(&theme, &dialog.confirm_label)
                } else {
                    popover::btn_primary(&theme, &dialog.confirm_label)
                }
                .id("work-outline-confirm-accept")
                .debug_selector(|| "work-outline-confirm-accept".into())
                .on_click(cx.listener(|this, _, window, cx| {
                    this.resolve_work_outline_modal(true, window, cx);
                }));
                popover::dialog_card(&theme)
                    .child(popover::dialog_title(&theme, &dialog.title))
                    .child(
                        div()
                            .mt(px(6.0))
                            .child(popover::dialog_body(&theme, dialog.body.clone())),
                    )
                    .child(
                        div()
                            .mt(px(16.0))
                            .flex()
                            .flex_row()
                            .justify_end()
                            .gap(px(8.0))
                            .child(
                                popover::btn_ghost(
                                    &theme,
                                    &dialog.cancel_label,
                                    "work-outline-confirm-cancel",
                                )
                                .id("work-outline-confirm-cancel")
                                .debug_selector(|| "work-outline-confirm-cancel".into())
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.resolve_work_outline_modal(false, window, cx);
                                    },
                                )),
                            )
                            .child(accept),
                    )
                    .into_any_element()
            }
            WorkOutlineModal::Archive(dialog) => {
                let policy = dialog.policy();
                let has_descendants = dialog.descendant_count > 0;
                let mut card = popover::dialog_card(&theme)
                    .child(popover::dialog_title(&theme, "Archive work item?"));
                let body = if has_descendants {
                    format!(
                        "\u{201C}{}\u{201D} has {} descendant work item{}. Choose what happens to them.",
                        dialog.item_title,
                        dialog.descendant_count,
                        if dialog.descendant_count == 1 {
                            ""
                        } else {
                            "s"
                        }
                    )
                } else {
                    format!(
                        "\u{201C}{}\u{201D} will be archived. You can restore it later.",
                        dialog.item_title
                    )
                };
                card = card.child(div().mt(px(6.0)).child(popover::dialog_body(&theme, body)));
                if has_descendants {
                    let option = |id: &'static str,
                                  label: String,
                                  selected: bool,
                                  choice: ArchiveChoice,
                                  cx: &mut Context<Self>| {
                        popover::menu_row(&theme, selected, id)
                            .id(id)
                            .debug_selector(move || id.into())
                            .role(gpui::Role::RadioButton)
                            .aria_label(label.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(WorkOutlineModal::Archive(dialog)) =
                                    this.work_outline_modal.as_mut()
                                {
                                    dialog.choice = Some(choice);
                                    cx.notify();
                                }
                            }))
                            .child(icon(icons::CHECK).size(px(14.0)).flex_none().text_color(
                                if selected {
                                    theme.text
                                } else {
                                    gpui::transparent_black()
                                },
                            ))
                            .child(div().flex_1().min_w_0().child(SharedString::from(label)))
                    };
                    let archive_selected = dialog.choice == Some(ArchiveChoice::ArchiveDescendants);
                    let move_selected = dialog.choice == Some(ArchiveChoice::MoveDescendants);
                    let mut options = div().mt(px(12.0)).flex().flex_col().gap(px(2.0));
                    options = options.child(option(
                        "work-outline-archive-descendants",
                        "Archive the descendants too".into(),
                        archive_selected,
                        ArchiveChoice::ArchiveDescendants,
                        cx,
                    ));
                    options = options.child(option(
                        "work-outline-move-descendants",
                        "Move the descendants to another parent first".into(),
                        move_selected,
                        ArchiveChoice::MoveDescendants,
                        cx,
                    ));
                    if move_selected {
                        let mut list = div()
                            .id("work-outline-parent-list")
                            .ml(px(22.0))
                            .max_h(px(180.0))
                            .overflow_y_scroll()
                            .flex()
                            .flex_col();
                        for (ix, parent) in dialog.parents.iter().enumerate() {
                            let selected = ix == dialog.parent_ix;
                            list = list.child(
                                popover::menu_row(
                                    &theme,
                                    selected,
                                    format!("work-outline-parent-{ix}"),
                                )
                                .id(("work-outline-parent", ix))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(WorkOutlineModal::Archive(dialog)) =
                                        this.work_outline_modal.as_mut()
                                    {
                                        dialog.parent_ix = ix;
                                        cx.notify();
                                    }
                                }))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(parent.label.clone()),
                                ),
                            );
                        }
                        options = options.child(list);
                    }
                    card = card.child(options);
                }
                let confirm_label = match dialog.choice {
                    Some(ArchiveChoice::MoveDescendants) => "Move and archive",
                    _ => "Archive",
                };
                let confirm = if policy.is_some() {
                    popover::btn_danger(&theme, confirm_label)
                        .id("work-outline-archive-confirm")
                        .debug_selector(|| "work-outline-archive-confirm".into())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.resolve_work_outline_modal(true, window, cx);
                        }))
                        .into_any_element()
                } else {
                    // No valid choice yet: visibly inert, no handler.
                    popover::btn_danger(&theme, confirm_label)
                        .id("work-outline-archive-confirm-disabled")
                        .opacity(0.4)
                        .cursor_default()
                        .into_any_element()
                };
                card.child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "work-outline-archive-cancel")
                                .id("work-outline-archive-cancel")
                                .debug_selector(|| "work-outline-archive-cancel".into())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.resolve_work_outline_modal(false, window, cx);
                                })),
                        )
                        .child(confirm),
                )
                .into_any_element()
            }
        };
        let card = div()
            .track_focus(&self.work_outline_modal_focus)
            .child(card)
            .into_any_element();
        Some(popover::modal("work-outline-modal", viewport, card))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parents() -> Vec<WorkOutlineParentChoice> {
        vec![
            WorkOutlineParentChoice {
                work_item_id: None,
                label: "Space root".into(),
            },
            WorkOutlineParentChoice {
                work_item_id: Some("w-api".into()),
                label: "Launch v2 / API".into(),
            },
        ]
    }

    fn archive_dialog(descendants: usize) -> ArchiveDialog {
        ArchiveDialog::new(WorkOutlineArchiveRequest {
            item_title: "Launch v2".into(),
            descendant_count: descendants,
            parents: parents(),
            on_decision: Box::new(|_, _, _| {}),
        })
    }

    #[test]
    fn archive_with_descendants_requires_an_explicit_choice() {
        let mut dialog = archive_dialog(3);
        assert_eq!(dialog.policy(), None);
        dialog.choice = Some(ArchiveChoice::ArchiveDescendants);
        assert_eq!(
            dialog.policy(),
            Some(DescendantArchivePolicy::ArchiveDescendants)
        );
        dialog.choice = Some(ArchiveChoice::MoveDescendants);
        assert_eq!(
            dialog.policy(),
            Some(DescendantArchivePolicy::MoveDescendants {
                destination_parent_work_item_id: None
            })
        );
        dialog.parent_ix = 1;
        assert_eq!(
            dialog.policy(),
            Some(DescendantArchivePolicy::MoveDescendants {
                destination_parent_work_item_id: Some("w-api".into())
            })
        );
    }

    #[test]
    fn archive_without_descendants_needs_no_choice() {
        assert_eq!(
            archive_dialog(0).policy(),
            Some(DescendantArchivePolicy::ArchiveDescendants)
        );
    }

    #[test]
    fn moving_descendants_with_no_destination_is_not_confirmable() {
        let mut dialog = archive_dialog(2);
        dialog.parents.clear();
        dialog.choice = Some(ArchiveChoice::MoveDescendants);
        assert_eq!(dialog.policy(), None);
    }

    #[test]
    fn references_open_externally_unless_a_chat_browser_exists() {
        let url = "https://github.com/acme/launch/pull/42";
        // Overview-hosted details: external, whatever the preference.
        assert!(matches!(
            reference_open(url, false, true, true),
            ReferenceOpen::External(_)
        ));
        assert!(matches!(
            reference_open(url, true, true, true),
            ReferenceOpen::Internal(_)
        ));
        assert!(matches!(
            reference_open(url, true, false, true),
            ReferenceOpen::External(_)
        ));
        assert!(matches!(
            reference_open(url, true, true, false),
            ReferenceOpen::External(_)
        ));
    }

    #[test]
    fn non_web_references_are_rejected() {
        for url in ["file:///etc/passwd", "javascript:alert(1)", "not a url", ""] {
            assert_eq!(
                reference_open(url, false, true, true),
                ReferenceOpen::Rejected,
                "{url}"
            );
        }
    }

    #[test]
    fn navigation_keys_parse_are_unique_and_exclude_text_inputs() {
        for (index, (keys, _)) in OUTLINE_NAVIGATION_KEYS.iter().enumerate() {
            assert!(Keystroke::parse(&platform_combo(keys)).is_ok(), "{keys}");
            assert!(
                OUTLINE_NAVIGATION_KEYS[index + 1..]
                    .iter()
                    .all(|(other, _)| other != keys),
                "duplicate {keys}"
            );
        }
        let predicate = gpui::KeyBindingContextPredicate::parse(OUTLINE_NAVIGATION_CONTEXT)
            .expect("context predicate parses");
        let matches = |names: &[&str]| {
            let mut context = gpui::KeyContext::default();
            for name in names {
                context.add(*name);
            }
            predicate.eval(&[context])
        };
        assert!(matches(&["WorkOutline"]));
        for input in TEXT_INPUT_CONTEXTS {
            assert!(!matches(&["WorkOutline", input]), "{input}");
            assert!(OUTLINE_NAVIGATION_CONTEXT.contains(&format!("!{input}")));
        }
        assert!(!matches(&["Terminal"]));
    }

    #[test]
    fn command_bindings_skip_unbound_and_fall_back_for_unparseable() {
        let mut keymap = KeymapConfig::default();
        // Defaults: only Open Outline, Toggle Outline Focus, Open Full Chat,
        // Create Work Item and Toggle Linear Panel ship bound.
        assert_eq!(work_outline_bindings(&keymap).len(), 5);
        keymap.set(ShortcutId::RenameWorkItem, "mod-shift-m".into());
        assert_eq!(work_outline_bindings(&keymap).len(), 6);
        // Unparseable with a default: falls back to it.
        keymap.set(ShortcutId::OpenOutline, "mod-bogus-???".into());
        assert_eq!(work_outline_bindings(&keymap).len(), 6);
        // Unparseable without a default: unbound, never a panic.
        keymap.set(ShortcutId::MoveWorkItem, "mod-bogus-???".into());
        assert_eq!(work_outline_bindings(&keymap).len(), 6);
        keymap.set(ShortcutId::OpenOutline, String::new());
        assert_eq!(work_outline_bindings(&keymap).len(), 5);
    }

    fn test_shell<'a>(
        cx: &'a mut gpui::TestAppContext,
        dir: &std::path::Path,
    ) -> (Entity<Shell>, &'a mut gpui::VisualTestContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let data_dir = dir.to_path_buf();
        cx.add_window_view(move |_, cx| {
            let state = cx.new(|_| AppState::new());
            state.update(cx, |state, _| {
                // The overlays render only behind the Ready gate.
                state.connection = crate::state::ConnectionStatus::Ready;
                state.workspace_scope = Some(WorkspaceScope::Local);
                state.auth = Some(zeron_proto::AuthState::SignedOut);
            });
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: data_dir.clone(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: zeron_proto::HarnessId::Mock,
                },
                cx,
            )
        })
    }

    fn confirm_request(
        decisions: std::rc::Rc<std::cell::RefCell<Vec<bool>>>,
    ) -> WorkOutlineConfirmRequest {
        WorkOutlineConfirmRequest {
            title: "Discard changes?".into(),
            body: "Your edits to this work item are unsaved.".into(),
            confirm_label: "Discard".into(),
            cancel_label: "Keep editing".into(),
            destructive: true,
            on_decision: Box::new(move |accepted, _, _| decisions.borrow_mut().push(accepted)),
        }
    }

    fn escape() -> gpui::KeyDownEvent {
        gpui::KeyDownEvent {
            keystroke: Keystroke::parse("escape").unwrap(),
            is_held: false,
            prefer_character_input: false,
        }
    }

    #[gpui::test]
    fn escape_cancels_the_confirm_modal_and_reports_false(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let decisions = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        shell.update(cx, |shell, cx| {
            assert!(!shell.overlay_owns_keyboard(cx));
            shell.open_work_outline_confirm(confirm_request(decisions.clone()), cx);
            assert!(shell.overlay_owns_keyboard(cx), "modal owns the keyboard");
            assert!(
                shell.capture_escape_surface(cx),
                "escape is a shell surface"
            );
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(cx.debug_bounds("work-outline-confirm-accept").is_some());
        shell.update_in(cx, |shell, window, cx| {
            shell.on_key_down_capture(&escape(), window, cx)
        });
        cx.run_until_parked();
        assert_eq!(*decisions.borrow(), [false]);
        shell.update(cx, |shell, cx| {
            assert!(shell.work_outline_modal.is_none());
            assert!(!shell.overlay_owns_keyboard(cx));
        });
        // A second Escape reports nothing more.
        shell.update_in(cx, |shell, window, cx| {
            shell.on_key_down_capture(&escape(), window, cx)
        });
        cx.run_until_parked();
        assert_eq!(*decisions.borrow(), [false]);
    }

    #[gpui::test]
    fn confirm_button_reports_true_and_cancel_reports_false(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let decisions = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        for (selector, expected) in [
            ("work-outline-confirm-accept", true),
            ("work-outline-confirm-cancel", false),
        ] {
            shell.update(cx, |shell, cx| {
                shell.open_work_outline_confirm(confirm_request(decisions.clone()), cx)
            });
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear();
            });
            let center = cx.debug_bounds(selector).unwrap().center();
            cx.simulate_click(center, gpui::Modifiers::default());
            cx.run_until_parked();
            assert_eq!(decisions.borrow().last(), Some(&expected), "{selector}");
        }
        assert_eq!(decisions.borrow().len(), 2);
    }

    #[gpui::test]
    fn a_newer_modal_supersedes_the_open_one_with_a_cancel(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let first = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let second = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        shell.update(cx, |shell, cx| {
            shell.open_work_outline_confirm(confirm_request(first.clone()), cx);
            shell.open_work_outline_confirm(confirm_request(second.clone()), cx);
        });
        cx.run_until_parked();
        assert_eq!(*first.borrow(), [false]);
        assert!(second.borrow().is_empty());
        shell.update(cx, |shell, cx| assert!(shell.overlay_owns_keyboard(cx)));
    }

    #[gpui::test]
    fn archive_modal_keeps_confirm_disabled_until_a_choice(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let decisions: std::rc::Rc<std::cell::RefCell<Vec<Option<DescendantArchivePolicy>>>> =
            Default::default();
        let sink = decisions.clone();
        shell.update(cx, |shell, cx| {
            shell.open_work_outline_archive_confirm(
                WorkOutlineArchiveRequest {
                    item_title: "Launch v2".into(),
                    descendant_count: 2,
                    parents: parents(),
                    on_decision: Box::new(move |policy, _, _| sink.borrow_mut().push(policy)),
                },
                cx,
            )
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(
            cx.debug_bounds("work-outline-archive-confirm").is_none(),
            "no confirm target without an explicit choice"
        );
        let center = cx
            .debug_bounds("work-outline-move-descendants")
            .unwrap()
            .center();
        cx.simulate_click(center, gpui::Modifiers::default());
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        let confirm = cx
            .debug_bounds("work-outline-archive-confirm")
            .expect("choice enables confirm")
            .center();
        cx.simulate_click(confirm, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            *decisions.borrow(),
            [Some(DescendantArchivePolicy::MoveDescendants {
                destination_parent_work_item_id: None
            })]
        );
    }

    #[gpui::test]
    fn drift_modal_shows_counts_and_reports_the_decision(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let decisions = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        for (selector, expected) in [
            ("work-outline-drift-confirm", true),
            ("work-outline-drift-cancel", false),
        ] {
            let sink = decisions.clone();
            shell.update(cx, |shell, cx| {
                shell.open_work_outline_drift_confirm(
                    WorkOutlineDriftRequest {
                        title: "Move and change instructions?".into(),
                        confirm_label: "Move".into(),
                        impact: DriftImpact {
                            chat_ids: vec!["c1".into(), "c2".into()],
                            item_ids: vec!["w1".into()],
                        },
                        on_decision: Box::new(move |ok, _, _| sink.borrow_mut().push(ok)),
                    },
                    cx,
                );
                assert!(shell.overlay_owns_keyboard(cx));
            });
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear();
            });
            assert!(cx.debug_bounds("work-outline-drift").is_some());
            assert!(cx.debug_bounds("work-outline-drift-counts").is_some());
            let center = cx.debug_bounds(selector).unwrap().center();
            cx.simulate_click(center, gpui::Modifiers::default());
            cx.run_until_parked();
            assert_eq!(decisions.borrow().last(), Some(&expected), "{selector}");
        }
    }

    #[gpui::test]
    fn commands_route_to_the_overview(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        shell.update_in(cx, |shell, window, cx| {
            assert!(!matches!(shell.route, Route::Overview));
            shell.run_work_outline_command(WorkOutlineCommand::OpenOutline, window, cx);
            assert!(matches!(shell.route, Route::Overview));
            assert!(shell.overview_page.is_some());
        });
    }

    #[gpui::test]
    fn modal_takes_focus_so_outline_keys_do_nothing_and_cancel_restores_it(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::work_outline::model::row_key;
        use crate::work_outline::outline_view::{CreateSibling, OpenSelected};
        use crate::work_outline::store::fixture_ids::LAUNCH;

        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.spaces = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "s1", "deviceId": "d", "path": "/work/alpha",
                        "createdAt": "2026-01-01T00:00:00Z"
                    }))
                    .unwrap(),
                ];
            })
        });
        shell.update_in(cx, |shell, window, cx| {
            shell.run_work_outline_command(WorkOutlineCommand::OpenOutline, window, cx);
            let overview = shell.overview_page.clone().unwrap();
            overview.update(cx, |overview, cx| {
                overview.show_work_outline_fixture(window, cx)
            });
        });
        let overview = shell.read_with(cx, |shell, _| shell.overview_page.clone().unwrap());
        let outline = overview.read_with(cx, |overview, _| overview.outline_entity());
        let draw = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear();
            });
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear());
        };
        draw(cx);
        let outline_focus = cx.update(|_, cx| gpui::Focusable::focus_handle(outline.read(cx), cx));
        outline.update(cx, |outline, cx| {
            outline.select(&row_key::item("s1", LAUNCH), cx)
        });
        cx.update(|window, _| assert!(outline_focus.is_focused(window), "outline starts focused"));

        // Sanity: with no modal the outline's key actions do act.
        cx.update(|window, cx| window.dispatch_action(Box::new(CreateSibling), cx));
        outline.read_with(cx, |outline, _| assert!(outline.is_editing()));
        outline.update(cx, |outline, cx| outline.cancel_edit(cx));
        cx.update(|window, cx| window.focus(&outline_focus, cx));
        draw(cx);

        let decisions = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        shell.update(cx, |shell, cx| {
            shell.open_work_outline_confirm(confirm_request(decisions.clone()), cx)
        });
        draw(cx);
        cx.update(|window, _| {
            assert!(!outline_focus.is_focused(window), "modal took focus");
        });
        cx.update(|window, cx| {
            window.dispatch_action(Box::new(OpenSelected), cx);
            window.dispatch_action(Box::new(CreateSibling), cx);
        });
        cx.run_until_parked();
        overview.read_with(cx, |overview, _| assert!(!overview.has_work_item_details()));
        outline.read_with(cx, |outline, _| assert!(!outline.is_editing()));

        // Cancel restores focus to the outline, and its keys work again.
        shell.update_in(cx, |shell, window, cx| {
            shell.cancel_work_outline_modal(window, cx);
        });
        cx.run_until_parked();
        draw(cx);
        assert_eq!(*decisions.borrow(), [false]);
        cx.update(|window, _| assert!(outline_focus.is_focused(window), "focus restored"));
        cx.update(|window, cx| window.dispatch_action(Box::new(OpenSelected), cx));
        cx.run_until_parked();
        overview.read_with(cx, |overview, _| assert!(overview.has_work_item_details()));
    }

    fn shortcuts_open(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) -> bool {
        shell.read_with(cx, |shell, _| {
            matches!(shell.work_outline_modal, Some(WorkOutlineModal::Shortcuts))
        })
    }

    fn redraw(cx: &mut gpui::VisualTestContext) {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear());
    }

    #[gpui::test]
    fn show_outline_shortcuts_opens_the_reference_and_escape_closes_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        // The palette entry runs the command through the same entry point.
        shell.update_in(cx, |shell, window, cx| {
            assert!(!matches!(shell.route, Route::Overview));
            shell.run_work_outline_command(WorkOutlineCommand::ShowOutlineShortcuts, window, cx);
            assert!(!matches!(shell.route, Route::Overview), "no route switch");
            assert!(shell.overlay_owns_keyboard(cx));
        });
        assert!(shortcuts_open(&shell, cx));
        redraw(cx);
        assert!(cx.debug_bounds("work-outline-shortcuts").is_some());
        for group in ["navigate", "edit", "restructure", "commands"] {
            let selector: &'static str =
                Box::leak(format!("work-outline-shortcuts-{group}").into_boxed_str());
            assert!(cx.debug_bounds(selector).is_some(), "{group}");
        }
        shell.update_in(cx, |shell, window, cx| {
            shell.on_key_down_capture(&escape(), window, cx)
        });
        cx.run_until_parked();
        assert!(!shortcuts_open(&shell, cx));
        shell.update(cx, |shell, cx| assert!(!shell.overlay_owns_keyboard(cx)));

        // The keymap action (a user-recorded chord) opens it too, and Close
        // dismisses it.
        cx.update(|window, cx| {
            window.dispatch_action(
                Box::new(RunWorkOutlineCommand(
                    WorkOutlineCommand::ShowOutlineShortcuts,
                )),
                cx,
            )
        });
        cx.run_until_parked();
        assert!(shortcuts_open(&shell, cx));
        redraw(cx);
        let close = cx
            .debug_bounds("work-outline-shortcuts-close")
            .unwrap()
            .center();
        cx.simulate_click(close, gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(!shortcuts_open(&shell, cx));
    }

    #[gpui::test]
    fn question_mark_opens_the_reference_only_in_the_outline(cx: &mut gpui::TestAppContext) {
        use crate::work_outline::model::row_key;
        use crate::work_outline::outline_view::CreateSibling;
        use crate::work_outline::store::fixture_ids::LAUNCH;

        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.spaces = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "s1", "deviceId": "d", "path": "/work/alpha",
                        "createdAt": "2026-01-01T00:00:00Z"
                    }))
                    .unwrap(),
                ];
            })
        });
        shell.update_in(cx, |shell, window, cx| {
            shell.run_work_outline_command(WorkOutlineCommand::OpenOutline, window, cx);
            let overview = shell.overview_page.clone().unwrap();
            overview.update(cx, |overview, cx| {
                overview.show_work_outline_fixture(window, cx)
            });
        });
        let overview = shell.read_with(cx, |shell, _| shell.overview_page.clone().unwrap());
        let outline = overview.read_with(cx, |overview, _| overview.outline_entity());
        redraw(cx);
        let outline_focus = cx.update(|_, cx| gpui::Focusable::focus_handle(outline.read(cx), cx));
        outline.update(cx, |outline, cx| {
            outline.select(&row_key::item("s1", LAUNCH), cx)
        });
        cx.update(|window, cx| window.focus(&outline_focus, cx));
        redraw(cx);

        // `?` in the Outline opens it; Esc closes it and hands focus back.
        cx.simulate_keystrokes("?");
        cx.run_until_parked();
        assert!(shortcuts_open(&shell, cx), "? opens the reference");
        redraw(cx);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!shortcuts_open(&shell, cx), "Esc closes it");
        redraw(cx);
        cx.update(|window, _| assert!(outline_focus.is_focused(window), "focus restored"));

        // In the inline editor (a text input) `?` is just a character.
        cx.update(|window, cx| window.dispatch_action(Box::new(CreateSibling), cx));
        redraw(cx);
        outline.read_with(cx, |outline, _| assert!(outline.is_editing()));
        cx.simulate_keystrokes("?");
        cx.run_until_parked();
        assert!(!shortcuts_open(&shell, cx), "never from a text input");
        outline.update(cx, |outline, cx| outline.cancel_edit(cx));

        // Outside the Outline (the Overview search field) it does nothing.
        let search = overview.read_with(cx, |overview, cx| overview.search_focus_handle(cx));
        cx.update(|window, cx| window.focus(&search, cx));
        redraw(cx);
        cx.simulate_keystrokes("?");
        cx.run_until_parked();
        assert!(!shortcuts_open(&shell, cx), "not outside the Outline");

        // The Outline toolbar button opens it as well.
        let button = cx.debug_bounds("outline-shortcuts").unwrap().center();
        cx.simulate_click(button, gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(shortcuts_open(&shell, cx), "toolbar button opens it");
    }

    /// A Shell on the Outline fixture (one Space, its Chats live), with the
    /// app's real keymap installed. Returns the Overview and its Outline.
    fn toggle_shell<'a>(
        cx: &'a mut gpui::TestAppContext,
        dir: &std::path::Path,
    ) -> (
        Entity<Shell>,
        Entity<crate::overview::Overview>,
        Entity<crate::work_outline::outline_view::OutlineView>,
        &'a mut gpui::VisualTestContext,
    ) {
        use crate::work_outline::store::fixture_ids::*;
        // The transcript and composer render here (the chat panel and the
        // chat route), so their globals must exist.
        cx.update(|cx| {
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
            crate::settings::init(crate::settings::UiSettings::default(), dir, cx);
        });
        let (shell, cx) = test_shell(cx, dir);
        cx.update(|_, cx| {
            crate::shell::apply_keymap(
                cx,
                &KeymapConfig::default(),
                crate::settings::ComposerSendBehavior::default(),
            )
        });
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.spaces = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "s1", "deviceId": "d", "path": "/work/alpha",
                        "createdAt": "2026-01-01T00:00:00Z"
                    }))
                    .unwrap(),
                ];
                state.chats = [CHAT_API, CHAT_AUTH, CHAT_UI, CHAT_LOOSE_A]
                    .iter()
                    .map(|id| {
                        serde_json::from_value(serde_json::json!({
                            "id": id, "deviceId": "d", "archived": false,
                            "title": format!("Chat {id}"), "spaceId": "s1",
                            "cwd": "/work/alpha",
                            "createdAt": chrono::Utc::now(),
                            "lastMessageAt": chrono::Utc::now(),
                        }))
                        .unwrap()
                    })
                    .collect();
            })
        });
        shell.update_in(cx, |shell, window, cx| {
            shell.run_work_outline_command(WorkOutlineCommand::OpenOutline, window, cx);
            let overview = shell.overview_page.clone().unwrap();
            overview.update(cx, |overview, cx| {
                overview.show_work_outline_fixture(window, cx)
            });
        });
        let overview = shell.read_with(cx, |shell, _| shell.overview_page.clone().unwrap());
        let outline = overview.read_with(cx, |overview, _| overview.outline_entity());
        redraw(cx);
        (shell, overview, outline, cx)
    }

    /// The toggle's key as the app binds it on this platform.
    fn toggle_key() -> String {
        platform_combo(ShortcutId::ToggleOutlineFocus.default_combo())
    }

    fn tree_focused(
        outline: &Entity<crate::work_outline::outline_view::OutlineView>,
        cx: &mut gpui::VisualTestContext,
    ) -> bool {
        cx.update(|window, cx| {
            gpui::Focusable::focus_handle(outline.read(cx), cx).is_focused(window)
        })
    }

    fn composer_focused(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) -> bool {
        cx.update(|window, cx| shell.read(cx).composer.focus_handle(cx).is_focused(window))
    }

    fn outline_selection(
        outline: &Entity<crate::work_outline::outline_view::OutlineView>,
        cx: &mut gpui::VisualTestContext,
    ) -> Option<String> {
        outline.read_with(cx, |outline, _| outline.selected_key().map(str::to_owned))
    }

    /// Enter on a Chat row opens it beside the Outline with the composer
    /// focused; the toggle key (through `apply_keymap`, from the focused
    /// composer) jumps to that Chat's row, expanding its collapsed ancestor,
    /// and focuses the tree; pressing it again returns to the Chat with the
    /// composer focused and the draft intact. Selection never changes.
    #[gpui::test]
    fn toggle_outline_focus_round_trips_between_the_chat_panel_and_its_row(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::work_outline::model::row_key;
        use crate::work_outline::store::fixture_ids::*;

        let dir = tempfile::tempdir().unwrap();
        let (shell, overview, outline, cx) = toggle_shell(cx, dir.path());
        let chat_key = row_key::chat(CHAT_AUTH);
        let launch = row_key::item("s1", LAUNCH);

        // Enter on the Chat row: the panel opens on it, composer focused.
        outline.update(cx, |outline, cx| outline.select(&chat_key, cx));
        cx.update(|window, cx| {
            let handle = gpui::Focusable::focus_handle(outline.read(cx), cx);
            window.focus(&handle, cx)
        });
        redraw(cx);
        cx.simulate_keystrokes("enter");
        redraw(cx);
        assert_eq!(
            shell.read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.clone()),
            Some(CHAT_AUTH.to_owned())
        );
        assert_eq!(
            overview.read_with(cx, |o, cx| o.shown_panel_chat(cx)),
            Some(CHAT_AUTH.to_owned())
        );
        assert!(composer_focused(&shell, cx), "Enter focuses the composer");
        cx.simulate_input("half-written prompt");
        redraw(cx);

        // Collapse the Chat's top-level ancestor so the jump must expand it.
        cx.update(|window, cx| {
            let handle = gpui::Focusable::focus_handle(outline.read(cx), cx);
            window.focus(&handle, cx)
        });
        outline.update(cx, |outline, cx| outline.select(&launch, cx));
        redraw(cx);
        cx.simulate_keystrokes("left");
        redraw(cx);
        outline.read_with(cx, |outline, _| {
            assert!(outline.model().position(&chat_key).is_none(), "collapsed")
        });
        let composer = shell.read_with(cx, |shell, _| shell.composer.clone());
        cx.update(|window, cx| window.focus(&composer.focus_handle(cx), cx));
        redraw(cx);
        assert!(composer_focused(&shell, cx));

        // Chat -> tree.
        cx.simulate_keystrokes(&toggle_key());
        redraw(cx);
        assert!(tree_focused(&outline, cx), "the tree has focus");
        assert_eq!(outline_selection(&outline, cx), Some(chat_key.clone()));
        outline.read_with(cx, |outline, _| {
            assert!(outline.model().position(&chat_key).is_some(), "expanded")
        });
        assert!(
            cx.debug_bounds("outline-row-chat:chat-auth").is_some(),
            "the row is rendered (scrolled into view)"
        );
        assert_eq!(
            shell.read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.clone()),
            Some(CHAT_AUTH.to_owned()),
            "the Chat stays selected"
        );

        // Tree -> Chat.
        cx.simulate_keystrokes(&toggle_key());
        redraw(cx);
        assert!(composer_focused(&shell, cx), "back in the composer");
        assert!(shell.read_with(cx, |shell, _| matches!(shell.route, Route::Overview)));
        assert_eq!(
            overview.read_with(cx, |o, cx| o.shown_panel_chat(cx)),
            Some(CHAT_AUTH.to_owned())
        );
        assert_eq!(
            composer.read_with(cx, |composer, cx| composer.draft_text(cx)),
            "half-written prompt",
            "the draft survives the round trip"
        );
        assert_eq!(outline_selection(&outline, cx), Some(chat_key.clone()));

        // Open Outline (Shift+Cmd+L) from that Chat's composer is the same
        // Chat -> row jump, even after the selection moved elsewhere.
        outline.update(cx, |outline, cx| outline.select(&launch, cx));
        cx.simulate_keystrokes(&platform_combo(ShortcutId::OpenOutline.default_combo()));
        redraw(cx);
        assert!(tree_focused(&outline, cx));
        assert_eq!(outline_selection(&outline, cx), Some(chat_key));
    }

    /// From the full chat route the toggle lands on the Outline row and
    /// returns to the full chat route.
    #[gpui::test]
    fn toggle_outline_focus_returns_to_the_full_chat_route(cx: &mut gpui::TestAppContext) {
        use crate::work_outline::model::row_key;
        use crate::work_outline::store::fixture_ids::*;

        let dir = tempfile::tempdir().unwrap();
        let (shell, _overview, outline, cx) = toggle_shell(cx, dir.path());
        shell.update(cx, |shell, cx| shell.open_chat(CHAT_UI.into(), cx));
        redraw(cx);
        assert!(composer_focused(&shell, cx));
        cx.simulate_keystrokes(&toggle_key());
        redraw(cx);
        assert!(shell.read_with(cx, |shell, _| matches!(shell.route, Route::Overview)));
        assert!(tree_focused(&outline, cx));
        assert_eq!(
            outline_selection(&outline, cx),
            Some(row_key::chat(CHAT_UI))
        );
        cx.simulate_keystrokes(&toggle_key());
        redraw(cx);
        assert!(shell.read_with(cx, |shell, _| matches!(shell.route, Route::Chat)));
        assert!(composer_focused(&shell, cx));
        assert_eq!(
            shell.read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.clone()),
            Some(CHAT_UI.to_owned())
        );
    }

    /// From the tree with no Chat to return to: a notice, nothing else.
    #[gpui::test]
    fn toggle_outline_focus_without_a_chat_shows_a_notice(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, _overview, outline, cx) = toggle_shell(cx, dir.path());
        cx.update(|window, cx| {
            let handle = gpui::Focusable::focus_handle(outline.read(cx), cx);
            window.focus(&handle, cx)
        });
        redraw(cx);
        cx.simulate_keystrokes(&toggle_key());
        redraw(cx);
        assert!(tree_focused(&outline, cx));
        assert!(shell.read_with(cx, |shell, _| matches!(shell.route, Route::Overview)));
        assert_eq!(
            outline.read_with(cx, |outline, _| outline.notice().map(str::to_owned)),
            Some("Open a Chat from the Outline first".to_owned())
        );
    }

    /// Open Full Chat's key as the app binds it on this platform.
    fn full_chat_key() -> String {
        platform_combo(ShortcutId::OpenFullChat.default_combo())
    }

    fn route_is_chat(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) -> bool {
        shell.read_with(cx, |shell, _| matches!(shell.route, Route::Chat))
    }

    fn selected_chat(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) -> Option<String> {
        shell.read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.clone())
    }

    /// From the chat panel's focused composer (opened with Enter on its
    /// row), Open Full Chat through the app keymap closes the panel and
    /// shows that Chat as the full chat, composer focused and the draft
    /// intact. Toggle Outline Focus then lands on its Outline row, and again
    /// returns to the full chat with the draft still there.
    #[gpui::test]
    fn open_full_chat_from_the_panel_composer_keeps_the_draft_and_toggles_back(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::work_outline::model::row_key;
        use crate::work_outline::store::fixture_ids::*;

        let dir = tempfile::tempdir().unwrap();
        let (shell, overview, outline, cx) = toggle_shell(cx, dir.path());
        let chat_key = row_key::chat(CHAT_AUTH);
        outline.update(cx, |outline, cx| outline.select(&chat_key, cx));
        cx.update(|window, cx| {
            let handle = gpui::Focusable::focus_handle(outline.read(cx), cx);
            window.focus(&handle, cx)
        });
        redraw(cx);
        cx.simulate_keystrokes("enter");
        redraw(cx);
        assert_eq!(
            overview.read_with(cx, |o, cx| o.shown_panel_chat(cx)),
            Some(CHAT_AUTH.to_owned())
        );
        assert!(composer_focused(&shell, cx));
        cx.simulate_input("half-written prompt");
        redraw(cx);
        let composer = shell.read_with(cx, |shell, _| shell.composer.clone());

        cx.simulate_keystrokes(&full_chat_key());
        redraw(cx);
        assert!(route_is_chat(&shell, cx), "the full chat route");
        assert_eq!(selected_chat(&shell, cx), Some(CHAT_AUTH.to_owned()));
        assert!(composer_focused(&shell, cx), "composer focused");
        assert_eq!(
            composer.read_with(cx, |composer, cx| composer.draft_text(cx)),
            "half-written prompt",
            "the draft survives"
        );
        assert_eq!(
            overview.read_with(cx, |o, cx| o.shown_panel_chat(cx)),
            None,
            "the panel closed, as the button closes it"
        );

        // Full chat -> its Outline row -> back to the full chat.
        cx.simulate_keystrokes(&toggle_key());
        redraw(cx);
        assert!(shell.read_with(cx, |shell, _| matches!(shell.route, Route::Overview)));
        assert!(tree_focused(&outline, cx));
        assert_eq!(outline_selection(&outline, cx), Some(chat_key));
        cx.simulate_keystrokes(&toggle_key());
        redraw(cx);
        assert!(route_is_chat(&shell, cx));
        assert!(composer_focused(&shell, cx));
        assert_eq!(selected_chat(&shell, cx), Some(CHAT_AUTH.to_owned()));
        assert_eq!(
            composer.read_with(cx, |composer, cx| composer.draft_text(cx)),
            "half-written prompt"
        );

        // On the full chat route the key has nothing more to do.
        cx.simulate_keystrokes(&full_chat_key());
        redraw(cx);
        assert!(route_is_chat(&shell, cx));
        assert_eq!(selected_chat(&shell, cx), Some(CHAT_AUTH.to_owned()));
    }

    /// With the tree focused on a Chat row (no panel), the key opens that
    /// Chat's full chat; the row wins over a panel showing another Chat.
    /// On a work item row with no panel it only shows a notice.
    #[gpui::test]
    fn open_full_chat_from_a_selected_outline_chat_row(cx: &mut gpui::TestAppContext) {
        use crate::work_outline::model::row_key;
        use crate::work_outline::store::fixture_ids::*;

        let dir = tempfile::tempdir().unwrap();
        let (shell, overview, outline, cx) = toggle_shell(cx, dir.path());
        let focus_tree = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                let handle = gpui::Focusable::focus_handle(outline.read(cx), cx);
                window.focus(&handle, cx)
            });
            redraw(cx);
        };

        // A work item row and no panel: a notice, nothing else.
        outline.update(cx, |outline, cx| {
            outline.select(&row_key::item("s1", LAUNCH), cx)
        });
        focus_tree(cx);
        cx.simulate_keystrokes(&full_chat_key());
        redraw(cx);
        assert!(!route_is_chat(&shell, cx));
        assert!(tree_focused(&outline, cx));
        assert_eq!(
            outline.read_with(cx, |outline, _| outline.notice().map(str::to_owned)),
            Some("Select a Chat or open one beside the Outline first".to_owned())
        );

        // Panel on API, tree focused on the UI Chat row: the row wins.
        outline.update(cx, |outline, cx| {
            outline.select(&row_key::chat(CHAT_API), cx)
        });
        focus_tree(cx);
        cx.simulate_keystrokes("enter");
        redraw(cx);
        assert_eq!(
            overview.read_with(cx, |o, cx| o.shown_panel_chat(cx)),
            Some(CHAT_API.to_owned())
        );
        outline.update(cx, |outline, cx| {
            outline.select(&row_key::chat(CHAT_UI), cx)
        });
        focus_tree(cx);
        cx.simulate_keystrokes(&full_chat_key());
        redraw(cx);
        assert!(route_is_chat(&shell, cx));
        assert_eq!(selected_chat(&shell, cx), Some(CHAT_UI.to_owned()));
        assert!(composer_focused(&shell, cx));
    }

    /// In List mode the selected row is the one the chat panel shows (its
    /// highlight): with focus outside the panel (the search field), the key
    /// opens that Chat's full chat.
    #[gpui::test]
    fn open_full_chat_from_the_highlighted_list_row(cx: &mut gpui::TestAppContext) {
        use crate::work_outline::store::fixture_ids::*;

        let dir = tempfile::tempdir().unwrap();
        let (shell, overview, _outline, cx) = toggle_shell(cx, dir.path());
        overview.update(cx, |overview, cx| {
            overview.show_list_with_panel_for_test(CHAT_API.into(), cx)
        });
        redraw(cx);
        assert_eq!(
            overview.read_with(cx, |o, cx| o.shown_panel_chat(cx)),
            Some(CHAT_API.to_owned())
        );
        cx.update(|window, cx| {
            let search = overview.read(cx).search_focus_handle(cx);
            window.focus(&search, cx)
        });
        redraw(cx);
        assert!(!composer_focused(&shell, cx));
        cx.simulate_keystrokes(&full_chat_key());
        redraw(cx);
        assert!(route_is_chat(&shell, cx));
        assert_eq!(selected_chat(&shell, cx), Some(CHAT_API.to_owned()));
        assert!(composer_focused(&shell, cx));
    }

    /// Through the real keymap, Open Full Chat's and Toggle Outline Focus's
    /// default keys each reach exactly one action in every context: theirs.
    /// Neither the composer, the Outline keys, gpui-base editing, nor the
    /// fixed app keys bind them.
    #[gpui::test]
    fn open_full_chat_and_toggle_keys_bind_only_their_command(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (_shell, _overview, _outline, cx) = toggle_shell(cx, dir.path());
        for (key, command) in [
            (full_chat_key(), WorkOutlineCommand::OpenFullChat),
            (toggle_key(), WorkOutlineCommand::ToggleOutlineFocus),
        ] {
            let keystroke = gpui::Keystroke::parse(&key).unwrap();
            let bindings = cx.update(|_, cx| cx.all_bindings_for_input(&[keystroke]));
            assert!(!bindings.is_empty(), "{key} is bound");
            for binding in bindings {
                assert!(
                    binding.action().partial_eq(&RunWorkOutlineCommand(command)),
                    "{key} also binds {:?}",
                    binding.action().name()
                );
            }
        }
        // The old toggle default is free again.
        let old = gpui::Keystroke::parse(&platform_combo("mod-\\")).unwrap();
        assert!(
            cx.update(|_, cx| cx.all_bindings_for_input(&[old]))
                .is_empty()
        );
    }

    /// Let `ms` of debounce time pass and settle.
    fn wait_ms(cx: &mut gpui::VisualTestContext, ms: u64) {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(ms));
        redraw(cx);
    }

    fn panel_chat(
        overview: &Entity<crate::overview::Overview>,
        cx: &mut gpui::VisualTestContext,
    ) -> Option<String> {
        overview.read_with(cx, |o, cx| o.shown_panel_chat(cx))
    }

    /// The selected row's Chat, if it is a live Chat row.
    fn selected_row_chat(
        outline: &Entity<crate::work_outline::outline_view::OutlineView>,
        cx: &mut gpui::VisualTestContext,
    ) -> Option<String> {
        outline.read_with(cx, |outline, _| outline.selected_chat())
    }

    /// Select the row just above `key` and focus the tree, so one Down
    /// arrow lands on `key` through the real keymap.
    fn park_above(
        outline: &Entity<crate::work_outline::outline_view::OutlineView>,
        key: &str,
        cx: &mut gpui::VisualTestContext,
    ) {
        let above = outline.read_with(cx, |outline, _| {
            let ix = outline.model().position(key).expect("row shown");
            outline.model().rows[ix - 1].key.clone()
        });
        outline.update(cx, |outline, cx| outline.select(&above, cx));
        cx.update(|window, cx| {
            let handle = gpui::Focusable::focus_handle(outline.read(cx), cx);
            window.focus(&handle, cx)
        });
        redraw(cx);
    }

    /// Press `key` until the selection is a live Chat row other than
    /// `from`; every step waits `step_ms`. Returns that Chat.
    fn arrow_to_next_chat(
        outline: &Entity<crate::work_outline::outline_view::OutlineView>,
        key: &str,
        from: Option<&str>,
        step_ms: u64,
        cx: &mut gpui::VisualTestContext,
    ) -> String {
        for _ in 0..20 {
            cx.simulate_keystrokes(key);
            wait_ms(cx, step_ms);
            if let Some(chat) = selected_row_chat(outline, cx)
                && Some(chat.as_str()) != from
            {
                return chat;
            }
        }
        panic!("no further Chat row with {key}");
    }

    /// Arrowing onto Chat rows previews each one in the chat panel after the
    /// debounce while the tree keeps focus (the composer never takes it and
    /// the route never changes); arrowing onto a work item row leaves the
    /// last preview; keys keep working; Enter then focuses the composer.
    #[gpui::test]
    fn arrowing_onto_chat_rows_previews_them_while_the_tree_keeps_focus(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::work_outline::model::row_key;
        use crate::work_outline::store::fixture_ids::*;

        let dir = tempfile::tempdir().unwrap();
        let (shell, overview, outline, cx) = toggle_shell(cx, dir.path());
        park_above(&outline, &row_key::chat(CHAT_API), cx);
        assert_eq!(panel_chat(&overview, cx), None);

        cx.simulate_keystrokes("down");
        redraw(cx);
        assert_eq!(selected_row_chat(&outline, cx), Some(CHAT_API.to_owned()));
        assert_eq!(panel_chat(&overview, cx), None, "not before the debounce");
        assert!(overview.read_with(cx, |o, _| o.outline_preview_pending()));
        wait_ms(cx, 120);
        assert_eq!(panel_chat(&overview, cx), Some(CHAT_API.to_owned()));
        assert!(tree_focused(&outline, cx), "the tree keeps focus");
        assert!(!composer_focused(&shell, cx));
        assert!(!route_is_chat(&shell, cx), "never the full chat route");
        assert_eq!(selected_chat(&shell, cx), Some(CHAT_API.to_owned()));

        // The next Chat row: the panel follows, focus stays.
        let next = arrow_to_next_chat(&outline, "down", Some(CHAT_API), 120, cx);
        assert_eq!(panel_chat(&overview, cx), Some(next.clone()));
        assert!(tree_focused(&outline, cx));
        assert!(!composer_focused(&shell, cx));

        // Up onto a work item row: the panel keeps the last preview.
        for _ in 0..20 {
            cx.simulate_keystrokes("up");
            wait_ms(cx, 120);
            if outline.read_with(cx, |o, _| o.selected_item().is_some()) {
                break;
            }
        }
        assert!(outline.read_with(cx, |o, _| o.selected_item().is_some()));
        assert_eq!(panel_chat(&overview, cx), Some(next.clone()));
        assert!(tree_focused(&outline, cx));

        // Keys still work after previews: back down to a Chat row, Enter
        // opens it beside the Outline with the composer focused.
        let chat = arrow_to_next_chat(&outline, "down", None, 120, cx);
        assert_eq!(panel_chat(&overview, cx), Some(chat.clone()));
        cx.simulate_keystrokes("enter");
        redraw(cx);
        assert!(composer_focused(&shell, cx), "Enter focuses the composer");
        assert_eq!(panel_chat(&overview, cx), Some(chat.clone()));
        assert!(!route_is_chat(&shell, cx));
        // And the toggle from the composer lands back on that row.
        cx.simulate_keystrokes(&toggle_key());
        redraw(cx);
        assert!(tree_focused(&outline, cx));
        assert_eq!(selected_row_chat(&outline, cx), Some(chat));
    }

    /// Moves faster than the debounce coalesce: crossing several Chat rows
    /// never shows an intermediate one, and only the resting row is shown.
    #[gpui::test]
    fn outline_preview_debounce_coalesces_rapid_moves(cx: &mut gpui::TestAppContext) {
        use crate::work_outline::model::row_key;
        use crate::work_outline::store::fixture_ids::*;

        let dir = tempfile::tempdir().unwrap();
        let (_shell, overview, outline, cx) = toggle_shell(cx, dir.path());
        park_above(&outline, &row_key::chat(CHAT_API), cx);
        cx.simulate_keystrokes("down");
        wait_ms(cx, 40);
        assert_eq!(selected_row_chat(&outline, cx), Some(CHAT_API.to_owned()));
        // Keep moving every 40 ms, across at least one more Chat row.
        let last = arrow_to_next_chat(&outline, "down", Some(CHAT_API), 40, cx);
        assert_ne!(last, CHAT_API);
        assert_eq!(panel_chat(&overview, cx), None, "nothing shown mid-burst");
        wait_ms(cx, 120);
        assert_eq!(panel_chat(&overview, cx), Some(last));
    }

    /// A click on a Chat row previews it too (focus in the tree); with Work
    /// Item Details in the panel slot, browsing Chat rows leaves Details
    /// open and does not switch the selected Chat.
    #[gpui::test]
    fn clicks_preview_and_work_item_details_keep_the_slot(cx: &mut gpui::TestAppContext) {
        use crate::work_outline::model::row_key;
        use crate::work_outline::store::fixture_ids::*;

        let dir = tempfile::tempdir().unwrap();
        let (shell, overview, outline, cx) = toggle_shell(cx, dir.path());
        let row = cx
            .debug_bounds("outline-row-chat:chat-ui")
            .unwrap()
            .center();
        cx.simulate_click(row, gpui::Modifiers::default());
        redraw(cx);
        assert_eq!(selected_row_chat(&outline, cx), Some(CHAT_UI.to_owned()));
        wait_ms(cx, 120);
        assert_eq!(panel_chat(&overview, cx), Some(CHAT_UI.to_owned()));
        assert!(tree_focused(&outline, cx));
        assert!(!composer_focused(&shell, cx));

        // Details for a work item: Enter on its row.
        outline.update(cx, |outline, cx| {
            outline.select(&row_key::item("s1", LAUNCH), cx)
        });
        cx.update(|window, cx| {
            let handle = gpui::Focusable::focus_handle(outline.read(cx), cx);
            window.focus(&handle, cx)
        });
        redraw(cx);
        cx.simulate_keystrokes("enter");
        redraw(cx);
        assert!(overview.read_with(cx, |o, _| o.has_work_item_details()));
        park_above(&outline, &row_key::chat(CHAT_API), cx);
        cx.simulate_keystrokes("down");
        wait_ms(cx, 120);
        assert_eq!(selected_row_chat(&outline, cx), Some(CHAT_API.to_owned()));
        assert!(
            overview.read_with(cx, |o, _| o.has_work_item_details()),
            "Details stay open"
        );
        assert_eq!(selected_chat(&shell, cx), Some(CHAT_UI.to_owned()));
        assert!(tree_focused(&outline, cx));
    }

    // ----- Find (⌘F) in the Overview ------------------------------------

    fn find_key() -> String {
        platform_combo(super::super::FOCUS_OVERVIEW_SEARCH_COMBO)
    }

    fn search_focused(
        overview: &Entity<crate::overview::Overview>,
        cx: &mut gpui::VisualTestContext,
    ) -> bool {
        cx.update(|window, cx| overview.read(cx).search_focused(window, cx))
    }

    fn body_focused(
        overview: &Entity<crate::overview::Overview>,
        cx: &mut gpui::VisualTestContext,
    ) -> bool {
        cx.update(|window, cx| overview.read(cx).body_focused(window))
    }

    fn search_text(
        overview: &Entity<crate::overview::Overview>,
        cx: &mut gpui::VisualTestContext,
    ) -> String {
        overview.read_with(cx, |overview, cx| overview.search_text(cx))
    }

    fn set_mode(
        overview: &Entity<crate::overview::Overview>,
        mode: crate::overview::OverviewMode,
        cx: &mut gpui::VisualTestContext,
    ) {
        overview.update(cx, |overview, cx| overview.set_mode_for_test(mode, cx));
        redraw(cx);
    }

    /// Through the real keymap, ⌘F focuses the shared search field and
    /// selects its text in List, Canvas and Outline, whatever had focus.
    #[gpui::test]
    fn find_focuses_the_search_in_list_canvas_and_outline(cx: &mut gpui::TestAppContext) {
        use crate::overview::OverviewMode;

        let dir = tempfile::tempdir().unwrap();
        let (_shell, overview, outline, cx) = toggle_shell(cx, dir.path());
        for mode in [
            OverviewMode::Outline,
            OverviewMode::List,
            OverviewMode::Canvas,
        ] {
            set_mode(&overview, mode, cx);
            overview.update(cx, |overview, cx| {
                overview.set_search_text_for_test("old", cx)
            });
            redraw(cx);
            // From the tree / surface...
            cx.update(|window, cx| match mode {
                OverviewMode::Outline => {
                    let handle = gpui::Focusable::focus_handle(outline.read(cx), cx);
                    window.focus(&handle, cx)
                }
                _ => window.blur(),
            });
            redraw(cx);
            assert!(!search_focused(&overview, cx), "{mode:?} precondition");
            cx.simulate_keystrokes(&find_key());
            redraw(cx);
            assert!(search_focused(&overview, cx), "{mode:?}: ⌘F focuses search");
            // ...and its text is selected: typing replaces it.
            cx.simulate_keystrokes("n e w");
            assert_eq!(search_text(&overview, cx), "new", "{mode:?}");
            // Again while already in the field: still selects.
            cx.simulate_keystrokes(&find_key());
            cx.simulate_keystrokes("x");
            assert_eq!(search_text(&overview, cx), "x", "{mode:?}");
            overview.update(cx, |overview, cx| overview.set_search_text_for_test("", cx));
        }
    }

    /// On the full chat route ⌘F keeps doing what it did: nothing — the
    /// composer keeps focus and the Overview search is not touched.
    #[gpui::test]
    fn find_inside_an_open_chat_is_left_alone(cx: &mut gpui::TestAppContext) {
        use crate::work_outline::store::fixture_ids::*;

        let dir = tempfile::tempdir().unwrap();
        let (shell, overview, _outline, cx) = toggle_shell(cx, dir.path());
        overview.update(cx, |overview, cx| {
            overview.set_search_text_for_test("keep", cx)
        });
        shell.update(cx, |shell, cx| shell.open_chat(CHAT_API.to_owned(), cx));
        redraw(cx);
        assert!(route_is_chat(&shell, cx));
        cx.update(|window, cx| {
            let composer = shell.read(cx).composer.focus_handle(cx);
            window.focus(&composer, cx)
        });
        redraw(cx);
        assert!(composer_focused(&shell, cx));
        cx.simulate_keystrokes(&find_key());
        redraw(cx);
        assert!(route_is_chat(&shell, cx));
        assert!(composer_focused(&shell, cx), "composer keeps focus");
        assert!(!search_focused(&overview, cx));
        assert_eq!(search_text(&overview, cx), "keep");
    }

    /// Esc in the search field clears a non-empty query first, then hands
    /// focus to the mode's body.
    #[gpui::test]
    fn escape_in_search_clears_then_returns_focus_to_the_body(cx: &mut gpui::TestAppContext) {
        use crate::overview::OverviewMode;

        let dir = tempfile::tempdir().unwrap();
        let (_shell, overview, outline, cx) = toggle_shell(cx, dir.path());
        for mode in [
            OverviewMode::List,
            OverviewMode::Canvas,
            OverviewMode::Outline,
        ] {
            set_mode(&overview, mode, cx);
            cx.simulate_keystrokes(&find_key());
            cx.simulate_keystrokes("z q");
            assert_eq!(search_text(&overview, cx), "zq", "{mode:?}");
            cx.simulate_keystrokes("escape");
            redraw(cx);
            assert_eq!(search_text(&overview, cx), "", "{mode:?}: Esc clears");
            assert!(
                search_focused(&overview, cx),
                "{mode:?}: stays in the field"
            );
            cx.simulate_keystrokes("escape");
            redraw(cx);
            assert!(!search_focused(&overview, cx), "{mode:?}");
            match mode {
                OverviewMode::Outline => assert!(tree_focused(&outline, cx)),
                _ => assert!(body_focused(&overview, cx), "{mode:?}: body focused"),
            }
        }
    }

    /// Enter and Down from the search select the first matching row (List
    /// and Outline) so the arrows work immediately.
    #[gpui::test]
    fn enter_and_down_from_search_move_to_the_first_row(cx: &mut gpui::TestAppContext) {
        use crate::overview::OverviewMode;

        let dir = tempfile::tempdir().unwrap();
        let (_shell, overview, outline, cx) = toggle_shell(cx, dir.path());

        for key in ["down", "enter"] {
            set_mode(&overview, OverviewMode::List, cx);
            overview.update(cx, |overview, cx| {
                overview.set_list_selection_for_test(None, cx)
            });
            cx.simulate_keystrokes(&find_key());
            cx.simulate_keystrokes(key);
            redraw(cx);
            let (selected, order) = overview.read_with(cx, |o, _| o.list_selection_for_test());
            assert!(order.len() > 1, "fixture shows several chats");
            assert_eq!(selected.as_ref(), order.first(), "List {key}");
            assert!(body_focused(&overview, cx));
            assert!(!search_focused(&overview, cx));
            // Arrows work immediately.
            cx.simulate_keystrokes("down");
            redraw(cx);
            let (selected, _) = overview.read_with(cx, |o, _| o.list_selection_for_test());
            assert_eq!(selected.as_ref(), order.get(1), "List down after {key}");
            cx.simulate_keystrokes("up up");
            redraw(cx);
            assert!(
                search_focused(&overview, cx),
                "Up from the first row returns"
            );
        }

        for key in ["down", "enter"] {
            set_mode(&overview, OverviewMode::Outline, cx);
            outline.update(cx, |outline, cx| {
                let last = outline.model().rows.last().unwrap().key.clone();
                outline.select(&last, cx)
            });
            cx.simulate_keystrokes(&find_key());
            cx.simulate_keystrokes(key);
            redraw(cx);
            assert!(tree_focused(&outline, cx), "Outline {key}");
            let first = outline.read_with(cx, |o, _| o.model().rows[0].key.clone());
            assert_eq!(
                outline_selection(&outline, cx),
                Some(first),
                "Outline {key}"
            );
            let second = outline.read_with(cx, |o, _| o.model().rows[1].key.clone());
            cx.simulate_keystrokes("down");
            redraw(cx);
            assert_eq!(
                outline_selection(&outline, cx),
                Some(second),
                "arrow after {key}"
            );
        }
    }

    /// Enter on a selected List row opens the chat beside the Overview.
    #[gpui::test]
    fn enter_on_a_selected_list_row_opens_the_chat_panel(cx: &mut gpui::TestAppContext) {
        use crate::overview::OverviewMode;

        let dir = tempfile::tempdir().unwrap();
        let (_shell, overview, _outline, cx) = toggle_shell(cx, dir.path());
        set_mode(&overview, OverviewMode::List, cx);
        cx.simulate_keystrokes(&find_key());
        cx.simulate_keystrokes("down");
        redraw(cx);
        let (selected, _) = overview.read_with(cx, |o, _| o.list_selection_for_test());
        cx.simulate_keystrokes("enter");
        redraw(cx);
        assert_eq!(panel_chat(&overview, cx), selected);
        assert!(panel_chat(&overview, cx).is_some());
    }

    /// The Toggle Linear Panel key as the app binds it on this platform.
    fn linear_key() -> String {
        platform_combo(ShortcutId::OpenLinear.default_combo())
    }

    /// Turn Linear on (or off) for the fixture's Space, with scripted
    /// Linear replies (nothing reaches Linear).
    fn set_linear(
        shell: &Entity<Shell>,
        overview: &Entity<crate::overview::Overview>,
        enabled: bool,
        cx: &mut gpui::VisualTestContext,
    ) {
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.work_outline_actions_override =
                    Some(crate::work_outline::WorkOutlineActions::scripted(
                        std::sync::Arc::new(|method, _| {
                            Ok(match method {
                                "LinearMyIssues" | "LinearMyProjects" => serde_json::json!([]),
                                _ => serde_json::json!({}),
                            })
                        }),
                    ));
            })
        });
        let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
        snapshot.space_settings.linear_enabled = enabled;
        overview.update(cx, |overview, cx| {
            overview.set_outline_loads(
                std::collections::HashMap::from([(
                    "s1".to_string(),
                    crate::work_outline::store::WorkOutlineLoad::Ready(std::sync::Arc::new(
                        snapshot,
                    )),
                )]),
                cx,
            )
        });
        redraw(cx);
    }

    fn linear_search_focused(
        overview: &Entity<crate::overview::Overview>,
        cx: &mut gpui::VisualTestContext,
    ) -> bool {
        cx.update(|window, cx| {
            overview
                .read(cx)
                .linear_panel()
                .is_some_and(|panel| panel.read(cx).search_focused(window, cx))
        })
    }

    /// The Linear toggle through the real keymap: from the chat panel's
    /// focused composer it opens the panel beside the chat panel and focuses
    /// its search (the draft untouched); from the composer again it only
    /// moves focus back to the search; from the search it closes the panel
    /// and returns focus to the tree. The chat panel stays up throughout.
    #[gpui::test]
    fn linear_key_toggles_the_panel_beside_the_chat_panel(cx: &mut gpui::TestAppContext) {
        use crate::work_outline::model::row_key;
        use crate::work_outline::store::fixture_ids::*;

        let dir = tempfile::tempdir().unwrap();
        let (shell, overview, outline, cx) = toggle_shell(cx, dir.path());
        cx.update(|_, cx| crate::motion::set_reduced_motion(cx, true));
        cx.simulate_resize(gpui::size(px(1900.0), px(900.0)));
        set_linear(&shell, &overview, true, cx);

        // Enter on a Chat row: the chat panel opens, composer focused.
        outline.update(cx, |outline, cx| {
            outline.select(&row_key::chat(CHAT_AUTH), cx)
        });
        cx.update(|window, cx| {
            let handle = gpui::Focusable::focus_handle(outline.read(cx), cx);
            window.focus(&handle, cx)
        });
        redraw(cx);
        cx.simulate_keystrokes("enter");
        redraw(cx);
        assert!(composer_focused(&shell, cx));
        cx.simulate_input("draft");
        redraw(cx);

        // Composer -> open + search focused; the chat panel stays.
        cx.simulate_keystrokes(&linear_key());
        redraw(cx);
        assert!(overview.read_with(cx, |o, _| o.linear_panel().is_some()));
        assert!(linear_search_focused(&overview, cx), "the search has focus");
        assert!(cx.debug_bounds("linear-panel").is_some());
        assert!(
            cx.debug_bounds("overview-chat-panel").is_some(),
            "both open"
        );
        assert_eq!(
            overview.read_with(cx, |o, cx| o.shown_panel_chat(cx)),
            Some(CHAT_AUTH.to_owned())
        );
        let composer = shell.read_with(cx, |shell, _| shell.composer.clone());
        assert_eq!(composer.read_with(cx, |c, cx| c.draft_text(cx)), "draft");

        // The chat composer is still focusable beside the panel, and the
        // key from there goes back to the search without closing.
        cx.update(|window, cx| window.focus(&composer.focus_handle(cx), cx));
        redraw(cx);
        assert!(composer_focused(&shell, cx));
        cx.simulate_keystrokes(&linear_key());
        redraw(cx);
        assert!(overview.read_with(cx, |o, _| o.linear_panel().is_some()));
        assert!(linear_search_focused(&overview, cx));
        assert_eq!(composer.read_with(cx, |c, cx| c.draft_text(cx)), "draft");

        // Search -> closed, focus back on the tree.
        cx.simulate_keystrokes(&linear_key());
        redraw(cx);
        assert!(overview.read_with(cx, |o, _| o.linear_panel().is_none()));
        assert!(cx.debug_bounds("linear-panel").is_none());
        assert!(tree_focused(&outline, cx), "the tree has focus again");
        assert!(cx.debug_bounds("overview-chat-panel").is_some());

        // From the tree it opens again.
        cx.simulate_keystrokes(&linear_key());
        redraw(cx);
        assert!(linear_search_focused(&overview, cx));
    }

    /// No Space with Linear on: the key does nothing but explain, on the
    /// Overview and off it (where it does not even switch route).
    #[gpui::test]
    fn linear_key_is_a_no_op_while_linear_is_off(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, overview, outline, cx) = toggle_shell(cx, dir.path());
        set_linear(&shell, &overview, false, cx);
        cx.simulate_keystrokes(&linear_key());
        redraw(cx);
        assert!(overview.read_with(cx, |o, _| o.linear_panel().is_none()));
        assert_eq!(
            outline.read_with(cx, |o, _| o.notice().map(str::to_owned)),
            Some(crate::linear::LINEAR_OFF_NOTICE.to_owned())
        );
        shell.update(cx, |shell, cx| {
            shell.open_settings(SettingsSection::Linear, cx)
        });
        redraw(cx);
        cx.simulate_keystrokes(&linear_key());
        redraw(cx);
        assert!(shell.read_with(cx, |shell, _| matches!(shell.route, Route::Settings(_))));
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.sidebar_notice.clone()),
            Some(crate::linear::LINEAR_OFF_NOTICE.into())
        );
    }
}
