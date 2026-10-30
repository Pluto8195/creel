use super::*;
use chrono::TimeZone;
use mast_proto::{AgentAccount, AgentUsageWindow, InterruptedOutcome, InterruptedRun, ManagedAccount};

use crate::running_chats::{self, Input, Toggled};

fn now() -> DateTime<Utc> {
    // A Friday.
    Utc.with_ymd_and_hms(2026, 10, 30, 12, 0, 0).unwrap()
}

fn input(id: &str, state: RunState, mins_ago: i64, account: Option<&str>) -> Input {
    Input {
        chat_id: id.into(),
        parent_id: None,
        space_id: Some("m".into()),
        title: format!("chat {id}"),
        state,
        since: Some(now() - chrono::Duration::minutes(mins_ago)),
        activity: String::new(),
        item_path: None,
        account: account.map(str::to_owned),
        model: None,
        background: 0,
    }
}

fn snapshot(inputs: &[Input]) -> Snapshot {
    running_chats::snapshot(inputs, &[("m".into(), "Mast".into())], &Toggled::new(), now())
}

fn window(label: &str, fraction: f32, resets_in_mins: i64) -> mast_proto::AgentUsageWindow {
    AgentUsageWindow {
        label: label.into(),
        used_fraction: fraction,
        resets_at: Some(now() + chrono::Duration::minutes(resets_in_mins)),
    }
}

fn menu(snapshot: &Snapshot, usage: Option<&[UsageAccount]>, pending: Option<PendingBuild>) -> MenuView {
    build_menu(
        &MenuInput {
            snapshot,
            interrupted: None,
            usage,
            pending,
            light: AppLight::Input,
            now: now(),
        },
        &Utc,
    )
}

fn texts(view: &MenuView) -> Vec<String> {
    view.items
        .iter()
        .map(|item| match item {
            Item::Title(text) | Item::Section(text) => text.clone(),
            Item::Chat { text, .. } | Item::Line { text, .. } => text.text.clone(),
            Item::Action { label, .. } => label.clone(),
            Item::Separator => "---".into(),
        })
        .collect()
}

// --- reset countdowns -------------------------------------------------------

#[test]
fn resets_within_a_day_count_down() {
    let at = |mins: i64| Some(now() + chrono::Duration::minutes(mins));
    assert_eq!(format_reset(at(72), now(), &Utc), "resets in 1h 12m");
    assert_eq!(format_reset(at(38), now(), &Utc), "resets in 38m");
    assert_eq!(format_reset(at(120), now(), &Utc), "resets in 2h");
    assert_eq!(format_reset(at(23 * 60 + 59), now(), &Utc), "resets in 23h 59m");
    assert_eq!(
        format_reset(Some(now() + chrono::Duration::seconds(20)), now(), &Utc),
        "resets in <1m"
    );
}

#[test]
fn resets_beyond_a_day_name_the_weekday_and_time() {
    // Friday 12:00 + 6 days = Thursday 12:00 the week after... use a fixed target.
    let thursday = Utc.with_ymd_and_hms(2026, 11, 5, 9, 0, 0).unwrap();
    assert_eq!(format_reset(Some(thursday), now(), &Utc), "resets Thu 9:00");
    let exactly_a_day = now() + chrono::Duration::hours(24);
    assert_eq!(format_reset(Some(exactly_a_day), now(), &Utc), "resets Sat 12:00");
}

#[test]
fn a_reset_in_the_past_is_resetting_and_no_reset_is_blank() {
    assert_eq!(
        format_reset(Some(now() - chrono::Duration::minutes(1)), now(), &Utc),
        "resetting"
    );
    assert_eq!(format_reset(None, now(), &Utc), "");
}

#[test]
fn reset_weekday_follows_the_given_time_zone() {
    let late = Utc.with_ymd_and_hms(2026, 11, 5, 23, 30, 0).unwrap();
    let plus_two = chrono::FixedOffset::east_opt(2 * 3600).unwrap();
    assert_eq!(format_reset(Some(late), now(), &Utc), "resets Thu 23:30");
    assert_eq!(format_reset(Some(late), now(), &plus_two), "resets Fri 1:30");
}

// --- bars and percentages ----------------------------------------------------

#[test]
fn bars_fill_by_tenths_and_clamp() {
    assert_eq!(usage_bar(0.0), "▱▱▱▱▱▱▱▱▱▱");
    assert_eq!(usage_bar(0.62), "▰▰▰▰▰▰▱▱▱▱");
    assert_eq!(usage_bar(1.0), "▰▰▰▰▰▰▰▰▰▰");
    assert_eq!(usage_bar(7.0), "▰▰▰▰▰▰▰▰▰▰");
    assert_eq!(usage_bar(-1.0), "▱▱▱▱▱▱▱▱▱▱");
    assert_eq!(percent(0.915), 92);
    assert_eq!(percent(2.0), 100);
}

