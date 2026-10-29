//! Headless tests for the chat header, its hover card and the Item tab. Every
//! store is filled by hand; nothing reaches GitHub, Linear or an engine.
use std::time::Duration;

use chrono::TimeZone;
use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext,
    VisualTestContext, Window, div, px,
};
use mast_proto::{
    ChatAccountPin, HarnessId, ManagedAccount, ManagedAccountsSnapshot, WorkOutlineSnapshot,
};

use super::*;
use crate::work_outline::required_actions::fixture::{R_UI, request};
use crate::work_outline::store::{fixture_ids::*, fixture_snapshot};

struct Host {
    shell: Entity<Shell>,
    chat_id: &'static str,
    tab: bool,
    _dir: tempfile::TempDir,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::default();
        let tab = self.tab;
        let (header, body) = self.shell.update(cx, |shell, cx| {
            let header = shell.render_chat_header_metadata(&theme, cx);
            let body = tab.then(|| shell.render_item_tab(cx));
            (header, body)
        });
        div()
            .relative()
            .w(px(900.0))
            .h(px(600.0))
            .children(header)
            .children(body.map(|body| div().w(px(420.0)).h(px(500.0)).child(body)))
    }
}

fn chat(id: &str) -> mast_proto::Chat {
    let mut chat: mast_proto::Chat = serde_json::from_value(serde_json::json!({
        "id": id,
        "deviceId": "device",
        "title": format!("Title of {id}"),
        "archived": false,
        "cwd": format!("/work/{id}"),
        "branch": "feature/header",
        "checkoutId": null,
        "config": null,
        "lastMessagePreview": null,
        "lastMessageAt": null,
        "createdAt": chrono::Utc.timestamp_opt(5, 0).unwrap(),
        "spaceId": "s1",
    }))
    .unwrap();
    chat.space_id = Some("s1".into());
    chat
}

