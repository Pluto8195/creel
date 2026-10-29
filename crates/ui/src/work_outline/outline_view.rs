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
use std::collections::{HashSet, VecDeque};
use std::ops::Range;
use std::time::Duration;

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding,
    ScrollStrategy, SharedString, Subscription, UniformListScrollHandle, WeakEntity, Window,
    actions, div, prelude::*, px,
};

use super::action_log;
use super::actions::new_operation_id;
use super::chips::{self, TextTooltip};
pub(crate) mod group;
mod move_toast;
pub(crate) mod templates;
mod tree_guides_view;
mod undo;

use super::hints::{HintKey, WithHint};
use super::instructions::{self, OutlineChange};
use super::model::{
    self, ChatRow, ItemRow, NoticeTone, OutlineChat, OutlineModel, OutlineOptions, OutlineRow,
    OutlineRowKind, OutlineSpace, row_key,
};
use super::nav::{self, NavCommand};
use super::optimistic::{self, LocalChange};
use super::restructure::{self, DropZone, MovePlan, RestructureOp};
use super::store::{WorkOutlineLoad, WorkOutlinePlacement, WorkOutlineSnapshot};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::overview_positions;
use crate::shell::Shell;
use crate::shell::work_outline_ui::OutlineNavKey;
use crate::state::AppState;
use crate::theme::Theme;

#[path = "context_menu.rs"]
pub(crate) mod context_menu;
#[path = "linear_outline.rs"]
pub(crate) mod linear_outline;
#[path = "pr_sync_outline.rs"]
pub(crate) mod pr_sync_outline;
#[path = "space_roots.rs"]
pub(crate) mod space_roots;

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
        OpenDetailsSelected,
        StartChatSelected,
        RenameSelected,
        CreateSibling,
        CreateChild,
        MoveItemUp,
        MoveItemDown,
        IndentItem,
        OutdentItem,
        UndoChange,
        RedoChange,
        GroupSelected,
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
        KeyBinding::new("up", SelectPrevious, ctx),
        KeyBinding::new("down", SelectNext, ctx),
        KeyBinding::new("alt-up", SelectPreviousSibling, ctx),
        KeyBinding::new("alt-down", SelectNextSibling, ctx),
        KeyBinding::new("left", Collapse, ctx),
        KeyBinding::new("right", Expand, ctx),
        KeyBinding::new("enter", OpenSelected, ctx),
        KeyBinding::new(
            &crate::settings::platform_combo("mod-i"),
            OpenDetailsSelected,
            ctx,
        ),
        KeyBinding::new(
            &crate::settings::platform_combo("mod-enter"),
            StartChatSelected,
            ctx,
        ),
        KeyBinding::new("f2", RenameSelected, ctx),
        KeyBinding::new("r", RenameSelected, ctx),
        KeyBinding::new("n", CreateSibling, ctx),
        KeyBinding::new("shift-n", CreateChild, ctx),
        KeyBinding::new("home", SelectFirst, ctx),
        KeyBinding::new("end", SelectLast, ctx),
        KeyBinding::new("shift-up", MoveItemUp, ctx),
        KeyBinding::new("shift-down", MoveItemDown, ctx),
        KeyBinding::new("shift-right", IndentItem, ctx),
        KeyBinding::new("shift-left", OutdentItem, ctx),
        KeyBinding::new("tab", IndentItem, ctx),
        KeyBinding::new("shift-tab", OutdentItem, ctx),
        KeyBinding::new(&crate::settings::platform_combo("mod-z"), UndoChange, ctx),
        KeyBinding::new(
            &crate::settings::platform_combo("mod-shift-g"),
            GroupSelected,
            ctx,
        ),
        KeyBinding::new("ctrl-z", UndoChange, ctx),
        KeyBinding::new(
            &crate::settings::platform_combo("mod-shift-z"),
            RedoChange,
            ctx,
        ),
        KeyBinding::new("ctrl-shift-z", RedoChange, ctx),
    ]
}

/// What the Overview should do in response to the Outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutlineEvent {
    /// Open the Chat's transcript.
    OpenChat(String),
    /// The user moved the selection to another row (arrow keys, Home / End,
    /// sibling jumps, a click). The Overview previews a Chat row in its chat
    /// panel without taking focus. Programmatic selection never sends it.
    SelectionMoved,
    /// Rename the Chat (the Shell's Chat rename dialog).
    RenameChat(String),
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
    /// The Space roots' order, hidden set, or reveal changed (the Overview
    /// redraws its toolbar count).
    SpacesArranged,
    /// A Linear panel issue was dropped on a work item: create it there.
    LinearIssueDropped {
        space_id: String,
        item_id: String,
        issue: Box<mast_proto::linear::LinearIssueSummary>,
    },
    /// A GitHub panel PR was dropped on a work item: create it there.
    PullRequestDropped {
        space_id: String,
        item_id: String,
        pr: Box<mast_proto::PullRequestStatusSummary>,
    },
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
        /// "Group into… › New container…": the items to move under it.
        group: Option<Vec<String>>,
        /// "New from template…": the title row of the new project.
        template: Option<Box<templates::TemplateFlow>>,
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
    /// Undo back to (or redo up to) the chosen change; entry value = `u<id>`
    /// / `r<id>` of the action log entry.
    RecentChanges,
    /// Group the items under a new container (entry value `None`) or the
    /// chosen work item.
    Group {
        space_id: String,
        item_ids: Vec<String>,
    },
    /// "New from template…": choose a template, or the parent of a project
    /// made from a Linear project (`outline_view/templates.rs`).
    Template(templates::TemplateStage),
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
    /// Remove the Chat from its work item (keyboard Outdent from a
    /// top-level item).
    RemoveChat {
        chat_id: String,
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
            Self::RemoveChat { chat_id } => OutlineChange::RemoveChat {
                chat_id: chat_id.clone(),
            },
        }
    }
}

