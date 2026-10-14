//! Shell-owned planning artifact surfaces: the Artifact pane tab
//! (`RightSurface::Artifact`), `zeron-artifact:` link activation from
//! transcripts, Details, and Required Actions, and registering unknown paths.
use super::*;

use crate::artifacts::model::{self as artifact_model, LinkResolution};
use crate::artifacts::tab::{ArtifactTab, ArtifactTabEvent, BrowserWebHost, WebHostFactory};

/// Artifact tabs by surface id.
#[derive(Default)]
pub(crate) struct ArtifactTabs {
    pub(crate) tabs: std::collections::HashMap<u64, Entity<ArtifactTab>>,
    subs: std::collections::HashMap<u64, Subscription>,
    seq: u64,
    /// Tests: the web renderer new tabs use.
    #[cfg(test)]
    pub(crate) web_factory: Option<WebHostFactory>,
}

impl Shell {
    fn artifact_web_factory(&self) -> WebHostFactory {
        #[cfg(test)]
        if let Some(factory) = &self.artifact_tabs.web_factory {
            return factory.clone();
        }
        BrowserWebHost::factory()
    }

    pub(super) fn artifact_tab_title(&self, id: u64, cx: &App) -> Option<SharedString> {
        self.artifact_tabs
            .tabs
            .get(&id)
            .map(|tab| tab.read(cx).title(cx))
    }

    pub(crate) fn artifact_tab(&self, id: u64) -> Option<Entity<ArtifactTab>> {
        self.artifact_tabs.tabs.get(&id).cloned()
    }

    /// The artifact tabs' embedded web views, for the native presentation
    /// pass (only the selected tab's view is visible).
    pub(super) fn artifact_browsers(
        &self,
        cx: &App,
    ) -> Vec<(u64, Entity<crate::browser::BrowserSurface>)> {
        self.artifact_tabs
            .tabs
            .iter()
            .filter_map(|(id, tab)| tab.read(cx).web_browser().map(|browser| (*id, browser)))
            .collect()
    }

    pub(super) fn close_artifact_tab(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(tab) = self.artifact_tabs.tabs.remove(&id)
            && let Some(browser) = tab.read(cx).web_browser()
        {
            browser.update(cx, |browser, cx| browser.close(cx));
        }
        self.artifact_tabs.subs.remove(&id);
    }

    /// Activate a `zeron-artifact:` link (an artifact id or a path). Returns
    /// `false` for any other link.
    pub(crate) fn open_artifact_link(
        &mut self,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(target) = artifact_model::link_target(url) else {
            return false;
        };
        let (cwd, space_id) = {
            let state = self.state.read(cx);
            state
                .chats
                .iter()
                .find(|chat| chat.id == self.active_chat)
                .map(|chat| (chat.cwd.clone(), chat.space_id.clone()))
                .unwrap_or_default()
        };
        let resolution = artifact_model::resolve_link(
            target,
            self.state.read(cx).artifacts(),
            cwd.as_deref(),
            space_id.as_deref(),
        );
        match resolution {
            LinkResolution::Open(id) | LinkResolution::UnknownId(id) => {
                self.open_artifact(id, window, cx)
            }
            LinkResolution::Register(path) => self.register_and_open_artifact(path, window, cx),
        }
        true
    }

    /// The chat whose right pane hosts an artifact: the current chat on the
    /// chat route, else the artifact's own chat, else any live chat of its
    /// work item.
    fn artifact_host_chat(&self, artifact_id: &str, cx: &App) -> Option<String> {
        if matches!(self.route, Route::Chat) && !self.active_chat.is_empty() {
            return Some(self.active_chat.clone());
        }
        let state = self.state.read(cx);
        let live = |id: &str| {
            state
                .chats
                .iter()
                .any(|chat| chat.id == id && !chat.archived)
        };
        let artifact = state.artifacts().artifact(artifact_id);
        if let Some(chat) = artifact
            .and_then(|artifact| artifact.chat_id.as_deref())
            .filter(|chat| live(chat))
        {
            return Some(chat.to_owned());
        }
        if let Some(artifact) = artifact
            && let Some(item_id) = artifact.work_item_id.as_deref()
            && let Some(snapshot) = state.work_outline(&artifact.space_id).snapshot()
            && let Some(item) = snapshot.item(item_id)
            && let Some(chat) = item.chat_ids.iter().find(|chat| live(chat))
        {
            return Some(chat.clone());
        }
        (!self.active_chat.is_empty()).then(|| self.active_chat.clone())
    }

