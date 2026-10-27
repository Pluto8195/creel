//! "Add issues from <project>…" on a project-linked work item (its
//! right-click menu and its Details Linear section; Linear-enabled Spaces
//! only). A compact picker over the Overview, independent of the Linear
//! panel: the project's open issues (`LinearProjectIssues`) with the panel's
//! issue rows and checkboxes, a search box, Select all (issues not added
//! yet), and Create: one `createWorkItemFromLinear` per checked issue under
//! the item (placement Last, one stable operation id per issue), then select
//! and reveal the first new node. An issue some live item of the Space
//! already links shows checked and disabled, with Reveal.
//!
//! A slug-keyed project link (a pasted URL) asks Linear by the real project
//! id once "My projects" resolves it (`project_links::resolved_project_id`).
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Subscription,
    Window, div, prelude::*, px,
};
use zeron_proto::linear::{LinearIssueLink, LinearIssueSummary};
use zeron_proto::{
    CreateWorkItemFromLinearParams, WorkItemExternalReferenceView, WorkOutlinePlacement,
    WorkOutlineSnapshot,
};

use super::LinkedIndex;
use super::client::{LinearUiError, ListRead};
use super::panel::{
    LinearPanel, LinearPanelEvent, Load, issue_checkbox, issue_matches, issue_summary,
};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::state::AppState;
use crate::theme::Theme;
use crate::work_outline::actions::OperationSlot;
use crate::work_outline::chips::TextTooltip;

/// Width of the picker card.
pub(crate) const PICKER_W: f32 = 440.0;

/// Whether `id` reads as a Linear UUID (a project id) rather than a URL slug.
fn looks_like_uuid(id: &str) -> bool {
    let parts: Vec<&str> = id.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(part, len)| part.len() == len && part.chars().all(|c| c.is_ascii_hexdigit()))
}

pub(crate) struct ProjectIssuePicker {
    state: Entity<AppState>,
    focus: FocusHandle,
    search: Entity<ComposerInput>,
    /// Put the cursor in the search on the next render.
    pending_focus: bool,
    pub(crate) space_id: String,
    pub(crate) item_id: String,
    reference: WorkItemExternalReferenceView,
    pub(crate) project_name: String,
    snapshot: Option<Arc<WorkOutlineSnapshot>>,
    pub(crate) issues: Load<Vec<LinearIssueSummary>>,
    pub(crate) checked: BTreeSet<String>,
    op_slots: HashMap<String, OperationSlot>,
    pub(crate) creating: bool,
    pub(crate) error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<LinearPanelEvent> for ProjectIssuePicker {}

impl Focusable for ProjectIssuePicker {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ProjectIssuePicker {
    pub(crate) fn new(
        state: Entity<AppState>,
        snapshot: Arc<WorkOutlineSnapshot>,
        item_id: String,
        reference: WorkItemExternalReferenceView,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            ComposerInput::with_context(
                "Search this project\u{2019}s issues",
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
        let project_name = reference
            .cached_title
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "this project".into());
        Self {
            state,
            focus: cx.focus_handle(),
            search,
            pending_focus: true,
            space_id: snapshot.space_id.clone(),
            item_id,
            reference,
            project_name,
            snapshot: Some(snapshot),
            issues: Load::Loading,
            checked: BTreeSet::new(),
            op_slots: HashMap::new(),
            creating: false,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    /// Fed by the Overview every frame.
    pub(crate) fn set_snapshot(&mut self, snapshot: Option<Arc<WorkOutlineSnapshot>>) {
        self.snapshot = snapshot;
    }

    /// Whether the picker still applies: Linear on, the item live and still
    /// linked to a project.
    pub(crate) fn still_valid(&self) -> bool {
        self.snapshot.as_deref().is_some_and(|snapshot| {
            super::space_enabled(Some(snapshot))
                && snapshot
                    .item(&self.item_id)
                    .is_some_and(|item| item.archived_at.is_none())
                && snapshot.linear_project_reference(&self.item_id).is_some()
        })
    }

    fn item_title(&self) -> String {
        self.snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.item(&self.item_id))
            .map(|item| item.title.clone())
            .unwrap_or_default()
    }

    pub(crate) fn query(&self, cx: &gpui::App) -> String {
        self.search.read(cx).text().trim().to_lowercase()
    }

    #[cfg(test)]
    pub(crate) fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        self.search
            .update(cx, |input, cx| input.set_text(query, cx));
    }

