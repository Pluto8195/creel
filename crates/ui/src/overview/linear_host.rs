//! The Overview's side of the Linear panel: its own LEFT column (between
//! the window edge and the Outline/List body), independent of the
//! right slot, so the chat panel / Work Item Details / Required Actions stay
//! up beside it. Open/close (toggle command + hotkey, Outline toolbar
//! button, Space row menu), the per-device open flag and width, the edge
//! drag, the narrow-window rule (shrink to the minimum, then overlay the
//! body), the reduced-motion-aware open/close tween, the Outline drop of a
//! panel issue, and select + reveal after a create. The column (headed
//! "Inbox") stacks the Linear panel and the GitHub section
//! (`overview/github_host.rs`), each shown only where it applies; nothing
//! renders unless some Space has Linear on or the PR review sync.
use std::cell::Cell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::time::Instant;

use gpui::{AnyElement, Context, Entity, MouseButton, Window, div, prelude::*, px};

use super::Overview;
use crate::linear::panel::{LinearPanel, LinearPanelEvent, LinearSpace, OutlineSelection};
use crate::linear::prefs;
use crate::linear::project_picker::ProjectIssuePicker;
use crate::theme::Theme;
use crate::work_outline::WorkOutlineCommand;
use crate::work_outline::hints::{HintKey, WithHint};
use crate::work_outline::model::{OutlineRowKind, row_key};

/// The narrowest the Outline/List body gets while the Linear column
/// is inline; past that the column shrinks, then overlays.
pub(crate) const OVERVIEW_BODY_MIN_W: f32 = 360.0;
/// Hit target of the column's right-edge resize grabber.
const RESIZE_HANDLE_W: f32 = 6.0;

/// Drag marker for the column's edge grabber.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LinearPanelResize;

struct LinearResizeGhost;

impl gpui::Render for LinearResizeGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// How the column lays out this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum LinearColumn {
    /// In the flex row at this width.
    Inline(f32),
    /// Too narrow for body + column: floats over the body's left edge.
    Overlay(f32),
}

/// The column's layout for an Overview `overview_w` wide (`None` before
/// the first paint) with `fixed` px taken by the other side panels: the
/// preferred width when the body keeps [`OVERVIEW_BODY_MIN_W`]; else as
/// narrow as needed down to [`prefs::PANEL_MIN_W`]; below that, an overlay
/// at the minimum so the body is never squeezed to nothing.
pub(crate) fn linear_column(overview_w: Option<f32>, preferred: f32, fixed: f32) -> LinearColumn {
    let preferred = prefs::clamp_width(preferred);
    let Some(total) = overview_w else {
        return LinearColumn::Inline(preferred);
    };
    let room = total - fixed - OVERVIEW_BODY_MIN_W;
    if room >= preferred {
        LinearColumn::Inline(preferred)
    } else if room >= prefs::PANEL_MIN_W {
        LinearColumn::Inline(room.floor())
    } else {
        LinearColumn::Overlay(prefs::PANEL_MIN_W)
    }
}

/// One open or close of the column, evaluated by hand each frame (the
/// Shell's `WidthTween` idiom: no `with_animation` wrapper to replay).
#[derive(Debug, Clone, Copy)]
pub(crate) struct LinearTween {
    opening: bool,
    started: Instant,
}

impl LinearTween {
    /// The shown fraction of the column (0 = closed) and whether done.
    fn fraction(&self, now: Instant) -> (f32, bool) {
        let spec = crate::motion::RESIZE;
        let total = spec.total().as_secs_f32().max(f32::EPSILON);
        let raw = (now.saturating_duration_since(self.started).as_secs_f32() / total).min(1.0);
        let eased = spec.progress(raw);
        let fraction = if self.opening { eased } else { 1.0 - eased };
        (fraction, raw >= 1.0)
    }
}

