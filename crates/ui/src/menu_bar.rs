//! The macOS menu bar item: the Mast lamp (mirroring the Dock icon) and a menu
//! with what is running, what needs the user, the interrupted-after-restart
//! list, a pending-build restart, and session/weekly usage for each account.
//!
//! [`build_menu`] is the one pure model: inputs gathered by
//! `shell/menu_bar_ui.rs`, nothing fetched here. [`usage_accounts`] turns the
//! two accounts snapshots into display accounts, [`format_reset`] writes the
//! reset countdowns and [`lamp_rgba`] draws the glyph. The AppKit glue in
//! [`platform`] is thin: it renders a [`MenuView`] and forwards clicks as
//! [`MenuEvent`]s.

use chrono::{DateTime, TimeZone, Utc};
use mast_proto::{AgentAccountsSnapshot, HarnessId, ManagedAccountsSnapshot};

use crate::running_chats::{Entry, InterruptedBlock, Row, RunState, Snapshot};
use crate::settings::accounts::{UsageLevel, usage_level};
use crate::status_light::AppLight;

/// Chat rows listed before "N more…".
pub const MAX_CHAT_ROWS: usize = 8;
/// Longest chat title in a menu row, in characters.
pub const MAX_TITLE_CHARS: usize = 44;
/// Cells in a usage bar.
pub const BAR_CELLS: usize = 10;
/// Usage is re-probed at most this often (the engine's own cache is 60 s).
pub const USAGE_REFRESH_SECS: u64 = 60;

// ---------------------------------------------------------------------------
// Glyph
// ---------------------------------------------------------------------------

/// Most accounts drawn as mini bars next to the lamp.
pub const MAX_GLYPH_BARS: usize = 4;

/// One mini bar per account: its session (5h) usage in percent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GlyphBars {
    pub percent: [u8; MAX_GLYPH_BARS],
    pub len: u8,
}

impl GlyphBars {
    pub fn from_usage(accounts: Option<&[UsageAccount]>) -> Self {
        let mut bars = Self::default();
        for window in accounts
            .into_iter()
            .flatten()
            .filter_map(|account| account.windows.first())
            .take(MAX_GLYPH_BARS)
        {
            bars.percent[bars.len as usize] = percent(window.fraction).min(100) as u8;
            bars.len += 1;
        }
        bars
    }

    fn levels(&self) -> impl Iterator<Item = u8> + '_ {
        self.percent[..self.len as usize].iter().copied()
    }
}

/// The menu bar glyph: the lamp colour, a ring when a newer build waits, and
/// one mini bar per account showing its session usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    pub light: AppLight,
    pub ring: bool,
    pub bars: GlyphBars,
}

/// Cofactr palette: silicon, clover, cobalt, bromine, cadmium.
pub fn lamp_color(light: AppLight) -> [u8; 3] {
    match light {
        AppLight::Idle => [0xB5, 0xB5, 0xB5],
        AppLight::Working => [0x38, 0xD0, 0x59],
        AppLight::Ready => [0x3F, 0x69, 0xCC],
        AppLight::Input => [0xDF, 0xBE, 0x1B],
        AppLight::Error => [0xFE, 0x4B, 0x25],
    }
}

/// Clover, bromine (80%+) or cadmium (95%+) for a usage percentage.
pub fn bar_color(percent: u8) -> [u8; 3] {
    match usage_level(f32::from(percent) / 100.0) {
        UsageLevel::Normal => [0x38, 0xD0, 0x59],
        UsageLevel::Warn => [0xDF, 0xBE, 0x1B],
        UsageLevel::Critical => [0xFE, 0x4B, 0x25],
    }
}

/// The pending-build ring (Cofactr fir, readable on a light and a dark bar).
pub const RING_COLOR: [u8; 3] = [0x61, 0x9C, 0x82];
/// The empty part of a mini bar (Cofactr silicon, faint).
pub const TRACK_COLOR: [u8; 3] = [0xB5, 0xB5, 0xB5];
const TRACK_ALPHA: u8 = 90;

/// Height of the glyph bitmap in pixels (22 pt at 2x); the lamp is a square
/// of this size at the left.
pub const GLYPH_PX: u32 = 44;
const BAR_W: u32 = 6;
const BAR_GAP: u32 = 3;
const BAR_LEAD: u32 = 4;
const BAR_H: u32 = 28;
const BAR_MIN_H: u32 = 3;

/// Bitmap width: the lamp square, then the bars.
pub fn glyph_width(glyph: Glyph) -> u32 {
    match glyph.bars.len as u32 {
        0 => GLYPH_PX,
        n => GLYPH_PX + BAR_LEAD + n * BAR_W + (n - 1) * BAR_GAP,
    }
}

