//! Work Item Details: the Overview-owned right-side surface (spec
//! "Apply progressive disclosure"; acceptance 26).
//!
//! It is hosted in the Overview's right slot *instead of* the chat panel /
//! subagent peek while open and never touches `chat_panel_open`, the Shell's
//! selection, or the Chat-keyed transcript/diff pane — closing it simply
//! lets the previous panel render again. Description and external
//! references stay collapsed until opened. Every edit goes through
//! `AppState::work_outline_actions()`; archive and draft discard confirm
//! through the Shell-owned modal (`Shell::open_work_outline_confirm`);
//! references open through `Shell::open_work_item_reference`.
//!
//! Milestone 2 adds the collapsed **Instructions** and **Effective
//! instructions** sections, the inheritance toggle, and a Space-root variant
//! (`item_id` empty) that edits Space-root instructions
//! (`details_instructions.rs`). Instruction edits save only on an explicit
//! Save; Cancel discards the draft; closing, switching items, or Escape with
//! an unsaved draft asks "Keep Editing / Discard" first.
use std::collections::HashSet;
use std::sync::Arc;

use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Subscription,
    WeakEntity, Window, div, prelude::*, px,
};
use zeron_proto::{
    ArchiveWorkItemParams, AssignChatToWorkItemParams, CreateWorkItemExternalReferenceParams,
    FieldPatch, RemoveChatFromWorkItemParams, RemoveWorkItemExternalReferenceParams,
    RestoreWorkItemParams, UpdateWorkItemParams, WorkItemChanges,
};

use super::actions::{ActionTask, new_operation_id};
use super::chips::{self, TextTooltip};
use super::command::{
    WorkOutlineArchiveRequest, WorkOutlineConfirmRequest, WorkOutlineParentChoice,
};
use super::model::{self, OutlineChat};
use super::store::{WorkItemEffectiveStatus, WorkItemStatus, WorkItemView, WorkOutlineSnapshot};
use crate::composer::skill_completion::{SkillCompletion, SkillInsertion, SkillSource};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::shell::Shell;
use crate::state::AppState;
use crate::theme::Theme;

#[path = "details_instructions.rs"]
mod instructions_section;
pub(crate) use instructions_section::InstructionDraft;

#[path = "details_execution.rs"]
mod execution_section;

#[path = "details_user_actions.rs"]
mod user_actions_section;
pub(crate) use user_actions_section::ProposalReview;

/// `item_id` of the Space-root variant of the details surface.
pub(crate) const SPACE_ROOT_ITEM_ID: &str = "";

/// Fixed width of the details surface.
pub(crate) const DETAILS_PANEL_W: f32 = 380.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DetailsEvent {
    /// The surface should close (after any discard confirmation).
    Close,
    OpenChat(String),
    OpenItem {
        space_id: String,
        item_id: String,
    },
    /// Start a Chat in this work item (Milestone 3).
    StartChat {
        space_id: String,
        item_id: String,
    },
    /// Associate (`Some`) or disassociate (`None`) a Chat. The Outline sends
    /// it like its own Chat moves: applied at once, with its Undo toast when
    /// the Chat's effective instructions change.
    ReassociateChat {
        space_id: String,
        chat_id: String,
        work_item_id: Option<String>,
    },
}

/// A parsed external-reference URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedReference {
    pub provider: String,
    pub reference_type: String,
    pub external_id: String,
    pub url: String,
}

/// Recognize GitHub PRs/issues and Linear issues; any other http(s) URL is
/// a plain web link. `None` for non-URLs.
pub(crate) fn parse_reference(raw: &str) -> Option<ParsedReference> {
    let url = raw.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    if rest.is_empty() || url.chars().count() > zeron_proto::limits::MAX_REFERENCE_URL_CHARS {
        return None;
    }
    let parts: Vec<&str> = rest
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .split('/')
        .collect();
    let make = |provider: &str, kind: &str, id: String| ParsedReference {
        provider: provider.to_string(),
        reference_type: kind.to_string(),
        external_id: id,
        url: url.to_string(),
    };
    match parts.as_slice() {
        [
            "github.com",
            owner,
            repo,
            kind @ ("pull" | "issues"),
            number,
            ..,
        ] if number.chars().all(|c| c.is_ascii_digit()) => {
            let reference_type = if *kind == "pull" {
                "pullRequest"
            } else {
                "issue"
            };
            Some(make(
                "github",
                reference_type,
                format!("{owner}/{repo}#{number}"),
            ))
        }
        ["linear.app", _, "issue", id, ..] => Some(make("linear", "issue", id.to_uppercase())),
        _ => Some(make("web", "link", url.to_string())),
    }
}