/// The column's state on the Overview.
pub(crate) struct LinearColumnState {
    /// The user's choice (persisted): the column shows whenever this is on
    /// and some Space has Linear on.
    pub(crate) open: bool,
    /// Preferred width (persisted), clamped to the panel bounds.
    pub(crate) width: f32,
    /// Toggled before the UI-flag file loaded: the toggle wins.
    touched: bool,
    pub(crate) tween: Option<LinearTween>,
    /// The panel kept on screen while the close tween runs.
    closing: Option<Entity<LinearPanel>>,
    /// The GitHub section kept on screen while the close tween runs.
    closing_github: Option<Entity<crate::github::panel::GithubPanel>>,
    /// Pointer x and shown width when an edge drag began.
    resize_from: Option<(f32, f32)>,
    /// Width the column showed last frame (the drag's base).
    shown_width: f32,
    /// Overview width as last painted.
    overview_width: Rc<Cell<Option<f32>>>,
    /// Focus the panel's search on the next render.
    focus_search: bool,
    /// Focus the Outline tree (or List body) on the next render.
    return_focus: bool,
}

impl Default for LinearColumnState {
    fn default() -> Self {
        Self {
            open: false,
            width: prefs::PANEL_DEFAULT_W,
            touched: false,
            tween: None,
            closing: None,
            closing_github: None,
            resize_from: None,
            shown_width: prefs::PANEL_DEFAULT_W,
            overview_width: Rc::new(Cell::new(None)),
            focus_search: false,
            return_focus: false,
        }
    }
}

