//! The canonical plan's Linear project panel in the artifact tab (Artifacts
//! phase 5 H). It shows under the header of the item's pinned plan when the
//! item links a Linear project and its Space has Linear on. Read-only.
use gpui::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px,
};
use mast_proto::ArtifactView;

use super::ArtifactTab;
use crate::artifacts::{model, plan_linear};
use crate::linear::store::{ProjectDetailEntry, request_project_detail};
use crate::theme::Theme;

#[derive(Debug, Default)]
pub(crate) struct PlanState {
    /// The panel is folded to its one-line summary.
    pub(crate) collapsed: bool,
}

impl ArtifactTab {
    /// The linked project of a pinned plan's item: `(project id, cached title, url)`.
    pub(crate) fn plan_project(
        &self,
        artifact: &ArtifactView,
        cx: &gpui::App,
    ) -> Option<(String, String, String)> {
        if !artifact.pinned_as_plan {
            return None;
        }
        let item = artifact.work_item_id.as_deref()?;
        let load = self.state.read(cx).work_outline(&artifact.space_id);
        let snapshot = load.snapshot()?;
        if !crate::linear::space_enabled(Some(snapshot)) {
            return None;
        }
        let reference = snapshot.linear_project_reference(item)?;
        Some((
            reference.external_id.clone(),
            reference
                .cached_title
                .clone()
                .unwrap_or_else(|| "Linear project".to_owned()),
            reference.url.clone(),
        ))
    }

    pub(super) fn render_plan_linear(
        &mut self,
        artifact: &ArtifactView,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (project_id, title, url) = self.plan_project(artifact, cx)?;
        request_project_detail(&self.state, &project_id, false, cx);
        let entry: Option<ProjectDetailEntry> = self
            .state
            .read(cx)
            .linear
            .project_detail(&project_id)
            .cloned();
        let collapsed = self.plan.collapsed;
        let toggle = div()
            .id("artifact-plan-linear-toggle")
            .debug_selector(|| "artifact-plan-linear-toggle".into())
            .flex_none()
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(10.5))
            .text_color(theme.text_faint)
            .hover(|el| el.text_color(theme.text))
            .child(SharedString::from(if collapsed {
                "Show Linear project"
            } else {
                "Hide Linear project"
            }))
            .on_click(cx.listener(|this, _, _, cx| {
                this.plan.collapsed = !this.plan.collapsed;
                cx.notify();
            }));
        let panel = (!collapsed).then(|| {
            plan_linear::render(
                "artifact-plan-linear",
                entry.as_ref(),
                (&title, &url),
                model::now_ms(),
                self.shell.clone(),
                theme,
            )
        });
        Some(
            div()
                .flex_none()
                .px(px(12.0))
                .py(px(6.0))
                .flex()
                .flex_col()
                .gap(px(4.0))
                .border_b_1()
                .border_color(theme.border)
                .child(toggle)
                .children(panel)
                .into_any_element(),
        )
    }
}
