//! Work Item Details → **Instructions** (local list, add/edit with explicit
//! Save/Cancel, reorder, remove (archive), provenance badge with the Chat excerpt on
//! demand, the inheritance toggle) and **Effective instructions** (ordered
//! entries with source, local/inherited markers, the precedence rule, and
//! budget usage). Also the Space-root variant of the details surface.
//!
//! Persistence policy (acceptance 28): an instruction is written only by
//! Save (button or Enter); Cancel discards the draft; blur never saves;
//! closing the surface, switching items, opening a Chat, or Escape with an
//! unsaved draft goes through `confirm_discard` ("Keep Editing / Discard").
//! Edits and archives that change an assigned Chat's effective instructions,
//! and the inheritance toggle, go through the drift confirmation first.
use super::*;

/// Instructions longer than this many lines are clipped until "Show more".
pub(crate) const COLLAPSED_LINES: usize = 4;
/// Line height (rems) of the instruction text; the clip is an exact multiple.
const LINE_HEIGHT_REMS: f32 = 16.0;
/// Rough characters per wrapped line in the details panel, used to decide
/// whether an instruction would exceed [`COLLAPSED_LINES`].
const CHARS_PER_LINE: usize = 56;

/// Whether `text` probably wraps past [`COLLAPSED_LINES`]: explicit lines plus
/// an estimate of soft wrapping. Short instructions get no toggle.
pub(crate) fn needs_collapse(text: &str) -> bool {
    let lines: usize = text
        .lines()
        .map(|line| line.chars().count().div_ceil(CHARS_PER_LINE).max(1))
        .sum();
    lines > COLLAPSED_LINES
}

use mast_proto::{
    ArchiveWorkItemInstructionParams, CreateWorkItemInstructionParams, InstructionPlacement,
    InstructionScope, MoveWorkItemInstructionParams, UpdateWorkItemInstructionParams,
    WorkItemInstructionView, WorkOutlineError,
};

use crate::work_outline::actions::{OperationSlot, instruction_error_text};
use crate::work_outline::command::WorkOutlineDriftRequest;
use crate::work_outline::instructions::{self, OutlineChange};

/// The one open instruction editor.
#[derive(Debug, Clone)]
pub(crate) struct InstructionDraft {
    /// `None` = a new instruction (appended last).
    pub instruction_id: Option<String>,
    /// Text when the draft opened (dirty = the input differs).
    pub original: String,
    /// Reused across retries of the same text.
    pub op: OperationSlot,
}

impl WorkItemDetails {
    /// The scope this surface edits: the item, or `None` for the Space root.
    fn scope_item(&self) -> Option<&str> {
        (!self.is_space_root()).then_some(self.item_id.as_str())
    }

