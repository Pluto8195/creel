//! The Overview's side of the Linear panel: open/close (palette command,
//! Outline toolbar button, Space row menu), the right-slot render (under
//! Work Item Details and Required Actions, above the chat panel), the
//! Outline drop of a panel issue, and select + reveal after a create.
//! Nothing here renders unless some Space has Linear on.
use gpui::{AnyElement, Context, Entity, div, prelude::*};

use super::Overview;
use crate::linear::panel::{LinearPanel, LinearPanelEvent, LinearSpace, OutlineSelection};
use crate::theme::Theme;
use crate::work_outline::WorkOutlineCommand;
use crate::work_outline::hints::{HintKey, WithHint};
use crate::work_outline::model::{OutlineRowKind, row_key};

impl Overview {
    /// Every Space with Linear on, in Outline order, with its snapshot.
    pub(crate) fn linear_spaces(&self, cx: &gpui::App) -> Vec<LinearSpace> {
        self.outline_spaces(cx)
            .into_iter()
            .filter_map(|space| {
                let snapshot = space.load.snapshot()?.clone();
                crate::linear::space_enabled(Some(&snapshot)).then(|| LinearSpace {
                    id: space.id,
                    label: space.label,
                    snapshot,
                })
            })
            .collect()
    }

    /// The Outline's selection as the Linear panel reads it.
    fn linear_selection(&self, cx: &gpui::App) -> OutlineSelection {
        match self.outline.read(cx).selected_row().map(|row| &row.kind) {
            None => OutlineSelection::None,
            Some(OutlineRowKind::Item(item)) => OutlineSelection::Item {
                space_id: item.space_id.clone(),
                item_id: item.item_id.clone(),
            },
            Some(OutlineRowKind::Space { space_id, .. }) => {
                OutlineSelection::Space(space_id.clone())
            }
            Some(_) => OutlineSelection::Other,
        }
    }

    #[cfg(test)]
    pub(crate) fn linear_panel(&self) -> Option<Entity<LinearPanel>> {
        self.linear_panel.clone()
    }

    /// Open (or retarget) the Linear panel on `space_id`, else on the
    /// selected row's Space, else the first Linear-enabled Space, and
    /// refresh it. Work Item Details close first (through the discard
    /// policy); Required Actions close so the panel shows.
    pub(crate) fn open_linear_panel(
        &mut self,
        space_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let spaces = self.linear_spaces(cx);
        if spaces.is_empty() {
            return Err("Turn Linear on for a project first (its Space details)");
        }
        let enabled = |id: &str| spaces.iter().any(|space| space.id == id);
        let selected_space = match self.linear_selection(cx) {
            OutlineSelection::Item { space_id, .. } | OutlineSelection::Space(space_id) => {
                Some(space_id)
            }
            _ => None,
        };
        let target = space_id
            .filter(|id| enabled(id))
            .or(selected_space.filter(|id| enabled(id)))
            .unwrap_or_else(|| spaces[0].id.clone());
        let panel = match self.linear_panel.clone() {
            Some(panel) => {
                panel.update(cx, |panel, cx| panel.set_space(target, cx));
                panel
            }
            None => {
                let state = self.state.clone();
                let panel = cx.new(|cx| LinearPanel::new(state, target, cx));
                let events = cx.subscribe(&panel, |this: &mut Self, _, event, cx| {
                    this.on_linear_panel_event(event.clone(), cx)
                });
                self.linear_panel = Some(panel.clone());
                self._linear_panel_events = Some(events);
                panel
            }
        };
        let selection = self.linear_selection(cx);
        panel.update(cx, |panel, cx| {
            panel.set_data(spaces, selection);
            panel.refresh(cx);
        });
        self.close_required_actions(cx);
        if self.work_item_details.is_some() {
            self.leave_work_item_details(super::DetailsExit::Close, cx);
        }
        cx.notify();
        Ok(())
    }

    pub(crate) fn close_linear_panel(&mut self, cx: &mut Context<Self>) {
        if self.linear_panel.take().is_some() {
            self._linear_panel_events = None;
            cx.notify();
        }
    }

    fn on_linear_panel_event(&mut self, event: LinearPanelEvent, cx: &mut Context<Self>) {
        match event {
            LinearPanelEvent::Close => self.close_linear_panel(cx),
            LinearPanelEvent::Reveal { space_id, item_id } => {
                let key = row_key::item(&space_id, &item_id);
                let shown = self
                    .outline
                    .update(cx, |outline, cx| outline.reveal(&key, cx));
                if !shown {
                    self.outline.update(cx, |outline, cx| {
                        outline.show_notice("That item isn\u{2019}t shown in the Outline", cx)
                    });
                }
            }
            LinearPanelEvent::Created {
                space_id,
                item_id,
                parent_id,
            } => {
                let key = row_key::item(&space_id, &item_id);
                let parent = parent_id.map(|parent| row_key::item(&space_id, &parent));
                self.outline.update(cx, |outline, cx| {
                    outline.reveal_when_shown(&key, parent.as_deref(), cx)
                });
            }
            LinearPanelEvent::OpenSettings => {
                if let Some(shell) = self.shell.upgrade() {
                    cx.defer(move |cx| {
                        shell.update(cx, |shell, cx| shell.open_linear_settings(cx))
                    });
                }
            }
        }
    }

    /// A panel issue dropped on an Outline item: create it there.
    pub(super) fn linear_issue_dropped(
        &mut self,
        space_id: String,
        item_id: String,
        issue: zeron_proto::linear::LinearIssueSummary,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.linear_panel.clone() else {
            return;
        };
        panel.update(cx, |panel, cx| {
            panel.create_under(space_id, Some(item_id), vec![issue], None, cx)
        });
    }

    /// The panel for the right slot, fed this frame's data; closes it when
    /// no Space has Linear on any more.
    pub(super) fn render_linear_panel_slot(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let panel = self.linear_panel.clone()?;
        let spaces = self.linear_spaces(cx);
        if spaces.is_empty() {
            self.close_linear_panel(cx);
            return None;
        }
        let selection = self.linear_selection(cx);
        panel.update(cx, |panel, _| panel.set_data(spaces, selection));
        Some(div().h_full().flex_none().child(panel).into_any_element())
    }

    /// The Outline toolbar's Linear button (only while some Space has
    /// Linear on).
    pub(super) fn linear_toolbar_button(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.linear_spaces(cx).is_empty() {
            return None;
        }
        let active = self.linear_panel.is_some();
        Some(
            div()
                .id("outline-linear")
                .debug_selector(|| "outline-linear".into())
                .px(gpui::px(8.0))
                .py(gpui::px(3.0))
                .rounded(gpui::px(4.0))
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(11.0))
                .bg(theme.element_hover.opacity(0.5))
                .when(active, |el| {
                    el.bg(theme.element_active).text_color(theme.text)
                })
                .when(!active, |el| el.text_color(theme.text_muted))
                .hover(|el| el.text_color(theme.text))
                .child("Linear")
                .hint(
                    "Your Linear issues and projects",
                    HintKey::Command(WorkOutlineCommand::OpenLinear),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if this.linear_panel.is_some() {
                        this.close_linear_panel(cx);
                    } else if let Err(notice) = this.open_linear_panel(None, cx) {
                        this.outline
                            .update(cx, |outline, cx| outline.show_notice(notice, cx));
                    }
                }))
                .into_any_element(),
        )
    }
}
