//! The Outline presentation: a virtualized (`uniform_list`) compact tree of
//! Space roots, work items, and their Chats, with inline create/rename, a
//! move/associate picker, and context-scoped keyboard navigation.
//!
//! Keyboard: the tree element carries the [`KEY_CONTEXT`] key context only
//! while no inline editor or picker is open, so bare-key bindings never fire
//! while a text field owns focus. The actions below are bound by
//! `Shell::apply_keymap` through [`key_bindings`] (re-applied after every
//! keymap change because `apply_keymap` clears all bindings); handlers also
//! early-return while editing as a second guard.
//!
//! The view never updates the `Overview` synchronously (the Overview calls
//! into it); it reports opens through [`OutlineEvent`].
use std::collections::HashSet;
use std::ops::Range;
use std::time::Duration;

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding,
    ScrollStrategy, SharedString, Subscription, UniformListScrollHandle, WeakEntity, Window,
    actions, div, prelude::*, px,
};

use super::actions::new_operation_id;
use super::chips::{self, TextTooltip};
use super::command::WorkOutlineDriftRequest;
use super::instructions::{self, OutlineChange};
use super::model::{
    self, ChatRow, ItemRow, NoticeTone, OutlineChat, OutlineModel, OutlineOptions, OutlineRow,
    OutlineRowKind, OutlineSpace, row_key,
};
use super::nav::{self, NavCommand};
use super::restructure::{self, DropZone, MovePlan, RestructureOp};
use super::store::{WorkOutlinePlacement, WorkOutlineSnapshot};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::overview_positions;
use crate::shell::Shell;
use crate::state::AppState;
use crate::theme::Theme;

/// Key context of the Outline tree (bind outline keys under it).
pub(crate) const KEY_CONTEXT: &str = "WorkOutline";
/// Key context of the picker list.
pub(crate) const PICKER_KEY_CONTEXT: &str = "WorkOutlinePicker";

actions!(
    work_outline,
    [
        SelectNext,
        SelectPrevious,
        SelectFirst,
        SelectLast,
        SelectParent,
        SelectChild,
        SelectNextSibling,
        SelectPreviousSibling,
        Expand,
        Collapse,
        OpenSelected,
        RenameSelected,
        CreateSibling,
        CreateChild,
        MoveItemUp,
        MoveItemDown,
        IndentItem,
        OutdentItem,
    ]
);

/// Default outline navigation bindings, all scoped to [`KEY_CONTEXT`] (and
/// excluded from text-input contexts). `Shell::apply_keymap` registers these.
pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    // Mirrors `shell::work_outline_ui::OUTLINE_NAVIGATION_KEYS` (the Shell
    // binds those in `apply_keymap`); used by fixtures and tests that run
    // without a Shell.
    let ctx = Some("WorkOutline && !Composer && !MessageComposer && !PaletteSearch");
    vec![
        KeyBinding::new("up", SelectPreviousSibling, ctx),
        KeyBinding::new("down", SelectNextSibling, ctx),
        KeyBinding::new("alt-up", SelectParent, ctx),
        KeyBinding::new("alt-down", SelectChild, ctx),
        KeyBinding::new("left", Collapse, ctx),
        KeyBinding::new("right", Expand, ctx),
        KeyBinding::new("enter", OpenSelected, ctx),
        KeyBinding::new("f2", RenameSelected, ctx),
        KeyBinding::new("n", CreateSibling, ctx),
        KeyBinding::new("shift-n", CreateChild, ctx),
        KeyBinding::new("home", SelectFirst, ctx),
        KeyBinding::new("end", SelectLast, ctx),
        KeyBinding::new("alt-shift-up", MoveItemUp, ctx),
        KeyBinding::new("alt-shift-down", MoveItemDown, ctx),
        KeyBinding::new("tab", IndentItem, ctx),
        KeyBinding::new("shift-tab", OutdentItem, ctx),
    ]
}

/// What the Overview should do in response to the Outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutlineEvent {
    /// Open the Chat's transcript.
    OpenChat(String),
    /// Open Work Item Details.
    OpenDetails { space_id: String, item_id: String },
    /// Open the Space-root details (Space-root instructions).
    OpenSpaceRoot { space_id: String },
    /// Start a Chat in the work item (Milestone 3).
    StartChat { space_id: String, item_id: String },
    /// Review proposed instructions (Milestone 4); `item_id` empty = the
    /// Space root.
    OpenProposals { space_id: String, item_id: String },
    /// Open the Required Actions surface (Milestone 4).
    OpenRequiredActions,
}

/// What the Overview feeds the Outline each time its inputs change.
#[derive(Debug, Clone, Default)]
pub(crate) struct OutlineInputs {
    pub spaces: Vec<OutlineSpace>,
    pub chats: Vec<OutlineChat>,
    pub options: OutlineOptions,
}

/// Where an inline "new work item" row sits, re-resolved every frame so a
/// refresh while typing cannot misplace it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CreateAnchor {
    /// After this row's subtree, at its depth (a new sibling).
    After(String),
    /// First child of this row.
    FirstChildOf(String),
    /// Top of the outline.
    Top,
}

/// `(list index, depth)` of the inline create row.
pub(crate) fn create_slot(model: &OutlineModel, anchor: &CreateAnchor) -> (usize, u16) {
    match anchor {
        CreateAnchor::After(key) => match model.position(key) {
            Some(ix) => {
                let depth = model.rows[ix].depth;
                let mut end = ix + 1;
                while end < model.len() && model.rows[end].depth > depth {
                    end += 1;
                }
                (end, depth)
            }
            None => (0, 0),
        },
        CreateAnchor::FirstChildOf(key) => match model.position(key) {
            Some(ix) => (ix + 1, model.rows[ix].depth + 1),
            None => (0, 0),
        },
        CreateAnchor::Top => (0, 0),
    }
}

#[derive(Debug, Clone)]
enum EditKind {
    Create {
        space_id: String,
        parent: Option<String>,
        placement: WorkOutlinePlacement,
        anchor: CreateAnchor,
    },
    Rename {
        item_id: String,
        key: String,
    },
}

#[derive(Debug, Clone)]
struct InlineEdit {
    kind: EditKind,
    /// Reused across retries of the same edit (idempotent replay).
    operation_id: String,
    submitting: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PickerPurpose {
    /// Move `item_id`; entry value = destination parent (`None` = top).
    Move { item_id: String },
    /// Associate `chat_id`; entry value = work item.
    AssociateChat { chat_id: String },
    /// Associate a Chat with `item_id`; entry value = chat id.
    AssociateWithItem { item_id: String },
}

#[derive(Debug, Clone)]
struct PickerEntry {
    label: String,
    depth: u32,
    value: Option<String>,
}

struct OutlinePicker {
    title: String,
    entries: Vec<PickerEntry>,
    active: usize,
    purpose: PickerPurpose,
    operation_id: String,
    submitting: bool,
    /// Move picker only: place the item first ("Top") instead of last
    /// ("Bottom") under the chosen parent.
    place_first: bool,
}

/// A direct restructuring mutation (keyboard, palette, drag and drop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DirectMutation {
    Move(MovePlan),
    AssignChat {
        chat_id: String,
        work_item_id: String,
    },
}

impl DirectMutation {
    fn change(&self) -> OutlineChange {
        match self {
            Self::Move(plan) => OutlineChange::MoveItem {
                work_item_id: plan.work_item_id.clone(),
                new_parent: plan.destination_parent.clone(),
            },
            Self::AssignChat {
                chat_id,
                work_item_id,
            } => OutlineChange::AssignChat {
                chat_id: chat_id.clone(),
                work_item_id: work_item_id.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptState {
    /// Sent; no reply yet. Further restructuring waits.
    InFlight,
    /// Acknowledged; waiting for the snapshot that shows it, so the next
    /// press plans from the new order rather than a stale one.
    AwaitingRefresh,
    /// Failed: retrying the same mutation reuses `operation_id`.
    Failed,
}

#[derive(Debug, Clone)]
struct MutationAttempt {
    mutation: DirectMutation,
    space_id: String,
    operation_id: String,
    state: AttemptState,
    /// Address of the Space's snapshot when sent (refresh detection).
    sent_snapshot: usize,
}

/// How long an acknowledged restructuring blocks the next one when no new
/// snapshot arrives (a host that coalesces the frame).
const AWAIT_REFRESH: Duration = Duration::from_secs(2);

/// What an in-progress drag over the Outline would do (drawn as the drop
/// indicator; absent for refused targets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DropHint {
    /// Row key of the target work item.
    pub key: String,
    pub zone: DropZone,
}

/// The drag payload of an Outline row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutlineDrag {
    Item {
        space_id: String,
        item_id: String,
        title: String,
    },
    Chat {
        chat_id: String,
        space_id: String,
        title: String,
    },
}

impl OutlineDrag {
    fn title(&self) -> &str {
        match self {
            Self::Item { title, .. } | Self::Chat { title, .. } => title,
        }
    }
}

/// The ghost that follows the pointer while dragging a row.
struct OutlineDragGhost {
    title: SharedString,
}

impl Render for OutlineDragGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .max_w(px(260.0))
            .px(px(8.0))
            .py(px(3.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.surface_raised)
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text)
            .truncate()
            .child(self.title.clone())
    }
}