/// Straight-alpha RGBA of the glyph, `glyph_width` x `GLYPH_PX`: a disc (and a
/// ring around it when `glyph.ring`), then a mini bar per account standing on
/// a faint track. Lamp edges are anti-aliased by pixel coverage.
pub fn lamp_rgba(glyph: Glyph) -> Vec<u8> {
    let width = glyph_width(glyph);
    let centre = GLYPH_PX as f32 / 2.0;
    let (disc_r, ring_in, ring_out) = (13.0_f32, 17.0_f32, 19.5_f32);
    let coverage = |d: f32, inner: f32, outer: f32| -> f32 {
        ((outer + 0.5 - d).min(d - inner + 0.5)).clamp(0.0, 1.0)
    };
    let mut out = vec![0u8; (width * GLYPH_PX * 4) as usize];
    let mut put = |x: u32, y: u32, rgb: [u8; 3], alpha: u8| {
        let at = ((y * width + x) * 4) as usize;
        out[at..at + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], alpha]);
    };
    for y in 0..GLYPH_PX {
        for x in 0..GLYPH_PX {
            let d = ((x as f32 + 0.5 - centre).powi(2) + (y as f32 + 0.5 - centre).powi(2)).sqrt();
            let disc = coverage(d, -1.0, disc_r);
            let ring = if glyph.ring {
                coverage(d, ring_in, ring_out)
            } else {
                0.0
            };
            let (rgb, alpha) = if disc >= ring {
                (lamp_color(glyph.light), disc)
            } else {
                (RING_COLOR, ring)
            };
            put(x, y, rgb, (alpha * 255.0).round() as u8);
        }
    }
    let base = (GLYPH_PX + BAR_H) / 2; // bars stand on a shared baseline, centred vertically
    for (i, level) in glyph.bars.levels().enumerate() {
        let x0 = GLYPH_PX + BAR_LEAD + i as u32 * (BAR_W + BAR_GAP);
        let filled = (BAR_H * u32::from(level.min(100)) / 100).max(BAR_MIN_H).min(BAR_H);
        for dx in 0..BAR_W {
            for dy in 0..BAR_H {
                let y = base - 1 - dy;
                if dy < filled {
                    put(x0 + dx, y, bar_color(level), 255);
                } else {
                    put(x0 + dx, y, TRACK_COLOR, TRACK_ALPHA);
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Usage
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct UsageWindow {
    /// "5h", "wk" or "mo".
    pub label: String,
    /// 0.0..=1.0
    pub fraction: f32,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageAccount {
    pub name: String,
    /// Session first, then the longer window; empty when the probe has
    /// nothing (signed out, token rejected).
    pub windows: Vec<UsageWindow>,
    /// Why there are no windows ("API key", "token expired \u{2014} \u{2026}").
    pub note: Option<String>,
}

impl UsageAccount {
    /// Any window at or over the warn threshold (80%).
    pub fn near_limit(&self) -> bool {
        self.level() != UsageLevel::Normal
    }

    /// The worst level across the account's windows.
    pub fn level(&self) -> UsageLevel {
        self.windows
            .iter()
            .map(|window| usage_level(window.fraction))
            .max_by_key(|level| match level {
                UsageLevel::Normal => 0,
                UsageLevel::Warn => 1,
                UsageLevel::Critical => 2,
            })
            .unwrap_or(UsageLevel::Normal)
    }
}

fn harness_name(harness: HarnessId) -> Option<&'static str> {
    match harness {
        HarnessId::ClaudeCode => Some("Claude Code"),
        HarnessId::Codex => Some("Codex"),
        _ => None,
    }
}

fn short_label(label: &str) -> String {
    match label {
        "Session" => "5h".to_owned(),
        "Week" => "wk".to_owned(),
        "Month" => "mo".to_owned(),
        other => other.chars().take(3).collect::<String>().to_lowercase(),
    }
}

fn windows_of(windows: &[mast_proto::AgentUsageWindow]) -> Vec<UsageWindow> {
    windows
        .iter()
        .take(2)
        .map(|window| UsageWindow {
            label: short_label(&window.label),
            fraction: window.used_fraction.clamp(0.0, 1.0),
            resets_at: window.resets_at,
        })
        .collect()
}

/// Shown when a probe found nothing and nothing says why.
pub const NO_USAGE_DATA: &str = "no usage data";

fn account_of(
    name: String,
    windows: &[mast_proto::AgentUsageWindow],
    note: Option<String>,
) -> UsageAccount {
    let windows = windows_of(windows);
    let note = if windows.is_empty() {
        let note = note.unwrap_or_else(|| NO_USAGE_DATA.to_owned());
        tracing::debug!(target: "usage", account = %name, reason = %note, "usage unavailable");
        Some(note)
    } else {
        None
    };
    UsageAccount { name, windows, note }
}

/// The accounts in use, Claude Code then Codex: the live Default login of each
/// harness, then that harness's managed (per-project) accounts. Saved but
/// inactive Settings logins are not in use and are left out. An account with
/// no windows carries the reason (the engine's note for managed accounts, "API
/// key" for an API-key Default login).
pub fn usage_accounts(
    global: Option<&AgentAccountsSnapshot>,
    managed: Option<&ManagedAccountsSnapshot>,
) -> Vec<UsageAccount> {
    let mut out = Vec::new();
    for harness in [HarnessId::ClaudeCode, HarnessId::Codex] {
        let Some(product) = harness_name(harness) else {
            continue;
        };
        for account in global
            .into_iter()
            .flat_map(|snapshot| &snapshot.accounts)
            .filter(|account| account.harness == harness && account.active)
        {
            let note = (account.auth_kind == Some(mast_proto::AgentAuthKind::ApiKey))
                .then(|| "API key, no plan usage".to_owned());
            out.push(account_of(
                format!("Default \u{b7} {product}"),
                &account.usage_windows,
                note,
            ));
        }
        for account in managed
            .into_iter()
            .flat_map(|snapshot| &snapshot.accounts)
            .filter(|account| account.harness == harness)
        {
            let note = if account.signed_in {
                account.usage_note.clone()
            } else {
                Some("not signed in".to_owned())
            };
            out.push(account_of(
                format!("{} \u{b7} {product}", account.label),
                &account.usage_windows,
                note,
            ));
        }
    }
    out
}

/// "resets in 1h 12m" within a day, "resets Thu 9:00" (in `tz`) beyond it.
pub fn format_reset<Tz: TimeZone>(
    resets_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    tz: &Tz,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let Some(at) = resets_at else {
        return String::new();
    };
    let secs = at.signed_duration_since(now).num_seconds();
    if secs <= 0 {
        return "resetting".to_owned();
    }
    if secs < 24 * 3600 {
        let (h, m) = (secs / 3600, secs % 3600 / 60);
        return match (h, m) {
            (0, 0) => "resets in <1m".to_owned(),
            (0, m) => format!("resets in {m}m"),
            (h, 0) => format!("resets in {h}h"),
            (h, m) => format!("resets in {h}h {m}m"),
        };
    }
    format!("resets {}", at.with_timezone(tz).format("%a %-H:%M"))
}

/// Filled and empty cells of a usage bar: (6, 4) for 0.62.
pub fn bar_cells(fraction: f32) -> (usize, usize) {
    let filled = ((fraction.clamp(0.0, 1.0) * BAR_CELLS as f32).round() as usize).min(BAR_CELLS);
    (filled, BAR_CELLS - filled)
}

pub fn percent(fraction: f32) -> u32 {
    (fraction.clamp(0.0, 1.0) * 100.0).round() as u32
}

// ---------------------------------------------------------------------------
// Menu model
// ---------------------------------------------------------------------------

/// How a run of text is coloured. Indexes in [`Span`] are char offsets (every
/// glyph used here is in the BMP, so they equal UTF-16 offsets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Muted,
    /// The empty part of a usage bar.
    Track,
    Clover,
    Bromine,
    Cadmium,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub len: usize,
    pub tone: Tone,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Styled {
    pub text: String,
    pub spans: Vec<Span>,
}

impl Styled {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            spans: Vec::new(),
        }
    }

    /// Append `text` in `tone` (`None` keeps the default colour).
    fn push(&mut self, text: &str, tone: Option<Tone>) {
        let start = self.text.chars().count();
        let len = text.chars().count();
        self.text.push_str(text);
        if let Some(tone) = tone
            && len > 0
        {
            self.spans.push(Span { start, len, tone });
        }
    }
}

/// What a click does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    OpenChat(String),
    OpenMast,
    RestartWhenIdle,
    ResumeAll(Vec<String>),
    Quit,
}

/// What the AppKit glue reports back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEvent {
    /// The menu is about to open: refresh usage if it is stale.
    WillOpen,
    Action(MenuAction),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// The bold first line.
    Title(String),
    /// A muted section header.
    Section(String),
    /// A chat: clicking brings Mast forward on it.
    Chat { chat_id: String, text: Styled },
    /// A disabled information line.
    Line { text: Styled, indent: u8 },
    Action { action: MenuAction, label: String },
    Separator,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MenuView {
    pub glyph: Glyph,
    pub tooltip: String,
    pub items: Vec<Item>,
}

/// A newer installed build, as the menu needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingBuild {
    /// Number of changes, when the marker scan produced a list.
    pub changes: Option<usize>,
    /// "Restart when idle" is armed.
    pub armed: bool,
}