// --- near-limit rule ----------------------------------------------------------

fn account(name: &str, session: f32, week: f32) -> UsageAccount {
    UsageAccount {
        name: name.into(),
        windows: vec![
            UsageWindow {
                label: "5h".into(),
                fraction: session,
                resets_at: Some(now() + chrono::Duration::minutes(38)),
            },
            UsageWindow {
                label: "wk".into(),
                fraction: week,
                resets_at: None,
            },
        ],
    }
}

#[test]
fn near_limit_starts_at_eighty_percent_on_either_window() {
    assert!(!account("a", 0.79, 0.10).near_limit());
    assert!(account("a", 0.80, 0.10).near_limit());
    assert!(account("a", 0.10, 0.85).near_limit(), "the weekly window counts");
    assert_eq!(account("a", 0.94, 0.1).level(), UsageLevel::Warn);
    assert_eq!(account("a", 0.1, 0.95).level(), UsageLevel::Critical);
    assert!(!UsageAccount { name: "x".into(), windows: vec![] }.near_limit());
}

#[test]
fn a_near_limit_account_is_flagged_and_toned_in_the_menu() {
    let usage = [account("Default \u{b7} Claude Code", 0.62, 0.41), account("Personal \u{b7} Claude Code", 0.91, 0.73)];
    let view = menu(&snapshot(&[]), Some(&usage), None);
    let lines = texts(&view);
    assert!(!lines.iter().any(|l| l.starts_with("Default") && l.contains("near limit")));
    assert!(lines.iter().any(|l| l.starts_with("Personal") && l.contains("\u{26a0} near limit")));
    let personal_5h = view
        .items
        .iter()
        .find_map(|item| match item {
            Item::Line { text, .. } if text.text.contains("91%") => Some(text),
            _ => None,
        })
        .expect("the 91% line");
    assert!(personal_5h.text.contains("5h  ▰▰▰▰▰▰▰▰▰▱  91%"), "{}", personal_5h.text);
    assert!(personal_5h.text.ends_with("resets in 38m"));
    assert!(personal_5h.spans.iter().any(|s| s.tone == Tone::Bromine));
    let critical = [account("x", 0.97, 0.1)];
    let view = menu(&snapshot(&[]), Some(&critical), None);
    assert!(view.items.iter().any(|item| matches!(item,
        Item::Line { text, .. } if text.spans.iter().any(|s| s.tone == Tone::Cadmium))));
}

// --- usage accounts -----------------------------------------------------------

fn slot(id: &str, harness: HarnessId, active: bool, windows: Vec<AgentUsageWindow>) -> AgentAccount {
    AgentAccount {
        id: id.into(),
        harness,
        email: None,
        plan_label: None,
        active,
        usage_windows: windows,
        display_name: None,
        organization: None,
        auth_kind: None,
        switchable: true,
        saved_at: None,
        promoted: false,
    }
}

fn managed(label: &str, harness: HarnessId, signed_in: bool) -> ManagedAccount {
    ManagedAccount {
        id: label.into(),
        harness,
        label: label.into(),
        email: None,
        signed_in,
        plan_label: None,
        usage_windows: vec![window("Session", 0.2, 30), window("Week", 0.5, 3000)],
    }
}

#[test]
fn usage_accounts_list_default_then_managed_per_harness_and_skip_the_unused() {
    let global = AgentAccountsSnapshot {
        accounts: vec![
            slot("saved", HarnessId::ClaudeCode, false, vec![window("Session", 0.9, 5)]),
            slot("live", HarnessId::ClaudeCode, true, vec![window("Session", 0.62, 72), window("Week", 0.41, 3000)]),
            slot("cx", HarnessId::Codex, true, vec![window("Session", 0.18, 220), window("Month", 0.5, 9000)]),
            slot("cursor", HarnessId::Cursor, true, vec![window("Session", 0.5, 5)]),
        ],
        warnings: vec![],
    };
    let managed = ManagedAccountsSnapshot {
        accounts: vec![
            managed("Personal", HarnessId::ClaudeCode, true),
            managed("Signed out", HarnessId::ClaudeCode, false),
            managed("Work", HarnessId::Codex, true),
        ],
        ..Default::default()
    };
    let accounts = usage_accounts(Some(&global), Some(&managed));
    let names: Vec<&str> = accounts.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Default \u{b7} Claude Code",
            "Personal \u{b7} Claude Code",
            "Default \u{b7} Codex",
            "Work \u{b7} Codex"
        ]
    );
    let labels: Vec<&str> = accounts[0].windows.iter().map(|w| w.label.as_str()).collect();
    assert_eq!(labels, ["5h", "wk"]);
    assert_eq!(accounts[2].windows[1].label, "mo");
    assert!(usage_accounts(None, None).is_empty());
}