pub(crate) struct OutlineView {
    state: Entity<AppState>,
    focus: FocusHandle,
    picker_focus: FocusHandle,
    scroll: UniformListScrollHandle,
    inputs: OutlineInputs,
    model: OutlineModel,
    collapsed: HashSet<String>,
    collapsed_loaded: bool,
    selected: Option<String>,
    /// Select this key as soon as a refresh brings it in (a just-created item).
    pending_select: Option<String>,
    editing: Option<InlineEdit>,
    editor: Entity<ComposerInput>,
    picker: Option<OutlinePicker>,
    notice: Option<String>,
    notice_seq: u64,
    /// Item whose Work Item Details are open (row highlight).
    details_key: Option<String>,
    /// Chat shown in the Overview's chat panel (row highlight).
    panel_chat: Option<String>,
    /// Owner of the drift confirmation and the revision comparison.
    shell: Option<WeakEntity<Shell>>,
    /// The latest direct restructuring (keyboard, palette, drop).
    attempt: Option<MutationAttempt>,
    /// Reveal this row once a refresh moves it (selection follows a move).
    follow: Option<String>,
    /// The drop indicator of an in-progress drag.
    drop_hint: Option<DropHint>,
    _editor_events: Subscription,
    /// Tests run without a Shell: the drift confirmation lands here.
    #[cfg(test)]
    pub(crate) captured_drift: Option<WorkOutlineDriftRequest>,
}

impl EventEmitter<OutlineEvent> for OutlineView {}

