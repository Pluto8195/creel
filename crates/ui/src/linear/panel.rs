//! The Linear panel: the Overview's right-side surface for Linear-enabled
//! Spaces (same slot rules as Required Actions: Work Item Details render
//! over it; closing them shows it again).
//!
//! - **My issues** (`LinearMyIssues`) and **My projects**
//!   (`LinearMyProjects`, each expandable to `LinearProjectIssues` with
//!   multi-select), filtered client-side by the search box. Refreshed on
//!   open and by ↻ only.
//! - Rows referenced by a live item of the panel's Space show "Linked" and
//!   Reveal (selects the item in the Outline).
//! - Explicit actions only: Create (`createWorkItemFromLinear` under the
//!   Outline's selected item, or a parent the user picks; placement Last),
//!   Link (issue: `createWorkItemExternalReference`; project:
//!   `linkLinearProject`) to the selected item, drag an issue onto an
//!   Outline item, and Create nodes for the selected issues of a project.
//!   Each action keeps one operation id per issue/target, so a retry or a
//!   double click replays instead of duplicating.
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Subscription,
    Window, div, prelude::*, px,
};
use zeron_proto::linear::{
    LINEAR_ISSUE_REFERENCE, LINEAR_PROVIDER, LinearErrorCode, LinearIssueLink, LinearIssueSummary,
    LinearProjectSummary,
};
use zeron_proto::{
    CreateWorkItemExternalReferenceParams, CreateWorkItemFromLinearParams, LinkLinearProjectParams,
    WorkOutlinePlacement, WorkOutlineSnapshot,
};

use super::LinkedIndex;
use super::client::LinearUiError;
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::state::AppState;
use crate::theme::Theme;
use crate::work_outline::actions::OperationSlot;
use crate::work_outline::chips::TextTooltip;
use crate::work_outline::model;
use crate::work_outline::outline_view::OutlineDrag;
use crate::work_outline::outline_view::linear_outline::LinearIssueDrag;

/// Fixed width of the panel (the details surface's).
pub(crate) const LINEAR_PANEL_W: f32 = crate::work_outline::details_panel::DETAILS_PANEL_W;

/// Whether the palette lists Open Linear: some Space has Linear on.
pub(crate) fn palette_visible(state: &AppState) -> bool {
    state.spaces.iter().any(|space| {
        super::space_enabled(
            state
                .work_outline(&space.id)
                .snapshot()
                .map(|snapshot| snapshot.as_ref()),
        )
    })
}

/// One Linear-enabled Space the panel can show.
#[derive(Debug, Clone)]
pub(crate) struct LinearSpace {
    pub id: String,
    pub label: String,
    pub snapshot: Arc<WorkOutlineSnapshot>,
}

/// The Outline's selection, as the panel's actions read it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum OutlineSelection {
    #[default]
    None,
    Item {
        space_id: String,
        item_id: String,
    },
    /// A Space root row (create at the top level).
    Space(String),
    /// A Chat or another row: no parent.
    Other,
}

/// Where Create puts new items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParentTarget {
    /// Under this item (`None` = top level).
    Resolved(Option<String>),
    /// Nothing usable is selected: ask.
    Ask,
}

/// The parent Create uses for `selection` in `space_id`.
pub(crate) fn parent_target(
    selection: &OutlineSelection,
    space_id: &str,
    snapshot: &WorkOutlineSnapshot,
) -> ParentTarget {
    match selection {
        OutlineSelection::Item {
            space_id: space,
            item_id,
        } if space == space_id
            && snapshot
                .item(item_id)
                .is_some_and(|item| item.archived_at.is_none()) =>
        {
            ParentTarget::Resolved(Some(item_id.clone()))
        }
        OutlineSelection::Space(space) if space == space_id => ParentTarget::Resolved(None),
        _ => ParentTarget::Ask,
    }
}

fn query_matches(query: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    query.split_whitespace().all(|word| text.contains(word))
}

/// Whether an issue row matches the search (identifier, title, state,
/// assignee, project, labels).
pub(crate) fn issue_matches(query: &str, issue: &LinearIssueSummary) -> bool {
    let text = format!(
        "{} {} {} {} {} {}",
        issue.identifier,
        issue.title,
        issue.state.name,
        issue
            .assignee
            .as_ref()
            .map(super::user_name)
            .unwrap_or_default(),
        issue
            .project
            .as_ref()
            .map(|p| p.name.as_str())
            .unwrap_or(""),
        issue
            .labels
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    );
    query_matches(query, &text)
}

/// Whether a project row matches the search (name, state, lead).
pub(crate) fn project_matches(query: &str, project: &LinearProjectSummary) -> bool {
    let text = format!(
        "{} {} {}",
        project.name,
        project.state.as_deref().unwrap_or(""),
        project
            .lead
            .as_ref()
            .map(super::user_name)
            .unwrap_or_default()
    );
    query_matches(query, &text)
}

