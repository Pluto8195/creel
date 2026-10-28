//! Shell host of the work-context strip (`chat_work_context`): reads the
//! stores, asks for the PR, Linear issue and artifact data the strip shows
//! (the same fetch paths and caches the Outline chips use), and wires the
//! clicks to the Shell. The strip itself renders only what the stores hold.
use super::*;

use std::collections::HashMap;
use std::rc::Rc;

use crate::chat_work_context::{self, ResolveInput, Variant, WorkContext, strip};
use crate::review_chats::ReviewChat;

/// Height the strip's line adds under the chat header metadata; the
/// transcript's top fade clears it (the expanded panel overlays the
/// transcript on an opaque background).
pub(super) const CHAT_WORK_CONTEXT_CLEARANCE: f32 = 24.0;

impl Shell {
    /// The Chat's work context, when its item, an ancestor, or the review
    /// launcher gives it one. Never fetches; see [`Self::request_work_context_data`].
    fn resolve_chat_work_context(&self, chat_id: &str, cx: &Context<Self>) -> Option<WorkContext> {
        let links = self
            .chat_header_links
            .get(chat_id)
            .and_then(|entry| entry.status.as_ref())
            .map(chat_work_context::chat_prs)
            .unwrap_or_default();
        let state = self.state.read(cx);
        let chat = state.chats.iter().find(|chat| chat.id == chat_id)?;
        let space_id = chat.space_id.as_deref();
        let snapshot = space_id.and_then(|id| state.work_outline(id).snapshot().cloned());
        let artifacts = space_id.and_then(|id| state.artifacts().snapshot(id).cloned());
        chat_work_context::resolve(&ResolveInput {
            chat_id,
            snapshot: snapshot.as_deref(),
            chat_prs: &links,
            artifacts: artifacts.as_deref(),
        })
    }

    /// Ask the stores for what `context` shows (cached, so cheap on every
    /// render) and copy it for the strip.
    fn request_work_context_data(
        &mut self,
        context: &WorkContext,
        cx: &mut Context<Self>,
    ) -> strip::StripData {
        let (prs, ticket, project_id) = match &context.variant {
            Variant::PrReview(review) => (review.prs.clone(), review.ticket.clone(), None),
            Variant::Implementation(implementation) => (
                implementation.prs.clone(),
                implementation.ticket.clone(),
                None,
            ),
            Variant::Planning(planning) => (
                Vec::new(),
                None,
                planning.project.as_ref().map(|project| project.id.clone()),
            ),
        };
        for pr in &prs {
            crate::github::store::request_status(&self.state, pr, false, cx);
        }
        if !prs.is_empty() {
            crate::github::store::request_viewer(&self.state, cx);
        }
        if let Some(ticket) = &ticket {
            crate::linear::store::request_issue(&self.state, &ticket.identifier, false, cx);
        }
        let review_chats = self.work_context_review_chats(&prs, cx);
        let state = self.state.read(cx);
        let entries: HashMap<String, _> = prs
            .iter()
            .filter_map(|pr| Some((pr.key(), state.github.status(pr)?.clone())))
            .collect();
        strip::StripData {
            prs: entries,
            issue: ticket
                .as_ref()
                .and_then(|ticket| state.linear.issue(&ticket.identifier).cloned()),
            project: project_id.and_then(|id| {
                state
                    .linear
                    .projects
                    .as_ref()?
                    .iter()
                    .find(|project| project.id == id)
                    .cloned()
            }),
            viewer: state.github.viewer().map(str::to_owned),
            review_chats,
            now: chrono::Utc::now(),
        }
    }

    /// The review chats of `prs`, most recent first (one list across all of
    /// the context's PRs).
    fn work_context_review_chats(
        &self,
        prs: &[mast_proto::PullRequestRef],
        cx: &Context<Self>,
    ) -> Vec<ReviewChat> {
        let state = self.state.read(cx);
        let space_id = state
            .selected_chat
            .as_deref()
            .and_then(|id| state.chats.iter().find(|chat| chat.id == id))
            .and_then(|chat| chat.space_id.as_deref());
        let snapshot = space_id.and_then(|id| state.work_outline(id).snapshot().cloned());
        let mut chats: Vec<ReviewChat> = Vec::new();
        for pr in prs {
            for chat in crate::review_chats::for_pr(cx, snapshot.as_deref(), pr, &[], |id| {
                crate::review_chats::fact_from_state(state, id)
            }) {
                if !chats.iter().any(|known| known.chat_id == chat.chat_id) {
                    chats.push(chat);
                }
            }
        }
        chats.sort_by(|a, b| {
            b.updated
                .cmp(&a.updated)
                .then_with(|| a.chat_id.cmp(&b.chat_id))
        });
        chats
    }

