//! The status light: the masthead lamp shows what the agents are doing, in the
//! Dock icon and in the in-app logo.
//!
//! [`app_light`] is the one pure aggregation (most urgent state across every
//! chat and open Required Action). [`DockSync`] debounces what reaches AppKit
//! and [`plan`] says which of the icon and the badge actually changed, so the
//! platform layer in [`dock`] only runs for a real change.

use std::time::{Duration, Instant};

use zeron_proto::view::Indicator;

use crate::theme::Appearance;

/// Required Actions raised by "Ready to merge" start with this text.
pub const READY_TO_MERGE_PREFIX: &str = "Ready to merge";

/// How long a new target must hold before the Dock is touched.
pub const DEBOUNCE: Duration = Duration::from_millis(1000);

/// The lamp, most urgent last in [`AppLight::ALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppLight {
    #[default]
    Idle,
    Working,
    Ready,
    Input,
    Error,
}

impl AppLight {
    pub const ALL: [Self; 5] = [Self::Idle, Self::Working, Self::Ready, Self::Input, Self::Error];

    /// The 512 px Dock icon for this lamp.
    pub fn dock_png(self) -> &'static [u8] {
        match self {
            Self::Idle => include_bytes!("../assets/dock/mast-icon-idle.png"),
            Self::Working => include_bytes!("../assets/dock/mast-icon-working.png"),
            Self::Ready => include_bytes!("../assets/dock/mast-icon-ready.png"),
            Self::Input => include_bytes!("../assets/dock/mast-icon-input.png"),
            Self::Error => include_bytes!("../assets/dock/mast-icon-error.png"),
        }
    }

    /// The embedded in-app logo for this lamp on a theme appearance.
    pub fn logo_asset(self, appearance: Appearance) -> &'static str {
        use crate::icons::*;
        match (appearance.is_dark(), self) {
            (false, Self::Idle) => MAST_LOGO_LIGHT_IDLE,
            (false, Self::Working) => MAST_LOGO_LIGHT_WORKING,
            (false, Self::Ready) => MAST_LOGO_LIGHT_READY,
            (false, Self::Input) => MAST_LOGO_LIGHT_INPUT,
            (false, Self::Error) => MAST_LOGO_LIGHT_ERROR,
            (true, Self::Idle) => MAST_LOGO_DARK_IDLE,
            (true, Self::Working) => MAST_LOGO_DARK_WORKING,
            (true, Self::Ready) => MAST_LOGO_DARK_READY,
            (true, Self::Input) => MAST_LOGO_DARK_INPUT,
            (true, Self::Error) => MAST_LOGO_DARK_ERROR,
        }
    }
}

/// The aggregate: the lamp and the number of things waiting on the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LightSummary {
    pub light: AppLight,
    pub badge: u32,
}

/// True for a Required Action raised as "Ready to merge: <branch>".
pub fn is_ready_to_merge(text: &str) -> bool {
    text.trim_start().starts_with(READY_TO_MERGE_PREFIX)
}

/// Most urgent state across all chats and open Required Actions.
///
/// `indicators` are the staleness-checked indicators of the live chats;
/// `open_requests` is the text of every unresolved Required Action.
/// Error (any errored chat) > Input (any chat awaiting input, or any open
/// request that is not "Ready to merge") > Ready (any open "Ready to merge")
/// > Working > Idle. The badge counts awaiting-input chats, errored chats and
/// every open request.
pub fn app_light<S: AsRef<str>>(
    indicators: impl IntoIterator<Item = Indicator>,
    open_requests: impl IntoIterator<Item = S>,
) -> LightSummary {
    let (mut working, mut awaiting, mut errored) = (0u32, 0u32, 0u32);
    for indicator in indicators {
        match indicator {
            Indicator::Working => working += 1,
            Indicator::AwaitingInput => awaiting += 1,
            Indicator::Errored => errored += 1,
            Indicator::None => {}
        }
    }
    let (mut ready, mut other) = (0u32, 0u32);
    for text in open_requests {
        if is_ready_to_merge(text.as_ref()) {
            ready += 1;
        } else {
            other += 1;
        }
    }
    let light = if errored > 0 {
        AppLight::Error
    } else if awaiting > 0 || other > 0 {
        AppLight::Input
    } else if ready > 0 {
        AppLight::Ready
    } else if working > 0 {
        AppLight::Working
    } else {
        AppLight::Idle
    };
    LightSummary {
        light,
        badge: awaiting
            .saturating_add(errored)
            .saturating_add(ready)
            .saturating_add(other),
    }
}

