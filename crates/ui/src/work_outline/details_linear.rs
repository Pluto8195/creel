//! Linear in Work Item Details.
//!
//! - Space-root variant: the per-Space **Linear** toggle
//!   (`setSpaceLinearEnabled`) with a one-line explanation, and "Connect
//!   Linear…" (Settings → Linear) while this device has no key.
//! - Items of a Linear-enabled Space: the **Linear** section (live issue
//!   chip, project chip, ↻ = `LinearRefreshLinkedItems` for the Space,
//!   Unlink; on a project link also "Add issues from <project>…" and, for a
//!   pasted-URL link My projects resolves, Fix link), and "Synced from Linear" on a title / description that still
//!   follows the issue (`externalSyncModifiedFields` does not list it).
//! - Pasted references: a Linear URL becomes an `issue` / `project`
//!   reference only with Linear on; otherwise it stays a plain link and the
//!   Space shows a one-time "Enable Linear for this Space?" hint.
//!
//! A child module of `details_panel` (like the instruction and execution
//! sections), so it uses the surface's private state directly.
use std::collections::BTreeSet;

use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};
use zeron_proto::linear::{LINEAR_PROJECT_REFERENCE, LINEAR_PROVIDER, LinearProjectRef};
use zeron_proto::{
    CreateWorkItemExternalReferenceParams, LinkLinearProjectParams,
    RemoveWorkItemExternalReferenceParams, SetSpaceLinearEnabledParams,
};

use super::super::actions::{OperationSlot, new_operation_id};
use super::super::chips::TextTooltip;
use super::super::store::{WorkItemView, WorkOutlineSnapshot};
use super::{ParsedReference, WorkItemDetails};
use crate::linear::store::{self, IssueEntry};
use crate::linear::{LinearUrl, parse_linear_url, project_short_id};
use crate::theme::Theme;

/// Spaces that already showed the "Enable Linear for this Space?" hint
/// (device-local, under the data directory).
pub(crate) const PASTE_HINT_SEEN_FILE: &str = "linear-paste-hint-seen.json";

/// Per-surface Linear state.
#[derive(Debug, Default)]
pub(crate) struct LinearDetailsState {
    pub(crate) error: Option<String>,
    pub(crate) status_line: Option<String>,
    pub(crate) refreshing: bool,
    pub(crate) toggling: bool,
    /// The one-time hint after a Linear URL was pasted with Linear off.
    pub(crate) paste_hint: bool,
    toggle_op: OperationSlot,
}

impl WorkItemDetails {
    fn linear_enabled(&self) -> bool {
        crate::linear::space_enabled(self.snapshot.as_deref())
    }

    // ----------------------------------------------------------------------
    // Pasted references
    // ----------------------------------------------------------------------

    /// What a pasted reference becomes here. `None` when this module takes
    /// the add over (a project URL in a Linear-enabled Space).
    pub(super) fn linear_gate_reference(
        &mut self,
        parsed: ParsedReference,
        cx: &mut Context<Self>,
    ) -> Option<ParsedReference> {
        let Some(linear) = parse_linear_url(&parsed.url) else {
            return Some(parsed);
        };
        if !self.linear_enabled() {
            self.offer_paste_hint(cx);
            return Some(ParsedReference {
                provider: "web".into(),
                reference_type: "link".into(),
                external_id: parsed.url.clone(),
                url: parsed.url,
            });
        }
        match linear {
            LinearUrl::Issue(_) => Some(parsed),
            LinearUrl::Project(slug) => {
                self.link_pasted_project(parsed.url, slug, cx);
                None
            }
        }
    }