pub(crate) struct WorkItemDetails {
    state: Entity<AppState>,
    shell: WeakEntity<Shell>,
    pub(crate) space_id: String,
    pub(crate) item_id: String,
    snapshot: Option<Arc<WorkOutlineSnapshot>>,
    chats: Arc<Vec<OutlineChat>>,
    focus: FocusHandle,
    title_input: Entity<ComposerInput>,
    editing_title: bool,
    description_open: bool,
    description_input: Entity<ComposerInput>,
    editing_description: bool,
    references_open: bool,
    reference_input: Entity<ComposerInput>,
    adding_reference: bool,
    associate_open: bool,
    /// In-flight mutation keys (one per field/target), so a repeat click
    /// cannot double-submit.
    in_flight: HashSet<String>,
    error: Option<String>,
    /// Collapsed until opened (Space-root variant: open).
    pub(crate) instructions_open: bool,
    /// The one instruction being written or edited (explicit Save/Cancel).
    pub(crate) instruction_draft: Option<InstructionDraft>,
    instruction_input: Entity<ComposerInput>,
    /// `/` skill autocomplete on the instruction editor (plain
    /// "`name` skill" text, listed for this item's execution root).
    instruction_skills: Entity<SkillCompletion>,
    /// Instructions whose Chat excerpt is expanded.
    excerpts_open: HashSet<String>,
    instructions_error: Option<String>,
    /// Effective Instructions: collapsed until opened.
    pub(crate) effective_open: bool,
    /// Execution: collapsed until opened.
    pub(crate) execution_open: bool,
    /// The open execution editor (explicit Save/Cancel).
    pub(crate) execution_draft: Option<super::execution::ExecutionDraft>,
    execution_root_input: Entity<ComposerInput>,
    /// One expected-branch input per draft binding, in binding order.
    execution_ref_inputs: Vec<Entity<ComposerInput>>,
    /// Reused across retries of the same reset.
    execution_reset_op: super::actions::OperationSlot,
    pub(crate) execution_validation: super::execution::ValidationState,
    pub(crate) execution_error: Option<String>,
    /// Milestone 4: the Space-level Required Actions list (this item's
    /// subtree, or the whole Space on the Space-root variant).
    required_actions: Entity<super::required_actions_view::RequiredActionsList>,
    /// `None` = open while something is open.
    pub(crate) required_actions_open: Option<bool>,
    /// Proposed instructions: collapsed until opened.
    pub(crate) proposals_open: bool,
    pub(crate) proposal_review: Option<ProposalReview>,
    proposal_input: Entity<ComposerInput>,
    /// `/` skill autocomplete on the proposal review text.
    proposal_skills: Entity<SkillCompletion>,
    reason_input: Entity<ComposerInput>,
    pub(crate) proposal_history_open: bool,
    pub(crate) proposals_error: Option<String>,
    _subscriptions: Vec<Subscription>,
    /// Tests run without a Shell: the discard confirmation lands here.
    #[cfg(test)]
    pub(crate) captured_confirm: Option<WorkOutlineConfirmRequest>,
    /// Tests run without a Shell: the drift confirmation lands here.
    #[cfg(test)]
    pub(crate) captured_drift: Option<super::command::WorkOutlineDriftRequest>,
    /// Tests run without a Shell: the folder pick lands here.
    #[cfg(test)]
    pub(crate) captured_folder_pick: Option<super::execution::ExecutionFolderPick>,
}

impl EventEmitter<DetailsEvent> for WorkItemDetails {}

impl WorkItemDetails {
    pub(crate) fn new(
        state: Entity<AppState>,
        shell: WeakEntity<Shell>,
        space_id: String,
        item_id: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let title_input = cx.new(|cx| {
            ComposerInput::with_context("Title", crate::composer::PALETTE_SEARCH_CONTEXT, cx)
                .with_single_line()
                .with_text_metrics(13.0, 19.0)
        });
        // The generic composer context: Enter submits, Shift+Enter breaks a line.
        let description_input = cx.new(|cx| {
            ComposerInput::new("Description", cx)
                .with_text_metrics(12.5, 18.0)
                .with_auto_grow()
        });
        let reference_input = cx.new(|cx| {
            ComposerInput::with_context(
                "https://github.com/… or https://linear.app/…",
                crate::composer::PALETTE_SEARCH_CONTEXT,
                cx,
            )
            .with_single_line()
            .with_text_metrics(12.0, 18.0)
        });
        // Generic composer context: Enter saves (an explicit keystroke, like
        // the description editor), Shift+Enter breaks a line. Blur never saves.
        let instruction_input = cx.new(|cx| {
            ComposerInput::new("Instruction", cx)
                .with_text_metrics(12.5, 18.0)
                .with_auto_grow()
        });
        let instruction_skills = cx.new(|cx| {
            SkillCompletion::new(
                state.clone(),
                instruction_input.clone(),
                SkillInsertion::InstructionText,
                cx,
            )
        });
        let execution_root_input = cx.new(|cx| {
            ComposerInput::with_context(
                "/absolute/path/to/folder",
                crate::composer::PALETTE_SEARCH_CONTEXT,
                cx,
            )
            .with_single_line()
            .with_text_metrics(12.0, 18.0)
        });
        let is_space_root = item_id == SPACE_ROOT_ITEM_ID;
        // Generic composer context (Shift+Enter breaks a line); Approve is
        // explicit (button only), blur never approves.
        let proposal_input = cx.new(|cx| {
            ComposerInput::new("Proposed instruction", cx)
                .with_text_metrics(12.5, 18.0)
                .with_auto_grow()
        });
        let proposal_skills = cx.new(|cx| {
            SkillCompletion::new(
                state.clone(),
                proposal_input.clone(),
                SkillInsertion::InstructionText,
                cx,
            )
        });
        let reason_input = cx.new(|cx| {
            ComposerInput::with_context(
                "Optional reason",
                crate::composer::PALETTE_SEARCH_CONTEXT,
                cx,
            )
            .with_single_line()
            .with_text_metrics(12.0, 18.0)
        });
        let (required_actions, required_actions_events) =
            Self::new_required_actions(&state, &space_id, &item_id, cx);
        let subscriptions = vec![
            required_actions_events,
            cx.subscribe(&reason_input, |this: &mut Self, _, event, cx| {
                if matches!(event, ComposerInputEvent::Submitted) {
                    this.reject_proposal(cx);
                }
            }),
            cx.subscribe(&execution_root_input, |this: &mut Self, _, event, cx| {
                if matches!(event, ComposerInputEvent::Submitted) {
                    this.save_execution(cx);
                }
            }),
            cx.subscribe(&instruction_input, |this: &mut Self, _, event, cx| {
                if matches!(event, ComposerInputEvent::Submitted) {
                    this.save_instruction(cx);
                }
            }),
            cx.subscribe(&title_input, |this: &mut Self, _, event, cx| {
                if matches!(event, ComposerInputEvent::Submitted) {
                    this.save_title(cx);
                }
            }),
            cx.subscribe(&description_input, |this: &mut Self, _, event, cx| {
                if matches!(event, ComposerInputEvent::Submitted) {
                    this.save_description(cx);
                }
            }),
            cx.subscribe(&reference_input, |this: &mut Self, _, event, cx| {
                if matches!(event, ComposerInputEvent::Submitted) {
                    this.add_reference(cx);
                }
            }),
        ];
        Self {
            state,
            shell,
            space_id,
            item_id,
            snapshot: None,
            chats: Arc::default(),
            focus: cx.focus_handle(),
            title_input,
            editing_title: false,
            description_open: false,
            description_input,
            editing_description: false,
            references_open: false,
            reference_input,
            adding_reference: false,
            associate_open: false,
            in_flight: HashSet::new(),
            error: None,
            instructions_open: is_space_root,
            instruction_draft: None,
            instruction_input,
            instruction_skills,
            excerpts_open: HashSet::new(),
            instructions_error: None,
            effective_open: false,
            execution_open: false,
            execution_draft: None,
            execution_root_input,
            execution_ref_inputs: Vec::new(),
            execution_reset_op: Default::default(),
            execution_validation: Default::default(),
            execution_error: None,
            required_actions,
            required_actions_open: None,
            proposals_open: false,
            proposal_review: None,
            proposal_input,
            proposal_skills,
            reason_input,
            proposal_history_open: false,
            proposals_error: None,
            _subscriptions: subscriptions,
            #[cfg(test)]
            captured_confirm: None,
            #[cfg(test)]
            captured_drift: None,
            #[cfg(test)]
            captured_folder_pick: None,
        }
    }

