//! Shell-owned Milestone 4 surfaces:
//!
//! - the user action notification detector, run from `on_state_changed`
//!   next to the session detector: every watched Space's snapshot goes
//!   through `UserActionNotifier` (zero-to-nonzero per work item, coalesced,
//!   retries deduplicated), and each notice plays the existing `Request`
//!   chime and posts the existing banner under the existing sound,
//!   notification, and background-only settings. No new sound or color.
//! - watching every Space's outline from the Shell, so notifications and the
//!   Required Actions counts work before the Overview is first opened;
//! - the "N proposed instructions" line in the origin Chat's header, whose
//!   Review opens the target's Work Item Details with proposals expanded.
use super::*;

use crate::work_outline::required_actions::{self, notice_outputs};

impl Shell {
    /// Diff every watched Space's outline against the last frame and notify
    /// on each work item's zero-to-nonzero transition.
    pub(super) fn notify_user_actions(
        &mut self,
        state: &Entity<AppState>,
        app_focused: bool,
        cx: &mut Context<Self>,
    ) {
        let (space_ids, snapshots) = {
            let state = state.read(cx);
            let ids: Vec<String> = state.spaces.iter().map(|space| space.id.clone()).collect();
            let snapshots: Vec<(String, std::sync::Arc<zeron_proto::WorkOutlineSnapshot>)> = ids
                .iter()
                .filter_map(|id| {
                    let load = state.work_outline(id);
                    Some((id.clone(), load.snapshot()?.clone()))
                })
                .collect();
            (ids, snapshots)
        };
        // Watch every Space (the Overview declares the same set when it
        // exists), so requests notify from app start.
        if state.read(cx).spaces_synced && self.user_action_watch.as_ref() != Some(&space_ids) {
            self.user_action_watch = Some(space_ids.clone());
            let ids = space_ids.clone();
            state.update(cx, |state, _| state.set_work_outline_visible(ids));
        }
        self.user_action_notifier
            .retain_spaces(|id| space_ids.iter().any(|space| space == id));
        let (sound, banner) = notice_outputs(&self.settings, app_focused);
        for (space_id, snapshot) in snapshots {
            for notice in self.user_action_notifier.observe(&space_id, &snapshot) {
                if sound {
                    crate::sound::play(crate::sound::Sound::Request);
                }
                if banner {
                    crate::notify::post(&notice.title(), &notice.body(), notice.chat_id.as_deref());
                }
            }
        }
    }

    /// "N proposed instructions · Review" under the chat header when this
    /// Chat proposed instructions that await review.
    pub(super) fn render_chat_proposal_status(
        &mut self,
        chat_id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (space_id, target, count) = {
            let state = self.state.read(cx);
            let space_id = state
                .chats
                .iter()
                .find(|chat| chat.id == chat_id)?
                .space_id
                .clone()?;
            let snapshot = state.work_outline(&space_id).snapshot()?.clone();
            let pending = required_actions::pending_proposals_from_chat(&snapshot, chat_id);
            let first = pending.first()?;
            let target = required_actions::proposal_target_item(first)
                .unwrap_or(crate::work_outline::details_panel::SPACE_ROOT_ITEM_ID)
                .to_owned();
            (space_id, target, pending.len())
        };
        Some(
            div()
                .debug_selector(|| "chat-header-proposals".into())
                .mt(px(2.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_muted)
                .child(
                    icon(icons::DOCUMENT)
                        .size(px(10.0))
                        .text_color(theme.text_muted),
                )
                .child(SharedString::from(required_actions::proposals_label(count)))
                .child(
                    div()
                        .id("chat-header-proposals-review")
                        .debug_selector(|| "chat-header-proposals-review".into())
                        .flex_none()
                        .px(px(4.0))
                        .rounded(px(3.0))
                        .cursor_pointer()
                        .text_color(theme.accent)
                        .hover(|el| el.bg(theme.element_hover))
                        .child("Review")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_work_item_proposals(space_id.clone(), target.clone(), cx)
                        })),
                )
                .into_any_element(),
        )
    }

    /// Open the Overview with the target's details and its proposals
    /// expanded (`item_id` empty = Space root).
    pub(crate) fn open_work_item_proposals(
        &mut self,
        space_id: String,
        item_id: String,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.route, Route::Overview) {
            self.open_overview(cx);
        }
        let page = self.ensure_overview_page(cx);
        page.update(cx, |overview, cx| {
            overview.open_proposals(space_id, item_id, cx)
        });
    }
}

#[cfg(test)]
mod status_light_tests {
    use super::*;
    use crate::status_light::AppLight;
    use zeron_proto::{SessionStatus, WorkspaceScope};

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
                state.auth = Some(zeron_proto::AuthState::SignedOut);
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
                    default_harness: zeron_proto::HarnessId::Mock,
                },
                cx,
            )
        })
    }

    #[gpui::test]
    fn the_logo_follows_the_light_and_the_setting_turns_it_off(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let state = shell.read_with(cx, |shell, _| shell.state.clone());
        let light = |cx: &mut gpui::VisualTestContext| {
            shell.update(cx, |shell, cx| {
                shell.sync_status_light(cx);
                shell.status_light.map(|summary| summary.light)
            })
        };
        assert_eq!(light(cx), Some(AppLight::Idle), "on by default");

        state.update(cx, |state, _| {
            state.sessions = vec![zeron_proto::Session {
                last_completed_turn: None,
                chat_id: "c1".into(),
                device_id: "d".into(),
                status: SessionStatus::Errored,
                started_at: None,
                updated_at: chrono::Utc::now(),
            }];
            state.chats = vec![zeron_proto::Chat {
                id: "c1".into(),
                device_id: "d".into(),
                title: None,
                archived: false,
                cwd: None,
                branch: None,
                checkout_id: None,
                source_context: None,
                config: None,
                last_message_preview: None,
                last_message_at: None,
                created_at: chrono::Utc::now(),
                harness_session_id: None,
                harness_session_cwd: None,
                actual_model: None,
                parent_chat_id: None,
                space_id: None,
                last_seen_at: None,
                room_gen: None,
                linked_pr_url: None,
                linked_pr_source: None,
                linked_ticket_id: None,
                linked_ticket_source: None,
            }];
        });
        assert_eq!(light(cx), Some(AppLight::Error));

        shell.update(cx, |shell, _| shell.settings.live_app_icon = false);
        assert_eq!(light(cx), None, "off keeps the plain logo");
        // Off restores the Dock at once: nothing is left pending.
        shell.read_with(cx, |shell, _| assert!(!shell.dock_sync.has_pending()));
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
    }
}
