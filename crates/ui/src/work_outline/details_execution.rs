//! Work Item Details → **Execution** (Milestone 3): the resolved execution
//! configuration and where it comes from, an edit form (execution root,
//! repository bindings with member / primary / worktree / expected branch),
//! Validate on the owning device, Reset to inherited, and the item's
//! exception Chats. The Space-root variant edits the Space default.
//!
//! Persistence policy (same as instructions, acceptance 28): the
//! configuration is written only by Save; Cancel discards; blur never saves;
//! closing, switching items, or Escape with a dirty draft asks "Keep Editing /
//! Discard". Saving or resetting a configuration that changes the resolution
//! of items with bound Chats confirms first: those Chats keep their captured
//! configuration (never retargeted) and show as exceptions.
use super::*;

use gpui::Task;
use mast_proto::{
    ConfigureExecutionParams, DescribeExecutionRepositoryParams, ExecutionConfiguration,
    ExecutionRepositoryIdentity, ExecutionScope, ResolvedExecutionConfiguration,
    ValidateExecutionConfigurationParams, WorkOutlineError, check_execution_configuration,
};

use crate::work_outline::execution::{
    self as exec, BindingDraft, ExecutionDraft, ExecutionFolderPick, FolderPickPurpose,
    ValidationHeadline, ValidationState,
};

impl WorkItemDetails {
    /// The scope this surface configures (Space default for the Space root).
    pub(crate) fn execution_scope(&self) -> ExecutionScope {
        exec::scope_for(&self.space_id, &self.item_id)
    }

    fn resolved_execution(&self) -> Option<ResolvedExecutionConfiguration> {
        self.snapshot
            .as_ref()?
            .resolve_execution(&self.execution_scope())
    }