impl From<restructure::ChatMove> for DirectMutation {
    fn from(plan: restructure::ChatMove) -> Self {
        match plan {
            restructure::ChatMove::Assign {
                chat_id,
                work_item_id,
            } => Self::AssignChat {
                chat_id,
                work_item_id,
            },
            restructure::ChatMove::Remove { chat_id } => Self::RemoveChat { chat_id },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptState {
    /// Sent; no reply yet. Further restructuring waits (keyboard presses
    /// queue) so the engine applies them in order.
    InFlight,
    /// Failed: retrying the same mutation reuses `operation_id`.
    Failed,
}

#[derive(Debug, Clone)]
struct MutationAttempt {
    mutation: DirectMutation,
    operation_id: String,
    state: AttemptState,
}

/// One of the user's own changes, shown before the engine confirms it (see
/// `work_outline::optimistic`).
#[derive(Debug, Clone)]
struct OptimisticChange {
    space_id: String,
    operation_id: String,
    change: LocalChange,
    /// The mutation succeeded; the change stays until a frame shows it.
    acknowledged: bool,
}

/// How long an acknowledged change is kept on top of frames that do not
/// show it yet; after that the engine's frame wins.
const AWAIT_REFRESH: Duration = Duration::from_secs(2);

/// How long a direct restructuring may stay in flight before it counts as
/// failed and the next press goes out (RPC calls carry no timeout of their
/// own).
pub(crate) const DIRECT_REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// Row kind for diagnostics (no titles).
fn row_kind_name(kind: &OutlineRowKind) -> &'static str {
    match kind {
        OutlineRowKind::Space { .. } => "space",
        OutlineRowKind::Item(_) => "item",
        OutlineRowKind::Chat(_) => "chat",
        OutlineRowKind::Unassociated { .. } => "unassociated",
        OutlineRowKind::Notice { .. } => "notice",
    }
}

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
    /// A Space root (multi-Space Outline): reorders the roots.
    Space { space_id: String, title: String },
    /// A Linear panel issue: dropped on a work item, creates it there.
    LinearIssue(linear_outline::LinearIssueDrag),
    /// A GitHub panel PR: dropped on a work item, creates it there.
    PullRequest(pr_sync_outline::PrDrag),
}

impl OutlineDrag {
    fn title(&self) -> &str {
        match self {
            Self::Item { title, .. } | Self::Chat { title, .. } | Self::Space { title, .. } => {
                title
            }
            Self::LinearIssue(drag) => &drag.issue.title,
            Self::PullRequest(drag) => &drag.pr.title,
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
    /// The additive multi-selection (row keys of item rows); see `group.rs`.
    multi: HashSet<String>,
    /// Select this key as soon as a refresh brings it in (a just-created item).
    pending_select: Option<String>,
    editing: Option<InlineEdit>,
    editor: Entity<ComposerInput>,
    picker: Option<OutlinePicker>,
    notice: Option<String>,
    notice_seq: u64,
    /// The persistent error card (title, full detail): never auto-hides.
    error_notice: Option<(SharedString, SharedString, SharedString)>,
    /// The non-blocking "Moved X · instructions changed" toast with Undo.
    move_toast: Option<move_toast::MoveToast>,
    move_toast_seq: u64,
    /// "Created project … from template …" with Undo, and the template flow
    /// a caller without a window asked for (`outline_view/templates.rs`).
    templates: templates::TemplatesState,
    /// An undo or redo is running (see `outline_view::undo`).
    log_busy: bool,
    /// The move toast's Undo is sending its inverse: it is not a new change.
    skip_log: bool,
    /// A press landed on a compact row's status glyph: the row's own click
    /// (which still fires for the same press) is ignored once.
    glyph_press: bool,
    /// Item whose Work Item Details are open (row highlight).
    details_key: Option<String>,
    /// Chat shown in the Overview's chat panel (row highlight).
    panel_chat: Option<String>,
    /// Owner of the drift confirmation and the revision comparison.
    shell: Option<WeakEntity<Shell>>,
    /// The latest direct restructuring (keyboard, palette, drop).
    attempt: Option<MutationAttempt>,
    /// Keyboard restructuring pressed while the previous one was in flight,
    /// sent in order as each reply lands.
    queued_restructure: VecDeque<RestructureOp>,
    /// The Spaces as the last frames delivered them; `inputs.spaces` is
    /// these with `optimistic` applied.
    frames: Vec<OutlineSpace>,
    /// The user's own changes not yet shown by a frame, oldest first.
    optimistic: Vec<OptimisticChange>,
    /// Reveal this row once a refresh moves it (selection follows a move).
    follow: Option<String>,
    /// The drop indicator of an in-progress drag.
    drop_hint: Option<DropHint>,
    /// Tree guides (cached with the model), hovered guide, move flash.
    guide_state: tree_guides_view::GuideState,
    /// The user's Space-root order (device-local; see `space_roots`).
    space_order: Vec<String>,
    /// Spaces hidden from the Outline (device-local; see `space_roots`).
    hidden_spaces: std::collections::BTreeSet<String>,
    /// "Hidden projects (N)" is on: hidden Spaces show as dimmed roots.
    show_hidden_spaces: bool,
    /// Fold a single-chat item's Chat into its row (on in the app).
    compact_single_chat: bool,
    _editor_events: Subscription,
    /// Focus gained / lost diagnostics (registered on first render, which
    /// has the window).
    focus_logs: Option<[Subscription; 2]>,
    /// The Outline's measured width (set while painting), for the PR icon
    /// cluster's narrow-width slot dropping; 0 until the first paint.
    row_width: std::rc::Rc<std::cell::Cell<f32>>,
    /// Tests pin the width instead of reading the paint measurement.
    row_width_override: Option<f32>,
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
            multi: HashSet::new(),
            pending_select: None,
            editing: None,
            editor,
            picker: None,
            notice: None,
            error_notice: None,
            notice_seq: 0,
            move_toast: None,
            move_toast_seq: 0,
            templates: Default::default(),
            compact_single_chat: true,
            log_busy: false,
            skip_log: false,
            glyph_press: false,
            details_key: None,
            panel_chat: None,
            shell: None,
            attempt: None,
            queued_restructure: VecDeque::new(),
            frames: Vec::new(),
            optimistic: Vec::new(),
            follow: None,
            drop_hint: None,
            guide_state: Default::default(),
            space_order: Vec::new(),
            hidden_spaces: std::collections::BTreeSet::new(),
            show_hidden_spaces: false,
            _editor_events: editor_events,
            focus_logs: None,
            row_width: Default::default(),
            row_width_override: None,
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

    pub(crate) fn set_compact_single_chat(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.compact_single_chat != on {
            self.compact_single_chat = on;
            self.reflatten();
            cx.notify();
        }
    }

    pub(crate) fn set_inputs(&mut self, inputs: OutlineInputs, cx: &mut Context<Self>) {
        self.ensure_collapsed_loaded(cx);
        self.frames = inputs.spaces.clone();
        self.inputs = inputs;
        self.retire_shown_optimistic();
        self.overlay_optimistic();
        self.reflatten();
        cx.notify();
    }

    /// A frame that shows an acknowledged change replaces it (frame wins).
    /// The engine applies one Space's changes in the order they were sent,
    /// so a frame showing a later acknowledged change also covers every
    /// earlier one, even when that one no longer reads as "shown" (the same
    /// item moved again): replaying it on the frame would flicker the item
    /// back to an intermediate place.
    fn retire_shown_optimistic(&mut self) {
        let shown: Vec<bool> = self
            .optimistic
            .iter()
            .map(|entry| {
                entry.acknowledged
                    && self
                        .frames
                        .iter()
                        .find(|space| space.id == entry.space_id)
                        .and_then(|space| space.load.snapshot())
                        .is_some_and(|frame| optimistic::apply(frame, &entry.change).is_none())
            })
            .collect();
        let retire: Vec<bool> = (0..self.optimistic.len())
            .map(|ix| {
                let entry = &self.optimistic[ix];
                entry.acknowledged
                    && (ix..self.optimistic.len()).any(|later| {
                        shown[later] && self.optimistic[later].space_id == entry.space_id
                    })
            })
            .collect();
        let mut ix = 0;
        self.optimistic.retain(|_| {
            ix += 1;
            !retire[ix - 1]
        });
    }

    /// Rebuild `inputs.spaces` from the frames with the pending changes
    /// applied in order.
    fn overlay_optimistic(&mut self) {
        let mut spaces = self.frames.clone();
        for space in &mut spaces {
            let patched = {
                let Some(frame) = space.load.snapshot() else {
                    continue;
                };
                let mut patched: Option<WorkOutlineSnapshot> = None;
                for entry in self.optimistic.iter().filter(|e| e.space_id == space.id) {
                    if let Some(next) =
                        optimistic::apply(patched.as_ref().unwrap_or(frame), &entry.change)
                    {
                        patched = Some(next);
                    }
                }
                patched
            };
            if let Some(patched) = patched {
                space.load = WorkOutlineLoad::Ready(std::sync::Arc::new(patched));
            }
        }
        self.inputs.spaces = spaces;
    }

    /// Show `change` now; `follow` is the row to select and reveal at its
    /// new place.
    fn show_optimistic(
        &mut self,
        space_id: &str,
        operation_id: &str,
        change: LocalChange,
        follow: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.optimistic
            .retain(|entry| entry.operation_id != operation_id);
        self.optimistic.push(OptimisticChange {
            space_id: space_id.to_owned(),
            operation_id: operation_id.to_owned(),
            change,
            acknowledged: false,
        });
        self.overlay_optimistic();
        if let Some(key) = follow.as_ref() {
            self.follow = Some(key.clone());
        }
        self.reflatten();
        if let Some(key) = follow {
            if self.model.position(&key).is_some() {
                self.follow = None;
                self.select(&key, cx);
            }
        }
        cx.notify();
    }

    /// The engine accepted `operation_id`: keep showing it until a frame does
    /// (or [`AWAIT_REFRESH`] passes, then the frame wins).
    fn acknowledge_optimistic(&mut self, operation_id: &str, cx: &mut Context<Self>) {
        let Some(entry) = self
            .optimistic
            .iter_mut()
            .find(|entry| entry.operation_id == operation_id)
        else {
            return;
        };
        entry.acknowledged = true;
        let operation_id = operation_id.to_owned();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AWAIT_REFRESH).await;
            this.update(cx, |this, cx| this.drop_optimistic(&operation_id, cx))
                .ok();
        })
        .detach();
    }

    /// Stop showing `operation_id` (a failed mutation rolls back this way).
    fn drop_optimistic(&mut self, operation_id: &str, cx: &mut Context<Self>) {
        let before = self.optimistic.len();
        self.optimistic
            .retain(|entry| entry.operation_id != operation_id);
        if self.optimistic.len() != before {
            self.overlay_optimistic();
            self.reflatten();
            cx.notify();
        }
    }

    /// Changes shown but not yet in a frame (tests).
    #[cfg(test)]
    pub(crate) fn optimistic_len(&self) -> usize {
        self.optimistic.len()
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
        self.load_space_prefs(&data_dir);
    }

    /// Flatten the inputs in the user's Space order, without hidden Spaces
    /// (unless revealed).
    fn flatten_inputs(&self, collapsed: &HashSet<String>) -> OutlineModel {
        let options = OutlineOptions {
            show_hidden_spaces: self.show_hidden_spaces,
            compact_single_chat: self.compact_single_chat,
            ..self.inputs.options.clone()
        };
        model::flatten(
            &self.arranged_spaces(),
            &self.inputs.chats,
            collapsed,
            &options,
        )
    }

