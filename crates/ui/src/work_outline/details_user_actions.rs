//! Work Item Details → Milestone 4: the Space-level **Required actions**
//! section (the shared [`RequiredActionsList`] scoped to this item's subtree,
//! or the whole Space on the Space-root variant), the completion
//! confirmation for an item with open requests, the "Created by" line for
//! agent-created items, and the nonmodal **Proposed instructions** review.
//!
//! Proposals stay out of the way until the user opens them (a compact
//! indicator on Outline rows and in the origin Chat's header leads here).
//! Review shows the editable text, the target scope, the descendants the
//! instruction would reach, and the proposing Chat. Approve sends the
//! reviewed text (`approveWorkItemInstructionProposal`), first through the
//! drift confirmation when it changes an assigned Chat's effective
//! instructions; Reject takes an optional reason. Pending, approved, and
//! rejected outcomes stay listed (history collapsed).
use super::*;

use mast_proto::{
    ApproveWorkItemInstructionProposalParams, ProposalAction,
    RejectWorkItemInstructionProposalParams, WorkItemInstructionProposalView, WorkOutlineActorKind,
    WorkOutlineErrorCode,
};

use crate::work_outline::proposal_diff::{self, DiffKind, ProposalGroup, Standing};

use crate::work_outline::actions::{OperationSlot, instruction_error_text};
use crate::work_outline::instructions::{self, OutlineChange};
use crate::work_outline::required_actions::{self, RequestScope, SpaceSnapshot};
use crate::work_outline::required_actions_view::{RequiredActionsEvent, RequiredActionsList};

/// The proposal under review (one at a time).
#[derive(Debug, Clone)]
pub(crate) struct ProposalReview {
    pub proposal_id: String,
    /// The proposed text (dirty = the input differs).
    pub original: String,
    /// The reject reason field is open.
    pub rejecting: bool,
    /// Approve: reused across retries of the same reviewed text.
    pub approve_op: OperationSlot,
    /// Reject: reused across retries of the same reason.
    pub reject_op: OperationSlot,
}

impl WorkItemDetails {
    /// The embedded Required Actions list for this surface's scope.
    pub(super) fn new_required_actions(
        state: &Entity<AppState>,
        space_id: &str,
        item_id: &str,
        cx: &mut Context<Self>,
    ) -> (Entity<RequiredActionsList>, Subscription) {
        let scope = if item_id == SPACE_ROOT_ITEM_ID {
            RequestScope::Space(space_id.to_owned())
        } else {
            RequestScope::Subtree {
                space_id: space_id.to_owned(),
                item_id: item_id.to_owned(),
            }
        };
        let list = cx.new(|cx| RequiredActionsList::new(state.clone(), scope, false, cx));
        let events = cx.subscribe(&list, |_: &mut Self, _, event, cx| match event {
            RequiredActionsEvent::OpenChat(chat_id) => {
                cx.emit(DetailsEvent::OpenChat(chat_id.clone()))
            }
            RequiredActionsEvent::OpenItem { space_id, item_id } => {
                cx.emit(DetailsEvent::OpenItem {
                    space_id: space_id.clone(),
                    item_id: item_id.clone(),
                })
            }
            RequiredActionsEvent::Close => {}
        });
        (list, events)
    }

    #[cfg(test)]
    pub(crate) fn required_actions_list(&self) -> Entity<RequiredActionsList> {
        self.required_actions.clone()
    }

    /// Expand Proposed instructions (the Outline / chat-header indicators).
    pub(crate) fn open_proposals(&mut self, cx: &mut Context<Self>) {
        self.proposals_open = true;
        cx.notify();
    }

    fn chat_label(&self, chat_id: Option<&str>) -> (String, bool) {
        match chat_id.and_then(|id| self.chats.iter().find(|chat| chat.id == id)) {
            Some(chat) if !chat.archived => (chat.title.clone(), true),
            _ if chat_id.is_some() => (required_actions::CHAT_UNAVAILABLE.to_owned(), false),
            _ => ("an agent".to_owned(), false),
        }
    }

