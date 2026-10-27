//! Start Chat from a work item (Work Outline Milestone 3 Part B): the
//! Shell-owned dialog behind the Outline row action, the Work Item Details
//! button, and the palette command.
//!
//! The dialog shows the item path, a prompt (generic composer input: Enter
//! starts, Shift+Enter breaks a line), the agent (the composer's own
//! harness/model/traits picker and Ask/Auto chip, starting from exactly what
//! a brand-new regular chat would use; Tab walks prompt → model → approval,
//! Esc closes an open picker before the dialog), where the
//! Chat will run with its validation on the owning device (blocking, with
//! each error's remediation), the worktree choice when the configuration
//! pins none, and the item's Effective Instructions (expanded with a notice
//! when they changed since the item's last launch). Start calls
//! `CreateChatForWorkItem`; success opens the new Chat. A failed or
//! unknown-outcome start keeps the dialog and retries with the SAME
//! operation id while the inputs are unchanged; a Chat the host created is
//! never deleted (a rejected first Turn offers Open Chat and Retry).
use super::work_outline_ui::WorkOutlineModal;
use super::*;

use std::sync::Arc;

use zeron_proto::{
    CreateChatForWorkItemParams, EffectiveInstructionEntry, ExecutionScope,
    ExecutionWorktreeIdentity, PrReference, PrReviewAction, ResolvedExecutionConfiguration,
    ValidateExecutionConfigurationParams, WorkItemLaunchState, WorkItemPrompt, WorkOutlineSnapshot,
};

use crate::composer::skill_completion::{SkillCompletion, SkillInsertion, SkillSource};
use crate::pickers::{Pickers, ReturnComposerFocus};
use crate::work_outline::actions::OperationSlot;
use crate::work_outline::execution::{self as exec, ValidationHeadline, ValidationState};
use crate::work_outline::launch::{self, LaunchFailure, WorktreePick, WorktreeTarget};
use crate::work_outline::model;

/// What a PR-mode Start Chat will launch.
#[derive(Clone)]
pub(super) struct PrLaunch {
    pub(super) reference: PrReference,
    pub(super) action: PrReviewAction,
    /// `(spaceId, workItemId)` the new Chat is filed under.
    pub(super) item: Option<(String, String)>,
}

pub(super) struct StartChatDialog {
    pub(super) space_id: String,
    /// Empty in PR mode without a work item.
    pub(super) item_id: String,
    item_path: String,
    /// PR mode: a review of this PR instead of a work-item launch. The
    /// prompt, the checkout and the PR link come from the host's
    /// `StartPrReview` when Start is confirmed; the dialog adds the agent.
    pub(super) pr: Option<PrLaunch>,
    /// The Space's owning device.
    device_id: String,
    pub(super) input: Entity<ComposerInput>,
    /// `/` skill autocomplete on the prompt: the picked agent's skills for
    /// the item's resolved execution root, inserted as the composer does.
    pub(super) skills: Entity<SkillCompletion>,
    /// Folder the skill list comes from (resolved execution root, else the
    /// Space path).
    skill_root: String,
    /// Skill harness while the picker has no harness resolved yet.
    fallback_harness: zeron_proto::HarnessId,
    pub(super) worktree_name: Entity<ComposerInput>,
    /// The composer's harness/model picker in launcher mode: a new chat on
    /// the owning device, seeded with the composer's sticky defaults. Its
    /// resolved config is the `ChatConfig` Start sends.
    pub(super) pickers: Entity<Pickers>,
    /// The picker closed through Esc / its chip: focus returns to the prompt
    /// on the next frame (so Enter starts).
    pub(super) focus_prompt_pending: bool,
    resolved: Option<ResolvedExecutionConfiguration>,
    pub(super) target: WorktreeTarget,
    /// Worktree choices of the target repository: `None` while describing.
    pub(super) worktrees: Option<Result<Vec<ExecutionWorktreeIdentity>, String>>,
    pub(super) pick: WorktreePick,
    pub(super) validation: ValidationState,
    effective: Vec<EffectiveInstructionEntry>,
    pub(super) instructions_changed: bool,
    pub(super) instructions_open: bool,
    /// Why Start cannot work at all (the owner lacks the capability).
    pub(super) launch_blocker: Option<String>,
    op: OperationSlot,
    pub(super) starting: bool,
    pub(super) failure: Option<LaunchFailure>,
    /// The host created the Chat but rejected its first Turn:
    /// `(chat id, detail)`.
    pub(super) rejected: Option<(String, String)>,
    _input_events: Subscription,
    _picker_events: Subscription,
    _picker_observe: Subscription,
}

impl StartChatDialog {
    pub(super) fn input_focus(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }

    /// Where focus starts and returns to: the prompt, or in PR mode (which
    /// has no prompt box) the modal itself so Enter starts.
    pub(super) fn initial_focus(&self, modal: &FocusHandle, cx: &App) -> FocusHandle {
        if self.pr.is_some() {
            modal.clone()
        } else {
            self.input_focus(cx)
        }
    }

    /// Whether validation lets the launch proceed.
    pub(super) fn validated(&self) -> bool {
        matches!(&self.validation, ValidationState::Done(reply) if reply.ok)
    }

    fn skill_source(&self, cx: &App) -> SkillSource {
        SkillSource {
            harness: self
                .pickers
                .read(cx)
                .resolved(cx)
                .harness
                .unwrap_or(self.fallback_harness),
            cwd: self.skill_root.clone(),
            device_id: (!self.device_id.is_empty()).then(|| self.device_id.clone()),
        }
    }

    pub(super) fn can_start(&self, cx: &App) -> bool {
        if self.pr.is_some() {
            return !self.starting;
        }
        !self.starting
            && self.launch_blocker.is_none()
            && self.validated()
            && !self.input.read(cx).text().trim().is_empty()
    }
}

impl Shell {
    fn start_chat_dialog(&mut self) -> Option<&mut StartChatDialog> {
        match self.work_outline_modal.as_mut() {
            Some(WorkOutlineModal::StartChat(dialog)) => Some(dialog),
            _ => None,
        }
    }