/// What the Usage section has to show.
#[derive(Debug, Clone, Copy)]
pub enum UsageView<'a> {
    /// The first probe has not answered yet.
    Loading,
    /// Every probe failed and nothing is cached: the reason.
    Failed(&'a str),
    Accounts(&'a [UsageAccount]),
}

impl<'a> UsageView<'a> {
    fn accounts(self) -> Option<&'a [UsageAccount]> {
        match self {
            Self::Accounts(accounts) => Some(accounts),
            _ => None,
        }
    }
}

pub struct MenuInput<'a> {
    pub snapshot: &'a Snapshot,
    pub interrupted: Option<&'a InterruptedBlock>,
    pub usage: UsageView<'a>,
    pub pending: Option<PendingBuild>,
    pub light: AppLight,
    pub now: DateTime<Utc>,
}

pub fn truncate_title(title: &str) -> String {
    if title.chars().count() <= MAX_TITLE_CHARS {
        return title.to_owned();
    }
    let mut cut: String = title.chars().take(MAX_TITLE_CHARS - 1).collect();
    cut.push('\u{2026}');
    cut
}

fn chat_item(row: &Row) -> Item {
    let mut text = Styled::default();
    let dot = match row.state {
        RunState::Errored => Tone::Cadmium,
        RunState::Awaiting => Tone::Bromine,
        RunState::Working | RunState::IdleBackground => Tone::Clover,
    };
    text.push("\u{25cf} ", Some(dot));
    text.push(&truncate_title(&row.title), None);
    let detail = if row.state.needs_you() {
        format!("{} {}", row.state.label(), row.elapsed)
    } else {
        match &row.account {
            Some(account) => format!("{account} \u{b7} {}", row.elapsed),
            None => row.elapsed.clone(),
        }
    };
    text.push(&format!("   {detail}"), Some(Tone::Muted));
    Item::Chat {
        chat_id: row.chat_id.clone(),
        text,
    }
}