    // ----------------------------------------------------------------------
    // Completion with open requests
    // ----------------------------------------------------------------------

    /// Completing an item that still has open requests asks first; confirm
    /// completes and resolves them (`resolveOpenRequests: true`).
    pub(super) fn request_completion(&mut self, cx: &mut Context<Self>) {
        let open = self
            .item()
            .map_or(0, |item| item.rollup.unresolved_user_action_count as usize);
        if open == 0 {
            self.send_completion(false, cx);
            return;
        }
        self.confirm_completion(open, cx);
    }

    fn confirm_completion(&mut self, open: usize, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        let noun = instructions::plural(
            open,
            "open user action request",
            "open user action requests",
        );
        let request = WorkOutlineConfirmRequest {
            title: "Complete with open requests?".into(),
            body: format!(
                "This work item has {noun}. Completing it resolves {} and records the resolution \
                 in Required Actions.",
                if open == 1 { "it" } else { "them" }
            )
            .into(),
            confirm_label: "Complete and resolve".into(),
            cancel_label: "Cancel".into(),
            destructive: false,
            on_decision: Box::new(move |confirmed, _window, cx| {
                if confirmed {
                    weak.update(cx, |this, cx| this.send_completion(true, cx))
                        .ok();
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
            drop(request);
            self.error = Some("Completing needs the app window to confirm".into());
            cx.notify();
        }
    }

    fn send_completion(&mut self, resolve_open_requests: bool, cx: &mut Context<Self>) {
        let task =
            self.state
                .read(cx)
                .work_outline_actions()
                .update_work_item(UpdateWorkItemParams {
                    work_item_id: self.item_id.clone(),
                    changes: WorkItemChanges {
                        status: Some(WorkItemStatus::Completed),
                        resolve_open_requests,
                        ..Default::default()
                    },
                    operation_id: new_operation_id(),
                });
        let key = "status".to_owned();
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
                    Ok(_) => {}
                    // A request arrived after the click: ask with the host's count.
                    Err(error)
                        if error.code == WorkOutlineErrorCode::OpenUserActionRequests
                            && !resolve_open_requests =>
                    {
                        let open = error.open_request_count().unwrap_or(1);
                        this.confirm_completion(open, cx);
                    }
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ----------------------------------------------------------------------
    // Proposals
    // ----------------------------------------------------------------------

    /// The proposal review whose text differs from the proposal.
    pub(crate) fn proposal_review_dirty(&self, cx: &gpui::App) -> bool {
        self.proposal_review.as_ref().is_some_and(|review| {
            self.proposal_input.read(cx).text().trim() != review.original.trim()
        })
    }

    fn proposal(&self, id: &str) -> Option<WorkItemInstructionProposalView> {
        self.snapshot.as_ref()?.instruction_proposal(id).cloned()
    }

    /// Open the review of `proposal_id` with its text in the editor. A dirty
    /// review of another proposal is never replaced silently.
    pub(crate) fn begin_proposal_review(
        &mut self,
        proposal_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.proposal_review_dirty(cx) {
            self.proposals_error = Some("Approve, reject, or cancel the open review first".into());
            cx.notify();
            return;
        }
        let Some(proposal) = self.proposal(&proposal_id) else {
            return;
        };
        self.proposals_open = true;
        self.proposals_error = None;
        self.proposal_review = Some(ProposalReview {
            proposal_id,
            original: proposal.text.clone(),
            rejecting: false,
            approve_op: OperationSlot::default(),
            reject_op: OperationSlot::default(),
        });
        self.proposal_input
            .update(cx, |input, cx| input.set_text(proposal.text, cx));
        self.reason_input
            .update(cx, |input, cx| input.set_text("", cx));
        let handle = self.proposal_input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Tests drive the review editor through this.
    #[cfg(test)]
    pub(crate) fn set_proposal_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.proposal_input
            .update(cx, |input, cx| input.set_text(text, cx));
    }

    #[cfg(test)]
    pub(crate) fn set_reject_reason(&mut self, text: &str, cx: &mut Context<Self>) {
        self.reason_input
            .update(cx, |input, cx| input.set_text(text, cx));
    }

    pub(crate) fn cancel_proposal_review(&mut self, cx: &mut Context<Self>) {
        self.proposal_review = None;
        self.proposals_error = None;
        cx.notify();
    }

    /// The drift-confirmed change accepting `proposal` with `text` makes.
    fn proposal_change(
        &self,
        proposal: &WorkItemInstructionProposalView,
        text: &str,
    ) -> OutlineChange {
        match (proposal.action, proposal.instruction_id.clone()) {
            (ProposalAction::Edit, Some(instruction_id)) => OutlineChange::EditInstruction {
                instruction_id,
                text: text.to_owned(),
            },
            (ProposalAction::Remove, Some(instruction_id)) => {
                OutlineChange::ArchiveInstruction { instruction_id }
            }
            _ => OutlineChange::AddInstruction {
                work_item_id: required_actions::proposal_target_item(proposal).map(str::to_owned),
                text: text.to_owned(),
            },
        }
    }

    /// Approve with the reviewed text, through the drift confirmation when
    /// the change affects an assigned Chat's effective instructions.
    pub(crate) fn approve_proposal(&mut self, cx: &mut Context<Self>) {
        let Some(review) = self.proposal_review.clone() else {
            return;
        };
        let Some(proposal) = self.proposal(&review.proposal_id) else {
            self.proposals_error = Some("That proposal is gone".into());
            cx.notify();
            return;
        };
        let text = self.proposal_input.read(cx).text().trim().to_owned();
        if text.is_empty() {
            self.proposals_error = Some("Enter the instruction text".into());
            cx.notify();
            return;
        }
        let max = mast_proto::user_action_limits::MAX_PROPOSAL_TEXT_CHARS;
        let chars = text.chars().count();
        if chars > max {
            self.proposals_error = Some(format!(
                "This instruction is {chars} characters; the limit is {max}"
            ));
            cx.notify();
            return;
        }
        self.accept_with(proposal, text, cx);
    }

    /// Accept an edit or remove as proposed (no editor). A stale proposal is
    /// applied anyway: the card already showed the three-way view.
    pub(crate) fn accept_proposal(&mut self, proposal_id: &str, cx: &mut Context<Self>) {
        let Some(proposal) = self.proposal(proposal_id) else {
            self.proposals_error = Some("That proposal is gone".into());
            cx.notify();
            return;
        };
        let text = proposal.text.clone();
        self.accept_with(proposal, text, cx);
    }

    fn accept_with(
        &mut self,
        proposal: WorkItemInstructionProposalView,
        text: String,
        cx: &mut Context<Self>,
    ) {
        let allow_stale = self.snapshot.as_ref().is_some_and(|snapshot| {
            matches!(
                proposal_diff::standing(snapshot, &proposal),
                Standing::Stale { .. }
            )
        });
        let change = self.proposal_change(&proposal, &text);
        let proposal_id = proposal.id.clone();
        self.with_drift_confirm(change, cx, move |this, cx| {
            this.send_approve(proposal_id, text, allow_stale, cx)
        });
    }

    /// Dismiss without a reason (an edit or remove card's one-click path).
    pub(crate) fn dismiss_proposal(&mut self, proposal_id: &str, cx: &mut Context<Self>) {
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .reject_instruction_proposal(RejectWorkItemInstructionProposalParams {
                proposal_id: proposal_id.to_owned(),
                reason: None,
                operation_id: new_operation_id(),
            });
        self.run_proposal(format!("proposal:{proposal_id}"), task, cx);
    }

    fn send_approve(
        &mut self,
        proposal_id: String,
        text: String,
        allow_stale: bool,
        cx: &mut Context<Self>,
    ) {
        let operation_id = match self.proposal_review.as_mut() {
            Some(review) if review.proposal_id == proposal_id => review.approve_op.id_for(&text),
            _ => new_operation_id(),
        };
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .approve_instruction_proposal(ApproveWorkItemInstructionProposalParams {
                proposal_id: proposal_id.clone(),
                approved_text: text,
                placement: None,
                allow_stale,
                operation_id,
            });
        self.run_proposal(format!("proposal:{proposal_id}"), task, cx);
    }

    /// Open the optional-reason field (first click) or reject (second).
    pub(crate) fn reject_proposal(&mut self, cx: &mut Context<Self>) {
        let Some(review) = self.proposal_review.as_mut() else {
            return;
        };
        if !review.rejecting {
            review.rejecting = true;
            cx.notify();
            return;
        }
        let reason: String = self
            .reason_input
            .read(cx)
            .text()
            .trim()
            .chars()
            .take(mast_proto::user_action_limits::MAX_NOTE_CHARS)
            .collect();
        let review = self.proposal_review.as_mut().expect("checked above");
        let operation_id = review.reject_op.id_for(&reason);
        let proposal_id = review.proposal_id.clone();
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .reject_instruction_proposal(RejectWorkItemInstructionProposalParams {
                proposal_id: proposal_id.clone(),
                reason: (!reason.is_empty()).then_some(reason),
                operation_id,
            });
        self.run_proposal(format!("proposal:{proposal_id}"), task, cx);
    }

    fn run_proposal(
        &mut self,
        key: String,
        task: super::super::actions::ActionTask,
        cx: &mut Context<Self>,
    ) {
        if !self.in_flight.insert(key.clone()) {
            return;
        }
        self.proposals_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.in_flight.remove(&key);
                match result {
                    Ok(_) => this.proposal_review = None,
                    Err(error) => this.proposals_error = Some(instruction_error_text(&error)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ----------------------------------------------------------------------
    // Rendering
    // ----------------------------------------------------------------------

    /// "Created by an agent in <Chat>" for agent-created items.
    pub(super) fn render_created_by(
        &self,
        item: &WorkItemView,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let actor = item.created_by.as_ref()?;
        if actor.kind != WorkOutlineActorKind::Agent {
            return None;
        }
        let (label, available) = self.chat_label(actor.chat_id.as_deref());
        let chat_id = actor.chat_id.clone();
        Some(
            div()
                .debug_selector(|| "work-item-created-by".into())
                .flex()
                .items_center()
                .gap(px(4.0))
                .text_size(crate::typography::ui_rems(10.5))
                .text_color(theme.text_faint)
                .child("Created by an agent in")
                .child(
                    div()
                        .id("work-item-created-by-chat")
                        .debug_selector(|| "work-item-created-by-chat".into())
                        .min_w_0()
                        .truncate()
                        .when(available, |el| {
                            el.cursor_pointer()
                                .text_color(theme.text_muted)
                                .hover(|el| el.text_color(theme.text))
                        })
                        .when(!available, |el| el.italic())
                        .child(SharedString::from(label))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            if let (true, Some(chat_id)) = (available, chat_id.clone()) {
                                cx.emit(DetailsEvent::OpenChat(chat_id));
                            }
                        })),
                )
                .into_any_element(),
        )
    }

    /// The Required actions section (header with the open count; open by
    /// default while something is open).
    pub(super) fn render_required_actions_section(
        &mut self,
        snapshot: &Arc<WorkOutlineSnapshot>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = self
            .state
            .read(cx)
            .space_row(&self.space_id)
            .map(|space| space.display_name().to_owned())
            .unwrap_or_default();
        let spaces = vec![SpaceSnapshot {
            space_id: self.space_id.clone(),
            label,
            snapshot: snapshot.clone(),
        }];
        let chats = self.chats.clone();
        self.required_actions
            .update(cx, |list, _| list.set_data(spaces, chats));
        let rows = self.required_actions.read(cx).rows();
        let open_count = rows.open.len();
        // A Reply & resolve keeps the section open so its Open chat link
        // stays in reach after the last open request resolved.
        let open = self
            .required_actions_open
            .unwrap_or(open_count > 0 || self.required_actions.read(cx).has_replied());
        let header_color = if open_count > 0 {
            chips::attention_color(theme)
        } else {
            theme.text_muted
        };
        div()
            .debug_selector(|| "work-item-required-actions".into())
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                Self::section_header(
                    "work-item-required-actions-toggle",
                    format!("Required actions ({open_count})"),
                    open,
                    theme,
                )
                .text_color(header_color)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.required_actions_open = Some(!open);
                    cx.notify();
                })),
            )
            .when(open, |el| el.child(self.required_actions.clone()))
            .into_any_element()
    }

