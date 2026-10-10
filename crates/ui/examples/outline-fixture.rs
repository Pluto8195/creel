//! Isolated, offline Work Outline fixture: an Overview over two Spaces fed the
//! store fixture (three-level outline, associated / child / dangling /
//! unassociated / projectless chats), opened in Outline mode. Run on a
//! dedicated display:
//!
//! `cargo run -p zeron-ui --example outline-fixture`
//!
//! ZERON_PALETTE_LIGHT selects the light appearance; ZERON_SIDEBAR_COMPACT the
//! compact row density.
use gpui::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use zeron_ui::*;

fn chat(id: &str, space: Option<&str>, title: &str, minutes_ago: i64) -> zeron_proto::Chat {
    let mut chat: zeron_proto::Chat = serde_json::from_value(serde_json::json!({
        "id": id, "deviceId": "local", "title": title, "archived": false,
        "createdAt": "2026-09-08T00:00:00Z",
        "config": {"harness": "claude-code", "model": "claude-sonnet-4-6", "reasoning": null, "sandbox": "workspace-write"}
    }))
    .unwrap();
    chat.space_id = space.map(Into::into);
    chat.last_message_at = Some(chrono::Utc::now() - chrono::Duration::minutes(minutes_ago));
    chat
}

fn main() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let _guard = runtime.enter();
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let temp = tempfile::tempdir()?;
    let data = temp.path().to_path_buf();
    gpui_platform::application().with_assets(icons::Assets).run(move |cx| {
        gpui_tokio::init(cx);
        gpui_base::init(cx);
        let mut settings = settings::UiSettings::default();
        settings.sidebar_compact = std::env::var_os("ZERON_SIDEBAR_COMPACT").is_some();
        settings.surface = zeron_theme::SurfacePreference::Frosted;
        settings.save(&data).unwrap();
        settings::init(settings.clone(), data.clone(), cx);
        let fonts = typography::register_fonts(cx);
        typography::init(settings.ui_font_family.clone(), settings.ui_font_size, settings.terminal_font_family.clone(), settings.terminal_font_size, settings.code_font_family.clone(), settings.code_font_size, fonts, cx);
        theme_library::init(data.clone(), cx);
        appearance::init(if std::env::var_os("ZERON_PALETTE_LIGHT").is_some() { appearance::AppearanceMode::Light } else { appearance::AppearanceMode::Dark }, settings.theme_selection, settings.accent, settings.surface, cx);
        history::init(settings.git_history_columns, settings.git_history_column_widths,
            settings.git_history_column_order, settings.git_history_author_display, cx);
        composer::init(cx, settings.composer_send_behavior);
        terminal::panel::init(cx);
        app_menus::init(cx);
        cx.bind_keys(overview::Overview::work_outline_key_bindings());

        let state = cx.new(|_| {
            let mut s = state::AppState::new();
            s.connection = zeron_proto::view::ConnectionStatus::Ready;
            s.workspace_scope = Some(zeron_proto::WorkspaceScope::Local);
            s.local_device_id = Some("local".into());
            s.data_dir = Some(data.clone());
            s.chats_synced = true;
            s.spaces_synced = true;
            s.devices = vec![serde_json::from_value(serde_json::json!({"id":"local","name":"This device","platform":std::env::consts::OS,"lastSeenAt":null})).unwrap()];
            s.spaces = vec![
                serde_json::from_value(serde_json::json!({"id":"alpha","deviceId":"local","name":"fieldnotes","path":"/tmp/fieldnotes","createdAt":"2026-09-08T00:00:00Z"})).unwrap(),
                serde_json::from_value(serde_json::json!({"id":"beta","deviceId":"local","name":"server","path":"/tmp/server","createdAt":"2026-09-08T00:00:00Z"})).unwrap(),
            ];
            // Space `alpha` gets the fixture verbatim; `beta` the same shape
            // with ids suffixed `-1` (see `Overview::show_work_outline_fixture`).
            let titles = [
                ("chat-api", "Design the public API"),
                ("chat-auth", "Implement auth endpoints"),
                ("chat-ui", "Build the launch screens"),
                ("chat-loose-a", "Investigate flaky sync test"),
                ("chat-loose-b", "Draft release notes"),
                ("chat-orphan", "Old spike on caching"),
            ];
            for (ix, (id, title)) in titles.iter().enumerate() {
                let mut c = chat(id, Some("alpha"), title, ix as i64 * 9);
                if *id == "chat-api" {
                    c.branch = Some("fieldnotes/public-api".into());
                }
                s.chats.push(c);
                s.chats.push(chat(&format!("{id}-1"), Some("beta"), title, ix as i64 * 13 + 5));
            }
            let mut child = chat("chat-api-review", Some("alpha"), "Review API naming (subagent)", 3);
            child.parent_chat_id = Some("chat-api".into());
            s.chats.push(child);
            s.chats.push(chat("chat-scratch", None, "Scratch: regex help", 40));
            s
        });
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(gpui::point(px(12.), px(30.)), size(px(1100.), px(760.))))),
                    titlebar: Some(gpui::TitlebarOptions { title: None, appears_transparent: true, traffic_light_position: Some(gpui::point(px(14.), px(14.))) }),
                    app_owns_titlebar_drag: true,
                    ..Default::default()
                },
                |_, cx| {
                    let transcript = cx.new(|cx| transcript::Transcript::new(state.clone(), cx));
                    let composer = cx.new(|cx| composer::Composer::new(state.clone(), cx));
                    cx.new(|cx| overview::Overview::new(state.clone(), gpui::WeakEntity::new_invalid(), transcript, composer, cx))
                },
            )
            .unwrap();
        window
            .update(cx, |overview, window, cx| overview.show_work_outline_fixture(window, cx))
            .unwrap();
        cx.activate(true);
    });
    Ok(())
}
