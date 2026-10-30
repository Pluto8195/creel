//! "Convert to project…": the wizard in the Project section of Work Item
//! Details. Pick a template, see the exact preview (nothing is sent), tick or
//! redirect the existing children, **Apply**. One undo step.
//!
//! A child module of `details_panel`, so it uses the surface's private state.
use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};
use mast_proto::{WorkItemView, WorkItemTemplateView};

use super::super::action_log;
use super::super::actions::new_operation_id;
use super::super::convert::{self, MoveChoice};
use super::{DetailsEvent, WorkItemDetails};
use crate::theme::Theme;

/// The open wizard of one item.
#[derive(Debug, Clone)]
pub(crate) struct ConvertState {
    pub item_id: String,
    /// The picked template (`None` = still choosing).
    pub template_id: Option<String>,
    pub choices: Vec<MoveChoice>,
    pub error: Option<String>,
}

/// Every non-root node path of the template, pre-order.
pub(crate) fn node_paths(template: &WorkItemTemplateView) -> Vec<Vec<String>> {
    fn walk(node: &mast_proto::TemplateNode, path: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
        for child in &node.children {
            path.push(child.title.clone());
            out.push(path.clone());
            walk(child, path, out);
            path.pop();
        }
    }
    let mut out = Vec::new();
    walk(&template.body, &mut Vec::new(), &mut out);
    out
}

impl WorkItemDetails {
    /// Open the wizard on this item (needs at least one template).
    pub(crate) fn begin_convert(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.item().cloned() else {
            return;
        };
        let reason = match self.snapshot.as_deref() {
            None => Some("This project\u{2019}s outline is not loaded"),
            Some(_) if item.archived_at.is_some() => Some("Restore this work item first"),
            Some(_) if item.is_project => Some("This item is already a project"),
            Some(snapshot) if snapshot.templates.is_empty() => {
                Some("Save a template first (Save as template\u{2026} on any item)")
            }
            Some(_) => None,
        };
        if let Some(reason) = reason {
            cx.emit(DetailsEvent::Notice {
                text: reason.into(),
                error: false,
            });
            return;
        }
        self.templates.convert = Some(ConvertState {
            item_id: item.id,
            template_id: None,
            choices: Vec::new(),
            error: None,
        });
        cx.notify();
    }

    pub(crate) fn cancel_convert(&mut self, cx: &mut Context<Self>) {
        if self.templates.convert.take().is_some() {
            cx.notify();
        }
    }

    /// Pick the template: the default moves are computed, nothing is sent.
    pub(crate) fn convert_pick_template(&mut self, template_id: String, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let Some(state) = self.templates.convert.as_mut() else {
            return;
        };
        let Some(template) = snapshot.templates.iter().find(|t| t.id == template_id) else {
            return;
        };
        state.choices = convert::default_moves(&snapshot, &state.item_id, template);
        state.template_id = Some(template_id);
        state.error = None;
        cx.notify();
    }

    /// Advance child `ix` through "stays" and each template node.
    pub(crate) fn convert_cycle_child(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let Some(state) = self.templates.convert.as_mut() else {
            return;
        };
        let Some(template) = state
            .template_id
            .as_ref()
            .and_then(|id| snapshot.templates.iter().find(|t| &t.id == id))
        else {
            return;
        };
        let paths = node_paths(template);
        if let Some(choice) = state.choices.get_mut(ix) {
            let next = match &choice.1 {
                None => paths.first().cloned(),
                Some(current) => paths
                    .iter()
                    .position(|path| path == current)
                    .and_then(|at| paths.get(at + 1).cloned()),
            };
            choice.1 = next;
        }
        cx.notify();
    }