/// The aggregate for the live app state: staleness-checked indicators of the
/// non-archived chats and every unresolved Required Action the UI has loaded.
pub(crate) fn summarize_state(
    state: &crate::state::AppState,
    now: chrono::DateTime<chrono::Utc>,
) -> LightSummary {
    let requests: Vec<String> = state
        .spaces
        .iter()
        .filter_map(|space| state.work_outline(&space.id).snapshot().cloned())
        .flat_map(|snapshot| {
            snapshot
                .user_action_requests
                .iter()
                .filter(|request| request.is_unresolved())
                .map(|request| request.text.clone())
                .collect::<Vec<_>>()
        })
        .collect();
    app_light(
        state
            .chats
            .iter()
            .filter(|chat| !chat.archived)
            .map(|chat| state.indicator_for(&chat.id, now)),
        requests,
    )
}

/// What the Dock should show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DockTarget {
    /// The bundle icon and no badge (the setting is off).
    #[default]
    Default,
    Live(LightSummary),
}

impl DockTarget {
    /// `None` restores the bundle icon.
    fn icon(self) -> Option<AppLight> {
        match self {
            Self::Default => None,
            Self::Live(summary) => Some(summary.light),
        }
    }

    fn badge(self) -> u32 {
        match self {
            Self::Default => 0,
            Self::Live(summary) => summary.badge,
        }
    }
}

/// The AppKit calls needed to go from `applied` to `next`: each field is
/// `Some` only when that part changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DockOps {
    /// `Some(None)` restores the bundle icon.
    pub icon: Option<Option<AppLight>>,
    pub badge: Option<u32>,
}

pub fn plan(applied: DockTarget, next: DockTarget) -> DockOps {
    DockOps {
        icon: (applied.icon() != next.icon()).then(|| next.icon()),
        badge: (applied.badge() != next.badge()).then(|| next.badge()),
    }
}

/// Debounces the Dock target: a changed target is applied once it has held for
/// [`DEBOUNCE`]; going back to the applied value cancels the pending change;
/// switching the feature off is immediate.
#[derive(Debug, Default)]
pub struct DockSync {
    applied: DockTarget,
    pending: Option<(DockTarget, Instant)>,
}

impl DockSync {
    /// Record the latest wanted target. Returns the ops to run right now (only
    /// for the immediate off switch).
    pub fn observe(&mut self, target: DockTarget, now: Instant) -> Option<DockOps> {
        if target == self.applied {
            self.pending = None;
            return None;
        }
        if target == DockTarget::Default {
            return self.commit(target);
        }
        match self.pending {
            Some((pending, _)) if pending == target => {}
            _ => self.pending = Some((target, now)),
        }
        None
    }

    /// The pending target has held long enough: commit it.
    pub fn take_due(&mut self, now: Instant) -> Option<DockOps> {
        let (target, since) = self.pending?;
        (now.saturating_duration_since(since) >= DEBOUNCE)
            .then(|| self.commit(target))
            .flatten()
    }

    /// A change is waiting for its debounce to elapse.
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Time until the pending change is due.
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        let (_, since) = self.pending?;
        Some(DEBOUNCE.saturating_sub(now.saturating_duration_since(since)))
    }

    fn commit(&mut self, target: DockTarget) -> Option<DockOps> {
        self.pending = None;
        let ops = plan(self.applied, target);
        self.applied = target;
        (ops.icon.is_some() || ops.badge.is_some()).then_some(ops)
    }
}

/// Run `ops` against AppKit. Main thread only; a no-op off macOS.
pub fn apply(ops: DockOps) {
    // Unit tests drive real shells; they must never repaint the developer's Dock.
    if cfg!(test) {
        return;
    }
    if let Some(icon) = ops.icon {
        dock::set_icon(icon.map(AppLight::dock_png));
    }
    if let Some(count) = ops.badge {
        dock::set_badge(count);
    }
}

