//! "New from template…" for the Outline.
//!
//! The flow: pick a template (skipped when one is preselected, e.g. from the
//! Templates list), then name the new project in the inline title row
//! (prefilled with the template's root title). A project made from a Linear
//! project (Linear panel) skips the title (the Linear project's name) and
//! asks for the parent instead, then links the new root to the Linear project
//! with the existing `linkLinearProject` mutation. Nothing is written to
//! Linear.
//!
//! The effect is one action and one undo step: the plan's requests
//! (`work_outline::templates::plan_instantiation`) are sent in order, each
//! awaited, and logged as a single [`action_log::instantiate_entry`]: undo
//! archives the new root with its descendants, redo restores it.

use mast_proto::linear::LinearProjectRef;

use super::super::templates as plan;
use super::*;

/// How long the "Created project" toast stays.
pub(crate) const TEMPLATE_TOAST_LIFETIME: Duration = Duration::from_secs(8);

/// One "New from template…" in progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TemplateFlow {
    pub space_id: String,
    /// The new project's parent: `None` = the Space's top level.
    pub parent: Option<String>,
    /// Ask for the parent (the Linear entry) instead of using `parent`.
    pub ask_parent: bool,
    /// Skip the template picker (a Templates list row).
    pub template_id: Option<String>,
    /// Make the title the Linear project's name and link the new root to it.
    pub linear: Option<LinearProjectRef>,
}

impl TemplateFlow {
    pub(crate) fn new(space_id: impl Into<String>, parent: Option<String>) -> Self {
        Self {
            space_id: space_id.into(),
            parent,
            ask_parent: false,
            template_id: None,
            linear: None,
        }
    }
}

/// Which picker of the flow is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TemplateStage {
    /// Entry value = template id.
    Choose(TemplateFlow),
    /// Entry value = parent item id (`None` = top level); Linear flow only.
    Parent(TemplateFlow),
}

#[derive(Debug, Clone)]
struct TemplateToast {
    text: String,
    seq: u64,
}

/// The template flow's view state.
#[derive(Debug, Default)]
pub(crate) struct TemplatesState {
    /// A flow a caller without a window asked for; opened on the next render.
    pending: Option<TemplateFlow>,
    toast: Option<TemplateToast>,
    seq: u64,
}

impl OutlineView {
    /// Ask for "New from template…" from a place that has no window (the
    /// Details panel, the Linear panel); the next render opens it.
    pub(crate) fn request_new_from_template(&mut self, flow: TemplateFlow, cx: &mut Context<Self>) {
        self.templates.pending = Some(flow);
        cx.notify();
    }