    fn reflatten(&mut self) {
        let previous = std::mem::take(&mut self.model);
        self.model = self.flatten_inputs(&self.collapsed);
        self.refresh_guides();
        if let Some(follow) = self.follow.clone() {
            let place =
                |model: &OutlineModel| model.position(&follow).map(|ix| (ix, model.rows[ix].depth));
            let before = place(&previous);
            if let Some((after, depth)) = place(&self.model) {
                if before != Some((after, depth)) {
                    // The moved item's new place: select and reveal it.
                    self.selected = Some(
                        self.model
                            .canonical_key(&follow)
                            .unwrap_or(&follow)
                            .to_string(),
                    );
                    self.follow = None;
                    self.scroll.scroll_to_item(after, ScrollStrategy::Nearest);
                    return;
                }
            }
        }
        if let Some(pending) = self.pending_select.clone() {
            if let Some(key) = self.model.canonical_key(&pending) {
                self.selected = Some(key.to_string());
                self.pending_select = None;
                return;
            }
        }
        if let Some(selected) = self.selected.clone() {
            self.selected = nav::reconcile_selection(&previous, &self.model, &selected);
        }
        self.prune_multi();
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

    /// The Chat the selection stands for when it is opened or previewed: a
    /// Chat row's, or a compact item row's. Rename / move keys keep acting on
    /// the item ([`Self::selected_chat`] is `None` there).
    pub(crate) fn selected_open_chat(&self) -> Option<String> {
        self.selected_row()
            .and_then(OutlineRow::open_chat)
            .filter(|chat| !chat.dangling)
            .map(|chat| chat.chat.id.clone())
    }

    pub(crate) fn select(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(ix) = self.model.position(key) {
            self.selected = Some(self.model.rows[ix].key.clone());
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    /// Select `key` even when a collapsed ancestor hides it: expand every
    /// collapsed ancestor first (persisted like a manual expand), then select
    /// and scroll it into view. `false` (selection untouched) when the row is
    /// not in the Outline at all, e.g. filtered out or in no shown Space.
    pub(crate) fn reveal(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        if self.model.position(key).is_none() {
            let expanded = self.flatten_inputs(&HashSet::new());
            let Some(ix) = expanded.position(key) else {
                return false;
            };
            let hidden_by: Vec<String> = expanded
                .ancestor_keys(ix)
                .into_iter()
                .filter(|ancestor| self.collapsed.contains(*ancestor))
                .map(str::to_owned)
                .collect();
            for ancestor in hidden_by {
                self.set_collapsed(&ancestor, false, cx);
            }
        }
        if self.model.position(key).is_none() {
            return false;
        }
        self.select(key, cx);
        true
    }

    /// Move the selection off `key` before its row goes away: the next row
    /// past its subtree, else the row above. Nothing when `key` is not shown.
    pub(crate) fn select_past(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(ix) = self.model.position(key) else {
            return;
        };
        let depth = self.model.rows[ix].depth;
        let next = self.model.rows[ix + 1..]
            .iter()
            .find(|row| row.depth <= depth)
            .or_else(|| ix.checked_sub(1).map(|above| &self.model.rows[above]))
            .map(|row| row.key.clone());
        if let Some(next) = next {
            self.select(&next, cx);
        }
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    /// Focus the tree and select its first row (Enter / Down from the
    /// Overview search), previewing it like any keyboard selection move.
    pub(crate) fn focus_first_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.navigate(NavCommand::First, cx);
    }

    pub(crate) fn is_editing(&self) -> bool {
        self.editing.is_some() || self.picker.is_some()
    }

    fn navigate(&mut self, command: NavCommand, cx: &mut Context<Self>) {
        if self.is_editing() {
            cx.propagate();
            return;
        }
        self.clear_multi(cx);
        let result = nav::navigate(&self.model, self.selected.as_deref(), command);
        if let Some((key, collapsed)) = result.set_collapsed {
            self.set_collapsed(&key, collapsed, cx);
        }
        if let Some(key) = result.select {
            let moved = self.selected.as_deref() != Some(key.as_str());
            self.select(&key, cx);
            if moved {
                cx.emit(OutlineEvent::SelectionMoved);
            }
        }
    }

    /// Mod+Enter: Start Chat in the selected work item, or in a Chat row's
    /// work item (a notice for a Chat outside any work item). Other rows do
    /// nothing.
    fn start_chat_selected(&mut self, cx: &mut Context<Self>) {
        if self.is_editing() {
            cx.propagate();
            return;
        }
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        let target = match &row.kind {
            OutlineRowKind::Item(item) => Some((item.space_id.clone(), item.item_id.clone())),
            OutlineRowKind::Chat(chat) => match chat.work_item.clone() {
                Some(target) => Some(target),
                None => {
                    self.show_notice("This Chat is not in a work item", cx);
                    return;
                }
            },
            _ => None,
        };
        if let Some((space_id, item_id)) = target {
            cx.emit(OutlineEvent::StartChat { space_id, item_id });
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

    /// ⌘I: open the selected item's Work Item Details (a compact row's
    /// Enter opens its Chat). Selection and the action log are untouched.
    fn open_details_selected(&mut self, cx: &mut Context<Self>) {
        if self.is_editing() {
            cx.propagate();
            return;
        }
        if let Some(OutlineRowKind::Item(item)) = self.selected_row().map(|row| &row.kind) {
            cx.emit(OutlineEvent::OpenDetails {
                space_id: item.space_id.clone(),
                item_id: item.item_id.clone(),
            });
        }
    }

    /// Open Work Item Details for an item (a compact row's status glyph).
    pub(crate) fn open_item_details(
        &mut self,
        space_id: String,
        item_id: String,
        cx: &mut Context<Self>,
    ) {
        cx.emit(OutlineEvent::OpenDetails { space_id, item_id });
    }

    fn open_row(&mut self, row: &OutlineRow, cx: &mut Context<Self>) {
        match &row.kind {
            // A compact row opens its Chat (the common action); Open Details
            // in its context menu reaches the item.
            OutlineRowKind::Item(ItemRow {
                compact_chat: Some(chat),
                ..
            }) => cx.emit(OutlineEvent::OpenChat(chat.chat.id.clone())),
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

    /// Show a failure as a persistent, copyable error card. Unlike
    /// [`Self::show_notice`] it never auto-hides: the user dismisses it (or
    /// the next error replaces it). The first line of `text` is the title and
    /// the rest the detail.
    pub(crate) fn show_error(&mut self, text: impl AsRef<str>, cx: &mut Context<Self>) {
        let text = text.as_ref().trim();
        let (title, detail) = crate::error_notice::split_error(text);
        self.error_notice = Some((title, detail, text.to_owned().into()));
        cx.notify();
    }

    /// [`Self::show_error`] with an explicit title; `detail` is the full text.
    pub(crate) fn show_error_titled(
        &mut self,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let (title, detail) = (title.into(), detail.into());
        let copy = crate::error_notice::copy_text(&title, &detail).into();
        self.error_notice = Some((title, detail, copy));
        cx.notify();
    }

    pub(crate) fn dismiss_error(&mut self, cx: &mut Context<Self>) {
        if self.error_notice.take().is_some() {
            cx.notify();
        }
    }

    /// The persistent error card's `(title, detail)`.
    pub(crate) fn error_notice(&self) -> Option<(&str, &str)> {
        self.error_notice
            .as_ref()
            .map(|(title, detail, _)| (title.as_ref(), detail.as_ref()))
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
                group: None,
                template: None,
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
            .take(mast_proto::limits::MAX_TITLE_CHARS)
            .collect();
        edit.submitting = true;
        let operation_id = edit.operation_id.clone();
        let kind = edit.kind.clone();
        if let EditKind::Create {
            template: Some(flow),
            ..
        } = &kind
        {
            // "New from template…": create the whole project.
            let flow = (**flow).clone();
            self.editing = None;
            if let Err(notice) = self.instantiate_template(flow, title, cx) {
                self.show_notice(notice, cx);
            }
            cx.notify();
            return;
        }
        if let EditKind::Create {
            space_id,
            group: Some(item_ids),
            ..
        } = &kind
        {
            // "Group into… › New container…": create it and move the items.
            self.editing = None;
            if let Err(notice) = self.group_items(space_id, item_ids, group::GroupDest::New(title), cx)
            {
                self.show_notice(notice, cx);
            }
            cx.notify();
            return;
        }
        let actions = self.state.read(cx).work_outline_actions();
        let (task, space_id, change, created_key, draft) = match kind {
            EditKind::Create {
                space_id,
                parent,
                placement,
                ..
            } => {
                let work_item_id = mast_proto::derive_id(&operation_id, mast_proto::roles::ITEM);
                let task = actions.create_work_item(mast_proto::CreateWorkItemParams {
                    space_id: space_id.clone(),
                    parent_work_item_id: parent.clone(),
                    placement: placement.clone(),
                    title: title.clone(),
                    description: None,
                    status: None,
                    operation_id: operation_id.clone(),
                });
                let key = row_key::item(&space_id, &work_item_id);
                let draft =
                    action_log::create_entry(&work_item_id, &title, Some(operation_id.clone()));
                let change = LocalChange::Create {
                    work_item_id,
                    parent,
                    placement,
                    title,
                    created_at: chrono::Utc::now().timestamp_millis(),
                };
                (task, Some(space_id), change, Some(key), Some(draft))
            }
            EditKind::Rename { item_id, .. } => {
                let space_id = self
                    .snapshot_with_item(&item_id)
                    .map(|snapshot| snapshot.space_id.clone());
                let changes = mast_proto::WorkItemChanges {
                    title: Some(title.clone()),
                    ..Default::default()
                };
                let draft = self.snapshot_with_item(&item_id).and_then(|snapshot| {
                    action_log::update_entry(
                        snapshot,
                        &item_id,
                        &changes,
                        Some(operation_id.clone()),
                    )
                });
                let task = actions.update_work_item(mast_proto::UpdateWorkItemParams {
                    work_item_id: item_id.clone(),
                    changes,
                    operation_id: operation_id.clone(),
                });
                let change = LocalChange::Rename {
                    work_item_id: item_id,
                    title,
                };
                (task, space_id, change, None, draft)
            }
        };
        // Show the result now; the reply only confirms (or rolls back).
        self.editing = None;
        if let Some(space_id) = space_id.as_deref() {
            self.show_optimistic(space_id, &operation_id, change, None, cx);
            self.record_change(space_id, draft, cx);
        }
        if let Some(key) = created_key.clone() {
            if self.model.position(&key).is_some() {
                self.select(&key, cx);
            } else {
                self.pending_select = Some(key);
            }
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                Ok(_) => {
                    this.confirm_change(&operation_id, cx);
                    this.acknowledge_optimistic(&operation_id, cx)
                }
                Err(error) => {
                    if created_key.is_some() && this.pending_select == created_key {
                        this.pending_select = None;
                    }
                    this.forget_change(&operation_id, cx);
                    this.drop_optimistic(&operation_id, cx);
                    this.show_error(error.message.clone(), cx);
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
        if purpose == PickerPurpose::RecentChanges {
            self.choose_recent_change(&entry, window, cx);
            return;
        }
        if let PickerPurpose::Group { space_id, item_ids } = purpose {
            self.choose_group_entry(space_id, item_ids, entry, window, cx);
            return;
        }
        if let PickerPurpose::Template(stage) = purpose {
            self.choose_template_entry(stage, entry, window, cx);
            return;
        }
        let operation_id = picker.operation_id.clone();
        let place_first = picker.place_first;
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
            PickerPurpose::RecentChanges | PickerPurpose::Group { .. } | PickerPurpose::Template(_) => None,
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
            PickerPurpose::RecentChanges | PickerPurpose::Group { .. } | PickerPurpose::Template(_) => None,
        };
        let impact = change
            .as_ref()
            .zip(snapshot)
            .map(|(change, snapshot)| instructions::drift_impact(snapshot, change))
            .unwrap_or_default();
        // A move or association applies at once; one that changes assigned
        // Chats' instructions raises an Undo toast instead of a modal.
        let toast = if impact.needs_confirmation() {
            let mutation = match &purpose {
                PickerPurpose::Move { item_id } => Some(DirectMutation::Move(MovePlan {
                    work_item_id: item_id.clone(),
                    destination_parent: entry.value.clone(),
                    placement: if place_first {
                        WorkOutlinePlacement::First
                    } else {
                        WorkOutlinePlacement::Last
                    },
                })),
                PickerPurpose::AssociateChat { chat_id } => {
                    entry
                        .value
                        .clone()
                        .map(|work_item_id| DirectMutation::AssignChat {
                            chat_id: chat_id.clone(),
                            work_item_id,
                        })
                }
                PickerPurpose::AssociateWithItem { item_id } => {
                    entry
                        .value
                        .clone()
                        .map(|chat_id| DirectMutation::AssignChat {
                            chat_id,
                            work_item_id: item_id.clone(),
                        })
                }
                PickerPurpose::RecentChanges | PickerPurpose::Group { .. } | PickerPurpose::Template(_) => None,
            };
            snapshot
                .map(|snapshot| snapshot.space_id.clone())
                .zip(mutation)
                .map(|(space_id, mutation)| self.draft_move_toast(&space_id, &mutation, &impact))
        } else {
            None
        };
        self.send_picker_entry(ix, window, cx);
        if let Some(draft) = toast {
            self.show_move_toast(draft, Some(operation_id), cx);
        }
    }

    /// Show the chosen move or association at once, close the picker, and
    /// send it; a failure rolls the change back with a notice.
    fn send_picker_entry(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.as_ref() else {
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
        let placement = if picker.place_first {
            WorkOutlinePlacement::First
        } else {
            WorkOutlinePlacement::Last
        };
        let (task, item_id, change) = match picker.purpose.clone() {
            PickerPurpose::RecentChanges | PickerPurpose::Group { .. } | PickerPurpose::Template(_) => return,
            PickerPurpose::Move { item_id } => (
                actions.move_work_item(mast_proto::MoveWorkItemParams {
                    work_item_id: item_id.clone(),
                    destination_parent_work_item_id: entry.value.clone(),
                    placement: placement.clone(),
                    operation_id: operation_id.clone(),
                }),
                item_id.clone(),
                LocalChange::Move {
                    work_item_id: item_id,
                    destination_parent: entry.value.clone(),
                    placement,
                },
            ),
            PickerPurpose::AssociateChat { chat_id } => {
                let Some(item_id) = entry.value.clone() else {
                    return;
                };
                (
                    actions.assign_chat(mast_proto::AssignChatToWorkItemParams {
                        chat_id: chat_id.clone(),
                        work_item_id: item_id.clone(),
                        operation_id: operation_id.clone(),
                    }),
                    item_id.clone(),
                    LocalChange::AssignChat {
                        chat_id,
                        work_item_id: item_id,
                    },
                )
            }
            PickerPurpose::AssociateWithItem { item_id } => {
                let Some(chat_id) = entry.value.clone() else {
                    return;
                };
                (
                    actions.assign_chat(mast_proto::AssignChatToWorkItemParams {
                        chat_id: chat_id.clone(),
                        work_item_id: item_id.clone(),
                        operation_id: operation_id.clone(),
                    }),
                    item_id.clone(),
                    LocalChange::AssignChat {
                        chat_id,
                        work_item_id: item_id,
                    },
                )
            }
        };
        let space_id = self
            .snapshot_with_item(&item_id)
            .map(|snapshot| snapshot.space_id.clone());
        let draft = space_id.as_deref().and_then(|space_id| {
            let mutation = match &change {
                LocalChange::Move {
                    work_item_id,
                    destination_parent,
                    placement,
                } => DirectMutation::Move(MovePlan {
                    work_item_id: work_item_id.clone(),
                    destination_parent: destination_parent.clone(),
                    placement: placement.clone(),
                }),
                LocalChange::AssignChat {
                    chat_id,
                    work_item_id,
                } => DirectMutation::AssignChat {
                    chat_id: chat_id.clone(),
                    work_item_id: work_item_id.clone(),
                },
                _ => return None,
            };
            self.direct_draft(space_id, &mutation, &operation_id)
        });
        self.close_picker(window, cx);
        if let Some(space_id) = space_id {
            // A move: the moved row is followed (and its new parent expanded).
            let follow = match &change {
                LocalChange::Move {
                    work_item_id,
                    destination_parent,
                    ..
                } => {
                    if let Some(parent) = destination_parent {
                        self.set_collapsed(&row_key::item(&space_id, parent), false, cx);
                    }
                    Some(row_key::item(&space_id, work_item_id))
                }
                _ => None,
            };
            self.show_optimistic(&space_id, &operation_id, change, follow, cx);
            self.record_change(&space_id, draft, cx);
        }
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                Ok(_) => {
                    this.confirm_change(&operation_id, cx);
                    this.acknowledge_optimistic(&operation_id, cx)
                }
                Err(error) => {
                    this.forget_change(&operation_id, cx);
                    this.drop_optimistic(&operation_id, cx);
                    this.dismiss_move_toast_for(&operation_id, cx);
                    this.show_error(error.message.clone(), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    // ----------------------------------------------------------------------
    // Direct restructuring (keyboard, palette, drag and drop)
    // ----------------------------------------------------------------------

    /// The restructuring keys and palette commands: re-associate the
    /// selected Chat ([`Self::move_selected_chat`]) or restructure the
    /// selected work item ([`Self::restructure_selected`]).
    pub(crate) fn restructure_selection(
        &mut self,
        op: RestructureOp,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        match self.selected_row().and_then(OutlineRow::chat).cloned() {
            Some(chat) => self.move_selected_chat(&chat, op, cx),
            None => self.restructure_selected(op, cx),
        }
    }

    /// Move up / down, indent or outdent the selected work item (with its
    /// descendants). A boundary no-op is `Ok` and sends nothing; `Err`
    /// carries the notice to show. Chat rows go through
    /// [`Self::restructure_selection`].
    pub(crate) fn restructure_selected(
        &mut self,
        op: RestructureOp,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let Some((space_id, item_id)) = self.selected_item() else {
            tracing::info!(target: "work_outline_keys", ?op, "restructure: no selected work item");
            return Err("Select a work item first");
        };
        let Some(snapshot) = self.snapshot(&space_id) else {
            tracing::info!(
                target: "work_outline_keys",
                ?op, %space_id, %item_id,
                "restructure: outline not loaded"
            );
            return Err("This project's outline is not loaded");
        };
        if snapshot
            .item(&item_id)
            .is_some_and(|item| item.archived_at.is_some())
        {
            tracing::info!(
                target: "work_outline_keys",
                ?op, %space_id, %item_id,
                "restructure: item archived"
            );
            return Err("Restore this work item before moving it");
        }
        let model = &self.model;
        let shown = |id: &str| model.position(&row_key::item(&space_id, id)).is_some();
        let plan = restructure::plan(snapshot, &item_id, op, shown);
        match plan {
            Some(plan) => {
                tracing::info!(
                    target: "work_outline_keys",
                    ?op, %space_id, %item_id,
                    destination_parent = ?plan.destination_parent,
                    placement = ?plan.placement,
                    "restructure: planned"
                );
                self.submit_direct(space_id, DirectMutation::Move(plan), cx);
            }
            None => {
                let reason = restructure::noop_reason(snapshot, &item_id, op, shown);
                tracing::info!(
                    target: "work_outline_keys",
                    ?op, %space_id, %item_id, reason,
                    "restructure: plan=None (no-op)"
                );
            }
        }
        Ok(())
    }

    /// The Chat-row form of the restructuring keys: Up / Down re-associate
    /// the Chat with the previous / next shown work item of its Space in
    /// visual row order, Outdent with its item's parent (out of its item
    /// from the top level), Indent does nothing (see
    /// [`restructure::chat_plan`]). Only this Chat moves; Chats nested under
    /// it keep their own items. Sent through [`Self::submit_direct`], like a
    /// Chat drop: drift confirmation, optimistic display, rollback, queueing.
    fn move_selected_chat(
        &mut self,
        row: &ChatRow,
        op: RestructureOp,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let chat_id = row.chat.id.clone();
        if row.dangling {
            return Err("This chat no longer exists on any device");
        }
        let Some(space_id) = row.chat.space_id.clone() else {
            return Err("Chats without a project cannot be associated");
        };
        let Some(snapshot) = self.snapshot(&space_id) else {
            return Err("This project's outline is not loaded");
        };
        let model = &self.model;
        let shown = |id: &str| model.position(&row_key::item(&space_id, id)).is_some();
        match restructure::chat_plan(snapshot, &chat_id, op, shown) {
            Some(plan) => {
                tracing::info!(
                    target: "work_outline_keys",
                    ?op, %space_id, %chat_id, ?plan,
                    "restructure chat: planned"
                );
                self.submit_direct(space_id, plan.into(), cx);
            }
            None => tracing::info!(
                target: "work_outline_keys",
                ?op, %space_id, %chat_id,
                "restructure chat: plan=None (no-op)"
            ),
        }
        Ok(())
    }

    /// Key handler for the restructuring actions. Without a selected work
    /// item or Chat the key propagates (Tab / Shift+Tab then walk focus as
    /// usual).
    /// A press while the previous move is in flight queues behind it: the
    /// change already shows, so the next plan starts from the new order.
    fn restructure_key(&mut self, op: RestructureOp, cx: &mut Context<Self>) {
        tracing::info!(
            target: "work_outline_keys",
            ?op,
            selected = ?self.selected.as_deref(),
            "restructure key action"
        );
        if self.is_editing() {
            tracing::info!(
                target: "work_outline_keys",
                ?op,
                inline_editor = self.editing.is_some(),
                picker = self.picker.is_some(),
                "restructure key: editing, propagating"
            );
            cx.propagate();
            return;
        }
        // A Space root: ⇧↑ / ⇧↓ reorder the roots (device-local, instant);
        // Indent / Outdent mean nothing there and propagate.
        if let Some(space_id) = self.selected_space_root() {
            match op {
                RestructureOp::MoveUp | RestructureOp::MoveDown => {
                    self.move_space(&space_id, op == RestructureOp::MoveUp, cx);
                }
                RestructureOp::Indent | RestructureOp::Outdent => cx.propagate(),
            }
            return;
        }
        let movable = self.selected_row().is_some_and(|row| {
            matches!(row.kind, OutlineRowKind::Item(_) | OutlineRowKind::Chat(_))
        });
        if !movable {
            tracing::info!(
                target: "work_outline_keys",
                ?op,
                selected = ?self.selected.as_deref(),
                row_kind = self.selected_row().map(|row| row_kind_name(&row.kind)),
                "restructure key: no selected work item or Chat, propagating"
            );
            cx.propagate();
            return;
        }
        if self.restructuring_busy() {
            self.queued_restructure.push_back(op);
            tracing::info!(
                target: "work_outline_keys",
                ?op,
                in_flight = ?self.attempt.as_ref().map(|attempt| attempt.operation_id.as_str()),
                queued = self.queued_restructure.len(),
                "restructure key: busy, queued"
            );
            return;
        }
        let anchor = self.move_anchor();
        match self.restructure_selection(op, cx) {
            Ok(()) => self.flash_if_moved(anchor, cx),
            Err(notice) => self.show_notice(notice, cx),
        }
    }

    /// Whether a direct restructuring is in flight (its reply not back yet).
    pub(crate) fn restructuring_busy(&self) -> bool {
        self.attempt
            .as_ref()
            .is_some_and(|attempt| attempt.state == AttemptState::InFlight)
    }

    /// Work Item Details' associate / disassociate: the Chat move the keys
    /// and drops send, so it applies at once and raises the Undo toast when
    /// the Chat's effective instructions change.
    pub(crate) fn reassociate_chat(
        &mut self,
        space_id: &str,
        chat_id: &str,
        work_item_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.restructuring_busy() {
            self.show_notice(
                "Still applying the previous move. Try again in a moment",
                cx,
            );
            return;
        }
        let chat_id = chat_id.to_owned();
        let mutation = match work_item_id {
            Some(work_item_id) => DirectMutation::AssignChat {
                chat_id,
                work_item_id,
            },
            None => DirectMutation::RemoveChat { chat_id },
        };
        self.submit_direct(space_id.to_owned(), mutation, cx);
    }

    /// Send `mutation` at once (see the Undo toast when it changes an
    /// assigned Chat's effective instructions). Ignored while the previous
    /// restructuring is in flight (the engine must apply them in order).
    pub(crate) fn submit_direct(
        &mut self,
        space_id: String,
        mutation: DirectMutation,
        cx: &mut Context<Self>,
    ) {
        if self.restructuring_busy() {
            tracing::info!(
                target: "work_outline_keys",
                %space_id,
                "submit_direct: ignored, a restructuring is in flight"
            );
            return;
        }
        let change = mutation.change();
        let impact = self
            .snapshot(&space_id)
            .map(|snapshot| instructions::drift_impact(snapshot, &change))
            .unwrap_or_default();
        // Applies at once. A move that changes assigned Chats' instructions
        // raises an Undo toast instead of a blocking confirmation.
        let toast = impact
            .needs_confirmation()
            .then(|| self.draft_move_toast(&space_id, &mutation, &impact));
        if toast.is_some() {
            tracing::info!(
                target: "work_outline_keys",
                %space_id,
                chats = impact.chat_ids.len(),
                items = impact.item_ids.len(),
                "submit_direct: instructions change, applying with an Undo toast"
            );
        }
        self.send_direct(space_id, mutation, cx);
        if let Some(draft) = toast {
            let operation_id = self
                .attempt
                .as_ref()
                .map(|attempt| attempt.operation_id.clone());
            self.show_move_toast(draft, operation_id, cx);
        }
    }

    /// Show `mutation` at once, send it, and reconcile with the reply: on
    /// success the change stays until a frame shows it and the next queued
    /// key press goes out; on failure it rolls back with a notice.
    fn send_direct(&mut self, space_id: String, mutation: DirectMutation, cx: &mut Context<Self>) {
        if self.restructuring_busy() {
            tracing::info!(
                target: "work_outline_keys",
                %space_id,
                "send_direct: ignored, a restructuring is in flight"
            );
            return;
        }
        // A retry of the failed mutation replays its operation id.
        let operation_id = match self.attempt.as_ref() {
            Some(attempt) if attempt.mutation == mutation => attempt.operation_id.clone(),
            _ => new_operation_id(),
        };
        let actions = self.state.read(cx).work_outline_actions();
        let draft = if self.skip_log {
            None
        } else {
            self.direct_draft(&space_id, &mutation, &operation_id)
        };
        let (task, change, follow_key, expand_key) = match &mutation {
            DirectMutation::Move(plan) => (
                actions.move_work_item(plan.params(operation_id.clone())),
                LocalChange::Move {
                    work_item_id: plan.work_item_id.clone(),
                    destination_parent: plan.destination_parent.clone(),
                    placement: plan.placement.clone(),
                },
                row_key::item(&space_id, &plan.work_item_id),
                plan.destination_parent
                    .as_deref()
                    .map(|parent| row_key::item(&space_id, parent)),
            ),
            DirectMutation::AssignChat {
                chat_id,
                work_item_id,
            } => (
                actions.assign_chat(mast_proto::AssignChatToWorkItemParams {
                    chat_id: chat_id.clone(),
                    work_item_id: work_item_id.clone(),
                    operation_id: operation_id.clone(),
                }),
                LocalChange::AssignChat {
                    chat_id: chat_id.clone(),
                    work_item_id: work_item_id.clone(),
                },
                row_key::chat(chat_id),
                Some(row_key::item(&space_id, work_item_id)),
            ),
            DirectMutation::RemoveChat { chat_id } => (
                actions.remove_chat(mast_proto::RemoveChatFromWorkItemParams {
                    chat_id: chat_id.clone(),
                    operation_id: operation_id.clone(),
                }),
                LocalChange::RemoveChat {
                    chat_id: chat_id.clone(),
                },
                row_key::chat(chat_id),
                Some(row_key::unassociated(Some(&space_id))),
            ),
        };
        self.attempt = Some(MutationAttempt {
            mutation,
            operation_id: operation_id.clone(),
            state: AttemptState::InFlight,
        });
        if let Some(key) = expand_key.as_deref() {
            self.set_collapsed(key, false, cx);
        }
        self.show_optimistic(&space_id, &operation_id, change, Some(follow_key), cx);
        self.record_change(&space_id, draft, cx);
        tracing::info!(
            target: "work_outline_keys",
            %space_id, %operation_id,
            "send_direct: sent"
        );
        // A reply that never lands must not wedge restructuring: every later
        // press would queue forever and drops / picker moves would be ignored.
        let timeout_id = operation_id.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DIRECT_REPLY_TIMEOUT).await;
            this.update(cx, |this, cx| this.expire_direct_attempt(&timeout_id, cx))
                .ok();
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let ours = this
                    .attempt
                    .as_ref()
                    .is_some_and(|attempt| attempt.operation_id == operation_id);
                match result {
                    Ok(_) => {
                        tracing::info!(
                            target: "work_outline_keys",
                            %operation_id, ours,
                            queued = this.queued_restructure.len(),
                            "send_direct: reply ok"
                        );
                        if ours {
                            this.attempt = None;
                        }
                        this.confirm_change(&operation_id, cx);
                        this.acknowledge_optimistic(&operation_id, cx);
                        // Boundary no-ops send nothing: keep going until a
                        // press goes out or the queue is empty. A late reply
                        // to an expired attempt must not drain presses queued
                        // behind the one now in flight.
                        while !this.restructuring_busy() {
                            let Some(op) = this.queued_restructure.pop_front() else {
                                break;
                            };
                            if let Err(notice) = this.restructure_selection(op, cx) {
                                this.queued_restructure.clear();
                                this.show_notice(notice, cx);
                            }
                        }
                    }
                    Err(error) => {
                        tracing::info!(
                            target: "work_outline_keys",
                            %operation_id, ours,
                            code = ?error.code,
                            error = %error.message,
                            "send_direct: reply failed"
                        );
                        if ours {
                            if let Some(attempt) = this.attempt.as_mut() {
                                attempt.state = AttemptState::Failed;
                            }
                        }
                        if ours || !this.restructuring_busy() {
                            this.queued_restructure.clear();
                            this.follow = None;
                        }
                        this.forget_change(&operation_id, cx);
                        this.drop_optimistic(&operation_id, cx);
                        this.dismiss_move_toast_for(&operation_id, cx);
                        this.show_error(error.message.clone(), cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// The reply to `operation_id` did not land within
    /// [`DIRECT_REPLY_TIMEOUT`]: release the restructuring lock as a failure
    /// (a retry of the same move replays the operation id, so a late apply
    /// stays idempotent), roll the change back, and say so. A frame showing
    /// a late apply still wins.
    fn expire_direct_attempt(&mut self, operation_id: &str, cx: &mut Context<Self>) {
        let Some(attempt) = self.attempt.as_mut() else {
            return;
        };
        if attempt.operation_id != operation_id || attempt.state != AttemptState::InFlight {
            return;
        }
        attempt.state = AttemptState::Failed;
        tracing::warn!(
            target: "work_outline_keys",
            %operation_id,
            dropped_queued = self.queued_restructure.len(),
            "send_direct: no reply in time; released the restructuring lock"
        );
        self.queued_restructure.clear();
        self.follow = None;
        self.forget_change(operation_id, cx);
        self.drop_optimistic(operation_id, cx);
        self.dismiss_move_toast_for(operation_id, cx);
        self.show_error("The engine did not answer in time. Try the move again", cx);
    }

    // ----------------------------------------------------------------------
    // Drag and drop
    // ----------------------------------------------------------------------

    /// The drag payload of `row`, or `None` when the row cannot be dragged:
    /// archived work items, Chats without a project or whose row is gone.
    fn drag_payload(row: &OutlineRow) -> Option<OutlineDrag> {
        match &row.kind {
            OutlineRowKind::Item(item) if !item.archived => Some(OutlineDrag::Item {
                space_id: item.space_id.clone(),
                item_id: item.item_id.clone(),
                title: item.title.clone(),
            }),
            OutlineRowKind::Chat(chat) if !chat.dangling => {
                chat.chat
                    .space_id
                    .clone()
                    .map(|space_id| OutlineDrag::Chat {
                        chat_id: chat.chat.id.clone(),
                        space_id,
                        title: chat.chat.title.clone(),
                    })
            }
            OutlineRowKind::Space {
                space_id,
                label,
                hidden: false,
            } => Some(OutlineDrag::Space {
                space_id: space_id.clone(),
                title: label.clone(),
            }),
            _ => None,
        }
    }

    /// What dropping `drag` on work item `target_id` of `space_id` in `zone`
    /// sends, or `None` when the drop is refused (self / descendant targets,
    /// archived items, another Space, a move that changes nothing, a Chat
    /// already in that item, or a restructuring still landing).
    fn drop_mutation(
        &self,
        drag: &OutlineDrag,
        space_id: &str,
        target_id: &str,
        zone: DropZone,
    ) -> Option<DirectMutation> {
        if self.is_editing() || self.restructuring_busy() {
            return None;
        }
        let snapshot = self.snapshot(space_id)?;
        match drag {
            OutlineDrag::Item {
                space_id: from,
                item_id,
                ..
            } => {
                if from != space_id {
                    return None;
                }
                restructure::drop_plan(snapshot, item_id, target_id, zone).map(DirectMutation::Move)
            }
            OutlineDrag::Chat {
                chat_id,
                space_id: from,
                ..
            } => {
                let target = snapshot
                    .item(target_id)
                    .filter(|item| item.archived_at.is_none())?;
                if from != space_id || target.chat_ids.iter().any(|id| id == chat_id) {
                    return None;
                }
                Some(DirectMutation::AssignChat {
                    chat_id: chat_id.clone(),
                    work_item_id: target_id.to_string(),
                })
            }
            // Space roots only drop among Space roots; Linear issues create
            // through `linear_outline`.
            OutlineDrag::Space { .. }
            | OutlineDrag::LinearIssue(_)
            | OutlineDrag::PullRequest(_) => None,
        }
    }

    /// The drop indicator of an in-progress drag (tests).
    pub(crate) fn drop_hint(&self) -> Option<&DropHint> {
        self.drop_hint.as_ref()
    }

    /// The pointer moved during a drag; `bounds` are those of work item row
    /// `key`. Shows the indicator for an accepted drop on that row, none for
    /// a refused one, and clears it when the pointer leaves the row.
    fn drag_over_item(
        &mut self,
        key: &str,
        target: (&str, &str),
        drag: &OutlineDrag,
        position: gpui::Point<gpui::Pixels>,
        bounds: gpui::Bounds<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let hint = if bounds.contains(&position) {
            let zone = match drag {
                // A Chat (or a Linear issue) goes into the item wherever it
                // lands on the row.
                OutlineDrag::Chat { .. }
                | OutlineDrag::LinearIssue(_)
                | OutlineDrag::PullRequest(_) => DropZone::Into,
                OutlineDrag::Item { .. } | OutlineDrag::Space { .. } => restructure::drop_zone(
                    f32::from(position.y - bounds.top()),
                    f32::from(bounds.size.height),
                ),
            };
            let accepted = match drag {
                OutlineDrag::LinearIssue(drag) => {
                    self.accepts_linear_drop(drag, target.0, target.1)
                }
                OutlineDrag::PullRequest(drag) => self.accepts_pr_drop(drag, target.0, target.1),
                _ => self.drop_mutation(drag, target.0, target.1, zone).is_some(),
            };
            accepted.then(|| DropHint {
                key: key.to_string(),
                zone,
            })
        } else if self.drop_hint.as_ref().is_some_and(|hint| hint.key != key) {
            // Another row owns the indicator.
            return;
        } else {
            None
        };
        if self.drop_hint != hint {
            self.drop_hint = hint;
            cx.notify();
        }
    }

    /// `drag` was released over work item row `key`: send what its
    /// indicator showed (nothing when it showed none).
    fn drop_on_item(
        &mut self,
        key: &str,
        target: (&str, &str),
        drag: &OutlineDrag,
        cx: &mut Context<Self>,
    ) {
        let hint = self.drop_hint.take();
        cx.notify();
        let Some(hint) = hint.filter(|hint| hint.key == key) else {
            return;
        };
        if let OutlineDrag::LinearIssue(drag) = drag {
            self.drop_linear_issue(drag, target, cx);
            return;
        }
        if let OutlineDrag::PullRequest(drag) = drag {
            self.drop_pull_request(drag, target, cx);
            return;
        }
        if let Some(mutation) = self.drop_mutation(drag, target.0, target.1, hint.zone) {
            self.submit_direct(target.0.to_string(), mutation, cx);
        }
    }

    /// Move picker: put the item at the top (`true`) or bottom of the
    /// chosen parent. A changed request gets a fresh operation id.
    pub(crate) fn set_picker_place_first(&mut self, first: bool, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        if picker.submitting || picker.place_first == first {
            return;
        }
        picker.place_first = first;
        picker.operation_id = new_operation_id();
        cx.notify();
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
        let is_move = matches!(picker.purpose, PickerPurpose::Move { .. });
        match event.keystroke.key.as_str() {
            "down" => picker.active = (picker.active + 1).min(last),
            "up" => picker.active = picker.active.saturating_sub(1),
            "left" if is_move => self.set_picker_place_first(true, cx),
            "right" if is_move => self.set_picker_place_first(false, cx),
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

    /// First use: every loaded, available Space in the Outline has no live
    /// work item, nothing is being searched or created. The line and its two
    /// actions disappear as soon as an item exists (no permanent hints).
    pub(crate) fn shows_first_use(&self, _cx: &App) -> bool {
        if !self.inputs.options.query.is_empty()
            || matches!(
                self.editing.as_ref().map(|edit| &edit.kind),
                Some(EditKind::Create { .. })
            )
        {
            return false;
        }
        let mut loaded = self
            .inputs
            .spaces
            .iter()
            .filter_map(|space| space.load.snapshot())
            .filter(|snapshot| snapshot.available)
            .peekable();
        loaded.peek().is_some()
            && loaded.all(|snapshot| snapshot.items.iter().all(|item| item.archived_at.is_some()))
    }

    /// The first-use line: what work items are for, "Create work item"
    /// (with its live binding) and "Learn more" (the shortcut reference).
    fn render_first_use(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let keymap = crate::settings::current(cx).keymap;
        let create_keys = super::hints::hint_keys(
            &keymap,
            HintKey::Command(super::command::WorkOutlineCommand::CreateWorkItem),
        );
        let button = |id: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .flex_none()
                .flex()
                .items_center()
                .gap(px(5.0))
                .px(px(7.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .text_color(theme.text_muted)
                .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
        };
        div()
            .debug_selector(|| "outline-first-use".into())
            .flex_none()
            .mx(px(14.0))
            .my(px(6.0))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.0))
            .text_size(crate::typography::ui_rems(11.5))
            .text_color(theme.text_muted)
            .child("No work items yet \u{2014} group Chats and share instructions under one.")
            .child(
                button("outline-first-use-create")
                    .child("Create work item")
                    .when_some(create_keys, |el, keys| {
                        el.child(crate::popover::kbd_hint(theme, &keys))
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Err(notice) = this.begin_create(false, window, cx) {
                            this.show_notice(notice, cx);
                        }
                    })),
            )
            .child(
                button("outline-first-use-learn-more")
                    .child("Learn more")
                    .hint(
                        "Outline keys and commands",
                        HintKey::Outline(OutlineNavKey::ShowShortcuts),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(shell) = this.shell.as_ref().and_then(WeakEntity::upgrade) {
                            shell.update(cx, |shell, cx| shell.open_outline_shortcuts(cx));
                        }
                    })),
            )
            .into_any_element()
    }

    /// Commit the inline editor with `title` (tests).
    #[cfg(test)]
    pub(crate) fn commit_title_for_test(&mut self, title: &str, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |input, cx| input.set_text(title, cx));
        self.commit_edit(cx);
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
                    "enter" => {
                        this.commit_edit(cx);
                        // The edit row goes away: keep the keys (⌘Z included)
                        // working on the tree.
                        if !this.is_editing() {
                            window.focus(&this.focus, cx);
                        }
                    }
                    "escape" => this.cancel_edit_and_refocus(window, cx),
                    _ => return,
                }
                cx.stop_propagation();
                window.prevent_default();
            }))
            .child(self.editor.clone())
            .into_any_element()
    }

    /// A small "▤ 3" indicator: the item's live planning artifacts.
    fn artifact_count_chip(
        &self,
        key: &str,
        item: &model::ItemRow,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let (count, changed) = {
            let state = self.state.read(cx);
            let viewed = crate::settings::current(cx).artifact_last_viewed;
            let artifacts = state
                .artifacts()
                .for_work_item(&item.space_id, &item.item_id);
            let changed = artifacts.iter().find_map(|artifact| {
                crate::artifacts::questions_ui::list_marker(
                    &artifact.id,
                    artifact.current_revision,
                    &viewed,
                )
            });
            (artifacts.len(), changed)
        };
        if count == 0 {
            return None;
        }
        let selector = format!("outline-artifacts-{key}");
        let tip: SharedString = if count == 1 {
            "1 artifact".into()
        } else {
            format!("{count} artifacts").into()
        };
        Some(
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .flex_none()
                .flex()
                .items_center()
                .gap(px(2.0))
                .text_size(crate::typography::ui_rems(10.0))
                .text_color(theme.text_faint)
                .child(crate::icons::icon(crate::icons::DOCUMENT).size(px(10.0)))
                .child(SharedString::from(count.to_string()))
                .children(changed.map(|(from, to)| {
                    crate::artifacts::questions_ui::changed_dot(
                        format!("outline-artifacts-changed-{key}"),
                        from,
                        to,
                        theme,
                    )
                }))
                .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
                .into_any_element(),
        )
    }

    /// The "Instructions changed" chip of a Chat whose work item's
    /// instructions moved on since it started.
    fn drift_chip(
        &self,
        key: &str,
        chat: &ChatRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let drift = chat
            .work_item
            .as_ref()
            .and_then(|(space, _)| self.snapshot(space))
            .and_then(|snapshot| snapshot.chat_instruction_state(&chat.chat.id))
            .is_some_and(|state| state.drift);
        (drift && !chat.dangling).then(|| {
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
                .hint(chips::INSTRUCTION_DRIFT_TOOLTIP, HintKey::None)
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if let Some(shell) = this.shell.as_ref().and_then(WeakEntity::upgrade) {
                        shell.update(cx, |shell, cx| shell.open_instruction_compare(&chat_id, cx));
                    }
                }))
                .into_any_element()
        })
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
        let in_group = self.multi.contains(&key);
        let highlighted = self.details_key.as_deref() == Some(key.as_str())
            || row
                .open_chat()
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
                .debug_selector({
                    let selector = format!("outline-disclosure-{key}");
                    move || selector.clone()
                })
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
                .hint(
                    if collapsed { "Expand" } else { "Collapse" },
                    HintKey::Outline(if collapsed {
                        OutlineNavKey::Expand
                    } else {
                        OutlineNavKey::Collapse
                    }),
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
            OutlineRowKind::Space {
                label,
                space_id,
                hidden: true,
            } => {
                let unhide = space_id.clone();
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
                            .text_color(theme.text_muted)
                            .child(SharedString::from(label.clone())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(crate::typography::ui_rems(10.0))
                            .text_color(theme.text_faint)
                            .child("Hidden"),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("outline-space-unhide-{key}")))
                            .debug_selector({
                                let selector = format!("outline-space-unhide-{space_id}");
                                move || selector.clone()
                            })
                            .flex_none()
                            .px(px(4.0))
                            .rounded(px(3.0))
                            .cursor_pointer()
                            .text_size(crate::typography::ui_rems(10.0))
                            .text_color(theme.text_muted)
                            .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
                            .child("Unhide")
                            .hint("Show this project in the Outline again", HintKey::None)
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation()
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if !this.is_editing() {
                                    this.set_space_hidden(&unhide, false, cx);
                                }
                            })),
                    )
                    .into_any_element()
            }
            OutlineRowKind::Space {
                label, space_id, ..
            } => {
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
                    let artifacts = self.artifact_count_chip(&key, item, theme, cx);
                    // Rough widths of the chips that share the row with the
                    // PR icon cluster (they size to their content).
                    let other_width = if item.pending_proposals > 0 {
                        56.0
                    } else {
                        0.0
                    } + if artifacts.is_some() { 34.0 } else { 0.0 }
                        + if item.execution_exceptions > 0 {
                            28.0
                        } else {
                            0.0
                        };
                    let external =
                        self.external_chips(&key, item, row.depth, other_width, theme, cx);
                    let compact_chat = item.compact_chat.as_ref().map(|chat| {
                        let drift = self.drift_chip(&key, chat, theme, cx);
                        compact_chat_segment(&key, chat, drift, theme)
                    });
                    // A compact row's status glyph opens the item's details
                    // (the rest of the row opens the Chat).
                    let press_glyph = cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.glyph_press = true;
                    });
                    let open_details = item.compact_chat.as_ref().map(|_| {
                        let space_id = item.space_id.clone();
                        let item_id = item.item_id.clone();
                        let open = cx.listener(move |this, _: &gpui::ClickEvent, _, cx| {
                            cx.stop_propagation();
                            if !this.is_editing() {
                                this.open_item_details(space_id.clone(), item_id.clone(), cx);
                            }
                        });
                        (
                            Box::new(press_glyph)
                                as Box<dyn Fn(&gpui::MouseDownEvent, &mut Window, &mut App)>,
                            Box::new(open) as Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App)>,
                        )
                    });
                    item_body(
                        &key,
                        item,
                        compact_chat,
                        open_details,
                        linear_outline::join_chips(
                            linear_outline::join_chips(proposals, artifacts),
                            external.chips,
                        ),
                        external.pr_slots,
                        theme,
                    )
                }
            }
            OutlineRowKind::Chat(chat) => {
                let chip = self.drift_chip(&key, chat, theme, cx);
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
                    .hint(
                        "Start a Chat in this work item",
                        HintKey::Outline(OutlineNavKey::StartChat),
                    )
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
                .is_some_and(|chat| chat.chat.archived || chat.dangling)
            || matches!(row.kind, OutlineRowKind::Space { hidden: true, .. });
        // Drag and drop: rows drag while nothing is being edited; live work
        // items are the drop targets. The indicator is static (no motion).
        let drag = (!renaming && self.editing.is_none())
            .then(|| Self::drag_payload(row))
            .flatten();
        let drop_target = row
            .item()
            .filter(|item| !item.archived)
            .map(|item| (item.space_id.clone(), item.item_id.clone()));
        // Space roots take Space-root drags (reordering).
        let space_drop_target = match &row.kind {
            OutlineRowKind::Space {
                space_id,
                hidden: false,
                ..
            } => Some(space_id.clone()),
            _ => None,
        };
        let drop_zone = self
            .drop_hint
            .as_ref()
            .filter(|hint| hint.key == key)
            .map(|hint| hint.zone);
        let drop_line = drop_zone
            .filter(|zone| *zone != DropZone::Into)
            .map(|zone| self.drop_line(ix, zone, theme));
        let drop_label = drop_zone.and_then(|_| self.drop_label_element(theme));
        let guides = self.guide_layer(ix, compact, theme, cx);
        let flash = self.move_flash_overlay(&key, theme);
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
            .when((selected || in_group) && focused, |el| {
                el.bg(theme.element_active)
            })
            .when((selected || in_group) && !focused, |el| {
                el.bg(theme.element_hover)
            })
            .when(!(selected || in_group), |el| {
                el.hover(|el| el.bg(theme.element_hover.opacity(0.6)))
            })
            .when(dimmed, |el| el.opacity(0.6))
            .relative()
            .when(drop_zone == Some(DropZone::Into), |el| {
                el.bg(theme.accent_wash).border_color(theme.accent)
            })
            .cursor_pointer()
            .when_some(drag, |el, drag| {
                el.on_drag(drag, |drag, _, _, cx| {
                    cx.stop_propagation();
                    let title = SharedString::from(drag.title().to_string());
                    cx.new(|_| OutlineDragGhost { title })
                })
            })
            .when_some(drop_target, |el, (space_id, item_id)| {
                let move_key = key.clone();
                let move_target = (space_id.clone(), item_id.clone());
                let drop_key = key.clone();
                el.on_drag_move::<OutlineDrag>(cx.listener(
                    move |this, event: &gpui::DragMoveEvent<OutlineDrag>, _, cx| {
                        let drag = event.drag(cx).clone();
                        this.drag_over_item(
                            &move_key,
                            (&move_target.0, &move_target.1),
                            &drag,
                            event.event.position,
                            event.bounds,
                            cx,
                        );
                    },
                ))
                .on_drop::<OutlineDrag>(cx.listener(
                    move |this, drag: &OutlineDrag, _, cx| {
                        this.drop_on_item(&drop_key, (&space_id, &item_id), drag, cx);
                    },
                ))
            })
            .when_some(space_drop_target, |el, space_id| {
                let move_key = key.clone();
                let move_target = space_id.clone();
                let drop_key = key.clone();
                el.on_drag_move::<OutlineDrag>(cx.listener(
                    move |this, event: &gpui::DragMoveEvent<OutlineDrag>, _, cx| {
                        let drag = event.drag(cx).clone();
                        this.drag_over_space(
                            &move_key,
                            &move_target,
                            &drag,
                            event.event.position,
                            event.bounds,
                            cx,
                        );
                    },
                ))
                .on_drop::<OutlineDrag>(cx.listener(
                    move |this, drag: &OutlineDrag, _, cx| {
                        this.drop_on_space(&drop_key, &space_id, drag, cx);
                    },
                ))
            })
            .on_mouse_down(
                gpui::MouseButton::Right,
                context_menu::row_listener(key.clone(), cx),
            )
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.glyph_press = false;
                    if !this.is_editing() {
                        window.focus(&this.focus, cx);
                    }
                }),
            )
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    let kind = row_kind_name(&open_row.kind);
                    if std::mem::take(&mut this.glyph_press) {
                        return;
                    }
                    if this.is_editing() {
                        tracing::info!(
                            target: "work_outline_keys",
                            row = %click_key, kind,
                            inline_editor = this.editing.is_some(),
                            picker = this.picker.is_some(),
                            "row click ignored: editing"
                        );
                        return;
                    }
                    let modifiers = event.modifiers();
                    if event.click_count() < 2 && open_row.item().is_some() {
                        if modifiers.secondary() {
                            this.toggle_multi(&click_key, cx);
                            return;
                        }
                        if modifiers.shift {
                            this.extend_multi_to(&click_key, cx);
                            return;
                        }
                    }
                    this.clear_multi(cx);
                    let moved = this.selected.as_deref() != Some(click_key.as_str());
                    this.select(&click_key, cx);
                    let opens_details = event.click_count() >= 2;
                    if opens_details {
                        this.open_row(&open_row, cx);
                    } else if moved {
                        cx.emit(OutlineEvent::SelectionMoved);
                    }
                    tracing::info!(
                        target: "work_outline_keys",
                        row = %click_key, kind,
                        clicks = event.click_count(),
                        opens_details = opens_details && kind == "item",
                        selected = ?this.selected.as_deref(),
                        outline_focused = this.focus.is_focused(window),
                        outline_contains_focus = this.focus.contains_focused(window, cx),
                        context_stack = ?window.context_stack(),
                        "row click"
                    );
                }),
            )
            .children(flash)
            .children(guides)
            .child(disclosure)
            .child(body)
            .children(start_chat)
            .children(drop_line)
            .children(drop_label)
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
            .hint(tip, HintKey::None)
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
        // Move only: where under the chosen parent ("Top" / "Bottom"; Left /
        // Right switch it from the keyboard).
        let placement = matches!(picker.purpose, PickerPurpose::Move { .. }).then(|| {
            let chip = |label: &'static str, first: bool, cx: &mut Context<Self>| {
                let active = picker.place_first == first;
                let selector = format!("outline-picker-place-{}", label.to_lowercase());
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .px(px(6.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .when(active, |el| {
                        el.bg(theme.element_active).text_color(theme.text)
                    })
                    .when(!active, |el| el.hover(|el| el.bg(theme.element_hover)))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_picker_place_first(first, cx);
                    }))
            };
            div()
                .id("outline-picker-placement")
                .flex_none()
                .flex()
                .items_center()
                .gap(px(2.0))
                .child("Place at")
                .child(chip("Top", true, cx))
                .child(chip("Bottom", false, cx))
                .tooltip(|_, cx| {
                    cx.new(|_| {
                        TextTooltip(
                            "Where the item goes among the new parent\u{2019}s children \
                             (\u{2190} / \u{2192})"
                                .into(),
                        )
                    })
                    .into()
                })
        });
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
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(picker.title.clone())),
                        )
                        .children(placement)
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

