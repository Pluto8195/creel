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
            super::chat_item_ui::CHAT_HEADER_TITLE_ROW_CLEARANCE
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
        "chat-item-card-runs-in",
        "chat-item-card-model",
        "chat-item-card-needs-you",
    ] {
        assert!(has(vcx, selector), "{selector}");
    }
    // The card is the condensed form: no full path or children list.
    assert!(!has(vcx, "chat-item-card-path"));
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
    let at = vcx.debug_bounds("chat-item-card-runs-in").unwrap().center();
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