    pub(super) fn open_requested_template_flow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(flow) = self.templates.pending.take() else {
            return;
        };
        if let Err(notice) = self.begin_new_from_template(flow, window, cx) {
            self.show_notice(notice, cx);
        }
    }

    /// Start the flow: the template picker, or straight to the next step
    /// when the template is already chosen. `Err` is the notice to show.
    pub(crate) fn begin_new_from_template(
        &mut self,
        flow: TemplateFlow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let snapshot = self
            .snapshot(&flow.space_id)
            .filter(|snapshot| snapshot.available)
            .ok_or("This project\u{2019}s outline is not available yet")?;
        if let Some(parent) = &flow.parent {
            if !snapshot
                .item(parent)
                .is_some_and(|item| item.archived_at.is_none())
            {
                return Err("Restore this work item to add a project under it".into());
            }
        }
        if snapshot.templates.is_empty() {
            return Err(
                "No templates in this project yet. Mark a project, then Save as Template\u{2026}"
                    .into(),
            );
        }
        if let Some(id) = &flow.template_id {
            if !snapshot.templates.iter().any(|template| &template.id == id) {
                return Err("That template no longer exists".into());
            }
            return self.after_template_chosen(flow, window, cx);
        }
        let entries = snapshot
            .templates
            .iter()
            .map(|template| PickerEntry {
                label: format!("{} \u{b7} {}", template.name, plan::summary_line(template)),
                depth: 0,
                value: Some(template.id.clone()),
            })
            .collect();
        self.open_picker(
            "New from template\u{2026}".to_string(),
            entries,
            PickerPurpose::Template(TemplateStage::Choose(flow)),
            window,
            cx,
        );
        Ok(())
    }

    /// An entry of a template picker was chosen.
    pub(super) fn choose_template_entry(
        &mut self,
        stage: TemplateStage,
        entry: PickerEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_picker(window, cx);
        let result = match stage {
            TemplateStage::Choose(mut flow) => match entry.value {
                Some(id) => {
                    flow.template_id = Some(id);
                    self.after_template_chosen(flow, window, cx)
                }
                None => Ok(()),
            },
            TemplateStage::Parent(mut flow) => {
                flow.parent = entry.value;
                flow.ask_parent = false;
                let title = flow
                    .linear
                    .as_ref()
                    .map(|project| project.name.clone())
                    .unwrap_or_default();
                self.instantiate_template(flow, title, cx)
            }
        };
        if let Err(notice) = result {
            self.show_notice(notice, cx);
        }
    }

    /// The template is known: ask for the parent (Linear), or for the title.
    fn after_template_chosen(
        &mut self,
        flow: TemplateFlow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let snapshot = self
            .snapshot(&flow.space_id)
            .ok_or("This project\u{2019}s outline is not loaded")?;
        if flow.linear.is_some() {
            if flow.ask_parent {
                let mut entries = vec![PickerEntry {
                    label: "Top level".to_string(),
                    depth: 0,
                    value: None,
                }];
                entries.extend(model::destinations(snapshot, None).into_iter().map(|dest| {
                    PickerEntry {
                        label: dest.title,
                        depth: dest.depth + 1,
                        value: dest.item_id,
                    }
                }));
                self.open_picker(
                    "Create the project under\u{2026}".to_string(),
                    entries,
                    PickerPurpose::Template(TemplateStage::Parent(flow)),
                    window,
                    cx,
                );
                return Ok(());
            }
            let title = flow
                .linear
                .as_ref()
                .map(|project| project.name.clone())
                .unwrap_or_default();
            return self.instantiate_template(flow, title, cx);
        }
        let template = snapshot
            .templates
            .iter()
            .find(|template| Some(&template.id) == flow.template_id.as_ref())
            .ok_or("That template no longer exists")?;
        let prefill = template.body.title.clone();
        let space_id = flow.space_id.clone();
        let (parent, placement, anchor) = match &flow.parent {
            Some(parent) => {
                let key = row_key::item(&space_id, parent);
                if self.model.row(&key).is_some_and(|row| row.collapsed) {
                    self.set_collapsed(&key, false, cx);
                }
                (
                    Some(parent.clone()),
                    WorkOutlinePlacement::Last,
                    CreateAnchor::FirstChildOf(key),
                )
            }
            None => {
                let anchor = if self.model.multi_space {
                    let key = row_key::space(&space_id);
                    if self.model.row(&key).is_some_and(|row| row.collapsed) {
                        self.set_collapsed(&key, false, cx);
                    }
                    CreateAnchor::FirstChildOf(key)
                } else {
                    CreateAnchor::Top
                };
                (None, WorkOutlinePlacement::Last, anchor)
            }
        };
        self.cancel_edit(cx);
        self.editing = Some(InlineEdit {
            kind: EditKind::Create {
                space_id,
                parent,
                placement,
                anchor,
                group: None,
                template: Some(Box::new(flow)),
            },
            operation_id: new_operation_id(),
            submitting: false,
        });
        self.start_editor(&prefill, window, cx);
        Ok(())
    }

    /// Create the project: the plan's requests in order as ONE action and ONE
    /// undo step, select the new root, and raise the toast. `Err` is the
    /// refusal to show; nothing was sent or changed then.
    pub(crate) fn instantiate_template(
        &mut self,
        flow: TemplateFlow,
        title: String,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        const BUSY: &str = "Still applying the previous change. Try again in a moment";
        if self.restructuring_busy() || self.log_busy {
            return Err(BUSY.into());
        }
        let space_id = flow.space_id.clone();
        let snapshot = self
            .snapshot(&space_id)
            .filter(|snapshot| snapshot.available)
            .ok_or("This project\u{2019}s outline is not available yet")?;
        let template = snapshot
            .templates
            .iter()
            .find(|template| Some(&template.id) == flow.template_id.as_ref())
            .cloned()
            .ok_or("That template no longer exists")?;
        if let Some(parent) = &flow.parent {
            if !snapshot
                .item(parent)
                .is_some_and(|item| item.archived_at.is_none())
            {
                return Err("That work item is not available".into());
            }
        }
        let title: String = title
            .trim()
            .chars()
            .take(mast_proto::limits::MAX_TITLE_CHARS)
            .collect();
        if title.is_empty() {
            return Err("Give the new project a title".into());
        }

        let operation_key = new_operation_id();
        let run = plan::plan_instantiation(
            &space_id,
            flow.parent.as_deref(),
            &template,
            &title,
            &operation_key,
        );
        let first_op = run.first_operation_id.clone();
        let draft = action_log::instantiate_entry(
            &run.root_id,
            &title,
            &template.name,
            Some(first_op.clone()),
        );
        let root_key = row_key::item(&space_id, &run.root_id);
        if let Some(parent) = &flow.parent {
            self.set_collapsed(&row_key::item(&space_id, parent), false, cx);
        }
        self.multi.clear();
        self.pending_select = Some(root_key.clone());
        self.record_change(&space_id, Some(draft), cx);

        let actions = self.state.read(cx).work_outline_actions();
        let total = run.mutations.len();
        let root_id = run.root_id.clone();
        let linear = flow.linear.clone();
        let link_op = mast_proto::derive_id(&operation_key, "link-linear-project");
        let template_name = template.name.clone();
        cx.spawn(async move |this, cx| {
            // One request at a time: later steps name items earlier ones make.
            let mut sent = 0usize;
            let mut failure = None;
            for mutation in run.mutations {
                match actions.mutate(mutation).await {
                    Ok(_) => sent += 1,
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
            let mut link_note = None;
            if failure.is_none() {
                if let Some(project) = linear {
                    let name = project.name.clone();
                    let result = actions
                        .link_linear_project(mast_proto::LinkLinearProjectParams {
                            work_item_id: root_id.clone(),
                            project,
                            operation_id: link_op,
                        })
                        .await;
                    link_note = Some(match result {
                        Ok(_) => format!(
                            " \u{b7} linked to Linear project {name}. Use \u{201c}Add Issues from {name}\u{2026}\u{201d} on it to add its issues"
                        ),
                        Err(error) => format!(
                            " \u{b7} couldn\u{2019}t link Linear project {name}: {}",
                            error.user_text()
                        ),
                    });
                }
            }
            this.update(cx, |this, cx| {
                match failure {
                    None => {
                        this.confirm_change(&first_op, cx);
                        let mut text = format!(
                            "Created project \u{2018}{title}\u{2019} from template \u{2018}{template_name}\u{2019}"
                        );
                        text.push_str(&link_note.unwrap_or_default());
                        this.show_template_toast(text, cx);
                    }
                    Some(error) => {
                        if sent == 0 {
                            // Nothing was created: nothing to undo.
                            this.forget_change(&first_op, cx);
                            if this.templates_selecting(&root_key) {
                                this.pending_select = None;
                            }
                        } else {
                            // A partial project exists; ⌘Z archives it.
                            this.confirm_change(&first_op, cx);
                        }
                        this.show_error(
                            format!(
                                "Couldn\u{2019}t create the project from the template\n{}{}",
                                error.message,
                                if sent > 0 {
                                    format!(
                                        " ({sent} of {total} steps were applied; undo removes the partial project)"
                                    )
                                } else {
                                    String::new()
                                }
                            ),
                            cx,
                        );
                    }
                }
            })
            .ok();
        })
        .detach();
        Ok(())
    }

    fn templates_selecting(&self, key: &str) -> bool {
        self.pending_select.as_deref() == Some(key)
    }

    // ----------------------------------------------------------------------
    // Toast
    // ----------------------------------------------------------------------

    /// Show the "Created project" toast (with Undo, which is the ⌘Z of the
    /// action log) and hide it after [`TEMPLATE_TOAST_LIFETIME`].
    fn show_template_toast(&mut self, text: String, cx: &mut Context<Self>) {
        self.templates.seq = self.templates.seq.wrapping_add(1);
        let seq = self.templates.seq;
        self.templates.toast = Some(TemplateToast { text, seq });
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(TEMPLATE_TOAST_LIFETIME)
                .await;
            this.update(cx, |this, cx| {
                if this
                    .templates
                    .toast
                    .as_ref()
                    .is_some_and(|toast| toast.seq == seq)
                {
                    this.templates.toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn template_toast_text(&self) -> Option<&str> {
        self.templates.toast.as_ref().map(|toast| toast.text.as_str())
    }

    pub(crate) fn dismiss_template_toast(&mut self, cx: &mut Context<Self>) {
        if self.templates.toast.take().is_some() {
            cx.notify();
        }
    }

    /// The toast's Undo: the action log's ⌘Z (this is its newest entry).
    pub(crate) fn undo_template_toast(&mut self, cx: &mut Context<Self>) {
        self.templates.toast = None;
        self.undo_change(cx);
        cx.notify();
    }

    pub(super) fn render_template_toast(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let text = self.template_toast_text()?.to_owned();
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .flex_none()
                .px(px(6.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .hover(|el| el.bg(theme.element_hover))
                .child(label)
        };
        Some(
            div()
                .debug_selector(|| "outline-template-toast".into())
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.0))
                .mx(px(10.0))
                .mb(px(4.0))
                .px(px(8.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .bg(theme.surface_card)
                .border_1()
                .border_color(theme.border)
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text)
                .child(div().flex_1().min_w_0().child(SharedString::from(text)))
                .child(
                    button("outline-template-toast-undo", "Undo")
                        .on_click(cx.listener(|this, _, _, cx| this.undo_template_toast(cx))),
                )
                .child(
                    button("outline-template-toast-close", "\u{2715}")
                        .on_click(cx.listener(|this, _, _, cx| this.dismiss_template_toast(cx))),
                )
                .into_any_element(),
        )
    }
}
