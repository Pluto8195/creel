//! Project templates in Work Item Details.
//!
//! - A project's **Project** section gets "Save as template…": a small name
//!   prompt prefilled with the project's title. A project that already links
//!   a live template offers "Update template" (replace it, revision + 1) or
//!   "Save as new". Saving is not an undo step; it sends
//!   `saveWorkItemTemplate` and reports through a notice (an error card on
//!   failure).
//! - The Space-root variant lists the Space's **Templates** (name, revision,
//!   item count) with "New from template…" and "Delete" per row; Delete
//!   confirms first.
//!
//! A child module of `details_panel` (like the other sections), so it uses
//! the surface's private state directly.
use gpui::{AnyElement, Context, Focusable as _, SharedString, Window, div, prelude::*, px};
use mast_proto::{DeleteWorkItemTemplateParams, SaveWorkItemTemplateParams, WorkItemTemplateView};

use super::super::actions::new_operation_id;
use super::super::command::WorkOutlineConfirmRequest;
use super::super::store::{WorkItemView, WorkOutlineSnapshot};
use super::{DetailsEvent, WorkItemDetails};
use crate::theme::Theme;

/// The open "Save as template…" prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SavePrompt {
    /// The live template the project already links: `(id, name, revision)`.
    pub existing: Option<(String, String, u32)>,
}

#[derive(Debug, Default)]
pub(crate) struct TemplatesDetailsState {
    pub prompt: Option<SavePrompt>,
    /// The failure the prompt shows (it stays open to retry).
    pub error: Option<String>,
}

