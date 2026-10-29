//! The running-chats popover: the status lamp in the titlebar (and ⌥⌘R) opens
//! a keyboard-driven list of everything mid-turn, waiting on the user or
//! failed. Grouping and ordering are the pure `running_chats` module; this
//! gathers the inputs from state and renders.
//!
//! Keys while it is open: ↑/↓ move, ↩ opens the chat, ⌘↩ opens it in the
//! Overview's side pane, I interrupts, →/← unfold/fold, Esc closes.
use super::*;
use gpui::{Div, ScrollHandle, point};

use crate::build_watch::{self, BuildStamp, Pending, RestartAction, RestartPlan, RestartState};
use crate::running_chats::{self, Entry, Input, Row, RunState, Section, SectionKind, Snapshot};

/// Fixed shortcut, listed in the `?` reference.
pub(crate) const TOGGLE_RUNNING_CHATS_COMBO: &str = "mod-alt-r";
const CARD_WIDTH: f32 = 440.0;
const LIST_MAX_HEIGHT: f32 = 460.0;

/// Shell-local popover state.
pub(super) struct RunningUi {
    pub(super) open: bool,
    position: Point<Pixels>,
    selected: Option<String>,
    toggled: running_chats::Toggled,
    focus: FocusHandle,
    scroll: ScrollHandle,
    // --- pending build (see `build_watch`) ---
    /// Whether this install can be replaced and relaunched (a macOS bundle).
    restart_enabled: bool,
    exe: Option<PathBuf>,
    launch: Option<BuildStamp>,
    pending: Option<Pending>,
    plan: RestartPlan,
    ticks: u32,
    changes_task: Option<gpui::Task<()>>,
    /// The chat the last I-key interrupt targeted (the engine is absent in tests).
    #[cfg(test)]
    interrupted: Option<String>,
}

/// How often the installed executable is stat'ed, in heartbeat ticks.
const BUILD_CHECK_EVERY_TICKS: u32 = 5;