/// A list's load state.
#[derive(Debug, Clone)]
pub(crate) enum Load<T> {
    Loading,
    Ready(T),
    Failed(LinearUiError),
}

impl<T> Load<T> {
    pub(crate) fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            _ => None,
        }
    }

    fn error(&self) -> Option<&LinearUiError> {
        match self {
            Self::Failed(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinearPanelEvent {
    Close,
    /// Select and reveal this item in the Outline.
    Reveal {
        space_id: String,
        item_id: String,
    },
    /// A create landed: select and reveal the new item once it shows.
    Created {
        space_id: String,
        item_id: String,
        parent_id: Option<String>,
    },
    /// Open Settings → Linear (connect / reconnect).
    OpenSettings,
}

/// A short status line under the search box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PanelNotice {
    pub text: String,
    pub error: bool,
}

/// Issues waiting for the user to pick a parent.
#[derive(Debug, Clone)]
pub(crate) struct ParentPicker {
    pub issues: Vec<LinearIssueSummary>,
    /// Clear this project's checked issues once created.
    pub project_id: Option<String>,
}

pub(crate) struct LinearPanel {
    state: Entity<AppState>,
    focus: FocusHandle,
    search: Entity<ComposerInput>,
    pub(crate) space_id: String,
    spaces: Vec<LinearSpace>,
    selection: OutlineSelection,
    pub(crate) my_issues: Load<Vec<LinearIssueSummary>>,
    pub(crate) my_projects: Load<Vec<LinearProjectSummary>>,
    pub(crate) expanded: BTreeSet<String>,
    pub(crate) project_issues: HashMap<String, Load<Vec<LinearIssueSummary>>>,
    /// Per project: the identifiers checked for Create nodes for selected.
    pub(crate) checked: HashMap<String, BTreeSet<String>>,
    op_slots: HashMap<String, OperationSlot>,
    pub(crate) busy: HashSet<String>,
    pub(crate) picker: Option<ParentPicker>,
    pub(crate) notice: Option<PanelNotice>,
    generation: u64,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<LinearPanelEvent> for LinearPanel {}

impl Focusable for LinearPanel {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl LinearPanel {
    pub(crate) fn new(state: Entity<AppState>, space_id: String, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            ComposerInput::with_context(
                "Search issues and projects",
                crate::composer::PALETTE_SEARCH_CONTEXT,
                cx,
            )
            .with_single_line()
            .with_text_metrics(12.0, 18.0)
        });
        let subscriptions = vec![cx.subscribe(&search, |_: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                cx.notify();
            }
        })];
        Self {
            state,
            focus: cx.focus_handle(),
            search,
            space_id,
            spaces: Vec::new(),
            selection: OutlineSelection::None,
            my_issues: Load::Loading,
            my_projects: Load::Loading,
            expanded: BTreeSet::new(),
            project_issues: HashMap::new(),
            checked: HashMap::new(),
            op_slots: HashMap::new(),
            busy: HashSet::new(),
            picker: None,
            notice: None,
            generation: 0,
            _subscriptions: subscriptions,
        }
    }

    /// Fed by the Overview every frame: the Linear-enabled Spaces and the
    /// Outline's selection. A Space that lost Linear moves the panel to the
    /// first enabled one.
    pub(crate) fn set_data(&mut self, spaces: Vec<LinearSpace>, selection: OutlineSelection) {
        if !spaces.iter().any(|space| space.id == self.space_id)
            && let Some(first) = spaces.first()
        {
            self.space_id = first.id.clone();
            self.picker = None;
        }
        self.spaces = spaces;
        self.selection = selection;
    }

    pub(crate) fn set_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        if self.space_id != space_id {
            self.space_id = space_id;
            self.picker = None;
            cx.notify();
        }
    }

    fn snapshot(&self) -> Option<&Arc<WorkOutlineSnapshot>> {
        self.spaces
            .iter()
            .find(|space| space.id == self.space_id)
            .map(|space| &space.snapshot)
    }

    pub(crate) fn query(&self, cx: &gpui::App) -> String {
        self.search.read(cx).text().trim().to_lowercase()
    }

    #[cfg(test)]
    pub(crate) fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        self.search
            .update(cx, |input, cx| input.set_text(query, cx));
    }

    // ----------------------------------------------------------------------
    // Loading
    // ----------------------------------------------------------------------

    /// Re-fetch My issues, My projects, and every expanded project.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let actions = self.state.read(cx).work_outline_actions();
        self.my_issues = Load::Loading;
        self.my_projects = Load::Loading;
        let issues = actions.linear_my_issues();
        let projects = actions.linear_my_projects();
        cx.spawn(async move |this, cx| {
            let issues = issues.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                if let Ok(issues) = &issues {
                    this.state
                        .update(cx, |state, _| state.linear.remember_issues(issues.iter()));
                }
                this.my_issues = match issues {
                    Ok(issues) => Load::Ready(issues),
                    Err(error) => Load::Failed(error),
                };
                cx.notify();
            })
            .ok();
            let projects = projects.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                if let Ok(projects) = &projects {
                    let projects = projects.clone();
                    this.state
                        .update(cx, |state, _| state.linear.projects = Some(projects));
                }
                this.my_projects = match projects {
                    Ok(projects) => Load::Ready(projects),
                    Err(error) => Load::Failed(error),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        for project_id in self.expanded.clone() {
            self.load_project(project_id, cx);
        }
        cx.notify();
    }

    fn load_project(&mut self, project_id: String, cx: &mut Context<Self>) {
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .linear_project_issues(&project_id);
        self.project_issues
            .insert(project_id.clone(), Load::Loading);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if let Ok(issues) = &result {
                    this.state
                        .update(cx, |state, _| state.linear.remember_issues(issues.iter()));
                }
                this.project_issues.insert(
                    project_id,
                    match result {
                        Ok(issues) => Load::Ready(issues),
                        Err(error) => Load::Failed(error),
                    },
                );
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn toggle_project(&mut self, project_id: &str, cx: &mut Context<Self>) {
        if self.expanded.remove(project_id) {
            cx.notify();
            return;
        }
        self.expanded.insert(project_id.to_string());
        if !matches!(
            self.project_issues.get(project_id),
            Some(Load::Ready(_) | Load::Loading)
        ) {
            self.load_project(project_id.to_string(), cx);
        }
        cx.notify();
    }

    pub(crate) fn toggle_checked(
        &mut self,
        project_id: &str,
        identifier: &str,
        cx: &mut Context<Self>,
    ) {
        let set = self.checked.entry(project_id.to_string()).or_default();
        if !set.remove(identifier) {
            set.insert(identifier.to_string());
        }
        cx.notify();
    }

    // ----------------------------------------------------------------------
    // Actions
    // ----------------------------------------------------------------------

    fn set_notice(&mut self, text: impl Into<String>, error: bool) {
        self.notice = Some(PanelNotice {
            text: text.into(),
            error,
        });
    }

    fn selected_item(&self) -> Option<String> {
        match &self.selection {
            OutlineSelection::Item { space_id, item_id } if *space_id == self.space_id => self
                .snapshot()
                .and_then(|snapshot| snapshot.item(item_id))
                .filter(|item| item.archived_at.is_none())
                .map(|item| item.id.clone()),
            _ => None,
        }
    }

    /// Create a work item for `issue` under the selected item (or ask).
    pub(crate) fn create_issue(&mut self, issue: LinearIssueSummary, cx: &mut Context<Self>) {
        self.create_with_target(vec![issue], None, cx);
    }

    /// Create one work item per checked issue of `project_id`.
    pub(crate) fn create_selected(&mut self, project_id: &str, cx: &mut Context<Self>) {
        let checked = self.checked.get(project_id).cloned().unwrap_or_default();
        let issues: Vec<LinearIssueSummary> = self
            .project_issues
            .get(project_id)
            .and_then(Load::ready)
            .map(|issues| {
                issues
                    .iter()
                    .filter(|issue| checked.contains(&issue.identifier))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if issues.is_empty() {
            self.set_notice("Select issues of the project first", false);
            cx.notify();
            return;
        }
        self.create_with_target(issues, Some(project_id.to_string()), cx);
    }

    fn create_with_target(
        &mut self,
        issues: Vec<LinearIssueSummary>,
        project_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(snapshot) = self.snapshot().cloned() else {
            return;
        };
        match parent_target(&self.selection, &self.space_id, &snapshot) {
            ParentTarget::Resolved(parent) => {
                let space_id = self.space_id.clone();
                self.create_under(space_id, parent, issues, project_id, cx);
            }
            ParentTarget::Ask => {
                self.picker = Some(ParentPicker { issues, project_id });
                cx.notify();
            }
        }
    }

    /// The parent picker's choice.
    pub(crate) fn choose_parent(&mut self, parent: Option<String>, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.take() else {
            return;
        };
        let space_id = self.space_id.clone();
        self.create_under(space_id, parent, picker.issues, picker.project_id, cx);
    }

    /// `createWorkItemFromLinear` for each issue, in order, under `parent`
    /// (placement Last). Also the Outline drop's entry point.
    pub(crate) fn create_under(
        &mut self,
        space_id: String,
        parent: Option<String>,
        issues: Vec<LinearIssueSummary>,
        project_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let mut params = Vec::new();
        for issue in issues {
            let busy = format!("create:{}", issue.identifier);
            if !self.busy.insert(busy.clone()) {
                continue;
            }
            let inputs = format!("{space_id}\u{1f}{parent:?}\u{1f}{}", issue.identifier);
            let operation_id = self
                .op_slots
                .entry(format!("create:{}", issue.id))
                .or_default()
                .id_for(&inputs);
            params.push((
                busy,
                issue.identifier.clone(),
                CreateWorkItemFromLinearParams {
                    space_id: space_id.clone(),
                    parent_work_item_id: parent.clone(),
                    placement: WorkOutlinePlacement::Last,
                    issue: LinearIssueLink::from(&issue),
                    operation_id,
                },
            ));
        }
        if params.is_empty() {
            return;
        }
        self.notice = None;
        let actions = self.state.read(cx).work_outline_actions();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let total = params.len();
            let mut created = Vec::new();
            let mut failure: Option<(String, LinearUiError)> = None;
            for (busy, identifier, params) in params {
                let operation_id = params.operation_id.clone();
                let result = actions.create_work_item_from_linear(params).await;
                let done = this.update(cx, |this, cx| {
                    this.busy.remove(&busy);
                    cx.notify();
                });
                if done.is_err() {
                    return;
                }
                match result {
                    Ok(reply) => created.push(reply.work_item_id.unwrap_or_else(|| {
                        zeron_proto::work_outline::derive_id(&operation_id, "item")
                    })),
                    Err(error) => {
                        failure.get_or_insert((identifier, error));
                    }
                }
            }
            this.update(cx, |this, cx| {
                match &failure {
                    Some((identifier, error)) => this.set_notice(
                        format!("Couldn\u{2019}t create {identifier}: {}", error.user_text()),
                        true,
                    ),
                    None => {
                        this.set_notice(
                            if total == 1 {
                                "Created 1 work item".to_string()
                            } else {
                                format!("Created {total} work items")
                            },
                            false,
                        );
                        if let Some(project_id) = &project_id {
                            this.checked.remove(project_id);
                        }
                    }
                }
                if let Some(item_id) = created.last() {
                    cx.emit(LinearPanelEvent::Created {
                        space_id: space_id.clone(),
                        item_id: item_id.clone(),
                        parent_id: parent.clone(),
                    });
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Link `issue` to the Outline's selected item.
    pub(crate) fn link_issue(&mut self, issue: LinearIssueSummary, cx: &mut Context<Self>) {
        let Some(item_id) = self.selected_item() else {
            self.set_notice("Select a work item in the Outline first", false);
            cx.notify();
            return;
        };
        let busy = format!("link:{}", issue.identifier);
        if !self.busy.insert(busy.clone()) {
            return;
        }
        let operation_id = self
            .op_slots
            .entry(busy.clone())
            .or_default()
            .id_for(&item_id);
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .link_linear_issue(CreateWorkItemExternalReferenceParams {
                work_item_id: item_id,
                provider: LINEAR_PROVIDER.into(),
                reference_type: LINEAR_ISSUE_REFERENCE.into(),
                external_id: issue.identifier.clone(),
                url: issue.url.clone(),
                cached_title: Some(issue.title.clone()),
                operation_id,
            });
        self.finish_link(busy, issue.identifier.clone(), task, cx);
    }

    /// Link `project` to the Outline's selected item.
    pub(crate) fn link_project(&mut self, project: LinearProjectSummary, cx: &mut Context<Self>) {
        let Some(item_id) = self.selected_item() else {
            self.set_notice("Select a work item in the Outline first", false);
            cx.notify();
            return;
        };
        let busy = format!("link-project:{}", project.id);
        if !self.busy.insert(busy.clone()) {
            return;
        }
        let operation_id = self
            .op_slots
            .entry(busy.clone())
            .or_default()
            .id_for(&item_id);
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .link_linear_project(LinkLinearProjectParams {
                work_item_id: item_id,
                project: project.to_ref(),
                operation_id,
            });
        self.finish_link(busy, project.name.clone(), task, cx);
    }

    fn finish_link(
        &mut self,
        busy: String,
        label: String,
        task: super::client::LinearTask<zeron_proto::WorkOutlineMutateReply>,
        cx: &mut Context<Self>,
    ) {
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.busy.remove(&busy);
                match result {
                    Ok(_) => this.set_notice(format!("Linked {label}"), false),
                    Err(error) => this.set_notice(
                        format!("Couldn\u{2019}t link {label}: {}", error.user_text()),
                        true,
                    ),
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

    fn small_button(
        id: impl Into<gpui::ElementId>,
        label: &'static str,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        div()
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
            .child(label)
    }

    fn section_title(label: String, theme: &Theme) -> gpui::Div {
        div()
            .pt(px(6.0))
            .text_size(crate::typography::ui_rems(11.0))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme.text_muted)
            .child(SharedString::from(label))
    }

    fn quiet(text: impl Into<SharedString>, theme: &Theme) -> gpui::Div {
        div()
            .py(px(4.0))
            .text_size(crate::typography::ui_rems(11.5))
            .text_color(theme.text_faint)
            .child(text.into())
    }

    /// The account-wide blocker (no key, rejected key, no Linear on this
    /// host) both lists share, if any.
    fn blocker(&self) -> Option<LinearUiError> {
        let error = self.my_issues.error().or(self.my_projects.error())?;
        let blocking = error.not_available()
            || error.is(LinearErrorCode::MissingKey)
            || error.is(LinearErrorCode::Unauthorized);
        blocking.then(|| error.clone())
    }

    fn render_blocker(
        &self,
        error: &LinearUiError,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, button): (String, Option<&'static str>) = if error.not_available() {
            (error.user_text(), None)
        } else if error.is(LinearErrorCode::MissingKey) {
            (
                "Connect Linear to see your issues and projects.".into(),
                Some("Connect Linear\u{2026}"),
            )
        } else {
            (
                "Linear rejected the API key. Reconnect to continue.".into(),
                Some("Reconnect\u{2026}"),
            )
        };
        div()
            .debug_selector(|| "linear-panel-blocker".into())
            .flex()
            .flex_col()
            .gap(px(6.0))
            .py(px(8.0))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(title)),
            )
            .when_some(button, |el, label| {
                el.child(
                    div().child(
                        Self::small_button("linear-panel-connect", label, theme)
                            .debug_selector(|| "linear-panel-connect".into())
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(LinearPanelEvent::OpenSettings)),
                            ),
                    ),
                )
            })
            .into_any_element()
    }

    fn render_list_error(
        &self,
        id: &'static str,
        error: &LinearUiError,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.warning)
                    .child(SharedString::from(error.user_text())),
            )
            .child(
                Self::small_button(id, "Retry", theme)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .into_any_element()
    }

    fn linked_marker(
        &self,
        id: String,
        item_id: Option<&str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let item_id = item_id?.to_string();
        let space_id = self.space_id.clone();
        let selector = id.clone();
        Some(
            div()
                .id(SharedString::from(id))
                .debug_selector(move || selector.clone())
                .flex_none()
                .flex()
                .items_center()
                .gap(px(3.0))
                .px(px(4.0))
                .rounded(px(3.0))
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(10.0))
                .text_color(theme.accent)
                .hover(|el| el.bg(theme.element_hover))
                .child("Linked \u{00b7} Reveal")
                .tooltip(|_, cx| {
                    cx.new(|_| {
                        TextTooltip(
                            "Already linked in this project: reveal it in the Outline".into(),
                        )
                    })
                    .into()
                })
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.stop_propagation();
                    cx.emit(LinearPanelEvent::Reveal {
                        space_id: space_id.clone(),
                        item_id: item_id.clone(),
                    });
                }))
                .into_any_element(),
        )
    }

    fn render_issue_row(
        &self,
        issue: &LinearIssueSummary,
        linked: &LinkedIndex,
        check: Option<(String, bool)>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let identifier = issue.identifier.clone();
        let state_color = super::hex_color(&issue.state.color, theme.text_muted);
        let mut meta = vec![issue.state.name.clone()];
        meta.extend(super::priority_text(issue.priority, &issue.priority_label));
        meta.extend(issue.assignee.as_ref().map(super::user_name));
        meta.extend(issue.project.as_ref().map(|p| p.name.clone()));
        let row_selector = format!("linear-issue-row-{identifier}");
        let creating = self.busy.contains(&format!("create:{identifier}"));
        let linking = self.busy.contains(&format!("link:{identifier}"));
        let marker = self.linked_marker(
            format!("linear-issue-linked-{identifier}"),
            linked.issue_item(&identifier),
            theme,
            cx,
        );
        let drag = OutlineDrag::LinearIssue(LinearIssueDrag {
            space_id: self.space_id.clone(),
            issue: Box::new(issue.clone()),
        });
        let create_issue = issue.clone();
        let link_issue = issue.clone();
        let checkbox = check.map(|(project_id, checked)| {
            let id = identifier.clone();
            div()
                .id(SharedString::from(format!(
                    "linear-issue-check-{identifier}"
                )))
                .debug_selector({
                    let selector = format!("linear-issue-check-{identifier}");
                    move || selector.clone()
                })
                .flex_none()
                .size(px(13.0))
                .rounded(px(3.0))
                .border_1()
                .border_color(if checked { theme.accent } else { theme.border })
                .when(checked, |el| el.bg(theme.accent))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.toggle_checked(&project_id, &id, cx)
                }))
        });
        div()
            .id(SharedString::from(row_selector.clone()))
            .debug_selector(move || row_selector.clone())
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(4.0))
            .py(px(3.0))
            .rounded(px(4.0))
            .hover(|el| el.bg(theme.element_hover.opacity(0.6)))
            .cursor_grab()
            .on_drag(drag, |drag, _, _, cx| {
                cx.stop_propagation();
                let title = match drag {
                    OutlineDrag::LinearIssue(drag) => zeron_proto::linear::linear_item_title(
                        &drag.issue.identifier,
                        &drag.issue.title,
                    ),
                    _ => String::new(),
                };
                cx.new(|_| LinearDragGhost {
                    title: title.into(),
                })
            })
            .children(checkbox)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .text_size(crate::typography::ui_rems(12.0))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(identifier.clone())),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.text)
                                    .child(SharedString::from(issue.title.clone())),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.0))
                            .text_size(crate::typography::ui_rems(10.5))
                            .text_color(theme.text_faint)
                            .child(div().flex_none().size(px(7.0)).rounded_full().bg(state_color))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .child(SharedString::from(meta.join(" \u{00b7} "))),
                            ),
                    ),
            )
            .children(marker)
            .child(
                Self::small_button(
                    SharedString::from(format!("linear-issue-create-{identifier}")),
                    if creating { "Creating\u{2026}" } else { "Create" },
                    theme,
                )
                .debug_selector({
                    let selector = format!("linear-issue-create-{identifier}");
                    move || selector.clone()
                })
                .tooltip(|_, cx| {
                    cx.new(|_| {
                        TextTooltip(
                            "Create a work item under the selected item (drag onto an item to choose)"
                                .into(),
                        )
                    })
                    .into()
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.create_issue(create_issue.clone(), cx)
                })),
            )
            .child(
                Self::small_button(
                    SharedString::from(format!("linear-issue-link-{identifier}")),
                    if linking { "Linking\u{2026}" } else { "Link" },
                    theme,
                )
                .debug_selector({
                    let selector = format!("linear-issue-link-{identifier}");
                    move || selector.clone()
                })
                .tooltip(|_, cx| {
                    cx.new(|_| TextTooltip("Link to the selected work item".into()))
                        .into()
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.link_issue(link_issue.clone(), cx)
                })),
            )
            .into_any_element()
    }

    fn render_project(
        &self,
        project: &LinearProjectSummary,
        query: &str,
        linked: &LinkedIndex,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let open = self.expanded.contains(&project.id);
        let issues = self.project_issues.get(&project.id);
        let matching_issues: Vec<&LinearIssueSummary> = issues
            .and_then(Load::ready)
            .map(|issues| {
                issues
                    .iter()
                    .filter(|issue| issue_matches(query, issue))
                    .collect()
            })
            .unwrap_or_default();
        if !project_matches(query, project) && matching_issues.is_empty() {
            return None;
        }
        let id = project.id.clone();
        let mut meta: Vec<String> = Vec::new();
        meta.extend(project.state.clone());
        meta.extend(project.lead.as_ref().map(super::user_name));
        if let Some(count) = project.open_issue_count {
            meta.push(format!("{count} open"));
        }
        let marker = self.linked_marker(
            format!("linear-project-linked-{id}"),
            linked.project_item(&project.id, &project.url),
            theme,
            cx,
        );
        let toggle_id = id.clone();
        let link_project = project.clone();
        let linking = self.busy.contains(&format!("link-project:{id}"));
        let row_selector = format!("linear-project-row-{id}");
        let header = div()
            .id(SharedString::from(row_selector.clone()))
            .debug_selector(move || row_selector.clone())
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(4.0))
            .py(px(3.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .hover(|el| el.bg(theme.element_hover.opacity(0.6)))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_project(&toggle_id, cx)))
            .child(
                crate::icons::icon(if open {
                    crate::icons::ALT_ARROW_DOWN
                } else {
                    crate::icons::ALT_ARROW_RIGHT
                })
                .size(px(10.0))
                .text_color(theme.text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .child(
                        div()
                            .truncate()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text)
                            .child(SharedString::from(project.name.clone())),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(crate::typography::ui_rems(10.5))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(meta.join(" \u{00b7} "))),
                    ),
            )
            .children(marker)
            .child(
                Self::small_button(
                    SharedString::from(format!("linear-project-link-{id}")),
                    if linking { "Linking\u{2026}" } else { "Link" },
                    theme,
                )
                .debug_selector({
                    let selector = format!("linear-project-link-{id}");
                    move || selector.clone()
                })
                .tooltip(|_, cx| {
                    cx.new(|_| TextTooltip("Link this project to the selected work item".into()))
                        .into()
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.link_project(link_project.clone(), cx)
                })),
            );
        let mut column = div().flex().flex_col().child(header);
        if open {
            let mut body = div().flex().flex_col().pl(px(14.0));
            match issues {
                None | Some(Load::Loading) => {
                    body = body.child(Self::quiet("Loading\u{2026}", theme))
                }
                Some(Load::Failed(error)) => {
                    body = body.child(Self::quiet(error.user_text(), theme))
                }
                Some(Load::Ready(all)) => {
                    if all.is_empty() {
                        body = body.child(Self::quiet("No open issues", theme));
                    }
                    let checked = self.checked.get(&id).cloned().unwrap_or_default();
                    for issue in &matching_issues {
                        let is_checked = checked.contains(&issue.identifier);
                        body = body.child(self.render_issue_row(
                            issue,
                            linked,
                            Some((id.clone(), is_checked)),
                            theme,
                            cx,
                        ));
                    }
                    if !checked.is_empty() {
                        let create_id = id.clone();
                        let label: &'static str = if checked.len() == 1 {
                            "Create node for selected"
                        } else {
                            "Create nodes for selected"
                        };
                        body = body.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .py(px(4.0))
                                .child(
                                    Self::small_button(
                                        SharedString::from(format!(
                                            "linear-project-create-selected-{id}"
                                        )),
                                        label,
                                        theme,
                                    )
                                    .debug_selector({
                                        let selector =
                                            format!("linear-project-create-selected-{id}");
                                        move || selector.clone()
                                    })
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| this.create_selected(&create_id, cx),
                                    )),
                                )
                                .child(
                                    div()
                                        .text_size(crate::typography::ui_rems(10.5))
                                        .text_color(theme.text_faint)
                                        .child(SharedString::from(format!(
                                            "{} selected",
                                            checked.len()
                                        ))),
                                ),
                        );
                    }
                }
            }
            column = column.child(body);
        }
        Some(column.into_any_element())
    }

    fn render_picker(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker = self.picker.as_ref()?;
        let snapshot = self.snapshot()?.clone();
        let count = picker.issues.len();
        let rows = model::destinations(&snapshot, None)
            .into_iter()
            .enumerate()
            .map(|(ix, dest)| {
                let label = match &dest.item_id {
                    Some(id) => model::path_label(
                        model::item_path(&snapshot, id)
                            .iter()
                            .map(|item| item.title.as_str()),
                    ),
                    None => "Top level".to_string(),
                };
                let parent = dest.item_id.clone();
                let selector =
                    format!("linear-parent-{}", dest.item_id.as_deref().unwrap_or("top"));
                div()
                    .id(("linear-parent", ix))
                    .debug_selector(move || selector.clone())
                    .px(px(6.0))
                    .py(px(3.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .truncate()
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.text)
                    .hover(|el| el.bg(theme.element_hover))
                    .child(SharedString::from(label))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.choose_parent(parent.clone(), cx)),
                    )
            });
        Some(
            div()
                .debug_selector(|| "linear-parent-picker".into())
                .flex()
                .flex_col()
                .gap(px(2.0))
                .p(px(6.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(theme.accent.opacity(0.6))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .text_size(crate::typography::ui_rems(11.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme.text_muted)
                                .child(SharedString::from(if count == 1 {
                                    "Create under\u{2026}".to_string()
                                } else {
                                    format!("Create {count} items under\u{2026}")
                                })),
                        )
                        .child(
                            Self::small_button("linear-parent-cancel", "Cancel", theme).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.picker = None;
                                    cx.notify();
                                }),
                            ),
                        ),
                )
                .child(
                    div()
                        .id("linear-parent-list")
                        .max_h(px(220.0))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .children(rows),
                )
                .into_any_element(),
        )
    }
}

