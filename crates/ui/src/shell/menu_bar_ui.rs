//! The macOS menu bar item's Shell side: gathers the inputs for
//! `menu_bar::build_menu`, pushes a changed menu to AppKit, refreshes account
//! usage when the menu opens, and runs the clicks that need the Shell.
//!
//! It piggybacks on the status light's signals (state changes and the 1 s
//! heartbeat); the only network traffic is one usage probe per account list
//! at most every 60 s, started at startup, when the menu opens and every five
//! minutes (the engine's own cache is the same 60 s, and tokens are never
//! refreshed).
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
    /// Why the last probe failed outright; shown only while there is no data.
    usage_error: Option<String>,
    schedule: menu_bar::UsageSchedule,
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
        // Startup (once the engine answers) and the periodic refresh ride the
        // heartbeat; the schedule says when a probe is actually due.
        self.request_usage(menu_bar::UsageTrigger::Tick, cx);
        let utc_now = Utc::now();
        let light = crate::status_light::summarize_state(self.state.read(cx), utc_now).light;
        let snapshot = self.running_snapshot(cx);
        let interrupted = self.interrupted_block(cx);
        let usage = match (&self.menu_bar.usage, &self.menu_bar.usage_error) {
            (Some(accounts), _) => menu_bar::UsageView::Accounts(accounts),
            (None, Some(reason)) => menu_bar::UsageView::Failed(reason),
            (None, None) => menu_bar::UsageView::Loading,
        };
        let view = menu_bar::build_menu(
            &MenuInput {
                snapshot: &snapshot,
                interrupted: interrupted.as_ref(),
                usage,
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

    /// Probe usage for the Default logins and the managed accounts when the
    /// schedule says so. The engine serves its 60 s cache, never refreshes a
    /// token, and a forced list is the only call that reaches the providers.
    fn request_usage(&mut self, trigger: menu_bar::UsageTrigger, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        if !self.menu_bar.schedule.due(now, trigger) {
            return;
        }
        let rpc = self.state.read(cx).accounts_rpc(None);
        // Under test only a scripted answer counts: shell tests that drive a
        // fake engine must not see unrelated account requests on its wire.
        #[cfg(test)]
        if !rpc.is_scripted() {
            return;
        }
        if !rpc.is_connected() {
            // Startup before the engine is up: not an attempt, try next tick.
            return;
        }
        tracing::debug!(target: "usage", ?trigger, "requesting account usage");
        self.menu_bar.schedule.begin();
        self.menu_bar.task = Some(cx.spawn(async move |this, cx| {
            // The global list first: a forced list clears the engine's whole
            // usage cache, and the managed list then refills its own entries.
            let global = rpc
                .call(
                    methods::LIST_AGENT_ACCOUNTS,
                    serde_json::json!({ "forceUsage": true }),
                )
                .await
                .and_then(|value| {
                    serde_json::from_value::<mast_proto::AgentAccountsSnapshot>(value)
                        .map_err(|err| err.to_string())
                });
            let managed = rpc
                .call(
                    methods::LIST_MANAGED_ACCOUNTS,
                    crate::managed_accounts::list_params(true),
                )
                .await
                .and_then(|value| {
                    serde_json::from_value::<mast_proto::ManagedAccountsSnapshot>(value)
                        .map_err(|err| err.to_string())
                });
            this.update(cx, |shell, cx| {
                shell.finish_usage(global, managed, cx);
            })
            .ok();
        }));
    }

    fn finish_usage(
        &mut self,
        global: Result<mast_proto::AgentAccountsSnapshot, String>,
        managed: Result<mast_proto::ManagedAccountsSnapshot, String>,
        cx: &mut Context<Self>,
    ) {
        self.menu_bar.task = None;
        let ok = global.is_ok() || managed.is_ok();
        self.menu_bar
            .schedule
            .finish(std::time::Instant::now(), ok);
        for error in [global.as_ref().err(), managed.as_ref().err()].into_iter().flatten() {
            tracing::debug!(target: "usage", %error, "account list for usage failed");
        }
        if ok {
            self.menu_bar.usage_error = None;
            self.menu_bar.usage = Some(menu_bar::usage_accounts(
                global.as_ref().ok(),
                managed.as_ref().ok(),
            ));
        } else {
            // Keep what was shown; with nothing shown, say why.
            self.menu_bar.usage_error = global.err().or(managed.err());
        }
        self.sync_menu_bar(true, cx);
    }

    /// Clicks that need the Shell (the rest are handled at the App level).
    pub(crate) fn handle_menu_bar_event(&mut self, event: MenuEvent, cx: &mut Context<Self>) {
        match event {
            MenuEvent::WillOpen => {
                self.request_usage(menu_bar::UsageTrigger::MenuOpen, cx);
                self.sync_menu_bar(true, cx);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use mast_proto::{
        AgentAccount, AgentAccountsSnapshot, AgentUsageWindow, HarnessId, ManagedAccount,
        ManagedAccountsSnapshot, WorkspaceScope,
    };

    use crate::menu_bar::{Item, UsageTrigger};

    type Calls = Arc<Mutex<Vec<String>>>;

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

    fn global() -> serde_json::Value {
        serde_json::to_value(AgentAccountsSnapshot {
            accounts: vec![AgentAccount {
                id: "live".into(),
                harness: HarnessId::ClaudeCode,
                email: None,
                plan_label: None,
                active: true,
                usage_windows: vec![AgentUsageWindow {
                    label: "Session".into(),
                    used_fraction: 0.28,
                    resets_at: None,
                }],
                display_name: None,
                organization: None,
                auth_kind: None,
                switchable: true,
                saved_at: None,
                promoted: false,
            }],
            warnings: vec![],
        })
        .unwrap()
    }

    fn managed() -> serde_json::Value {
        serde_json::to_value(ManagedAccountsSnapshot {
            accounts: vec![ManagedAccount {
                id: "m1".into(),
                harness: HarnessId::Codex,
                label: "me@x.com".into(),
                email: None,
                signed_in: true,
                plan_label: None,
                usage_windows: vec![],
                usage_note: Some("token expired \u{2014} start a chat on this account to refresh it".into()),
            }],
            ..Default::default()
        })
        .unwrap()
    }

    /// Answer the two usage lists; `fail` makes every call an error.
    fn script(
        shell: &Entity<Shell>,
        cx: &mut gpui::VisualTestContext,
        fail: Arc<Mutex<bool>>,
    ) -> Calls {
        let calls: Calls = Arc::default();
        let seen = calls.clone();
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.accounts_rpc_override = Some(Arc::new(move |method, params| {
                    seen.lock().unwrap().push(format!(
                        "{method} force={}",
                        params["forceUsage"].as_bool().unwrap_or(false)
                    ));
                    if *fail.lock().unwrap() {
                        return Err("boom".to_string());
                    }
                    match method {
                        mast_rpc::methods::LIST_AGENT_ACCOUNTS => Ok(global()),
                        mast_rpc::methods::LIST_MANAGED_ACCOUNTS => Ok(managed()),
                        other => Err(format!("unexpected {other}")),
                    }
                }));
            });
        });
        calls
    }

    fn lines(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) -> Vec<String> {
        shell.read_with(cx, |shell, _| {
            shell
                .menu_bar
                .last
                .as_ref()
                .map(|view| {
                    view.items
                        .iter()
                        .filter_map(|item| match item {
                            Item::Line { text, .. } => Some(text.text.clone()),
                            Item::Section(text) | Item::Title(text) => Some(text.clone()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    fn age_last_probe(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext, secs: u64) {
        shell.update(cx, |shell, _| {
            shell.menu_bar.schedule.begin();
            shell
                .menu_bar
                .schedule
                .finish(Instant::now() - Duration::from_secs(secs), true);
        });
    }

    #[gpui::test]
    fn nothing_is_requested_until_an_engine_can_answer(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        shell.update(cx, |shell, cx| shell.sync_menu_bar(true, cx));
        cx.run_until_parked();
        shell.read_with(cx, |shell, _| {
            assert!(shell.menu_bar.usage.is_none() && shell.menu_bar.usage_error.is_none());
            assert!(
                shell.menu_bar.schedule.due(Instant::now(), UsageTrigger::Tick),
                "the startup probe is still owed, not burnt on a missing engine"
            );
        });
        assert!(lines(&shell, cx).contains(&"Loading\u{2026}".to_string()));
    }

    #[gpui::test]
    fn the_first_heartbeat_with_an_engine_requests_usage_for_every_account_kind(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let calls = script(&shell, cx, Arc::default());
        shell.update(cx, |shell, cx| shell.sync_menu_bar(true, cx));
        cx.run_until_parked();
        let seen = calls.lock().unwrap().clone();
        assert_eq!(
            seen,
            ["ListAgentAccounts force=true", "ListManagedAccounts force=true"],
            "Default logins and managed accounts, both forced"
        );
        let lines = lines(&shell, cx);
        assert!(lines.iter().any(|l| l == "Default \u{b7} Claude Code"), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("5h  ") && l.contains("28%")), "{lines:?}");
        assert!(
            lines.iter().any(|l| l == "usage unavailable \u{2014} token expired \u{2014} start a chat on this account to refresh it"),
            "{lines:?}"
        );
        // Another heartbeat right away asks for nothing more.
        shell.update(cx, |shell, cx| shell.sync_menu_bar(true, cx));
        cx.run_until_parked();
        assert_eq!(calls.lock().unwrap().len(), 2);
    }

    #[gpui::test]
    fn the_periodic_refresh_and_a_menu_open_follow_the_schedule(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let calls = script(&shell, cx, Arc::default());
        shell.update(cx, |shell, cx| shell.sync_menu_bar(true, cx));
        cx.run_until_parked();
        assert_eq!(calls.lock().unwrap().len(), 2);

        // Opening the menu inside the 60 s cache window does nothing...
        shell.update(cx, |shell, cx| shell.handle_menu_bar_event(MenuEvent::WillOpen, cx));
        cx.run_until_parked();
        assert_eq!(calls.lock().unwrap().len(), 2);
        // ...and after a minute it refreshes,
        age_last_probe(&shell, cx, menu_bar::USAGE_STALE_SECS + 1);
        shell.update(cx, |shell, cx| shell.handle_menu_bar_event(MenuEvent::WillOpen, cx));
        cx.run_until_parked();
        assert_eq!(calls.lock().unwrap().len(), 4);

        // while the heartbeat alone waits for the five-minute period.
        age_last_probe(&shell, cx, menu_bar::USAGE_STALE_SECS + 1);
        shell.update(cx, |shell, cx| shell.sync_menu_bar(true, cx));
        cx.run_until_parked();
        assert_eq!(calls.lock().unwrap().len(), 4);
        age_last_probe(&shell, cx, menu_bar::USAGE_PERIODIC_SECS + 1);
        shell.update(cx, |shell, cx| shell.sync_menu_bar(true, cx));
        cx.run_until_parked();
        assert_eq!(calls.lock().unwrap().len(), 6);
    }

    #[gpui::test]
    fn a_failed_probe_shows_the_reason_and_later_failures_keep_the_data(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let fail = Arc::new(Mutex::new(true));
        let calls = script(&shell, cx, fail.clone());
        shell.update(cx, |shell, cx| shell.sync_menu_bar(true, cx));
        cx.run_until_parked();
        assert!(
            lines(&shell, cx).contains(&"usage unavailable \u{2014} boom".to_string()),
            "{:?}",
            lines(&shell, cx)
        );

        // A failed probe is retried after a minute, not left for five.
        *fail.lock().unwrap() = false;
        shell.update(cx, |shell, _| {
            shell.menu_bar.schedule.begin();
            shell.menu_bar.schedule.finish(
                Instant::now() - Duration::from_secs(menu_bar::USAGE_STALE_SECS + 1),
                false,
            );
        });
        shell.update(cx, |shell, cx| shell.sync_menu_bar(true, cx));
        cx.run_until_parked();
        assert!(lines(&shell, cx).contains(&"Default \u{b7} Claude Code".to_string()));
        let before = calls.lock().unwrap().len();

        // A later outright failure keeps the numbers on screen.
        *fail.lock().unwrap() = true;
        age_last_probe(&shell, cx, menu_bar::USAGE_PERIODIC_SECS + 1);
        shell.update(cx, |shell, cx| shell.sync_menu_bar(true, cx));
        cx.run_until_parked();
        assert!(calls.lock().unwrap().len() > before);
        assert!(lines(&shell, cx).contains(&"Default \u{b7} Claude Code".to_string()));
    }
}