fn quiet_line(text: &str, indent: u8) -> Item {
    let mut styled = Styled::default();
    styled.push(text, Some(Tone::Muted));
    Item::Line {
        text: styled,
        indent,
    }
}

fn usage_items<Tz: TimeZone>(usage: UsageView<'_>, now: DateTime<Utc>, tz: &Tz) -> Vec<Item>
where
    Tz::Offset: std::fmt::Display,
{
    let accounts = match usage {
        UsageView::Loading => {
            return vec![Item::Section("Usage".into()), quiet_line("Loading\u{2026}", 0)];
        }
        UsageView::Failed(reason) => {
            return vec![
                Item::Section("Usage".into()),
                quiet_line(&format!("usage unavailable \u{2014} {reason}"), 0),
            ];
        }
        UsageView::Accounts(accounts) => accounts,
    };
    if accounts.is_empty() {
        return vec![Item::Section("Usage".into()), quiet_line("no accounts signed in", 0)];
    }
    let tone_of = |level| match level {
        UsageLevel::Normal => Tone::Clover,
        UsageLevel::Warn => Tone::Bromine,
        UsageLevel::Critical => Tone::Cadmium,
    };
    let mut items = vec![Item::Section("Usage".into())];
    for account in accounts {
        let mut head = Styled::default();
        head.push(&account.name, None);
        if account.near_limit() {
            head.push("   \u{26a0} near limit", Some(tone_of(account.level())));
        }
        items.push(Item::Line {
            text: head,
            indent: 0,
        });
        if account.windows.is_empty() {
            let note = account.note.as_deref().unwrap_or(NO_USAGE_DATA);
            items.push(quiet_line(&format!("usage unavailable \u{2014} {note}"), 1));
        }
        for window in &account.windows {
            let (filled, empty) = bar_cells(window.fraction);
            let mut line = Styled::default();
            line.push(&format!("{}  ", window.label), Some(Tone::Muted));
            line.push(
                &"\u{2588}".repeat(filled),
                Some(tone_of(usage_level(window.fraction))),
            );
            line.push(&"\u{2588}".repeat(empty), Some(Tone::Track));
            line.push(&format!("  {:>3}%", percent(window.fraction)), None);
            let reset = format_reset(window.resets_at, now, tz);
            if !reset.is_empty() {
                line.push(&format!("   {reset}"), Some(Tone::Muted));
            }
            items.push(Item::Line {
                text: line,
                indent: 1,
            });
        }
    }
    items
}

/// The whole menu: headline, Needs you first, Running, Interrupted, Usage,
/// then restart / Open Mast / Quit.
pub fn build_menu<Tz: TimeZone>(input: &MenuInput<'_>, tz: &Tz) -> MenuView
where
    Tz::Offset: std::fmt::Display,
{
    let headline = input.snapshot.headline();
    let mut items = vec![Item::Title(headline.clone())];

    let rows: Vec<&Row> = input
        .snapshot
        .entries()
        .filter_map(|entry| match entry {
            Entry::Row(row) => Some(row),
            Entry::Fold { .. } => None,
        })
        .collect();
    let needs: Vec<&Row> = rows.iter().copied().filter(|r| r.state.needs_you()).collect();
    let running: Vec<&Row> = rows.iter().copied().filter(|r| !r.state.needs_you()).collect();
    let mut budget = MAX_CHAT_ROWS;
    for (label, group) in [("Needs you", needs), ("Running", running)] {
        if group.is_empty() || budget == 0 {
            continue;
        }
        items.push(Item::Section(format!("{label} \u{b7} {}", group.len())));
        for row in group.iter().take(budget) {
            items.push(chat_item(row));
        }
        budget = budget.saturating_sub(group.len());
    }
    let hidden = input.snapshot.total.saturating_sub(
        items
            .iter()
            .filter(|item| matches!(item, Item::Chat { .. }))
            .count(),
    );
    if hidden > 0 {
        items.push(Item::Action {
            action: MenuAction::OpenMast,
            label: format!("{hidden} more in Mast\u{2026}"),
        });
    }

    if let Some(block) = input.interrupted {
        items.push(Item::Separator);
        let at = DateTime::from_timestamp_millis(block.restarted_at_ms)
            .map(|at| at.with_timezone(tz).format("%H:%M").to_string())
            .unwrap_or_default();
        items.push(Item::Section(format!(
            "Interrupted by the restart at {at} \u{b7} {}",
            block.total()
        )));
        for row in &block.needs_decision {
            let mut text = Styled::default();
            text.push("\u{25cf} ", Some(Tone::Bromine));
            text.push(&truncate_title(&row.title), None);
            text.push(
                &format!("   {}", crate::running_chats::outcome_label(row.outcome)),
                Some(Tone::Muted),
            );
            items.push(Item::Chat {
                chat_id: row.chat_id.clone(),
                text,
            });
        }
        if !block.resumed.is_empty() {
            let mut text = Styled::default();
            text.push(
                &format!("{} resumed automatically", block.resumed.len()),
                Some(Tone::Muted),
            );
            items.push(Item::Line { text, indent: 0 });
        }
        if !block.needs_decision.is_empty() {
            items.push(Item::Action {
                action: MenuAction::ResumeAll(
                    block.needs_decision.iter().map(|r| r.chat_id.clone()).collect(),
                ),
                label: if block.needs_decision.len() == 1 {
                    "Resume".to_owned()
                } else {
                    format!("Resume all ({})", block.needs_decision.len())
                },
            });
        }
    }

    let usage = usage_items(input.usage, input.now, tz);
    if !usage.is_empty() {
        items.push(Item::Separator);
        items.extend(usage);
    }

    items.push(Item::Separator);
    if let Some(pending) = input.pending {
        if pending.armed {
            items.push(Item::Line {
                text: Styled::plain("Restarting when idle\u{2026}"),
                indent: 0,
            });
        } else {
            let label = match pending.changes {
                Some(n) if n > 0 => format!("Restart when idle  \u{b7}  {n} changes"),
                _ => "Restart when idle  \u{b7}  new build".to_owned(),
            };
            items.push(Item::Action {
                action: MenuAction::RestartWhenIdle,
                label,
            });
        }
    }
    items.push(Item::Action {
        action: MenuAction::OpenMast,
        label: "Open Mast".into(),
    });
    items.push(Item::Action {
        action: MenuAction::Quit,
        label: "Quit Mast".into(),
    });

    let mut tooltip = format!("Mast \u{b7} {headline}");
    if input.pending.is_some() {
        tooltip.push_str(" \u{b7} new build installed");
    }
    for account in input.usage.accounts().into_iter().flatten() {
        if let Some(window) = account.windows.first() {
            tooltip.push_str(&format!(
                "\n{} {} {}%",
                account.name,
                window.label,
                percent(window.fraction)
            ));
        }
    }
    MenuView {
        glyph: Glyph {
            light: input.light,
            ring: input.pending.is_some(),
            bars: GlyphBars::from_usage(input.usage.accounts()),
        },
        tooltip,
        items,
    }
}

// ---------------------------------------------------------------------------
// Usage refresh schedule
// ---------------------------------------------------------------------------

/// While the app runs, usage is re-probed this often (the engine's own cache
/// is 60 s, so a probe never costs more than one request per account).
pub const USAGE_PERIODIC_SECS: u64 = 300;
/// Menu opens refresh usage only if the last probe is at least this old, and a
/// failed probe is retried after it.
pub const USAGE_STALE_SECS: u64 = USAGE_REFRESH_SECS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageTrigger {
    /// The 1 s heartbeat (startup and the periodic refresh).
    Tick,
    /// The menu is about to open.
    MenuOpen,
}

/// When to probe usage. Pure: the caller passes the clock.
#[derive(Debug, Default)]
pub struct UsageSchedule {
    last: Option<std::time::Instant>,
    failed: bool,
    in_flight: bool,
}

impl UsageSchedule {
    /// Nothing has been probed yet: the startup probe is due.
    pub fn due(&self, now: std::time::Instant, trigger: UsageTrigger) -> bool {
        if self.in_flight {
            return false;
        }
        let Some(last) = self.last else {
            return true;
        };
        let age = now.saturating_duration_since(last).as_secs();
        match trigger {
            UsageTrigger::MenuOpen => age >= USAGE_STALE_SECS,
            UsageTrigger::Tick if self.failed => age >= USAGE_STALE_SECS,
            UsageTrigger::Tick => age >= USAGE_PERIODIC_SECS,
        }
    }

    pub fn begin(&mut self) {
        self.in_flight = true;
    }

    /// `ok` is false when every request failed.
    pub fn finish(&mut self, now: std::time::Instant, ok: bool) {
        self.in_flight = false;
        self.last = Some(now);
        self.failed = !ok;
    }

    pub fn in_flight(&self) -> bool {
        self.in_flight
    }
}

// ---------------------------------------------------------------------------
// AppKit glue
// ---------------------------------------------------------------------------

/// Show (or update) the status item; hide it with `None`. Main thread only; a
/// no-op under test and off macOS.
pub fn apply(view: Option<&MenuView>) {
    if cfg!(test) {
        return;
    }
    platform::apply(view);
}

/// The receiver for clicks, taken once at startup. Events are produced on the
/// main thread; nothing polls.
pub fn take_events() -> Option<futures::channel::mpsc::UnboundedReceiver<MenuEvent>> {
    platform::take_events()
}

fn glyph_png(glyph: Glyph) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, glyph_width(glyph), GLYPH_PX);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    // 144 dpi: AppKit reads the image as 22 x 22 points.
    encoder.set_pixel_dims(Some(png::PixelDimensions {
        xppu: 5669,
        yppu: 5669,
        unit: png::Unit::Meter,
    }));
    if let Ok(mut writer) = encoder.write_header() {
        let _ = writer.write_image_data(&lamp_rgba(glyph));
    }
    out
}