impl Focusable for OutlineView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl OutlineView {
    pub(crate) fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            ComposerInput::with_context(
                "Work item title",
                crate::composer::PALETTE_SEARCH_CONTEXT,
                cx,
            )
            .with_single_line()
            .with_text_metrics(12.5, 18.0)
            .with_accessibility_role(gpui::Role::TextInput)
        });
        let editor_events = cx.subscribe(&editor, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.commit_edit(cx);
            }
        });
        Self {
            state,
            focus: cx.focus_handle(),
            picker_focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            inputs: OutlineInputs::default(),
            model: OutlineModel::default(),
            collapsed: HashSet::new(),
            collapsed_loaded: false,
            selected: None,
            pending_select: None,
            editing: None,
            editor,
            picker: None,
            notice: None,
            notice_seq: 0,
            details_key: None,
            panel_chat: None,
            shell: None,
            attempt: None,
            follow: None,
            drop_hint: None,
            _editor_events: editor_events,
            #[cfg(test)]
            captured_drift: None,
        }
    }

    /// The Shell that owns the drift confirmation and revision comparison.
    pub(crate) fn with_shell(mut self, shell: WeakEntity<Shell>) -> Self {
        self.shell = Some(shell);
        self
    }

    // ----------------------------------------------------------------------
    // Inputs and model
    // ----------------------------------------------------------------------

    pub(crate) fn set_inputs(&mut self, inputs: OutlineInputs, cx: &mut Context<Self>) {
        self.ensure_collapsed_loaded(cx);
        self.inputs = inputs;
        let refreshed = self.attempt.as_ref().is_some_and(|attempt| {
            attempt.state == AttemptState::AwaitingRefresh
                && self.snapshot_address(&attempt.space_id) != attempt.sent_snapshot
        });
        if refreshed {
            self.attempt = None;
        }
        self.reflatten();
        if refreshed {
            // The frame showing the move has been applied (and revealed).
            self.follow = None;
        }
        cx.notify();
    }

    /// Identity of a Space's current snapshot (0 = none loaded).
    fn snapshot_address(&self, space_id: &str) -> usize {
        self.inputs
            .spaces
            .iter()
            .find(|space| space.id == space_id)
            .and_then(|space| space.load.snapshot())
            .map_or(0, |snapshot| std::sync::Arc::as_ptr(snapshot) as usize)
    }

    pub(crate) fn set_highlights(
        &mut self,
        details_key: Option<String>,
        panel_chat: Option<String>,
    ) {
        self.details_key = details_key;
        self.panel_chat = panel_chat;
    }

    pub(crate) fn model(&self) -> &OutlineModel {
        &self.model
    }

    pub(crate) fn hidden_completed(&self) -> usize {
        self.model.hidden_completed
    }

    fn ensure_collapsed_loaded(&mut self, cx: &App) {
        if self.collapsed_loaded {
            return;
        }
        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
            return;
        };
        self.collapsed_loaded = true;
        self.collapsed.extend(overview_positions::load_string_set(
            &data_dir,
            overview_positions::OUTLINE_COLLAPSED_FILE,
        ));
    }

    fn reflatten(&mut self) {
        let previous = std::mem::take(&mut self.model);
        self.model = model::flatten(
            &self.inputs.spaces,
            &self.inputs.chats,
            &self.collapsed,
            &self.inputs.options,
        );
        if let Some(follow) = self.follow.clone() {
            let place =
                |model: &OutlineModel| model.position(&follow).map(|ix| (ix, model.rows[ix].depth));
            let before = place(&previous);
            if let Some((after, depth)) = place(&self.model) {
                if before != Some((after, depth)) {
                    // The moved item's new place: select and reveal it.
                    self.selected = Some(follow);
                    self.follow = None;
                    self.scroll.scroll_to_item(after, ScrollStrategy::Nearest);
                    return;
                }
            }
        }
        if let Some(pending) = self.pending_select.clone() {
            if self.model.position(&pending).is_some() {
                self.selected = Some(pending);
                self.pending_select = None;
                return;
            }
        }
        if let Some(selected) = self.selected.clone() {
            self.selected = nav::reconcile_selection(&previous, &self.model, &selected);
        }
    }

    fn set_collapsed(&mut self, key: &str, collapsed: bool, cx: &mut Context<Self>) {
        let changed = if collapsed {
            self.collapsed.insert(key.to_string())
        } else {
            self.collapsed.remove(key)
        };
        if !changed {
            return;
        }
        if let Some(data_dir) = self.state.read(cx).data_dir.clone() {
            let set: std::collections::BTreeSet<String> = self.collapsed.iter().cloned().collect();
            if let Err(err) = overview_positions::save_string_set(
                &data_dir,
                overview_positions::OUTLINE_COLLAPSED_FILE,
                &set,
            ) {
                tracing::warn!(error = %err, "saving outline collapse state failed");
            }
        }
        self.reflatten();
        cx.notify();
    }

    fn snapshot(&self, space_id: &str) -> Option<&WorkOutlineSnapshot> {
        self.inputs
            .spaces
            .iter()
            .find(|space| space.id == space_id)
            .and_then(|space| space.load.snapshot())
            .map(|snapshot| snapshot.as_ref())
    }

    /// The loaded snapshot that contains `item_id`.
    fn snapshot_with_item(&self, item_id: &str) -> Option<&WorkOutlineSnapshot> {
        self.inputs
            .spaces
            .iter()
            .filter_map(|space| space.load.snapshot())
            .map(|snapshot| snapshot.as_ref())
            .find(|snapshot| snapshot.item(item_id).is_some())
    }

    // ----------------------------------------------------------------------
    // Selection
    // ----------------------------------------------------------------------

    pub(crate) fn selected_key(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    pub(crate) fn selected_row(&self) -> Option<&OutlineRow> {
        self.selected.as_deref().and_then(|key| self.model.row(key))
    }

    /// `(space id, item id)` of the selected work item.
    pub(crate) fn selected_item(&self) -> Option<(String, String)> {
        self.selected_row()
            .and_then(OutlineRow::item)
            .map(|item| (item.space_id.clone(), item.item_id.clone()))
    }

    pub(crate) fn selected_chat(&self) -> Option<String> {
        self.selected_row()
            .and_then(OutlineRow::chat)
            .filter(|chat| !chat.dangling)
            .map(|chat| chat.chat.id.clone())
    }

    pub(crate) fn select(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(ix) = self.model.position(key) {
            self.selected = Some(key.to_string());
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    pub(crate) fn is_editing(&self) -> bool {
        self.editing.is_some() || self.picker.is_some()
    }

    fn navigate(&mut self, command: NavCommand, cx: &mut Context<Self>) {
        if self.is_editing() {
            cx.propagate();
            return;
        }
        let result = nav::navigate(&self.model, self.selected.as_deref(), command);
        if let Some((key, collapsed)) = result.set_collapsed {
            self.set_collapsed(&key, collapsed, cx);
        }
        if let Some(key) = result.select {
            self.select(&key, cx);
        }
    }

    fn open_selected(&mut self, cx: &mut Context<Self>) {
        if self.is_editing() {
            cx.propagate();
            return;
        }
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        self.open_row(&row, cx);
    }

    fn open_row(&mut self, row: &OutlineRow, cx: &mut Context<Self>) {
        match &row.kind {
            OutlineRowKind::Item(item) => cx.emit(OutlineEvent::OpenDetails {
                space_id: item.space_id.clone(),
                item_id: item.item_id.clone(),
            }),
            OutlineRowKind::Chat(chat) if !chat.dangling => {
                cx.emit(OutlineEvent::OpenChat(chat.chat.id.clone()))
            }
            OutlineRowKind::Chat(_) => {
                self.show_notice("This chat no longer exists on any device", cx)
            }
            OutlineRowKind::Space { .. } | OutlineRowKind::Unassociated { .. } => {
                if row.has_children {
                    let collapse = !row.collapsed;
                    self.set_collapsed(&row.key, collapse, cx);
                }
            }
            OutlineRowKind::Notice { .. } => {}
        }
    }

    // ----------------------------------------------------------------------
    // Notice
    // ----------------------------------------------------------------------

    pub(crate) fn show_notice(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.notice = Some(text.into());
        self.notice_seq = self.notice_seq.wrapping_add(1);
        let seq = self.notice_seq;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(5)).await;
            this.update(cx, |this, cx| {
                if this.notice_seq == seq {
                    this.notice = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    // ----------------------------------------------------------------------
    // Inline create / rename
    // ----------------------------------------------------------------------

    /// The Space a new root item goes to: the selected row's, else the
    /// app's selected Space, else the only/first Space shown.
    fn target_space(&self, cx: &App) -> Option<String> {
        let shown = |id: &str| self.inputs.spaces.iter().any(|space| space.id == id);
        if let Some(space) = self.selected_row().and_then(OutlineRow::space_id) {
            if shown(space) {
                return Some(space.to_string());
            }
        }
        if let Some(space) = self.state.read(cx).selected_space.as_deref() {
            if shown(space) {
                return Some(space.to_string());
            }
        }
        self.inputs.spaces.first().map(|space| space.id.clone())
    }

    /// Start an inline create. `child`: under the selected item (required);
    /// otherwise a sibling after the selected item, or a top-level item in
    /// the target Space. `Err` carries the notice to show.
    pub(crate) fn begin_create(
        &mut self,
        child: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        self.cancel_edit(cx);
        let selected_item = self.selected_row().and_then(OutlineRow::item).cloned();
        let selected_key = self.selected.clone();
        let (space_id, parent, placement, anchor) = if child {
            let item = selected_item.ok_or("Select a work item first")?;
            let key = selected_key.unwrap_or_default();
            if self.model.row(&key).is_some_and(|row| row.collapsed) {
                self.set_collapsed(&key, false, cx);
            }
            (
                item.space_id.clone(),
                Some(item.item_id.clone()),
                WorkOutlinePlacement::First,
                CreateAnchor::FirstChildOf(key),
            )
        } else if let Some(item) = selected_item {
            let parent = self
                .snapshot(&item.space_id)
                .and_then(|snapshot| snapshot.item(&item.item_id))
                .and_then(|view| view.parent_work_item_id.clone());
            (
                item.space_id.clone(),
                parent,
                WorkOutlinePlacement::After {
                    sibling_work_item_id: item.item_id.clone(),
                },
                CreateAnchor::After(selected_key.unwrap_or_default()),
            )
        } else {
            let space_id = self
                .target_space(cx)
                .ok_or("Add a project to create work items")?;
            if self
                .snapshot(&space_id)
                .is_none_or(|snapshot| !snapshot.available)
            {
                return Err("This project's outline is not available yet");
            }
            let anchor = if self.model.multi_space {
                let key = row_key::space(&space_id);
                if self.model.row(&key).is_some_and(|row| row.collapsed) {
                    self.set_collapsed(&key, false, cx);
                }
                CreateAnchor::FirstChildOf(key)
            } else {
                CreateAnchor::Top
            };
            (space_id, None, WorkOutlinePlacement::First, anchor)
        };
        self.editing = Some(InlineEdit {
            kind: EditKind::Create {
                space_id,
                parent,
                placement,
                anchor,
            },
            operation_id: new_operation_id(),
            submitting: false,
        });
        self.start_editor("", window, cx);
        Ok(())
    }

    pub(crate) fn begin_rename(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        self.cancel_edit(cx);
        let row = self
            .selected_row()
            .cloned()
            .ok_or("Select a work item first")?;
        let item = row.item().ok_or("Select a work item first")?.clone();
        self.editing = Some(InlineEdit {
            kind: EditKind::Rename {
                item_id: item.item_id.clone(),
                key: row.key.clone(),
            },
            operation_id: new_operation_id(),
            submitting: false,
        });
        self.start_editor(&item.title, window, cx);
        Ok(())
    }

    fn start_editor(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |input, cx| input.set_text(text, cx));
        let handle = self.editor.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        if !text.is_empty() {
            window.dispatch_action(Box::new(crate::composer::SelectAll), cx);
        }
        cx.notify();
    }

    pub(crate) fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            cx.notify();
        }
    }

    fn cancel_edit_and_refocus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_edit(cx);
        window.focus(&self.focus, cx);
    }

    fn commit_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.editing.as_mut() else {
            return;
        };
        if edit.submitting {
            return;
        }
        let title = self.editor.read(cx).text().trim().to_string();
        if title.is_empty() {
            self.editing = None;
            cx.notify();
            return;
        }
        let title: String = title
            .chars()
            .take(zeron_proto::limits::MAX_TITLE_CHARS)
            .collect();
        edit.submitting = true;
        let actions = self.state.read(cx).work_outline_actions();
        let operation_id = edit.operation_id.clone();
        let (task, created_space) = match &edit.kind {
            EditKind::Create {
                space_id,
                parent,
                placement,
                ..
            } => (
                actions.create_work_item(zeron_proto::CreateWorkItemParams {
                    space_id: space_id.clone(),
                    parent_work_item_id: parent.clone(),
                    placement: placement.clone(),
                    title,
                    description: None,
                    status: None,
                    operation_id,
                }),
                Some(space_id.clone()),
            ),
            EditKind::Rename { item_id, .. } => (
                actions.update_work_item(zeron_proto::UpdateWorkItemParams {
                    work_item_id: item_id.clone(),
                    changes: zeron_proto::WorkItemChanges {
                        title: Some(title),
                        ..Default::default()
                    },
                    operation_id,
                }),
                None,
            ),
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                Ok(reply) => {
                    this.editing = None;
                    if let (Some(space_id), Some(item_id)) = (created_space, reply.work_item_id) {
                        let key = row_key::item(&space_id, &item_id);
                        if this.model.position(&key).is_some() {
                            this.select(&key, cx);
                        } else {
                            this.pending_select = Some(key);
                        }
                    }
                    cx.notify();
                }
                Err(error) => {
                    // Keep the editor (and its operation id) for a retry.
                    if let Some(edit) = this.editing.as_mut() {
                        edit.submitting = false;
                    }
                    this.show_notice(error.message.clone(), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    // ----------------------------------------------------------------------
    // Pickers
    // ----------------------------------------------------------------------

    /// "Move Work Item": valid destinations (Space top level and every live
    /// item except the selected one and its descendants).
    pub(crate) fn open_move_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let (space_id, item_id) = self.selected_item().ok_or("Select a work item first")?;
        let snapshot = self
            .snapshot(&space_id)
            .ok_or("This project's outline is not loaded")?;
        let entries = model::destinations(snapshot, Some(&item_id))
            .into_iter()
            .map(|dest| PickerEntry {
                label: dest.title,
                depth: dest.depth,
                value: dest.item_id,
            })
            .collect();
        let title = snapshot
            .item(&item_id)
            .map(|item| format!("Move “{}” to…", item.title))
            .unwrap_or_else(|| "Move to…".to_string());
        self.open_picker(title, entries, PickerPurpose::Move { item_id }, window, cx);
        Ok(())
    }

    /// "Associate Chat with Work Item". With a Chat (`chat_id`, or the
    /// selected Chat row): pick a work item of its Space. With a selected
    /// work item instead: pick a Chat of that Space.
    pub(crate) fn open_associate_picker(
        &mut self,
        chat_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let selected_chat = self.selected_chat();
        let target_chat =
            selected_chat.or_else(|| chat_id.filter(|_| self.selected_item().is_none()));
        if let Some(chat_id) = target_chat {
            let chat = self
                .inputs
                .chats
                .iter()
                .find(|chat| chat.id == chat_id)
                .cloned()
                .ok_or("That chat is not shown in the Overview")?;
            let space_id = chat
                .space_id
                .clone()
                .ok_or("Chats without a project cannot be associated")?;
            let snapshot = self
                .snapshot(&space_id)
                .ok_or("This project's outline is not loaded")?;
            let entries: Vec<PickerEntry> = model::destinations(snapshot, None)
                .into_iter()
                .map(|dest| PickerEntry {
                    label: dest.title,
                    depth: dest.depth,
                    value: dest.item_id,
                })
                .collect();
            if entries.is_empty() {
                return Err("This project has no work items yet");
            }
            self.open_picker(
                format!("Associate “{}” with…", chat.title),
                entries,
                PickerPurpose::AssociateChat { chat_id },
                window,
                cx,
            );
            return Ok(());
        }
        let (space_id, item_id) = self
            .selected_item()
            .ok_or("Select a chat or a work item first")?;
        let associated: HashSet<&str> = self
            .snapshot(&space_id)
            .map(|snapshot| {
                snapshot
                    .items
                    .iter()
                    .flat_map(|item| item.chat_ids.iter().map(String::as_str))
                    .collect()
            })
            .unwrap_or_default();
        let mut chats: Vec<&OutlineChat> = self
            .inputs
            .chats
            .iter()
            .filter(|chat| {
                chat.visible
                    && chat.space_id.as_deref() == Some(space_id.as_str())
                    && !associated.contains(chat.id.as_str())
            })
            .collect();
        chats.sort_by(|a, b| b.activity.cmp(&a.activity));
        if chats.is_empty() {
            return Err("No unassociated chats in this project");
        }
        let entries = chats
            .into_iter()
            .map(|chat| PickerEntry {
                label: chat.title.clone(),
                depth: 0,
                value: Some(chat.id.clone()),
            })
            .collect();
        self.open_picker(
            "Associate a chat…".to_string(),
            entries,
            PickerPurpose::AssociateWithItem { item_id },
            window,
            cx,
        );
        Ok(())
    }

    fn open_picker(
        &mut self,
        title: String,
        entries: Vec<PickerEntry>,
        purpose: PickerPurpose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel_edit(cx);
        self.picker = Some(OutlinePicker {
            title,
            entries,
            active: 0,
            purpose,
            operation_id: new_operation_id(),
            submitting: false,
            place_first: false,
        });
        window.focus(&self.picker_focus, cx);
        cx.notify();
    }

    pub(crate) fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    pub(crate) fn choose_picker_entry(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        if picker.submitting {
            return;
        }
        let Some(entry) = picker.entries.get(ix).cloned() else {
            return;
        };
        let purpose = picker.purpose.clone();
        let change = match &purpose {
            PickerPurpose::Move { item_id } => Some(OutlineChange::MoveItem {
                work_item_id: item_id.clone(),
                new_parent: entry.value.clone(),
            }),
            PickerPurpose::AssociateChat { chat_id } => {
                entry.value.clone().map(|item| OutlineChange::AssignChat {
                    chat_id: chat_id.clone(),
                    work_item_id: item,
                })
            }
            PickerPurpose::AssociateWithItem { item_id } => {
                entry.value.clone().map(|chat| OutlineChange::AssignChat {
                    chat_id: chat,
                    work_item_id: item_id.clone(),
                })
            }
        };
        let snapshot = match &purpose {
            PickerPurpose::Move { item_id } | PickerPurpose::AssociateWithItem { item_id } => {
                self.snapshot_with_item(item_id)
            }
            PickerPurpose::AssociateChat { chat_id } => self
                .inputs
                .chats
                .iter()
                .find(|chat| &chat.id == chat_id)
                .and_then(|chat| chat.space_id.as_deref())
                .and_then(|space| self.snapshot(space)),
        };
        let impact = change
            .as_ref()
            .zip(snapshot)
            .map(|(change, snapshot)| instructions::drift_impact(snapshot, change))
            .unwrap_or_default();
        if let (true, Some(change)) = (impact.needs_confirmation(), change) {
            let weak = cx.entity().downgrade();
            let request = WorkOutlineDriftRequest {
                title: change.title().into(),
                confirm_label: change.confirm_label().into(),
                impact,
                on_decision: Box::new(move |confirmed, window, cx| {
                    if confirmed {
                        weak.update(cx, |this, cx| this.send_picker_entry(ix, window, cx))
                            .ok();
                    }
                }),
            };
            self.request_drift_confirm(request, cx);
            return;
        }
        self.send_picker_entry(ix, window, cx);
    }

    /// Hand a drift confirmation to the Shell (tests: capture it).
    fn request_drift_confirm(&mut self, request: WorkOutlineDriftRequest, cx: &mut Context<Self>) {
        if let Some(shell) = self.shell.as_ref().and_then(WeakEntity::upgrade) {
            shell.update(cx, |shell, cx| {
                shell.open_work_outline_drift_confirm(request, cx)
            });
            return;
        }
        #[cfg(test)]
        {
            self.captured_drift = Some(request);
        }
        #[cfg(not(test))]
        {
            drop(request);
            self.show_notice("This change needs the app window to confirm", cx);
        }
    }

    fn send_picker_entry(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        if picker.submitting {
            return;
        }
        let Some(entry) = picker.entries.get(ix).cloned() else {
            return;
        };
        let actions = self.state.read(cx).work_outline_actions();
        let operation_id = picker.operation_id.clone();
        let task = match picker.purpose.clone() {
            PickerPurpose::Move { item_id } => {
                actions.move_work_item(zeron_proto::MoveWorkItemParams {
                    work_item_id: item_id,
                    destination_parent_work_item_id: entry.value.clone(),
                    placement: WorkOutlinePlacement::Last,
                    operation_id,
                })
            }
            PickerPurpose::AssociateChat { chat_id } => {
                let Some(item_id) = entry.value.clone() else {
                    return;
                };
                actions.assign_chat(zeron_proto::AssignChatToWorkItemParams {
                    chat_id,
                    work_item_id: item_id,
                    operation_id,
                })
            }
            PickerPurpose::AssociateWithItem { item_id } => {
                let Some(chat_id) = entry.value.clone() else {
                    return;
                };
                actions.assign_chat(zeron_proto::AssignChatToWorkItemParams {
                    chat_id,
                    work_item_id: item_id,
                    operation_id,
                })
            }
        };
        picker.submitting = true;
        cx.notify();
        let _ = window;
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(_) => this.close_picker(window, cx),
                Err(error) => {
                    if let Some(picker) = this.picker.as_mut() {
                        picker.submitting = false;
                    }
                    this.show_notice(error.message.clone(), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    // ----------------------------------------------------------------------
    // Direct restructuring (keyboard, palette, drag and drop)
    // ----------------------------------------------------------------------

    /// Move up / down, indent or outdent the selected work item (with its
    /// descendants). A boundary no-op is `Ok` and sends nothing; `Err`
    /// carries the notice to show.
    pub(crate) fn restructure_selected(
        &mut self,
        op: RestructureOp,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let (space_id, item_id) = self.selected_item().ok_or("Select a work item first")?;
        let snapshot = self
            .snapshot(&space_id)
            .ok_or("This project's outline is not loaded")?;
        if snapshot
            .item(&item_id)
            .is_some_and(|item| item.archived_at.is_some())
        {
            return Err("Restore this work item before moving it");
        }
        let model = &self.model;
        let plan = restructure::plan(snapshot, &item_id, op, |id| {
            model.position(&row_key::item(&space_id, id)).is_some()
        });
        if let Some(plan) = plan {
            self.submit_direct(space_id, DirectMutation::Move(plan), cx);
        }
        Ok(())
    }

    /// Key handler for the restructuring actions. Without a selected work
    /// item the key propagates (Tab / Shift+Tab then walk focus as usual).
    fn restructure_key(&mut self, op: RestructureOp, cx: &mut Context<Self>) {
        if self.is_editing() || self.selected_item().is_none() {
            cx.propagate();
            return;
        }
        if let Err(notice) = self.restructure_selected(op, cx) {
            self.show_notice(notice, cx);
        }
    }

    /// Whether a direct restructuring is in flight or awaiting its refresh.
    pub(crate) fn restructuring_busy(&self) -> bool {
        self.attempt
            .as_ref()
            .is_some_and(|attempt| attempt.state != AttemptState::Failed)
    }

    /// Send `mutation` after the drift confirmation when it changes an
    /// assigned Chat's effective instructions. Ignored while the previous
    /// restructuring has not landed yet (it would plan from a stale order).
    pub(crate) fn submit_direct(
        &mut self,
        space_id: String,
        mutation: DirectMutation,
        cx: &mut Context<Self>,
    ) {
        if self.restructuring_busy() {
            return;
        }
        let change = mutation.change();
        let impact = self
            .snapshot(&space_id)
            .map(|snapshot| instructions::drift_impact(snapshot, &change))
            .unwrap_or_default();
        if impact.needs_confirmation() {
            let weak = cx.entity().downgrade();
            let request = WorkOutlineDriftRequest {
                title: change.title().into(),
                confirm_label: change.confirm_label().into(),
                impact,
                on_decision: Box::new(move |confirmed, _, cx| {
                    if confirmed {
                        weak.update(cx, |this, cx| this.send_direct(space_id, mutation, cx))
                            .ok();
                    }
                }),
            };
            self.request_drift_confirm(request, cx);
            return;
        }
        self.send_direct(space_id, mutation, cx);
    }

    fn send_direct(&mut self, space_id: String, mutation: DirectMutation, cx: &mut Context<Self>) {
        if self.restructuring_busy() {
            return;
        }
        // A retry of the failed mutation replays its operation id.
        let operation_id = match self.attempt.as_ref() {
            Some(attempt) if attempt.mutation == mutation => attempt.operation_id.clone(),
            _ => new_operation_id(),
        };
        let actions = self.state.read(cx).work_outline_actions();
        let (task, follow_key, expand_key) = match &mutation {
            DirectMutation::Move(plan) => (
                actions.move_work_item(plan.params(operation_id.clone())),
                row_key::item(&space_id, &plan.work_item_id),
                plan.destination_parent
                    .as_deref()
                    .map(|parent| row_key::item(&space_id, parent)),
            ),
            DirectMutation::AssignChat {
                chat_id,
                work_item_id,
            } => (
                actions.assign_chat(zeron_proto::AssignChatToWorkItemParams {
                    chat_id: chat_id.clone(),
                    work_item_id: work_item_id.clone(),
                    operation_id: operation_id.clone(),
                }),
                row_key::chat(chat_id),
                Some(row_key::item(&space_id, work_item_id)),
            ),
        };
        let sent_snapshot = self.snapshot_address(&space_id);
        self.attempt = Some(MutationAttempt {
            mutation,
            space_id: space_id.clone(),
            operation_id: operation_id.clone(),
            state: AttemptState::InFlight,
            sent_snapshot,
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let acknowledged = this
                .update(cx, |this, cx| {
                    let ours = this
                        .attempt
                        .as_ref()
                        .is_some_and(|attempt| attempt.operation_id == operation_id);
                    match result {
                        Ok(_) => {
                            if let Some(key) = expand_key.as_deref() {
                                this.set_collapsed(key, false, cx);
                            }
                            if this.snapshot_address(&space_id) != sent_snapshot {
                                // The frame already landed: reveal now.
                                if ours {
                                    this.attempt = None;
                                }
                                this.select(&follow_key, cx);
                                false
                            } else {
                                if ours {
                                    if let Some(attempt) = this.attempt.as_mut() {
                                        attempt.state = AttemptState::AwaitingRefresh;
                                    }
                                }
                                this.follow = Some(follow_key.clone());
                                if this.model.position(&follow_key).is_some() {
                                    this.selected = Some(follow_key.clone());
                                }
                                cx.notify();
                                true
                            }
                        }
                        Err(error) => {
                            if ours {
                                if let Some(attempt) = this.attempt.as_mut() {
                                    attempt.state = AttemptState::Failed;
                                }
                            }
                            this.show_notice(error.message.clone(), cx);
                            false
                        }
                    }
                })
                .unwrap_or(false);
            if !acknowledged {
                return;
            }
            cx.background_executor().timer(AWAIT_REFRESH).await;
            this.update(cx, |this, _| {
                if this.attempt.as_ref().is_some_and(|attempt| {
                    attempt.operation_id == operation_id
                        && attempt.state == AttemptState::AwaitingRefresh
                }) {
                    this.attempt = None;
                    this.follow = None;
                }
            })
            .ok();
        })
        .detach();
    }

    fn picker_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let last = picker.entries.len().saturating_sub(1);
        match event.keystroke.key.as_str() {
            "down" => picker.active = (picker.active + 1).min(last),
            "up" => picker.active = picker.active.saturating_sub(1),
            "enter" => {
                let ix = picker.active;
                self.choose_picker_entry(ix, window, cx);
            }
            "escape" => self.close_picker(window, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    // ----------------------------------------------------------------------
    // Rendering
    // ----------------------------------------------------------------------

    fn row_height(compact: bool) -> f32 {
        if compact { 24.0 } else { 28.0 }
    }

    fn render_rows(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = Theme::of(cx).clone();
        let compact = crate::settings::current(cx).sidebar_compact;
        let create = match self.editing.as_ref().map(|edit| &edit.kind) {
            Some(EditKind::Create { anchor, .. }) => Some(create_slot(&self.model, anchor)),
            _ => None,
        };
        let focused = self.focus.contains_focused(window, cx);
        range
            .map(|list_ix| match create {
                Some((slot, depth)) if list_ix == slot => {
                    self.render_editor_row(depth, compact, &theme, cx)
                }
                Some((slot, _)) if list_ix > slot => {
                    self.render_row(list_ix - 1, compact, focused, &theme, cx)
                }
                _ => self.render_row(list_ix, compact, focused, &theme, cx),
            })
            .collect()
    }

    fn indent(depth: u16) -> f32 {
        8.0 + depth as f32 * 14.0
    }

    fn render_editor_row(
        &self,
        depth: u16,
        compact: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let submitting = self.editing.as_ref().is_some_and(|edit| edit.submitting);
        div()
            .id("outline-row-editor")
            .debug_selector(|| "outline-row-editor".into())
            .h(px(Self::row_height(compact)))
            .w_full()
            .flex()
            .items_center()
            .pl(px(Self::indent(depth) + 16.0))
            .pr(px(8.0))
            .child(self.editor_box(submitting, theme, cx))
            .into_any_element()
    }

    fn editor_box(&self, submitting: bool, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex_1()
            .min_w_0()
            .h(px(20.0))
            .px(px(6.0))
            .flex()
            .items_center()
            .rounded(px(4.0))
            .border_1()
            .border_color(theme.accent)
            .bg(theme.surface)
            .overflow_hidden()
            .when(submitting, |el| el.opacity(0.6))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "enter" => this.commit_edit(cx),
                    "escape" => this.cancel_edit_and_refocus(window, cx),
                    _ => return,
                }
                cx.stop_propagation();
                window.prevent_default();
            }))
            .child(self.editor.clone())
            .into_any_element()
    }

    fn render_row(
        &self,
        ix: usize,
        compact: bool,
        focused: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self.model.rows.get(ix) else {
            return div().into_any_element();
        };
        let key = row.key.clone();
        let selected = self.selected.as_deref() == Some(key.as_str());
        let highlighted = self.details_key.as_deref() == Some(key.as_str())
            || row
                .chat()
                .is_some_and(|chat| self.panel_chat.as_deref() == Some(chat.chat.id.as_str()));
        let renaming = matches!(
            self.editing.as_ref().map(|edit| &edit.kind),
            Some(EditKind::Rename { key: k, .. }) if *k == key
        );
        let selector = format!("outline-row-{key}");
        let disclosure = if row.has_children {
            let toggle_key = key.clone();
            let collapsed = row.collapsed;
            div()
                .id(SharedString::from(format!("outline-disclosure-{key}")))
                .flex_none()
                .size(px(14.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(3.0))
                .cursor_pointer()
                .hover(|el| el.bg(theme.element_hover))
                .child(
                    crate::icons::icon(if collapsed {
                        crate::icons::ALT_ARROW_RIGHT
                    } else {
                        crate::icons::ALT_ARROW_DOWN
                    })
                    .size(px(10.0))
                    .text_color(theme.text_muted),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.set_collapsed(&toggle_key, !collapsed, cx);
                }))
                .into_any_element()
        } else {
            div().flex_none().size(px(14.0)).into_any_element()
        };
        let body: AnyElement = match &row.kind {
            OutlineRowKind::Space { label, space_id } => {
                let open_space = space_id.clone();
                let (totals, proposals) = self
                    .snapshot(space_id)
                    .map(|snapshot| {
                        (
                            super::required_actions::space_totals(snapshot),
                            super::required_actions::pending_proposal_count(snapshot, None),
                        )
                    })
                    .unwrap_or_default();
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(SharedString::from(label.clone())),
                    )
                    .when_some(totals, |el, totals| {
                        el.child(status_totals_element(
                            format!("outline-totals-{key}"),
                            totals,
                            theme,
                        ))
                    })
                    .when(proposals > 0, |el| {
                        el.child(self.proposals_chip(&key, space_id, "", proposals, theme, cx))
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!(
                                "outline-space-instructions-{key}"
                            )))
                            .debug_selector({
                                let selector = format!("outline-space-instructions-{space_id}");
                                move || selector.clone()
                            })
                            .flex_none()
                            .px(px(4.0))
                            .rounded(px(3.0))
                            .cursor_pointer()
                            .text_size(crate::typography::ui_rems(10.0))
                            .text_color(theme.text_faint)
                            .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
                            .child("Instructions")
                            .tooltip(|_, cx| {
                                cx.new(|_| TextTooltip("Space-root instructions".into()))
                                    .into()
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if !this.is_editing() {
                                    cx.emit(OutlineEvent::OpenSpaceRoot {
                                        space_id: open_space.clone(),
                                    });
                                }
                            })),
                    )
                    .into_any_element()
            }
            OutlineRowKind::Unassociated { label, .. } => div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.text_muted)
                        .child(SharedString::from(label.clone())),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(crate::typography::ui_rems(10.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(row.child_count.to_string())),
                )
                .into_any_element(),
            OutlineRowKind::Notice { text, tone, .. } => div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(match tone {
                    NoticeTone::Muted => theme.text_faint,
                    NoticeTone::Warning => theme.warning,
                })
                .child(SharedString::from(text.clone()))
                .into_any_element(),
            OutlineRowKind::Item(item) => {
                if renaming {
                    let submitting = self.editing.as_ref().is_some_and(|edit| edit.submitting);
                    self.editor_box(submitting, theme, cx)
                } else {
                    let proposals = (item.pending_proposals > 0).then(|| {
                        self.proposals_chip(
                            &key,
                            &item.space_id,
                            &item.item_id,
                            item.pending_proposals,
                            theme,
                            cx,
                        )
                    });
                    item_body(&key, item, proposals, theme)
                }
            }
            OutlineRowKind::Chat(chat) => {
                let drift = chat
                    .work_item
                    .as_ref()
                    .and_then(|(space, _)| self.snapshot(space))
                    .and_then(|snapshot| snapshot.chat_instruction_state(&chat.chat.id))
                    .is_some_and(|state| state.drift);
                let chip = (drift && !chat.dangling).then(|| {
                    let chat_id = chat.chat.id.clone();
                    div()
                        .id(SharedString::from(format!("outline-drift-{key}")))
                        .debug_selector({
                            let selector = format!("outline-drift-{key}");
                            move || selector.clone()
                        })
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(2.0))
                        .px(px(4.0))
                        .rounded(px(3.0))
                        .cursor_pointer()
                        .text_size(crate::typography::ui_rems(10.0))
                        .text_color(theme.warning)
                        .hover(|el| el.bg(theme.element_hover))
                        .child(
                            crate::icons::icon(crate::icons::DANGER_TRIANGLE)
                                .size(px(10.0))
                                .text_color(theme.warning),
                        )
                        .child("Instructions changed")
                        .tooltip(|_, cx| {
                            cx.new(|_| {
                                TextTooltip(
                                    "Instructions changed since this Chat\u{2019}s applied revision. \
                                     Click to compare and apply."
                                        .into(),
                                )
                            })
                            .into()
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if let Some(shell) = this.shell.as_ref().and_then(WeakEntity::upgrade)
                            {
                                shell.update(cx, |shell, cx| {
                                    shell.open_instruction_compare(&chat_id, cx)
                                });
                            }
                        }))
                        .into_any_element()
                });
                chat_body(&key, chat, chip, theme)
            }
        };
        // Row action: Start Chat on the selected, live work item.
        let start_chat = row
            .item()
            .filter(|item| selected && !renaming && !item.archived)
            .map(|item| {
                let space_id = item.space_id.clone();
                let item_id = item.item_id.clone();
                div()
                    .id(SharedString::from(format!("outline-start-chat-{key}")))
                    .debug_selector({
                        let selector = format!("outline-start-chat-{key}");
                        move || selector.clone()
                    })
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .px(px(4.0))
                    .rounded(px(3.0))
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(10.0))
                    .text_color(theme.text_muted)
                    .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
                    .child(
                        crate::icons::icon(crate::icons::ACTION_PLAY)
                            .size(px(10.0))
                            .text_color(theme.text_muted),
                    )
                    .child("Start Chat")
                    .tooltip(|_, cx| {
                        cx.new(|_| TextTooltip("Start a Chat in this work item".into()))
                            .into()
                    })
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        if !this.is_editing() {
                            cx.emit(OutlineEvent::StartChat {
                                space_id: space_id.clone(),
                                item_id: item_id.clone(),
                            });
                        }
                    }))
            });
        let open_row = row.clone();
        let click_key = key.clone();
        let dimmed = row
            .item()
            .is_some_and(|item| item.archived || item.kept_for_descendants)
            || row
                .chat()
                .is_some_and(|chat| chat.chat.archived || chat.dangling);
        div()
            .id(SharedString::from(format!("outline-row-{key}")))
            .debug_selector(move || selector.clone())
            .h(px(Self::row_height(compact)))
            .w_full()
            .flex()
            .items_center()
            .gap(px(4.0))
            .pl(px(Self::indent(row.depth)))
            .pr(px(8.0))
            .rounded(px(4.0))
            .text_size(crate::typography::ui_rems(12.5))
            .border_1()
            .border_color(gpui::transparent_black())
            .when(highlighted, |el| el.border_color(theme.accent.opacity(0.6)))
            .when(selected && focused, |el| el.bg(theme.element_active))
            .when(selected && !focused, |el| el.bg(theme.element_hover))
            .when(!selected, |el| {
                el.hover(|el| el.bg(theme.element_hover.opacity(0.6)))
            })
            .when(dimmed, |el| el.opacity(0.6))
            .cursor_pointer()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if !this.is_editing() {
                        window.focus(&this.focus, cx);
                    }
                }),
            )
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                if this.is_editing() {
                    return;
                }
                this.select(&click_key, cx);
                if event.click_count() >= 2 {
                    this.open_row(&open_row, cx);
                }
            }))
            .child(disclosure)
            .child(body)
            .children(start_chat)
            .into_any_element()
    }

    /// "N proposed instructions" (compact: icon + count, full text in the
    /// tooltip); opens the review in details. `item_id` empty = Space root.
    fn proposals_chip(
        &self,
        key: &str,
        space_id: &str,
        item_id: &str,
        count: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tip: SharedString = format!(
            "{} \u{2014} click to review",
            super::required_actions::proposals_label(count)
        )
        .into();
        let selector = format!("outline-proposals-{key}");
        let space_id = space_id.to_owned();
        let item_id = item_id.to_owned();
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .flex_none()
            .flex()
            .items_center()
            .gap(px(2.0))
            .px(px(3.0))
            .rounded(px(3.0))
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(10.0))
            .text_color(theme.text_muted)
            .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
            .child(
                crate::icons::icon(crate::icons::DOCUMENT)
                    .size(px(10.0))
                    .text_color(theme.text_muted),
            )
            .child(SharedString::from(format!("{count} proposed")))
            .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                if !this.is_editing() {
                    cx.emit(OutlineEvent::OpenProposals {
                        space_id: space_id.clone(),
                        item_id: item_id.clone(),
                    });
                }
            }))
            .into_any_element()
    }

    fn render_picker(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker = self.picker.as_ref()?;
        let rows = picker.entries.iter().enumerate().map(|(ix, entry)| {
            let active = ix == picker.active;
            div()
                .id(("outline-picker-entry", ix))
                .debug_selector(move || format!("outline-picker-entry-{ix}"))
                .h(px(24.0))
                .flex()
                .items_center()
                .pl(px(8.0 + entry.depth as f32 * 12.0))
                .pr(px(8.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text)
                .when(active, |el| el.bg(theme.element_active))
                .hover(|el| el.bg(theme.element_hover))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(entry.label.clone())),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.choose_picker_entry(ix, window, cx);
                }))
        });
        Some(
            div()
                .id("outline-picker")
                .debug_selector(|| "outline-picker".into())
                .key_context(PICKER_KEY_CONTEXT)
                .track_focus(&self.picker_focus)
                .mx(px(8.0))
                .mb(px(6.0))
                .p(px(6.0))
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.surface_raised)
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    this.picker_key(event, window, cx);
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .pb(px(4.0))
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(picker.title.clone()))
                        .child(
                            div()
                                .id("outline-picker-cancel")
                                .px(px(6.0))
                                .rounded(px(4.0))
                                .cursor_pointer()
                                .hover(|el| el.bg(theme.element_hover))
                                .child("Cancel")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.close_picker(window, cx);
                                })),
                        ),
                )
                .child(
                    div()
                        .id("outline-picker-list")
                        .max_h(px(240.0))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .children(rows),
                )
                .into_any_element(),
        )
    }
}