    /// Fetch the project's open issues (resolving a slug link to the real
    /// project id through My projects first).
    pub(crate) fn load(&mut self, cx: &mut Context<Self>) {
        let actions = self.state.read(cx).work_outline_actions();
        let cached = self.state.read(cx).linear.projects.clone();
        let lookup = (cached.is_none() && !looks_like_uuid(&self.reference.external_id))
            .then(|| actions.linear_my_projects(ListRead::Cached));
        let reference = self.reference.clone();
        self.issues = Load::Loading;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let mut projects = cached;
            if let Some(lookup) = lookup
                && let Ok(found) = lookup.await
            {
                projects = Some(found);
            }
            let project_id =
                super::project_links::resolved_project_id(&reference, projects.as_deref());
            let fetched = this
                .update(cx, |this, cx| {
                    if let Some(projects) = projects.clone() {
                        this.state.update(cx, |state, cx| {
                            if state.linear.projects.is_none() {
                                state.linear.projects = Some(projects);
                                cx.notify();
                            }
                        });
                    }
                    actions.linear_project_issues(&project_id, ListRead::Cached)
                })
                .ok();
            let Some(task) = fetched else {
                return;
            };
            let result = task.await;
            this.update(cx, |this, cx| {
                if let Ok(issues) = &result {
                    this.state
                        .update(cx, |state, _| state.linear.remember_issues(issues.iter()));
                }
                this.issues = match result {
                    Ok(issues) => Load::Ready(issues),
                    Err(error) => Load::Failed(error),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn linked(&self) -> LinkedIndex {
        self.snapshot
            .as_deref()
            .map(LinkedIndex::from_snapshot)
            .unwrap_or_default()
    }

    /// The issues the search shows, in Linear's order.
    pub(crate) fn visible(&self, cx: &gpui::App) -> Vec<LinearIssueSummary> {
        let query = self.query(cx);
        self.issues
            .ready()
            .map(|issues| {
                issues
                    .iter()
                    .filter(|issue| issue_matches(&query, issue))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn toggle(&mut self, identifier: &str, cx: &mut Context<Self>) {
        if self.linked().issue_item(identifier).is_some() {
            return;
        }
        if !self.checked.remove(identifier) {
            self.checked.insert(identifier.to_string());
        }
        cx.notify();
    }

    /// Check every shown issue that no item links yet.
    pub(crate) fn select_all(&mut self, cx: &mut Context<Self>) {
        let linked = self.linked();
        for issue in self.visible(cx) {
            if linked.issue_item(&issue.identifier).is_none() {
                self.checked.insert(issue.identifier);
            }
        }
        cx.notify();
    }

    /// The checked issues not added yet, in list order.
    fn to_create(&self) -> Vec<LinearIssueSummary> {
        let linked = self.linked();
        self.issues
            .ready()
            .map(|issues| {
                issues
                    .iter()
                    .filter(|issue| {
                        self.checked.contains(&issue.identifier)
                            && linked.issue_item(&issue.identifier).is_none()
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// One `createWorkItemFromLinear` per checked issue under the item, in
    /// order; then select and reveal the first new node and close (a
    /// failure keeps the picker open with the rest still checked).
    pub(crate) fn create(&mut self, cx: &mut Context<Self>) {
        if self.creating {
            return;
        }
        let issues = self.to_create();
        if issues.is_empty() {
            self.error = Some("Select issues to add first".into());
            cx.notify();
            return;
        }
        let space_id = self.space_id.clone();
        let parent = self.item_id.clone();
        let params: Vec<(String, CreateWorkItemFromLinearParams)> = issues
            .iter()
            .map(|issue| {
                let inputs = format!("{space_id}\u{1f}{parent}\u{1f}{}", issue.identifier);
                let operation_id = self
                    .op_slots
                    .entry(issue.id.clone())
                    .or_default()
                    .id_for(&inputs);
                (
                    issue.identifier.clone(),
                    CreateWorkItemFromLinearParams {
                        space_id: space_id.clone(),
                        parent_work_item_id: Some(parent.clone()),
                        placement: WorkOutlinePlacement::Last,
                        issue: LinearIssueLink::from(issue),
                        operation_id,
                    },
                )
            })
            .collect();
        self.creating = true;
        self.error = None;
        let actions = self.state.read(cx).work_outline_actions();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let mut created: Vec<(String, String)> = Vec::new();
            let mut failure: Option<(String, LinearUiError)> = None;
            for (identifier, params) in params {
                let operation_id = params.operation_id.clone();
                match actions.create_work_item_from_linear(params).await {
                    Ok(reply) => created.push((
                        identifier,
                        reply.work_item_id.unwrap_or_else(|| {
                            zeron_proto::work_outline::derive_id(&operation_id, "item")
                        }),
                    )),
                    Err(error) => {
                        failure.get_or_insert((identifier, error));
                    }
                }
            }
            this.update(cx, |this, cx| {
                this.creating = false;
                for (identifier, _) in &created {
                    this.checked.remove(identifier);
                }
                if let Some((_, item_id)) = created.first() {
                    cx.emit(LinearPanelEvent::Created {
                        space_id: this.space_id.clone(),
                        item_id: item_id.clone(),
                        parent_id: Some(this.item_id.clone()),
                    });
                }
                match failure {
                    Some((identifier, error)) => {
                        this.error = Some(format!(
                            "Couldn\u{2019}t add {identifier}: {}",
                            error.user_text()
                        ))
                    }
                    None => cx.emit(LinearPanelEvent::Close),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_row(
        &self,
        issue: &LinearIssueSummary,
        linked: &LinkedIndex,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let identifier = issue.identifier.clone();
        let added = linked.issue_item(&identifier).map(str::to_string);
        let checked = added.is_some() || self.checked.contains(&identifier);
        let toggle_id = identifier.clone();
        let checkbox = issue_checkbox(
            format!("linear-add-issues-check-{identifier}"),
            checked,
            added.is_some(),
            theme,
        )
        .when(added.is_none(), |el| {
            el.on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.toggle(&toggle_id, cx)
            }))
        });
        let reveal = added.map(|item_id| {
            let space_id = self.space_id.clone();
            let selector = format!("linear-add-issues-reveal-{identifier}");
            LinearPanel::small_button(
                SharedString::from(selector.clone()),
                "Added \u{00b7} Reveal",
                theme,
            )
            .debug_selector(move || selector.clone())
            .text_color(theme.accent)
            .tooltip(|_, cx| {
                cx.new(|_| {
                    TextTooltip(
                        "Already a work item in this project: reveal it in the Outline".into(),
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
        });
        let row_selector = format!("linear-add-issues-row-{identifier}");
        let click_id = identifier.clone();
        let clickable = reveal.is_none();
        div()
            .id(SharedString::from(row_selector.clone()))
            .debug_selector(move || row_selector.clone())
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(4.0))
            .py(px(3.0))
            .rounded(px(4.0))
            .when(clickable, |el| {
                el.cursor_pointer()
                    .hover(|el| el.bg(theme.element_hover.opacity(0.6)))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(&click_id, cx)))
            })
            .child(checkbox)
            .child(issue_summary(issue, theme).min_w(px(90.0)))
            .children(reveal)
            .into_any_element()
    }
}

impl Render for ProjectIssuePicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        if std::mem::take(&mut self.pending_focus) {
            let handle = self.search.focus_handle(cx);
            window.focus(&handle, cx);
        }
        let linked = self.linked();
        let visible = self.visible(cx);
        let unadded = visible
            .iter()
            .filter(|issue| linked.issue_item(&issue.identifier).is_none())
            .count();
        let count = self.to_create().len();
        let item_title = self.item_title();
        let mut list = div()
            .id("linear-add-issues-list")
            .max_h(px(320.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(1.0));
        let quiet = |text: String| {
            div()
                .py(px(6.0))
                .text_size(crate::typography::ui_rems(11.5))
                .text_color(theme.text_faint)
                .child(SharedString::from(text))
        };
        match &self.issues {
            Load::Loading => list = list.child(quiet("Loading\u{2026}".into())),
            Load::Failed(error) => list = list.child(quiet(error.user_text())),
            Load::Ready(all) => {
                if all.is_empty() {
                    list = list.child(quiet("No open issues in this project".into()));
                } else if visible.is_empty() {
                    list = list.child(quiet("No matching issues".into()));
                }
                for issue in &visible {
                    list = list.child(self.render_row(issue, &linked, &theme, cx));
                }
            }
        }
        let create_label: SharedString = if self.creating {
            "Adding\u{2026}".into()
        } else if count == 0 {
            "Create".into()
        } else if count == 1 {
            "Create 1 work item".into()
        } else {
            format!("Create {count} work items").into()
        };
        let create_tip: SharedString = format!(
            "One work item per checked issue, as the last children of \u{201c}{item_title}\u{201d}"
        )
        .into();
        div()
            .id("linear-add-issues")
            .debug_selector(|| "linear-add-issues".into())
            .track_focus(&self.focus)
            .occlude()
            .w(px(PICKER_W))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .p(px(10.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.surface_dialog)
            .shadow_lg()
            .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    cx.emit(LinearPanelEvent::Close);
                }
            }))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(crate::typography::ui_rems(13.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme.text)
                                    .child(SharedString::from(format!(
                                        "Add issues from {}",
                                        self.project_name
                                    ))),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(crate::typography::ui_rems(11.0))
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(format!(
                                        "Open issues become work items under \u{201c}{item_title}\u{201d}"
                                    ))),
                            ),
                    )
                    .child(
                        LinearPanel::small_button("linear-add-issues-cancel", "Cancel", &theme)
                            .debug_selector(|| "linear-add-issues-cancel".into())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(LinearPanelEvent::Close))),
                    ),
            )
            .child(
                div()
                    .px(px(6.0))
                    .py(px(3.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(theme.border)
                    .child(self.search.clone()),
            )
            .child(list)
            .when_some(self.error.clone(), |el, error| {
                el.child(
                    div()
                        .debug_selector(|| "linear-add-issues-error".into())
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.warning)
                        .child(SharedString::from(error)),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        LinearPanel::small_button("linear-add-issues-select-all", "Select all", &theme)
                            .debug_selector(|| "linear-add-issues-select-all".into())
                            .when(unadded == 0, |el| el.opacity(0.5))
                            .tooltip(|_, cx| {
                                cx.new(|_| {
                                    TextTooltip(
                                        "Check every shown issue that isn\u{2019}t a work item yet"
                                            .into(),
                                    )
                                })
                                .into()
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.select_all(cx))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(crate::typography::ui_rems(10.5))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(format!("{count} selected"))),
                    )
                    .child(
                        LinearPanel::small_button("linear-add-issues-create", create_label, &theme)
                            .debug_selector(|| "linear-add-issues-create".into())
                            .when(count > 0, |el| {
                                el.border_color(theme.accent).text_color(theme.text)
                            })
                            .when(count == 0, |el| el.opacity(0.5))
                            .tooltip(move |_, cx| {
                                cx.new(|_| TextTooltip(create_tip.clone())).into()
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.create(cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_ids_skip_the_my_projects_lookup() {
        assert!(looks_like_uuid("5a8b7c9d-1234-4abc-9def-0123456789ab"));
        assert!(!looks_like_uuid("receiveai-ui-upgrades-9f8e7d6c5b4a"));
        assert!(!looks_like_uuid("p-launch"));
    }
}