/// The column element for the Overview root.
pub(crate) enum LinearSlot {
    Inline(AnyElement),
    Overlay(AnyElement),
}

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

    /// Whether the left column shows (either section).
    pub(crate) fn column_shown(&self) -> bool {
        self.linear_panel.is_some() || self.github_panel.is_some()
    }

    /// Feed (or create) the GitHub section. Opening (`open`) retargets it:
    /// `target` when it applies, else the selected row's Space, and
    /// refreshes it; a new section starts on the first applicable Space and
    /// refreshes once.
    fn sync_github_panel(&mut self, target: Option<String>, open: bool, cx: &mut Context<Self>) {
        let spaces = self.github_spaces(cx);
        if spaces.is_empty() {
            self.drop_github_panel();
            return;
        }
        let applies = |id: &str| spaces.iter().any(|space| space.id == id);
        let selected_space = match self.linear_selection(cx) {
            OutlineSelection::Item { space_id, .. } | OutlineSelection::Space(space_id) => {
                Some(space_id)
            }
            _ => None,
        };
        let target = open
            .then(|| {
                target
                    .filter(|id| applies(id))
                    .or(selected_space.filter(|id| applies(id)))
            })
            .flatten();
        if self.github_panel.is_none()
            && let Some(panel) = self.linear_column.closing_github.take()
        {
            self.github_panel = Some(panel);
            self._github_panel_events = None;
        }
        let fresh = self.github_panel.is_none();
        let panel =
            self.ensure_github_panel(target.clone().unwrap_or_else(|| spaces[0].id.clone()), cx);
        let selection = self.linear_selection(cx);
        panel.update(cx, |panel, cx| {
            panel.set_data(spaces, selection, cx);
            let moved = target.is_some_and(|target| {
                let moved = panel.space_id != target;
                panel.set_space(target, cx);
                moved
            });
            if (fresh || open) && !moved {
                panel.refresh(cx);
            }
        });
    }

    #[cfg(test)]
    pub(crate) fn linear_panel(&self) -> Option<gpui::Entity<LinearPanel>> {
        self.linear_panel.clone()
    }

    #[cfg(test)]
    pub(crate) fn linear_column_state(&self) -> &LinearColumnState {
        &self.linear_column
    }

    /// Restore the open flag and width from the UI-flag set (a toggle made
    /// before the file loaded wins for the open flag).
    pub(super) fn restore_linear_column(&mut self, flags: &BTreeSet<String>) {
        if !self.linear_column.touched {
            self.linear_column.open = flags.contains(prefs::PANEL_OPEN_FLAG);
        }
        if let Some(width) = prefs::width_from_flags(flags) {
            self.linear_column.width = width;
            self.linear_column.shown_width = width;
        }
    }

    fn set_linear_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.linear_column.open = open;
        if !self.prefs_loaded {
            self.linear_column.touched = true;
        }
        self.persist_ui_flag(prefs::PANEL_OPEN_FLAG.to_string(), open, cx);
    }

    /// Create the panel (or take back the one still closing) on `target`.
    fn ensure_linear_panel(
        &mut self,
        target: String,
        cx: &mut Context<Self>,
    ) -> Entity<LinearPanel> {
        if self.linear_panel.is_none()
            && let Some(panel) = self.linear_column.closing.take()
        {
            self.linear_panel = Some(panel);
        }
        match self.linear_panel.clone() {
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
        }
    }

    fn start_linear_tween(&mut self, opening: bool, cx: &gpui::App) {
        self.linear_column.tween = (!crate::motion::reduced_motion(cx)).then(|| LinearTween {
            opening,
            started: Instant::now(),
        });
        if !opening && self.linear_column.tween.is_none() {
            self.linear_column.closing = None;
            self.linear_column.closing_github = None;
        }
    }

    /// Open (or retarget) the Linear panel on `space_id`, else on the
    /// selected row's Space, else the first Linear-enabled Space, and
    /// refresh it. The right slot (chat panel, Work Item Details, Required
    /// Actions) is left exactly as it is.
    pub(crate) fn open_linear_panel(
        &mut self,
        space_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let spaces = self.linear_spaces(cx);
        let github = !self.github_spaces(cx).is_empty();
        if spaces.is_empty() && !github {
            return Err(crate::linear::LINEAR_OFF_NOTICE);
        }
        let was_shown = self.column_shown();
        if github {
            self.sync_github_panel(space_id.clone(), true, cx);
        }
        if spaces.is_empty() {
            if !self.linear_column.open {
                self.set_linear_open(true, cx);
            }
            if !was_shown {
                self.start_linear_tween(true, cx);
            }
            cx.notify();
            return Ok(());
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
        let panel = self.ensure_linear_panel(target, cx);
        let selection = self.linear_selection(cx);
        panel.update(cx, |panel, cx| {
            panel.set_data(spaces, selection);
            panel.refresh(cx);
        });
        if !self.linear_column.open {
            self.set_linear_open(true, cx);
        }
        if !was_shown {
            self.start_linear_tween(true, cx);
        }
        cx.notify();
        Ok(())
    }

    /// Close the column (persisted). `return_focus`: hand the keyboard back
    /// to the Outline tree (List: the body) on the next render.
    pub(crate) fn close_linear_panel(&mut self, return_focus: bool, cx: &mut Context<Self>) {
        if self.linear_column.open {
            self.set_linear_open(false, cx);
        }
        let shown = self.column_shown();
        if let Some(panel) = self.linear_panel.take() {
            self._linear_panel_events = None;
            self.linear_column.closing = Some(panel);
        }
        if let Some(panel) = self.github_panel.take() {
            self._github_panel_events = None;
            self.linear_column.closing_github = Some(panel);
        }
        if shown {
            self.start_linear_tween(false, cx);
        }
        self.linear_column.return_focus |= return_focus;
        self.linear_column.focus_search = false;
        cx.notify();
    }

    /// The toggle (command, hotkey, palette): closed → open and focus its
    /// search; open but focus elsewhere (tree, chat composer) → focus its
    /// search; open with focus inside → close and return focus to the tree.
    /// With no Space on Linear it does nothing but explain.
    pub(crate) fn toggle_linear_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        if self.column_shown() {
            let focused = self
                .linear_panel
                .as_ref()
                .is_some_and(|panel| panel.read(cx).contains_focus(window, cx))
                || self
                    .github_panel
                    .as_ref()
                    .is_some_and(|panel| panel.read(cx).contains_focus(window, cx));
            if focused {
                self.close_linear_panel(false, cx);
                self.focus_overview_body(window, cx);
            } else {
                self.focus_column(window, cx);
            }
            cx.notify();
            return Ok(());
        }
        self.open_linear_panel(None, cx)?;
        self.linear_column.focus_search = true;
        self.focus_column(window, cx);
        Ok(())
    }

    /// The Linear search when Linear shows, else the GitHub section.
    fn focus_column(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.linear_panel.clone() {
            panel.update(cx, |panel, cx| panel.focus_search(window, cx));
        } else if let Some(panel) = self.github_panel.clone() {
            panel.update(cx, |panel, cx| panel.focus(window, cx));
        }
    }

    /// The Outline tree in Outline mode, else the List body.
    fn focus_overview_body(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.view_mode == super::OverviewMode::Outline {
            let handle = gpui::Focusable::focus_handle(self.outline.read(cx), cx);
            window.focus(&handle, cx);
        } else {
            window.focus(&self.body_focus, cx);
        }
    }

    fn on_linear_panel_event(&mut self, event: LinearPanelEvent, cx: &mut Context<Self>) {
        match event {
            LinearPanelEvent::Close => self.close_linear_panel(true, cx),
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
            LinearPanelEvent::CreateProjectItem { space_id, project } => {
                let mut flow = crate::work_outline::outline_view::templates::TemplateFlow::new(
                    space_id, None,
                );
                flow.ask_parent = true;
                flow.linear = Some(project);
                self.outline
                    .update(cx, |outline, cx| outline.request_new_from_template(flow, cx));
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
        issue: mast_proto::linear::LinearIssueSummary,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.linear_panel.clone() else {
            return;
        };
        panel.update(cx, |panel, cx| {
            panel.create_under(space_id, Some(item_id), vec![issue], None, cx)
        });
    }

    /// "Add issues from <project>…" on project-linked item `item_id` (its
    /// menu or Details): open the picker over the Overview. Works with the
    /// Linear panel closed; refused outside Linear-enabled Spaces.
    pub(crate) fn open_project_issue_picker(
        &mut self,
        space_id: String,
        item_id: String,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let snapshot = self
            .outline_snapshot(&space_id, cx)
            .filter(|snapshot| crate::linear::space_enabled(Some(snapshot)))
            .ok_or(crate::linear::LINEAR_OFF_NOTICE)?;
        let reference = snapshot
            .item(&item_id)
            .filter(|item| item.archived_at.is_none())
            .and_then(|_| snapshot.linear_project_reference(&item_id))
            .cloned()
            .ok_or("This work item isn\u{2019}t linked to a Linear project")?;
        let state = self.state.clone();
        let picker = cx.new(|cx| {
            let mut picker = ProjectIssuePicker::new(state, snapshot, item_id, reference, cx);
            picker.load(cx);
            picker
        });
        let events = cx.subscribe(&picker, |this: &mut Self, _, event, cx| match event {
            LinearPanelEvent::Close => this.close_project_issue_picker(cx),
            LinearPanelEvent::Created { .. } | LinearPanelEvent::Reveal { .. } => {
                this.on_linear_panel_event(event.clone(), cx)
            }
            LinearPanelEvent::OpenSettings | LinearPanelEvent::CreateProjectItem { .. } => {
                this.on_linear_panel_event(event.clone(), cx)
            }
        });
        self.project_issue_picker = Some(picker);
        self._project_issue_picker_events = Some(events);
        cx.notify();
        Ok(())
    }

    pub(crate) fn close_project_issue_picker(&mut self, cx: &mut Context<Self>) {
        if self.project_issue_picker.take().is_some() {
            self._project_issue_picker_events = None;
            self.linear_column.return_focus = true;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub(crate) fn project_issue_picker(&self) -> Option<Entity<ProjectIssuePicker>> {
        self.project_issue_picker.clone()
    }

    /// The picker overlay (a dimmed backdrop that closes it on click), or
    /// nothing. Closes itself once the item or Linear goes away.
    pub(super) fn render_project_issue_picker(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let picker = self.project_issue_picker.clone()?;
        let space_id = picker.read(cx).space_id.clone();
        let snapshot = self.outline_snapshot(&space_id, cx);
        let valid = picker.update(cx, |picker, _| {
            picker.set_snapshot(snapshot);
            picker.still_valid()
        });
        if !valid {
            self.project_issue_picker = None;
            self._project_issue_picker_events = None;
            return None;
        }
        let theme = Theme::of(cx).clone();
        Some(
            div()
                .id("linear-add-issues-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(64.0))
                .bg(theme.scrim())
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.close_project_issue_picker(cx)),
                )
                .child(
                    div()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(picker),
                )
                .into_any_element(),
        )
    }

    /// Overview width as last painted (`None` before the first paint).
    pub(super) fn overview_width(&self) -> Option<f32> {
        self.linear_column.overview_width.get()
    }

    /// Zero-size paint hook recording the Overview's own width (the
    /// narrow-window rule needs it); a change re-renders once.
    pub(super) fn linear_width_capture(&self) -> AnyElement {
        let cell = self.linear_column.overview_width.clone();
        gpui::canvas(
            move |bounds, window, _| {
                let width = f32::from(bounds.size.width);
                if cell.get().is_none_or(|old| (old - width).abs() >= 0.5) {
                    cell.set(Some(width));
                    window.refresh();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }

    /// Edge drag: the shown width plus the pointer's travel, clamped.
    pub(super) fn linear_resize_drag(&mut self, x: f32, cx: &mut Context<Self>) {
        let Some((start_x, start_w)) = self.linear_column.resize_from else {
            return;
        };
        let width = prefs::clamp_width(start_w + (x - start_x));
        if (width - self.linear_column.width).abs() >= 0.5 {
            self.linear_column.width = width;
            cx.notify();
        }
    }

    fn finish_linear_resize(&mut self, cx: &mut Context<Self>) {
        if self.linear_column.resize_from.take().is_some() {
            self.persist_linear_width(cx);
        }
    }

    pub(super) fn persist_linear_width(&self, cx: &Context<Self>) {
        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
            return;
        };
        let file = crate::overview_positions::UI_FLAGS_FILE;
        let mut flags = crate::overview_positions::load_string_set(&data_dir, file);
        prefs::set_width_flag(&mut flags, self.linear_column.width);
        self.save_string_set(file, &flags, cx);
    }

    /// The column for this frame, or `None` while closed. Creates the
    /// panel when the persisted state says open and a Space has Linear on;
    /// hides it (keeping that state) while none does. `fixed`: px the PR
    /// sidebar and the right slot take.
    pub(super) fn render_linear_column(
        &mut self,
        fixed: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<LinearSlot> {
        let spaces = self.linear_spaces(cx);
        let github_spaces = self.github_spaces(cx);
        if spaces.is_empty() {
            if self.linear_panel.take().is_some() {
                self._linear_panel_events = None;
            }
            self.linear_column.closing = None;
        }
        if github_spaces.is_empty() {
            self.drop_github_panel();
            self.linear_column.closing_github = None;
        } else if self.linear_column.open {
            // Restored open, or the PR review sync came to a Space: keep
            // the GitHub section fed (it refreshes once when created).
            self.sync_github_panel(None, false, cx);
        }
        if spaces.is_empty() && github_spaces.is_empty() {
            self.linear_column.tween = None;
        }
        if !spaces.is_empty() && self.linear_column.open && self.linear_panel.is_none() {
            // Restored open (a fresh Overview, or Linear came back on).
            let target = spaces[0].id.clone();
            let panel = self.ensure_linear_panel(target, cx);
            let selection = self.linear_selection(cx);
            let restored = spaces.clone();
            panel.update(cx, |panel, cx| {
                panel.set_data(restored, selection);
                panel.refresh(cx);
            });
        }
        if std::mem::take(&mut self.linear_column.return_focus) {
            self.focus_overview_body(window, cx);
        }
        let mut fraction = 1.0;
        if let Some(tween) = self.linear_column.tween {
            let (shown, done) = tween.fraction(Instant::now());
            if done {
                self.linear_column.tween = None;
                self.linear_column.closing = None;
                self.linear_column.closing_github = None;
            } else {
                fraction = shown;
                window.request_animation_frame();
            }
        }
        let closing = !self.column_shown();
        let panel = self
            .linear_panel
            .clone()
            .or_else(|| self.linear_column.closing.clone());
        let github = self
            .github_panel
            .clone()
            .or_else(|| self.linear_column.closing_github.clone());
        if panel.is_none() && github.is_none() {
            return None;
        }
        let selection = self.linear_selection(cx);
        if let Some(panel) = &panel {
            panel.update(cx, |panel, _| {
                panel.embedded = true;
                panel.set_data(spaces, selection);
            });
        }
        if !closing && std::mem::take(&mut self.linear_column.focus_search) {
            self.focus_column(window, cx);
        }
        let layout = linear_column(
            self.linear_column.overview_width.get(),
            self.linear_column.width,
            fixed,
        );
        let (target, overlay) = match layout {
            LinearColumn::Inline(width) => (width, false),
            LinearColumn::Overlay(width) => (width, true),
        };
        let shown = (target * fraction).max(0.0);
        self.linear_column.shown_width = target;
        let theme = Theme::of(cx).clone();
        let handle = (!overlay && !closing).then(|| self.linear_resize_handle(&theme, cx));
        let column = div()
            .id("linear-column")
            .debug_selector(|| "linear-column".into())
            .relative()
            .flex_none()
            .h_full()
            .w(px(shown))
            .overflow_hidden()
            .when(overlay, |el| {
                el.shadow_lg()
                    .occlude()
                    .border_r_1()
                    .border_color(theme.border)
            })
            .child(
                div()
                    .h_full()
                    .w(px(target))
                    .flex()
                    .flex_col()
                    .border_r_1()
                    .border_color(theme.border)
                    .bg(theme.surface)
                    .child(self.column_header(&theme, cx))
                    .when_some(panel, |el, panel| {
                        el.child(div().flex_1().min_h_0().child(panel))
                    })
                    .when_some(github, |el, github| {
                        el.child(
                            div()
                                .when(!github.read(cx).folded, |el| el.flex_1())
                                .min_h_0()
                                .flex()
                                .flex_col()
                                .border_t_1()
                                .border_color(theme.border)
                                .child(github),
                        )
                    }),
            )
            .children(handle)
            .into_any_element();
        Some(if overlay {
            LinearSlot::Overlay(column)
        } else {
            LinearSlot::Inline(column)
        })
    }

    /// The column's own header: "Inbox" and the close button (the toggle's
    /// hint).
    fn column_header(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(14.0))
            .pt(px(8.0))
            .child(
                div()
                    .flex_1()
                    .text_size(crate::typography::ui_rems(10.5))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text_faint)
                    .child("INBOX"),
            )
            .child(
                div()
                    .id("inbox-column-close")
                    .debug_selector(|| "inbox-column-close".into())
                    .flex_none()
                    .size(px(20.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(|el| el.bg(theme.element_hover))
                    .child(
                        crate::icons::icon(crate::icons::CLOSE)
                            .size(px(11.0))
                            .text_color(theme.text_muted),
                    )
                    .hint(
                        "Hide the Inbox column",
                        HintKey::Command(WorkOutlineCommand::OpenLinear),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.close_linear_panel(true, cx))),
            )
            .into_any_element()
    }

    fn linear_resize_handle(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let hover = theme.border_strong;
        let active = self.linear_column.resize_from.is_some();
        div()
            .id("linear-column-resize")
            .debug_selector(|| "linear-column-resize".into())
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(RESIZE_HANDLE_W))
            .cursor_col_resize()
            .when(active, |el| el.bg(hover.opacity(0.7)))
            .hover(move |style| style.bg(hover.opacity(0.7)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                    this.linear_column.resize_from =
                        Some((f32::from(event.position.x), this.linear_column.shown_width));
                    cx.notify();
                }),
            )
            .on_drag(LinearPanelResize, |_, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| LinearResizeGhost)
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseUpEvent, _, cx| {
                    if event.click_count == 2 {
                        this.linear_column.resize_from = None;
                        this.linear_column.width = prefs::PANEL_DEFAULT_W;
                        this.persist_linear_width(cx);
                        cx.notify();
                    } else {
                        this.finish_linear_resize(cx);
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_linear_resize(cx)),
            )
            .into_any_element()
    }

    /// The Outline toolbar's Linear button (only while some Space has
    /// Linear on): the same toggle as the hotkey.
    pub(super) fn linear_toolbar_button(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.linear_spaces(cx).is_empty() && self.github_spaces(cx).is_empty() {
            return None;
        }
        let active = self.column_shown();
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
                .child("Inbox")
                .hint(
                    if active {
                        "Hide the Inbox column"
                    } else {
                        "Show your Linear issues and GitHub PRs beside the Outline"
                    },
                    HintKey::Command(WorkOutlineCommand::OpenLinear),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.column_shown() {
                        this.close_linear_panel(true, cx);
                    } else if let Err(notice) = this.open_linear_panel(None, cx) {
                        this.outline
                            .update(cx, |outline, cx| outline.show_notice(notice, cx));
                    } else {
                        this.focus_column(window, cx);
                    }
                }))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_keeps_the_body_minimum_then_shrinks_then_overlays() {
        let min = prefs::PANEL_MIN_W;
        // Unknown width (first frame): preferred.
        assert_eq!(linear_column(None, 340.0, 0.0), LinearColumn::Inline(340.0));
        // Room for everything.
        assert_eq!(
            linear_column(Some(1600.0), 340.0, 540.0),
            LinearColumn::Inline(340.0)
        );
        // Shrinks so the body keeps its minimum.
        let total = 540.0 + OVERVIEW_BODY_MIN_W + 300.0;
        assert_eq!(
            linear_column(Some(total), 340.0, 540.0),
            LinearColumn::Inline(300.0)
        );
        // Down to the minimum exactly.
        let total = 540.0 + OVERVIEW_BODY_MIN_W + min;
        assert_eq!(
            linear_column(Some(total), 340.0, 540.0),
            LinearColumn::Inline(min)
        );
        // Narrower still: overlay at the minimum.
        assert_eq!(
            linear_column(Some(total - 1.0), 340.0, 540.0),
            LinearColumn::Overlay(min)
        );
        assert_eq!(
            linear_column(Some(200.0), 340.0, 0.0),
            LinearColumn::Overlay(min)
        );
        // The preferred width is clamped first.
        assert_eq!(
            linear_column(Some(4000.0), 9000.0, 0.0),
            LinearColumn::Inline(prefs::PANEL_MAX_W)
        );
    }

    #[test]
    fn tween_runs_open_and_close_to_completion() {
        let start = Instant::now();
        let open = LinearTween {
            opening: true,
            started: start,
        };
        assert_eq!(open.fraction(start), (0.0, false));
        let end = start + crate::motion::RESIZE.total();
        assert_eq!(open.fraction(end), (1.0, true));
        let close = LinearTween {
            opening: false,
            started: start,
        };
        assert_eq!(close.fraction(start), (1.0, false));
        assert_eq!(close.fraction(end), (0.0, true));
    }
}