impl RunningUi {
    pub(super) fn new(restart_enabled: bool, cx: &mut Context<Shell>) -> Self {
        let exe = std::env::current_exe().ok();
        Self {
            open: false,
            position: point(px(12.0), px(Theme::TITLEBAR_HEIGHT + 4.0)),
            selected: None,
            toggled: Default::default(),
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            restart_enabled,
            launch: exe.as_deref().and_then(BuildStamp::of),
            exe,
            pending: None,
            plan: RestartPlan::default(),
            ticks: 0,
            changes_task: None,
            #[cfg(test)]
            interrupted: None,
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Relaunches requested under test (the real relaunch never runs).
    pub(super) static RELAUNCHES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn run_state(indicator: crate::state::Indicator, background: u32) -> Option<RunState> {
    use crate::state::Indicator;
    match indicator {
        Indicator::Working => Some(RunState::Working),
        Indicator::AwaitingInput => Some(RunState::Awaiting),
        Indicator::Errored => Some(RunState::Errored),
        Indicator::None if background > 0 => Some(RunState::IdleBackground),
        Indicator::None => None,
    }
}

impl Shell {
    /// Every non-archived chat that is running, waiting or errored.
    fn running_inputs(&self, cx: &App) -> Vec<Input> {
        let now = Utc::now();
        let state = self.state.read(cx);
        let mut inputs = Vec::new();
        for chat in state.chats.iter().filter(|chat| !chat.archived) {
            let background = 0; // not tracked at session level yet
            let Some(run) = run_state(state.indicator_for(&chat.id, now), background) else {
                continue;
            };
            let session = state.session_for(&chat.id);
            let facts = self.chat_item_facts(&chat.id, cx);
            let item_path = facts
                .as_ref()
                .and_then(|facts| facts.item.as_ref())
                .map(|item| {
                    item.path
                        .iter()
                        .map(|segment| segment.title.as_str())
                        .collect::<Vec<_>>()
                        .join(" / ")
                })
                .filter(|path| !path.is_empty());
            inputs.push(Input {
                chat_id: chat.id.clone(),
                parent_id: chat.parent_chat_id.clone(),
                space_id: chat.space_id.clone(),
                title: facts
                    .as_ref()
                    .map(|facts| facts.title.clone())
                    .unwrap_or_else(|| "New session".to_owned()),
                state: run,
                since: session.and_then(|s| s.started_at.or(Some(s.updated_at))),
                activity: chat
                    .last_message_preview
                    .as_deref()
                    .map(crate::transcript::single_line)
                    .unwrap_or_default(),
                item_path,
                account: facts.as_ref().and_then(|facts| facts.account.clone()),
                model: facts.as_ref().and_then(|facts| facts.model.label.clone()),
                background,
            });
        }
        inputs
    }

    pub(super) fn running_snapshot(&self, cx: &App) -> Snapshot {
        let state = self.state.read(cx);
        let spaces: Vec<(String, String)> = state
            .spaces
            .iter()
            .map(|space| {
                let name = space.name.clone().unwrap_or_else(|| {
                    std::path::Path::new(&space.path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| space.path.clone())
                });
                (space.id.clone(), name)
            })
            .collect();
        running_chats::snapshot(
            &self.running_inputs(cx),
            &spaces,
            &self.running.toggled,
            Utc::now(),
        )
    }

    pub(crate) fn toggle_running_chats(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running.open {
            self.close_running_chats(window, cx);
            return;
        }
        let snapshot = self.running_snapshot(cx);
        self.running.selected = running_chats::reconcile(&snapshot, None);
        self.running.open = true;
        window.focus(&self.running.focus, cx);
        cx.notify();
    }

    pub(super) fn close_running_chats(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.running.open) {
            return;
        }
        window.focus(&self.composer.focus_handle(cx), cx);
        cx.notify();
    }

    fn selected_running_row(&self, cx: &App) -> Option<Row> {
        let snapshot = self.running_snapshot(cx);
        match snapshot.find(self.running.selected.as_deref()?)? {
            Entry::Row(row) => Some(row.clone()),
            Entry::Fold { .. } => None,
        }
    }

    fn open_running_chat(&mut self, chat_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.running.open = false;
        self.open_chat(chat_id, cx);
        let _ = window;
    }

    fn open_running_chat_in_pane(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.running.open = false;
        let page = self.ensure_overview_page(cx);
        self.open_overview(cx);
        page.update(cx, |overview, cx| overview.open_chat_panel(chat_id, cx));
    }

    fn toggle_running_fold(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.running.toggled.remove(key) {
            self.running.toggled.insert(key.to_owned());
        }
        cx.notify();
    }

    /// ↩ on a row opens it; on a fold line it folds or unfolds.
    fn activate_running(&mut self, new_pane: bool, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.running_snapshot(cx);
        let Some(entry) = self
            .running
            .selected
            .as_deref()
            .and_then(|key| snapshot.find(key))
            .cloned()
        else {
            return;
        };
        match entry {
            Entry::Row(row) if new_pane => self.open_running_chat_in_pane(row.chat_id, cx),
            Entry::Row(row) => self.open_running_chat(row.chat_id, window, cx),
            Entry::Fold { key, .. } => self.toggle_running_fold(&key, cx),
        }
    }

    fn interrupt_running_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(row) = self.selected_running_row(cx) {
            #[cfg(test)]
            {
                self.running.interrupted = Some(row.chat_id.clone());
            }
            self.composer
                .update(cx, |composer, cx| composer.interrupt_chat(row.chat_id, cx));
            cx.notify();
        }
    }

    /// → opens the fold under the highlight (or the coordinator's own); ← closes it.
    fn fold_running_selected(&mut self, open: bool, cx: &mut Context<Self>) {
        let snapshot = self.running_snapshot(cx);
        let Some(key) = self.running.selected.clone() else {
            return;
        };
        let fold_key = match snapshot.find(&key) {
            Some(Entry::Fold { key, open: now_open, .. }) if *now_open != open => key.clone(),
            Some(Entry::Row(row)) => {
                let folded_below = snapshot
                    .entries()
                    .any(|e| matches!(e, Entry::Fold { key, .. } if *key == row.chat_id));
                if open == folded_below {
                    row.chat_id.clone()
                } else {
                    return;
                }
            }
            _ => return,
        };
        self.toggle_running_fold(&fold_key, cx);
    }

    pub(super) fn handle_running_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        match key {
            "escape" => self.close_running_chats(window, cx),
            "up" | "down" => {
                let snapshot = self.running_snapshot(cx);
                self.running.selected = running_chats::step(
                    &snapshot,
                    self.running.selected.as_deref(),
                    if key == "up" { -1 } else { 1 },
                );
            }
            "enter" => self.activate_running(modifiers.platform, window, cx),
            "right" => self.fold_running_selected(true, cx),
            "left" => self.fold_running_selected(false, cx),
            "i" if !modifiers.platform && !modifiers.control && !modifiers.alt => {
                self.interrupt_running_selected(cx)
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    // ----------------------------------------------------------------------
    // Pending build and restart
    // ----------------------------------------------------------------------

    /// Chats mid-turn right now (working, not waiting on the user).
    fn mid_turn_rows(&self, cx: &App) -> Vec<Row> {
        self.running_snapshot(cx)
            .entries()
            .filter_map(|entry| match entry {
                Entry::Row(row) if row.state == RunState::Working => Some(row.clone()),
                _ => None,
            })
            .collect()
    }

    /// The 1 s heartbeat: look for a newer installed build every few seconds
    /// and let an armed "restart when idle" fire.
    pub(super) fn running_tick(&mut self, cx: &mut Context<Self>) {
        if !self.running.restart_enabled {
            return;
        }
        self.running.ticks = self.running.ticks.wrapping_add(1);
        if self.running.ticks % BUILD_CHECK_EVERY_TICKS == 0 {
            let installed = self.running.exe.as_deref().and_then(BuildStamp::of);
            self.observe_installed(installed, cx);
        }
        if self.running.plan.is_armed() {
            let mid_turn = self.mid_turn_rows(cx).len();
            if self.running.plan.tick(mid_turn) == RestartAction::Relaunch {
                self.relaunch_now(cx);
            }
        }
    }

    /// Record what the installed executable looks like now. A stamp unlike the
    /// launch stamp is a pending build; matching again (rolled back) clears it.
    pub(super) fn observe_installed(&mut self, installed: Option<BuildStamp>, cx: &mut Context<Self>) {
        let Some(launch) = self.running.launch else {
            return;
        };
        if build_watch::is_newer_installed(&launch, installed.as_ref()) {
            let stamp = installed.expect("newer implies a stamp");
            if self.running.pending.as_ref().map(|p| p.installed) == Some(stamp) {
                return;
            }
            self.running.pending = Some(Pending {
                installed: stamp,
                changes: None,
            });
            self.load_pending_changes(stamp, cx);
            cx.notify();
        } else if installed.is_some() && self.running.pending.take().is_some() {
            self.running.plan.build_gone();
            self.running.changes_task = None;
            cx.notify();
        }
    }

    fn load_pending_changes(&mut self, stamp: BuildStamp, cx: &mut Context<Self>) {
        let (Some(exe), Some(running)) = (self.running.exe.clone(), build_watch::running_build())
        else {
            return;
        };
        self.running.changes_task = Some(cx.spawn(async move |this, cx| {
            let changes = cx
                .background_executor()
                .spawn(async move { build_watch::changes_since(running, &exe) })
                .await;
            this.update(cx, |shell, cx| {
                if let Some(pending) = shell.running.pending.as_mut()
                    && pending.installed == stamp
                    && pending.changes != changes
                {
                    pending.changes = changes;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn arm_restart_when_idle(&mut self, cx: &mut Context<Self>) {
        self.running.plan.arm();
        cx.notify();
    }

    fn request_restart_now(&mut self, cx: &mut Context<Self>) {
        let mid_turn = self.mid_turn_rows(cx).len();
        if self.running.plan.request_now(mid_turn) == RestartAction::Relaunch {
            self.relaunch_now(cx);
        }
        cx.notify();
    }

    fn confirm_restart(&mut self, cx: &mut Context<Self>) {
        if self.running.plan.confirm() == RestartAction::Relaunch {
            self.relaunch_now(cx);
        }
        cx.notify();
    }

    fn cancel_restart(&mut self, cx: &mut Context<Self>) {
        self.running.plan.cancel();
        cx.notify();
    }

    /// Save-guarded exit, then the detached relauncher and quit. Never
    /// relaunches under test.
    pub(super) fn relaunch_now(&mut self, cx: &mut Context<Self>) {
        if !self.prepare_exit(PendingExit::Relaunch, cx) {
            self.running.plan.cancel();
            return;
        }
        #[cfg(test)]
        {
            RELAUNCHES.with(|count| count.set(count.get() + 1));
            return;
        }
        #[cfg(not(test))]
        {
            if let mast_update::InstallKind::MacApp { bundle } = &self.install {
                mast_update::relaunch_app_after_exit(bundle);
                crate::app_menus::quit_after_save(cx);
            } else {
                self.running.plan.cancel();
            }
        }
    }

    // ----------------------------------------------------------------------
    // Rendering
    // ----------------------------------------------------------------------

    /// The titlebar lamp: the lit mark, one click from the list.
    pub(super) fn render_running_lamp(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mark = self.logo(theme.appearance).w(px(12.0)).h(px(17.3));
        let open = self.running.open;
        div()
            .id("running-lamp")
            .flex_none()
            .ml(px(TITLEBAR_GROUP_GAP))
            .size(px(24.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .cursor_pointer()
            .when(self.running.pending.is_some(), |el| {
                el.border_1().border_color(theme.accent)
            })
            .when(open, |el| el.bg(theme.glass_hover()))
            .hover(|el| el.bg(theme.glass_hover()))
            .occlude()
            .on_click(cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
                this.running.position = point(
                    (event.position().x - px(12.0)).max(px(8.0)),
                    event.position().y + px(18.0),
                );
                this.toggle_running_chats(window, cx);
            }))
            .child(mark)
            .into_any_element()
    }

    pub(super) fn render_running_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.running.open {
            return None;
        }
        let theme = Theme::of(cx).for_popup();
        let snapshot = self.running_snapshot(cx);
        self.running.selected =
            running_chats::reconcile(&snapshot, self.running.selected.as_deref());
        let _ = window;
        let selected = self.running.selected.clone();

        let mut list = div()
            .id("running-list")
            .flex()
            .flex_col()
            .max_h(px(LIST_MAX_HEIGHT))
            .overflow_y_scroll()
            .track_scroll(&self.running.scroll);
        if snapshot.sections.is_empty() {
            list = list.child(
                div()
                    .px(px(10.0))
                    .py(px(14.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from("All idle")),
            );
        }
        for section in &snapshot.sections {
            list = list.child(self.render_running_section(section, selected.as_deref(), &theme, cx));
        }
        let card = popover::popover_card(&theme)
            .w(px(CARD_WIDTH))
            .flex()
            .flex_col()
            .track_focus(&self.running.focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                this.handle_running_key(event, window, cx)
            }))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.close_running_chats(window, cx);
            }))
            .child(
                div()
                    .px(px(8.0))
                    .py(px(6.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text_muted)
                    .child(SharedString::from(snapshot.headline())),
            )
            .children(self.render_restart_banner(&snapshot, &theme, cx))
            .child(list)
            .child(running_key_hints(&theme));
        Some(popover::menu_at(
            "running-chats",
            self.running.position,
            card.into_any_element(),
            None,
        ))
    }

    /// "New build installed" with the restart controls. Only exists while a
    /// newer build is waiting; with none there is no restart UI at all.
    fn render_restart_banner(
        &self,
        snapshot: &Snapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let pending = self.running.pending.as_ref()?;
        let mid_turn: Vec<&Row> = snapshot
            .entries()
            .filter_map(|entry| match entry {
                Entry::Row(row) if row.state == RunState::Working => Some(row),
                _ => None,
            })
            .collect();
        let clock = |ms: i64| {
            chrono::DateTime::from_timestamp_millis(ms)
                .map(|at| at.with_timezone(&chrono::Local).format("%H:%M").to_string())
                .unwrap_or_default()
        };
        let summary = match pending.changes.as_deref() {
            Some(changes) if !changes.is_empty() => {
                let shown: Vec<&str> = changes.iter().take(6).map(|c| c.label.as_str()).collect();
                let more = changes.len().saturating_sub(shown.len());
                format!(
                    "Running the {} build. Missing: {}{} ({} change{}).",
                    self.running.launch.map(|l| clock(l.mtime_ms)).unwrap_or_default(),
                    shown.join(", "),
                    if more > 0 { format!(" +{more} more") } else { String::new() },
                    changes.len(),
                    if changes.len() == 1 { "" } else { "s" },
                )
            }
            _ => format!(
                "Running the {} build.",
                self.running.launch.map(|l| clock(l.mtime_ms)).unwrap_or_default()
            ),
        };
        let title = match pending.changes.as_deref() {
            Some(changes) if !changes.is_empty() => format!(
                "New build installed \u{b7} {} change{}",
                changes.len(),
                if changes.len() == 1 { "" } else { "s" }
            ),
            _ => "New build installed".to_owned(),
        };
        let mut banner = div()
            .mx(px(4.0))
            .mb(px(4.0))
            .px(px(10.0))
            .py(px(8.0))
            .gap(px(6.0))
            .flex()
            .flex_col()
            .rounded(px(popover::MENU_ITEM_RADIUS))
            .border_1()
            .border_color(theme.accent)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(SharedString::from(title)),
                    )
                    .child(
                        div()
                            .text_color(theme.text_muted)
                            .child(SharedString::from(format!(
                                "installed {}",
                                clock(pending.installed.mtime_ms)
                            ))),
                    ),
            )
            .child(
                div()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(summary)),
            );
        let buttons = div().flex().flex_row().gap(px(6.0));
        banner = match self.running.plan.state {
            RestartState::Confirming => banner
                .child(
                    div()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(SharedString::from(format!(
                            "Restart now? {} chat{} mid-turn will be interrupted.",
                            mid_turn.len(),
                            if mid_turn.len() == 1 { " is" } else { "s are" }
                        ))),
                )
                .children(mid_turn.iter().take(6).map(|row| {
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(8.0))
                        .text_size(crate::typography::ui_rems(12.0))
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .child(SharedString::from(row.title.clone())),
                        )
                        .child(
                            div()
                                .text_color(theme.text_muted)
                                .child(SharedString::from(row.elapsed.clone())),
                        )
                }))
                .child(
                    buttons
                        .child(running_button("restart-anyway", "Restart anyway", false, theme).on_click(
                            cx.listener(|this, _, _, cx| this.confirm_restart(cx)),
                        ))
                        .child(running_button("restart-when-idle", "Restart when idle", true, theme).on_click(
                            cx.listener(|this, _, _, cx| this.arm_restart_when_idle(cx)),
                        ))
                        .child(running_button("restart-wait", "Wait", false, theme).on_click(
                            cx.listener(|this, _, _, cx| this.cancel_restart(cx)),
                        )),
                ),
            RestartState::WhenIdle { .. } => banner
                .child(
                    div()
                        .child(SharedString::from(format!(
                            "Restarting when idle \u{b7} {} working",
                            mid_turn.len()
                        ))),
                )
                .child(
                    buttons
                        .child(running_button("restart-cancel", "Cancel", false, theme).on_click(
                            cx.listener(|this, _, _, cx| this.cancel_restart(cx)),
                        ))
                        .child(running_button("restart-now", "Restart now\u{2026}", false, theme).on_click(
                            cx.listener(|this, _, _, cx| this.request_restart_now(cx)),
                        )),
                ),
            RestartState::Relaunching => banner.child(
                div()
                    .text_color(theme.text_muted)
                    .child(SharedString::from("Restarting\u{2026}")),
            ),
            RestartState::Idle if mid_turn.is_empty() => banner.child(
                buttons.child(running_button("restart-now", "Restart", true, theme).on_click(
                    cx.listener(|this, _, _, cx| this.request_restart_now(cx)),
                )),
            ),
            RestartState::Idle => banner.child(
                buttons
                    .child(running_button("restart-when-idle", "Restart when idle", true, theme).on_click(
                        cx.listener(|this, _, _, cx| this.arm_restart_when_idle(cx)),
                    ))
                    .child(running_button("restart-now", "Restart now\u{2026}", false, theme).on_click(
                        cx.listener(|this, _, _, cx| this.request_restart_now(cx)),
                    )),
            ),
        };
        Some(banner.into_any_element())
    }

    fn render_running_section(
        &self,
        section: &Section,
        selected: Option<&str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = format!("{} \u{b7} {}", section.label, section.count);
        let urgent = section.kind == SectionKind::NeedsYou;
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .px(px(8.0))
                    .pt(px(8.0))
                    .pb(px(2.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(if urgent { theme.warning } else { theme.text_faint })
                    .child(SharedString::from(label)),
            )
            .children(section.entries.iter().map(|entry| {
                let active = selected == Some(entry.key().as_str());
                match entry {
                    Entry::Row(row) => self.render_running_row(row, active, theme, cx),
                    Entry::Fold { key, hidden, open } => {
                        let toggle = key.clone();
                        popover::menu_row_nav(theme, false, active, format!("running-fold-{key}"))
                            .id(SharedString::from(format!("running-fold-{key}")))
                            .pl(px(24.0))
                            .text_color(theme.text_muted)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_running_fold(&toggle, cx)
                            }))
                            .child(SharedString::from(format!(
                                "{} {hidden} more",
                                if *open { "\u{25be}" } else { "\u{25b8}" }
                            )))
                    }
                    .into_any_element(),
                }
            }))
            .into_any_element()
    }

    fn render_running_row(
        &self,
        row: &Row,
        active: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let dot = match row.state {
            RunState::Awaiting => theme.warning,
            RunState::Errored => theme.danger,
            RunState::Working => theme.busy,
            RunState::IdleBackground => theme.text_faint,
        };
        let meta = [row.item_path.clone(), row.account.clone(), row.model.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" \u{b7} ");
        let activity = if row.state == RunState::IdleBackground {
            format!("idle \u{b7} {} background running", row.background)
        } else {
            row.activity.clone()
        };
        let chat_id = row.chat_id.clone();
        let pane_id = row.chat_id.clone();
        popover::menu_row_nav(theme, false, active, format!("running-row-{}", row.chat_id))
            .id(SharedString::from(format!("running-row-{}", row.chat_id)))
            .items_start()
            .pl(px(8.0 + 16.0 * f32::from(row.depth)))
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                if event.modifiers().platform {
                    this.open_running_chat_in_pane(pane_id.clone(), cx);
                } else {
                    this.open_running_chat(chat_id.clone(), window, cx);
                }
            }))
            .child(
                div()
                    .mt(px(5.0))
                    .size(px(7.0))
                    .flex_none()
                    .rounded_full()
                    .bg(dot),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(SharedString::from(row.title.clone())),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(row.elapsed.clone())),
                            ),
                    )
                    .children((!activity.is_empty()).then(|| {
                        div()
                            .truncate()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(activity))
                    }))
                    .children((!meta.is_empty()).then(|| {
                        div()
                            .truncate()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(meta))
                    })),
            )
            .into_any_element()
    }
}