    /// Apply: send the plan's mutations in order, then log ONE undo step.
    pub(crate) fn apply_convert(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let Some(state) = self.templates.convert.clone() else {
            return;
        };
        let Some(template) = state
            .template_id
            .as_ref()
            .and_then(|id| snapshot.templates.iter().find(|t| &t.id == id))
            .cloned()
        else {
            return;
        };
        let key = new_operation_id();
        let plan = match convert::plan_conversion(&snapshot, &state.item_id, &template, &state.choices, &key)
        {
            Ok(plan) => plan,
            Err(message) => {
                self.convert_error(message, cx);
                return;
            }
        };
        let in_flight = "convert-project".to_owned();
        if !self.in_flight.insert(in_flight.clone()) {
            return;
        }
        let entry = action_log::convert_entry(&snapshot, &plan, Some(plan.first_operation_id.clone()));
        let actions = self.state.read(cx).work_outline_actions();
        let space_id = self.space_id.clone();
        let name = template.name.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let total = plan.mutations.len();
            let mut failure = None;
            for (done, mutation) in plan.mutations.into_iter().enumerate() {
                if let Err(error) = actions.mutate(mutation).await {
                    failure = Some(format!(
                        "{} (applied {done} of {total} steps; undo it from Recent Changes or fix by hand)",
                        error.message
                    ));
                    break;
                }
            }
            this.update(cx, |this, cx| {
                this.in_flight.remove(&in_flight);
                match failure {
                    Some(message) => this.convert_error(message, cx),
                    None => {
                        if let Some(entry) = entry {
                            this.state
                                .update(cx, |state, _| state.action_log.record(&space_id, entry));
                        }
                        this.templates.convert = None;
                        cx.emit(DetailsEvent::Notice {
                            text: format!("Converted to a project from template \u{2018}{name}\u{2019}"),
                            error: false,
                        });
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn convert_error(&mut self, message: String, cx: &mut Context<Self>) {
        if let Some(state) = self.templates.convert.as_mut() {
            state.error = Some(message.clone());
        }
        cx.emit(DetailsEvent::Notice {
            text: format!("Convert to project\n{message}"),
            error: true,
        });
        cx.notify();
    }

    /// The wizard for `item` (empty unless it is open on this item).
    pub(super) fn render_convert(
        &self,
        item: &WorkItemView,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut out = Vec::new();
        let (Some(state), Some(snapshot)) = (
            self.templates
                .convert
                .as_ref()
                .filter(|state| state.item_id == item.id),
            self.snapshot.as_deref(),
        ) else {
            return out;
        };
        let muted = |text: SharedString| {
            div()
                .text_size(crate::typography::ui_rems(10.5))
                .text_color(theme.text_muted)
                .child(text)
                .into_any_element()
        };
        out.push(muted(
            "Convert to project: pick the template this item should become. Nothing is changed until Apply."
                .into(),
        ));
        let mut picks = div().flex().flex_wrap().gap(px(4.0));
        for (ix, template) in snapshot.templates.iter().enumerate() {
            let selected = state.template_id.as_deref() == Some(template.id.as_str());
            let id = template.id.clone();
            let selector = format!("convert-template-{}", template.id);
            picks = picks.child(
                div()
                    .id(("convert-template", ix))
                    .debug_selector(move || selector.clone())
                    .px(px(6.0))
                    .py(px(1.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(if selected { theme.accent } else { theme.border })
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(if selected { theme.text } else { theme.text_muted })
                    .hover(|el| el.bg(theme.element_hover))
                    .child(SharedString::from(template.name.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.convert_pick_template(id.clone(), cx)
                    })),
            );
        }
        out.push(picks.into_any_element());
        if let Some(template) = state
            .template_id
            .as_ref()
            .and_then(|id| snapshot.templates.iter().find(|t| &t.id == id))
        {
            let paths = node_paths(template);
            // Children: tick/redirect each (stays, or under a template node).
            for (ix, (child_id, target)) in state.choices.iter().enumerate() {
                let Some(child) = snapshot.item(child_id) else {
                    continue;
                };
                if paths.is_empty() {
                    break;
                }
                let selector = format!("convert-child-{ix}");
                let label = match target {
                    Some(path) => format!("\u{2611} {} \u{2192} {}", child.title, path.join(" / ")),
                    None => format!("\u{2610} {} (stays)", child.title),
                };
                out.push(
                    div()
                        .id(("convert-child", ix))
                        .debug_selector(move || selector.clone())
                        .cursor_pointer()
                        .truncate()
                        .text_size(crate::typography::ui_rems(10.5))
                        .text_color(theme.text)
                        .hover(|el| el.text_color(theme.text_muted))
                        .child(SharedString::from(label))
                        .on_click(cx.listener(move |this, _, _, cx| this.convert_cycle_child(ix, cx)))
                        .into_any_element(),
                );
            }
            match convert::plan_conversion(snapshot, &item.id, template, &state.choices, "preview") {
                Ok(plan) => {
                    out.push(
                        div()
                            .debug_selector(|| "convert-preview".into())
                            .flex()
                            .flex_col()
                            .gap(px(1.0))
                            .children(convert::preview_lines(&plan).into_iter().map(|line| {
                                div()
                                    .text_size(crate::typography::ui_rems(10.5))
                                    .text_color(theme.text)
                                    .child(SharedString::from(format!("\u{2022} {line}")))
                            }))
                            .into_any_element(),
                    );
                }
                Err(message) => out.push(muted(SharedString::from(message))),
            }
        }
        if let Some(error) = &state.error {
            out.push(muted(SharedString::from(error.clone())));
        }
        out.push(
            div()
                .flex()
                .gap(px(6.0))
                .when(state.template_id.is_some(), |row| {
                    row.child(
                        Self::small_button("work-item-convert-apply", "Apply", theme)
                            .debug_selector(|| "work-item-convert-apply".into())
                            .on_click(cx.listener(|this, _, _, cx| this.apply_convert(cx))),
                    )
                })
                .child(
                    Self::small_button("work-item-convert-cancel", "Cancel", theme)
                        .debug_selector(|| "work-item-convert-cancel".into())
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_convert(cx))),
                )
                .into_any_element(),
        );
        out
    }
}