/// `pr_slots` is the PR icon cluster of a PR row: it carries the attention
/// indicator, the descendant totals and the chat count in its slots and
/// tooltips, so the row leaves those (and the redundant references icon) out.
/// Press and click handlers of a compact row's status glyph.
type GlyphHandlers = (
    Box<dyn Fn(&gpui::MouseDownEvent, &mut Window, &mut App)>,
    Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App)>,
);

fn item_body(
    key: &str,
    item: &ItemRow,
    compact_chat: Option<AnyElement>,
    open_details: Option<GlyphHandlers>,
    proposals: Option<AnyElement>,
    pr_slots: Option<AnyElement>,
    theme: &Theme,
) -> AnyElement {
    let compact_pr = pr_slots.is_some();
    let totals = (!item.archived && !compact_pr)
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
                .debug_selector({
                    let selector = format!("outline-status-{key}");
                    move || selector.clone()
                })
                .flex_none()
                .w(px(12.0))
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(color)
                .child(glyph)
                .map(|el| match open_details {
                    Some((press, open)) => el
                        .cursor_pointer()
                        .rounded(px(3.0))
                        .hover(|el| el.bg(theme.element_hover))
                        .hint("Open details", HintKey::Outline(OutlineNavKey::OpenDetails))
                        .on_mouse_down(gpui::MouseButton::Left, press)
                        .on_click(open),
                    None => {
                        el.tooltip(move |_, cx| cx.new(|_| TextTooltip(status_tip.clone())).into())
                    }
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme.text)
                .child(SharedString::from(item.title.clone())),
        )
        .children(compact_chat)
        .when_some(
            chips::attention_indicator(
                format!("outline-attention-{key}"),
                item.needs_user_action(),
                item.unresolved_actions,
                theme,
            )
            .filter(|_| !compact_pr),
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
        .when(
            item.chat_count > 0 && !compact_pr && item.compact_chat.is_none(),
            |el| {
                el.child(
                    div()
                        .id(SharedString::from(format!("outline-chats-{key}")))
                        .debug_selector({
                            let selector = format!("outline-chats-{key}");
                            move || selector.clone()
                        })
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
            },
        )
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
        .when(item.has_references && !compact_pr, |el| {
            el.child(
                div()
                    .id(SharedString::from(format!("outline-refs-{key}")))
                    .debug_selector({
                        let selector = format!("outline-refs-{key}");
                        move || selector.clone()
                    })
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
        .children(pr_slots)
        .into_any_element()
}

/// A compact item row's Chat: live-state dot, instruction-drift chip and
/// harness badge, in the same styles as a Chat row. The title, status, PR /
/// Linear icons and attention chips are the item's, so nothing repeats.
fn compact_chat_segment(
    key: &str,
    row: &ChatRow,
    drift: Option<AnyElement>,
    theme: &Theme,
) -> AnyElement {
    let chat = &row.chat;
    let dot = crate::shell::spaces::status_dot_color(chat.status, theme);
    let tip: SharedString = format!("Chat: {}", chat.title).into();
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(
            div()
                .id(SharedString::from(format!("outline-chat-dot-{key}")))
                .debug_selector({
                    let selector = format!("outline-chat-dot-{key}");
                    move || selector.clone()
                })
                .flex_none()
                .size(px(6.0))
                .rounded_full()
                .bg(dot)
                .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into()),
        )
        .when_some(drift, |el, drift| el.child(drift))
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
        // A drag released outside a row (or cancelled) leaves no indicator.
        if self.drop_hint.is_some() && !cx.has_active_drag() {
            self.drop_hint = None;
        }
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
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        context_menu::space_root_listener(space.id.clone(), cx),
                    )
            }),
            _ => None,
        };
        let notice = self.notice.clone();
        self.open_requested_template_flow(window, cx);
        let move_toast = self.render_move_toast(&theme, cx);
        let template_toast = self.render_template_toast(&theme, cx);
        if self.focus_logs.is_none() {
            self.focus_logs = Some([
                cx.on_focus_in(&self.focus, window, |this, window, _| {
                    tracing::info!(
                        target: "work_outline_keys",
                        outline_focused = this.focus.is_focused(window),
                        editing = this.is_editing(),
                        "outline gained focus"
                    );
                }),
                cx.on_focus_out(&self.focus, window, |this, _, window, _| {
                    tracing::info!(
                        target: "work_outline_keys",
                        editing = this.is_editing(),
                        context_stack = ?window.context_stack(),
                        "outline lost focus"
                    );
                }),
            ]);
        }
        let mut first_use = self
            .shows_first_use(cx)
            .then(|| self.render_first_use(&theme, cx));
        let list: AnyElement = if count == 0 && first_use.is_some() {
            // First use: the compact line takes the empty tree's place.
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .children(first_use.take())
                .into_any_element()
        } else if count == 0 {
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
            .relative()
            .debug_selector(|| "work-outline".into())
            .track_focus(&self.focus)
            .when(!editing, |el| el.key_context(KEY_CONTEXT))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                // Esc closes the move toast; every other key goes on to the
                // tree's bindings (focus never leaves it).
                if event.keystroke.key == "escape"
                    && !this.is_editing()
                    && this.clear_multi(cx)
                {
                    cx.stop_propagation();
                    return;
                }
                if event.keystroke.key == "escape"
                    && this.move_toast_text().is_some()
                    && !this.is_editing()
                {
                    this.dismiss_move_toast(cx);
                    cx.stop_propagation();
                }
            }))
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
            .on_action(
                cx.listener(|this, _: &OpenDetailsSelected, _, cx| this.open_details_selected(cx)),
            )
            .on_action(
                cx.listener(|this, _: &StartChatSelected, _, cx| this.start_chat_selected(cx)),
            )
            .on_action(cx.listener(|this, _: &RenameSelected, window, cx| {
                if this.is_editing() {
                    cx.propagate();
                    return;
                }
                // A Chat row renames through the Shell's Chat rename dialog,
                // the same flow as the sidebar's Rename.
                if let Some(chat_id) = this.selected_chat() {
                    cx.emit(OutlineEvent::RenameChat(chat_id));
                    return;
                }
                if this.selected_row().is_some_and(|row| row.chat().is_some()) {
                    this.show_notice("This chat no longer exists on any device", cx);
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
            .on_action(cx.listener(|this, _: &UndoChange, _, cx| this.undo_change(cx)))
            .on_action(
                cx.listener(|this, _: &GroupSelected, window, cx| this.group_selected(window, cx)),
            )
            .on_action(cx.listener(|this, _: &RedoChange, _, cx| this.redo_change(cx)))
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
            .when_some(move_toast, |el, toast| el.child(toast))
            .when_some(template_toast, |el, toast| el.child(toast))
            .when_some(self.error_notice.clone(), |el, (title, detail, copy)| {
                let outline = cx.entity();
                el.child(
                    div()
                        .debug_selector(|| "outline-error".into())
                        .flex_none()
                        .mx(px(14.0))
                        .mb(px(6.0))
                        .child(
                            crate::error_notice::ErrorNotice::new(
                                "outline-error-notice",
                                &theme,
                                title,
                                detail,
                            )
                            .with_copy(copy)
                            .on_dismiss(move |_, cx| {
                                outline.update(cx, |this, cx| this.dismiss_error(cx))
                            }),
                        ),
                )
            })
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
            // With only Chats in the tree, the first-use line sits above them.
            .children(first_use)
            .child(list)
            .child(self.width_probe(cx))
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
