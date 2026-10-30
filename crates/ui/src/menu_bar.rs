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

/// The menu bar glyph: the lamp colour, plus a ring when a newer build waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    pub light: AppLight,
    pub ring: bool,
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

/// The pending-build ring (Cofactr fir, readable on a light and a dark bar).
pub const RING_COLOR: [u8; 3] = [0x61, 0x9C, 0x82];

/// Edge length of the glyph bitmap in pixels (22 pt at 2x).
pub const GLYPH_PX: u32 = 44;

/// Straight-alpha RGBA of the glyph: a disc, and a ring around it when
/// `glyph.ring`. Edges are anti-aliased by pixel coverage.
pub fn lamp_rgba(glyph: Glyph) -> Vec<u8> {
    let size = GLYPH_PX as f32;
    let centre = size / 2.0;
    let (disc_r, ring_in, ring_out) = (13.0_f32, 17.0_f32, 19.5_f32);
    let coverage = |d: f32, inner: f32, outer: f32| -> f32 {
        ((outer + 0.5 - d).min(d - inner + 0.5)).clamp(0.0, 1.0)
    };
    let mut out = Vec::with_capacity((GLYPH_PX * GLYPH_PX * 4) as usize);
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
            out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], (alpha * 255.0).round() as u8]);
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

/// The accounts in use, Claude Code then Codex: the live Default login of each
/// harness, then that harness's signed-in managed (per-project) accounts.
/// Saved but inactive Settings logins are not in use and are left out.
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
            out.push(UsageAccount {
                name: format!("Default \u{b7} {product}"),
                windows: windows_of(&account.usage_windows),
            });
        }
        for account in managed
            .into_iter()
            .flat_map(|snapshot| &snapshot.accounts)
            .filter(|account| account.harness == harness && account.signed_in)
        {
            out.push(UsageAccount {
                name: format!("{} \u{b7} {product}", account.label),
                windows: windows_of(&account.usage_windows),
            });
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

/// `▰▰▰▰▰▰▱▱▱▱` for 0.62.
pub fn usage_bar(fraction: f32) -> String {
    let filled = ((fraction.clamp(0.0, 1.0) * BAR_CELLS as f32).round() as usize).min(BAR_CELLS);
    let mut bar = "\u{25b0}".repeat(filled);
    bar.push_str(&"\u{25b1}".repeat(BAR_CELLS - filled));
    bar
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

pub struct MenuInput<'a> {
    pub snapshot: &'a Snapshot,
    pub interrupted: Option<&'a InterruptedBlock>,
    /// `None` while the first probe is loading.
    pub usage: Option<&'a [UsageAccount]>,
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

fn usage_items<Tz: TimeZone>(
    accounts: Option<&[UsageAccount]>,
    now: DateTime<Utc>,
    tz: &Tz,
) -> Vec<Item>
where
    Tz::Offset: std::fmt::Display,
{
    let Some(accounts) = accounts else {
        return vec![
            Item::Section("Usage".into()),
            Item::Line {
                text: Styled::plain("Loading\u{2026}"),
                indent: 0,
            },
        ];
    };
    if accounts.is_empty() {
        return Vec::new();
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
            items.push(Item::Line {
                text: {
                    let mut text = Styled::default();
                    text.push("usage unavailable", Some(Tone::Muted));
                    text
                },
                indent: 1,
            });
        }
        for window in &account.windows {
            let mut line = Styled::default();
            line.push(&format!("{}  ", window.label), Some(Tone::Muted));
            line.push(
                &format!(
                    "{} {:>3}%",
                    usage_bar(window.fraction),
                    percent(window.fraction)
                ),
                Some(tone_of(usage_level(window.fraction))),
            );
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
    MenuView {
        glyph: Glyph {
            light: input.light,
            ring: input.pending.is_some(),
        },
        tooltip,
        items,
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
    let mut encoder = png::Encoder::new(&mut out, GLYPH_PX, GLYPH_PX);
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
        let action = GLUE.with(|glue| glue.borrow().actions.get(tag as usize).cloned());
        if let Some(action) = action {
            send(MenuEvent::Action(action));
        }
    }

    extern "C" fn menu_will_open(_this: &Object, _sel: Sel, _menu: *mut Object) {
        send(MenuEvent::WillOpen);
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

    unsafe fn color(tone: Option<Tone>) -> *mut Object {
        let rgb = match tone {
            None => return unsafe { msg_send![class!(NSColor), labelColor] },
            Some(Tone::Muted) => return unsafe { msg_send![class!(NSColor), secondaryLabelColor] },
            Some(Tone::Clover) => [0x38, 0xD0, 0x59],
            Some(Tone::Bromine) => [0xDF, 0xBE, 0x1B],
            Some(Tone::Cadmium) => [0xFE, 0x4B, 0x25],
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
    unsafe fn attributed(styled: &Styled, bold: bool, mono: bool) -> *mut Object {
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
                let values = [color(tone), font(bold, mono)];
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
                let _: () = msg_send![item, setAttributedTitle: attributed(styled, bold, mono)];
            }
            let _: () = msg_send![item, setTarget: target];
            let _: () = msg_send![item, setIndentationLevel: indent as isize];
            match action {
                Some(tag) => {
                    let _: () = msg_send![item, setTag: tag as isize];
                    let _: () = msg_send![item, setEnabled: true];
                }
                None => {
                    let _: () = msg_send![item, setEnabled: false];
                }
            }
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

    unsafe fn fill(menu: *mut Object, target: *mut Object, items: &[Item], actions: &mut Vec<MenuAction>) {
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
                        add_item(menu, target, Some(&styled), text, true, false, 0, None);
                    }
                    Item::Section(text) => {
                        let mut styled = Styled::default();
                        styled.push(text, Some(Tone::Muted));
                        add_item(menu, target, Some(&styled), text, false, false, 0, None);
                    }
                    Item::Chat { chat_id, text } => {
                        let tag = click(MenuAction::OpenChat(chat_id.clone()));
                        add_item(menu, target, Some(text), &text.text, false, false, 0, Some(tag));
                    }
                    Item::Line { text, indent } => {
                        add_item(menu, target, Some(text), &text.text, false, true, *indent, None);
                    }
                    Item::Action { action, label } => {
                        let tag = click(action.clone());
                        add_item(menu, target, None, label, false, false, 0, Some(tag));
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
                let mut actions = std::mem::take(&mut glue.actions);
                fill(menu, target, &view.items, &mut actions);
                glue.actions = actions;
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