fn running_key_hints(theme: &Theme) -> AnyElement {
    let hint = |keys: &'static str, what: &'static str| {
        div()
            .flex()
            .flex_row()
            .gap(px(4.0))
            .child(div().text_color(theme.text_muted).child(keys))
            .child(div().child(what))
    };
    div()
        .flex()
        .flex_row()
        .gap(px(12.0))
        .px(px(8.0))
        .pt(px(6.0))
        .pb(px(2.0))
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(theme.text_faint)
        .child(hint("\u{2191}\u{2193}", "move"))
        .child(hint("\u{21a9}", "open"))
        .child(hint("\u{2318}\u{21a9}", "in pane"))
        .child(hint("I", "interrupt"))
        .into_any_element()
}

fn running_button(id: &'static str, label: &'static str, primary: bool, theme: &Theme) -> gpui::Stateful<Div> {
    let button = div()
        .id(id)
        .px(px(10.0))
        .py(px(4.0))
        .rounded(px(6.0))
        .cursor_pointer()
        .border_1()
        .text_size(crate::typography::ui_rems(12.0));
    if primary {
        button
            .border_color(theme.accent)
            .bg(theme.accent)
            .text_color(theme.on_accent)
    } else {
        button
            .border_color(theme.border_strong)
            .hover(|el| el.bg(theme.glass_hover()))
    }
    .child(SharedString::from(label))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_watch::BuildStamp;
    use mast_proto::{SessionStatus, WorkspaceScope};

    fn test_shell<'a>(
        cx: &'a mut gpui::TestAppContext,
        dir: &std::path::Path,
    ) -> (Entity<Shell>, &'a mut gpui::VisualTestContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let data_dir = dir.to_path_buf();
        cx.add_window_view(move |_, cx| {
            let state = cx.new(|_| AppState::new());
            state.update(cx, |state, _| {
                state.connection = crate::state::ConnectionStatus::Ready;
                state.workspace_scope = Some(WorkspaceScope::Local);
                state.auth = Some(mast_proto::AuthState::SignedOut);
            });
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: data_dir.clone(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: mast_proto::HarnessId::Mock,
                },
                cx,
            )
        })
    }

    fn chat(id: &str, parent: Option<&str>) -> mast_proto::Chat {
        mast_proto::Chat {
            id: id.into(),
            device_id: "d".into(),
            title: Some(format!("Chat {id}")),
            archived: false,
            cwd: None,
            branch: None,
            checkout_id: None,
            source_context: None,
            config: None,
            last_message_preview: Some("cargo test".into()),
            last_message_at: None,
            created_at: chrono::Utc::now(),
            harness_session_id: None,
            harness_session_cwd: None,
            actual_model: None,
            parent_chat_id: parent.map(str::to_owned),
            space_id: None,
            last_seen_at: None,
            room_gen: None,
            linked_pr_url: None,
            linked_pr_source: None,
            linked_ticket_id: None,
            linked_ticket_source: None,
        }
    }

    fn session(id: &str, status: SessionStatus, mins_ago: i64) -> mast_proto::Session {
        mast_proto::Session {
            last_completed_turn: None,
            chat_id: id.into(),
            device_id: "d".into(),
            status,
            started_at: Some(chrono::Utc::now() - chrono::Duration::minutes(mins_ago)),
            updated_at: chrono::Utc::now(),
        }
    }

    /// `n` chats: chat 0 awaits input, the rest work, chats 1.. hang under chat 1.
    fn seed(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext, n: usize) {
        let state = shell.read_with(cx, |shell, _| shell.state.clone());
        state.update(cx, |state, _| {
            state.chats = (0..n)
                .map(|i| chat(&format!("c{i}"), (i > 1).then_some("c1")))
                .collect();
            state.sessions = (0..n)
                .map(|i| {
                    let status = if i == 0 {
                        SessionStatus::AwaitingInput
                    } else {
                        SessionStatus::Working
                    };
                    session(&format!("c{i}"), status, 5 + i as i64)
                })
                .collect();
        });
    }

    fn key(name: &str) -> gpui::KeyDownEvent {
        gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse(name).unwrap(),
            is_held: false,
            prefer_character_input: false,
        }
    }

    fn press(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext, name: &str) {
        shell.update_in(cx, |shell, window, cx| {
            shell.handle_running_key(&key(name), window, cx)
        });
    }

    fn selected(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) -> Option<String> {
        shell.read_with(cx, |shell, _| shell.running.selected.clone())
    }

    fn redraw(cx: &mut gpui::VisualTestContext) {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
    }

    #[gpui::test]
    fn zero_chats_opens_an_empty_list_with_no_restart_ui(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        shell.update_in(cx, |shell, window, cx| shell.toggle_running_chats(window, cx));
        redraw(cx);
        shell.read_with(cx, |shell, cx| {
            assert!(shell.running.open);
            let snapshot = shell.running_snapshot(cx);
            assert_eq!(snapshot.headline(), "Nothing running");
            assert_eq!(shell.running.selected, None);
            assert!(shell.running.pending.is_none(), "no pending build, no banner");
        });
        press(&shell, cx, "down");
        press(&shell, cx, "enter");
        press(&shell, cx, "i");
        press(&shell, cx, "escape");
        shell.read_with(cx, |shell, _| assert!(!shell.running.open));
    }

    #[gpui::test]
    fn three_chats_group_navigate_open_and_interrupt(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        seed(&shell, cx, 3);
        shell.update_in(cx, |shell, window, cx| shell.toggle_running_chats(window, cx));
        redraw(cx);
        shell.read_with(cx, |shell, cx| {
            let snapshot = shell.running_snapshot(cx);
            assert_eq!(snapshot.headline(), "3 running \u{b7} 1 need you");
            assert_eq!(snapshot.sections[0].kind, SectionKind::NeedsYou);
            // The awaiting chat is first and selected; the rest follow in a
            // Space-less group with the child under its coordinator.
            assert_eq!(shell.running.selected.as_deref(), Some("c0"));
        });
        press(&shell, cx, "down");
        assert_eq!(selected(&shell, cx).as_deref(), Some("c1"));
        press(&shell, cx, "down");
        assert_eq!(selected(&shell, cx).as_deref(), Some("c2"));
        press(&shell, cx, "down");
        assert_eq!(selected(&shell, cx).as_deref(), Some("c0"), "wraps");
        press(&shell, cx, "up");
        assert_eq!(selected(&shell, cx).as_deref(), Some("c2"), "wraps back");

        press(&shell, cx, "i");
        let interrupted = shell.read_with(cx, |shell, _| shell.running.interrupted.clone());
        assert_eq!(interrupted.as_deref(), Some("c2"), "I interrupts the highlighted chat");

        press(&shell, cx, "up");
        press(&shell, cx, "enter");
        shell.read_with(cx, |shell, cx| {
            assert!(!shell.running.open, "opening closes the popover");
            assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("c1"));
        });
    }

    #[gpui::test]
    fn fifteen_chats_fold_and_unfold_from_the_keyboard(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        seed(&shell, cx, 15);
        shell.update_in(cx, |shell, window, cx| shell.toggle_running_chats(window, cx));
        redraw(cx);
        let count = |shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext| {
            shell.read_with(cx, |shell, cx| shell.running_snapshot(cx).entries().count())
        };
        // c0 (needs you), c1 with 13 children folded behind one line.
        assert_eq!(count(&shell, cx), 3);
        press(&shell, cx, "down"); // c1
        press(&shell, cx, "right");
        assert_eq!(count(&shell, cx), 15, "→ unfolds the coordinator's children");
        press(&shell, cx, "left");
        assert_eq!(count(&shell, cx), 3, "← folds them again");
        press(&shell, cx, "down"); // the fold line
        press(&shell, cx, "enter");
        assert_eq!(count(&shell, cx), 15, "↩ on the fold line toggles it");
        redraw(cx);
    }

    #[gpui::test]
    fn a_newer_installed_build_shows_restart_controls_and_nothing_else_does(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        seed(&shell, cx, 3);
        let launch = BuildStamp { inode: 1, mtime_ms: 1_000, len: 10 };
        shell.update(cx, |shell, cx| {
            shell.running.launch = Some(launch);
            shell.observe_installed(Some(launch), cx);
            assert!(shell.running.pending.is_none(), "same build: no pending");
            shell.observe_installed(None, cx);
            assert!(shell.running.pending.is_none(), "unreadable: no pending");
        });
        let newer = BuildStamp { inode: 2, mtime_ms: 2_000, len: 11 };
        shell.update(cx, |shell, cx| shell.observe_installed(Some(newer), cx));
        shell.update_in(cx, |shell, window, cx| shell.toggle_running_chats(window, cx));
        redraw(cx);
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.running.pending.as_ref().map(|p| p.installed), Some(newer));
        });
        // Rolled back to the launch build: the banner and any plan go away.
        shell.update(cx, |shell, cx| {
            shell.running.plan.arm();
            shell.observe_installed(Some(launch), cx);
            assert!(shell.running.pending.is_none());
            assert_eq!(shell.running.plan.state, RestartState::Idle);
        });
        redraw(cx);
    }

    #[gpui::test]
    fn restart_now_confirms_when_chats_are_mid_turn_and_never_really_relaunches(
        cx: &mut gpui::TestAppContext,
    ) {
        RELAUNCHES.with(|count| count.set(0));
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        seed(&shell, cx, 3);
        shell.update(cx, |shell, cx| shell.request_restart_now(cx));
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.running.plan.state, RestartState::Confirming);
        });
        assert_eq!(RELAUNCHES.with(|count| count.get()), 0);
        shell.update(cx, |shell, cx| shell.cancel_restart(cx));
        shell.update(cx, |shell, cx| {
            shell.request_restart_now(cx);
            shell.confirm_restart(cx);
        });
        assert_eq!(RELAUNCHES.with(|count| count.get()), 1, "Restart anyway");
    }

    #[gpui::test]
    fn restart_when_idle_waits_for_the_last_turn_to_end(cx: &mut gpui::TestAppContext) {
        RELAUNCHES.with(|count| count.set(0));
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        seed(&shell, cx, 3);
        shell.update(cx, |shell, cx| {
            shell.running.restart_enabled = true;
            shell.arm_restart_when_idle(cx);
            shell.running_tick(cx);
            shell.running_tick(cx);
        });
        assert_eq!(RELAUNCHES.with(|count| count.get()), 0, "chats still working");
        // Every chat finishes.
        let state = shell.read_with(cx, |shell, _| shell.state.clone());
        state.update(cx, |state, _| {
            for session in &mut state.sessions {
                session.status = SessionStatus::Idle;
            }
        });
        shell.update(cx, |shell, cx| shell.running_tick(cx));
        assert_eq!(RELAUNCHES.with(|count| count.get()), 0, "one quiet tick is not enough");
        shell.update(cx, |shell, cx| shell.running_tick(cx));
        assert_eq!(RELAUNCHES.with(|count| count.get()), 1);
    }

    #[gpui::test]
    fn restart_with_nothing_mid_turn_is_one_click(cx: &mut gpui::TestAppContext) {
        RELAUNCHES.with(|count| count.set(0));
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        shell.update(cx, |shell, cx| shell.request_restart_now(cx));
        assert_eq!(RELAUNCHES.with(|count| count.get()), 1);
    }
}