/// `edit` runs on the Space's snapshot before the Shell sees it.
fn mount<'a>(
    cx: &'a mut TestAppContext,
    chat_id: &'static str,
    tab: bool,
    edit: impl FnOnce(&mut WorkOutlineSnapshot) + 'static,
) -> (Entity<Shell>, &'a mut VisualTestContext) {
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
    let (host, vcx) = cx.add_window_view(move |_, cx| {
        let shell = cx.new(|cx| {
            let state = cx.new(|_| AppState::new());
            state.update(cx, |state, _| {
                let mut snapshot = fixture_snapshot("s1");
                snapshot.external_references =
                    vec![crate::github::fixture::reference("r1", "s1", API, "app", 7)];
                edit(&mut snapshot);
                state.set_work_outline_for_test(snapshot);
                state.chats = [CHAT_API, CHAT_LOOSE_A, CHAT_UI]
                    .iter()
                    .map(|id| chat(id))
                    .collect();
                state.selected_chat = Some(chat_id.into());
                state.set_managed_accounts(
                    Some("device"),
                    ManagedAccountsSnapshot {
                        accounts: vec![ManagedAccount {
                            id: "a1".into(),
                            harness: HarnessId::ClaudeCode,
                            label: "Work".into(),
                            email: None,
                            signed_in: false,
                        }],
                        project_defaults: Vec::new(),
                        chat_pins: vec![ChatAccountPin {
                            chat_id: CHAT_API.into(),
                            harness: HarnessId::ClaudeCode,
                            account_id: Some("a1".into()),
                        }],
                        saved: Vec::new(),
                    },
                );
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
            shell.active_chat = chat_id.into();
            // The header's link fetch would reach for an engine; the cache
            // entry says "nothing linked" and is fresh for the whole test.
            shell.chat_header_links.insert(
                chat_id.into(),
                ChatHeaderLinkCache {
                    status: None,
                    fetched_at: Instant::now(),
                },
            );
            shell
        });
        Host {
            shell,
            chat_id,
            tab,
            _dir: dir,
        }
    });
    let shell = host.read_with(vcx, |host, _| host.shell.clone());
    draw(vcx);
    (shell, vcx)
}

fn draw(vcx: &mut VisualTestContext) {
    vcx.update(|window, cx| window.draw(cx).clear());
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear());
}

fn has(vcx: &mut VisualTestContext, selector: &'static str) -> bool {
    vcx.debug_bounds(selector).is_some()
}

/// Drift on the chat, one agent ask, and one plan question on its item.
fn needs_attention(snapshot: &mut WorkOutlineSnapshot) {
    snapshot
        .chat_instruction_states
        .push(mast_proto::ChatInstructionState {
            chat_id: CHAT_API.into(),
            selected_revision_id: Some("rev-1".into()),
            selected_revision_hash: Some("aaaaaaaaaaaa".into()),
            selected_revision_integrity: None,
            current_effective_hash: Some("bbbbbbbbbbbb".into()),
            drift: true,
            delivered_revision_id: Some("rev-1".into()),
            delivered_revision_hash: Some("aaaaaaaaaaaa".into()),
        });
    snapshot.user_action_requests.push(request(
        R_UI,
        "s1",
        API,
        CHAT_API,
        "Pick the auth provider",
    ));
    let mut question = request("req-q", "s1", API, CHAT_API, "Q-2: which tenant?");
    question.dedupe_key = Some("artq:art1:Q-2".into());
    snapshot.user_action_requests.push(question);
}

#[gpui::test]
fn the_header_shows_breadcrumb_title_chips_and_badges(cx: &mut TestAppContext) {
    let (_, vcx) = mount(cx, CHAT_API, false, needs_attention);
    for selector in [
        "chat-header",
        "chat-header-breadcrumb",
        "chat-header-crumb-0",
        "chat-header-title",
        "chat-header-chip-model",
        "chat-header-chip-account",
        "chat-header-chip-branch",
        "chat-header-chip-pr-0",
        "chat-header-badge-attention",
        "chat-header-badge-questions",
    ] {
        assert!(has(vcx, selector), "{selector} should be on the header");
    }
}

#[gpui::test]
fn a_chat_with_nothing_to_flag_has_no_badges_and_a_chat_without_an_item_no_breadcrumb(
    cx: &mut TestAppContext,
) {
    let (_, vcx) = mount(cx, CHAT_API, false, |_| {});
    assert!(has(vcx, "chat-header-breadcrumb"));
    assert!(!has(vcx, "chat-header-badge-attention"));
    assert!(!has(vcx, "chat-header-badge-questions"));
    let (_, vcx) = mount(cx, CHAT_LOOSE_A, false, |_| {});
    assert!(has(vcx, "chat-header-title"));
    assert!(has(vcx, "chat-header-chip-model"));
    assert!(!has(vcx, "chat-header-breadcrumb"));
    assert!(!has(vcx, "chat-header-chip-pr-0"));
    assert!(!has(vcx, "chat-header-chip-account"));
}

#[gpui::test]
fn the_old_strip_drift_and_proposal_lines_are_gone_from_the_header(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, |snapshot| {
        needs_attention(snapshot);
        snapshot
            .instruction_proposals
            .push(crate::work_outline::required_actions::fixture::proposal(
                "p1",
                "s1",
                Some(API),
                CHAT_API,
                "Prefer small PRs",
            ));
    });
    for selector in [
        "work-context-strip",
        "chat-header-instruction-status",
        "chat-header-instruction-drift",
        "chat-header-proposals",
    ] {
        assert!(!has(vcx, selector), "{selector} must not be in the header");
    }
    // One row of title + one row of chips; nothing grows with the item's state.
    shell.read_with(vcx, |shell, _| {
        assert_eq!(
            shell.chat_header_extra_clearance,
            super::chat_item_ui::CHAT_HEADER_STACKED_CLEARANCE
        )
    });
}

