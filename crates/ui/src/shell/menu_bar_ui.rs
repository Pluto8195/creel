//! The macOS menu bar item's Shell side: gathers the inputs for
//! `menu_bar::build_menu`, pushes a changed menu to AppKit, refreshes account
//! usage when the menu opens, and runs the clicks that need the Shell.
//!
//! It piggybacks on the status light's signals (state changes and the 1 s
//! heartbeat); the only network traffic is one usage probe per account list
//! at most every 60 s, started when the menu opens (the engine's own cache is
//! the same 60 s, and tokens are never refreshed).
use super::*;

use crate::menu_bar::{self, MenuAction, MenuEvent, MenuInput};

/// The menu is rebuilt at most this often from state-change signals.
const CHECK_EVERY: std::time::Duration = std::time::Duration::from_millis(900);

#[derive(Default)]
pub(super) struct MenuBarUi {
    /// What AppKit currently shows; `None` while the item is hidden.
    last: Option<menu_bar::MenuView>,
    checked_at: Option<std::time::Instant>,
    /// `None` until the first probe answers.
    usage: Option<Vec<menu_bar::UsageAccount>>,
    fetched_at: Option<std::time::Instant>,
    task: Option<gpui::Task<()>>,
}

impl Shell {
    /// Keep the menu bar item in step with the app. Cheap when nothing
    /// changed: the view is compared and AppKit is only touched on a change.
    pub(super) fn sync_menu_bar(&mut self, force: bool, cx: &mut Context<Self>) {
        if !self.settings.show_menu_bar_item {
            if self.menu_bar.last.take().is_some() {
                menu_bar::apply(None);
            }
            return;
        }
        let now = std::time::Instant::now();
        if !force
            && self
                .menu_bar
                .checked_at
                .is_some_and(|at| now.duration_since(at) < CHECK_EVERY)
        {
            return;
        }
        self.menu_bar.checked_at = Some(now);
        if self.menu_bar.fetched_at.is_none() {
            self.refresh_menu_usage(cx);
        }
        let utc_now = Utc::now();
        let light = crate::status_light::summarize_state(self.state.read(cx), utc_now).light;
        let snapshot = self.running_snapshot(cx);
        let interrupted = self.interrupted_block(cx);
        let view = menu_bar::build_menu(
            &MenuInput {
                snapshot: &snapshot,
                interrupted: interrupted.as_ref(),
                usage: self.menu_bar.usage.as_deref(),
                pending: self.pending_build(),
                light,
                now: utc_now,
            },
            &chrono::Local,
        );
        if self.menu_bar.last.as_ref() != Some(&view) {
            menu_bar::apply(Some(&view));
            self.menu_bar.last = Some(view);
        }
    }

    /// Probe usage for the Default logins and the managed accounts, unless a
    /// probe ran within the last minute or one is in flight.
    fn refresh_menu_usage(&mut self, cx: &mut Context<Self>) {
        let fresh = self.menu_bar.fetched_at.is_some_and(|at| {
            at.elapsed() < std::time::Duration::from_secs(menu_bar::USAGE_REFRESH_SECS)
        });
        // The engine is absent (or scripted) under test.
        if cfg!(test) || fresh || self.menu_bar.task.is_some() {
            return;
        }
        let rpc = self.state.read(cx).accounts_rpc(None);
        self.menu_bar.task = Some(cx.spawn(async move |this, cx| {
            // The global list first: a forced list clears the engine's whole
            // usage cache, and the managed list then refills its own entries.
            let global = rpc
                .call(
                    methods::LIST_AGENT_ACCOUNTS,
                    serde_json::json!({ "forceUsage": true }),
                )
                .await
                .ok()
                .and_then(|value| {
                    serde_json::from_value::<mast_proto::AgentAccountsSnapshot>(value).ok()
                });
            let managed = rpc
                .call(
                    methods::LIST_MANAGED_ACCOUNTS,
                    crate::managed_accounts::list_params(true),
                )
                .await
                .ok()
                .and_then(|value| {
                    serde_json::from_value::<mast_proto::ManagedAccountsSnapshot>(value).ok()
                });
            this.update(cx, |shell, cx| {
                shell.menu_bar.task = None;
                shell.menu_bar.fetched_at = Some(std::time::Instant::now());
                let accounts = menu_bar::usage_accounts(global.as_ref(), managed.as_ref());
                // A failed probe keeps what was shown; the first one shows nothing.
                if global.is_some() || managed.is_some() || shell.menu_bar.usage.is_none() {
                    shell.menu_bar.usage = Some(accounts);
                }
                shell.sync_menu_bar(true, cx);
            })
            .ok();
        }));
    }

    /// Clicks that need the Shell (the rest are handled at the App level).
    pub(crate) fn handle_menu_bar_event(&mut self, event: MenuEvent, cx: &mut Context<Self>) {
        match event {
            MenuEvent::WillOpen => {
                self.sync_menu_bar(true, cx);
                self.refresh_menu_usage(cx);
            }
            MenuEvent::Action(MenuAction::RestartWhenIdle) => {
                if self.pending_build().is_some() {
                    self.arm_restart_when_idle(cx);
                    self.sync_menu_bar(true, cx);
                }
            }
            MenuEvent::Action(MenuAction::ResumeAll(chat_ids)) => {
                self.handle_interrupted(chat_ids, true, cx);
                self.sync_menu_bar(true, cx);
            }
            MenuEvent::Action(_) => {}
        }
    }
}