// --- menu sections and order ----------------------------------------------------

#[test]
fn the_menu_lists_needs_you_before_running_then_usage_then_the_footer() {
    let snap = snapshot(&[
        input("a", RunState::Working, 24, Some("Default")),
        input("b", RunState::Awaiting, 2, None),
        input("c", RunState::Working, 6, None),
        input("d", RunState::Errored, 11, None),
        input("e", RunState::Working, 63, Some("Personal")),
    ]);
    let usage = [account("Default \u{b7} Claude Code", 0.62, 0.41)];
    let pending = PendingBuild { changes: Some(4), armed: false };
    let view = menu(&snap, Some(&usage), Some(pending));
    let lines = texts(&view);
    assert_eq!(lines[0], "5 running \u{b7} 2 need you");
    let at = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("{needle}: {lines:?}"));
    assert_eq!(lines[1], "Needs you \u{b7} 2");
    assert!(at("chat b") < at("chat d") || at("chat d") < at("Running"));
    assert!(at("Needs you") < at("Running \u{b7} 3"));
    assert!(at("chat d") < at("Running \u{b7} 3"), "errored chats are in Needs you");
    assert!(at("Running \u{b7} 3") < at("chat a"));
    assert!(at("chat e") < at("Usage"));
    assert!(at("Usage") < at("Restart when idle"));
    assert!(at("Restart when idle") < at("Open Mast"));
    assert_eq!(lines.last().unwrap(), "Quit Mast");
    assert!(lines.iter().any(|l| l == "Restart when idle  \u{b7}  4 changes"));
    // A running row names its account and elapsed time; a needs-you row says why.
    assert!(lines[at("chat a")].ends_with("Default \u{b7} 24m"));
    assert!(lines[at("chat b")].ends_with("needs you 2m"));
    assert!(lines[at("chat d")].ends_with("errored 11m"));
}

#[test]
fn clicking_a_chat_row_opens_that_chat() {
    let snap = snapshot(&[input("b", RunState::Awaiting, 2, None)]);
    let view = menu(&snap, None, None);
    assert!(view.items.iter().any(|item| matches!(item,
        Item::Chat { chat_id, .. } if chat_id == "b")));
}

#[test]
fn restart_shows_only_with_a_pending_build_and_once_armed_is_a_status_line() {
    let snap = snapshot(&[input("a", RunState::Working, 1, None)]);
    assert!(!texts(&menu(&snap, None, None)).iter().any(|l| l.contains("Restart")));
    let armed = menu(&snap, None, Some(PendingBuild { changes: None, armed: true }));
    assert!(texts(&armed).iter().any(|l| l == "Restarting when idle\u{2026}"));
    assert!(!armed.items.iter().any(|item| matches!(item,
        Item::Action { action: MenuAction::RestartWhenIdle, .. })));
    let unarmed = menu(&snap, None, Some(PendingBuild { changes: None, armed: false }));
    assert!(texts(&unarmed).iter().any(|l| l == "Restart when idle  \u{b7}  new build"));
}

#[test]
fn nothing_running_has_the_footer_only() {
    let view = menu(&snapshot(&[]), Some(&[]), None);
    assert_eq!(texts(&view), ["Nothing running", "---", "Open Mast", "Quit Mast"]);
}

#[test]
fn usage_loads_then_unavailable_accounts_say_so() {
    let loading = texts(&menu(&snapshot(&[]), None, None));
    assert!(loading.iter().any(|l| l == "Usage") && loading.iter().any(|l| l == "Loading\u{2026}"));
    let empty = [UsageAccount { name: "Default \u{b7} Codex".into(), windows: vec![] }];
    let lines = texts(&menu(&snapshot(&[]), Some(&empty), None));
    assert!(lines.iter().any(|l| l == "usage unavailable"));
}

#[test]
fn long_lists_cap_and_point_to_mast() {
    let inputs: Vec<Input> = (0..12)
        .map(|i| input(&format!("w{i:02}"), RunState::Working, i + 1, None))
        .collect();
    let view = menu(&snapshot(&inputs), None, None);
    let chats = view.items.iter().filter(|i| matches!(i, Item::Chat { .. })).count();
    assert_eq!(chats, MAX_CHAT_ROWS);
    assert!(texts(&view).iter().any(|l| l == "4 more in Mast\u{2026}"));
}