#[gpui::test]
fn the_item_tab_shows_the_item_sheet(cx: &mut TestAppContext) {
    let (_, vcx) = mount(cx, CHAT_API, true, |snapshot| {
        needs_attention(snapshot);
        snapshot
            .instruction_proposals
            .push(crate::work_outline::required_actions::fixture::proposal(
                "p1",
                "s1",
                Some(API),
                CHAT_API,
                "Prefer small PRs",
            ));
    });
    for selector in [
        "chat-item-tab",
        "chat-item-path",
        "chat-item-path-segment-0",
        "chat-item-instructions",
        "chat-item-instructions-edit",
        // The drift line (Compare / Apply) and the proposal line live here now.
        "chat-header-instruction-status",
        "chat-header-instructions-compare",
        "chat-header-proposals",
        "chat-item-runs-in",
        "chat-item-model",
        "chat-item-account",
        "chat-item-links",
        "chat-item-pr-0",
        "chat-item-needs-you",
        "chat-item-answer-0",
    ] {
        assert!(has(vcx, selector), "{selector} should be in the Item tab");
    }
    assert!(!has(vcx, "chat-item-work-item"));
    assert!(!has(vcx, "chat-item-add-to-outline"));
}

#[gpui::test]
fn a_chat_without_an_item_gets_only_runs_in_model_account_and_add_to_outline(
    cx: &mut TestAppContext,
) {
    let (_, vcx) = mount(cx, CHAT_LOOSE_A, true, |_| {});
    for selector in [
        "chat-item-tab",
        "chat-item-runs-in",
        "chat-item-model",
        "chat-item-account",
        "chat-item-add-to-outline",
    ] {
        assert!(has(vcx, selector), "{selector}");
    }
    for selector in [
        "chat-item-path",
        "chat-item-instructions",
        "chat-item-links",
        "chat-item-needs-you",
        "chat-item-plan",
        "chat-item-children",
    ] {
        assert!(!has(vcx, selector), "{selector} needs an item");
    }
}

#[gpui::test]
fn the_badges_open_the_item_tab_and_the_pane_opens_with_it(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, needs_attention);
    shell.read_with(vcx, |shell, cx| assert!(!shell.right_pane_open(cx)));
    let at = vcx
        .debug_bounds("chat-header-badge-attention")
        .unwrap()
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    draw(vcx);
    shell.read_with(vcx, |shell, cx| {
        assert!(shell.right_pane_open(cx));
        assert_eq!(shell.resolved_right_active(cx), RightSurface::Item);
        // The Item tab is not an ordinary, closable tab.
        assert!(shell.right_surface_rows(cx).is_empty());
        assert_eq!(shell.closable_right_surface(cx), None);
    });
}

#[gpui::test]
fn the_item_tab_stays_selected_over_other_tabs_until_one_is_picked(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
    shell.update(vcx, |shell, cx| {
        shell.open_item_tab(cx);
        shell.right_tabs.insert(
            shell.panel_key(cx),
            vec![RightSurface::Subagent(7)],
        );
        cx.notify();
    });
    shell.read_with(vcx, |shell, cx| {
        assert_eq!(shell.resolved_right_active(cx), RightSurface::Item)
    });
    shell.update(vcx, |shell, cx| shell.set_right_active(RightSurface::Picker, cx));
    shell.read_with(vcx, |shell, cx| {
        assert_ne!(shell.resolved_right_active(cx), RightSurface::Item)
    });
}

// ----------------------------------------------------------------------
// Hover card
// ----------------------------------------------------------------------

fn hover_header(vcx: &mut VisualTestContext) {
    let at = vcx.debug_bounds("chat-header-title").unwrap().center();
    vcx.simulate_mouse_move(at, None, gpui::Modifiers::default());
    draw(vcx);
}

fn leave(vcx: &mut VisualTestContext) {
    vcx.simulate_mouse_move(gpui::point(px(890.0), px(590.0)), None, gpui::Modifiers::default());
    draw(vcx);
}

fn wait(vcx: &mut VisualTestContext, ms: u64) {
    vcx.executor().advance_clock(Duration::from_millis(ms));
    draw(vcx);
}