    /// Show an artifact in the right pane, reusing an open tab for it.
    pub(crate) fn open_artifact(
        &mut self,
        artifact_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(host) = self.artifact_host_chat(&artifact_id, cx) else {
            self.sidebar_notice = Some("Open a chat in this project to view its artifacts".into());
            cx.notify();
            return;
        };
        if host != self.active_chat || !matches!(self.route, Route::Chat | Route::Overview) {
            // Switch first; the Shell's `active_chat` follows the selection
            // through its state observer, so open once that has run.
            self.open_chat(host, cx);
            let this = cx.weak_entity();
            window.defer(cx, move |window, cx| {
                this.update(cx, |this, cx| {
                    this.open_artifact_here(artifact_id, window, cx)
                })
                .ok();
            });
            return;
        }
        self.leave_overview_for_chat(cx);
        self.open_artifact_here(artifact_id, window, cx);
    }

    fn open_artifact_here(
        &mut self,
        artifact_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_chat.is_empty() {
            return;
        }
        self.set_surfaces_open(true, cx);
        let key = self.panel_key(cx);
        let existing = self.right_tabs.get(&key).and_then(|tabs| {
            tabs.iter().find_map(|tab| match tab {
                RightSurface::Artifact(id) => self
                    .artifact_tabs
                    .tabs
                    .get(id)
                    .filter(|tab| tab.read(cx).artifact_id == artifact_id)
                    .map(|_| *id),
                _ => None,
            })
        });
        if let Some(id) = existing {
            self.set_right_active(RightSurface::Artifact(id), cx);
            return;
        }
        self.artifact_tabs.seq += 1;
        let id = self.artifact_tabs.seq;
        let factory = self.artifact_web_factory();
        let state = self.state.clone();
        let shell = cx.weak_entity();
        let tab =
            cx.new(|cx| ArtifactTab::new(state, Some(shell), artifact_id, factory, window, cx));
        let owner = key.clone();
        let sub = cx.subscribe_in(&tab, window, move |this, _, event, window, cx| {
            if this.panel_key(cx) != owner {
                return;
            }
            match event {
                ArtifactTabEvent::Changed => cx.notify(),
                ArtifactTabEvent::OpenFile { path, line, column } => {
                    this.add_file_surface_at(path.clone(), *line, *column, window, cx)
                }
                ArtifactTabEvent::OpenUrl(url) => {
                    if crate::settings::current(cx).open_web_links_in_zeron
                        && cfg!(any(target_os = "macos", target_os = "linux"))
                    {
                        this.add_browser_surface(Some(url.clone()), window, cx);
                    } else {
                        cx.open_url(url);
                    }
                }
            }
        });
        self.artifact_tabs.tabs.insert(id, tab);
        self.artifact_tabs.subs.insert(id, sub);
        self.right_tabs
            .entry(key)
            .or_default()
            .push(RightSurface::Artifact(id));
        self.set_right_active(RightSurface::Artifact(id), cx);
    }