// objc 0.2's `msg_send!` checks a `cfg(feature = "cargo-clippy")` this crate lacks.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
mod dock {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    /// Set the Dock icon from PNG bytes; `None` restores the bundle icon.
    pub fn set_icon(png: Option<&'static [u8]>) {
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            if app.is_null() {
                return;
            }
            let Some(png) = png else {
                let nil: *mut Object = std::ptr::null_mut();
                let _: () = msg_send![app, setApplicationIconImage: nil];
                return;
            };
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
            let _: () = msg_send![app, setApplicationIconImage: image];
            let _: () = msg_send![image, release];
        }
    }

    /// Set the Dock badge; zero clears it.
    pub fn set_badge(count: u32) {
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            if app.is_null() {
                return;
            }
            let tile: *mut Object = msg_send![app, dockTile];
            if tile.is_null() {
                return;
            }
            let label: *mut Object = if count == 0 {
                std::ptr::null_mut()
            } else {
                let text = std::ffi::CString::new(count.to_string()).unwrap_or_default();
                msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()]
            };
            let _: () = msg_send![tile, setBadgeLabel: label];
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod dock {
    pub fn set_icon(_png: Option<&'static [u8]>) {}
    pub fn set_badge(_count: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use Indicator::{AwaitingInput, Errored, None as Quiet, Working};

    fn light(indicators: &[Indicator], requests: &[&str]) -> LightSummary {
        app_light(indicators.iter().copied(), requests.iter().copied())
    }

    #[test]
    fn nothing_going_on_is_idle() {
        assert_eq!(light(&[], &[]), LightSummary { light: AppLight::Idle, badge: 0 });
        assert_eq!(light(&[Quiet, Quiet], &[]).light, AppLight::Idle);
    }

    #[test]
    fn working_alone_is_working_with_no_badge() {
        assert_eq!(light(&[Working, Quiet], &[]), LightSummary { light: AppLight::Working, badge: 0 });
    }

    #[test]
    fn a_ready_to_merge_request_is_ready_and_beats_working() {
        let summary = light(&[Working], &["Ready to merge: status-light"]);
        assert_eq!(summary, LightSummary { light: AppLight::Ready, badge: 1 });
        assert!(is_ready_to_merge("  Ready to merge: x"));
        assert!(!is_ready_to_merge("Is this Ready to merge?"));
    }

    #[test]
    fn awaiting_input_is_input_and_beats_ready_and_working() {
        let summary = light(&[Working, AwaitingInput], &["Ready to merge: a"]);
        assert_eq!(summary, LightSummary { light: AppLight::Input, badge: 2 });
    }

    #[test]
    fn any_other_open_request_is_input_even_without_a_waiting_chat() {
        let summary = light(&[Quiet], &["Which colour?", "Ready to merge: a"]);
        assert_eq!(summary, LightSummary { light: AppLight::Input, badge: 2 });
    }

    #[test]
    fn an_errored_chat_beats_everything() {
        let summary = light(&[Working, AwaitingInput, Errored], &["Which colour?", "Ready to merge: a"]);
        assert_eq!(summary, LightSummary { light: AppLight::Error, badge: 4 });
    }

    #[test]
    fn the_badge_counts_waiting_chats_errors_and_every_open_request() {
        let summary = light(
            &[AwaitingInput, AwaitingInput, Errored, Working],
            &["a", "b", "Ready to merge: x"],
        );
        assert_eq!(summary.badge, 2 + 1 + 3);
    }

    fn live(light: AppLight, badge: u32) -> DockTarget {
        DockTarget::Live(LightSummary { light, badge })
    }

    #[test]
    fn plan_touches_only_what_changed() {
        let a = live(AppLight::Working, 0);
        assert_eq!(plan(a, a), DockOps { icon: None, badge: None });
        assert_eq!(
            plan(a, live(AppLight::Working, 3)),
            DockOps { icon: None, badge: Some(3) }
        );
        assert_eq!(
            plan(a, live(AppLight::Input, 0)),
            DockOps { icon: Some(Some(AppLight::Input)), badge: None }
        );
        assert_eq!(
            plan(live(AppLight::Error, 2), DockTarget::Default),
            DockOps { icon: Some(None), badge: Some(0) }
        );
        assert_eq!(
            plan(DockTarget::Default, live(AppLight::Idle, 0)),
            DockOps { icon: Some(Some(AppLight::Idle)), badge: None }
        );
    }

    #[test]
    fn a_change_waits_out_the_debounce_then_applies_once() {
        let t0 = Instant::now();
        let mut sync = DockSync::default();
        let target = live(AppLight::Working, 0);
        assert_eq!(sync.observe(target, t0), None);
        assert!(sync.has_pending());
        assert_eq!(sync.take_due(t0 + Duration::from_millis(900)), None);
        let ops = sync.take_due(t0 + DEBOUNCE).expect("due after the debounce");
        assert_eq!(ops.icon, Some(Some(AppLight::Working)));
        assert!(!sync.has_pending());
        assert_eq!(sync.observe(target, t0 + DEBOUNCE), None);
        assert_eq!(sync.take_due(t0 + DEBOUNCE * 5), None);
    }

    #[test]
    fn a_flicker_back_to_the_applied_value_cancels_the_change() {
        let t0 = Instant::now();
        let mut sync = DockSync::default();
        sync.observe(live(AppLight::Idle, 0), t0);
        sync.take_due(t0 + DEBOUNCE);
        sync.observe(live(AppLight::Error, 1), t0 + DEBOUNCE);
        sync.observe(live(AppLight::Idle, 0), t0 + DEBOUNCE + Duration::from_millis(200));
        assert!(!sync.has_pending());
        assert_eq!(sync.take_due(t0 + DEBOUNCE * 4), None);
    }

    #[test]
    fn a_new_target_restarts_the_debounce_but_the_same_one_does_not() {
        let t0 = Instant::now();
        let mut sync = DockSync::default();
        sync.observe(live(AppLight::Working, 0), t0);
        sync.observe(live(AppLight::Working, 0), t0 + Duration::from_millis(600));
        assert!(sync.take_due(t0 + DEBOUNCE).is_some(), "same target keeps its clock");
        sync.observe(live(AppLight::Input, 1), t0 + DEBOUNCE);
        sync.observe(live(AppLight::Input, 2), t0 + DEBOUNCE + Duration::from_millis(600));
        assert_eq!(sync.take_due(t0 + DEBOUNCE + DEBOUNCE), None, "new target restarted");
        let ops = sync.take_due(t0 + DEBOUNCE * 2 + Duration::from_millis(600)).unwrap();
        assert_eq!(ops.badge, Some(2));
    }

    #[test]
    fn switching_the_feature_off_restores_immediately() {
        let t0 = Instant::now();
        let mut sync = DockSync::default();
        sync.observe(live(AppLight::Input, 2), t0);
        sync.take_due(t0 + DEBOUNCE);
        sync.observe(live(AppLight::Error, 3), t0 + DEBOUNCE);
        let ops = sync.observe(DockTarget::Default, t0 + DEBOUNCE).expect("immediate");
        assert_eq!(ops, DockOps { icon: Some(None), badge: Some(0) });
        assert!(!sync.has_pending());
        assert_eq!(sync.observe(DockTarget::Default, t0 + DEBOUNCE), None);
    }

    #[test]
    fn every_lamp_has_a_png_dock_icon_and_logos() {
        for lamp in AppLight::ALL {
            assert_eq!(&lamp.dock_png()[1..4], b"PNG");
            for appearance in [Appearance::Light, Appearance::Dark] {
                let asset = lamp.logo_asset(appearance);
                assert!(gpui::AssetSource::load(&crate::icons::Assets, asset).unwrap().is_some(), "{asset}");
            }
        }
    }

    #[test]
    fn each_lamp_logo_renders_its_own_lens_colour() {
        let renderer = gpui::SvgRenderer::new(std::sync::Arc::new(crate::icons::Assets));
        // A strongly saturated lens pixel of each lamp's hue (Cofactr palette).
        let hue: [(AppLight, fn(&[u8]) -> bool); 4] = [
            // clover green
            (AppLight::Working, |p| p[1] as i32 > p[0] as i32 + 60 && p[1] as i32 > p[2] as i32 + 40),
            // cobalt blue
            (AppLight::Ready, |p| p[0] as i32 > p[1] as i32 + 50 && p[0] as i32 > p[2] as i32 + 80),
            // bromine amber
            (AppLight::Input, |p| p[2] as i32 > 180 && p[1] as i32 > 150 && (p[0] as i32) < 90),
            // cadmium orange-red
            (AppLight::Error, |p| p[2] as i32 > 200 && (p[1] as i32) < 110 && (p[0] as i32) < 90),
        ];
        for appearance in [Appearance::Light, Appearance::Dark] {
            for (lamp, has_hue) in hue {
                let asset = lamp.logo_asset(appearance);
                let bytes = gpui::AssetSource::load(&crate::icons::Assets, asset).unwrap().unwrap();
                assert!(bytes.len() < 4096 + 1024, "{asset} is {} bytes", bytes.len());
                let image = renderer.render_single_frame(&bytes, 2.0).expect("logo renders");
                let px = image.as_bytes(0).unwrap(); // premultiplied BGRA
                assert_eq!(px[3], 0, "{asset}: top-left corner is transparent");
                let found = px
                    .chunks_exact(4)
                    .filter(|p| p[3] == 255)
                    .any(|p| has_hue(&[p[0], p[1], p[2]]));
                assert!(found, "{asset}: lens shows its lamp colour");
            }
        }
    }
}
