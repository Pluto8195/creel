//! Shell-owned Linear surfaces: Settings → Linear, and the Linear entries of
//! the Outline's right-click menus (`crate::linear::menu`).
use super::*;

use crate::linear::menu::LinearMenuAction;
use crate::linear::settings_page::LinearSettingsPage;

impl Shell {
    /// The Settings → Linear page (created on first visit).
    pub(super) fn linear_settings_outlet(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let page = match &self.linear_settings_page {
            Some(page) => page.clone(),
            None => {
                let state = self.state.clone();
                let page = cx.new(|cx| LinearSettingsPage::new(state, cx));
                self.linear_settings_page = Some(page.clone());
                page
            }
        };
        page.into_any_element()
    }

    /// Open Settings → Linear (Connect / Reconnect from Linear surfaces).
    pub(crate) fn open_linear_settings(&mut self, cx: &mut Context<Self>) {
        self.open_settings(SettingsSection::Linear, cx);
    }

    /// Open the Overview's Linear panel (its left column) on `space_id`.
    pub(crate) fn open_linear_panel(&mut self, space_id: Option<String>, cx: &mut Context<Self>) {
        if !matches!(self.route, Route::Overview) {
            self.open_overview(cx);
        }
        let page = self.ensure_overview_page(cx);
        let result = page.update(cx, |overview, cx| overview.open_linear_panel(space_id, cx));
        if let Err(notice) = result {
            self.sidebar_notice = Some(notice.into());
            cx.notify();
        }
    }

    /// Run a Linear entry of an Outline right-click menu.
    pub(super) fn run_linear_menu_action(
        &mut self,
        action: LinearMenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            LinearMenuAction::OpenInLinear(url) => self.open_work_item_reference(&url, window, cx),
            LinearMenuAction::RefreshFromLinear {
                space_id,
                identifier,
            } => {
                let weak = cx.entity().downgrade();
                crate::linear::store::refresh_linked(
                    &self.state,
                    &space_id,
                    vec![identifier],
                    cx,
                    move |result, cx| {
                        if let Err(error) = result {
                            weak.update(cx, |shell, cx| {
                                shell.sidebar_notice = Some(error.user_text().into());
                                cx.notify();
                            })
                            .ok();
                        }
                    },
                );
            }
            LinearMenuAction::UnlinkIssue(reference_id) => {
                let task = self
                    .state
                    .read(cx)
                    .work_outline_actions()
                    .remove_external_reference(mast_proto::RemoveWorkItemExternalReferenceParams {
                        reference_id,
                        operation_id: crate::work_outline::actions::new_operation_id(),
                    });
                cx.spawn(async move |this, cx| {
                    if let Err(error) = task.await {
                        this.update(cx, |shell, cx| {
                            shell.sidebar_notice = Some(error.message.into());
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
            }
            LinearMenuAction::SetAssignedDestination { space_id, item_id } => {
                let task = self
                    .state
                    .read(cx)
                    .work_outline_actions()
                    .set_linear_assigned_destination(
                        mast_proto::SetLinearAssignedDestinationParams {
                            space_id,
                            work_item_id: item_id,
                            operation_id: crate::work_outline::actions::new_operation_id(),
                        },
                    );
                cx.spawn(async move |this, cx| {
                    if let Err(error) = task.await {
                        this.update(cx, |shell, cx| {
                            shell.sidebar_notice = Some(
                                format!(
                                    "Couldn\u{2019}t change the linked Linear item: {}",
                                    error.user_text()
                                )
                                .into(),
                            );
                            cx.notify();
                        })
                        .ok();
                    }
                })
                .detach();
            }
            LinearMenuAction::OpenPanel(space_id) => self.open_linear_panel(Some(space_id), cx),
            LinearMenuAction::AddProjectIssues { space_id, item_id } => {
                if !matches!(self.route, Route::Overview) {
                    self.open_overview(cx);
                }
                let page = self.ensure_overview_page(cx);
                let result = page.update(cx, |overview, cx| {
                    overview.open_project_issue_picker(space_id, item_id, cx)
                });
                if let Err(notice) = result {
                    self.sidebar_notice = Some(notice.into());
                    cx.notify();
                }
            }
        }
    }
}