#[gpui::test]
fn the_card_shows_after_the_hover_delay_and_hides_on_mouse_out(cx: &mut TestAppContext) {
    let (_, vcx) = mount(cx, CHAT_API, false, needs_attention);
    assert!(!has(vcx, "chat-item-card"));
    hover_header(vcx);
    wait(vcx, 200);
    assert!(!has(vcx, "chat-item-card"), "not before ~400 ms");
    wait(vcx, 300);
    assert!(has(vcx, "chat-item-card"));
    for selector in [
        "chat-item-card-instructions",
        "chat-item-card-links",
        "chat-item-card-needs-you",
    ] {
        assert!(has(vcx, selector), "{selector}");
    }
    // The card is the condensed form: the header chips already carry the
    // model, account and branch, and the tab holds the rest.
    for selector in [
        "chat-item-card-runs-in",
        "chat-item-card-model",
        "chat-item-card-account",
        "chat-item-card-identifiers",
        "chat-item-card-path",
    ] {
        assert!(!has(vcx, selector), "{selector}");
    }
    assert!(!has(vcx, "chat-item-card-children"));
    leave(vcx);
    wait(vcx, 400);
    assert!(!has(vcx, "chat-item-card"));
}

#[gpui::test]
fn a_quick_pass_over_the_header_shows_nothing(cx: &mut TestAppContext) {
    let (_, vcx) = mount(cx, CHAT_API, false, needs_attention);
    hover_header(vcx);
    wait(vcx, 100);
    leave(vcx);
    wait(vcx, 600);
    assert!(!has(vcx, "chat-item-card"));
}

#[gpui::test]
fn the_shortcut_pins_the_card_open_and_releases_it(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, needs_attention);
    shell.update(vcx, |shell, cx| shell.toggle_chat_item_card_pin(cx));
    draw(vcx);
    assert!(has(vcx, "chat-item-card"), "pinned without any hover");
    hover_header(vcx);
    leave(vcx);
    wait(vcx, 600);
    assert!(has(vcx, "chat-item-card"), "mouse-out does not hide a pin");
    shell.update(vcx, |shell, cx| shell.toggle_chat_item_card_pin(cx));
    draw(vcx);
    assert!(!has(vcx, "chat-item-card"), "released, pointer elsewhere");
}

#[gpui::test]
fn the_card_never_takes_the_keyboard_from_the_composer(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, needs_attention);
    let focus = shell.read_with(vcx, |shell, cx| shell.composer.read(cx).focus_handle(cx));
    vcx.update(|window, cx| window.focus(&focus, cx));
    let focused = |vcx: &mut VisualTestContext| vcx.update(|window, _| focus.is_focused(window));
    assert!(focused(vcx));
    hover_header(vcx);
    wait(vcx, 500);
    assert!(has(vcx, "chat-item-card"));
    assert!(focused(vcx), "showing the card keeps focus");
    let at = vcx.debug_bounds("chat-item-card-instructions").unwrap().center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    draw(vcx);
    assert!(focused(vcx), "clicking inside the card keeps focus");
    shell.update(vcx, |shell, cx| shell.toggle_chat_item_card_pin(cx));
    draw(vcx);
    assert!(focused(vcx), "pinning keeps focus");
    let at = vcx.debug_bounds("chat-header-title").unwrap().center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    draw(vcx);
    assert!(focused(vcx), "clicking the header keeps focus");
}

#[gpui::test]
fn changing_chat_drops_the_card_and_its_pin(cx: &mut TestAppContext) {
    let (shell, vcx) = mount(cx, CHAT_API, false, needs_attention);
    shell.update(vcx, |shell, cx| shell.toggle_chat_item_card_pin(cx));
    draw(vcx);
    assert!(has(vcx, "chat-item-card"));
    shell.update(vcx, |shell, cx| {
        shell.state.update(cx, |state, _| {
            state.selected_chat = Some(CHAT_UI.into());
        });
        shell.active_chat = CHAT_UI.into();
        shell.chat_header_links.insert(
            CHAT_UI.into(),
            ChatHeaderLinkCache {
                status: None,
                fetched_at: Instant::now(),
            },
        );
        cx.notify();
    });
    draw(vcx);
    assert!(!has(vcx, "chat-item-card"));
}