/// The compact descendant totals ("3 active · 1 user action required"):
/// faint text, the attention part in the attention token.
fn status_totals_element(
    id: String,
    totals: super::required_actions::StatusTotals,
    theme: &Theme,
) -> AnyElement {
    let tip: SharedString = format!("Including descendants: {}", totals.label()).into();
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(3.0))
        .text_size(crate::typography::ui_rems(10.0))
        .text_color(theme.text_faint)
        .when_some(totals.active_label(), |el, label| {
            el.child(SharedString::from(label))
        })
        .when(totals.active > 0 && totals.user_action_required > 0, |el| {
            el.child("\u{b7}")
        })
        .when_some(totals.attention_label(), |el, label| {
            el.child(
                div()
                    .text_color(chips::attention_color(theme))
                    .child(SharedString::from(label)),
            )
        })
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
        .into_any_element()
}

fn item_body(
    key: &str,
    item: &ItemRow,
    proposals: Option<AnyElement>,
    theme: &Theme,
) -> AnyElement {
    let totals = (!item.archived)
        .then(|| super::required_actions::status_totals(&item.rollup))
        .flatten();
    let (glyph, label, color) = chips::status_style(item.effective_status, theme);
    let status_tip: SharedString = format!("Status: {label}").into();
    let chats_tip: SharedString = format!(
        "{} associated chat{}",
        item.chat_count,
        if item.chat_count == 1 { "" } else { "s" }
    )
    .into();
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(
            div()
                .id(SharedString::from(format!("outline-status-{key}")))
                .flex_none()
                .w(px(12.0))
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(color)
                .child(glyph)
                .tooltip(move |_, cx| cx.new(|_| TextTooltip(status_tip.clone())).into()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme.text)
                .child(SharedString::from(item.title.clone())),
        )
        .when_some(
            chips::attention_indicator(
                format!("outline-attention-{key}"),
                item.needs_user_action(),
                item.unresolved_actions,
                theme,
                1.0,
            ),
            |el, indicator| el.child(indicator),
        )
        .when_some(totals, |el, totals| {
            el.child(status_totals_element(
                format!("outline-totals-{key}"),
                totals,
                theme,
            ))
        })
        .children(proposals)
        .when(item.chat_count > 0, |el| {
            el.child(
                div()
                    .id(SharedString::from(format!("outline-chats-{key}")))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .text_size(crate::typography::ui_rems(10.0))
                    .text_color(theme.text_faint)
                    .child(
                        crate::icons::icon(crate::icons::CHAT_ROUND_LINE)
                            .size(px(10.0))
                            .text_color(theme.text_faint),
                    )
                    .child(SharedString::from(item.chat_count.to_string()))
                    .tooltip(move |_, cx| cx.new(|_| TextTooltip(chats_tip.clone())).into()),
            )
        })
        .when(item.execution_exceptions > 0, |el| {
            let tip = super::execution::exception_tooltip(item.execution_exceptions as usize);
            el.child(
                div()
                    .id(SharedString::from(format!(
                        "outline-execution-exceptions-{key}"
                    )))
                    .debug_selector({
                        let selector = format!("outline-execution-exceptions-{key}");
                        move || selector.clone()
                    })
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .text_size(crate::typography::ui_rems(10.0))
                    .text_color(theme.accent)
                    .child(
                        crate::icons::icon(crate::icons::FOLDER_WITH_FILES)
                            .size(px(10.0))
                            .text_color(theme.accent),
                    )
                    .child(SharedString::from(item.execution_exceptions.to_string()))
                    .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into()),
            )
        })
        .when(item.has_references, |el| {
            el.child(
                div()
                    .id(SharedString::from(format!("outline-refs-{key}")))
                    .flex_none()
                    .child(
                        crate::icons::icon(crate::icons::GLOBAL)
                            .size(px(10.0))
                            .text_color(theme.text_faint),
                    )
                    .tooltip(|_, cx| {
                        cx.new(|_| TextTooltip("Has external references".into()))
                            .into()
                    }),
            )
        })
        .into_any_element()
}