/// The ghost that follows the pointer while dragging an issue row.
struct LinearDragGhost {
    title: SharedString,
}

impl Render for LinearDragGhost {
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

impl Render for LinearPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let query = self.query(cx);
        let linked = self
            .snapshot()
            .map(|snapshot| LinkedIndex::from_snapshot(snapshot))
            .unwrap_or_default();
        let label = self
            .spaces
            .iter()
            .find(|space| space.id == self.space_id)
            .map(|space| space.label.clone())
            .unwrap_or_default();
        let multi = self.spaces.len() > 1;
        let next_space = multi.then(|| {
            let ix = self
                .spaces
                .iter()
                .position(|space| space.id == self.space_id)
                .unwrap_or(0);
            self.spaces[(ix + 1) % self.spaces.len()].id.clone()
        });
        let header = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(14.0))
            .pt(px(10.0))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(13.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child("Linear"),
            )
            .child(
                div()
                    .id("linear-panel-space")
                    .debug_selector(|| "linear-panel-space".into())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .when(multi, |el| {
                        el.cursor_pointer().hover(|el| el.text_color(theme.text))
                    })
                    .child(SharedString::from(if multi {
                        format!("{label} \u{25be}")
                    } else {
                        label
                    }))
                    .when_some(next_space, |el, next| {
                        el.on_click(
                            cx.listener(move |this, _, _, cx| this.set_space(next.clone(), cx)),
                        )
                    }),
            )
            .child(
                div()
                    .id("linear-panel-refresh")
                    .debug_selector(|| "linear-panel-refresh".into())
                    .flex_none()
                    .size(px(22.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(|el| el.bg(theme.element_hover))
                    .child(
                        crate::icons::icon(crate::icons::REFRESH)
                            .size(px(12.0))
                            .text_color(theme.text_muted),
                    )
                    .tooltip(|_, cx| cx.new(|_| TextTooltip("Refresh from Linear".into())).into())
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .child(
                div()
                    .id("linear-panel-close")
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
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(LinearPanelEvent::Close))),
            );
        let search = div()
            .mx(px(14.0))
            .mt(px(8.0))
            .px(px(6.0))
            .py(px(3.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(theme.border)
            .child(self.search.clone());
        let mut body = div()
            .id("linear-panel-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .px(px(14.0))
            .py(px(8.0));
        if let Some(notice) = self.notice.clone() {
            body = body.child(
                div()
                    .debug_selector(|| "linear-panel-notice".into())
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(if notice.error {
                        theme.warning
                    } else {
                        theme.text_muted
                    })
                    .child(SharedString::from(notice.text)),
            );
        }
        if let Some(picker) = self.render_picker(&theme, cx) {
            body = body.child(picker);
        }
        if let Some(error) = self.blocker() {
            body = body.child(self.render_blocker(&error, &theme, cx));
        } else {
            // My issues.
            let issues: Vec<LinearIssueSummary> = self
                .my_issues
                .ready()
                .map(|issues| {
                    issues
                        .iter()
                        .filter(|issue| issue_matches(&query, issue))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            body = body.child(Self::section_title(
                match &self.my_issues {
                    Load::Ready(_) => format!("My issues ({})", issues.len()),
                    _ => "My issues".to_string(),
                },
                &theme,
            ));
            match &self.my_issues {
                Load::Loading => body = body.child(Self::quiet("Loading\u{2026}", &theme)),
                Load::Failed(error) => {
                    let error = error.clone();
                    body = body.child(self.render_list_error(
                        "linear-issues-retry",
                        &error,
                        &theme,
                        cx,
                    ))
                }
                Load::Ready(all) => {
                    if all.is_empty() {
                        body = body.child(Self::quiet("No open issues assigned to you", &theme));
                    } else if issues.is_empty() {
                        body = body.child(Self::quiet("No matching issues", &theme));
                    }
                    for issue in &issues {
                        body = body.child(self.render_issue_row(issue, &linked, None, &theme, cx));
                    }
                }
            }
            // My projects.
            body = body.child(Self::section_title("My projects".to_string(), &theme));
            match &self.my_projects {
                Load::Loading => body = body.child(Self::quiet("Loading\u{2026}", &theme)),
                Load::Failed(error) => {
                    let error = error.clone();
                    body = body.child(self.render_list_error(
                        "linear-projects-retry",
                        &error,
                        &theme,
                        cx,
                    ))
                }
                Load::Ready(all) => {
                    let all = all.clone();
                    if all.is_empty() {
                        body = body.child(Self::quiet("No active projects", &theme));
                    }
                    let mut shown = 0;
                    for project in &all {
                        if let Some(row) = self.render_project(project, &query, &linked, &theme, cx)
                        {
                            shown += 1;
                            body = body.child(row);
                        }
                    }
                    if !all.is_empty() && shown == 0 {
                        body = body.child(Self::quiet("No matching projects", &theme));
                    }
                }
            }
        }
        div()
            .id("linear-panel")
            .debug_selector(|| "linear-panel".into())
            .track_focus(&self.focus)
            .flex_none()
            .w(px(LINEAR_PANEL_W))
            .h_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    if this.picker.take().is_none() {
                        cx.emit(LinearPanelEvent::Close);
                    }
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(header)
            .child(search)
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_outline::store::fixture_ids::*;

    #[test]
    fn parent_target_uses_the_selected_item_of_this_space_or_asks() {
        let snapshot = crate::work_outline::store::fixture_snapshot("s1");
        let item = |space: &str, id: &str| OutlineSelection::Item {
            space_id: space.into(),
            item_id: id.into(),
        };
        assert_eq!(
            parent_target(&item("s1", API), "s1", &snapshot),
            ParentTarget::Resolved(Some(API.into()))
        );
        assert_eq!(
            parent_target(&OutlineSelection::Space("s1".into()), "s1", &snapshot),
            ParentTarget::Resolved(None)
        );
        assert_eq!(
            parent_target(&item("s2", API), "s1", &snapshot),
            ParentTarget::Ask
        );
        assert_eq!(
            parent_target(&OutlineSelection::None, "s1", &snapshot),
            ParentTarget::Ask
        );
        assert_eq!(
            parent_target(&OutlineSelection::Other, "s1", &snapshot),
            ParentTarget::Ask
        );
    }

    #[test]
    fn search_matches_issue_and_project_fields() {
        let issue = super::super::fixture::issue("ENG-7", "Fix login");
        assert!(issue_matches("", &issue));
        assert!(issue_matches("eng-7", &issue));
        assert!(issue_matches("login progress", &issue));
        assert!(issue_matches("mikey", &issue));
        assert!(issue_matches("launch", &issue), "project name");
        assert!(!issue_matches("billing", &issue));
        let project = super::super::fixture::project("p-1", "Billing");
        assert!(project_matches("bill tod", &project));
        assert!(!project_matches("launch", &project));
    }
}