    /// Proposed instructions for this scope; `None` when there are none.
    pub(super) fn render_proposals_section(
        &mut self,
        snapshot: &WorkOutlineSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let scope_item = (!self.is_space_root()).then_some(self.item_id.as_str());
        let proposals: Vec<WorkItemInstructionProposalView> =
            required_actions::proposals_for(snapshot, scope_item)
                .cloned()
                .collect();
        if proposals.is_empty() {
            return None;
        }
        let (pending, reviewed): (Vec<_>, Vec<_>) = proposals
            .into_iter()
            .partition(required_actions::is_pending);
        let open = self.proposals_open || self.proposal_review.is_some();
        let now = chrono::Utc::now();
        let mut section = div()
            .debug_selector(|| "work-item-proposals".into())
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                Self::section_header(
                    "work-item-proposals-toggle",
                    if pending.is_empty() {
                        "Proposed instructions".to_owned()
                    } else {
                        format!("Proposed instructions ({} pending)", pending.len())
                    },
                    open,
                    theme,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.proposals_open = !this.proposals_open;
                    cx.notify();
                })),
            );
        if !open {
            return Some(section.into_any_element());
        }
        if pending.is_empty() {
            section = section.child(
                div()
                    .pl(px(14.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_faint)
                    .child("No pending proposals"),
            );
        }
        let target_label = match scope_item {
            Some(id) => model::path_label(
                model::item_path(snapshot, id)
                    .iter()
                    .map(|item| item.title.as_str()),
            ),
            None => "Space root".to_owned(),
        };
        let reach = instructions::affected_descendant_count(snapshot, scope_item);
        for group in proposal_diff::group_pending(pending) {
            section = section.child(self.render_proposal_group(
                group,
                snapshot,
                &target_label,
                reach,
                now,
                theme,
                cx,
            ));
        }
        if !reviewed.is_empty() {
            let history_open = self.proposal_history_open;
            section = section.child(
                div()
                    .id("work-item-proposals-history-toggle")
                    .debug_selector(|| "work-item-proposals-history-toggle".into())
                    .pl(px(14.0))
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_muted)
                    .hover(|el| el.text_color(theme.text))
                    .child(SharedString::from(format!(
                        "{} History ({})",
                        if history_open { "\u{25be}" } else { "\u{25b8}" },
                        reviewed.len()
                    )))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.proposal_history_open = !this.proposal_history_open;
                        cx.notify();
                    })),
            );
            if history_open {
                for proposal in reviewed {
                    section = section.child(Self::render_reviewed_proposal(&proposal, now, theme));
                }
            }
        }
        if let Some(error) = self.proposals_error.clone() {
            section = section.child(
                div()
                    .debug_selector(|| "work-item-proposals-error".into())
                    .pl(px(14.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.warning)
                    .child(SharedString::from(error)),
            );
        }
        Some(section.into_any_element())
    }

    /// One group of pending proposals. Edits and removes of one instruction
    /// share a header with that instruction's current text; an add is alone.
    #[allow(clippy::too_many_arguments)]
    fn render_proposal_group(
        &self,
        group: ProposalGroup,
        snapshot: &WorkOutlineSnapshot,
        target_label: &str,
        reach: usize,
        now: chrono::DateTime<chrono::Utc>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut column = div().flex().flex_col().gap(px(4.0));
        if let Some(instruction_id) = group.instruction_id.as_deref() {
            let current = snapshot
                .instructions
                .iter()
                .find(|i| i.id == instruction_id)
                .map(|i| i.text.clone())
                .or_else(|| group.proposals[0].base_text.clone())
                .unwrap_or_default();
            let count = group.proposals.len();
            let selector = format!("work-item-proposal-group-{instruction_id}");
            column = column
                .debug_selector(move || selector.clone())
                .pl(px(14.0))
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(10.5))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(if count > 1 {
                            format!("Instruction \u{b7} {count} proposed changes")
                        } else {
                            "Instruction".to_owned()
                        })),
                )
                .child(
                    div()
                        .max_h(px(54.0))
                        .overflow_hidden()
                        .text_size(crate::typography::ui_rems(11.5))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(current)),
                );
        }
        for proposal in &group.proposals {
            column = column.child(self.render_pending_proposal(
                proposal,
                snapshot,
                target_label,
                reach,
                now,
                theme,
                cx,
            ));
        }
        column.into_any_element()
    }

    /// An inline diff: added words tinted, removed words struck through.
    fn diff_text(runs: &[(DiffKind, String)], selector: String, theme: &Theme) -> AnyElement {
        let (text, ranges) = proposal_diff::diff_layout(runs);
        let added = gpui::HighlightStyle {
            background_color: Some(theme.diff_add.opacity(0.22)),
            ..Default::default()
        };
        let removed = gpui::HighlightStyle {
            color: Some(theme.diff_del),
            strikethrough: Some(gpui::StrikethroughStyle {
                thickness: px(1.0),
                color: Some(theme.diff_del),
            }),
            ..Default::default()
        };
        let highlights = ranges.into_iter().map(|(range, kind)| {
            (
                range,
                if kind == DiffKind::Added {
                    added
                } else {
                    removed
                },
            )
        });
        div()
            .debug_selector(move || selector.clone())
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text)
            .child(gpui::StyledText::new(text).with_highlights(highlights))
            .into_any_element()
    }

    fn rationale_line(
        proposal: &WorkItemInstructionProposalView,
        theme: &Theme,
    ) -> Option<AnyElement> {
        let rationale = proposal.rationale.as_deref().filter(|r| !r.is_empty())?;
        let selector = format!("work-item-proposal-rationale-{}", proposal.id);
        Some(
            div()
                .debug_selector(move || selector.clone())
                .text_size(crate::typography::ui_rems(10.5))
                .text_color(theme.text_muted)
                .italic()
                .child(SharedString::from(format!("Why: {rationale}")))
                .into_any_element(),
        )
    }

    /// The body of a pending edit or remove card: the diff (or the struck
    /// instruction) and, when the instruction changed since the proposal,
    /// the three-way view.
    fn modification_body(
        proposal: &WorkItemInstructionProposalView,
        standing: &Standing,
        theme: &Theme,
    ) -> AnyElement {
        let id = &proposal.id;
        let base = proposal.base_text.clone().unwrap_or_default();
        let proposed = |runs_from: &str| {
            if proposal.action == ProposalAction::Remove {
                vec![(DiffKind::Removed, runs_from.to_owned())]
            } else {
                proposal_diff::word_diff(runs_from, &proposal.text)
            }
        };
        let mut body = div().flex().flex_col().gap(px(3.0));
        let label = |text: &'static str| {
            div()
                .text_size(crate::typography::ui_rems(10.0))
                .text_color(theme.text_faint)
                .child(text)
        };
        match standing {
            Standing::Current => {
                body = body.child(Self::diff_text(
                    &proposed(&base),
                    format!("work-item-proposal-diff-{id}"),
                    theme,
                ));
            }
            Standing::Stale { now } => {
                let notice = if now.is_some() {
                    "This instruction changed after the proposal was made. Apply it anyway or dismiss it."
                } else {
                    "This instruction was archived after the proposal was made."
                };
                body = body
                    .child(
                        div()
                            .debug_selector({
                                let selector = format!("work-item-proposal-stale-{id}");
                                move || selector.clone()
                            })
                            .text_size(crate::typography::ui_rems(10.5))
                            .text_color(theme.warning)
                            .child(notice),
                    )
                    .child(label("When proposed"))
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(base.clone())),
                    );
                if let Some(now) = now {
                    body = body
                        .child(label("Now (changes since)"))
                        .child(Self::diff_text(
                            &proposal_diff::word_diff(&base, now),
                            format!("work-item-proposal-now-{id}"),
                            theme,
                        ));
                }
                body = body
                    .child(label(if proposal.action == ProposalAction::Remove {
                        "Proposed: remove"
                    } else {
                        "Proposed (against when proposed)"
                    }))
                    .child(Self::diff_text(
                        &proposed(&base),
                        format!("work-item-proposal-diff-{id}"),
                        theme,
                    ));
            }
        }
        if let Some(line) = Self::rationale_line(proposal, theme) {
            body = body.child(line);
        }
        body.into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_pending_proposal(
        &self,
        proposal: &WorkItemInstructionProposalView,
        snapshot: &WorkOutlineSnapshot,
        target_label: &str,
        reach: usize,
        now: chrono::DateTime<chrono::Utc>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = proposal.id.clone();
        let (chat, available) = self.chat_label(proposal.source_ref.chat_id.as_deref());
        let reviewing = self
            .proposal_review
            .as_ref()
            .filter(|review| review.proposal_id == id);
        let busy = self.in_flight.contains(&format!("proposal:{id}"));
        let open_chat = proposal.source_ref.chat_id.clone();
        let provenance = div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .text_size(crate::typography::ui_rems(10.5))
            .text_color(theme.text_faint)
            .child("Proposed by")
            .child(
                div()
                    .id(SharedString::from(format!("work-item-proposal-chat-{id}")))
                    .min_w_0()
                    .max_w(px(160.0))
                    .truncate()
                    .when(available, |el| {
                        el.cursor_pointer()
                            .text_color(theme.text_muted)
                            .hover(|el| el.text_color(theme.text))
                    })
                    .when(!available, |el| el.italic())
                    .child(SharedString::from(chat))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        if let (true, Some(chat_id)) = (available, open_chat.clone()) {
                            cx.emit(DetailsEvent::OpenChat(chat_id));
                        }
                    })),
            )
            .child("\u{b7}")
            .child(SharedString::from(required_actions::age_label(
                proposal.created_at,
                now,
            )));
        let selector = format!("work-item-proposal-{id}");
        let mut row = div()
            .debug_selector(move || selector.clone())
            .flex()
            .flex_col()
            .gap(px(4.0))
            .pl(px(14.0))
            .py(px(2.0))
            .child(provenance);
        let is_modification = proposal.action != ProposalAction::Add;
        let Some(review) = reviewing else {
            let review_id = id.clone();
            if is_modification {
                let standing = proposal_diff::standing(snapshot, proposal);
                let stale = matches!(standing, Standing::Stale { .. });
                let archived = matches!(standing, Standing::Stale { now: None });
                let accept_id = id.clone();
                let dismiss_id = id.clone();
                let accept_label = match (stale, busy) {
                    (_, true) => "Accepting\u{2026}",
                    (true, false) => "Apply anyway",
                    (false, false) => "Accept",
                };
                let mut buttons = div().flex().gap(px(6.0));
                if !archived {
                    buttons = buttons.child(
                        Self::small_button(
                            SharedString::from(format!("work-item-proposal-accept-{id}")),
                            accept_label,
                            theme,
                        )
                        .debug_selector({
                            let selector = format!("work-item-proposal-accept-{id}");
                            move || selector.clone()
                        })
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.accept_proposal(&accept_id, cx)),
                        ),
                    );
                    if proposal.action == ProposalAction::Edit {
                        buttons = buttons.child(
                            Self::small_button(
                                SharedString::from(format!("work-item-proposal-review-{id}")),
                                "Edit then accept",
                                theme,
                            )
                            .debug_selector({
                                let selector = format!("work-item-proposal-review-{id}");
                                move || selector.clone()
                            })
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.begin_proposal_review(review_id.clone(), window, cx)
                                },
                            )),
                        );
                    }
                }
                buttons = buttons.child(
                    Self::small_button(
                        SharedString::from(format!("work-item-proposal-dismiss-{id}")),
                        "Dismiss",
                        theme,
                    )
                    .debug_selector({
                        let selector = format!("work-item-proposal-dismiss-{id}");
                        move || selector.clone()
                    })
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.dismiss_proposal(&dismiss_id, cx)),
                    ),
                );
                return row
                    .child(Self::modification_body(proposal, &standing, theme))
                    .child(buttons)
                    .into_any_element();
            }
            return row
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text)
                        .child(SharedString::from(proposal.text.clone())),
                )
                .children(Self::rationale_line(proposal, theme))
                .child(
                    div().child(
                        Self::small_button(
                            SharedString::from(format!("work-item-proposal-review-{id}")),
                            "Review",
                            theme,
                        )
                        .debug_selector({
                            let selector = format!("work-item-proposal-review-{id}");
                            move || selector.clone()
                        })
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.begin_proposal_review(review_id.clone(), window, cx)
                            },
                        )),
                    ),
                )
                .into_any_element();
        };
        let rejecting = review.rejecting;
        row = row
            .child(Self::editor_frame(
                &self.proposal_input,
                &self.proposal_skills,
                "work-item-proposal-editor",
                theme,
            ))
            .child(
                div()
                    .debug_selector(|| "work-item-proposal-target".into())
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(format!(
                        "Target: {target_label} \u{b7} reaches {}",
                        instructions::plural(
                            reach,
                            "descendant work item",
                            "descendant work items"
                        )
                    ))),
            );
        if rejecting {
            row = row.child(
                div()
                    .debug_selector(|| "work-item-proposal-reason".into())
                    .child(Self::input_frame(&self.reason_input, theme)),
            );
        }
        row.child(
            div()
                .flex()
                .gap(px(6.0))
                .when(!rejecting, |el| {
                    el.child(
                        Self::small_button(
                            "work-item-proposal-approve",
                            match (busy, is_modification) {
                                (true, _) => "Approving\u{2026}",
                                (false, true) => "Accept",
                                (false, false) => "Approve",
                            },
                            theme,
                        )
                        .debug_selector(|| "work-item-proposal-approve".into())
                        .on_click(cx.listener(|this, _, _, cx| this.approve_proposal(cx))),
                    )
                })
                .child(
                    Self::small_button(
                        "work-item-proposal-reject",
                        if rejecting {
                            "Reject"
                        } else {
                            "Reject\u{2026}"
                        },
                        theme,
                    )
                    .debug_selector(|| "work-item-proposal-reject".into())
                    .on_click(cx.listener(|this, _, _, cx| this.reject_proposal(cx))),
                )
                .child(
                    Self::small_button("work-item-proposal-cancel", "Cancel", theme)
                        .debug_selector(|| "work-item-proposal-cancel".into())
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_proposal_review(cx))),
                ),
        )
        .into_any_element()
    }

    fn render_reviewed_proposal(
        proposal: &WorkItemInstructionProposalView,
        now: chrono::DateTime<chrono::Utc>,
        theme: &Theme,
    ) -> AnyElement {
        let approved = proposal.status == mast_proto::InstructionProposalStatus::Approved;
        let review = proposal.review.as_ref();
        let mut outcome = format!(
            "{} {}",
            if approved { "Accepted" } else { "Rejected" },
            review.map_or_else(String::new, |review| required_actions::age_label(
                review.at, now
            ))
        );
        if let Some(reason) = review
            .and_then(|review| review.reason.as_deref())
            .filter(|reason| !reason.trim().is_empty())
        {
            outcome.push_str(" \u{b7} ");
            outcome.push_str(reason);
        }
        let selector = format!("work-item-proposal-history-{}", proposal.id);
        div()
            .debug_selector(move || selector.clone())
            .pl(px(14.0))
            .flex()
            .flex_col()
            .child(
                div()
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(match proposal.action {
                        ProposalAction::Add => proposal.text.clone(),
                        ProposalAction::Edit => format!("Edit: {}", proposal.text),
                        ProposalAction::Remove => format!(
                            "Remove: {}",
                            proposal.base_text.clone().unwrap_or_default()
                        ),
                    })),
            )
            .child(
                div()
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(if approved {
                        theme.success
                    } else {
                        theme.text_faint
                    })
                    .child(SharedString::from(outcome)),
            )
            .into_any_element()
    }
}