impl WorkItemDetails {
    /// "Save as template…": open the name prompt (projects only).
    pub(crate) fn begin_save_template(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.item().cloned() else {
            return;
        };
        if let Some(reason) = super::super::templates::save_blocked(self.snapshot.as_deref(), &item.id)
        {
            self.templates.error = Some(reason.into());
            cx.emit(DetailsEvent::Notice {
                text: reason.into(),
                error: false,
            });
            cx.notify();
            return;
        }
        let existing = item
            .template_id
            .as_ref()
            .and_then(|id| {
                self.snapshot
                    .as_ref()?
                    .templates
                    .iter()
                    .find(|template| &template.id == id)
            })
            .map(|template| (template.id.clone(), template.name.clone(), template.revision));
        let prefill = existing
            .as_ref()
            .map_or_else(|| item.title.clone(), |(_, name, _)| name.clone());
        self.templates.prompt = Some(SavePrompt { existing });
        self.templates.error = None;
        self.template_name_input
            .update(cx, |input, cx| input.set_text(prefill, cx));
        let handle = self.template_name_input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    pub(crate) fn cancel_save_template(&mut self, cx: &mut Context<Self>) {
        self.templates.prompt = None;
        self.templates.error = None;
        cx.notify();
    }

    /// Enter in the name prompt: update the linked template when there is
    /// one, else save a new one.
    pub(super) fn submit_save_template(&mut self, cx: &mut Context<Self>) {
        let update = self
            .templates
            .prompt
            .as_ref()
            .is_some_and(|prompt| prompt.existing.is_some());
        self.save_template(!update, cx);
    }

    /// Send `saveWorkItemTemplate` for this project: a new template
    /// (`as_new`) or a replacement of the one it links.
    pub(crate) fn save_template(&mut self, as_new: bool, cx: &mut Context<Self>) {
        let Some(prompt) = self.templates.prompt.clone() else {
            return;
        };
        let name = self
            .template_name_input
            .read(cx)
            .text()
            .trim()
            .to_string();
        let max = mast_proto::template_limits::MAX_TEMPLATE_NAME_CHARS;
        if name.is_empty() || name.chars().count() > max {
            self.templates.error = Some(format!("Name the template (1 to {max} characters)"));
            cx.notify();
            return;
        }
        let replacing = (!as_new).then(|| prompt.existing.clone()).flatten();
        let key = "template-save";
        if !self.in_flight.insert(key.to_owned()) {
            return;
        }
        self.templates.error = None;
        let revision = replacing.as_ref().map_or(1, |(_, _, revision)| revision + 1);
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .save_work_item_template(SaveWorkItemTemplateParams {
                space_id: self.space_id.clone(),
                project_work_item_id: self.item_id.clone(),
                name: name.clone(),
                template_id: replacing.map(|(id, _, _)| id),
                operation_id: new_operation_id(),
            });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.in_flight.remove(key);
                match result {
                    Ok(_) => {
                        this.templates.prompt = None;
                        cx.emit(DetailsEvent::Notice {
                            text: format!("Saved template \u{2018}{name}\u{2019} \u{b7} rev {revision}"),
                            error: false,
                        });
                    }
                    Err(error) => {
                        this.templates.error = Some(error.message.clone());
                        cx.emit(DetailsEvent::Notice {
                            text: format!("Couldn\u{2019}t save the template\n{}", error.message),
                            error: true,
                        });
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Delete `template` after the confirmation.
    pub(crate) fn request_delete_template(&mut self, template: &WorkItemTemplateView, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        let id = template.id.clone();
        let name = template.name.clone();
        let request = WorkOutlineConfirmRequest {
            title: format!("Delete template \u{2018}{}\u{2019}?", template.name).into(),
            body: "Projects made from it keep working; they just can\u{2019}t be compared with it any more."
                .into(),
            confirm_label: "Delete".into(),
            cancel_label: "Cancel".into(),
            destructive: true,
            on_decision: Box::new(move |confirmed, _window, cx| {
                if confirmed {
                    weak.update(cx, |this, cx| this.delete_template(id, name, cx))
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
        drop(request);
    }

    fn delete_template(&mut self, template_id: String, name: String, cx: &mut Context<Self>) {
        let key = format!("template-delete:{template_id}");
        if !self.in_flight.insert(key.clone()) {
            return;
        }
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .delete_work_item_template(DeleteWorkItemTemplateParams {
                template_id,
                operation_id: new_operation_id(),
            });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.in_flight.remove(&key);
                cx.emit(match result {
                    Ok(_) => DetailsEvent::Notice {
                        text: format!("Deleted template \u{2018}{name}\u{2019}"),
                        error: false,
                    },
                    Err(error) => DetailsEvent::Notice {
                        text: format!("Couldn\u{2019}t delete the template\n{}", error.message),
                        error: true,
                    },
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The Project section's template controls: the button, and the prompt
    /// when open.
    pub(super) fn render_save_template(
        &self,
        item: &WorkItemView,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut out = Vec::new();
        let Some(prompt) = &self.templates.prompt else {
            return out;
        };
        let note = match &prompt.existing {
            Some((_, name, revision)) => format!(
                "Update \u{2018}{name}\u{2019} (rev {revision} \u{2192} {}) or save a copy under a new name.",
                revision + 1
            ),
            None => format!(
                "Saves a copy of \u{2018}{}\u{2019} and its items; later edits to it don\u{2019}t change the template.",
                item.title
            ),
        };
        out.push(
            div()
                .text_size(crate::typography::ui_rems(10.5))
                .text_color(theme.text_muted)
                .child(SharedString::from(note))
                .into_any_element(),
        );
        out.push(
            div()
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "enter" {
                        this.submit_save_template(cx);
                        cx.stop_propagation();
                        window.prevent_default();
                    }
                }))
                .child(Self::input_frame(&self.template_name_input, theme))
                .into_any_element(),
        );
        if let Some(error) = &self.templates.error {
            out.push(
                div()
                    .debug_selector(|| "work-item-template-error".into())
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(error.clone()))
                    .into_any_element(),
            );
        }
        let mut buttons = div().flex().gap(px(6.0));
        if prompt.existing.is_some() {
            buttons = buttons
                .child(
                    Self::small_button("work-item-template-update", "Update template", theme)
                        .debug_selector(|| "work-item-template-update".into())
                        .on_click(cx.listener(|this, _, _, cx| this.save_template(false, cx))),
                )
                .child(
                    Self::small_button("work-item-template-new-copy", "Save as new", theme)
                        .debug_selector(|| "work-item-template-new-copy".into())
                        .on_click(cx.listener(|this, _, _, cx| this.save_template(true, cx))),
                );
        } else {
            buttons = buttons.child(
                Self::small_button("work-item-template-save", "Save", theme)
                    .debug_selector(|| "work-item-template-save".into())
                    .on_click(cx.listener(|this, _, _, cx| this.save_template(true, cx))),
            );
        }
        buttons = buttons.child(
            Self::small_button("work-item-template-cancel", "Cancel", theme)
                .on_click(cx.listener(|this, _, _, cx| this.cancel_save_template(cx))),
        );
        out.push(buttons.into_any_element());
        out
    }

    /// The Space-root "Templates" section.
    pub(super) fn render_templates_section(
        &self,
        snapshot: &WorkOutlineSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mut section = div()
            .debug_selector(|| "space-templates".into())
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text_muted)
                    .child("Templates"),
            );
        if snapshot.templates.is_empty() {
            return section.child(
                div()
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_muted)
                    .child("No templates yet. Mark a project, then Save as template\u{2026} on it."),
            );
        }
        for (ix, template) in snapshot.templates.iter().enumerate() {
            let new_id = template.id.clone();
            let space_id = self.space_id.clone();
            let delete = template.clone();
            let row_selector = format!("space-template-{}", template.id);
            let new_selector = format!("space-template-new-{}", template.id);
            let delete_selector = format!("space-template-delete-{}", template.id);
            section = section.child(
                div()
                    .debug_selector(move || row_selector.clone())
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(11.5))
                            .text_color(theme.text)
                            .child(SharedString::from(format!(
                                "{} \u{b7} {}",
                                template.name,
                                super::super::templates::summary_line(template)
                            ))),
                    )
                    .child(
                        Self::small_button(("space-template-new", ix), "New from template\u{2026}", theme)
                            .debug_selector(move || new_selector.clone())
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(DetailsEvent::NewFromTemplate {
                                    space_id: space_id.clone(),
                                    template_id: Some(new_id.clone()),
                                })
                            })),
                    )
                    .child(
                        Self::small_button(("space-template-delete", ix), "Delete", theme)
                            .debug_selector(move || delete_selector.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.request_delete_template(&delete, cx)
                            })),
                    ),
            );
        }
        section
    }
}