    /// The work-context strip for the full chat header, with the extra
    /// clearance it needs (0 when the Chat has no context).
    pub(super) fn render_chat_work_context(
        &mut self,
        chat_id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let Some(context) = self.resolve_chat_work_context(chat_id, cx) else {
            self.chat_header_extra_clearance = 0.0;
            return None;
        };
        self.chat_header_extra_clearance = CHAT_WORK_CONTEXT_CLEARANCE;
        let data = self.request_work_context_data(&context, cx);
        let expanded = self.chat_work_context_open.contains(chat_id);
        let space_id = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(|chat| chat.space_id.clone());
        let shell = cx.entity();
        let toggle_chat = chat_id.to_owned();
        let toggle_shell = shell.clone();
        let url_shell = shell.clone();
        let item_shell = shell.clone();
        let artifact_shell = shell.clone();
        let chat_shell = shell.clone();
        let actions = strip::Actions {
            toggle: Rc::new(move |_, cx| {
                toggle_shell.update(cx, |this, cx| {
                    if !this.chat_work_context_open.remove(&toggle_chat) {
                        this.chat_work_context_open.insert(toggle_chat.clone());
                    }
                    cx.notify();
                });
            }),
            open_url: Rc::new(move |url, window, cx| {
                let external = window.modifiers().secondary();
                url_shell.update(cx, |this, cx| {
                    this.open_web_reference(&url, external, window, cx)
                });
            }),
            open_item: Rc::new(move |item_id, window, cx| {
                let Some(space_id) = space_id.clone() else {
                    return;
                };
                item_shell.update(cx, |this, cx| {
                    this.reveal_work_item_in_outline(&space_id, &item_id, window, cx)
                });
            }),
            open_artifact: Rc::new(move |artifact_id, window, cx| {
                artifact_shell.update(cx, |this, cx| this.open_artifact(artifact_id, window, cx));
            }),
            open_chat: Rc::new(move |chat_id, _, cx| {
                chat_shell.update(cx, |this, cx| this.open_chat(chat_id, cx));
            }),
            shell: Some(shell.downgrade()),
        };
        Some(strip::render(&context, &data, expanded, &actions, theme))
    }

    /// Show `item_id` in the Outline (expanding collapsed ancestors).
    fn reveal_work_item_in_outline(
        &mut self,
        space_id: &str,
        item_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_overview(cx);
        let page = self.ensure_overview_page(cx);
        page.update(cx, |overview, cx| {
            overview.focus_outline_on_chat(None, window, cx)
        });
        let key = crate::work_outline::model::row_key::item(space_id, item_id);
        let outline = page.read(cx).outline_entity();
        outline.update(cx, |outline, cx| outline.reveal(&key, cx));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use chrono::TimeZone;
    use gpui::{AppContext as _, Render, TestAppContext, VisualTestContext, div, px};
    use mast_engine::pr_ticket_cache::{ChatLinkStatus, PrLinkStatus};
    use mast_proto::ChatLinkSource;

    use super::*;
    use crate::github::fixture as gf;
    use crate::work_outline::store::{fixture_ids::*, fixture_snapshot};

    struct Host {
        shell: Entity<Shell>,
        chat_id: &'static str,
        _dir: tempfile::TempDir,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::default();
            let chat_id = self.chat_id;
            let strip = self.shell.update(cx, |shell, cx| {
                shell.render_chat_work_context(chat_id, &theme, cx)
            });
            div().w(px(900.0)).children(strip)
        }
    }

    fn chat(id: &str) -> mast_proto::Chat {
        mast_proto::Chat {
            id: id.into(),
            device_id: "device".into(),
            title: None,
            archived: false,
            cwd: None,
            branch: None,
            checkout_id: None,
            source_context: None,
            config: None,
            last_message_preview: None,
            last_message_at: None,
            created_at: chrono::Utc.timestamp_opt(5, 0).unwrap(),
            harness_session_id: None,
            harness_session_cwd: None,
            actual_model: None,
            parent_chat_id: None,
            space_id: Some("s1".into()),
            last_seen_at: None,
            room_gen: None,
            linked_pr_url: None,
            linked_pr_source: None,
            linked_ticket_id: None,
            linked_ticket_source: None,
        }
    }

