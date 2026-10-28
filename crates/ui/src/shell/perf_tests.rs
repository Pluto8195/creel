//! Synthetic busy session for the `AppState` observers: 250 chats, then 50
//! streaming updates to one chat that is not selected and 50 to the selected
//! chat. Each view entity is observed from the outside, so the counts are the
//! notifies (repaints requested) per view, whatever the view does internally.
//! Run with `--nocapture` to print the table.

use super::*;
use gpui::{AppContext, Entity, TestAppContext};
use std::{cell::Cell, rc::Rc};

type Counter = (&'static str, Rc<Cell<u64>>);

fn watch<T: 'static>(cx: &mut TestAppContext, name: &'static str, entity: &Entity<T>) -> Counter {
    let count = Rc::new(Cell::new(0));
    let seen = count.clone();
    cx.update(|cx| {
        cx.observe(entity, move |_, _| { if std::env::var("PERF_BT").is_ok_and(|v| v == name) && seen.get() == 3 { eprintln!("{}", std::backtrace::Backtrace::force_capture()); } seen.set(seen.get() + 1) })
            .detach()
    });
    (name, count)
}

fn chat_row(index: usize) -> zeron_proto::Chat {
    use chrono::TimeZone;
    zeron_proto::Chat {
        id: format!("c{index}"),
        device_id: "local".into(),
        title: Some(format!("Chat {index}")),
        archived: false,
        cwd: Some(format!("/work/repo{}", index % 25)),
        branch: None,
        checkout_id: None,
        source_context: None,
        config: None,
        last_message_preview: None,
        last_message_at: Some(Utc.timestamp_opt(1_700_000_000 + index as i64, 0).unwrap()),
        created_at: Utc.timestamp_opt(1_600_000_000, 0).unwrap(),
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
    }
}

fn totals(counters: &[Counter]) -> Vec<(&'static str, u64)> {
    counters.iter().map(|(n, c)| (*n, c.get())).collect()
}

fn delta(before: &[(&'static str, u64)], after: &[(&'static str, u64)]) -> Vec<(&'static str, u64)> {
    before
        .iter()
        .zip(after)
        .map(|((n, b), (_, a))| (*n, a - b))
        .collect()
}

#[gpui::test]
fn streaming_one_chat_among_many_notifies_few_views(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        settings::init(settings::UiSettings::default(), dir.path(), cx);
        crate::history::init(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            cx,
        );
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
    });
    let state = cx.new(|_| AppState::new());
    let window = cx.add_window({
        let state = state.clone();
        let data_dir = dir.path().to_path_buf();
        move |_, cx| {
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir,
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: zeron_proto::HarnessId::Mock,
                },
                cx,
            )
        }
    });
    state.update(cx, |state, _| {
        state.chats = (0..250).map(chat_row).collect();
        state.chats_synced = true;
        state.selected_chat = Some("c0".into());
        state.data_dir = Some(dir.path().to_path_buf());
    });
    let shell = window.root(cx).unwrap();
    let (transcript, composer) = shell.read_with(cx, |s, _| (s.transcript.clone(), s.composer.clone()));
    let pickers = composer.read_with(cx, |c, _| c.pickers().clone());
    let weak = shell.downgrade();
    let chat_types = cx.new(|cx| {
        crate::chat_type_view::ChatTypeView::new(
            state.clone(),
            weak.clone(),
            crate::chat_type_view::ChatViewKind::PrReview,
            cx,
        )
    });
    let accounts = cx.new(|cx| crate::settings::accounts::AccountsPage::new(state.clone(), cx));
    let archived = cx.new(|cx| crate::settings::archived::ArchivedPage::new(state.clone(), cx));
    let devices = cx.new(|cx| crate::settings::devices::DevicesPage::new(state.clone(), cx));
    let linear = cx.new(|cx| crate::linear::settings_page::LinearSettingsPage::new(state.clone(), cx));
    let changes = cx.new(|cx| crate::changes::Changes::new(state.clone(), cx));
    let terminal = cx.new(|cx| crate::terminal::panel::TerminalPanel::new(state.clone(), cx));
    let files = cx.new(|cx| {
        crate::files::FilesSurface::new_explorer(state.clone(), "c0".into(), false, cx)
    });
    cx.run_until_parked();

    let counters = vec![
        watch(cx, "shell", &shell),
        watch(cx, "transcript", &transcript),
        watch(cx, "composer", &composer),
        watch(cx, "pickers", &pickers),
        watch(cx, "chat_type_view", &chat_types),
        watch(cx, "settings accounts", &accounts),
        watch(cx, "settings archived", &archived),
        watch(cx, "settings devices", &devices),
        watch(cx, "settings linear", &linear),
        watch(cx, "changes", &changes),
        watch(cx, "terminal panel", &terminal),
        watch(cx, "files", &files),
    ];
    let start = totals(&counters);

    // A: a live heartbeat for a chat nobody has open: its session row's
    // lease timestamp moves, no visible field does.
    for i in 0..50 {
        state.update(cx, |state, cx| {
            state.sessions = vec![zeron_proto::Session {
                last_completed_turn: None,
                chat_id: "c1".into(),
                device_id: "local".into(),
                status: zeron_proto::SessionStatus::Working,
                started_at: None,
                updated_at: Utc::now() + chrono::TimeDelta::milliseconds(i),
            }];
            cx.notify();
        });
        cx.run_until_parked();
    }
    let after_heartbeats = totals(&counters);

    // B: the same chat's row gets new preview text each time (a real row
    // change, visible in lists that show previews).
    for i in 0..50 {
        state.update(cx, |state, cx| {
            state.chats[1].last_message_preview = Some(format!("preview {i}"));
            cx.notify();
        });
        cx.run_until_parked();
    }
    let after_previews = totals(&counters);

    // C: streaming into the selected chat: its transcript changes, no
    // chat/space/session field does.
    for i in 0..50 {
        state.update(cx, |state, cx| {
            state.transcript.push(zeron_doc::SessionMessageEntry {
                id: format!("m{i}"),
                role: zeron_doc::MessageRole::Assistant,
                parts: vec![zeron_doc::MessagePart::Text {
                    id: "t0".into(),
                    text: format!("token {i}"),
                }],
                created_at: i,
                device_id: "local".into(),
                status: None,
                continuation_of: None,
            });
            state.transcript_revision = state.transcript_revision.wrapping_add(1);
            cx.notify();
        });
        cx.run_until_parked();
    }
    let after_selected = totals(&counters);

    eprintln!("PERF notifies per view (50 updates each)");
    eprintln!("{:<22}{:>12}{:>12}{:>12}", "view", "heartbeat", "preview", "selected");
    let a = delta(&start, &after_heartbeats);
    let b = delta(&after_heartbeats, &after_previews);
    let c = delta(&after_previews, &after_selected);
    for (((name, a), (_, b)), (_, c)) in a.iter().zip(&b).zip(&c) {
        eprintln!("{name:<22}{a:>12}{b:>12}{c:>12}");
    }
}