#[gpui::test]
fn the_keys_are_registered_and_listed_in_the_reference(_cx: &mut TestAppContext) {
    use crate::work_outline::hints::shortcut_reference_on;
    let keymap = crate::settings::KeymapConfig::default();
    let groups = shortcut_reference_on(true, &keymap);
    let group = groups
        .iter()
        .find(|group| group.title == "Chat header")
        .expect("the reference lists the chat header keys");
    let keys: Vec<_> = group
        .rows
        .iter()
        .map(|row| (row.label, row.keys.clone().unwrap_or_default()))
        .collect();
    assert!(keys.contains(&("Open the Item tab", "\u{2325}\u{2318};".to_owned())), "{keys:?}");
    assert!(
        keys.contains(&(
            "Pin or release the context card",
            "\u{2325}\u{2318}I".to_owned()
        )),
        "{keys:?}"
    );
    // Both parse and neither shadows a customizable default.
    for combo in [OPEN_ITEM_TAB_COMBO, TOGGLE_ITEM_CARD_COMBO] {
        assert!(gpui::Keystroke::parse(&crate::settings::platform_combo(combo)).is_ok());
        assert!(
            crate::settings::ShortcutId::ALL
                .into_iter()
                .all(|id| id.default_combo() != combo),
            "{combo} clashes"
        );
    }
}

#[gpui::test]
fn the_item_tab_lists_identifiers_with_copy_for_chats_with_and_without_an_item(
    cx: &mut TestAppContext,
) {
    for chat_id in [CHAT_API, CHAT_LOOSE_A] {
        let (shell, vcx) = mount(cx, chat_id, true, |_| {});
        shell.update(vcx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                let chat = state.chats.iter_mut().find(|c| c.id == chat_id).unwrap();
                chat.harness_session_id = Some("sess-123".into());
            })
        });
        draw(vcx);
        for selector in [
            "chat-item-identifiers",
            "chat-item-id-chat",
            "chat-item-id-session",
        ] {
            assert!(has(vcx, selector), "{selector} for {chat_id}");
        }
        let at = vcx.debug_bounds("chat-item-id-chat").unwrap().center();
        vcx.simulate_click(at, gpui::Modifiers::default());
        draw(vcx);
        assert_eq!(
            vcx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
            Some(chat_id)
        );
        let at = vcx.debug_bounds("chat-item-id-session").unwrap().center();
        vcx.simulate_click(at, gpui::Modifiers::default());
        draw(vcx);
        assert_eq!(
            vcx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
            Some("sess-123")
        );
        shell.read_with(vcx, |shell, _| {
            assert_eq!(shell.copied_chat_identity.as_deref(), Some("session"))
        });
    }
}

#[gpui::test]
fn a_chat_without_a_session_id_lists_no_session_row_and_the_header_has_no_identifiers(
    cx: &mut TestAppContext,
) {
    let (_, vcx) = mount(cx, CHAT_API, true, |_| {});
    assert!(has(vcx, "chat-item-id-chat"));
    assert!(!has(vcx, "chat-item-id-session"));
    let (_, vcx) = mount(cx, CHAT_API, false, |_| {});
    assert!(!has(vcx, "chat-item-identifiers"));
}