    /// Open Start Chat for a work item. Errors are short notices for the
    /// caller (unknown item, archived item, outline not loaded).
    pub(crate) fn open_start_chat(
        &mut self,
        space_id: &str,
        item_id: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let snapshot: Arc<WorkOutlineSnapshot> = self
            .state
            .read(cx)
            .work_outline(space_id)
            .snapshot()
            .cloned()
            .ok_or("This project's outline is not loaded")?;
        let item = snapshot.item(item_id).ok_or("That work item is gone")?;
        if item.archived_at.is_some() {
            return Err("Restore the work item to start a Chat in it");
        }
        let item_path = model::path_label(
            model::item_path(&snapshot, item_id)
                .iter()
                .map(|item| item.title.as_str()),
        );
        let resolved = snapshot.resolve_execution(&ExecutionScope::WorkItem {
            work_item_id: item_id.to_owned(),
        });
        let target = resolved
            .as_ref()
            .filter(|resolved| resolved.invalid_reason.is_none())
            .map(|resolved| launch::worktree_target(&resolved.configuration))
            .unwrap_or(WorktreeTarget::None);
        let device_id = resolved
            .as_ref()
            .map(|resolved| resolved.device_id.clone())
            .filter(|device| !device.is_empty())
            .or_else(|| {
                self.state
                    .read(cx)
                    .space_row(space_id)
                    .map(|space| space.device_id.clone())
            })
            .unwrap_or_default();
        let launch_blocker = {
            let state = self.state.read(cx);
            state
                .devices
                .iter()
                .find(|device| device.id == device_id)
                .filter(|device| {
                    !device.supports(zeron_proto::capabilities::WORK_OUTLINE_LAUNCH_V1)
                })
                .map(|device| {
                    format!(
                        "Update Mast on {} to start Chats from work items",
                        device.name
                    )
                })
        };
        let effective = snapshot.effective_instructions(item_id);
        let instructions_changed =
            launch::instructions_changed_since_last_launch(&snapshot, item_id);
        let composer_pickers = self.composer.read(cx).pickers().clone();
        let picker_device = Some(device_id.clone()).filter(|device| !device.is_empty());
        let pickers = cx.new(|cx| Pickers::new_chat_launcher(&composer_pickers, picker_device, cx));
        let item_for_events = item_id.to_owned();
        let picker_events = cx.subscribe(
            &pickers,
            move |this: &mut Shell, _, _: &ReturnComposerFocus, cx| {
                if let Some(dialog) = this
                    .start_chat_dialog()
                    .filter(|dialog| dialog.item_id == item_for_events)
                {
                    dialog.focus_prompt_pending = true;
                    cx.notify();
                }
            },
        );
        // A harness pick re-points the prompt's `/` skills at that agent.
        let item_for_observe = item_id.to_owned();
        let picker_observe = cx.observe(&pickers, move |this: &mut Shell, _, cx| {
            let Some(dialog) = this
                .start_chat_dialog()
                .filter(|dialog| dialog.item_id == item_for_observe)
            else {
                return;
            };
            let skills = dialog.skills.clone();
            let source = dialog.skill_source(cx);
            skills.update(cx, |skills, cx| skills.set_source(source, cx));
            cx.notify();
        });
        let input = cx.new(|cx| {
            ComposerInput::new("What should the agent do?", cx)
                .with_text_metrics(12.5, 18.0)
                .with_auto_grow()
        });
        let skills = cx.new(|cx| {
            SkillCompletion::new(
                self.state.clone(),
                input.clone(),
                SkillInsertion::SlashCommand,
                cx,
            )
            .over_modal()
        });
        let skill_root = resolved
            .as_ref()
            .map(|resolved| resolved.configuration.execution_root.clone())
            .filter(|root| !root.is_empty())
            .or_else(|| {
                self.state
                    .read(cx)
                    .space_row(space_id)
                    .map(|space| space.path.clone())
            })
            .unwrap_or_default();
        let fallback_harness = self.skill_harness(cx);
        let worktree_name = cx.new(|cx| {
            ComposerInput::with_context(
                "Worktree name",
                crate::composer::PALETTE_SEARCH_CONTEXT,
                cx,
            )
            .with_single_line()
            .with_text_metrics(12.0, 18.0)
        });
        let input_events = cx.subscribe(&input, |this: &mut Shell, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_start_chat(cx);
            }
        });
        let describe = target.describe_path().map(str::to_owned);
        let dialog = StartChatDialog {
            space_id: space_id.to_owned(),
            item_id: item_id.to_owned(),
            item_path,
            pr: None,
            device_id,
            input,
            skills,
            skill_root,
            fallback_harness,
            worktree_name,
            pickers,
            focus_prompt_pending: false,
            resolved,
            target,
            worktrees: None,
            pick: WorktreePick::SpaceWorkspace,
            validation: ValidationState::Idle,
            effective: effective.entries,
            instructions_changed,
            instructions_open: instructions_changed,
            launch_blocker,
            op: OperationSlot::default(),
            starting: false,
            failure: None,
            rejected: None,
            _input_events: input_events,
            _picker_events: picker_events,
            _picker_observe: picker_observe,
        };
        let source = dialog.skill_source(cx);
        dialog
            .skills
            .update(cx, |skills, cx| skills.set_source(source, cx));
        self.begin_work_outline_modal(WorkOutlineModal::StartChat(dialog), cx);
        self.validate_start_chat(cx);
        if let Some(path) = describe {
            self.describe_start_chat_target(path, cx);
        }
        Ok(())
    }

    /// [`Self::open_start_chat`], showing a failure as the sidebar notice.
    pub(crate) fn open_start_chat_or_notice(
        &mut self,
        space_id: &str,
        item_id: &str,
        cx: &mut Context<Self>,
    ) {
        if let Err(notice) = self.open_start_chat(space_id, item_id, cx) {
            self.set_sidebar_error(notice);
            cx.notify();
        }
    }

    /// Open Start Chat in PR mode (see [`StartChatDialog::pr`]): the agent
    /// picker starts from the composer's sticky new-chat defaults like the
    /// work-item dialog, on the device that owns the item's Space.
    pub(super) fn open_start_chat_for_pr(
        &mut self,
        reference: PrReference,
        action: PrReviewAction,
        item: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let (space_id, item_id) = item.clone().unwrap_or_default();
        let item_path = item
            .as_ref()
            .and_then(|(space, item)| {
                let state = self.state.read(cx);
                let snapshot = state.work_outline(space).snapshot()?.clone();
                Some(model::path_label(
                    model::item_path(&snapshot, item)
                        .iter()
                        .map(|item| item.title.as_str()),
                ))
            })
            .unwrap_or_default();
        let device_id = self
            .state
            .read(cx)
            .space_row(&space_id)
            .map(|space| space.device_id.clone())
            .or_else(|| self.state.read(cx).effective_device_id())
            .unwrap_or_default();
        let composer_pickers = self.composer.read(cx).pickers().clone();
        let picker_device = Some(device_id.clone()).filter(|device| !device.is_empty());
        let pickers = cx.new(|cx| Pickers::new_chat_launcher(&composer_pickers, picker_device, cx));
        let key = (reference.url(), action);
        let picker_events = cx.subscribe(
            &pickers,
            move |this: &mut Shell, _, _: &ReturnComposerFocus, cx| {
                if let Some(dialog) = this.start_chat_dialog().filter(|dialog| {
                    dialog
                        .pr
                        .as_ref()
                        .is_some_and(|pr| (pr.reference.url(), pr.action) == key)
                }) {
                    dialog.focus_prompt_pending = true;
                    cx.notify();
                }
            },
        );
        let picker_observe = cx.observe(&pickers, |_, _, cx| cx.notify());
        // The prompt, skills and worktree inputs belong to the work-item
        // launch; PR mode never shows them.
        let input = cx.new(|cx| ComposerInput::new("", cx));
        let skills = cx.new(|cx| {
            SkillCompletion::new(
                self.state.clone(),
                input.clone(),
                SkillInsertion::SlashCommand,
                cx,
            )
            .over_modal()
        });
        let worktree_name = cx.new(|cx| {
            ComposerInput::with_context(
                "Worktree name",
                crate::composer::PALETTE_SEARCH_CONTEXT,
                cx,
            )
            .with_single_line()
        });
        let input_events = cx.subscribe(&input, |_, _, _, _| {});
        let dialog = StartChatDialog {
            space_id,
            item_id,
            item_path,
            pr: Some(PrLaunch {
                reference,
                action,
                item,
            }),
            device_id,
            input,
            skills,
            skill_root: String::new(),
            fallback_harness: self.skill_harness(cx),
            worktree_name,
            pickers,
            focus_prompt_pending: false,
            resolved: None,
            target: WorktreeTarget::None,
            worktrees: None,
            pick: WorktreePick::SpaceWorkspace,
            validation: ValidationState::Idle,
            effective: Vec::new(),
            instructions_changed: false,
            instructions_open: false,
            launch_blocker: None,
            op: OperationSlot::default(),
            starting: false,
            failure: None,
            rejected: None,
            _input_events: input_events,
            _picker_events: picker_events,
            _picker_observe: picker_observe,
        };
        self.begin_work_outline_modal(WorkOutlineModal::StartChat(dialog), cx);
        Ok(())
    }

    /// Start in PR mode: `StartPrReview` with the dialog's agent pick, then
    /// open the new Chat. A refusal stays in the dialog.
    fn submit_start_pr_review(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.start_chat_dialog() else {
            return;
        };
        let Some(pr) = dialog.pr.clone() else {
            return;
        };
        if dialog.starting {
            return;
        }
        let config = dialog.pickers.read(cx).resolved(cx).chat_config();
        let pickers = dialog.pickers.clone();
        dialog.starting = true;
        dialog.failure = None;
        let key = (pr.reference.url(), pr.action);
        let task = self.launch_pr_review(pr.reference, pr.action, pr.item, config, cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let same = |dialog: &StartChatDialog| {
                    dialog
                        .pr
                        .as_ref()
                        .is_some_and(|pr| (pr.reference.url(), pr.action) == key)
                };
                let Some(dialog) = this.start_chat_dialog().filter(|d| same(d)) else {
                    return;
                };
                dialog.starting = false;
                match result {
                    Ok(_) => {
                        // A chat started with these picks: they become the
                        // composer's last-used new-chat selection.
                        let picks = pickers.read(cx).new_chat_defaults().clone();
                        let composer_pickers = this.composer.read(cx).pickers().clone();
                        composer_pickers.update(cx, |composer_pickers, cx| {
                            composer_pickers.adopt_new_chat_picks(&picks, cx)
                        });
                        this.work_outline_modal = None;
                        this.work_outline_modal_focus_pending = false;
                        this.work_outline_prior_focus = None;
                    }
                    Err(error) => {
                        dialog.failure = Some(LaunchFailure {
                            code: None,
                            message: crate::pr_review_launch::failure_text(&error),
                            remediation: None,
                            unknown_outcome: false,
                        });
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Validate the item's configuration on its owning device (the engine
    /// forwards). Start stays disabled until it passes.
    pub(super) fn validate_start_chat(&mut self, cx: &mut Context<Self>) {
        let actions = self.state.read(cx).work_outline_actions();
        let Some(dialog) = self.start_chat_dialog() else {
            return;
        };
        let item_id = dialog.item_id.clone();
        dialog.validation = ValidationState::Running;
        let task = actions.validate_execution(ValidateExecutionConfigurationParams {
            scope: Some(ExecutionScope::WorkItem {
                work_item_id: item_id.clone(),
            }),
            ..Default::default()
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let Some(dialog) = this.start_chat_dialog().filter(|d| d.item_id == item_id) else {
                    return;
                };
                dialog.validation = match result {
                    Ok(reply) => ValidationState::Done(reply),
                    Err(error) => ValidationState::Failed(exec::execution_error_text(&error)),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Fetch the target repository's checkouts for the worktree choice. A
    /// root that is not a Git checkout simply offers no choice.
    fn describe_start_chat_target(&mut self, path: String, cx: &mut Context<Self>) {
        let (actions, local) = {
            let state = self.state.read(cx);
            (state.work_outline_actions(), state.local_device_id.clone())
        };
        let Some(dialog) = self.start_chat_dialog() else {
            return;
        };
        let item_id = dialog.item_id.clone();
        let target_device_id = Some(dialog.device_id.clone())
            .filter(|device| !device.is_empty() && local.as_deref() != Some(device.as_str()));
        let task = actions.describe_repository(zeron_proto::DescribeExecutionRepositoryParams {
            path: path.clone(),
            target_device_id,
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let Some(dialog) = this.start_chat_dialog().filter(|d| d.item_id == item_id) else {
                    return;
                };
                match result {
                    Ok(identity) => {
                        let own = identity.repo_path.clone();
                        dialog.worktrees = Some(Ok(identity
                            .worktrees
                            .into_iter()
                            .filter(|checkout| {
                                checkout.checkout_id.is_some()
                                    && !checkout.prunable
                                    && checkout.path != own
                                    && checkout.path != path
                            })
                            .collect()));
                    }
                    Err(error) => {
                        if matches!(dialog.target, WorktreeTarget::Root { .. }) {
                            // Not a Git checkout: nothing to choose.
                            dialog.target = WorktreeTarget::None;
                        } else {
                            dialog.worktrees = Some(Err(exec::execution_error_text(&error)));
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn set_start_chat_pick(&mut self, pick: WorktreePick, cx: &mut Context<Self>) {
        let Some(dialog) = self.start_chat_dialog() else {
            return;
        };
        if pick == WorktreePick::CreateNew && dialog.worktree_name.read(cx).text().is_empty() {
            let name = crate::workspace_chip::worktree_name(dialog.input.read(cx).text());
            dialog
                .worktree_name
                .update(cx, |input, cx| input.set_text(name, cx));
        }
        if let Some(dialog) = self.start_chat_dialog() {
            dialog.pick = pick;
        }
        cx.notify();
    }

    /// Escape while the dialog's picker is open closes the picker only.
    pub(super) fn close_start_chat_picker(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(pickers) = self.start_chat_dialog().map(|d| d.pickers.clone()) else {
            return false;
        };
        pickers.update(cx, |pickers, cx| pickers.close_open_popover(cx))
    }

    /// The dialog's open picker (palette/shortcut forwarding).
    pub(super) fn start_chat_pickers(&mut self) -> Option<Entity<Pickers>> {
        self.start_chat_dialog()
            .map(|dialog| dialog.pickers.clone())
    }

    /// Tab / Shift+Tab walk prompt → model chip → approval chip (cycling).
    /// Keys inside the open picker, the worktree name, or an open `/` list
    /// are left alone.
    pub(super) fn start_chat_tab(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = event.keystroke.modifiers;
        let modal_focus = self.work_outline_modal_focus.clone();
        // PR mode has no prompt box to press Enter in: Enter on the dialog
        // itself, or ⌘↩ from a chip, starts.
        if event.keystroke.key == "enter"
            && !modifiers.shift
            && !modifiers.alt
            && !modifiers.control
            && self.start_chat_dialog().is_some_and(|dialog| {
                dialog.pr.is_some()
                    && !dialog.pickers.read(cx).is_open()
                    && (modifiers.platform || modal_focus.is_focused(window))
            })
        {
            self.submit_start_chat(cx);
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key != "tab" || modifiers.control || modifiers.alt || modifiers.platform
        {
            return;
        }
        let Some(dialog) = self.start_chat_dialog() else {
            return;
        };
        if dialog.skills.read(cx).is_open() {
            return;
        }
        let pickers = dialog.pickers.read(cx);
        if pickers.is_open() {
            return;
        }
        let order = [
            dialog.initial_focus(&modal_focus, cx),
            pickers.model_chip_focus().clone(),
            pickers.approval_chip_focus().clone(),
        ];
        let Some(current) = order.iter().position(|handle| handle.is_focused(window)) else {
            return;
        };
        let next = if modifiers.shift {
            (current + order.len() - 1) % order.len()
        } else {
            (current + 1) % order.len()
        };
        window.focus(&order[next], cx);
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn toggle_start_chat_instructions(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = self.start_chat_dialog() {
            dialog.instructions_open = !dialog.instructions_open;
            cx.notify();
        }
    }

    /// Start (explicit): `CreateChatForWorkItem`, then open the new Chat.
    pub(super) fn submit_start_chat(&mut self, cx: &mut Context<Self>) {
        if self
            .start_chat_dialog()
            .is_some_and(|dialog| dialog.pr.is_some())
        {
            return self.submit_start_pr_review(cx);
        }
        let actions = self.state.read(cx).work_outline_actions();
        let Some(dialog) = self.start_chat_dialog() else {
            return;
        };
        if dialog.starting {
            return;
        }
        let text = dialog.input.read(cx).text().trim().to_owned();
        let blocked = if text.is_empty() {
            Some("Write what the agent should do".to_owned())
        } else if let Some(blocker) = dialog.launch_blocker.clone() {
            Some(blocker)
        } else if !dialog.validated() {
            Some(match &dialog.validation {
                ValidationState::Running | ValidationState::Idle => {
                    "Wait for validation to finish".to_owned()
                }
                _ => "Fix the execution configuration first (see above)".to_owned(),
            })
        } else {
            None
        };
        if let Some(message) = blocked {
            dialog.failure = Some(LaunchFailure {
                code: None,
                message,
                remediation: None,
                unknown_outcome: false,
            });
            cx.notify();
            return;
        }
        let name = dialog.worktree_name.read(cx).text().to_owned();
        let worktree_choice = match launch::worktree_choice(&dialog.pick, &name) {
            Ok(choice) => choice,
            Err(message) => {
                dialog.failure = Some(LaunchFailure {
                    code: None,
                    message,
                    remediation: None,
                    unknown_outcome: false,
                });
                cx.notify();
                return;
            }
        };
        // Built exactly as the composer builds a new chat's `createChat`
        // config (concrete harness, model, reasoning, options, approval).
        let config = dialog.pickers.read(cx).resolved(cx).chat_config();
        let pickers = dialog.pickers.clone();
        let mut params = CreateChatForWorkItemParams {
            work_item_id: dialog.item_id.clone(),
            initial_prompt: WorkItemPrompt {
                text,
                attachments: Vec::new(),
            },
            config,
            execution_override: None,
            worktree_choice,
            operation_id: String::new(),
            parent_chat_id: None,
            target_device_id: None,
        };
        // Same inputs → same operation id (a retry completes the same start).
        let inputs = serde_json::to_string(&params).unwrap_or_default();
        params.operation_id = dialog.op.id_for(&inputs);
        dialog.starting = true;
        dialog.failure = None;
        let item_id = dialog.item_id.clone();
        let task = actions.create_chat_for_work_item(params);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let Some(dialog) = this.start_chat_dialog().filter(|d| d.item_id == item_id) else {
                    return;
                };
                dialog.starting = false;
                if result.is_ok() {
                    // A chat started with these picks: they become the
                    // composer's last-used new-chat selection.
                    let picks = pickers.read(cx).new_chat_defaults().clone();
                    let composer_pickers = this.composer.read(cx).pickers().clone();
                    composer_pickers.update(cx, |composer_pickers, cx| {
                        composer_pickers.adopt_new_chat_picks(&picks, cx)
                    });
                }
                let Some(dialog) = this.start_chat_dialog().filter(|d| d.item_id == item_id) else {
                    return;
                };
                match result {
                    Ok(reply) if reply.state == WorkItemLaunchState::Rejected => {
                        dialog.rejected = Some((
                            reply.chat_id,
                            reply
                                .detail
                                .unwrap_or_else(|| "The host rejected the first Turn".into()),
                        ));
                        cx.notify();
                    }
                    Ok(reply) => {
                        this.work_outline_modal = None;
                        this.work_outline_modal_focus_pending = false;
                        this.work_outline_prior_focus = None;
                        this.open_chat(reply.chat_id, cx);
                    }
                    Err(error) => {
                        dialog.failure = Some(launch::launch_failure(&error));
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Open the Chat the host created even though its first Turn was
    /// rejected (it is never deleted).
    pub(super) fn open_rejected_start_chat(&mut self, cx: &mut Context<Self>) {
        let Some(chat_id) = self
            .start_chat_dialog()
            .and_then(|dialog| dialog.rejected.as_ref().map(|(id, _)| id.clone()))
        else {
            return;
        };
        self.work_outline_modal = None;
        self.work_outline_modal_focus_pending = false;
        self.work_outline_prior_focus = None;
        self.open_chat(chat_id, cx);
    }
}

fn note(selector: &'static str, text: impl Into<SharedString>, color: gpui::Hsla) -> gpui::Div {
    div()
        .debug_selector(move || selector.into())
        .text_size(crate::typography::ui_rems(11.5))
        .text_color(color)
        .child(text.into())
}

/// The shared persistent, copyable error card, tagged for tests.
fn error_card(
    selector: &'static str,
    theme: &Theme,
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
) -> gpui::Div {
    div()
        .debug_selector(move || selector.into())
        .w_full()
        .child(crate::error_notice::ErrorNotice::new(
            selector, theme, title, detail,
        ))
}

fn section_label(text: &'static str, theme: &Theme) -> gpui::Div {
    div()
        .mt(px(10.0))
        .text_size(crate::typography::ui_rems(11.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.text_muted)
        .child(text)
}

fn device_name(shell: &Shell, device_id: &str, cx: &App) -> String {
    shell
        .state
        .read(cx)
        .devices
        .iter()
        .find(|device| device.id == device_id)
        .map(|device| device.name.clone())
        .unwrap_or_else(|| device_id.to_owned())
}

/// The Start Chat card.
/// The Start Chat body's height budget in a `viewport_height` window: the
/// window minus the card chrome (title, item path, buttons, scrim margin),
/// never below the original 440px.
fn start_chat_body_max_height(viewport_height: f32) -> f32 {
    (viewport_height - 200.0).max(440.0)
}

pub(super) fn start_chat_card(
    dialog: &StartChatDialog,
    shell: &Shell,
    viewport_height: f32,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let can_start = dialog.can_start(cx);
    if let Some(pr) = &dialog.pr {
        return pr_start_chat_card(dialog, pr, can_start, viewport_height, theme, cx);
    }
    // Agent: the composer's own model chip (harness/model/traits popover)
    // and Ask/Auto chip, hosted in launcher mode.
    let agents = div()
        .debug_selector(|| "start-chat-agent".into())
        .mt(px(4.0))
        .flex()
        .flex_row()
        .items_center()
        .child(dialog.pickers.clone());

    // Where it runs + validation.
    let mut execution = div().flex().flex_col().gap(px(2.0));
    match &dialog.resolved {
        Some(resolved) => {
            execution = execution.child(note(
                "start-chat-execution",
                launch::execution_summary(resolved),
                theme.text,
            ));
        }
        None => {
            execution = execution.child(note(
                "start-chat-execution",
                "The Space is not available",
                theme.warning,
            ));
        }
    }
    match &dialog.validation {
        ValidationState::Idle | ValidationState::Running => {
            execution = execution.child(note(
                "start-chat-validation-running",
                "Validating on the owning device\u{2026}",
                theme.text_muted,
            ));
        }
        ValidationState::Failed(message) => {
            execution = execution.child(error_card(
                "start-chat-validation-failed",
                theme,
                "Couldn't validate this Chat's setup",
                message.clone(),
            ));
        }
        ValidationState::Done(reply) => {
            let (selector, text, color): (&'static str, String, gpui::Hsla) =
                match exec::validation_headline(reply) {
                    ValidationHeadline::Ready => {
                        ("start-chat-validation-ready", "Ready".into(), theme.success)
                    }
                    ValidationHeadline::DeviceOffline { device_id } => (
                        "start-chat-validation-device-offline",
                        format!(
                            "{} is offline \u{2014} Mast will not start this Chat elsewhere",
                            device_name(shell, &device_id, cx)
                        ),
                        theme.warning,
                    ),
                    ValidationHeadline::HostUnsupported { device_id } => (
                        "start-chat-validation-host-unsupported",
                        format!(
                            "{} runs a Mast that can\u{2019}t validate execution",
                            device_name(shell, &device_id, cx)
                        ),
                        theme.warning,
                    ),
                    ValidationHeadline::Blocked { count } => (
                        "start-chat-validation-blocked",
                        format!(
                            "Blocked \u{b7} {count} problem{}",
                            if count == 1 { "" } else { "s" }
                        ),
                        theme.danger,
                    ),
                };
            execution = execution.child(note(selector, text, color));
            for (ix, issue) in exec::validation_issues(reply).into_iter().enumerate() {
                execution = execution.child(
                    div()
                        .debug_selector(move || format!("start-chat-issue-{ix}"))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(crate::typography::ui_rems(11.5))
                                .text_color(theme.text)
                                .child(SharedString::from(issue.message)),
                        )
                        .child(
                            div()
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(theme.text_muted)
                                .child(SharedString::from(issue.remediation)),
                        ),
                );
            }
            if !reply.ok {
                execution = execution.child(
                    div().child(
                        popover::btn_ghost(theme, "Validate again", "start-chat-revalidate")
                            .id("start-chat-revalidate")
                            .debug_selector(|| "start-chat-revalidate".into())
                            .on_click(cx.listener(|this, _, _, cx| this.validate_start_chat(cx))),
                    ),
                );
            }
        }
    }

    // Worktree choice (only when the configuration pins none, and only when
    // the target repository's checkout is the execution root).
    let worktree_unavailable = match &dialog.target {
        WorktreeTarget::Unavailable { reason } => Some(note(
            "start-chat-worktree-unavailable",
            reason.clone(),
            theme.text_muted,
        )),
        _ => None,
    };
    let worktree =
        (!matches!(
            dialog.target,
            WorktreeTarget::None | WorktreeTarget::Unavailable { .. }
        ))
        .then(|| {
            let option =
                |id: SharedString, label: String, pick: WorktreePick, cx: &mut Context<Shell>| {
                    let selected = dialog.pick == pick;
                    let selector = id.to_string();
                    popover::menu_row(theme, selected, id.clone())
                        .id(id)
                        .debug_selector(move || selector.clone())
                        .role(gpui::Role::RadioButton)
                        .aria_label(label.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_start_chat_pick(pick.clone(), cx)
                        }))
                        .child(icon(icons::CHECK).size(px(14.0)).flex_none().text_color(
                            if selected {
                                theme.text
                            } else {
                                gpui::transparent_black()
                            },
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(label)),
                        )
                };
            let workspace_path = dialog.target.describe_path().unwrap_or_default().to_owned();
            let mut list = div()
                .debug_selector(|| "start-chat-worktree".into())
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(option(
                    "start-chat-worktree-workspace".into(),
                    format!("Space workspace \u{b7} {workspace_path}"),
                    WorktreePick::SpaceWorkspace,
                    cx,
                ));
            match &dialog.worktrees {
                None => {
                    list = list.child(note(
                        "start-chat-worktree-loading",
                        "Loading worktrees\u{2026}",
                        theme.text_muted,
                    ));
                }
                Some(Err(message)) => {
                    list = list.child(error_card(
                        "start-chat-worktree-error",
                        theme,
                        "Couldn't list worktrees",
                        message.clone(),
                    ));
                }
                Some(Ok(worktrees)) => {
                    for (ix, checkout) in worktrees.iter().enumerate() {
                        let Some(id) = checkout.checkout_id.clone() else {
                            continue;
                        };
                        list = list.child(option(
                            format!("start-chat-worktree-existing-{ix}").into(),
                            format!(
                                "{} \u{b7} {}",
                                exec::path_name(&checkout.path),
                                exec::ref_label(
                                    checkout.branch.as_deref(),
                                    checkout.head_sha.as_deref()
                                )
                            ),
                            WorktreePick::Existing {
                                worktree_id: id,
                                worktree_path: checkout.path.clone(),
                            },
                            cx,
                        ));
                    }
                }
            }
            list = list.child(option(
                "start-chat-worktree-new".into(),
                "New worktree".into(),
                WorktreePick::CreateNew,
                cx,
            ));
            if dialog.pick == WorktreePick::CreateNew {
                list = list.child(
                    div()
                        .ml(px(22.0))
                        .px(px(8.0))
                        .py(px(4.0))
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(theme.border_strong)
                        .bg(theme.input_bg)
                        .child(dialog.worktree_name.clone()),
                );
            }
            list
        });

    // Effective Instructions.
    let count = dialog.effective.len();
    let instructions = div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .id("start-chat-instructions-toggle")
                .debug_selector(|| "start-chat-instructions-toggle".into())
                .mt(px(10.0))
                .flex()
                .items_center()
                .gap(px(4.0))
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(11.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text_muted)
                .hover(|el| el.text_color(theme.text))
                .child(
                    icon(if dialog.instructions_open {
                        icons::ALT_ARROW_DOWN
                    } else {
                        icons::ALT_ARROW_RIGHT
                    })
                    .size(px(10.0))
                    .text_color(theme.text_muted),
                )
                .child(SharedString::from(format!(
                    "Effective instructions ({count})"
                )))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_start_chat_instructions(cx))),
        )
        .when(dialog.instructions_changed, |el| {
            el.child(note(
                "start-chat-instructions-changed",
                "Instructions changed since this item\u{2019}s last launch",
                theme.warning,
            ))
        })
        .when(dialog.instructions_open, |el| {
            el.child(
                div()
                    .debug_selector(|| "start-chat-instructions".into())
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .when(count == 0, |el| {
                        el.child(note(
                            "start-chat-instructions-none",
                            "No Work Outline instructions apply",
                            theme.text_faint,
                        ))
                    })
                    .children(dialog.effective.iter().map(|entry| {
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(crate::typography::ui_rems(10.5))
                                    .text_color(theme.text_faint)
                                    .child(SharedString::from(format!(
                                        "{}. {}{}",
                                        entry.order + 1,
                                        entry.scope_title,
                                        if entry.inherited { " (inherited)" } else { "" }
                                    ))),
                            )
                            .child(
                                div()
                                    .text_size(crate::typography::ui_rems(11.5))
                                    .text_color(theme.text)
                                    .child(SharedString::from(entry.text.clone())),
                            )
                    })),
            )
        });

    let failure = dialog.failure.clone().map(|failure| {
        div()
            .debug_selector(|| "start-chat-failure".into())
            .mt(px(8.0))
            .flex()
            .flex_col()
            .child(error_card(
                "start-chat-failure-message",
                theme,
                failure.message,
                failure.remediation.unwrap_or_default(),
            ))
    });
    let rejected = dialog.rejected.clone().map(|(_, detail)| {
        div()
            .debug_selector(|| "start-chat-rejected".into())
            .mt(px(8.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(error_card(
                "start-chat-rejected-detail",
                theme,
                "The Chat was created but its first Turn was rejected",
                detail,
            ))
            .child(
                div().child(
                    popover::btn_ghost(theme, "Open Chat", "start-chat-open-rejected")
                        .id("start-chat-open-rejected")
                        .debug_selector(|| "start-chat-open-rejected".into())
                        .on_click(cx.listener(|this, _, _, cx| this.open_rejected_start_chat(cx))),
                ),
            )
    });

    let start_label = if dialog.starting {
        "Starting\u{2026}"
    } else if dialog.failure.is_some() || dialog.rejected.is_some() {
        "Retry"
    } else {
        "Start Chat"
    };
    popover::dialog_card(theme)
        .w(px(520.0))
        .debug_selector(|| "start-chat".into())
        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
            this.start_chat_tab(event, window, cx)
        }))
        .child(popover::dialog_title(theme, "Start Chat"))
        .child(note(
            "start-chat-item",
            dialog.item_path.clone(),
            theme.text_muted,
        ))
        .child(
            div()
                .id("start-chat-body")
                // Room for the prompt at its auto-grow cap (40% of the
                // window) plus the sections under it; the rest scrolls.
                .max_h(px(start_chat_body_max_height(viewport_height)))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .child(
                    // Auto-grows with the prompt (the input owns the cap and
                    // scrolls past it); no height cap here, which would clip.
                    div()
                        .relative()
                        .debug_selector(|| "start-chat-prompt".into())
                        .flex_none()
                        .mt(px(8.0))
                        .w_full()
                        .px(px(8.0))
                        .py(px(6.0))
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(theme.border_strong)
                        .bg(theme.input_bg)
                        .child(dialog.input.clone())
                        .child(dialog.skills.clone()),
                )
                .child(section_label("Agent", theme))
                .child(agents)
                .child(section_label("Runs in", theme))
                .child(execution)
                .when_some(worktree_unavailable, |el, unavailable| {
                    el.child(section_label("Worktree", theme))
                        .child(unavailable)
                })
                .when_some(worktree, |el, worktree| {
                    el.child(section_label("Worktree", theme)).child(worktree)
                })
                .child(instructions)
                .when_some(dialog.launch_blocker.clone(), |el, blocker| {
                    el.child(note("start-chat-blocker", blocker, theme.warning).mt(px(8.0)))
                })
                .when_some(failure, |el, failure| el.child(failure))
                .when_some(rejected, |el, rejected| el.child(rejected)),
        )
        .child(
            div()
                .mt(px(16.0))
                .flex()
                .justify_end()
                .gap(px(8.0))
                .child(
                    popover::btn_ghost(theme, "Cancel", "start-chat-cancel")
                        .id("start-chat-cancel")
                        .debug_selector(|| "start-chat-cancel".into())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.resolve_work_outline_modal(false, window, cx);
                        })),
                )
                .child(
                    popover::btn_primary(theme, start_label)
                        .id("start-chat-start")
                        .debug_selector(|| "start-chat-start".into())
                        .when(!can_start, |el| el.opacity(0.5))
                        .on_click(cx.listener(|this, _, _, cx| this.submit_start_chat(cx))),
                ),
        )
        .into_any_element()
}

/// The PR-mode Start Chat card: what will run (the review prompt, the PR
/// link, where the host checks the branch out) is shown as a summary; the
/// agent picker is the work-item dialog's own.
fn pr_start_chat_card(
    dialog: &StartChatDialog,
    pr: &PrLaunch,
    can_start: bool,
    viewport_height: f32,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let url = pr.reference.url();
    let label = format!(
        "{}/{}#{}",
        pr.reference.owner, pr.reference.name, pr.reference.number
    );
    let title = match pr.action {
        PrReviewAction::CodeReview => "Review PR",
        PrReviewAction::FixCi => "Fix CI",
    };
    let header = if dialog.item_path.is_empty() {
        label.clone()
    } else {
        format!("{label} \u{b7} {}", dialog.item_path)
    };
    let monospace_line = |selector: &'static str, text: String| {
        div()
            .debug_selector(move || selector.into())
            .px(px(8.0))
            .py(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.input_bg)
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text)
            .child(SharedString::from(text))
    };
    let agents = div()
        .debug_selector(|| "start-chat-agent".into())
        .mt(px(4.0))
        .flex()
        .flex_row()
        .items_center()
        .child(dialog.pickers.clone());
    let failure = dialog.failure.clone().map(|failure| {
        div()
            .debug_selector(|| "start-chat-failure".into())
            .mt(px(8.0))
            .flex()
            .flex_col()
            .child(error_card(
                "start-chat-failure-message",
                theme,
                crate::chat_type_view::REVIEW_FAILURE_TITLE,
                failure.message,
            ))
    });
    let start_label = if dialog.starting {
        "Starting\u{2026}"
    } else if dialog.failure.is_some() {
        "Retry"
    } else {
        "Start Chat"
    };
    popover::dialog_card(theme)
        .w(px(520.0))
        .debug_selector(|| "start-chat".into())
        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
            this.start_chat_tab(event, window, cx)
        }))
        .child(popover::dialog_title(theme, title))
        .child(note("start-chat-item", header, theme.text_muted))
        .child(
            div()
                .id("start-chat-body")
                .max_h(px(start_chat_body_max_height(viewport_height)))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .child(section_label("Prompt", theme))
                .child(div().mt(px(4.0)).child(monospace_line(
                    "start-chat-pr-prompt",
                    crate::pr_review_launch::prompt_preview(pr.action, &url),
                )))
                .child(
                    note(
                        "start-chat-pr-prompt-note",
                        "The review instructions are added when the Chat starts.",
                        theme.text_faint,
                    )
                    .mt(px(2.0)),
                )
                .child(section_label("Linked to", theme))
                .child(note("start-chat-pr-link", url, theme.text).mt(px(2.0)))
                .child(section_label("Agent", theme))
                .child(agents)
                .child(section_label("Runs in", theme))
                .child(
                    note(
                        "start-chat-pr-root",
                        crate::pr_review_launch::RUNS_IN,
                        theme.text,
                    )
                    .mt(px(2.0)),
                )
                .when_some(failure, |el, failure| el.child(failure)),
        )
        .child(
            div()
                .mt(px(16.0))
                .flex()
                .justify_end()
                .gap(px(8.0))
                .child(
                    popover::btn_ghost(theme, "Cancel", "start-chat-cancel")
                        .id("start-chat-cancel")
                        .debug_selector(|| "start-chat-cancel".into())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.resolve_work_outline_modal(false, window, cx);
                        })),
                )
                .child(
                    popover::btn_primary(theme, start_label)
                        .id("start-chat-start")
                        .debug_selector(|| "start-chat-start".into())
                        .when(!can_start, |el| el.opacity(0.5))
                        .on_click(cx.listener(|this, _, _, cx| this.submit_start_chat(cx))),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_outline::WorkOutlineActions;
    use crate::work_outline::execution::fixture as execution_fixture;
    use crate::work_outline::store::fixture_ids::*;
    use std::sync::Mutex;

    type Calls = Arc<Mutex<Vec<(String, serde_json::Value)>>>;
    type Reply = Arc<
        Mutex<
            Box<
                dyn FnMut(&serde_json::Value) -> Result<serde_json::Value, zeron_rpc::RpcError>
                    + Send,
            >,
        >,
    >;

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

    fn validated_ok() -> serde_json::Value {
        serde_json::to_value(execution_fixture::launch_validated()).unwrap()
    }

    fn identity(path: &str) -> serde_json::Value {
        serde_json::json!({
            "repoId": "repo:alpha", "repoFingerprint": "root", "repoPath": path,
            "checkoutId": "checkout:alpha", "mainCheckout": true,
            "worktrees": [
                {"path": path, "checkoutId": "checkout:alpha", "branch": "main", "main": true},
                {"path": format!("{path}/.worktrees/fix"), "checkoutId": "checkout:fix",
                 "branch": "fix", "main": false},
                {"path": "/gone", "prunable": true},
            ],
        })
    }

    /// Seed Space `s1` (owned by device `d`, which advertises the launch
    /// capability unless `capable` is false) with `snapshot`; validation
    /// answers `validate`, Describe answers [`identity`], and
    /// `CreateChatForWorkItem` answers `create`.
    fn seed(
        shell: &Entity<Shell>,
        cx: &mut gpui::VisualTestContext,
        snapshot: WorkOutlineSnapshot,
        capable: bool,
        validate: serde_json::Value,
        create: Reply,
    ) -> Calls {
        let calls: Calls = Arc::default();
        let sink = calls.clone();
        let actions = WorkOutlineActions::scripted(Arc::new(move |method, params| {
            sink.lock()
                .unwrap()
                .push((method.to_owned(), params.clone()));
            match method {
                zeron_rpc::methods::VALIDATE_EXECUTION_CONFIGURATION => Ok(validate.clone()),
                zeron_rpc::methods::DESCRIBE_EXECUTION_REPOSITORY => {
                    Ok(identity(params["path"].as_str().unwrap_or_default()))
                }
                zeron_rpc::methods::CREATE_CHAT_FOR_WORK_ITEM => (create.lock().unwrap())(&params),
                zeron_rpc::methods::LIST_COMMANDS => Ok(serde_json::json!([
                    {"name": "code-review", "description": "Review a diff"},
                    {"name": "open-pr", "description": "Open a PR"},
                    {"name": "compact", "description": "Compact the conversation"},
                ])),
                _ => Ok(serde_json::json!({})),
            }
        }));
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.spaces = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "s1", "deviceId": "d", "path": "/work/alpha",
                        "createdAt": "2026-01-01T00:00:00Z"
                    }))
                    .unwrap(),
                ];
                state.chats = [CHAT_API, CHAT_AUTH, CHAT_UI]
                    .iter()
                    .map(|id| {
                        serde_json::from_value(serde_json::json!({
                            "id": id, "deviceId": "d", "archived": false,
                            "createdAt": "2026-01-01T00:00:00Z", "spaceId": "s1",
                            "title": format!("Chat {id}"),
                        }))
                        .unwrap()
                    })
                    .collect();
                let capabilities: Vec<&str> = if capable {
                    vec![zeron_proto::capabilities::WORK_OUTLINE_LAUNCH_V1]
                } else {
                    Vec::new()
                };
                state.devices = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "d", "name": "Studio", "platform": "macos",
                        "lastSeenAt": null, "capabilities": capabilities,
                    }))
                    .unwrap(),
                ];
                state.set_work_outline_visible(vec!["s1".into()]);
                // A re-seed with the same content is not a change.
                state.apply_work_outline_frame("s1", snapshot);
                state.work_outline_actions_override = Some(actions);
            })
        });
        calls
    }

    fn draw(cx: &mut gpui::VisualTestContext) {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear());
    }

    fn calls_to(calls: &Calls, method: &str) -> Vec<serde_json::Value> {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }

    fn launched(chat: &'static str) -> Reply {
        Arc::new(Mutex::new(Box::new(move |_: &serde_json::Value| {
            Ok(serde_json::json!({
                "chatId": chat, "messageId": "m", "commandId": "k",
                "executionRoot": "/work/alpha", "state": "queued",
            }))
        })))
    }

    fn click(cx: &mut gpui::VisualTestContext, selector: &'static str) {
        let at = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .center();
        cx.simulate_click(at, gpui::Modifiers::default());
        draw(cx);
    }

    fn set_prompt(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext, text: &str) {
        shell.update(cx, |shell, cx| {
            let input = shell.start_chat_dialog().unwrap().input.clone();
            input.update(cx, |input, cx| input.set_text(text, cx));
        });
    }

    /// Validation runs on open; the worktree choice lists the target
    /// repository's live checkouts; Start sends the prompt and the choice,
    /// retries an unknown outcome with the SAME operation id, then opens the
    /// new Chat.
    #[gpui::test]
    fn start_chat_offers_worktrees_and_retries_with_one_operation_id(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        // The first four attempts (one call with its transport retries) fail
        // in transport; the user's retry succeeds.
        let attempts = Arc::new(Mutex::new(0usize));
        let counter = attempts.clone();
        let create: Reply = Arc::new(Mutex::new(Box::new(move |_: &serde_json::Value| {
            let mut n = counter.lock().unwrap();
            *n += 1;
            if *n <= crate::work_outline::actions::MAX_ATTEMPTS as usize {
                Err(zeron_rpc::RpcError::Closed)
            } else {
                Ok(serde_json::json!({
                    "chatId": "chat-new", "messageId": "m", "commandId": "k",
                    "executionRoot": "/work/alpha", "state": "queued",
                }))
            }
        })));
        let calls = seed(
            &shell,
            cx,
            execution_fixture::snapshot("s1"),
            true,
            validated_ok(),
            create,
        );
        shell
            .update(cx, |shell, cx| {
                shell.open_start_chat("s1", HOUSEKEEPING, cx)
            })
            .unwrap();
        draw(cx);
        assert!(cx.debug_bounds("start-chat").is_some());
        assert!(cx.debug_bounds("start-chat-validation-ready").is_some());
        let validate = calls_to(&calls, zeron_rpc::methods::VALIDATE_EXECUTION_CONFIGURATION);
        assert_eq!(
            validate[0],
            serde_json::json!({
                "scope": {"type": "workItem", "workItemId": HOUSEKEEPING},
                "hostCapabilities": {"additionalDirectories": false},
            })
        );
        // Housekeeping runs in the implicit Space folder: its repository's
        // checkouts are offered (prunable and the folder itself excluded).
        let describe = calls_to(&calls, zeron_rpc::methods::DESCRIBE_EXECUTION_REPOSITORY);
        assert_eq!(describe[0]["path"], "/work/alpha");
        assert_eq!(describe[0]["targetDeviceId"], "d");
        assert!(cx.debug_bounds("start-chat-worktree-workspace").is_some());
        assert!(cx.debug_bounds("start-chat-worktree-existing-0").is_some());
        assert!(cx.debug_bounds("start-chat-worktree-existing-1").is_none());
        assert!(cx.debug_bounds("start-chat-worktree-new").is_some());

        // Empty prompt: nothing is sent.
        click(cx, "start-chat-start");
        assert!(calls_to(&calls, zeron_rpc::methods::CREATE_CHAT_FOR_WORK_ITEM).is_empty());
        assert!(cx.debug_bounds("start-chat-failure").is_some());

        set_prompt(&shell, cx, "Tidy the build scripts");
        click(cx, "start-chat-worktree-existing-0");
        click(cx, "start-chat-start");
        assert!(
            cx.debug_bounds("start-chat-failure-message").is_some(),
            "unknown outcome"
        );
        shell.read_with(cx, |shell, _| {
            let Some(WorkOutlineModal::StartChat(dialog)) = &shell.work_outline_modal else {
                panic!("the dialog stays open");
            };
            assert!(dialog.failure.as_ref().unwrap().unknown_outcome);
        });
        click(cx, "start-chat-start");
        let creates = calls_to(&calls, zeron_rpc::methods::CREATE_CHAT_FOR_WORK_ITEM);
        assert_eq!(
            creates.len(),
            crate::work_outline::actions::MAX_ATTEMPTS as usize + 1
        );
        let op = creates[0]["operationId"].clone();
        assert!(
            creates.iter().all(|c| c["operationId"] == op),
            "one operation id"
        );
        assert_eq!(creates[0]["workItemId"], HOUSEKEEPING);
        assert_eq!(
            creates[0]["initialPrompt"]["text"],
            "Tidy the build scripts"
        );
        assert_eq!(
            creates[0]["worktreeChoice"],
            serde_json::json!({
                "type": "useExisting", "worktreeId": "checkout:fix",
                "worktreePath": "/work/alpha/.worktrees/fix",
            })
        );
        assert!(
            creates[0].get("targetDeviceId").is_none(),
            "the engine forwards"
        );
        shell.read_with(cx, |shell, cx| {
            assert!(shell.work_outline_modal.is_none());
            assert_eq!(
                shell.state.read(cx).selected_chat.as_deref(),
                Some("chat-new")
            );
        });
    }

    /// A configuration whose primary repository's checkout is not the
    /// execution root offers no worktree choice and says why; the launch
    /// sends none.
    #[gpui::test]
    fn worktree_choice_is_hidden_when_the_root_is_not_the_primary_checkout(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let calls = seed(
            &shell,
            cx,
            execution_fixture::snapshot("s1"),
            true,
            validated_ok(),
            launched("chat-launch"),
        );
        shell
            .update(cx, |shell, cx| shell.open_start_chat("s1", LAUNCH, cx))
            .unwrap();
        draw(cx);
        assert!(cx.debug_bounds("start-chat-worktree-unavailable").is_some());
        assert!(cx.debug_bounds("start-chat-worktree-new").is_none());
        assert!(cx.debug_bounds("start-chat-worktree-workspace").is_none());
        assert!(calls_to(&calls, zeron_rpc::methods::DESCRIBE_EXECUTION_REPOSITORY).is_empty());
        set_prompt(&shell, cx, "Ship the launch page");
        click(cx, "start-chat-start");
        let creates = calls_to(&calls, zeron_rpc::methods::CREATE_CHAT_FOR_WORK_ITEM);
        assert_eq!(creates.len(), 1);
        assert!(
            creates[0].get("worktreeChoice").is_none(),
            "{:?}",
            creates[0]
        );
    }

    /// Blocking validation shows each error's remediation and keeps Start
    /// from sending; a host-side validation rejection splits into message
    /// and remediation; a new worktree carries its name.
    #[gpui::test]
    fn blocked_validation_and_host_rejections_show_remediation(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let offline = serde_json::to_value(
            zeron_proto::ValidateExecutionConfigurationReply::from_result(
                None,
                Err(vec![zeron_proto::ExecutionValidationError::DeviceOffline {
                    device_id: "d".into(),
                    detail: String::new(),
                }]),
            ),
        )
        .unwrap();
        let calls = seed(
            &shell,
            cx,
            execution_fixture::snapshot("s1"),
            true,
            offline,
            launched("x"),
        );
        shell
            .update(cx, |shell, cx| shell.open_start_chat("s1", API, cx))
            .unwrap();
        draw(cx);
        assert!(
            cx.debug_bounds("start-chat-validation-device-offline")
                .is_some()
        );
        assert!(cx.debug_bounds("start-chat-issue-0").is_some());
        assert!(cx.debug_bounds("start-chat-revalidate").is_some());
        set_prompt(&shell, cx, "Ship it");
        click(cx, "start-chat-start");
        assert!(calls_to(&calls, zeron_rpc::methods::CREATE_CHAT_FOR_WORK_ITEM).is_empty());

        // Validation passes now, but the host rejects at launch.
        let error = zeron_proto::ExecutionValidationError::WorktreePruned {
            repo_id: "repo:flagship".into(),
            worktree_id: "wt".into(),
            path: "/work/alpha/.worktrees/gone".into(),
        };
        let wire = format!(
            "work-outline/invalid: execution-scope:{}: {}. {}",
            error.code(),
            error.message(),
            error.remediation()
        );
        let calls = seed(
            &shell,
            cx,
            execution_fixture::snapshot("s1"),
            true,
            validated_ok(),
            Arc::new(Mutex::new(Box::new(move |_: &serde_json::Value| {
                Err(zeron_rpc::RpcError::Failed(wire.clone()))
            }))),
        );
        click(cx, "start-chat-revalidate");
        assert!(cx.debug_bounds("start-chat-validation-ready").is_some());
        click(cx, "start-chat-worktree-new");
        shell.read_with(cx, |shell, cx| {
            let Some(WorkOutlineModal::StartChat(dialog)) = &shell.work_outline_modal else {
                panic!("open");
            };
            assert_eq!(dialog.worktree_name.read(cx).text(), "ship-it");
        });
        click(cx, "start-chat-start");
        let creates = calls_to(&calls, zeron_rpc::methods::CREATE_CHAT_FOR_WORK_ITEM);
        assert_eq!(creates.len(), 1, "coded failures are not retried");
        assert_eq!(
            creates[0]["worktreeChoice"],
            serde_json::json!({"type": "createNew", "name": "ship-it"})
        );
        shell.read_with(cx, |shell, _| {
            let Some(WorkOutlineModal::StartChat(dialog)) = &shell.work_outline_modal else {
                panic!("open");
            };
            let failure = dialog.failure.as_ref().unwrap();
            assert_eq!(failure.code.as_deref(), Some("worktreePruned"));
            assert!(failure.message.contains("/work/alpha/.worktrees/gone"));
            assert_eq!(failure.remediation.as_deref(), Some(error.remediation()));
        });
        assert!(cx.debug_bounds("start-chat-failure-message").is_some());
    }

    /// Effective Instructions open expanded when they changed since the
    /// item's last launch; an owner without the launch capability blocks;
    /// a rejected first Turn keeps the created Chat reachable.
    #[gpui::test]
    fn instructions_capability_and_rejected_turns(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let mut snapshot = execution_fixture::snapshot("s1");
        let instructions = crate::work_outline::instructions::fixture::snapshot("s1");
        snapshot.instructions = instructions.instructions;
        snapshot.chat_instruction_states = instructions.chat_instruction_states;
        snapshot.revisions = instructions.revisions;
        let rejected: Reply = Arc::new(Mutex::new(Box::new(|_: &serde_json::Value| {
            Ok(serde_json::json!({
                "chatId": "chat-made", "messageId": "m", "commandId": "k",
                "executionRoot": "/work/alpha/flagship", "state": "rejected",
                "detail": "codex is not installed",
            }))
        })));
        seed(&shell, cx, snapshot.clone(), true, validated_ok(), rejected);
        // API: chat-api applied an older revision → changed → expanded.
        shell
            .update(cx, |shell, cx| shell.open_start_chat("s1", API, cx))
            .unwrap();
        draw(cx);
        assert!(cx.debug_bounds("start-chat-instructions-changed").is_some());
        assert!(cx.debug_bounds("start-chat-instructions").is_some());
        set_prompt(&shell, cx, "Document the endpoints");
        click(cx, "start-chat-start");
        assert!(cx.debug_bounds("start-chat-rejected").is_some());
        assert!(cx.debug_bounds("start-chat-open-rejected").is_some());
        // (Below the fold of the scrolling body in the test window.)
        shell.update(cx, |shell, cx| shell.open_rejected_start_chat(cx));
        draw(cx);
        shell.read_with(cx, |shell, cx| {
            assert!(shell.work_outline_modal.is_none());
            assert_eq!(
                shell.state.read(cx).selected_chat.as_deref(),
                Some("chat-made")
            );
        });

        // Auth: chat-auth runs the current revision → collapsed.
        shell
            .update(cx, |shell, cx| shell.open_start_chat("s1", API_AUTH, cx))
            .unwrap();
        draw(cx);
        assert!(cx.debug_bounds("start-chat-instructions-changed").is_none());
        assert!(cx.debug_bounds("start-chat-instructions").is_none());

        // An owner without `work-outline-launch-v1`: blocked up front.
        let calls = seed(&shell, cx, snapshot, false, validated_ok(), launched("x"));
        shell
            .update(cx, |shell, cx| shell.open_start_chat("s1", API_AUTH, cx))
            .unwrap();
        draw(cx);
        assert!(cx.debug_bounds("start-chat-blocker").is_some());
        set_prompt(&shell, cx, "Anything");
        click(cx, "start-chat-start");
        assert!(calls_to(&calls, zeron_rpc::methods::CREATE_CHAT_FOR_WORK_ITEM).is_empty());

        // Archived items cannot start Chats.
        let error = shell.update(cx, |shell, cx| shell.open_start_chat("s1", RETIRED, cx));
        assert!(error.is_err());
    }

    fn prompt_skills(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) -> Vec<String> {
        shell.update(cx, |shell, cx| {
            let skills = shell.start_chat_dialog().unwrap().skills.clone();
            let names = skills.read(cx).visible_names();
            if skills.read(cx).is_open() {
                names
            } else {
                Vec::new()
            }
        })
    }

    fn prompt_text(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) -> String {
        shell.update(cx, |shell, cx| {
            let input = shell.start_chat_dialog().unwrap().input.clone();
            input.read(cx).text().to_owned()
        })
    }

    /// `/` in the prompt lists the picked agent's skills for the item's
    /// resolved execution root (not the Space folder), on the owning device;
    /// arrows move, Enter accepts the composer's own `/name` form without
    /// starting, and Esc closes the list but not the dialog.
    #[gpui::test]
    fn prompt_offers_skills_for_the_resolved_execution_root(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let calls = seed(
            &shell,
            cx,
            execution_fixture::snapshot("s1"),
            true,
            validated_ok(),
            launched("chat-new"),
        );
        // Auth inherits API's configuration: root /work/alpha/flagship.
        shell
            .update(cx, |shell, cx| shell.open_start_chat("s1", API_AUTH, cx))
            .unwrap();
        draw(cx);
        let focus = shell.read_with(cx, |shell, cx| {
            let Some(WorkOutlineModal::StartChat(dialog)) = &shell.work_outline_modal else {
                panic!("dialog");
            };
            dialog.input_focus(cx)
        });
        cx.update(|window, cx| {
            window.activate_window();
            window.focus(&focus, cx);
        });
        draw(cx);
        cx.simulate_input("Fix it with /");
        draw(cx);
        let lists = calls_to(&calls, zeron_rpc::methods::LIST_COMMANDS);
        assert_eq!(lists.len(), 1);
        assert_eq!(lists[0]["cwd"], "/work/alpha/flagship");
        assert_eq!(lists[0]["targetDeviceId"], "d");
        assert!(lists[0]["harness"].is_string());
        // Start Chat keeps the harness's built-in commands.
        assert_eq!(
            prompt_skills(&shell, cx),
            vec!["code-review", "open-pr", "compact"]
        );
        assert!(cx.debug_bounds("skill-completion").is_some());
        assert!(cx.debug_bounds("skill-completion-row-1").is_some());

        // Esc closes the list; the dialog stays.
        cx.simulate_keystrokes("escape");
        draw(cx);
        assert!(prompt_skills(&shell, cx).is_empty());
        assert!(cx.debug_bounds("skill-completion").is_none());
        assert!(cx.debug_bounds("start-chat").is_some(), "dialog stays open");

        // Typing reopens and filters from the cache; Down + Enter accepts.
        cx.simulate_input("o");
        draw(cx);
        assert_eq!(calls_to(&calls, zeron_rpc::methods::LIST_COMMANDS).len(), 1);
        let names = prompt_skills(&shell, cx);
        assert!(names.len() >= 1 && names.contains(&"open-pr".to_owned()));
        let open_pr = names.iter().position(|name| name == "open-pr").unwrap();
        for _ in 0..open_pr {
            cx.simulate_keystrokes("down");
        }
        cx.simulate_keystrokes("enter");
        draw(cx);
        assert_eq!(prompt_text(&shell, cx), "Fix it with /open-pr ");
        assert!(prompt_skills(&shell, cx).is_empty());
        assert!(
            calls_to(&calls, zeron_rpc::methods::CREATE_CHAT_FOR_WORK_ITEM).is_empty(),
            "Enter accepted the skill instead of starting"
        );

        // Tab accepts too.
        cx.simulate_input("/cod");
        draw(cx);
        assert_eq!(prompt_skills(&shell, cx), vec!["code-review"]);
        cx.simulate_keystrokes("tab");
        draw(cx);
        assert_eq!(
            prompt_text(&shell, cx),
            "Fix it with /open-pr /code-review "
        );
    }

    /// An item rooted at a submodule folder below the Space path (the
    /// Design item: root `<Space>/ui`, `ui` bound as a pinned non-primary
    /// member) lists that folder's skills, never the Space folder's.
    #[gpui::test]
    fn prompt_offers_skills_for_a_submodule_root_below_the_space(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let mut snapshot = execution_fixture::snapshot("s1");
        let scope = ExecutionScope::WorkItem {
            work_item_id: HOUSEKEEPING.into(),
        };
        let mut binding = execution_fixture::binding("repo:ui", "/work/alpha/ui", true, false);
        binding.worktree_id = Some("checkout:ui".into());
        binding.worktree_path = Some("/work/alpha/ui".into());
        snapshot
            .execution_configurations
            .push(zeron_proto::ExecutionConfigurationView {
                id: scope.row_id(),
                space_id: "s1".into(),
                work_item_id: Some(HOUSEKEEPING.into()),
                configuration: Some(zeron_proto::ExecutionConfiguration {
                    execution_root: "/work/alpha/ui".into(),
                    bindings: vec![binding],
                    schema: 1,
                }),
                invalid_reason: None,
                updated_by: None,
                updated_at: 1,
            });
        let calls = seed(
            &shell,
            cx,
            snapshot,
            true,
            validated_ok(),
            launched("chat-new"),
        );
        shell
            .update(cx, |shell, cx| {
                shell.open_start_chat("s1", HOUSEKEEPING, cx)
            })
            .unwrap();
        draw(cx);
        let focus = shell.read_with(cx, |shell, cx| {
            let Some(WorkOutlineModal::StartChat(dialog)) = &shell.work_outline_modal else {
                panic!("dialog");
            };
            dialog.input_focus(cx)
        });
        cx.update(|window, cx| {
            window.activate_window();
            window.focus(&focus, cx);
        });
        draw(cx);
        cx.simulate_input("/");
        draw(cx);
        let lists = calls_to(&calls, zeron_rpc::methods::LIST_COMMANDS);
        assert_eq!(lists.len(), 1);
        assert_eq!(lists[0]["cwd"], "/work/alpha/ui");
        assert_eq!(lists[0]["targetDeviceId"], "d");
        assert!(!prompt_skills(&shell, cx).is_empty());
    }

    /// The prompt grows with its text from three rows up to 40% of the
    /// window, then scrolls internally with the caret in view; the wheel
    /// scrolls it; nothing caps (and clips) it from outside.
    #[gpui::test]
    fn prompt_grows_to_the_cap_then_scrolls(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        seed(
            &shell,
            cx,
            execution_fixture::snapshot("s1"),
            true,
            validated_ok(),
            launched("chat-new"),
        );
        shell
            .update(cx, |shell, cx| {
                shell.open_start_chat("s1", HOUSEKEEPING, cx)
            })
            .unwrap();
        draw(cx);
        let height = |cx: &mut gpui::VisualTestContext| {
            f32::from(cx.debug_bounds("start-chat-prompt").unwrap().size.height)
        };
        let window_height = cx.update(|window, _| f32::from(window.viewport_size().height));
        let empty = height(cx);
        set_prompt(&shell, cx, "one line");
        draw(cx);
        assert_eq!(height(cx), empty, "three rows minimum");
        set_prompt(&shell, cx, &vec!["line"; 8].join("\n"));
        draw(cx);
        let grown = height(cx);
        assert!(
            grown > empty + 30.0,
            "grows with content: {grown} vs {empty}"
        );
        let long = (0..200)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        set_prompt(&shell, cx, &long);
        draw(cx);
        let capped = height(cx);
        assert!(capped > grown);
        assert!(
            capped <= window_height * crate::composer::AUTO_GROW_MAX_FRACTION + 16.0,
            "capped at 40% of the window: {capped} of {window_height}"
        );
        let input = shell.update(cx, |shell, _| {
            shell.start_chat_dialog().unwrap().input.clone()
        });
        input.read_with(cx, |input, _| {
            assert!(input.scroll_top() > 0.0, "scrolled to the caret");
            assert!(input.caret_in_view());
        });
        let before = input.read_with(cx, |input, _| input.scroll_top());
        let center = cx.debug_bounds("start-chat-prompt").unwrap().center();
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: center,
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(120.0))),
            ..Default::default()
        });
        draw(cx);
        let after = input.read_with(cx, |input, _| input.scroll_top());
        assert!(
            after < before,
            "the wheel scrolls inside: {after} vs {before}"
        );
    }

    /// Claude Code (Fable first, as the live catalog leads) and Codex.
    fn picker_catalog() -> (
        Vec<zeron_engine::registry::HarnessDescriptor>,
        Vec<(zeron_proto::HarnessId, Vec<zeron_proto::Model>)>,
    ) {
        let harnesses = serde_json::from_value(serde_json::json!([
            {"id": "claude-code", "name": "Claude Code", "supportsSteering": true,
             "steeringMode": "step-boundary", "reasoningLevels": []},
            {"id": "codex", "name": "Codex", "supportsSteering": true,
             "steeringMode": "step-boundary", "reasoningLevels": []},
        ]))
        .unwrap();
        let claude = serde_json::from_value(serde_json::json!([
            {"id": "fable", "label": "Fable", "reasoningLevels": ["low", "medium", "high"]},
            {"id": "opus", "label": "Opus", "reasoningLevels": ["low", "medium", "high"]},
            {"id": "sonnet", "label": "Sonnet", "reasoningLevels": ["low", "medium", "high"]},
        ]))
        .unwrap();
        let codex = serde_json::from_value(serde_json::json!([
            {"id": "gpt-5.4", "label": "GPT-5.4", "reasoningLevels": ["low", "medium", "high"]},
        ]))
        .unwrap();
        (
            harnesses,
            vec![
                (zeron_proto::HarnessId::ClaudeCode, claude),
                (zeron_proto::HarnessId::Codex, codex),
            ],
        )
    }

    /// The composer remembers Claude Code · Opus · Medium for new chats
    /// while a Codex chat is selected; its catalog is loaded for device `d`.
    fn seed_composer_picker(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) {
        // A selected chat renders the chat route (its history columns).
        cx.update(|_, cx| {
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            )
        });
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.selected_space = Some("s1".into());
                state.selected_chat = Some(CHAT_API.into());
                state.apply_chat_config(
                    CHAT_API,
                    zeron_proto::ChatConfig {
                        harness: zeron_proto::HarnessId::Codex,
                        model: Some("gpt-5.4".into()),
                        reasoning: Some(zeron_proto::ReasoningLevel::Low),
                        model_options: Default::default(),
                        sandbox: zeron_proto::SandboxLevel::WorkspaceWrite,
                        auto_approve: true,
                    },
                );
            });
            let pickers = shell.composer.read(cx).pickers().clone();
            pickers.update(cx, |pickers, cx| {
                let (harnesses, models) = picker_catalog();
                pickers.seed_catalog_for_tests(harnesses, models, cx);
                let defaults = pickers.defaults_mut_for_tests();
                defaults.remember_model(
                    zeron_proto::HarnessId::ClaudeCode,
                    "opus".into(),
                    "Opus".into(),
                );
                defaults.reasoning = Some(zeron_proto::ReasoningLevel::Medium);
            });
        });
    }

    fn dialog_pickers(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) -> Entity<Pickers> {
        shell.update(cx, |shell, _| shell.start_chat_pickers().expect("dialog"))
    }

    fn focused(cx: &mut gpui::VisualTestContext, handle: &FocusHandle) -> bool {
        let handle = handle.clone();
        cx.update(|window, _| handle.is_focused(window))
    }

    /// The dialog hosts the composer's own model and approval chips, starts
    /// from exactly what a brand-new regular chat would use (the remembered
    /// Opus · Medium, not the harness's first model nor the selected chat's
    /// Codex config), and Start sends the picked model/effort/approval as the
    /// composer's `createChat` config — which the composer then remembers.
    #[gpui::test]
    fn start_chat_uses_the_composer_picker_and_sends_its_config(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let calls = seed(
            &shell,
            cx,
            execution_fixture::snapshot("s1"),
            true,
            validated_ok(),
            launched("chat-new"),
        );
        seed_composer_picker(&shell, cx);
        shell
            .update(cx, |shell, cx| {
                shell.open_start_chat("s1", HOUSEKEEPING, cx)
            })
            .unwrap();
        draw(cx);
        assert!(cx.debug_bounds("start-chat-agent").is_some());
        assert!(
            cx.debug_bounds("picker-model").is_some(),
            "the composer's model chip"
        );
        assert!(
            cx.debug_bounds("picker-approval-mode").is_some(),
            "the approval chip"
        );
        assert!(
            cx.debug_bounds("start-chat-agent-0").is_none(),
            "no harness list"
        );

        let pickers = dialog_pickers(&shell, cx);
        let initial = pickers.read_with(cx, |pickers, cx| pickers.resolved(cx));
        assert_eq!(initial.harness, Some(zeron_proto::HarnessId::ClaudeCode));
        assert_eq!(
            initial.model.as_deref(),
            Some("opus"),
            "not forced to Fable"
        );
        assert_eq!(initial.reasoning, Some(zeron_proto::ReasoningLevel::Medium));
        assert!(!initial.auto_approve, "new chats start at Ask");
        // Identical to the composer's own new-chat canvas resolution.
        let composer_pickers =
            shell.update(cx, |shell, cx| shell.composer.read(cx).pickers().clone());
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                state.selected_chat = None;
                cx.notify();
            })
        });
        draw(cx);
        let new_chat = composer_pickers.read_with(cx, |pickers, cx| pickers.resolved(cx));
        assert_eq!(initial, new_chat);

        // Keyboard: type, Tab to the model chip, Enter opens the composer's
        // picker; a search + Enter picks a model without starting the chat.
        let prompt = shell.read_with(cx, |shell, cx| {
            let Some(WorkOutlineModal::StartChat(dialog)) = &shell.work_outline_modal else {
                panic!("dialog");
            };
            dialog.input_focus(cx)
        });
        cx.update(|window, cx| {
            window.activate_window();
            window.focus(&prompt, cx);
        });
        draw(cx);
        cx.simulate_input("Ship it");
        let (model_chip, approval_chip) = pickers.read_with(cx, |pickers, _| {
            (
                pickers.model_chip_focus().clone(),
                pickers.approval_chip_focus().clone(),
            )
        });
        cx.simulate_keystrokes("tab");
        draw(cx);
        assert!(focused(cx, &model_chip), "Tab reaches the model chip");
        cx.simulate_keystrokes("tab");
        draw(cx);
        assert!(focused(cx, &approval_chip));
        cx.simulate_keystrokes("space");
        draw(cx);
        assert!(pickers.read_with(cx, |pickers, cx| pickers.resolved(cx).auto_approve));
        cx.simulate_keystrokes("shift-tab");
        draw(cx);
        assert!(focused(cx, &model_chip));
        cx.simulate_keystrokes("enter");
        draw(cx);
        assert!(pickers.read_with(cx, |pickers, _| pickers.is_open()));
        cx.simulate_input("sonnet");
        draw(cx);
        cx.simulate_keystrokes("enter");
        draw(cx);
        assert_eq!(
            pickers
                .read_with(cx, |pickers, cx| pickers.resolved(cx).model)
                .as_deref(),
            Some("sonnet")
        );
        assert!(calls_to(&calls, zeron_rpc::methods::CREATE_CHAT_FOR_WORK_ITEM).is_empty());
        pickers.update(cx, |pickers, cx| {
            pickers.pick_reasoning_for_tests(zeron_proto::ReasoningLevel::High, cx)
        });
        // Esc closes the picker only and hands focus back to the prompt.
        cx.simulate_keystrokes("escape");
        draw(cx);
        assert!(!pickers.read_with(cx, |pickers, _| pickers.is_open()));
        assert!(cx.debug_bounds("start-chat").is_some(), "dialog stays open");
        assert!(focused(cx, &prompt));

        // Enter in the prompt starts with the picked config.
        cx.simulate_keystrokes("enter");
        draw(cx);
        let creates = calls_to(&calls, zeron_rpc::methods::CREATE_CHAT_FOR_WORK_ITEM);
        assert_eq!(creates.len(), 1);
        assert_eq!(creates[0]["initialPrompt"]["text"], "Ship it");
        assert_eq!(
            creates[0]["config"],
            serde_json::to_value(zeron_proto::ChatConfig {
                harness: zeron_proto::HarnessId::ClaudeCode,
                model: Some("sonnet".into()),
                reasoning: Some(zeron_proto::ReasoningLevel::High),
                model_options: Default::default(),
                sandbox: zeron_proto::SandboxLevel::WorkspaceWrite,
                auto_approve: true,
            })
            .unwrap()
        );
        // The started chat's picks are the composer's last-used selection.
        composer_pickers.read_with(cx, |pickers, _| {
            let defaults = pickers.new_chat_defaults();
            assert_eq!(
                defaults
                    .model_for(zeron_proto::HarnessId::ClaudeCode)
                    .map(|m| m.id.as_str()),
                Some("sonnet")
            );
            assert_eq!(defaults.reasoning, Some(zeron_proto::ReasoningLevel::High));
        });
        shell.read_with(cx, |shell, _| assert!(shell.work_outline_modal.is_none()));
    }

    type PrCalls = Arc<Mutex<Vec<serde_json::Value>>>;

    /// Answer `StartPrReview` from `answer`, recording every request.
    fn script_pr_review(
        shell: &Entity<Shell>,
        cx: &mut gpui::VisualTestContext,
        answer: impl Fn(&serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync + 'static,
    ) -> PrCalls {
        let calls: PrCalls = Arc::default();
        let sink = calls.clone();
        shell.update(cx, |shell, _| {
            shell.pr_review_script = Some(Arc::new(move |params| {
                sink.lock().unwrap().push(params.clone());
                answer(params)
            }));
        });
        calls
    }

    fn pr_reply(chat: &str) -> serde_json::Value {
        serde_json::json!({
            "chatId": chat, "spaceId": "s1", "deviceId": "d",
            "title": "Review flagship#7: Add X",
            "prUrl": "https://github.com/acme/flagship/pull/7",
            "branch": "feature-x", "checkoutPath": "/work/alpha",
            "checkoutKind": "canonicalCheckout", "cwd": "/work/alpha",
            "behindBy": 0, "prompt": "/code-review …",
        })
    }

    fn open_pr_dialog(
        shell: &Entity<Shell>,
        cx: &mut gpui::VisualTestContext,
        action: PrReviewAction,
    ) {
        shell
            .update(cx, |shell, cx| {
                shell.open_start_pr_review(
                    PrReference::parse("acme/flagship#7").unwrap(),
                    action,
                    Some(("s1".into(), HOUSEKEEPING.into())),
                    cx,
                )
            })
            .unwrap();
        draw(cx);
    }

    /// A PR start opens the regular dialog, pre-filled with the review
    /// prompt, the PR link and where it runs, and the composer's model
    /// picker; nothing reaches the host until Start, which sends the picked
    /// model with the same item filing.
    #[gpui::test]
    fn a_pr_start_opens_the_dialog_and_starts_with_the_picked_model(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let calls = seed(
            &shell,
            cx,
            execution_fixture::snapshot("s1"),
            true,
            validated_ok(),
            launched("unused"),
        );
        seed_composer_picker(&shell, cx);
        let pr_calls = script_pr_review(&shell, cx, |_| Ok(pr_reply("chat-review")));
        open_pr_dialog(&shell, cx, PrReviewAction::CodeReview);

        for selector in [
            "start-chat",
            "start-chat-pr-prompt",
            "start-chat-pr-link",
            "start-chat-pr-root",
            "start-chat-agent",
            "picker-model",
            "picker-approval-mode",
        ] {
            assert!(cx.debug_bounds(selector).is_some(), "{selector} renders");
        }
        assert!(
            cx.debug_bounds("start-chat-prompt").is_none(),
            "no work-item prompt box"
        );
        shell.read_with(cx, |shell, _| {
            let Some(WorkOutlineModal::StartChat(dialog)) = &shell.work_outline_modal else {
                panic!("dialog");
            };
            let pr = dialog.pr.as_ref().expect("PR mode");
            assert_eq!(pr.reference.url(), "https://github.com/acme/flagship/pull/7");
            assert_eq!(
                crate::pr_review_launch::prompt_preview(pr.action, &pr.reference.url()),
                "/code-review https://github.com/acme/flagship/pull/7"
            );
            assert_eq!(pr.item, Some(("s1".to_string(), HOUSEKEEPING.to_string())));
        });
        assert!(pr_calls.lock().unwrap().is_empty(), "opening starts nothing");
        assert!(calls_to(&calls, zeron_rpc::methods::CREATE_CHAT_FOR_WORK_ITEM).is_empty());

        // Starts from the composer's remembered Opus; pick Sonnet by keyboard
        // (focus starts on the dialog, Tab walks to the model chip).
        let pickers = dialog_pickers(&shell, cx);
        assert_eq!(
            pickers
                .read_with(cx, |pickers, cx| pickers.resolved(cx).model)
                .as_deref(),
            Some("opus")
        );
        cx.update(|window, _| window.activate_window());
        draw(cx);
        let model_chip = pickers.read_with(cx, |pickers, _| pickers.model_chip_focus().clone());
        cx.simulate_keystrokes("tab");
        draw(cx);
        assert!(focused(cx, &model_chip), "Tab reaches the model chip");
        cx.simulate_keystrokes("enter");
        draw(cx);
        assert!(pickers.read_with(cx, |pickers, _| pickers.is_open()));
        cx.simulate_input("sonnet");
        draw(cx);
        cx.simulate_keystrokes("enter");
        draw(cx);
        assert_eq!(
            pickers
                .read_with(cx, |pickers, cx| pickers.resolved(cx).model)
                .as_deref(),
            Some("sonnet")
        );
        assert!(
            pr_calls.lock().unwrap().is_empty(),
            "picking a model starts nothing"
        );

        // Esc closes the picker only; the dialog stays open and Start sends.
        cx.simulate_keystrokes("escape");
        draw(cx);
        assert!(!pickers.read_with(cx, |pickers, _| pickers.is_open()));
        assert!(cx.debug_bounds("start-chat").is_some(), "dialog stays open");
        // Focus is back on the dialog, so Enter starts.
        cx.simulate_keystrokes("enter");
        draw(cx);
        let sent = pr_calls.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["repoOwner"], "acme");
        assert_eq!(sent[0]["repoName"], "flagship");
        assert_eq!(sent[0]["number"], 7);
        assert_eq!(sent[0]["action"], "codeReview");
        assert_eq!(sent[0]["spaceId"], "s1");
        assert_eq!(sent[0]["workItemId"], HOUSEKEEPING);
        assert_eq!(sent[0]["fresh"], true);
        assert_eq!(sent[0]["config"]["harness"], "claude_code");
        assert_eq!(sent[0]["config"]["model"], "sonnet");
        shell.read_with(cx, |shell, cx| {
            assert!(shell.work_outline_modal.is_none(), "the dialog closed");
            assert_eq!(
                shell.state.read(cx).selected_chat.as_deref(),
                Some("chat-review")
            );
        });
        // The picks are the composer's last-used selection.
        let composer_pickers =
            shell.update(cx, |shell, cx| shell.composer.read(cx).pickers().clone());
        composer_pickers.read_with(cx, |pickers, _| {
            assert_eq!(
                pickers
                    .new_chat_defaults()
                    .model_for(zeron_proto::HarnessId::ClaudeCode)
                    .map(|m| m.id.as_str()),
                Some("sonnet")
            );
        });
    }

    /// Enter on the dialog starts, Escape cancels without starting, and a
    /// refusal keeps the dialog so Retry reuses the same operation id.
    #[gpui::test]
    fn a_pr_dialog_starts_on_enter_cancels_on_escape_and_retries_after_a_refusal(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        seed(
            &shell,
            cx,
            execution_fixture::snapshot("s1"),
            true,
            validated_ok(),
            launched("unused"),
        );
        seed_composer_picker(&shell, cx);
        let refuse = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let flag = refuse.clone();
        let pr_calls = script_pr_review(&shell, cx, move |_| {
            if flag.load(std::sync::atomic::Ordering::SeqCst) {
                Err("pr-review/fork: PR 7 comes from a fork".into())
            } else {
                Ok(pr_reply("chat-fixed"))
            }
        });
        cx.update(|window, _| window.activate_window());

        open_pr_dialog(&shell, cx, PrReviewAction::FixCi);
        cx.simulate_keystrokes("escape");
        draw(cx);
        assert!(cx.debug_bounds("start-chat").is_none(), "Escape cancels");
        assert!(pr_calls.lock().unwrap().is_empty());

        open_pr_dialog(&shell, cx, PrReviewAction::FixCi);
        cx.simulate_keystrokes("enter");
        draw(cx);
        assert_eq!(pr_calls.lock().unwrap().len(), 1, "Enter starts");
        assert!(
            cx.debug_bounds("start-chat-failure-message").is_some(),
            "the refusal shows in the dialog, which stays open"
        );
        refuse.store(false, std::sync::atomic::Ordering::SeqCst);
        click(cx, "start-chat-start");
        let sent = pr_calls.lock().unwrap().clone();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0]["action"], "fixCi");
        assert_eq!(sent[0]["operationId"], sent[1]["operationId"]);
        shell.read_with(cx, |shell, cx| {
            assert!(shell.work_outline_modal.is_none());
            assert_eq!(
                shell.state.read(cx).selected_chat.as_deref(),
                Some("chat-fixed")
            );
        });
    }

    /// Esc closes an open picker first, then the dialog.
    #[gpui::test]
    fn escape_closes_the_picker_before_the_dialog(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        seed(
            &shell,
            cx,
            execution_fixture::snapshot("s1"),
            true,
            validated_ok(),
            launched("chat-new"),
        );
        seed_composer_picker(&shell, cx);
        shell
            .update(cx, |shell, cx| {
                shell.open_start_chat("s1", HOUSEKEEPING, cx)
            })
            .unwrap();
        draw(cx);
        cx.update(|window, _| window.activate_window());
        click(cx, "picker-model");
        let pickers = dialog_pickers(&shell, cx);
        assert!(pickers.read_with(cx, |pickers, _| pickers.is_open()));
        cx.simulate_keystrokes("escape");
        draw(cx);
        assert!(!pickers.read_with(cx, |pickers, _| pickers.is_open()));
        assert!(cx.debug_bounds("start-chat").is_some());
        cx.simulate_keystrokes("escape");
        draw(cx);
        assert!(cx.debug_bounds("start-chat").is_none());
        shell.read_with(cx, |shell, _| assert!(shell.work_outline_modal.is_none()));
    }
}