fn chat_body(key: &str, row: &ChatRow, drift: Option<AnyElement>, theme: &Theme) -> AnyElement {
    let chat = &row.chat;
    if row.dangling {
        let tip: SharedString = format!(
            "The association survives but chat {} is gone. Disassociate it from Work Item Details.",
            chat.id
        )
        .into();
        return div()
            .id(SharedString::from(format!("outline-dangling-{key}")))
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                crate::icons::icon(crate::icons::DANGER_TRIANGLE)
                    .size(px(10.0))
                    .text_color(theme.warning),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .italic()
                    .text_color(theme.text_muted)
                    .child(SharedString::from(format!("Missing chat · {}", chat.id))),
            )
            .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
            .into_any_element();
    }
    let dot = crate::shell::spaces::status_dot_color(chat.status, theme);
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(div().flex_none().size(px(6.0)).rounded_full().bg(dot))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme.text_muted)
                .child(SharedString::from(chat.title.clone())),
        )
        .when_some(drift, |el, drift| el.child(drift))
        .when_some(chat.branch.clone(), |el, branch| {
            el.child(
                div()
                    .flex_none()
                    .max_w(px(140.0))
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .text_size(crate::typography::ui_rems(10.0))
                    .text_color(theme.text_faint)
                    .child(
                        crate::icons::icon(crate::icons::GIT_BRANCH)
                            .size(px(10.0))
                            .text_color(theme.text_faint),
                    )
                    .child(div().min_w_0().truncate().child(SharedString::from(branch))),
            )
        })
        .when_some(chat.pr_number.filter(|n| *n > 0), |el, number| {
            el.child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .text_size(crate::typography::ui_rems(10.0))
                    .text_color(theme.text_faint)
                    .child(
                        crate::icons::icon(crate::icons::PULL_REQUEST)
                            .size(px(10.0))
                            .text_color(theme.text_faint),
                    )
                    .child(SharedString::from(format!("#{number}"))),
            )
        })
        .when_some(chat.harness.clone(), |el, harness| {
            el.child(
                div()
                    .flex_none()
                    .max_w(px(100.0))
                    .px(px(4.0))
                    .rounded(px(3.0))
                    .bg(theme.element_hover.opacity(0.7))
                    .truncate()
                    .text_size(crate::typography::ui_rems(9.0))
                    .text_color(theme.text_muted.opacity(0.85))
                    .child(SharedString::from(harness)),
            )
        })
        .into_any_element()
}