    type Calls = Arc<Mutex<Vec<String>>>;

    /// A Shell with a `w-launch` > `w-api` review item (PR `acme/app#7`) and
    /// the chats of the store fixture; `launcher` also links `acme/app#9` to
    /// `chat-loose-a`, as the review launcher does.
    fn mount<'a>(
        cx: &'a mut TestAppContext,
        chat_id: &'static str,
        launcher: bool,
    ) -> (Entity<Shell>, Calls, &'a mut VisualTestContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
        });
        let calls: Calls = Default::default();
        let sink = calls.clone();
        let (host, vcx) = cx.add_window_view(move |_, cx| {
            let shell = cx.new(|cx| {
                let state = cx.new(|_| AppState::new());
                state.update(cx, |state, _| {
                    let mut snapshot = fixture_snapshot("s1");
                    snapshot.space_settings.pr_review_sync_work_item_id = Some(LAUNCH.into());
                    snapshot.external_references = vec![gf::reference("r1", "s1", API, "app", 7)];
                    state.set_work_outline_for_test(snapshot);
                    state.chats = [CHAT_API, CHAT_LOOSE_A, CHAT_UI]
                        .iter()
                        .map(|id| chat(id))
                        .collect();
                    state.work_outline_actions_override =
                        Some(crate::work_outline::WorkOutlineActions::scripted(Arc::new(
                            move |method, params| {
                                sink.lock().unwrap().push(method.to_string());
                                if method == "PrViewer" {
                                    return Ok(serde_json::json!({"login": "someone-else"}));
                                }
                                assert_eq!(method, "PrStatus", "{params}");
                                let number = params["number"].as_u64().unwrap_or(7);
                                Ok(serde_json::to_value(gf::summary("app", number, "Add auth"))
                                    .unwrap())
                            },
                        )));
                });
                let mut shell = Shell::new(
                    state,
                    EngineBootConfig {
                        data_dir: dir.path().into(),
                        ipc_port: 0,
                        edge_url: "http://127.0.0.1:1".into(),
                        edge_token: None,
                        org_id: None,
                        workos_client_id: None,
                        default_harness: mast_proto::HarnessId::Mock,
                    },
                    cx,
                );
                if launcher {
                    shell.chat_header_links.insert(
                        CHAT_LOOSE_A.into(),
                        ChatHeaderLinkCache {
                            status: Some(ChatLinkStatus {
                                pr_links: vec![PrLinkStatus {
                                    url: "https://github.com/acme/app/pull/9".into(),
                                    source: Some(ChatLinkSource::ReviewLauncher),
                                    detail: None,
                                }],
                                ..ChatLinkStatus::default()
                            }),
                            fetched_at: Instant::now(),
                        },
                    );
                }
                shell
            });
            Host {
                shell,
                chat_id,
                _dir: dir,
            }
        });
        let shell = host.read_with(vcx, |host, _| host.shell.clone());
        draw(vcx);
        (shell, calls, vcx)
    }

    /// The scripted calls other than the viewer's.
    fn status_calls(calls: &Calls) -> Vec<String> {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|method| *method != "PrViewer")
            .cloned()
            .collect()
    }

    fn viewer_calls(calls: &Calls) -> usize {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|method| *method == "PrViewer")
            .count()
    }

    fn draw(vcx: &mut VisualTestContext) {
        vcx.update(|window, cx| window.draw(cx).clear());
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear());
    }

    #[gpui::test]
    fn a_chat_on_a_review_item_gets_the_strip_and_fetches_its_status(cx: &mut TestAppContext) {
        let (shell, calls, vcx) = mount(cx, CHAT_API, false);
        assert!(vcx.debug_bounds("work-context-strip").is_some());
        assert!(vcx.debug_bounds("work-context-pr-number").is_some());
        // The status arrived through the shared store and replaced the note.
        assert_eq!(status_calls(&calls), ["PrStatus"]);
        assert_eq!(viewer_calls(&calls), 1, "the viewer is asked once");
        assert!(vcx.debug_bounds("work-context-pr-title").is_some());
        assert!(vcx.debug_bounds("work-context-pr-note").is_none());
        shell.read_with(vcx, |shell, _| {
            assert_eq!(
                shell.chat_header_extra_clearance,
                CHAT_WORK_CONTEXT_CLEARANCE
            );
        });
    }

    #[gpui::test]
    fn clicking_the_number_opens_the_pr_through_the_shells_handler(cx: &mut TestAppContext) {
        let (shell, _, vcx) = mount(cx, CHAT_API, false);
        let at = vcx.debug_bounds("work-context-pr-number").unwrap().center();
        vcx.simulate_click(at, gpui::Modifiers::default());
        draw(vcx);
        assert_eq!(
            vcx.opened_url().as_deref(),
            Some("https://github.com/acme/app/pull/7")
        );
        // Not a toggle.
        shell.read_with(vcx, |shell, _| {
            assert!(shell.chat_work_context_open.is_empty())
        });
        // The chevron toggles the Shell's per-chat state and shows the panel.
        let at = vcx.debug_bounds("work-context-chevron").unwrap().center();
        vcx.simulate_click(at, gpui::Modifiers::default());
        draw(vcx);
        shell.read_with(vcx, |shell, _| {
            assert!(shell.chat_work_context_open.contains(CHAT_API));
        });
        assert!(vcx.debug_bounds("work-context-panel").is_some());
    }

    #[gpui::test]
    fn a_launcher_review_chat_gets_the_strip_for_its_pr(cx: &mut TestAppContext) {
        let (_, calls, vcx) = mount(cx, CHAT_LOOSE_A, true);
        assert!(vcx.debug_bounds("work-context-strip").is_some());
        assert_eq!(status_calls(&calls), ["PrStatus"]);
        let at = vcx.debug_bounds("work-context-pr-number").unwrap().center();
        vcx.simulate_click(at, gpui::Modifiers::default());
        assert_eq!(
            vcx.opened_url().as_deref(),
            Some("https://github.com/acme/app/pull/9")
        );
    }

    #[gpui::test]
    fn chats_with_no_pr_are_unchanged(cx: &mut TestAppContext) {
        for chat_id in [CHAT_LOOSE_A, CHAT_UI] {
            let (shell, calls, vcx) = mount(cx, chat_id, false);
            assert!(
                vcx.debug_bounds("work-context-strip").is_none(),
                "{chat_id}"
            );
            assert!(calls.lock().unwrap().is_empty(), "no fetch for {chat_id}");
            shell.read_with(vcx, |shell, _| {
                assert_eq!(shell.chat_header_extra_clearance, 0.0)
            });
        }
    }

    /// Web and PR links from every surface go through one Shell entry point
    /// (`open_web_reference`). Only the model is asserted: tabs, URLs, the
    /// route and the external opener, never rendering.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    mod web_references {
        use super::*;
        use crate::shell::Route;
        use gpui::{ClickEvent, Modifiers, MouseClickEvent, MouseDownEvent, MouseUpEvent};

        const PR_7: &str = "https://github.com/acme/app/pull/7";
        const PR_99: &str = "https://github.com/acme/app/pull/99";

        fn open(shell: &Entity<Shell>, vcx: &mut VisualTestContext, url: &str, external: bool) {
            vcx.update(|window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.open_web_reference(url, external, window, cx)
                })
            });
            vcx.run_until_parked();
        }

        fn select(shell: &Entity<Shell>, vcx: &mut VisualTestContext, chat: &str, route: Route) {
            shell.update(vcx, |shell, cx| {
                shell.route = route;
                let state = shell.state.clone();
                state.update(cx, |state, cx| state.select_chat(Some(chat.into()), cx));
            });
            vcx.run_until_parked();
        }

        fn add_space(shell: &Entity<Shell>, vcx: &mut VisualTestContext) {
            shell.update(vcx, |shell, cx| {
                shell.state.update(cx, |state, _| {
                    state.spaces.push(mast_proto::Space {
                        id: "s1".into(),
                        device_id: "device".into(),
                        path: "/project".into(),
                        name: None,
                        git_detected: false,
                        git_checked_at: None,
                        checkout_id: None,
                        created_at: chrono::Utc.timestamp_opt(0, 0).unwrap(),
                    })
                })
            });
        }

        fn tabs(shell: &Entity<Shell>, vcx: &mut VisualTestContext) -> Vec<String> {
            shell.read_with(vcx, |shell, cx| {
                let mut tabs: Vec<_> = shell.browsers.iter().collect();
                tabs.sort_by_key(|(id, _)| **id);
                tabs.into_iter()
                    .map(|(_, browser)| browser.read(cx).page.url.clone().unwrap_or_default())
                    .collect()
            })
        }

        fn set_open_in_mast(vcx: &mut VisualTestContext, on: bool) {
            vcx.update(|_, cx| {
                crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
                    settings.open_web_links_in_zeron = on
                })
            });
        }

        /// The chat route with `chat` on screen.
        fn on_chat(shell: &Entity<Shell>, vcx: &mut VisualTestContext, chat: &str) {
            select(shell, vcx, chat, Route::Chat);
            shell.read_with(vcx, |shell, _| assert_eq!(shell.active_chat, chat));
        }

        #[gpui::test]
        fn the_strip_opens_the_pr_in_a_side_tab_and_focuses_it_the_second_time(
            cx: &mut TestAppContext,
        ) {
            let (shell, _, vcx) = mount(cx, CHAT_API, false);
            on_chat(&shell, vcx, CHAT_API);
            let at = vcx.debug_bounds("work-context-pr-number").unwrap().center();
            vcx.simulate_click(at, Modifiers::default());
            draw(vcx);
            assert_eq!(tabs(&shell, vcx), [PR_7]);
            assert!(vcx.opened_url().is_none(), "not the external browser");
            shell.read_with(vcx, |shell, cx| {
                assert!(shell.right_pane_open(cx));
                let first = shell.browser_seq;
                assert_eq!(
                    shell.resolved_right_active(cx),
                    RightSurface::Browser(first)
                );
            });
            // Another page opens a second tab; the PR again focuses the first.
            open(&shell, vcx, "https://example.com/docs", false);
            assert_eq!(tabs(&shell, vcx).len(), 2);
            let at = vcx.debug_bounds("work-context-pr-number").unwrap().center();
            vcx.simulate_click(at, Modifiers::default());
            draw(vcx);
            assert_eq!(tabs(&shell, vcx), [PR_7, "https://example.com/docs"]);
            shell.read_with(vcx, |shell, cx| {
                let first = *shell.browsers.keys().min().unwrap();
                assert_eq!(
                    shell.resolved_right_active(cx),
                    RightSurface::Browser(first)
                );
            });
        }

        #[gpui::test]
        fn command_click_and_the_off_setting_open_the_external_browser(cx: &mut TestAppContext) {
            let (shell, _, vcx) = mount(cx, CHAT_API, false);
            on_chat(&shell, vcx, CHAT_API);
            // ⌘-click on the strip's number.
            let at = vcx.debug_bounds("work-context-pr-number").unwrap().center();
            vcx.simulate_click(at, Modifiers::secondary_key());
            draw(vcx);
            assert_eq!(vcx.opened_url().as_deref(), Some(PR_7));
            assert!(tabs(&shell, vcx).is_empty());
            // The explicit external entry (context menu).
            open(&shell, vcx, "https://example.com/menu", true);
            assert_eq!(
                vcx.opened_url().as_deref(),
                Some("https://example.com/menu")
            );
            // With the setting off nothing changes.
            set_open_in_mast(vcx, false);
            open(&shell, vcx, "https://example.com/off", false);
            assert_eq!(vcx.opened_url().as_deref(), Some("https://example.com/off"));
            assert!(tabs(&shell, vcx).is_empty());
        }

        #[gpui::test]
        fn pr_chip_clicks_follow_the_same_rules(cx: &mut TestAppContext) {
            // `open_pr` backs the outline row icons, the details panel, the
            // overview detail rows and the PR chips.
            let (shell, _, vcx) = mount(cx, CHAT_API, false);
            on_chat(&shell, vcx, CHAT_API);
            let click = |modifiers: Modifiers| {
                ClickEvent::Mouse(MouseClickEvent {
                    down: MouseDownEvent {
                        button: gpui::MouseButton::Left,
                        click_count: 1,
                        modifiers,
                        ..Default::default()
                    },
                    up: MouseUpEvent {
                        button: gpui::MouseButton::Left,
                        click_count: 1,
                        modifiers,
                        ..Default::default()
                    },
                })
            };
            let weak = shell.downgrade();
            vcx.update(|window, cx| {
                crate::github::chip::open_pr(Some(weak.clone()), PR_7.into())(
                    &click(Modifiers::default()),
                    window,
                    cx,
                )
            });
            assert_eq!(tabs(&shell, vcx), [PR_7]);
            assert!(vcx.opened_url().is_none());
            // Again: focus, no duplicate.
            vcx.update(|window, cx| {
                crate::github::chip::open_pr(Some(weak.clone()), PR_7.into())(
                    &click(Modifiers::default()),
                    window,
                    cx,
                )
            });
            assert_eq!(tabs(&shell, vcx).len(), 1);
            // ⌘-click.
            vcx.update(|window, cx| {
                crate::github::chip::open_pr(Some(weak.clone()), PR_99.into())(
                    &click(Modifiers::secondary_key()),
                    window,
                    cx,
                )
            });
            assert_eq!(vcx.opened_url().as_deref(), Some(PR_99));
            assert_eq!(tabs(&shell, vcx).len(), 1);
        }

        #[gpui::test]
        fn a_route_without_a_side_pane_goes_to_the_review_chat(cx: &mut TestAppContext) {
            let (shell, _, vcx) = mount(cx, CHAT_API, false);
            add_space(&shell, vcx);
            // The PR page (or the outline): nothing selected, no side pane.
            shell.update(vcx, |shell, cx| {
                shell.route = Route::PrReviewChats;
                cx.notify();
            });
            // PR #7 is reviewed in CHAT_API (assigned to its review item).
            open(&shell, vcx, PR_7, false);
            shell.read_with(vcx, |shell, cx| {
                assert!(matches!(shell.route, Route::Chat));
                assert_eq!(shell.active_chat, CHAT_API);
                assert_eq!(
                    shell.state.read(cx).selected_chat.as_deref(),
                    Some(CHAT_API)
                );
                assert!(
                    shell.right_pane_open(cx),
                    "the browser sits beside the chat"
                );
            });
            assert_eq!(tabs(&shell, vcx), [PR_7]);
            assert!(vcx.opened_url().is_none());
        }

        #[gpui::test]
        fn a_pr_without_a_review_chat_uses_the_selected_chat_else_goes_external(
            cx: &mut TestAppContext,
        ) {
            let (shell, _, vcx) = mount(cx, CHAT_API, false);
            add_space(&shell, vcx);
            // Nothing to host a tab: the external browser.
            shell.update(vcx, |shell, cx| {
                shell.route = Route::Overview;
                cx.notify();
            });
            open(&shell, vcx, PR_99, false);
            assert_eq!(vcx.opened_url().as_deref(), Some(PR_99));
            assert!(tabs(&shell, vcx).is_empty());
            assert!(matches!(
                shell.read_with(vcx, |s, _| s.route),
                Route::Overview
            ));
            // The Overview previews CHAT_UI: the tab opens beside it.
            select(&shell, vcx, CHAT_UI, Route::Overview);
            open(&shell, vcx, PR_99, false);
            shell.read_with(vcx, |shell, cx| {
                assert!(matches!(shell.route, Route::Chat));
                assert_eq!(shell.active_chat, CHAT_UI);
                assert!(shell.right_pane_open(cx));
            });
            assert_eq!(tabs(&shell, vcx), [PR_99]);
            // A plain web link takes the same route.
            select(&shell, vcx, CHAT_UI, Route::PrReviewChats);
            open(&shell, vcx, "https://example.com/spec", false);
            assert_eq!(tabs(&shell, vcx), [PR_99, "https://example.com/spec"]);
        }

        #[gpui::test]
        fn off_the_chat_route_the_setting_and_command_click_still_win(cx: &mut TestAppContext) {
            let (shell, _, vcx) = mount(cx, CHAT_API, false);
            add_space(&shell, vcx);
            select(&shell, vcx, CHAT_API, Route::PrReviewChats);
            open(&shell, vcx, PR_7, true);
            assert_eq!(vcx.opened_url().as_deref(), Some(PR_7));
            assert!(matches!(
                shell.read_with(vcx, |s, _| s.route),
                Route::PrReviewChats
            ));
            set_open_in_mast(vcx, false);
            open(&shell, vcx, PR_99, false);
            assert_eq!(vcx.opened_url().as_deref(), Some(PR_99));
            assert!(tabs(&shell, vcx).is_empty());
            assert!(matches!(
                shell.read_with(vcx, |s, _| s.route),
                Route::PrReviewChats
            ));
        }

        #[gpui::test]
        fn a_link_that_is_not_web_is_refused(cx: &mut TestAppContext) {
            let (shell, _, vcx) = mount(cx, CHAT_API, false);
            on_chat(&shell, vcx, CHAT_API);
            open(&shell, vcx, "file:///etc/passwd", false);
            assert!(tabs(&shell, vcx).is_empty());
            assert!(vcx.opened_url().is_none());
            shell.read_with(vcx, |shell, _| assert!(shell.sidebar_notice.is_some()));
        }
    }
}