#[test]
fn titles_are_truncated_with_an_ellipsis() {
    assert_eq!(truncate_title("short"), "short");
    let long = "x".repeat(80);
    let cut = truncate_title(&long);
    assert_eq!(cut.chars().count(), MAX_TITLE_CHARS);
    assert!(cut.ends_with('\u{2026}'));
}

// --- interrupted -------------------------------------------------------------------

#[test]
fn the_interrupted_block_offers_resume_all_for_chats_needing_a_decision() {
    let runs = [
        InterruptedRun { chat_id: "r".into(), interrupted_at_ms: now().timestamp_millis(), outcome: InterruptedOutcome::Resumed },
        InterruptedRun { chat_id: "s".into(), interrupted_at_ms: now().timestamp_millis(), outcome: InterruptedOutcome::Exhausted },
        InterruptedRun { chat_id: "t".into(), interrupted_at_ms: now().timestamp_millis(), outcome: InterruptedOutcome::Stale },
    ];
    let block = running_chats::interrupted_block(&runs, |id| Some(format!("chat {id}"))).unwrap();
    let snap = snapshot(&[]);
    let view = build_menu(
        &MenuInput {
            snapshot: &snap,
            interrupted: Some(&block),
            usage: Some(&[]),
            pending: None,
            light: AppLight::Idle,
            now: now(),
        },
        &Utc,
    );
    let lines = texts(&view);
    assert!(lines.iter().any(|l| l == "Interrupted by the restart at 12:00 \u{b7} 3"));
    assert!(lines.iter().any(|l| l == "1 resumed automatically"));
    assert!(lines.iter().any(|l| l == "Resume all (2)"));
    let resume = view.items.iter().find_map(|item| match item {
        Item::Action { action: MenuAction::ResumeAll(ids), .. } => Some(ids.clone()),
        _ => None,
    });
    assert_eq!(resume, Some(vec!["s".to_owned(), "t".to_owned()]));
}

// --- glyph ----------------------------------------------------------------------------

fn pixel(rgba: &[u8], x: u32, y: u32) -> [u8; 4] {
    let at = ((y * GLYPH_PX + x) * 4) as usize;
    [rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]]
}

#[test]
fn the_glyph_is_the_lamp_colour_in_the_middle_and_clear_in_the_corners() {
    for light in AppLight::ALL {
        let rgba = lamp_rgba(Glyph { light, ring: false });
        assert_eq!(rgba.len(), (GLYPH_PX * GLYPH_PX * 4) as usize);
        let [r, g, b, a] = pixel(&rgba, GLYPH_PX / 2, GLYPH_PX / 2);
        assert_eq!([r, g, b], lamp_color(light), "{light:?}");
        assert_eq!(a, 255);
        assert_eq!(pixel(&rgba, 0, 0)[3], 0, "{light:?} corner is clear");
    }
}

#[test]
fn every_state_has_its_own_colour() {
    let colours: Vec<[u8; 3]> = AppLight::ALL.iter().map(|l| lamp_color(*l)).collect();
    for (i, a) in colours.iter().enumerate() {
        for b in &colours[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

#[test]
fn a_pending_build_adds_a_ring_and_nothing_else_changes_inside() {
    let plain = lamp_rgba(Glyph { light: AppLight::Working, ring: false });
    let ringed = lamp_rgba(Glyph { light: AppLight::Working, ring: true });
    // On the ring's radius (x = centre + 18).
    let on_ring = (GLYPH_PX / 2 + 18, GLYPH_PX / 2);
    assert_eq!(pixel(&plain, on_ring.0, on_ring.1)[3], 0);
    assert_eq!(pixel(&ringed, on_ring.0, on_ring.1), [RING_COLOR[0], RING_COLOR[1], RING_COLOR[2], 255]);
    assert_eq!(pixel(&plain, GLYPH_PX / 2, GLYPH_PX / 2), pixel(&ringed, GLYPH_PX / 2, GLYPH_PX / 2));
}

#[test]
fn the_glyph_follows_the_light_and_the_pending_build() {
    let snap = snapshot(&[]);
    let view = build_menu(
        &MenuInput { snapshot: &snap, interrupted: None, usage: Some(&[]), pending: Some(PendingBuild { changes: None, armed: false }), light: AppLight::Error, now: now() },
        &Utc,
    );
    assert_eq!(view.glyph, Glyph { light: AppLight::Error, ring: true });
    assert!(view.tooltip.contains("new build installed"));
    let view = menu(&snap, Some(&[]), None);
    assert_eq!(view.glyph, Glyph { light: AppLight::Input, ring: false });
}

#[test]
fn the_glyph_encodes_as_a_png_at_144_dpi() {
    let png = glyph_png(Glyph { light: AppLight::Idle, ring: true });
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert!(png.windows(4).any(|w| w == b"pHYs"));
}