impl Render for OutlineView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let editing = self.is_editing();
        let count = self.model.len()
            + usize::from(matches!(
                self.editing.as_ref().map(|edit| &edit.kind),
                Some(EditKind::Create { .. })
            ));
        let picker = self.render_picker(&theme, cx);
        // Single Space (no Space row): a compact entry to its Space-root
        // instructions. Multi-Space outlines put it on each Space row.
        let space_root_entry = match self.inputs.spaces.as_slice() {
            [space] => space.load.snapshot().map(|snapshot| {
                let count = instructions::live_instructions(snapshot, None).len();
                let space_id = space.id.clone();
                // Milestone 4: Space-wide totals and Space-root proposals.
                let totals = super::required_actions::space_totals(snapshot);
                let proposals = super::required_actions::pending_proposal_count(snapshot, None);
                let proposals_chip = (proposals > 0).then(|| {
                    self.proposals_chip(
                        &model::row_key::space(&space.id),
                        &space.id,
                        "",
                        proposals,
                        &theme,
                        cx,
                    )
                });
                div()
                    .id("outline-space-root-entry")
                    .debug_selector(|| "outline-space-root-entry".into())
                    .flex_none()
                    .mx(px(12.0))
                    .mb(px(2.0))
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_faint)
                    .hover(|el| el.text_color(theme.text))
                    .child(
                        crate::icons::icon(crate::icons::DOCUMENT)
                            .size(px(10.0))
                            .text_color(theme.text_faint),
                    )
                    .child(SharedString::from(format!(
                        "Space root instructions ({count})"
                    )))
                    .children(proposals_chip)
                    .child(div().flex_1())
                    .when_some(totals, |el, totals| {
                        el.child(status_totals_element(
                            "outline-totals-space-root".into(),
                            totals,
                            &theme,
                        ))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.is_editing() {
                            cx.emit(OutlineEvent::OpenSpaceRoot {
                                space_id: space_id.clone(),
                            });
                        }
                    }))
            }),
            _ => None,
        };
        let notice = self.notice.clone();
        let _ = window;
        let list: AnyElement = if count == 0 {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted)
                .child(if self.inputs.options.query.is_empty() {
                    "No chats or work items"
                } else {
                    "Nothing matches the search"
                })
                .into_any_element()
        } else {
            gpui::uniform_list("work-outline-rows", count, cx.processor(Self::render_rows))
                .flex_1()
                .min_h_0()
                .px(px(6.0))
                .pb(px(8.0))
                .track_scroll(&self.scroll)
                .into_any_element()
        };
        div()
            .id("work-outline")
            .debug_selector(|| "work-outline".into())
            .track_focus(&self.focus)
            .when(!editing, |el| el.key_context(KEY_CONTEXT))
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .on_action(
                cx.listener(|this, _: &SelectNext, _, cx| this.navigate(NavCommand::Next, cx)),
            )
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                this.navigate(NavCommand::Previous, cx)
            }))
            .on_action(
                cx.listener(|this, _: &SelectFirst, _, cx| this.navigate(NavCommand::First, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectLast, _, cx| this.navigate(NavCommand::Last, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectParent, _, cx| this.navigate(NavCommand::Parent, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectChild, _, cx| this.navigate(NavCommand::Child, cx)),
            )
            .on_action(cx.listener(|this, _: &SelectNextSibling, _, cx| {
                this.navigate(NavCommand::NextSibling, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPreviousSibling, _, cx| {
                this.navigate(NavCommand::PreviousSibling, cx)
            }))
            .on_action(cx.listener(|this, _: &Expand, _, cx| this.navigate(NavCommand::Expand, cx)))
            .on_action(
                cx.listener(|this, _: &Collapse, _, cx| this.navigate(NavCommand::Collapse, cx)),
            )
            .on_action(cx.listener(|this, _: &OpenSelected, _, cx| this.open_selected(cx)))
            .on_action(cx.listener(|this, _: &RenameSelected, window, cx| {
                if this.is_editing() {
                    cx.propagate();
                    return;
                }
                if let Err(notice) = this.begin_rename(window, cx) {
                    this.show_notice(notice, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CreateSibling, window, cx| {
                if this.is_editing() {
                    cx.propagate();
                    return;
                }
                if let Err(notice) = this.begin_create(false, window, cx) {
                    this.show_notice(notice, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &MoveItemUp, _, cx| {
                this.restructure_key(RestructureOp::MoveUp, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveItemDown, _, cx| {
                this.restructure_key(RestructureOp::MoveDown, cx)
            }))
            .on_action(cx.listener(|this, _: &IndentItem, _, cx| {
                this.restructure_key(RestructureOp::Indent, cx)
            }))
            .on_action(cx.listener(|this, _: &OutdentItem, _, cx| {
                this.restructure_key(RestructureOp::Outdent, cx)
            }))
            .on_action(cx.listener(|this, _: &CreateChild, window, cx| {
                if this.is_editing() {
                    cx.propagate();
                    return;
                }
                if let Err(notice) = this.begin_create(true, window, cx) {
                    this.show_notice(notice, cx);
                }
            }))
            .when_some(notice, |el, notice| {
                el.child(
                    div()
                        .debug_selector(|| "outline-notice".into())
                        .flex_none()
                        .mx(px(14.0))
                        .mb(px(4.0))
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(notice)),
                )
            })
            .when_some(space_root_entry, |el, entry| el.child(entry))
            .when_some(picker, |el, picker| el.child(picker))
            .child(list)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::super::model::tests::{fixture_chats, ready};
    use super::super::model::{OutlineOptions, flatten, row_key};
    use super::super::store::fixture_ids::*;
    use super::super::store::fixture_snapshot;
    use super::*;

    #[test]
    fn create_slot_follows_the_anchor() {
        let m = flatten(
            &[ready("s1", "Repo", fixture_snapshot("s1"))],
            &fixture_chats("s1"),
            &HashSet::new(),
            &OutlineOptions::default(),
        );
        let launch = row_key::item("s1", LAUNCH);
        // A sibling of Launch v2 goes after its whole subtree.
        let housekeeping = m.position(&row_key::item("s1", HOUSEKEEPING)).unwrap();
        assert_eq!(
            create_slot(&m, &CreateAnchor::After(launch.clone())),
            (housekeeping, 0)
        );
        assert_eq!(create_slot(&m, &CreateAnchor::FirstChildOf(launch)), (1, 1));
        assert_eq!(create_slot(&m, &CreateAnchor::Top), (0, 0));
        assert_eq!(
            create_slot(&m, &CreateAnchor::After("item:gone".into())),
            (0, 0)
        );
    }

    #[test]
    fn bindings_are_scoped_to_the_outline_and_exclude_text_inputs() {
        for binding in key_bindings() {
            let predicate = binding.predicate().expect("scoped").to_string();
            assert!(predicate.contains(KEY_CONTEXT), "{predicate}");
            assert!(predicate.contains("!PaletteSearch"), "{predicate}");
        }
    }
}