    /// The Space-root variant (edits Space-root instructions only).
    pub(crate) fn is_space_root(&self) -> bool {
        self.item_id == SPACE_ROOT_ITEM_ID
    }

    /// Fed by the Overview every frame it renders the surface.
    pub(crate) fn set_data(
        &mut self,
        snapshot: Option<Arc<WorkOutlineSnapshot>>,
        chats: Arc<Vec<OutlineChat>>,
    ) {
        self.snapshot = snapshot;
        self.chats = chats;
    }

    pub(crate) fn details_focus(&self) -> &FocusHandle {
        &self.focus
    }

    fn item(&self) -> Option<&WorkItemView> {
        self.snapshot.as_ref()?.item(&self.item_id)
    }

    /// A draft that closing would lose.
    pub(crate) fn has_unsaved_draft(&self, cx: &gpui::App) -> bool {
        if self.instruction_draft_dirty(cx)
            || self.execution_draft_dirty(cx)
            || self.proposal_review_dirty(cx)
        {
            return true;
        }
        let Some(item) = self.item() else {
            return false;
        };
        (self.editing_title && self.title_input.read(cx).text().trim() != item.title.trim())
            || (self.editing_description
                && self.description_input.read(cx).text().trim()
                    != item.description.as_deref().unwrap_or("").trim())
            || (self.adding_reference && !self.reference_input.read(cx).text().trim().is_empty())
    }

    /// Close, asking first when a draft would be lost.
    pub(crate) fn request_close(&mut self, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        self.confirm_discard(cx, move |cx| {
            weak.update(cx, |_, cx| cx.emit(DetailsEvent::Close)).ok();
        });
    }