    /// `zeron-artifact:<path>` that no artifact covers: register the file
    /// into the chat's scope, then open it. A host without the artifact RPCs
    /// opens the file as an ordinary workspace link instead.
    fn register_and_open_artifact(
        &mut self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(params) = self.register_params_for_active_chat(&path, cx) else {
            if !self.open_workspace_file_link(&path, window, cx) {
                self.sidebar_notice = Some("That artifact link can\u{2019}t be opened".into());
                cx.notify();
            }
            return;
        };
        let call = self.state.read(cx).artifact_actions().register(params);
        cx.spawn_in(window, async move |this, cx| {
            let reply = call.await;
            this.update_in(cx, |this, window, cx| match reply {
                Ok(reply) => this.open_artifact(reply.artifact_id, window, cx),
                Err(error) => {
                    if !this.open_workspace_file_link(&path, window, cx) {
                        this.sidebar_notice = Some(error.text().into());
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// `ArtifactRegister` for a path linked from the active chat: its Space,
    /// its work item (when assigned), the chat itself, forwarded to the
    /// Space's device. `None` without a chat in a Space.
    pub(crate) fn register_params_for_active_chat(
        &self,
        path: &str,
        cx: &App,
    ) -> Option<zeron_proto::ArtifactRegisterParams> {
        let state = self.state.read(cx);
        let chat = state
            .chats
            .iter()
            .find(|chat| chat.id == self.active_chat)?;
        let space_id = chat.space_id.clone()?;
        let work_item_id = state
            .work_outline(&space_id)
            .snapshot()
            .and_then(|snapshot| {
                snapshot
                    .items
                    .iter()
                    .find(|item| item.archived_at.is_none() && item.chat_ids.contains(&chat.id))
                    .map(|item| item.id.clone())
            });
        // Relative paths are relative to the chat's checkout; make them
        // absolute so the host resolves them the same way.
        let path = match (&chat.cwd, path.starts_with('/')) {
            (Some(cwd), false) => format!("{}/{}", cwd.trim_end_matches('/'), path),
            _ => path.to_owned(),
        };
        let space_device = state
            .space_row(&space_id)
            .map(|space| space.device_id.clone());
        let target_device_id =
            space_device.filter(|device| state.local_device_id.as_deref() != Some(device.as_str()));
        Some(zeron_proto::ArtifactRegisterParams {
            space_id,
            work_item_id,
            chat_id: Some(chat.id.clone()),
            path,
            title: None,
            kind: None,
            role: None,
            target_device_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::artifacts::tab::FakeWebHost;
    use crate::artifacts::tests::{
        Calls, Reply, Shown, artifact, content_reply, id, init_ui, scripted, snapshot,
    };
    use zeron_proto::{ArtifactContentEncoding, ArtifactKind};

    /// A Shell on the chat route with chat `c1` (Space `s1`, cwd
    /// `/work/alpha`) selected, `artifacts` installed, and a scripted engine.
    pub(crate) fn artifact_shell<'a>(
        cx: &'a mut gpui::TestAppContext,
        dir: &std::path::Path,
        artifacts: Vec<zeron_proto::ArtifactView>,
        reply: Reply,
    ) -> (Entity<Shell>, Calls, Shown, &'a mut gpui::VisualTestContext) {
        cx.update(|cx| {
            crate::app_menus::init(cx);
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
        });
        let data_dir = dir.to_path_buf();
        let (shell, vcx) = cx.add_window_view(move |_, cx| {
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
        });
        let (actions, calls) = scripted(reply);
        let shown: Shown = Default::default();
        let factory = FakeWebHost::factory(shown.clone());
        shell.update(vcx, |shell, cx| {
            shell.artifact_tabs.web_factory = Some(factory);
            shell.state.update(cx, |state, cx| {
                state.local_device_id = Some("dev".into());
                state.spaces = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "s1", "deviceId": "dev", "path": "/work/alpha",
                        "createdAt": "2026-01-01T00:00:00Z"
                    }))
                    .unwrap(),
                ];
                state.chats = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "c1", "deviceId": "dev", "archived": false,
                        "createdAt": "2026-01-01T00:00:00Z", "spaceId": "s1",
                        "cwd": "/work/alpha", "title": "Planning"
                    }))
                    .unwrap(),
                ];
                state.work_outline_actions_override = Some(actions);
                state.set_test_artifacts(snapshot(artifacts));
                state.select_chat(Some("c1".into()), cx);
            });
        });
        vcx.run_until_parked();
        (shell, calls, shown, vcx)
    }

    fn open_artifact_ids(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) -> Vec<String> {
        shell.read_with(cx, |shell, cx| {
            let key = shell.panel_key(cx);
            shell
                .right_tabs
                .get(&key)
                .into_iter()
                .flatten()
                .filter_map(|surface| match surface {
                    RightSurface::Artifact(id) => shell
                        .artifact_tab(*id)
                        .map(|tab| tab.read(cx).artifact_id.clone()),
                    _ => None,
                })
                .collect()
        })
    }

    #[gpui::test]
    fn artifact_links_open_a_tab_by_id_and_register_unknown_paths(cx: &mut gpui::TestAppContext) {
        let dir = init_ui(cx);
        let mut plan = artifact(&id(1), "Plan", ArtifactKind::Markdown);
        plan.space_id = "s1".into();
        plan.chat_id = Some("c1".into());
        let mut imported = artifact(&id(2), "notes", ArtifactKind::Markdown);
        imported.origin_path = Some("/work/alpha/notes/new.md".into());
        let reply: Reply = Arc::new(|method, params| {
            Ok(match method {
                zeron_rpc::methods::ARTIFACT_REGISTER => serde_json::json!({
                    "artifactId": id(2), "revision": 1,
                    "url": format!("zeron-artifact:{}", id(2)),
                    "storedPath": "/work/alpha/.zeron/artifacts/_chats/c1/new.md",
                    "changed": true
                }),
                _ => content_reply(
                    ArtifactKind::Markdown,
                    1,
                    &format!("artifact {}", params["artifactId"]),
                    ArtifactContentEncoding::Utf8,
                ),
            })
        });
        let (shell, calls, _, vcx) = artifact_shell(cx, dir.path(), vec![plan], reply);

        // An id link opens (and a second click reuses) the tab.
        shell.update_in(vcx, |shell, window, cx| {
            assert!(shell.open_artifact_link(&format!("zeron-artifact:{}", id(1)), window, cx));
            assert!(shell.open_artifact_link(&format!("zeron-artifact:{}", id(1)), window, cx));
            assert!(!shell.open_artifact_link("zeron-file:src/a.rs", window, cx));
        });
        vcx.run_until_parked();
        assert_eq!(open_artifact_ids(&shell, vcx), [id(1)]);
        shell.read_with(vcx, |shell, cx| {
            assert!(matches!(
                shell.resolved_right_active(cx),
                RightSurface::Artifact(_)
            ));
        });

        // An unknown path registers into the chat's scope, then opens.
        shell.update_in(vcx, |shell, window, cx| {
            shell.state.update(cx, |state, _| {
                let mut both = snapshot(vec![]);
                both.space_id = "s1".into();
                let mut plan = artifact(&id(1), "Plan", ArtifactKind::Markdown);
                plan.space_id = "s1".into();
                both.artifacts = vec![plan];
                state.set_test_artifacts(both);
            });
            shell.open_artifact_link("zeron-artifact:notes/new.md", window, cx);
        });
        // The engine registered it; its next frame names the new artifact.
        shell.update(vcx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                let mut both = snapshot(vec![]);
                both.space_id = "s1".into();
                let mut plan = artifact(&id(1), "Plan", ArtifactKind::Markdown);
                plan.space_id = "s1".into();
                imported.space_id = "s1".into();
                both.artifacts = vec![plan, imported.clone()];
                state.set_test_artifacts(both);
            });
        });
        vcx.run_until_parked();
        let register = calls
            .lock()
            .unwrap()
            .iter()
            .find(|(method, _)| method == zeron_rpc::methods::ARTIFACT_REGISTER)
            .map(|(_, params)| params.clone())
            .expect("registered");
        assert_eq!(register["spaceId"], "s1");
        assert_eq!(register["chatId"], "c1");
        assert_eq!(register["path"], "/work/alpha/notes/new.md");
        assert!(register.get("targetDeviceId").is_none());
        assert_eq!(open_artifact_ids(&shell, vcx), [id(1), id(2)]);

        // A known path resolves without registering again.
        let registers = |calls: &Calls| {
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(method, _)| method == zeron_rpc::methods::ARTIFACT_REGISTER)
                .count()
        };
        shell.update_in(vcx, |shell, window, cx| {
            shell.open_artifact_link("zeron-artifact:notes/new.md", window, cx);
        });
        vcx.run_until_parked();
        assert_eq!(registers(&calls), 1);
    }

    #[gpui::test]
    fn transcript_artifact_links_route_through_the_session_handler(cx: &mut gpui::TestAppContext) {
        let dir = init_ui(cx);
        let mut plan = artifact(&id(1), "Plan", ArtifactKind::Markdown);
        plan.space_id = "s1".into();
        let reply: Reply = Arc::new(|_, _| {
            Ok(content_reply(
                ArtifactKind::Markdown,
                1,
                "x",
                ArtifactContentEncoding::Utf8,
            ))
        });
        let (shell, _, _, vcx) = artifact_shell(cx, dir.path(), vec![plan], reply);
        shell.update_in(vcx, |shell, window, cx| {
            let activation = crate::markdown::render::LinkActivation {
                target: crate::markdown::render::LinkTarget::new(
                    "Plan",
                    &format!("zeron-artifact:{}", id(1)),
                ),
                action: crate::markdown::render::LinkAction::Primary,
                source_session: Some("c1".into()),
            };
            assert_eq!(
                shell.activate_session_link(&activation, window, cx),
                crate::markdown::render::LinkOutcome::Internal
            );
        });
        vcx.run_until_parked();
        assert_eq!(open_artifact_ids(&shell, vcx), [id(1)]);
    }
}