/// Web and PR links from every surface go through one Shell entry point
/// (`open_web_reference`). Only the model is asserted: tabs, URLs, the
/// route and the external opener.
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod web_references {
    use super::*;
    use crate::shell::Route;
    use gpui::Modifiers;

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
                settings.open_web_links_in_mast = on
            })
        });
    }

    fn click(vcx: &mut VisualTestContext, selector: &'static str, modifiers: Modifiers) {
        let at = vcx.debug_bounds(selector).unwrap().center();
        vcx.simulate_click(at, modifiers);
        draw(vcx);
    }

    #[gpui::test]
    fn the_header_pr_chip_opens_a_side_tab_and_focuses_it_the_second_time(
        cx: &mut TestAppContext,
    ) {
        let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
        select(&shell, vcx, CHAT_API, Route::Chat);
        click(vcx, "chat-header-chip-pr-0", Modifiers::default());
        assert_eq!(tabs(&shell, vcx), [PR_7]);
        assert!(vcx.opened_url().is_none(), "not the external browser");
        shell.read_with(vcx, |shell, cx| {
            assert!(shell.right_pane_open(cx));
            let first = *shell.browsers.keys().min().unwrap();
            assert_eq!(
                shell.resolved_right_active(cx),
                RightSurface::Browser(first)
            );
        });
        // Another page opens a second tab; the chip again focuses the first.
        open(&shell, vcx, "https://example.com/docs", false);
        assert_eq!(tabs(&shell, vcx).len(), 2);
        click(vcx, "chat-header-chip-pr-0", Modifiers::default());
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
    fn the_item_tab_pr_link_follows_the_same_rules(cx: &mut TestAppContext) {
        let (shell, vcx) = mount(cx, CHAT_API, true, |_| {});
        select(&shell, vcx, CHAT_API, Route::Chat);
        click(vcx, "chat-item-pr-0", Modifiers::default());
        assert_eq!(tabs(&shell, vcx), [PR_7]);
        assert!(vcx.opened_url().is_none());
        // Again: focus, no duplicate.
        click(vcx, "chat-item-pr-0", Modifiers::default());
        assert_eq!(tabs(&shell, vcx).len(), 1);
        // ⌘-click goes external.
        click(vcx, "chat-item-pr-0", Modifiers::secondary_key());
        assert_eq!(vcx.opened_url().as_deref(), Some(PR_7));
        assert_eq!(tabs(&shell, vcx).len(), 1);
    }

    #[gpui::test]
    fn command_click_and_the_off_setting_open_the_external_browser(cx: &mut TestAppContext) {
        let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
        select(&shell, vcx, CHAT_API, Route::Chat);
        click(vcx, "chat-header-chip-pr-0", Modifiers::secondary_key());
        assert_eq!(vcx.opened_url().as_deref(), Some(PR_7));
        assert!(tabs(&shell, vcx).is_empty());
        // The explicit external entry (context menu).
        open(&shell, vcx, "https://example.com/menu", true);
        assert_eq!(
            vcx.opened_url().as_deref(),
            Some("https://example.com/menu")
        );
        // With the setting off nothing opens in Mast, chip included.
        set_open_in_mast(vcx, false);
        click(vcx, "chat-header-chip-pr-0", Modifiers::default());
        assert_eq!(vcx.opened_url().as_deref(), Some(PR_7));
        open(&shell, vcx, "https://example.com/off", false);
        assert_eq!(vcx.opened_url().as_deref(), Some("https://example.com/off"));
        assert!(tabs(&shell, vcx).is_empty());
    }

    #[gpui::test]
    fn a_route_without_a_side_pane_goes_to_the_review_chat(cx: &mut TestAppContext) {
        let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
        shell.update(vcx, |shell, cx| {
            shell.route = Route::PrReviewChats;
            cx.notify();
        });
        // PR #7 is reviewed in CHAT_API (assigned to its review item).
        open(&shell, vcx, PR_7, false);
        shell.read_with(vcx, |shell, cx| {
            assert!(matches!(shell.route, Route::Chat));
            assert_eq!(shell.active_chat, CHAT_API);
            assert!(shell.right_pane_open(cx), "the browser sits beside the chat");
        });
        assert_eq!(tabs(&shell, vcx), [PR_7]);
        assert!(vcx.opened_url().is_none());
    }

    #[gpui::test]
    fn a_pr_without_a_review_chat_uses_the_selected_chat_else_goes_external(
        cx: &mut TestAppContext,
    ) {
        let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
        // Nothing to host a tab: the external browser.
        shell.update(vcx, |shell, cx| {
            shell.route = Route::Overview;
            shell.state.update(cx, |state, cx| state.select_chat(None, cx));
        });
        vcx.run_until_parked();
        open(&shell, vcx, PR_99, false);
        assert_eq!(vcx.opened_url().as_deref(), Some(PR_99));
        assert!(tabs(&shell, vcx).is_empty());
        assert!(matches!(shell.read_with(vcx, |s, _| s.route), Route::Overview));
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
        let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
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
        let (shell, vcx) = mount(cx, CHAT_API, false, |_| {});
        select(&shell, vcx, CHAT_API, Route::Chat);
        open(&shell, vcx, "file:///etc/passwd", false);
        assert!(tabs(&shell, vcx).is_empty());
        assert!(vcx.opened_url().is_none());
        shell.read_with(vcx, |shell, _| assert!(shell.sidebar_notice.is_some()));
    }
}