    /// Run `then` once no draft would be lost: immediately when there is
    /// none, otherwise only after the user picks Discard in the Shell's
    /// "Keep Editing / Discard" confirmation (the drafts are dropped first).
    /// Keep Editing, or no Shell to ask through, leaves the draft and does
    /// nothing. Every path that replaces or closes this surface goes here.
    pub(crate) fn confirm_discard(
        &mut self,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut gpui::App) + 'static,
    ) {
        if !self.has_unsaved_draft(cx) {
            // Deferred: callers are mid-update of this entity and the caller's.
            cx.defer(then);
            return;
        }
        let weak = cx.entity().downgrade();
        let request = WorkOutlineConfirmRequest {
            title: "Discard unsaved changes?".into(),
            body: if self.instruction_draft_dirty(cx) {
                "Your instruction edits have not been saved.".into()
            } else if self.execution_draft_dirty(cx) {
                "Your execution configuration edits have not been saved.".into()
            } else if self.proposal_review_dirty(cx) {
                "Your edits to the proposed instruction have not been approved.".into()
            } else {
                "Your edits to this work item have not been saved.".into()
            },
            confirm_label: "Discard".into(),
            cancel_label: "Keep Editing".into(),
            destructive: true,
            on_decision: Box::new(move |confirmed, _window, cx| {
                if !confirmed {
                    return;
                }
                let discarded = weak
                    .update(cx, |this, cx| {
                        this.discard_drafts();
                        cx.notify();
                    })
                    .is_ok();
                if discarded {
                    then(cx);
                }
            }),
        };
        if let Some(shell) = self.shell.upgrade() {
            shell.update(cx, |shell, cx| shell.open_work_outline_confirm(request, cx));
            return;
        }
        #[cfg(test)]
        {
            self.captured_confirm = Some(request);
        }
        #[cfg(not(test))]
        {
            // No Shell (fixtures): keep the draft rather than drop it.
            drop(request);
            self.error = Some("Save or cancel your edits first".into());
            cx.notify();
        }
    }

    /// Start a title edit with `text` (tests drive drafts through this).
    #[cfg(test)]
    pub(crate) fn begin_title_draft(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_title_edit(window, cx);
        self.title_input
            .update(cx, |input, cx| input.set_text(text, cx));
    }

    /// Open the description editor (tests).
    #[cfg(test)]
    pub(crate) fn begin_description_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.description_open = true;
        self.begin_description_edit(window, cx);
    }

    /// `(input, skill autocomplete)` of the multi-line editors (tests):
    /// `"instruction"`, `"proposal"`, or `"description"` (no autocomplete).
    #[cfg(test)]
    pub(crate) fn editor(
        &self,
        which: &str,
    ) -> (Entity<ComposerInput>, Option<Entity<SkillCompletion>>) {
        match which {
            "instruction" => (
                self.instruction_input.clone(),
                Some(self.instruction_skills.clone()),
            ),
            "proposal" => (
                self.proposal_input.clone(),
                Some(self.proposal_skills.clone()),
            ),
            "description" => (self.description_input.clone(), None),
            other => panic!("no {other} editor"),
        }
    }

    fn discard_drafts(&mut self) {
        self.editing_title = false;
        self.editing_description = false;
        self.adding_reference = false;
        self.instruction_draft = None;
        self.execution_draft = None;
        self.execution_ref_inputs.clear();
        self.proposal_review = None;
    }

    // ----------------------------------------------------------------------
    // Mutations
    // ----------------------------------------------------------------------

    fn run(
        &mut self,
        key: impl Into<String>,
        task: ActionTask,
        cx: &mut Context<Self>,
        on_ok: impl FnOnce(&mut Self) + 'static,
    ) {
        let key = key.into();
        if !self.in_flight.insert(key.clone()) {
            return;
        }
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.in_flight.remove(&key);
                match result {
                    Ok(_) => on_ok(this),
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn update_item(
        &mut self,
        key: &str,
        changes: WorkItemChanges,
        cx: &mut Context<Self>,
        on_ok: impl FnOnce(&mut Self) + 'static,
    ) {
        let task =
            self.state
                .read(cx)
                .work_outline_actions()
                .update_work_item(UpdateWorkItemParams {
                    work_item_id: self.item_id.clone(),
                    changes,
                    operation_id: new_operation_id(),
                });
        self.run(key, task, cx, on_ok);
    }

    fn begin_title_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = self
            .item()
            .map(|item| item.title.clone())
            .unwrap_or_default();
        self.editing_title = true;
        self.title_input
            .update(cx, |input, cx| input.set_text(title, cx));
        let handle = self.title_input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    fn save_title(&mut self, cx: &mut Context<Self>) {
        let title: String = self
            .title_input
            .read(cx)
            .text()
            .trim()
            .chars()
            .take(zeron_proto::limits::MAX_TITLE_CHARS)
            .collect();
        if title.is_empty() || self.item().is_some_and(|item| item.title == title) {
            self.editing_title = false;
            cx.notify();
            return;
        }
        self.update_item(
            "title",
            WorkItemChanges {
                title: Some(title),
                ..Default::default()
            },
            cx,
            |this| this.editing_title = false,
        );
    }

    fn begin_description_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self
            .item()
            .and_then(|item| item.description.clone())
            .unwrap_or_default();
        self.editing_description = true;
        self.description_input
            .update(cx, |input, cx| input.set_text(text, cx));
        let handle = self.description_input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    fn save_description(&mut self, cx: &mut Context<Self>) {
        let text: String = self
            .description_input
            .read(cx)
            .text()
            .trim()
            .chars()
            .take(zeron_proto::limits::MAX_DESCRIPTION_CHARS)
            .collect();
        let patch = if text.is_empty() {
            FieldPatch::Clear
        } else {
            FieldPatch::Set(text)
        };
        self.update_item(
            "description",
            WorkItemChanges {
                description: patch,
                ..Default::default()
            },
            cx,
            |this| this.editing_description = false,
        );
    }

    pub(crate) fn set_status(&mut self, status: WorkItemStatus, cx: &mut Context<Self>) {
        // Re-completing an item that gained a request is gated too.
        let open = self
            .item()
            .is_some_and(|item| item.rollup.unresolved_user_action_count > 0);
        if self.item().is_some_and(|item| {
            item.status == status && !(open && status == WorkItemStatus::Completed)
        }) {
            return;
        }
        if status == WorkItemStatus::Completed {
            self.request_completion(cx);
            return;
        }
        self.update_item(
            "status",
            WorkItemChanges {
                status: Some(status),
                ..Default::default()
            },
            cx,
            |_| {},
        );
    }

    pub(crate) fn disassociate(&mut self, chat_id: String, cx: &mut Context<Self>) {
        cx.emit(DetailsEvent::ReassociateChat {
            space_id: self.space_id.clone(),
            chat_id,
            work_item_id: None,
        });
    }

    fn associate(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.associate_open = false;
        cx.emit(DetailsEvent::ReassociateChat {
            space_id: self.space_id.clone(),
            chat_id,
            work_item_id: Some(self.item_id.clone()),
        });
        cx.notify();
    }

    fn add_reference(&mut self, cx: &mut Context<Self>) {
        let raw = self.reference_input.read(cx).text().to_string();
        let Some(parsed) = parse_reference(&raw) else {
            self.error = Some("Enter an http(s) URL".into());
            cx.notify();
            return;
        };
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .create_external_reference(CreateWorkItemExternalReferenceParams {
                work_item_id: self.item_id.clone(),
                provider: parsed.provider,
                reference_type: parsed.reference_type,
                external_id: parsed.external_id,
                url: parsed.url,
                cached_title: None,
                operation_id: new_operation_id(),
            });
        let input = self.reference_input.clone();
        self.run("reference:add", task, cx, move |this| {
            this.adding_reference = false;
            let _ = input;
        });
    }

    fn remove_reference(&mut self, reference_id: String, cx: &mut Context<Self>) {
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .remove_external_reference(RemoveWorkItemExternalReferenceParams {
                reference_id: reference_id.clone(),
                operation_id: new_operation_id(),
            });
        self.run(format!("reference:{reference_id}"), task, cx, |_| {});
    }

    fn open_reference(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .shell
            .update(cx, |shell, cx| {
                shell.open_work_item_reference(&url, window, cx)
            })
            .is_err()
        {
            cx.open_url(&url);
        }
    }

    /// "Archive…": the Shell's archive confirmation, which for an item with
    /// descendants makes the user choose between archiving the subtree and
    /// moving the descendants to another parent (never an implicit default).
    pub(crate) fn request_archive(&mut self, cx: &mut Context<Self>) {
        let (Some(item), Some(snapshot)) = (self.item().cloned(), self.snapshot.clone()) else {
            return;
        };
        let descendant_count = model::descendants(&snapshot, &item.id).len();
        // Valid new parents: the Space root and every live item outside the
        // archived subtree, labeled with their path; the current parent
        // first so it is preselected.
        let mut parents: Vec<WorkOutlineParentChoice> =
            model::destinations(&snapshot, Some(&item.id))
                .into_iter()
                .map(|dest| WorkOutlineParentChoice {
                    label: match &dest.item_id {
                        Some(id) => model::path_label(
                            model::item_path(&snapshot, id)
                                .iter()
                                .map(|item| item.title.as_str()),
                        ),
                        None => "Top level".to_string(),
                    }
                    .into(),
                    work_item_id: dest.item_id,
                })
                .collect();
        if let Some(ix) = parents
            .iter()
            .position(|choice| choice.work_item_id == item.parent_work_item_id)
        {
            let current = parents.remove(ix);
            parents.insert(0, current);
        }
        let weak = cx.entity().downgrade();
        let request = WorkOutlineArchiveRequest {
            item_title: item.title.clone().into(),
            descendant_count,
            parents,
            on_decision: Box::new(move |policy, _window, cx| {
                let Some(policy) = policy else {
                    return;
                };
                weak.update(cx, |this, cx| {
                    let task = this
                        .state
                        .read(cx)
                        .work_outline_actions()
                        .archive_work_item(ArchiveWorkItemParams {
                            work_item_id: this.item_id.clone(),
                            descendant_archive_policy: policy,
                            operation_id: new_operation_id(),
                        });
                    this.run("archive", task, cx, |_| {});
                })
                .ok();
            }),
        };
        if self
            .shell
            .update(cx, |shell, cx| {
                shell.open_work_outline_archive_confirm(request, cx)
            })
            .is_err()
        {
            self.error = Some("Archive needs the app window".into());
            cx.notify();
        }
    }

    pub(crate) fn restore(&mut self, cx: &mut Context<Self>) {
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .restore_work_item(RestoreWorkItemParams {
                work_item_id: self.item_id.clone(),
                operation_id: new_operation_id(),
            });
        self.run("archive", task, cx, |_| {});
    }

    // ----------------------------------------------------------------------
    // Rendering
    // ----------------------------------------------------------------------

    fn section_header(
        id: &'static str,
        label: String,
        open: bool,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(4.0))
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(11.0))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme.text_muted)
            .hover(|el| el.text_color(theme.text))
            .child(
                crate::icons::icon(if open {
                    crate::icons::ALT_ARROW_DOWN
                } else {
                    crate::icons::ALT_ARROW_RIGHT
                })
                .size(px(10.0))
                .text_color(theme.text_muted),
            )
            .child(SharedString::from(label))
    }

    /// A compact details button. Its hover tooltip (label + live binding)
    /// comes from [`super::hints::details_button_hint`], keyed by the label.
    fn small_button(
        id: impl Into<gpui::ElementId>,
        label: &'static str,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        let button = div()
            .id(id)
            .flex_none()
            .px(px(6.0))
            .py(px(1.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(theme.border)
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(10.5))
            .text_color(theme.text_muted)
            .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
            .child(label);
        match super::hints::details_button_hint(label) {
            Some((tip, key)) => super::hints::WithHint::hint(button, tip, key),
            None => button,
        }
    }

    /// Point the editors' skill lists at this item's resolved execution root
    /// (the Space root's on the Space-root variant) for the harness a new
    /// Chat would use. Cheap when nothing changed; runs every render so a
    /// snapshot or configuration change re-targets an open list.
    fn sync_skill_sources(&self, cx: &mut Context<Self>) {
        if self.instruction_draft.is_none() && self.proposal_review.is_none() {
            return;
        }
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let harness = self
            .shell
            .upgrade()
            .map(|shell| shell.read(cx).skill_harness(cx))
            .unwrap_or(zeron_proto::HarnessId::ClaudeCode);
        let (space_path, space_device) = self
            .state
            .read(cx)
            .space_row(&self.space_id)
            .map(|space| (space.path.clone(), space.device_id.clone()))
            .unwrap_or_default();
        let source = SkillSource::for_outline_scope(
            &snapshot,
            &self.space_id,
            Some(self.item_id.as_str()),
            harness,
            &space_path,
            &space_device,
        );
        for skills in [&self.instruction_skills, &self.proposal_skills] {
            let source = source.clone();
            skills.update(cx, |skills, cx| skills.set_source(source, cx));
        }
    }

    /// [`Self::input_frame`] for a multi-line editor with `/` skill
    /// autocomplete: the frame anchors the popup and grows with the input
    /// (no height cap of its own, which would clip the input's last rows).
    fn editor_frame(
        input: &Entity<ComposerInput>,
        skills: &Entity<SkillCompletion>,
        selector: &'static str,
        theme: &Theme,
    ) -> gpui::Div {
        Self::input_frame(input, theme)
            .relative()
            .debug_selector(move || selector.into())
            .child(skills.clone())
    }

    fn input_frame(input: &Entity<ComposerInput>, theme: &Theme) -> gpui::Div {
        div()
            .w_full()
            .min_h(px(24.0))
            .px(px(6.0))
            .py(px(3.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(theme.accent)
            .bg(theme.surface)
            .child(input.clone())
    }
}

impl Render for WorkItemDetails {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        self.sync_skill_sources(cx);
        let shell_frame = div()
            .id("work-item-details")
            .debug_selector(|| "work-item-details".into())
            .track_focus(&self.focus)
            .flex_none()
            .w(px(DETAILS_PANEL_W))
            .h_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key != "escape" {
                    return;
                }
                if this.instruction_draft.is_some() {
                    if this.instruction_draft_dirty(cx) {
                        this.confirm_discard(cx, |_| {});
                    } else {
                        this.instruction_draft = None;
                    }
                } else if this.proposal_review.is_some() {
                    if this.proposal_review_dirty(cx) {
                        this.confirm_discard(cx, |_| {});
                    } else {
                        this.cancel_proposal_review(cx);
                    }
                } else if this.execution_draft.is_some() {
                    if this.execution_draft_dirty(cx) {
                        this.confirm_discard(cx, |_| {});
                    } else {
                        this.cancel_execution(cx);
                    }
                } else if this.editing_title {
                    this.editing_title = false;
                } else if this.editing_description {
                    this.editing_description = false;
                } else if this.adding_reference {
                    this.adding_reference = false;
                } else {
                    this.request_close(cx);
                }
                window.focus(&this.focus, cx);
                cx.stop_propagation();
                cx.notify();
            }));
        let close = div()
            .id("work-item-details-close")
            .flex_none()
            .size(px(22.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .cursor_pointer()
            .hover(|el| el.bg(theme.element_hover))
            .child(
                crate::icons::icon(crate::icons::CLOSE)
                    .size(px(12.0))
                    .text_color(theme.text_muted),
            )
            .on_click(cx.listener(|this, _, _, cx| this.request_close(cx)));
        if self.is_space_root() {
            return self.render_space_root(shell_frame, close, &theme, cx);
        }
        let Some(item) = self.item().cloned() else {
            return shell_frame
                .child(div().flex().justify_end().p(px(8.0)).child(close))
                .child(
                    div()
                        .px(px(16.0))
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text_muted)
                        .child(if self.snapshot.is_some() {
                            "This work item no longer exists."
                        } else {
                            "Loading…"
                        }),
                );
        };
        let snapshot = self.snapshot.clone().expect("item implies snapshot");

        // Breadcrumbs: ancestors are links.
        let path = model::item_path(&snapshot, &item.id);
        let root_space = self.space_id.clone();
        let root_crumb = [
            div()
                .id("work-item-crumb-space-root")
                .debug_selector(|| "work-item-crumb-space-root".into())
                .flex_shrink_0()
                .cursor_pointer()
                .hover(|el| el.text_color(theme.text))
                .child("Space root")
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(DetailsEvent::OpenItem {
                        space_id: root_space.clone(),
                        item_id: SPACE_ROOT_ITEM_ID.into(),
                    })
                }))
                .into_any_element(),
            div().flex_none().child("›").into_any_element(),
        ];
        let crumbs = root_crumb
            .into_iter()
            .chain(
                path.iter()
                    .take(path.len().saturating_sub(1))
                    .enumerate()
                    .flat_map(|(ix, ancestor)| {
                        let space_id = self.space_id.clone();
                        let item_id = ancestor.id.clone();
                        [
                            div()
                                .id(("work-item-crumb", ix))
                                .flex_shrink_0()
                                .max_w(px(140.0))
                                .truncate()
                                .cursor_pointer()
                                .hover(|el| el.text_color(theme.text))
                                .child(SharedString::from(ancestor.title.clone()))
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(DetailsEvent::OpenItem {
                                        space_id: space_id.clone(),
                                        item_id: item_id.clone(),
                                    })
                                }))
                                .into_any_element(),
                            div().flex_none().child("›").into_any_element(),
                        ]
                    }),
            )
            .collect::<Vec<AnyElement>>();
        let header = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(14.0))
            .pt(px(10.0))
            .child(
                div()
                    .debug_selector(|| "work-item-breadcrumbs".into())
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .overflow_hidden()
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_muted)
                    .children(crumbs),
            )
            .child(close);

        let title: AnyElement = if self.editing_title {
            div()
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "enter" {
                        this.save_title(cx);
                        cx.stop_propagation();
                        window.prevent_default();
                    }
                }))
                .child(Self::input_frame(&self.title_input, &theme))
                .into_any_element()
        } else {
            div()
                .id("work-item-title")
                .debug_selector(|| "work-item-title".into())
                .cursor_text()
                .text_size(crate::typography::ui_rems(15.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(SharedString::from(item.title.clone()))
                .on_click(cx.listener(|this, _, window, cx| this.begin_title_edit(window, cx)))
                .into_any_element()
        };

        // Status: base picker, effective state when an action is required.
        let status_pills = [
            WorkItemStatus::Pending,
            WorkItemStatus::Active,
            WorkItemStatus::Completed,
        ]
        .into_iter()
        .map(|status| {
            let active = item.status == status;
            div()
                .id(SharedString::from(format!(
                    "work-item-status-{}",
                    chips::base_status_label(status).to_lowercase()
                )))
                .px(px(8.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(11.0))
                .when(active, |el| {
                    el.bg(theme.element_active).text_color(theme.text)
                })
                .when(!active, |el| el.text_color(theme.text_muted))
                .hover(|el| el.text_color(theme.text))
                .child(chips::base_status_label(status))
                .on_click(cx.listener(move |this, _, _, cx| this.set_status(status, cx)))
        });
        let attention = (item.effective_status == WorkItemEffectiveStatus::UserActionRequired
            || item.rollup.unresolved_user_action_count > 0)
            .then(|| {
                let (glyph, label, color) =
                    chips::status_style(WorkItemEffectiveStatus::UserActionRequired, &theme);
                let count = item.rollup.unresolved_user_action_count;
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(color)
                    .child(SharedString::from(if count > 1 {
                        format!("{glyph} {label} ({count})")
                    } else {
                        format!("{glyph} {label}")
                    }))
            });
        let status_row = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .gap(px(2.0))
                    .p(px(2.0))
                    .rounded(px(6.0))
                    .bg(theme.element_hover.opacity(0.5))
                    .children(status_pills),
            )
            .when_some(attention, |el, attention| el.child(attention))
            .child(div().flex_1())
            .when(item.archived_at.is_none(), |el| {
                let space_id = self.space_id.clone();
                let item_id = item.id.clone();
                el.child(
                    Self::small_button("work-item-start-chat", "Start Chat", &theme)
                        .debug_selector(|| "work-item-start-chat".into())
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(DetailsEvent::StartChat {
                                space_id: space_id.clone(),
                                item_id: item_id.clone(),
                            })
                        })),
                )
            });

        // Description (collapsed until opened).
        let description_open = self.description_open || self.editing_description;
        let mut description = div().flex().flex_col().gap(px(4.0)).child(
            Self::section_header(
                "work-item-description-toggle",
                "Description".into(),
                description_open,
                &theme,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.description_open = !this.description_open;
                cx.notify();
            })),
        );
        if description_open {
            if self.editing_description {
                description = description
                    .child(
                        Self::input_frame(&self.description_input, &theme)
                            .debug_selector(|| "work-item-description-editor".into()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(6.0))
                            .child(
                                Self::small_button("work-item-description-save", "Save", &theme)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.save_description(cx)),
                                    ),
                            )
                            .child(
                                Self::small_button(
                                    "work-item-description-cancel",
                                    "Cancel",
                                    &theme,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.editing_description = false;
                                        cx.notify();
                                    },
                                )),
                            ),
                    );
            } else {
                let text = item.description.clone().filter(|d| !d.trim().is_empty());
                description = description.child(
                    div()
                        .id("work-item-description")
                        .cursor_text()
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(if text.is_some() {
                            theme.text
                        } else {
                            theme.text_faint
                        })
                        .child(SharedString::from(
                            text.unwrap_or_else(|| "No description".into()),
                        ))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.begin_description_edit(window, cx)
                        })),
                );
            }
        }

        // Associated chats.
        let chat_rows = item.chat_ids.iter().enumerate().map(|(ix, chat_id)| {
            let chat = self.chats.iter().find(|chat| &chat.id == chat_id);
            let label =
                chat.map_or_else(|| format!("Missing chat · {chat_id}"), |c| c.title.clone());
            let dot = chat.map(|c| crate::shell::spaces::status_dot_color(c.status, &theme));
            let open_id = chat_id.clone();
            let remove_id = chat_id.clone();
            let missing = chat.is_none();
            div()
                .id(("work-item-chat", ix))
                .debug_selector(move || format!("work-item-chat-{ix}"))
                .h(px(24.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .px(px(4.0))
                .rounded(px(4.0))
                .hover(|el| el.bg(theme.element_hover))
                .when_some(dot, |el, dot| {
                    el.child(div().flex_none().size(px(6.0)).rounded_full().bg(dot))
                })
                .child(
                    div()
                        .id(("work-item-chat-open", ix))
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(if missing {
                            theme.text_faint
                        } else {
                            theme.text
                        })
                        .when(missing, |el| el.italic())
                        .when(!missing, |el| el.cursor_pointer())
                        .child(SharedString::from(label))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            if !missing {
                                cx.emit(DetailsEvent::OpenChat(open_id.clone()));
                            }
                        })),
                )
                .child(
                    Self::small_button(("work-item-chat-remove", ix), "Disassociate", &theme)
                        .debug_selector(move || format!("work-item-chat-remove-{ix}"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.disassociate(remove_id.clone(), cx)
                        })),
                )
        });
        let associated: HashSet<&str> = snapshot
            .items
            .iter()
            .flat_map(|item| item.chat_ids.iter().map(String::as_str))
            .collect();
        let candidates: Vec<(String, String)> = if self.associate_open {
            self.chats
                .iter()
                .filter(|chat| {
                    chat.visible
                        && chat.space_id.as_deref() == Some(self.space_id.as_str())
                        && !associated.contains(chat.id.as_str())
                })
                .map(|chat| (chat.id.clone(), chat.title.clone()))
                .collect()
        } else {
            Vec::new()
        };
        let associate_list = self.associate_open.then(|| {
            div()
                .debug_selector(|| "work-item-associate-list".into())
                .flex()
                .flex_col()
                .pl(px(8.0))
                .when(candidates.is_empty(), |el| {
                    el.child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_faint)
                            .child("No unassociated chats in this project"),
                    )
                })
                .children(candidates.into_iter().enumerate().map(|(ix, (id, title))| {
                    div()
                        .id(("work-item-associate", ix))
                        .debug_selector(move || format!("work-item-associate-{ix}"))
                        .h(px(22.0))
                        .flex()
                        .items_center()
                        .px(px(4.0))
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .truncate()
                        .text_size(crate::typography::ui_rems(11.5))
                        .text_color(theme.text_muted)
                        .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
                        .child(SharedString::from(title))
                        .on_click(cx.listener(move |this, _, _, cx| this.associate(id.clone(), cx)))
                }))
        });
        let chats_section = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text_muted)
                            .child(SharedString::from(format!(
                                "Chats ({})",
                                item.chat_ids.len()
                            ))),
                    )
                    .child(
                        Self::small_button("work-item-associate-toggle", "Associate…", &theme)
                            .debug_selector(|| "work-item-associate-toggle".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.associate_open = !this.associate_open;
                                cx.notify();
                            })),
                    ),
            )
            .children(chat_rows)
            .when_some(associate_list, |el, list| el.child(list));

        // Immediate children.
        let children: Vec<WorkItemView> = item
            .child_ids
            .iter()
            .filter_map(|id| snapshot.item(id).cloned())
            .filter(|child| child.archived_at.is_none())
            .collect();
        let children_section = (!children.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(11.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text_muted)
                        .child(SharedString::from(format!("Items ({})", children.len()))),
                )
                .children(children.into_iter().enumerate().map(|(ix, child)| {
                    let (glyph, _, color) = chips::status_style(child.effective_status, &theme);
                    let space_id = self.space_id.clone();
                    let item_id = child.id.clone();
                    div()
                        .id(("work-item-child", ix))
                        .h(px(22.0))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .px(px(4.0))
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .hover(|el| el.bg(theme.element_hover))
                        .child(div().w(px(12.0)).text_color(color).child(glyph))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.text)
                                .child(SharedString::from(child.title)),
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(DetailsEvent::OpenItem {
                                space_id: space_id.clone(),
                                item_id: item_id.clone(),
                            })
                        }))
                }))
        });

        // External references (collapsed until opened).
        let references: Vec<_> = snapshot
            .external_references
            .iter()
            .filter(|reference| reference.work_item_id == item.id)
            .cloned()
            .collect();
        let refs_open = self.references_open || self.adding_reference;
        let mut refs_section = div().flex().flex_col().gap(px(2.0)).child(
            Self::section_header(
                "work-item-references-toggle",
                format!("External references ({})", references.len()),
                refs_open,
                &theme,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.references_open = !this.references_open;
                cx.notify();
            })),
        );
        if refs_open {
            refs_section =
                refs_section.children(references.into_iter().enumerate().map(|(ix, reference)| {
                    let label = reference
                        .cached_title
                        .clone()
                        .unwrap_or_else(|| reference.external_id.clone());
                    let tip: SharedString =
                        format!("{} · {}", reference.provider, reference.url).into();
                    let url = reference.url.clone();
                    let id = reference.id.clone();
                    div()
                        .h(px(22.0))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .px(px(4.0))
                        .child(
                            crate::icons::icon(crate::icons::GLOBAL)
                                .size(px(10.0))
                                .text_color(theme.text_faint),
                        )
                        .child(
                            div()
                                .id(("work-item-reference", ix))
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .cursor_pointer()
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.accent)
                                .child(SharedString::from(label))
                                .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_reference(url.clone(), window, cx)
                                })),
                        )
                        .child(
                            Self::small_button(
                                ("work-item-reference-remove", ix),
                                "Remove",
                                &theme,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| this.remove_reference(id.clone(), cx),
                            )),
                        )
                }));
            if self.adding_reference {
                refs_section = refs_section
                    .child(
                        div()
                            .on_key_down(cx.listener(
                                |this, event: &gpui::KeyDownEvent, window, cx| {
                                    if event.keystroke.key == "enter" {
                                        this.add_reference(cx);
                                        cx.stop_propagation();
                                        window.prevent_default();
                                    }
                                },
                            ))
                            .child(Self::input_frame(&self.reference_input, &theme)),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(6.0))
                            .child(
                                Self::small_button("work-item-reference-save", "Add", &theme)
                                    .on_click(cx.listener(|this, _, _, cx| this.add_reference(cx))),
                            )
                            .child(
                                Self::small_button("work-item-reference-cancel", "Cancel", &theme)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.adding_reference = false;
                                        cx.notify();
                                    })),
                            ),
                    );
            } else {
                refs_section = refs_section.child(
                    div().child(
                        Self::small_button("work-item-reference-add", "Add reference", &theme)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.adding_reference = true;
                                this.reference_input
                                    .update(cx, |input, cx| input.set_text("", cx));
                                let handle = this.reference_input.read(cx).focus_handle(cx);
                                window.focus(&handle, cx);
                                cx.notify();
                            })),
                    ),
                );
            }
        }

        // Archive / restore.
        let footer = if item.archived_at.is_some() {
            div().child(
                Self::small_button("work-item-restore", "Restore", &theme)
                    .on_click(cx.listener(|this, _, _, cx| this.restore(cx))),
            )
        } else {
            div().child(
                Self::small_button("work-item-archive", "Archive…", &theme)
                    .on_click(cx.listener(|this, _, _, cx| this.request_archive(cx))),
            )
        };

        let created_by = self.render_created_by(&item, &theme, cx);
        let required_actions = self.render_required_actions_section(&snapshot, &theme, cx);
        let proposals = self.render_proposals_section(&snapshot, &theme, cx);
        let error = self.error.clone();
        shell_frame.child(header).child(
            div()
                .id("work-item-details-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(12.0))
                .px(px(14.0))
                .py(px(10.0))
                .child(title)
                .when_some(created_by, |el, line| el.child(line))
                .child(status_row)
                .child(required_actions)
                .child(description)
                .child(self.render_instructions_section(Some(&item), &snapshot, &theme, cx))
                .when_some(proposals, |el, section| el.child(section))
                .child(self.render_effective_section(&item, &snapshot, &theme, cx))
                .child(self.render_execution_section(&snapshot, &theme, cx))
                .child(chats_section)
                .when_some(children_section, |el, section| el.child(section))
                .child(refs_section)
                .when_some(error, |el, error| {
                    el.child(
                        div()
                            .debug_selector(|| "work-item-details-error".into())
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.warning)
                            .child(SharedString::from(error)),
                    )
                })
                .child(footer),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_parse_github_linear_and_plain_links() {
        let pr = parse_reference(" https://github.com/acme/launch/pull/42/files ").unwrap();
        assert_eq!(
            (
                pr.provider.as_str(),
                pr.reference_type.as_str(),
                pr.external_id.as_str()
            ),
            ("github", "pullRequest", "acme/launch#42")
        );
        let issue = parse_reference("https://github.com/acme/launch/issues/7").unwrap();
        assert_eq!(issue.reference_type, "issue");
        let linear = parse_reference("https://linear.app/acme/issue/eng-12/fix-it").unwrap();
        assert_eq!(
            (linear.provider.as_str(), linear.external_id.as_str()),
            ("linear", "ENG-12")
        );
        let web = parse_reference("https://example.com/doc").unwrap();
        assert_eq!(
            (web.provider.as_str(), web.reference_type.as_str()),
            ("web", "link")
        );
        assert!(parse_reference("not a url").is_none());
        assert!(parse_reference("https://").is_none());
    }
}