    fn offer_paste_hint(&mut self, cx: &mut Context<Self>) {
        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
            self.linear.paste_hint = true;
            return;
        };
        let mut seen: BTreeSet<String> =
            crate::overview_positions::load_string_set(&data_dir, PASTE_HINT_SEEN_FILE);
        if !seen.insert(self.space_id.clone()) {
            return;
        }
        if let Err(err) =
            crate::overview_positions::save_string_set(&data_dir, PASTE_HINT_SEEN_FILE, &seen)
        {
            tracing::warn!(error = %err, "saving the Linear hint state failed");
        }
        self.linear.paste_hint = true;
    }

    /// A pasted project URL: link the matching "My projects" project
    /// (`linkLinearProject`); without a match, a `linear`/`project`
    /// reference keyed by the URL's slug.
    fn link_pasted_project(&mut self, url: String, slug: String, cx: &mut Context<Self>) {
        if !self.in_flight.insert("reference:add".into()) {
            return;
        }
        let actions = self.state.read(cx).work_outline_actions();
        let cached = self.state.read(cx).linear.projects.clone();
        let item_id = self.item_id.clone();
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let projects = match cached {
                Some(projects) => Some(projects),
                None => actions.linear_my_projects().await.ok(),
            };
            let short = project_short_id(&slug).map(str::to_owned);
            let matched = projects.and_then(|projects| {
                projects.into_iter().find(|project| {
                    project.url.trim_end_matches('/') == url.trim_end_matches('/')
                        || (short.is_some() && project_short_id(&project.url) == short.as_deref())
                })
            });
            let result = match matched {
                Some(project) => {
                    actions
                        .link_linear_project(LinkLinearProjectParams {
                            work_item_id: item_id,
                            project: LinearProjectRef {
                                id: project.id,
                                name: project.name,
                                url: project.url,
                            },
                            operation_id: new_operation_id(),
                        })
                        .await
                }
                None => {
                    actions
                        .link_linear_issue(CreateWorkItemExternalReferenceParams {
                            work_item_id: item_id,
                            provider: LINEAR_PROVIDER.into(),
                            reference_type: LINEAR_PROJECT_REFERENCE.into(),
                            external_id: slug,
                            url,
                            cached_title: None,
                            operation_id: new_operation_id(),
                        })
                        .await
                }
            };
            this.update(cx, |this, cx| {
                this.in_flight.remove("reference:add");
                match result {
                    Ok(_) => this.adding_reference = false,
                    Err(error) => this.error = Some(error.user_text()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn set_linear_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.linear.toggling {
            return;
        }
        let operation_id = self
            .linear
            .toggle_op
            .id_for(&format!("{}:{enabled}", self.space_id));
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .set_space_linear_enabled(SetSpaceLinearEnabledParams {
                space_id: self.space_id.clone(),
                enabled,
                operation_id,
            });
        self.linear.toggling = true;
        self.linear.error = None;
        if enabled {
            store::ensure_status(&self.state, cx);
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.linear.toggling = false;
                match result {
                    Ok(_) => {
                        this.linear.toggle_op = OperationSlot::default();
                        this.linear.paste_hint = false;
                    }
                    Err(error) => this.linear.error = Some(error.user_text()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_linear_settings(&mut self, cx: &mut Context<Self>) {
        if let Some(shell) = self.shell.upgrade() {
            cx.defer(move |cx| shell.update(cx, |shell, cx| shell.open_linear_settings(cx)));
        }
    }

    fn refresh_linear(&mut self, identifier: Option<String>, cx: &mut Context<Self>) {
        if self.linear.refreshing {
            return;
        }
        self.linear.refreshing = true;
        self.linear.error = None;
        self.linear.status_line = None;
        let weak = cx.entity().downgrade();
        store::refresh_linked(
            &self.state,
            &self.space_id,
            identifier.into_iter().collect(),
            cx,
            move |result, cx| {
                weak.update(cx, |this, cx| {
                    this.linear.refreshing = false;
                    match result {
                        Ok(0) => this.linear.status_line = Some("Up to date".into()),
                        Ok(n) => {
                            this.linear.status_line =
                                Some(format!("Updated {n} item{}", if n == 1 { "" } else { "s" }))
                        }
                        Err(error) => this.linear.error = Some(error.user_text()),
                    }
                    cx.notify();
                })
                .ok();
            },
        );
        cx.notify();
    }

    fn unlink_linear(&mut self, reference_id: String, cx: &mut Context<Self>) {
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

    /// Fix link: relink a slug-keyed project link by the real project id.
    fn fix_linear_project_link(
        &mut self,
        reference: zeron_proto::WorkItemExternalReferenceView,
        project: zeron_proto::linear::LinearProjectSummary,
        cx: &mut Context<Self>,
    ) {
        let key = format!("linear:fix:{}", reference.id);
        if !self.in_flight.insert(key.clone()) {
            return;
        }
        self.linear.error = None;
        let task =
            crate::linear::project_links::fix_project_link(&self.state, &reference, &project, cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.in_flight.remove(&key);
                match result {
                    Ok(()) => {
                        this.linear.status_line =
                            Some(format!("Fixed the link to {}", project.name))
                    }
                    Err(error) => this.linear.error = Some(error.user_text()),
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

    /// "Synced from Linear" under a field that still follows the issue.
    pub(super) fn linear_sync_marker(
        &self,
        item: &WorkItemView,
        snapshot: &WorkOutlineSnapshot,
        field: &'static str,
        theme: &Theme,
    ) -> Option<AnyElement> {
        if !crate::linear::space_enabled(Some(snapshot))
            || snapshot.linear_issue_reference(&item.id).is_none()
            || !item.follows_external_sync(field)
        {
            return None;
        }
        let selector = format!("work-item-{field}-synced");
        Some(
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .flex_none()
                .text_size(crate::typography::ui_rems(10.0))
                .text_color(theme.text_faint)
                .child("Synced from Linear")
                .tooltip(|_, cx| {
                    cx.new(|_| {
                        TextTooltip("Follows the Linear issue until you edit it here".into())
                    })
                    .into()
                })
                .into_any_element(),
        )
    }

    /// The item's Linear section (Linear-enabled Spaces, linked items only).
    pub(super) fn render_linear_section(
        &mut self,
        item: &WorkItemView,
        snapshot: &WorkOutlineSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !crate::linear::space_enabled(Some(snapshot)) {
            return None;
        }
        let issue = snapshot.linear_issue_reference(&item.id).cloned();
        let project = snapshot.linear_project_reference(&item.id).cloned();
        if issue.is_none() && project.is_none() {
            return None;
        }
        let shell = Some(self.shell.clone());
        let refresh_identifier = issue.as_ref().map(|r| r.external_id.clone());
        let mut section = div().flex().flex_col().gap(px(4.0)).child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    div()
                        .flex_1()
                        .text_size(crate::typography::ui_rems(11.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text_muted)
                        .child("Linear"),
                )
                .child(
                    Self::small_button(
                        "work-item-linear-refresh",
                        if self.linear.refreshing {
                            "Refreshing\u{2026}"
                        } else {
                            "\u{21bb} Refresh"
                        },
                        theme,
                    )
                    .debug_selector(|| "work-item-linear-refresh".into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.refresh_linear(refresh_identifier.clone(), cx)
                    })),
                ),
        );
        if let Some(reference) = issue {
            store::request_issue(&self.state, &reference.external_id, false, cx);
            let entry = self
                .state
                .read(cx)
                .linear
                .issue(&reference.external_id)
                .cloned();
            let title = match &entry {
                Some(IssueEntry::Ready { issue, .. }) => {
                    format!("{} \u{00b7} {}", issue.identifier, issue.title)
                }
                _ => reference.external_id.clone(),
            };
            let chip = super::super::outline_view::linear_outline::issue_chip(
                "work-item-linear-chip".into(),
                &reference.external_id,
                entry.as_ref(),
                reference.url.clone(),
                shell.clone(),
                theme,
            );
            let id = reference.id.clone();
            section = section.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(chip)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(11.5))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(title)),
                    )
                    .child(
                        Self::small_button("work-item-linear-unlink", "Unlink", theme)
                            .debug_selector(|| "work-item-linear-unlink".into())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.unlink_linear(id.clone(), cx)
                            })),
                    ),
            );
        }
        if let Some(reference) = project {
            let id = reference.id.clone();
            let project_name = reference
                .cached_title
                .clone()
                .unwrap_or_else(|| "project".into());
            let repair = self
                .state
                .read(cx)
                .linear
                .projects
                .clone()
                .and_then(|projects| {
                    crate::linear::project_links::repair_target(&reference, &projects).cloned()
                });
            let fix = repair.map(|project| {
                let reference = reference.clone();
                super::super::outline_view::linear_outline::fix_link_chip(
                    "work-item-linear-fix-link".into(),
                    &project.name,
                    theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.fix_linear_project_link(reference.clone(), project.clone(), cx)
                }))
            });
            let chip = super::super::outline_view::linear_outline::project_chip(
                "work-item-linear-project".into(),
                reference
                    .cached_title
                    .clone()
                    .unwrap_or_else(|| "Linear project".into()),
                reference.url.clone(),
                shell,
                theme,
            );
            section = section.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(chip)
                    .children(fix)
                    .child(div().flex_1())
                    .child(
                        Self::small_button("work-item-linear-unlink-project", "Unlink", theme)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.unlink_linear(id.clone(), cx)
                            })),
                    ),
            );
            let add_selector = "work-item-linear-add-issues";
            let tip: SharedString =
                format!("Pick open issues of {project_name} to add as work items under this one")
                    .into();
            section = section.child(
                div().flex().child(
                    crate::linear::panel::LinearPanel::small_button(
                        add_selector,
                        format!("Add issues from {project_name}\u{2026}"),
                        theme,
                    )
                    .max_w(px(320.0))
                    .debug_selector(move || add_selector.into())
                    .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.emit(super::DetailsEvent::AddLinearIssues {
                            space_id: this.space_id.clone(),
                            item_id: this.item_id.clone(),
                        })
                    })),
                ),
            );
        }
        if let Some(line) = self.linear.status_line.clone() {
            section = section.child(
                div()
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(line)),
            );
        }
        if let Some(error) = self.linear.error.clone() {
            section = section.child(
                div()
                    .debug_selector(|| "work-item-linear-error".into())
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.warning)
                    .child(SharedString::from(error)),
            );
        }
        Some(section.into_any_element())
    }

    /// The one-time "Enable Linear for this Space?" hint.
    pub(super) fn render_linear_paste_hint(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.linear.paste_hint || self.linear_enabled() {
            return None;
        }
        Some(
            div()
                .debug_selector(|| "work-item-linear-hint".into())
                .flex()
                .items_center()
                .gap(px(6.0))
                .px(px(8.0))
                .py(px(6.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(theme.border)
                .child(
                    div()
                        .flex_1()
                        .text_size(crate::typography::ui_rems(11.5))
                        .text_color(theme.text_muted)
                        .child("Linear link saved as a plain link. Enable Linear for this Space?"),
                )
                .child(
                    Self::small_button("work-item-linear-hint-enable", "Enable", theme)
                        .debug_selector(|| "work-item-linear-hint-enable".into())
                        .on_click(cx.listener(|this, _, _, cx| this.set_linear_enabled(true, cx))),
                )
                .child(
                    Self::small_button("work-item-linear-hint-dismiss", "Dismiss", theme)
                        .debug_selector(|| "work-item-linear-hint-dismiss".into())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.linear.paste_hint = false;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    /// The Space-root Linear toggle.
    pub(super) fn render_space_linear(
        &mut self,
        snapshot: &WorkOutlineSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let enabled = snapshot.space_settings.linear_enabled;
        // A Space with Linear off makes no Linear calls; turning it on
        // fetches the status (`set_linear_enabled`).
        if enabled {
            store::ensure_status(&self.state, cx);
        }
        let store = &self.state.read(cx).linear;
        let configured = store.configured();
        let not_available = matches!(&store.status, Some(Err(error)) if error.not_available());
        let toggle = crate::settings::widgets::toggle_switch(theme, enabled)
            .id("space-linear-toggle")
            .debug_selector(|| "space-linear-toggle".into())
            .cursor_pointer()
            .when(self.linear.toggling, |el| el.opacity(0.5))
            .on_click(cx.listener(move |this, _, _, cx| this.set_linear_enabled(!enabled, cx)));
        let mut section = div()
            .debug_selector(|| "space-linear".into())
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .text_size(crate::typography::ui_rems(11.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text_muted)
                            .child("Linear"),
                    )
                    .child(toggle),
            )
            .child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_faint)
                    .child(if enabled {
                        "On: the Linear panel, chips and links work in this project. Nothing is created or written automatically."
                    } else {
                        "Off: no Linear calls or UI in this project; Linear URLs stay plain links."
                    }),
            );
        if not_available {
            section = section.child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child("Linear isn\u{2019}t available on this host yet"),
            );
        } else if configured == Some(false) {
            section = section.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .child("No Linear API key on this device."),
                    )
                    .child(
                        Self::small_button("space-linear-connect", "Connect Linear\u{2026}", theme)
                            .debug_selector(|| "space-linear-connect".into())
                            .on_click(cx.listener(|this, _, _, cx| this.open_linear_settings(cx))),
                    ),
            );
        }
        if let Some(error) = self.linear.error.clone() {
            section = section.child(
                div()
                    .debug_selector(|| "space-linear-error".into())
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.warning)
                    .child(SharedString::from(error)),
            );
        }
        section.into_any_element()
    }
}

#[cfg(test)]
impl WorkItemDetails {
    /// Paste `url` into the external-reference field and add it (tests).
    pub(crate) fn paste_reference_for_test(&mut self, url: &str, cx: &mut Context<Self>) {
        self.adding_reference = true;
        self.reference_input
            .update(cx, |input, cx| input.set_text(url, cx));
        self.add_reference(cx);
    }
}