// objc 0.2's `msg_send!` checks a `cfg(feature = "cargo-clippy")` this crate lacks.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
mod platform {
    use std::cell::RefCell;
    use std::ffi::CString;
    use std::sync::Once;

    use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};

    use super::*;

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        static NSForegroundColorAttributeName: *mut Object;
        static NSFontAttributeName: *mut Object;
    }

    struct Live {
        item: *mut Object,
        menu: *mut Object,
    }

    #[derive(Default)]
    struct Glue {
        live: Option<Live>,
        sender: Option<UnboundedSender<MenuEvent>>,
        receiver: Option<UnboundedReceiver<MenuEvent>>,
        /// Click targets of the items currently in the menu, by tag.
        actions: Vec<MenuAction>,
        glyph: Option<Glyph>,
        /// What the menu was last filled from, and for which appearance, so a
        /// light/dark switch re-colours it when it next opens.
        view: Option<MenuView>,
        dark: bool,
    }

    thread_local! {
        // Everything here runs on the main thread (AppKit's, and gpui's).
        static GLUE: RefCell<Glue> = RefCell::new(Glue::default());
    }

    pub fn take_events() -> Option<UnboundedReceiver<MenuEvent>> {
        GLUE.with(|glue| {
            let mut glue = glue.borrow_mut();
            if glue.sender.is_none() {
                let (tx, rx) = unbounded();
                glue.sender = Some(tx);
                glue.receiver = Some(rx);
            }
            glue.receiver.take()
        })
    }

    fn send(event: MenuEvent) {
        GLUE.with(|glue| {
            if let Some(sender) = &glue.borrow().sender {
                let _ = sender.unbounded_send(event);
            }
        });
    }

    extern "C" fn item_clicked(_this: &Object, _sel: Sel, sender: *mut Object) {
        let tag: isize = unsafe { msg_send![sender, tag] };
        let action = usize::try_from(tag)
            .ok()
            .and_then(|tag| GLUE.with(|glue| glue.borrow().actions.get(tag).cloned()));
        if let Some(action) = action {
            send(MenuEvent::Action(action));
        }
    }

    extern "C" fn menu_will_open(_this: &Object, _sel: Sel, _menu: *mut Object) {
        // The menu bar may have switched between light and dark since the
        // titles were coloured: refill before it shows.
        GLUE.with(|glue| {
            let mut glue = glue.borrow_mut();
            let Some(live) = glue.live.as_ref() else {
                return;
            };
            let (item, menu) = (live.item, live.menu);
            let dark = unsafe { is_dark(item) };
            if dark != glue.dark
                && let Some(view) = glue.view.clone()
            {
                let mut actions = std::mem::take(&mut glue.actions);
                unsafe { fill(menu, target(), &view.items, &mut actions, dark) };
                glue.actions = actions;
                glue.dark = dark;
            }
        });
        send(MenuEvent::WillOpen);
    }

    /// Whether the menu bar the item lives in is dark.
    unsafe fn is_dark(item: *mut Object) -> bool {
        unsafe {
            let button: *mut Object = msg_send![item, button];
            if button.is_null() {
                return false;
            }
            let appearance: *mut Object = msg_send![button, effectiveAppearance];
            if appearance.is_null() {
                return false;
            }
            let name: *mut Object = msg_send![appearance, name];
            if name.is_null() {
                return false;
            }
            let utf8: *const std::os::raw::c_char = msg_send![name, UTF8String];
            !utf8.is_null()
                && std::ffi::CStr::from_ptr(utf8)
                    .to_string_lossy()
                    .contains("Dark")
        }
    }

    fn target() -> *mut Object {
        static REGISTER: Once = Once::new();
        REGISTER.call_once(|| {
            if let Some(mut decl) = ClassDecl::new("MastMenuBarTarget", class!(NSObject)) {
                unsafe {
                    decl.add_method(
                        sel!(menuItemClicked:),
                        item_clicked as extern "C" fn(&Object, Sel, *mut Object),
                    );
                    decl.add_method(
                        sel!(menuWillOpen:),
                        menu_will_open as extern "C" fn(&Object, Sel, *mut Object),
                    );
                }
                decl.register();
            }
        });
        let Some(class) = Class::get("MastMenuBarTarget") else {
            return std::ptr::null_mut();
        };
        thread_local! {
            static TARGET: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        }
        TARGET.with(|cell| {
            if cell.get() == 0 {
                let object: *mut Object = unsafe { msg_send![class, new] };
                cell.set(object as usize);
            }
            cell.get() as *mut Object
        })
    }

    unsafe fn ns_string(text: &str) -> *mut Object {
        let c = CString::new(text.replace('\0', "")).unwrap_or_default();
        unsafe { msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()] }
    }

    /// Text uses the system label colours (never dimmed); only the usage fills
    /// and warnings use palette tones, darker on a light menu bar for contrast.
    unsafe fn color(tone: Option<Tone>, dark: bool) -> *mut Object {
        let rgb = match (tone, dark) {
            (None, _) => return unsafe { msg_send![class!(NSColor), labelColor] },
            (Some(Tone::Muted), _) => {
                return unsafe { msg_send![class!(NSColor), secondaryLabelColor] };
            }
            (Some(Tone::Track), _) => unsafe {
                let base: *mut Object = msg_send![class!(NSColor), secondaryLabelColor];
                return msg_send![base, colorWithAlphaComponent: 0.35f64];
            },
            // Clover / bromine / cadmium; on light: juniper and darker shades.
            (Some(Tone::Clover), true) => [0x38, 0xD0, 0x59],
            (Some(Tone::Clover), false) => [0x24, 0x5F, 0x31],
            (Some(Tone::Bromine), true) => [0xDF, 0xBE, 0x1B],
            (Some(Tone::Bromine), false) => [0xA6, 0x8A, 0x00],
            (Some(Tone::Cadmium), true) => [0xFE, 0x4B, 0x25],
            (Some(Tone::Cadmium), false) => [0xD9, 0x3A, 0x15],
        };
        let [r, g, b] = rgb.map(|c: u8| f64::from(c) / 255.0);
        unsafe {
            msg_send![class!(NSColor), colorWithSRGBRed: r green: g blue: b alpha: 1.0f64]
        }
    }

    unsafe fn font(bold: bool, mono: bool) -> *mut Object {
        unsafe {
            if bold {
                msg_send![class!(NSFont), boldSystemFontOfSize: 13.0f64]
            } else if mono {
                msg_send![class!(NSFont), monospacedDigitSystemFontOfSize: 13.0f64 weight: 0.0f64]
            } else {
                msg_send![class!(NSFont), menuFontOfSize: 0.0f64]
            }
        }
    }

    /// An attributed title made of coloured runs.
    unsafe fn attributed(styled: &Styled, bold: bool, mono: bool, dark: bool) -> *mut Object {
        unsafe {
            let result: *mut Object = msg_send![class!(NSMutableAttributedString), alloc];
            let result: *mut Object = msg_send![result, init];
            let chars: Vec<char> = styled.text.chars().collect();
            let mut at = 0usize;
            let mut runs: Vec<(usize, usize, Option<Tone>)> = Vec::new();
            for span in &styled.spans {
                if span.start > at {
                    runs.push((at, span.start, None));
                }
                runs.push((span.start, span.start + span.len, Some(span.tone)));
                at = span.start + span.len;
            }
            if at < chars.len() {
                runs.push((at, chars.len(), None));
            }
            for (from, to, tone) in runs {
                let text: String = chars[from.min(chars.len())..to.min(chars.len())]
                    .iter()
                    .collect();
                let keys = [NSForegroundColorAttributeName, NSFontAttributeName];
                let values = [color(tone, dark), font(bold, mono)];
                let attributes: *mut Object = msg_send![
                    class!(NSDictionary),
                    dictionaryWithObjects: values.as_ptr()
                    forKeys: keys.as_ptr()
                    count: 2usize
                ];
                let piece: *mut Object = msg_send![class!(NSAttributedString), alloc];
                let piece: *mut Object =
                    msg_send![piece, initWithString: ns_string(&text) attributes: attributes];
                let _: () = msg_send![result, appendAttributedString: piece];
                let _: () = msg_send![piece, release];
            }
            let _: *mut Object = msg_send![result, autorelease];
            result
        }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn add_item(
        menu: *mut Object,
        target: *mut Object,
        title: Option<&Styled>,
        plain: &str,
        bold: bool,
        mono: bool,
        indent: u8,
        action: Option<usize>,
        dark: bool,
    ) {
        unsafe {
            let item: *mut Object = msg_send![class!(NSMenuItem), alloc];
            let item: *mut Object = msg_send![
                item,
                initWithTitle: ns_string(plain)
                action: sel!(menuItemClicked:)
                keyEquivalent: ns_string("")
            ];
            if let Some(styled) = title {
                let _: () = msg_send![item, setAttributedTitle: attributed(styled, bold, mono, dark)];
            }
            let _: () = msg_send![item, setTarget: target];
            let _: () = msg_send![item, setIndentationLevel: indent as isize];
            // Every row is enabled so AppKit never dims it; rows without an
            // action carry tag -1 and a click only closes the menu.
            let tag = action.map_or(-1, |tag| tag as isize);
            let _: () = msg_send![item, setTag: tag];
            let _: () = msg_send![item, setEnabled: true];
            let _: () = msg_send![menu, addItem: item];
            let _: () = msg_send![item, release];
        }
    }

    unsafe fn separator(menu: *mut Object) {
        unsafe {
            let item: *mut Object = msg_send![class!(NSMenuItem), separatorItem];
            let _: () = msg_send![menu, addItem: item];
        }
    }

    unsafe fn fill(
        menu: *mut Object,
        target: *mut Object,
        items: &[Item],
        actions: &mut Vec<MenuAction>,
        dark: bool,
    ) {
        unsafe {
            let _: () = msg_send![menu, removeAllItems];
            actions.clear();
            let mut click = |action: MenuAction| {
                actions.push(action);
                actions.len() - 1
            };
            for item in items {
                match item {
                    Item::Title(text) => {
                        let styled = Styled::plain(text.clone());
                        add_item(menu, target, Some(&styled), text, true, false, 0, None, dark);
                    }
                    Item::Section(text) => {
                        let mut styled = Styled::default();
                        styled.push(text, Some(Tone::Muted));
                        add_item(menu, target, Some(&styled), text, false, false, 0, None, dark);
                    }
                    Item::Chat { chat_id, text } => {
                        let tag = click(MenuAction::OpenChat(chat_id.clone()));
                        add_item(menu, target, Some(text), &text.text, false, false, 0, Some(tag), dark);
                    }
                    Item::Line { text, indent } => {
                        add_item(menu, target, Some(text), &text.text, false, true, *indent, None, dark);
                    }
                    Item::Action { action, label } => {
                        let tag = click(action.clone());
                        add_item(menu, target, None, label, false, false, 0, Some(tag), dark);
                    }
                    Item::Separator => separator(menu),
                }
            }
        }
    }

    unsafe fn set_icon(item: *mut Object, glyph: Glyph) {
        unsafe {
            let png = glyph_png(glyph);
            let data: *mut Object = msg_send![
                class!(NSData),
                dataWithBytes: png.as_ptr()
                length: png.len()
            ];
            let image: *mut Object = msg_send![class!(NSImage), alloc];
            let image: *mut Object = msg_send![image, initWithData: data];
            if image.is_null() {
                return;
            }
            let button: *mut Object = msg_send![item, button];
            if !button.is_null() {
                let _: () = msg_send![button, setImage: image];
            }
            let _: () = msg_send![image, release];
        }
    }

    pub fn apply(view: Option<&MenuView>) {
        unsafe {
            GLUE.with(|glue| {
                let mut glue = glue.borrow_mut();
                let Some(view) = view else {
                    if let Some(live) = glue.live.take() {
                        let bar: *mut Object = msg_send![class!(NSStatusBar), systemStatusBar];
                        let _: () = msg_send![bar, removeStatusItem: live.item];
                        let _: () = msg_send![live.menu, release];
                        let _: () = msg_send![live.item, release];
                    }
                    glue.glyph = None;
                    glue.view = None;
                    return;
                };
                let target = target();
                if target.is_null() {
                    return;
                }
                if glue.live.is_none() {
                    let bar: *mut Object = msg_send![class!(NSStatusBar), systemStatusBar];
                    // NSVariableStatusItemLength
                    let item: *mut Object = msg_send![bar, statusItemWithLength: -1.0f64];
                    if item.is_null() {
                        return;
                    }
                    let _: *mut Object = msg_send![item, retain];
                    let menu: *mut Object = msg_send![class!(NSMenu), alloc];
                    let menu: *mut Object = msg_send![menu, initWithTitle: ns_string("Mast")];
                    let _: () = msg_send![menu, setAutoenablesItems: false];
                    let _: () = msg_send![menu, setDelegate: target];
                    let _: () = msg_send![item, setMenu: menu];
                    glue.live = Some(Live { item, menu });
                    glue.glyph = None;
                }
                let Live { item, menu } = match &glue.live {
                    Some(live) => Live {
                        item: live.item,
                        menu: live.menu,
                    },
                    None => return,
                };
                if glue.glyph != Some(view.glyph) {
                    set_icon(item, view.glyph);
                    glue.glyph = Some(view.glyph);
                }
                let button: *mut Object = msg_send![item, button];
                if !button.is_null() {
                    let _: () = msg_send![button, setToolTip: ns_string(&view.tooltip)];
                }
                let dark = is_dark(item);
                let mut actions = std::mem::take(&mut glue.actions);
                fill(menu, target, &view.items, &mut actions, dark);
                glue.actions = actions;
                glue.dark = dark;
                glue.view = Some(view.clone());
            });
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub fn apply(_view: Option<&MenuView>) {}

    pub fn take_events() -> Option<futures::channel::mpsc::UnboundedReceiver<MenuEvent>> {
        None
    }
}

#[cfg(test)]
mod tests;