    /// The Space's owning device (paths live there).
    fn execution_device(&self, cx: &gpui::App) -> Option<String> {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.execution_space.as_ref())
            .map(|space| space.device_id.clone())
            .filter(|device| !device.is_empty())
            .or_else(|| {
                self.state
                    .read(cx)
                    .space_row(&self.space_id)
                    .map(|space| space.device_id.clone())
            })
    }

    fn device_label(&self, device_id: &str, cx: &gpui::App) -> String {
        self.state
            .read(cx)
            .devices
            .iter()
            .find(|device| device.id == device_id)
            .map(|device| device.name.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| device_id.to_owned())
    }

    fn execution_ref_texts(&self, cx: &gpui::App) -> Vec<String> {
        self.execution_ref_inputs
            .iter()
            .map(|input| input.read(cx).text().to_owned())
            .collect()
    }

    /// The configuration the open draft would save.
    pub(crate) fn execution_draft_configuration(
        &self,
        cx: &gpui::App,
    ) -> Option<ExecutionConfiguration> {
        let draft = self.execution_draft.as_ref()?;
        Some(draft.configuration(
            self.execution_root_input.read(cx).text(),
            &self.execution_ref_texts(cx),
        ))
    }

    /// An execution draft that differs from where it started.
    pub(crate) fn execution_draft_dirty(&self, cx: &gpui::App) -> bool {
        match (
            self.execution_draft.as_ref(),
            self.execution_draft_configuration(cx),
        ) {
            (Some(draft), Some(configuration)) => configuration != draft.original,
            _ => false,
        }
    }

    fn new_ref_input(&self, text: &str, cx: &mut Context<Self>) -> Entity<ComposerInput> {
        let text = text.to_owned();
        cx.new(|cx| {
            let mut input = ComposerInput::with_context(
                "Expected branch (optional)",
                crate::composer::PALETTE_SEARCH_CONTEXT,
                cx,
            )
            .with_single_line()
            .with_text_metrics(11.5, 17.0);
            input.set_text(text, cx);
            input
        })
    }

    /// Open the editor prefilled with the scope's resolved configuration
    /// (an inherited one becomes this scope's own row on Save).
    pub(crate) fn begin_execution_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.execution_draft.is_some() {
            return;
        }
        let Some(resolved) = self.resolved_execution() else {
            self.execution_error = Some("The Space is not available yet".into());
            cx.notify();
            return;
        };
        let mut configuration = resolved.configuration.clone();
        if configuration.execution_root.is_empty() && resolved.invalid_reason.is_some() {
            // An unreadable row: start from the Space folder, keep nothing else.
            configuration = ExecutionConfiguration::implicit(
                self.snapshot
                    .as_ref()
                    .and_then(|s| s.execution_space.as_ref())
                    .map(|space| space.path.clone())
                    .unwrap_or_default(),
            );
        }
        let draft = ExecutionDraft::from_configuration(&configuration);
        self.execution_ref_inputs = configuration
            .bindings
            .iter()
            .map(|binding| self.new_ref_input(binding.expected_ref.as_deref().unwrap_or(""), cx))
            .collect();
        let root = draft.execution_root.clone();
        self.execution_root_input
            .update(cx, |input, cx| input.set_text(root, cx));
        self.execution_draft = Some(draft);
        self.execution_open = true;
        self.execution_error = None;
        self.execution_validation = ValidationState::Idle;
        // Worktree choices come from the owning device.
        let count = configuration.bindings.len();
        for ix in 0..count {
            self.describe_binding(ix, cx);
        }
        let handle = self.execution_root_input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Cancel: discard the draft (explicit; no confirmation).
    pub(crate) fn cancel_execution(&mut self, cx: &mut Context<Self>) {
        self.execution_draft = None;
        self.execution_ref_inputs.clear();
        self.execution_error = None;
        cx.notify();
    }

    fn run_execution<T: 'static>(
        &mut self,
        key: &str,
        task: Task<Result<T, WorkOutlineError>>,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<T, WorkOutlineError>, &mut Context<Self>) + 'static,
    ) {
        let key = key.to_owned();
        if !self.in_flight.insert(key.clone()) {
            return;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.in_flight.remove(&key);
                done(this, result, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Run `then` now when `impact` touches no bound Chat; otherwise only
    /// after the Shell's confirmation (Chats keep their captured
    /// configuration and show as exceptions).
    fn with_execution_confirm(
        &mut self,
        impact: exec::ExecutionDriftImpact,
        reset: bool,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) {
        if !impact.needs_confirmation() {
            then(self, cx);
            return;
        }
        let weak = cx.entity().downgrade();
        let request = WorkOutlineConfirmRequest {
            title: if reset {
                "Reset the execution configuration?".into()
            } else {
                "Change the execution configuration?".into()
            },
            body: impact.body().into(),
            confirm_label: if reset { "Reset".into() } else { "Save".into() },
            cancel_label: if reset {
                "Cancel".into()
            } else {
                "Keep Editing".into()
            },
            destructive: false,
            on_decision: Box::new(move |confirmed, _window, cx| {
                if confirmed {
                    weak.update(cx, |this, cx| then(this, cx)).ok();
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
            self.execution_error = Some("This change needs the app window to confirm".into());
            cx.notify();
        }
    }

    /// Save (explicit). Unchanged content just closes the editor.
    pub(crate) fn save_execution(&mut self, cx: &mut Context<Self>) {
        let (Some(configuration), Some(snapshot)) = (
            self.execution_draft_configuration(cx),
            self.snapshot.clone(),
        ) else {
            return;
        };
        if let Err(reason) = check_execution_configuration(&configuration) {
            self.execution_error = Some(reason);
            cx.notify();
            return;
        }
        let unchanged = self
            .execution_draft
            .as_ref()
            .is_some_and(|draft| draft.original == configuration);
        if unchanged {
            self.cancel_execution(cx);
            return;
        }
        let scope = self.execution_scope();
        let impact = exec::execution_drift(&snapshot, &scope, Some(&configuration));
        self.with_execution_confirm(impact, false, cx, move |this, cx| {
            this.send_execution(Some(configuration), cx)
        });
    }

    fn send_execution(
        &mut self,
        configuration: Option<ExecutionConfiguration>,
        cx: &mut Context<Self>,
    ) {
        let inputs = serde_json::to_string(&configuration).unwrap_or_default();
        let operation_id = match (configuration.is_some(), self.execution_draft.as_mut()) {
            (true, Some(draft)) => draft.op.id_for(&inputs),
            _ => self.execution_reset_op.id_for(&inputs),
        };
        let reset = configuration.is_none();
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .configure_execution(ConfigureExecutionParams {
                scope: self.execution_scope(),
                configuration,
                operation_id,
            });
        self.execution_error = None;
        self.run_execution(
            "execution:save",
            task,
            cx,
            move |this, result, _| match result {
                Ok(_) => {
                    if reset {
                        this.execution_reset_op = Default::default();
                    } else {
                        this.execution_draft = None;
                        this.execution_ref_inputs.clear();
                    }
                    this.execution_validation = ValidationState::Idle;
                }
                Err(error) => this.execution_error = Some(exec::execution_error_text(&error)),
            },
        );
    }

    /// Reset to inherited: tombstone this scope's own row.
    pub(crate) fn reset_execution(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let scope = self.execution_scope();
        if !exec::owns_row(&snapshot, &scope) {
            return;
        }
        let impact = exec::execution_drift(&snapshot, &scope, None);
        self.with_execution_confirm(impact, true, cx, |this, cx| this.send_execution(None, cx));
    }

    /// Validate on the owning device: the open draft when it is dirty (sent
    /// as a resolved snapshot), else the scope's saved resolution.
    pub(crate) fn validate_execution(&mut self, cx: &mut Context<Self>) {
        let scope = self.execution_scope();
        let params = match (
            self.execution_draft_dirty(cx),
            self.execution_draft_configuration(cx),
        ) {
            (true, Some(configuration)) => {
                let Some(device_id) = self.execution_device(cx) else {
                    self.execution_error = Some("The Space is not available yet".into());
                    cx.notify();
                    return;
                };
                ValidateExecutionConfigurationParams {
                    resolved: Some(ResolvedExecutionConfiguration {
                        space_id: self.space_id.clone(),
                        device_id,
                        configuration,
                        source_scope: exec::source_of(&scope),
                        invalid_reason: None,
                    }),
                    ..Default::default()
                }
            }
            _ => ValidateExecutionConfigurationParams {
                scope: Some(scope),
                ..Default::default()
            },
        };
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .validate_execution(params);
        self.execution_validation = ValidationState::Running;
        self.run_execution("execution:validate", task, cx, |this, result, _| {
            this.execution_validation = match result {
                Ok(reply) => ValidationState::Done(reply),
                Err(error) => ValidationState::Failed(exec::execution_error_text(&error)),
            };
        });
    }

    /// Open the folder picker on the Space's device.
    pub(crate) fn pick_execution_folder(
        &mut self,
        purpose: FolderPickPurpose,
        cx: &mut Context<Self>,
    ) {
        let Some(device_id) = self.execution_device(cx) else {
            self.execution_error = Some("The Space is not available yet".into());
            cx.notify();
            return;
        };
        let start_path = Some(self.execution_root_input.read(cx).text().trim().to_owned())
            .filter(|path| mast_proto::is_absolute_device_path(path));
        let weak = cx.entity().downgrade();
        let (title, confirm_label) = match purpose {
            FolderPickPurpose::ExecutionRoot => ("Execution root", "Use this folder"),
            FolderPickPurpose::Repository => ("Add repository", "Add repository"),
        };
        let request = ExecutionFolderPick {
            title: title.into(),
            confirm_label: confirm_label.into(),
            device_id,
            start_path,
            reject_home_directory: purpose == FolderPickPurpose::ExecutionRoot,
            on_pick: Box::new(move |path, cx| {
                weak.update(cx, |this, cx| match purpose {
                    FolderPickPurpose::ExecutionRoot => this.set_execution_root(&path, cx),
                    FolderPickPurpose::Repository => this.add_execution_repository(path, cx),
                })
                .ok();
            }),
        };
        if let Some(shell) = self.shell.upgrade() {
            shell.update(cx, |shell, cx| shell.open_folder_pick(request, cx));
            return;
        }
        #[cfg(test)]
        {
            self.captured_folder_pick = Some(request);
        }
        #[cfg(not(test))]
        {
            drop(request);
            self.execution_error = Some("Choosing a folder needs the app window".into());
            cx.notify();
        }
    }

    pub(crate) fn set_execution_root(&mut self, path: &str, cx: &mut Context<Self>) {
        if self.execution_draft.is_none() {
            return;
        }
        let path = path.to_owned();
        self.execution_root_input
            .update(cx, |input, cx| input.set_text(path, cx));
        cx.notify();
    }

    fn describe_params(&self, path: String, cx: &gpui::App) -> DescribeExecutionRepositoryParams {
        let target_device_id = self
            .execution_device(cx)
            .filter(|device| !self.state.read(cx).is_local_device(device));
        DescribeExecutionRepositoryParams {
            path,
            target_device_id,
        }
    }

    /// Add the repository at `path` (from the picker): the owning device
    /// describes it (id, fingerprint, checkouts); a folder that is not a Git
    /// checkout is refused and nothing is added.
    pub(crate) fn add_execution_repository(&mut self, path: String, cx: &mut Context<Self>) {
        if self.execution_draft.is_none() {
            return;
        }
        let params = self.describe_params(path.clone(), cx);
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .describe_repository(params);
        self.execution_error = None;
        self.run_execution("execution:describe", task, cx, move |this, result, cx| {
            let identity = match result {
                Ok(identity) if !identity.repo_id.is_empty() => identity,
                Ok(_) => {
                    this.execution_error = Some(format!("{path} is not a Git repository"));
                    return;
                }
                Err(error) => {
                    this.execution_error = Some(format!(
                        "Couldn\u{2019}t add {path}: {}",
                        exec::execution_error_text(&error)
                    ));
                    return;
                }
            };
            let Some(draft) = this.execution_draft.as_mut() else {
                return;
            };
            match draft.add(exec::binding_from_identity(&identity)) {
                Ok(_) => {
                    let input = this.new_ref_input("", cx);
                    this.execution_ref_inputs.push(input);
                }
                Err(reason) => this.execution_error = Some(reason),
            }
        });
    }

    /// Fill a binding's worktree choices from the owning device.
    fn describe_binding(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(path) = self
            .execution_draft
            .as_mut()
            .and_then(|draft| draft.bindings.get_mut(ix))
            .map(|binding| {
                binding.describing = true;
                binding.describe_error = None;
                binding.binding.repo_path.clone()
            })
        else {
            return;
        };
        let repo_id = self
            .execution_draft
            .as_ref()
            .map(|draft| draft.bindings[ix].binding.repo_id.clone())
            .unwrap_or_default();
        let params = self.describe_params(path, cx);
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .describe_repository(params);
        self.run_execution(
            &format!("execution:describe:{repo_id}"),
            task,
            cx,
            move |this, result: Result<ExecutionRepositoryIdentity, _>, _| {
                let Some(binding) = this.execution_draft.as_mut().and_then(|draft| {
                    draft
                        .bindings
                        .iter_mut()
                        .find(|b| b.binding.repo_id == repo_id)
                }) else {
                    return;
                };
                binding.describing = false;
                match result {
                    Ok(identity) if identity.repo_id == repo_id => {
                        binding.checkouts = identity.worktrees;
                    }
                    Ok(_) => {
                        binding.describe_error =
                            Some("This path now holds a different repository".into());
                    }
                    Err(error) => {
                        binding.describe_error = Some(exec::execution_error_text(&error));
                    }
                }
            },
        );
    }

    pub(crate) fn remove_execution_binding(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(draft) = self.execution_draft.as_mut() {
            draft.remove(ix);
            if ix < self.execution_ref_inputs.len() {
                self.execution_ref_inputs.remove(ix);
            }
            cx.notify();
        }
    }

    pub(crate) fn edit_execution_binding(
        &mut self,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut ExecutionDraft),
    ) {
        if let Some(draft) = self.execution_draft.as_mut() {
            edit(draft);
            cx.notify();
        }
    }

    // ----------------------------------------------------------------------
    // Rendering
    // ----------------------------------------------------------------------

    fn checkbox(checked: bool, round: bool, theme: &Theme) -> gpui::Div {
        div()
            .flex_none()
            .size(px(12.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(if round { 6.0 } else { 3.0 }))
            .border_1()
            .border_color(theme.border_strong)
            .when(checked && round, |el| {
                el.child(div().size(px(6.0)).rounded_full().bg(theme.accent))
            })
            .when(checked && !round, |el| {
                el.child(
                    crate::icons::icon(crate::icons::CHECK)
                        .size(px(9.0))
                        .text_color(theme.text),
                )
            })
    }

    fn muted_line(text: impl Into<SharedString>, theme: &Theme) -> gpui::Div {
        div()
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text_muted)
            .child(text.into())
    }

    fn render_validation(&self, theme: &Theme, cx: &gpui::App) -> Option<AnyElement> {
        let block = div()
            .debug_selector(|| "work-item-execution-validation".into())
            .flex()
            .flex_col()
            .gap(px(3.0))
            .p(px(6.0))
            .rounded(px(4.0))
            .bg(theme.element_hover.opacity(0.4));
        let element = match &self.execution_validation {
            ValidationState::Idle => return None,
            ValidationState::Running => block.child(Self::muted_line("Validating\u{2026}", theme)),
            ValidationState::Failed(message) => block.child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.warning)
                    .child(SharedString::from(message.clone())),
            ),
            ValidationState::Done(reply) => {
                let headline = exec::validation_headline(reply);
                let (text, color) = match &headline {
                    ValidationHeadline::Ready => {
                        let device = reply
                            .validated
                            .as_ref()
                            .map(|v| self.device_label(&v.device_id, cx))
                            .unwrap_or_default();
                        (format!("Ready on {device}"), theme.success)
                    }
                    ValidationHeadline::DeviceOffline { device_id } => (
                        format!(
                            "{} is offline \u{2014} nothing was checked",
                            self.device_label(device_id, cx)
                        ),
                        theme.warning,
                    ),
                    ValidationHeadline::HostUnsupported { device_id } => (
                        format!(
                            "{} runs a Mast that can\u{2019}t validate execution",
                            self.device_label(device_id, cx)
                        ),
                        theme.warning,
                    ),
                    ValidationHeadline::Blocked { count } => (
                        format!(
                            "Blocked \u{b7} {count} problem{}",
                            if *count == 1 { "" } else { "s" }
                        ),
                        theme.danger,
                    ),
                };
                let kind = match &headline {
                    ValidationHeadline::Ready => "ready",
                    ValidationHeadline::DeviceOffline { .. } => "device-offline",
                    ValidationHeadline::HostUnsupported { .. } => "host-unsupported",
                    ValidationHeadline::Blocked { .. } => "blocked",
                };
                let mut block = block.child(
                    div()
                        .debug_selector(move || format!("work-item-execution-validation-{kind}"))
                        .text_size(crate::typography::ui_rems(11.5))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(color)
                        .child(SharedString::from(text)),
                );
                if let Some(validated) = reply.validated.as_ref() {
                    block = block.children(validated.repositories.iter().map(|repo| {
                        Self::muted_line(
                            format!(
                                "{} \u{b7} {} \u{b7} {}",
                                exec::path_name(&repo.repo_path),
                                exec::ref_label(repo.branch.as_deref(), repo.head_sha.as_deref()),
                                repo.checkout_path
                            ),
                            theme,
                        )
                    }));
                }
                block.children(exec::validation_issues(reply).into_iter().enumerate().map(
                    |(ix, issue)| {
                        div()
                            .debug_selector(move || format!("work-item-execution-issue-{ix}"))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(crate::typography::ui_rems(11.0))
                                    .text_color(theme.text)
                                    .child(SharedString::from(issue.message)),
                            )
                            .child(
                                div()
                                    .text_size(crate::typography::ui_rems(10.5))
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(issue.remediation)),
                            )
                    },
                ))
            }
        };
        Some(element.into_any_element())
    }

    fn render_exceptions(
        &self,
        snapshot: &WorkOutlineSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.is_space_root() {
            return None;
        }
        let exceptions = exec::exception_bindings(snapshot, &self.item_id);
        if exceptions.is_empty() {
            return None;
        }
        Some(
            div()
                .debug_selector(|| "work-item-execution-exceptions".into())
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(11.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text_muted)
                        .child("Exceptions"),
                )
                .children(exceptions.into_iter().enumerate().map(|(ix, binding)| {
                    let chat = self.chats.iter().find(|chat| chat.id == binding.chat_id);
                    let title = chat.map_or_else(
                        || format!("Missing chat \u{b7} {}", binding.chat_id),
                        |chat| chat.title.clone(),
                    );
                    let open_id = binding.chat_id.clone();
                    let missing = chat.is_none();
                    div()
                        .id(("work-item-execution-exception", ix))
                        .debug_selector(move || format!("work-item-execution-exception-{ix}"))
                        .flex()
                        .flex_col()
                        .px(px(4.0))
                        .rounded(px(4.0))
                        .hover(|el| el.bg(theme.element_hover))
                        .when(!missing, |el| el.cursor_pointer())
                        .child(
                            div()
                                .flex()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(crate::typography::ui_rems(12.0))
                                        .text_color(theme.text)
                                        .child(SharedString::from(title)),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_size(crate::typography::ui_rems(10.5))
                                        .text_color(theme.text_muted)
                                        .child("uses a different configuration"),
                                ),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(crate::typography::ui_rems(10.5))
                                .text_color(theme.text_faint)
                                .child(SharedString::from(exec::captured_summary(
                                    &binding.resolved.configuration,
                                ))),
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            if !missing {
                                cx.emit(DetailsEvent::OpenChat(open_id.clone()));
                            }
                        }))
                }))
                .into_any_element(),
        )
    }

    fn render_execution_view(
        &self,
        snapshot: &WorkOutlineSnapshot,
        resolved: &ResolvedExecutionConfiguration,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let scope = self.execution_scope();
        let source = exec::source_label(snapshot, &scope, resolved);
        let configuration = &resolved.configuration;
        let archived = self.item().is_some_and(|item| item.archived_at.is_some());
        let validating = self.in_flight.contains("execution:validate");
        let saving = self.in_flight.contains("execution:save");
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .debug_selector(|| "work-item-execution-source".into())
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(format!("Source: {source}"))),
            )
            .child(
                div()
                    .debug_selector(|| "work-item-execution-root".into())
                    .truncate()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text)
                    .child(SharedString::from(
                        if configuration.execution_root.is_empty() {
                            "No execution root".to_owned()
                        } else {
                            configuration.execution_root.clone()
                        },
                    )),
            )
            .when_some(resolved.invalid_reason.clone(), |el, reason| {
                el.child(
                    div()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.warning)
                        .child(SharedString::from(format!(
                            "This configuration can\u{2019}t be used: {reason}"
                        ))),
                )
            })
            .when(configuration.bindings.is_empty(), |el| {
                el.child(
                    div()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_faint)
                        .child("No repositories bound"),
                )
            })
            .children(
                configuration
                    .bindings
                    .iter()
                    .enumerate()
                    .map(|(ix, binding)| {
                        let tip: SharedString = binding.checkout_path().to_owned().into();
                        div()
                            .id(("work-item-execution-repo", ix))
                            .debug_selector(move || format!("work-item-execution-repo-{ix}"))
                            .flex()
                            .gap(px(6.0))
                            .text_size(crate::typography::ui_rems(11.5))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(theme.text)
                                    .child(SharedString::from(exec::path_name(&binding.repo_path))),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(exec::binding_summary(binding))),
                            )
                            .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
                    }),
            )
            .child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .when(!archived, |el| {
                        el.child(
                            Self::small_button("work-item-execution-edit", "Edit", theme)
                                .debug_selector(|| "work-item-execution-edit".into())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.begin_execution_draft(window, cx)
                                })),
                        )
                    })
                    .child(
                        Self::small_button(
                            "work-item-execution-validate",
                            if validating {
                                "Validating\u{2026}"
                            } else {
                                "Validate"
                            },
                            theme,
                        )
                        .debug_selector(|| "work-item-execution-validate".into())
                        .on_click(cx.listener(|this, _, _, cx| this.validate_execution(cx))),
                    )
                    .when(exec::owns_row(snapshot, &scope), |el| {
                        el.child(
                            Self::small_button(
                                "work-item-execution-reset",
                                if saving {
                                    "Resetting\u{2026}"
                                } else {
                                    "Reset to inherited"
                                },
                                theme,
                            )
                            .debug_selector(|| "work-item-execution-reset".into())
                            .on_click(cx.listener(|this, _, _, cx| this.reset_execution(cx))),
                        )
                    }),
            )
    }

    fn render_binding_editor(
        &self,
        ix: usize,
        draft: &BindingDraft,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let binding = &draft.binding;
        let selected_worktree = binding.worktree_id.clone();
        let choices: Vec<(Option<(String, String)>, String)> =
            std::iter::once((None, "Repository path".to_owned()))
                .chain(draft.worktree_choices().into_iter().map(|checkout| {
                    let label = format!(
                        "{}{} \u{b7} {}",
                        exec::path_name(&checkout.path),
                        if checkout.main { " (main)" } else { "" },
                        exec::ref_label(checkout.branch.as_deref(), checkout.head_sha.as_deref())
                    );
                    (
                        checkout
                            .checkout_id
                            .clone()
                            .map(|id| (id, checkout.path.clone())),
                        label,
                    )
                }))
                .collect();
        // A pinned worktree the description did not (yet) list stays visible.
        let pinned_unlisted = binding.worktree_id.as_ref().filter(|id| {
            !choices
                .iter()
                .any(|(choice, _)| choice.as_ref().map(|(c, _)| c) == Some(*id))
        });
        let ref_input = self.execution_ref_inputs.get(ix).cloned();
        div()
            .debug_selector(move || format!("work-item-execution-binding-{ix}"))
            .flex()
            .flex_col()
            .gap(px(3.0))
            .p(px(6.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex_none()
                            .text_size(crate::typography::ui_rems(12.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(SharedString::from(exec::path_name(&binding.repo_path))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(10.5))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(binding.repo_path.clone())),
                    )
                    .child(
                        Self::small_button(("work-item-execution-remove", ix), "Remove", theme)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_execution_binding(ix, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(12.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child(
                        div()
                            .id(("work-item-execution-member", ix))
                            .debug_selector(move || format!("work-item-execution-member-{ix}"))
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .cursor_pointer()
                            .hover(|el| el.text_color(theme.text))
                            .child(Self::checkbox(binding.member, false, theme))
                            .child("Member")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.edit_execution_binding(cx, |d| d.toggle_member(ix))
                            })),
                    )
                    .child(
                        div()
                            .id(("work-item-execution-primary", ix))
                            .debug_selector(move || format!("work-item-execution-primary-{ix}"))
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .cursor_pointer()
                            .hover(|el| el.text_color(theme.text))
                            .child(Self::checkbox(binding.primary, true, theme))
                            .child("Primary")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.edit_execution_binding(cx, |d| d.set_primary(ix))
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(4.0))
                    .child(Self::muted_line("Checkout:", theme))
                    .children(
                        choices
                            .into_iter()
                            .enumerate()
                            .map(|(cix, (choice, label))| {
                                let active =
                                    choice.as_ref().map(|(id, _)| id.clone()) == selected_worktree;
                                div()
                                    .id(SharedString::from(format!(
                                        "work-item-execution-worktree-{ix}-{cix}"
                                    )))
                                    .debug_selector(move || {
                                        format!("work-item-execution-worktree-{ix}-{cix}")
                                    })
                                    .px(px(5.0))
                                    .rounded(px(3.0))
                                    .border_1()
                                    .border_color(if active { theme.accent } else { theme.border })
                                    .cursor_pointer()
                                    .text_size(crate::typography::ui_rems(10.5))
                                    .text_color(if active { theme.text } else { theme.text_muted })
                                    .hover(|el| el.bg(theme.element_hover))
                                    .child(SharedString::from(label))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let choice = choice.clone();
                                        this.edit_execution_binding(cx, |d| {
                                            d.choose_worktree(ix, choice)
                                        })
                                    }))
                            }),
                    )
                    .when_some(pinned_unlisted.cloned(), |el, _| {
                        el.child(
                            div()
                                .px(px(5.0))
                                .rounded(px(3.0))
                                .border_1()
                                .border_color(theme.accent)
                                .text_size(crate::typography::ui_rems(10.5))
                                .text_color(theme.text)
                                .child(SharedString::from(format!(
                                    "{} (pinned)",
                                    exec::path_name(binding.checkout_path())
                                ))),
                        )
                    })
                    .when(draft.describing, |el| {
                        el.child(Self::muted_line("Loading worktrees\u{2026}", theme))
                    }),
            )
            .when_some(draft.describe_error.clone(), |el, error| {
                el.child(
                    div()
                        .text_size(crate::typography::ui_rems(10.5))
                        .text_color(theme.warning)
                        .child(SharedString::from(error)),
                )
            })
            .when_some(ref_input, |el, input| {
                el.child(Self::input_frame(&input, theme).border_color(theme.border))
            })
            .into_any_element()
    }

    fn render_execution_editor(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let Some(draft) = self.execution_draft.as_ref() else {
            return div();
        };
        let saving = self.in_flight.contains("execution:save");
        let describing = self.in_flight.contains("execution:describe");
        let validating = self.in_flight.contains("execution:validate");
        let bindings: Vec<AnyElement> = draft
            .bindings
            .iter()
            .enumerate()
            .map(|(ix, binding)| self.render_binding_editor(ix, binding, theme, cx))
            .collect();
        div()
            .debug_selector(|| "work-item-execution-editor".into())
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(Self::muted_line("Execution root", theme))
            .child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Self::input_frame(&self.execution_root_input, theme)),
                    )
                    .child(
                        Self::small_button(
                            "work-item-execution-choose-root",
                            "Choose\u{2026}",
                            theme,
                        )
                        .debug_selector(|| "work-item-execution-choose-root".into())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.pick_execution_folder(FolderPickPurpose::ExecutionRoot, cx)
                        })),
                    ),
            )
            .child(Self::muted_line("Repositories", theme))
            .children(bindings)
            .child(
                div().child(
                    Self::small_button(
                        "work-item-execution-add-repo",
                        if describing {
                            "Reading repository\u{2026}"
                        } else {
                            "Add repository\u{2026}"
                        },
                        theme,
                    )
                    .debug_selector(|| "work-item-execution-add-repo".into())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pick_execution_folder(FolderPickPurpose::Repository, cx)
                    })),
                ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .child(
                        Self::small_button(
                            "work-item-execution-save",
                            if saving { "Saving\u{2026}" } else { "Save" },
                            theme,
                        )
                        .debug_selector(|| "work-item-execution-save".into())
                        .on_click(cx.listener(|this, _, _, cx| this.save_execution(cx))),
                    )
                    .child(
                        Self::small_button("work-item-execution-cancel", "Cancel", theme)
                            .debug_selector(|| "work-item-execution-cancel".into())
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_execution(cx))),
                    )
                    .child(
                        Self::small_button(
                            "work-item-execution-validate",
                            if validating {
                                "Validating\u{2026}"
                            } else {
                                "Validate"
                            },
                            theme,
                        )
                        .debug_selector(|| "work-item-execution-validate".into())
                        .on_click(cx.listener(|this, _, _, cx| this.validate_execution(cx))),
                    ),
            )
    }

    pub(super) fn render_execution_section(
        &mut self,
        snapshot: &WorkOutlineSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let exception_count = if self.is_space_root() {
            0
        } else {
            snapshot.execution_exception_chats(&self.item_id).len()
        };
        let open = self.execution_open || self.execution_draft.is_some();
        let title = if self.is_space_root() {
            "Execution (Space default)"
        } else {
            "Execution"
        };
        let header = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                Self::section_header("work-item-execution-toggle", title.into(), open, theme)
                    .debug_selector(|| "work-item-execution-toggle".into())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.execution_open = !this.execution_open;
                        cx.notify();
                    })),
            )
            .when(exception_count > 0, |el| {
                let tip = exec::exception_tooltip(exception_count);
                el.child(
                    div()
                        .id("work-item-execution-exceptions-marker")
                        .debug_selector(|| "work-item-execution-exceptions-marker".into())
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
                        .child(SharedString::from(format!(
                            "{exception_count} exception{}",
                            if exception_count == 1 { "" } else { "s" }
                        )))
                        .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into()),
                )
            });
        let mut section = div()
            .debug_selector(|| "work-item-execution".into())
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(header);
        if !open {
            return section.into_any_element();
        }
        if self.execution_draft.is_some() {
            section = section.child(self.render_execution_editor(theme, cx));
        } else if let Some(resolved) = self.resolved_execution() {
            section = section.child(self.render_execution_view(snapshot, &resolved, theme, cx));
        } else {
            section = section.child(Self::muted_line(
                "Execution configuration needs the Space to be available",
                theme,
            ));
        }
        section
            .children(self.render_validation(theme, cx))
            .children(self.render_exceptions(snapshot, theme, cx))
            .when_some(self.execution_error.clone(), |el, error| {
                el.child(
                    div()
                        .debug_selector(|| "work-item-execution-error".into())
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.warning)
                        .child(SharedString::from(error)),
                )
            })
            .into_any_element()
    }
}