    fn local_instructions(&self) -> Vec<WorkItemInstructionView> {
        self.snapshot
            .as_ref()
            .map(|snapshot| {
                instructions::live_instructions(snapshot, self.scope_item())
                    .into_iter()
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// An instruction draft whose text differs from where it started.
    pub(crate) fn instruction_draft_dirty(&self, cx: &gpui::App) -> bool {
        self.instruction_draft.as_ref().is_some_and(|draft| {
            self.instruction_input.read(cx).text().trim() != draft.original.trim()
        })
    }

    /// Open the editor for `instruction_id` (or a new instruction). A dirty
    /// draft elsewhere is never replaced silently.
    pub(crate) fn begin_instruction_draft(
        &mut self,
        instruction_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.instruction_draft_dirty(cx) {
            self.instructions_error = Some("Save or cancel the open instruction first".into());
            cx.notify();
            return;
        }
        let original = instruction_id
            .as_deref()
            .and_then(|id| {
                self.local_instructions()
                    .into_iter()
                    .find(|instruction| instruction.id == id)
            })
            .map(|instruction| instruction.text)
            .unwrap_or_default();
        self.instructions_open = true;
        self.instructions_error = None;
        self.instruction_draft = Some(InstructionDraft {
            instruction_id,
            original: original.clone(),
            op: OperationSlot::default(),
        });
        self.instruction_input
            .update(cx, |input, cx| input.set_text(original, cx));
        let handle = self.instruction_input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Tests drive drafts through this.
    #[cfg(test)]
    pub(crate) fn set_instruction_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.instruction_input
            .update(cx, |input, cx| input.set_text(text, cx));
    }

    /// Cancel: discard the draft (an explicit action; no confirmation).
    pub(crate) fn cancel_instruction(&mut self, cx: &mut Context<Self>) {
        self.instruction_draft = None;
        self.instructions_error = None;
        cx.notify();
    }

    fn run_instruction(
        &mut self,
        key: impl Into<String>,
        task: super::super::actions::ActionTask,
        cx: &mut Context<Self>,
        on_ok: impl FnOnce(&mut Self) + 'static,
    ) {
        self.run_instruction_logged(key, task, None, cx, on_ok);
    }

    /// [`Self::run_instruction`] that records `change` in the action log once
    /// the engine acknowledges the request.
    fn run_instruction_logged(
        &mut self,
        key: impl Into<String>,
        task: super::super::actions::ActionTask,
        change: Option<super::super::action_log::Draft>,
        cx: &mut Context<Self>,
        on_ok: impl FnOnce(&mut Self) + 'static,
    ) {
        let key = key.into();
        if !self.in_flight.insert(key.clone()) {
            return;
        }
        self.instructions_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result: Result<_, WorkOutlineError> = task.await;
            this.update(cx, |this, cx| {
                this.in_flight.remove(&key);
                match result {
                    Ok(_) => {
                        if let Some(change) = change {
                            let space_id = this.space_id.clone();
                            this.state
                                .update(cx, |state, _| state.action_log.record(&space_id, change));
                        }
                        on_ok(this)
                    }
                    Err(error) => this.instructions_error = Some(instruction_error_text(&error)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Run `then` now when `change` alters no assigned Chat's effective
    /// instructions; otherwise only after the Shell's drift confirmation.
    pub(crate) fn with_drift_confirm(
        &mut self,
        change: OutlineChange,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) {
        let impact = self
            .snapshot
            .as_ref()
            .map(|snapshot| instructions::drift_impact(snapshot, &change))
            .unwrap_or_default();
        if !impact.needs_confirmation() {
            then(self, cx);
            return;
        }
        let weak = cx.entity().downgrade();
        let request = WorkOutlineDriftRequest {
            title: change.title().into(),
            confirm_label: change.confirm_label().into(),
            impact,
            on_decision: Box::new(move |confirmed, _window, cx| {
                if confirmed {
                    weak.update(cx, |this, cx| then(this, cx)).ok();
                }
            }),
        };
        if let Some(shell) = self.shell.upgrade() {
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
            self.instructions_error = Some("This change needs the app window to confirm".into());
            cx.notify();
        }
    }

    /// Save the open draft (explicit). Unchanged text just closes it.
    pub(crate) fn save_instruction(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.instruction_draft.clone() else {
            return;
        };
        let text = self.instruction_input.read(cx).text().trim().to_owned();
        if text.is_empty() {
            self.instructions_error = Some("Enter the instruction text".into());
            cx.notify();
            return;
        }
        let max = mast_proto::limits::MAX_INSTRUCTION_TEXT_CHARS;
        let chars = text.chars().count();
        if chars > max {
            self.instructions_error = Some(format!(
                "This instruction is {chars} characters; the limit is {max}"
            ));
            cx.notify();
            return;
        }
        match draft.instruction_id {
            Some(id) if text == draft.original.trim() => {
                let _ = id;
                self.cancel_instruction(cx);
            }
            Some(id) => {
                let change = OutlineChange::EditInstruction {
                    instruction_id: id.clone(),
                    text: text.clone(),
                };
                self.with_drift_confirm(change, cx, move |this, cx| {
                    this.send_instruction_update(id, text, cx)
                });
            }
            None => self.send_instruction_create(text, cx),
        }
    }

    fn draft_operation_id(&mut self, inputs: &str) -> String {
        match self.instruction_draft.as_mut() {
            Some(draft) => draft.op.id_for(inputs),
            None => super::super::actions::new_operation_id(),
        }
    }

    fn send_instruction_update(&mut self, id: String, text: String, cx: &mut Context<Self>) {
        let operation_id = self.draft_operation_id(&text);
        let change = self.snapshot.as_deref().and_then(|snapshot| {
            super::super::action_log::instruction_edit_entry(snapshot, &id, &text, None)
        });
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .update_instruction(UpdateWorkItemInstructionParams {
                instruction_id: id,
                text,
                operation_id,
            });
        self.run_instruction_logged("instruction:save", task, change, cx, |this| {
            this.instruction_draft = None
        });
    }

    fn send_instruction_create(&mut self, text: String, cx: &mut Context<Self>) {
        let operation_id = self.draft_operation_id(&text);
        let scope = match self.scope_item() {
            Some(id) => InstructionScope::WorkItem {
                work_item_id: id.to_owned(),
            },
            None => InstructionScope::SpaceRoot,
        };
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .create_instruction(CreateWorkItemInstructionParams {
                space_id: self.space_id.clone(),
                scope,
                text,
                placement: None,
                source: None,
                operation_id,
            });
        self.run_instruction("instruction:save", task, cx, |this| {
            this.instruction_draft = None
        });
    }

    /// Remove = archive the instruction (drift-confirmed when it changes an
    /// assigned Chat).
    pub(crate) fn archive_instruction(&mut self, id: String, cx: &mut Context<Self>) {
        let change = OutlineChange::ArchiveInstruction {
            instruction_id: id.clone(),
        };
        self.with_drift_confirm(change, cx, move |this, cx| {
            let task = this
                .state
                .read(cx)
                .work_outline_actions()
                .archive_instruction(ArchiveWorkItemInstructionParams {
                    instruction_id: id.clone(),
                    operation_id: super::super::actions::new_operation_id(),
                });
            this.run_instruction(format!("instruction:{id}"), task, cx, |_| {});
        });
    }

    /// Move one step up or down within the scope.
    pub(crate) fn move_instruction(&mut self, id: String, up: bool, cx: &mut Context<Self>) {
        let list = self.local_instructions();
        let Some(ix) = list.iter().position(|instruction| instruction.id == id) else {
            return;
        };
        let placement = if up {
            let Some(previous) = ix.checked_sub(1).and_then(|p| list.get(p)) else {
                return;
            };
            InstructionPlacement::Before {
                sibling_instruction_id: previous.id.clone(),
            }
        } else {
            let Some(next) = list.get(ix + 1) else {
                return;
            };
            InstructionPlacement::After {
                sibling_instruction_id: next.id.clone(),
            }
        };
        let change = self.snapshot.as_deref().and_then(|snapshot| {
            let scope: Vec<String> = list.iter().map(|i| i.id.clone()).collect();
            super::super::action_log::instruction_move_entry(
                snapshot,
                &scope,
                &id,
                placement.clone(),
                None,
            )
        });
        let task = self.state.read(cx).work_outline_actions().move_instruction(
            MoveWorkItemInstructionParams {
                instruction_id: id.clone(),
                placement,
                operation_id: super::super::actions::new_operation_id(),
            },
        );
        self.run_instruction_logged(format!("instruction:{id}"), task, change, cx, |_| {});
    }

    /// "Inherit parent instructions" (drift-confirmed).
    pub(crate) fn set_inheritance(&mut self, inherit: bool, cx: &mut Context<Self>) {
        if self
            .item()
            .is_none_or(|item| item.inherit_parent_instructions == inherit)
        {
            return;
        }
        let change = OutlineChange::SetInheritance {
            work_item_id: self.item_id.clone(),
            inherit,
        };
        self.with_drift_confirm(change, cx, move |this, cx| {
            this.update_item(
                "inherit",
                WorkItemChanges {
                    inherit_parent_instructions: Some(inherit),
                    ..Default::default()
                },
                cx,
                |_| {},
            );
        });
    }

    /// Expand Effective Instructions (palette "Open Effective Instructions",
    /// and later the pre-launch check of Milestone 3).
    pub(crate) fn open_effective(&mut self, cx: &mut Context<Self>) {
        self.effective_open = true;
        cx.notify();
    }

    // ----------------------------------------------------------------------
    // Rendering
    // ----------------------------------------------------------------------

    fn draft_editor(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let saving = self.in_flight.contains("instruction:save");
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(Self::editor_frame(
                &self.instruction_input,
                &self.instruction_skills,
                "work-item-instruction-editor",
                theme,
            ))
            .child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .child(
                        Self::small_button(
                            "work-item-instruction-save",
                            if saving { "Saving\u{2026}" } else { "Save" },
                            theme,
                        )
                        .debug_selector(|| "work-item-instruction-save".into())
                        .on_click(cx.listener(|this, _, _, cx| this.save_instruction(cx))),
                    )
                    .child(
                        Self::small_button("work-item-instruction-cancel", "Cancel", theme)
                            .debug_selector(|| "work-item-instruction-cancel".into())
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_instruction(cx))),
                    ),
            )
    }

    pub(super) fn render_instructions_section(
        &mut self,
        item: Option<&WorkItemView>,
        snapshot: &WorkOutlineSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let list = instructions::live_instructions(snapshot, self.scope_item());
        let list: Vec<WorkItemInstructionView> = list.into_iter().cloned().collect();
        let open = self.instructions_open || self.instruction_draft.is_some();
        let mut section = div()
            .debug_selector(|| "work-item-instructions".into())
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                Self::section_header(
                    "work-item-instructions-toggle",
                    format!("Instructions ({})", list.len()),
                    open,
                    theme,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.instructions_open = !this.instructions_open;
                    cx.notify();
                })),
            );
        if !open {
            return section.into_any_element();
        }
        // Inheritance applies only below a parent work item.
        if let Some(item) = item.filter(|item| item.parent_work_item_id.is_some()) {
            let inherit = item.inherit_parent_instructions;
            section = section.child(
                div()
                    .id("work-item-inherit-toggle")
                    .debug_selector(|| "work-item-inherit-toggle".into())
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.text_muted)
                    .hover(|el| el.text_color(theme.text))
                    .child(
                        div()
                            .size(px(12.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.0))
                            .border_1()
                            .border_color(theme.border_strong)
                            .when(inherit, |el| {
                                el.child(
                                    crate::icons::icon(crate::icons::CHECK)
                                        .size(px(9.0))
                                        .text_color(theme.text),
                                )
                            }),
                    )
                    .child("Inherit parent instructions")
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.set_inheritance(!inherit, cx)),
                    ),
            );
        }
        let editing_id = self
            .instruction_draft
            .as_ref()
            .map(|draft| draft.instruction_id.clone());
        let last = list.len().saturating_sub(1);
        for (ix, instruction) in list.iter().enumerate() {
            let editing = editing_id.as_ref() == Some(&Some(instruction.id.clone()));
            if editing {
                section = section.child(self.draft_editor(theme, cx));
                continue;
            }
            let badge = instructions::provenance_badge(instruction);
            let excerpt = instruction
                .source
                .as_ref()
                .and_then(|source| source.excerpt.clone())
                .filter(|_| self.excerpts_open.contains(&instruction.id));
            let toggle_id = instruction.id.clone();
            let (up_id, down_id, edit_id, archive_id, fold_id) = (
                instruction.id.clone(),
                instruction.id.clone(),
                instruction.id.clone(),
                instruction.id.clone(),
                instruction.id.clone(),
            );
            let collapsible = needs_collapse(&instruction.text);
            let expanded = self.instructions_expanded.contains(&instruction.id);
            let line_height = crate::typography::ui_rems(LINE_HEIGHT_REMS);
            section =
                section.child(
                    div()
                        .debug_selector(move || format!("work-item-instruction-{ix}"))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .px(px(4.0))
                        .py(px(3.0))
                        .rounded(px(4.0))
                        .hover(|el| el.bg(theme.element_hover.opacity(0.5)))
                        // Header: the number, the controls, and the provenance
                        // badge stay at the top whatever the text length.
                        .child(
                            div()
                                .debug_selector(move || {
                                    format!("work-item-instruction-header-{ix}")
                                })
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .text_size(crate::typography::ui_rems(12.0))
                                .child(
                                    div()
                                        .flex_none()
                                        .text_color(theme.text_faint)
                                        .child(SharedString::from(format!("{}.", ix + 1))),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .gap(px(4.0))
                                        .when(ix > 0, |el| {
                                            el.child(
                                                Self::small_button(
                                                    ("work-item-instruction-up", ix),
                                                    "\u{2191}",
                                                    theme,
                                                )
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.move_instruction(up_id.clone(), true, cx)
                                                })),
                                            )
                                        })
                                        .when(ix < last, |el| {
                                            el.child(
                                                Self::small_button(
                                                    ("work-item-instruction-down", ix),
                                                    "\u{2193}",
                                                    theme,
                                                )
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.move_instruction(
                                                        down_id.clone(),
                                                        false,
                                                        cx,
                                                    )
                                                })),
                                            )
                                        })
                                        .child(
                                            Self::small_button(
                                                ("work-item-instruction-edit", ix),
                                                "Edit",
                                                theme,
                                            )
                                            .debug_selector(move || {
                                                format!("work-item-instruction-edit-{ix}")
                                            })
                                            .on_click(
                                                cx.listener(move |this, _, window, cx| {
                                                    this.begin_instruction_draft(
                                                        Some(edit_id.clone()),
                                                        window,
                                                        cx,
                                                    )
                                                }),
                                            ),
                                        )
                                        .child(
                                            Self::small_button(
                                                ("work-item-instruction-archive", ix),
                                                "Remove",
                                                theme,
                                            )
                                            .debug_selector(move || {
                                                format!("work-item-instruction-archive-{ix}")
                                            })
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    this.archive_instruction(archive_id.clone(), cx)
                                                }),
                                            ),
                                        ),
                                )
                                .when_some(badge, |el, badge| {
                                    el.child(
                                        div()
                                            .id(("work-item-instruction-badge", ix))
                                            .debug_selector(move || {
                                                format!("work-item-instruction-badge-{ix}")
                                            })
                                            .flex_none()
                                            .px(px(4.0))
                                            .rounded(px(3.0))
                                            .bg(theme.element_hover.opacity(0.7))
                                            .cursor_pointer()
                                            .text_size(crate::typography::ui_rems(9.5))
                                            .text_color(theme.text_muted)
                                            .child(badge)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if !this.excerpts_open.remove(&toggle_id) {
                                                    this.excerpts_open.insert(toggle_id.clone());
                                                }
                                                cx.notify();
                                            })),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .debug_selector(move || format!("work-item-instruction-text-{ix}"))
                                .ml(px(14.0))
                                .min_w_0()
                                .text_size(crate::typography::ui_rems(12.0))
                                .line_height(line_height)
                                .text_color(theme.text)
                                .when(collapsible && !expanded, |el| {
                                    el.max_h(crate::typography::ui_rems(
                                        LINE_HEIGHT_REMS * COLLAPSED_LINES as f32,
                                    ))
                                    .overflow_hidden()
                                })
                                .child(SharedString::from(instruction.text.clone())),
                        )
                        .when(collapsible, |el| {
                            el.child(
                                div().ml(px(14.0)).flex().child(
                                    div()
                                        .id(("work-item-instruction-fold", ix))
                                        .debug_selector(move || {
                                            format!("work-item-instruction-fold-{ix}")
                                        })
                                        .cursor_pointer()
                                        .text_size(crate::typography::ui_rems(10.5))
                                        .text_color(theme.text_muted)
                                        .hover(|el| el.text_color(theme.text))
                                        .child(if expanded { "Show less" } else { "Show more" })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if !this.instructions_expanded.remove(&fold_id) {
                                                this.instructions_expanded.insert(fold_id.clone());
                                            }
                                            cx.notify();
                                        })),
                                ),
                            )
                        })
                        .when_some(excerpt, |el, excerpt| {
                            el.child(
                                div()
                                    .debug_selector(move || {
                                        format!("work-item-instruction-excerpt-{ix}")
                                    })
                                    .ml(px(14.0))
                                    .pl(px(6.0))
                                    .border_l_2()
                                    .border_color(theme.border_strong)
                                    .text_size(crate::typography::ui_rems(11.0))
                                    .text_color(theme.text_muted)
                                    .italic()
                                    .child(SharedString::from(excerpt)),
                            )
                        }),
                );
        }
        if editing_id == Some(None) {
            section = section.child(self.draft_editor(theme, cx));
        } else {
            section = section.child(
                div().child(
                    Self::small_button("work-item-instruction-add", "Add instruction", theme)
                        .debug_selector(|| "work-item-instruction-add".into())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.begin_instruction_draft(None, window, cx)
                        })),
                ),
            );
        }
        if let Some(error) = self.instructions_error.clone() {
            section = section.child(
                div()
                    .debug_selector(|| "work-item-instructions-error".into())
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.warning)
                    .child(SharedString::from(error)),
            );
        }
        section.into_any_element()
    }

    pub(super) fn render_effective_section(
        &mut self,
        item: &WorkItemView,
        snapshot: &WorkOutlineSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let effective = snapshot.effective_instructions(&item.id);
        let open = self.effective_open;
        let mut section = div()
            .debug_selector(|| "work-item-effective".into())
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                Self::section_header(
                    "work-item-effective-toggle",
                    format!("Effective instructions ({})", effective.entry_count),
                    open,
                    theme,
                )
                .when(!effective.within_budget, |el| el.text_color(theme.danger))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.effective_open = !this.effective_open;
                    cx.notify();
                })),
            );
        if !open {
            return section.into_any_element();
        }
        let over = !effective.within_budget;
        section = section.child(
            div()
                .debug_selector(|| "work-item-effective-budget".into())
                .text_size(crate::typography::ui_rems(10.5))
                .text_color(if over { theme.danger } else { theme.text_faint })
                .child(SharedString::from(format!(
                    "{} / {} characters \u{b7} {} / {} entries",
                    effective.rendered_chars,
                    effective.budget.max_chars,
                    effective.entry_count,
                    effective.budget.max_entries
                ))),
        );
        if let Some(error) = effective.budget_error.clone() {
            section = section.child(
                div()
                    .debug_selector(|| "work-item-effective-budget-error".into())
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.danger)
                    .child(SharedString::from(error)),
            );
        }
        if effective.entries.is_empty() {
            section = section.child(
                div()
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.text_faint)
                    .child("No instructions apply"),
            );
        }
        for (ix, entry) in effective.entries.iter().enumerate() {
            let space_id = self.space_id.clone();
            let target = match entry.source_kind {
                mast_proto::InstructionSourceKind::SpaceRoot => SPACE_ROOT_ITEM_ID.to_owned(),
                mast_proto::InstructionSourceKind::WorkItem => entry.scope_id.clone(),
            };
            let local = !entry.inherited;
            section = section.child(
                div()
                    .debug_selector(move || format!("work-item-effective-entry-{ix}"))
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .px(px(4.0))
                    .py(px(2.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .text_size(crate::typography::ui_rems(10.5))
                            .child(
                                div()
                                    .text_color(theme.text_faint)
                                    .child(SharedString::from(format!("{}.", ix + 1))),
                            )
                            .child(
                                div()
                                    .debug_selector(move || {
                                        format!("work-item-effective-marker-{ix}")
                                    })
                                    .px(px(4.0))
                                    .rounded(px(3.0))
                                    .when(local, |el| {
                                        el.bg(theme.accent_wash).text_color(theme.accent)
                                    })
                                    .when(!local, |el| {
                                        el.bg(theme.element_hover.opacity(0.7))
                                            .text_color(theme.text_muted)
                                    })
                                    .child(if local { "Local" } else { "Inherited" }),
                            )
                            .child(
                                div()
                                    .id(("work-item-effective-source", ix))
                                    .min_w_0()
                                    .truncate()
                                    .cursor_pointer()
                                    .text_color(theme.text_muted)
                                    .hover(|el| el.text_color(theme.text))
                                    .child(SharedString::from(entry.scope_title.clone()))
                                    .when(!local, |el| {
                                        el.on_click(cx.listener(move |_, _, _, cx| {
                                            cx.emit(DetailsEvent::OpenItem {
                                                space_id: space_id.clone(),
                                                item_id: target.clone(),
                                            })
                                        }))
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .pl(px(14.0))
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text)
                            .child(SharedString::from(entry.text.clone())),
                    ),
            );
        }
        if let Some(boundary) = effective
            .boundary_work_item_id
            .as_deref()
            .and_then(|id| snapshot.item(id))
        {
            section = section.child(
                div()
                    .debug_selector(|| "work-item-effective-boundary".into())
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(format!(
                        "Parent instructions stop at \u{201C}{}\u{201D}",
                        boundary.title
                    ))),
            );
        }
        section
            .child(
                div()
                    .debug_selector(|| "work-item-effective-precedence".into())
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(effective.precedence_rule.clone())),
            )
            .into_any_element()
    }

    /// The Space-root variant: Space-root instructions only.
    pub(super) fn render_space_root(
        &mut self,
        frame: gpui::Stateful<gpui::Div>,
        close: gpui::Stateful<gpui::Div>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let label = self
            .state
            .read(cx)
            .space_row(&self.space_id)
            .map(|space| space.display_name().to_owned())
            .unwrap_or_default();
        let header = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(14.0))
            .pt(px(10.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(label)),
            )
            .child(close);
        let Some(snapshot) = self.snapshot.clone() else {
            return frame.child(header).child(
                div()
                    .px(px(16.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child("Loading\u{2026}"),
            );
        };
        let required_actions = self.render_required_actions_section(&snapshot, theme, cx);
        let section = self.render_instructions_section(None, &snapshot, theme, cx);
        let proposals = self.render_proposals_section(&snapshot, theme, cx);
        let execution = self.render_execution_section(&snapshot, theme, cx);
        let linear = self.render_space_linear(&snapshot, theme, cx);
        let pr_sync = self.render_space_pr_sync(&snapshot, theme, cx);
        let templates = self.render_templates_section(&snapshot, theme, cx);
        frame.child(header).child(
            div()
                .id("work-item-details-body")
                .debug_selector(|| "space-root-details".into())
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(12.0))
                .px(px(14.0))
                .py(px(10.0))
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(15.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child("Space root"),
                )
                .child(required_actions)
                .child(section)
                .when_some(proposals, |el, proposals| el.child(proposals))
                .child(execution)
                .child(templates)
                .child(linear)
                .child(pr_sync),
        )
    }
}