#[gpui::test]
fn header_chips_carry_icons_and_the_title_stacks_under_the_breadcrumb(cx: &mut TestAppContext) {
    let (_, vcx) = mount(cx, CHAT_API, false, |_| {});
    for selector in [
        "chat-header-chip-model-icon",
        "chat-header-chip-account-icon",
        "chat-header-chip-branch-icon",
        "chat-header-chip-pr-0-icon",
    ] {
        assert!(has(vcx, selector), "{selector}");
    }
    let crumb = vcx.debug_bounds("chat-header-breadcrumb").unwrap();
    let title = vcx.debug_bounds("chat-header-title").unwrap();
    assert!(title.top() >= crumb.bottom(), "title sits under the breadcrumb");
}

#[gpui::test]
fn item_tab_sections_lead_with_icons(cx: &mut TestAppContext) {
    let (_, vcx) = mount(cx, CHAT_API, true, needs_attention);
    for name in [
        "path",
        "instructions",
        "runs-in",
        "model",
        "account",
        "links",
        "needs-you",
        "identifiers",
    ] {
        let selector: &'static str = Box::leak(format!("chat-item-{name}-icon").into_boxed_str());
        assert!(has(vcx, selector), "{selector}");
    }
}

#[gpui::test]
fn chip_tones_use_theme_tokens(_cx: &mut TestAppContext) {
    use super::chat_item_ui::{ChipTone, chip_palette};
    let theme = Theme::default();
    assert_eq!(chip_palette(&theme, ChipTone::Warn), (theme.warning, theme.warning));
    assert_eq!(chip_palette(&theme, ChipTone::Link), (theme.accent, theme.text));
    assert_eq!(chip_palette(&theme, ChipTone::Dim).1, theme.text_muted);
}

#[gpui::test]
fn the_item_tab_keeps_rows_tight_and_actions_together(cx: &mut TestAppContext) {
    let (_, vcx) = mount(cx, CHAT_API, true, needs_attention);
    // A link is one text line, not an empty line plus its label.
    let pr = vcx.debug_bounds("chat-item-pr-0").unwrap();
    assert!(pr.size.height < px(24.0), "{:?}", pr.size);
    // Compare and Apply sit side by side.
    let compare = vcx.debug_bounds("chat-header-instructions-compare").unwrap();
    let apply = vcx.debug_bounds("chat-header-instructions-apply");
    if let Some(apply) = apply {
        assert_eq!(compare.top(), apply.top());
    }
    // The account is a badge.
    assert!(has(vcx, "chat-item-account-badge"));
}

#[gpui::test]
fn child_rows_are_tight_single_lines(cx: &mut TestAppContext) {
    let (_, vcx) = mount(cx, CHAT_API, true, |_| {});
    assert!(has(vcx, "chat-item-children"));
    let row = vcx
        .debug_bounds(Box::leak(format!("chat-item-child-{API_AUTH}").into_boxed_str()))
        .expect("the child row");
    assert!(row.size.height < px(24.0), "{:?}", row.size);
}
