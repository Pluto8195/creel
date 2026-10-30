//! Orchestrator wakes, UI side: pure view-model logic (settings toggles, the
//! "N updates held" chip, hold-reason text, unseen marks) and the user-activity
//! reporter. No rendering and no fetching here; `shell/wake_ui.rs` draws it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use mast_proto::{
    NoteChatUserActivityParams, SetWakeSettingsParams, WakeHoldReason, WakeSettings, WakeStatus,
};

/// How often a visible Team / Item tab re-reads the wake status.
pub(crate) const WAKE_REFRESH: Duration = Duration::from_secs(5);
/// Longest gap between activity pings for one chat (unless the draft flag flips).
pub(crate) const ACTIVITY_THROTTLE: Duration = Duration::from_secs(10);

/// One switch in the wake control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WakeToggle {
    Enabled,
    RequiredActions,
    Errors,
    TurnFinished,
    Prs,
}

impl WakeToggle {
    pub(crate) const KINDS: [WakeToggle; 4] = [
        Self::RequiredActions,
        Self::Errors,
        Self::TurnFinished,
        Self::Prs,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Enabled => "Wake me on worker updates",
            Self::RequiredActions => "Required Actions",
            Self::Errors => "Errors",
            Self::TurnFinished => "Turn finished",
            Self::Prs => "PRs",
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::RequiredActions => "required-actions",
            Self::Errors => "errors",
            Self::TurnFinished => "turn-finished",
            Self::Prs => "prs",
        }
    }

    pub(crate) fn get(self, settings: &WakeSettings) -> bool {
        match self {
            Self::Enabled => settings.enabled,
            Self::RequiredActions => settings.required_actions,
            Self::Errors => settings.errors,
            Self::TurnFinished => settings.turn_finished,
            Self::Prs => settings.prs,
        }
    }

    /// `settings` with this switch flipped.
    pub(crate) fn flipped(self, mut settings: WakeSettings) -> WakeSettings {
        let slot = match self {
            Self::Enabled => &mut settings.enabled,
            Self::RequiredActions => &mut settings.required_actions,
            Self::Errors => &mut settings.errors,
            Self::TurnFinished => &mut settings.turn_finished,
            Self::Prs => &mut settings.prs,
        };
        *slot = !*slot;
        settings
    }

    /// What toggling this switch sends.
    pub(crate) fn params(self, chat_id: &str, settings: WakeSettings) -> SetWakeSettingsParams {
        SetWakeSettingsParams {
            chat_id: chat_id.to_owned(),
            settings: self.flipped(settings),
        }
    }
}

/// "Waiting: you're typing" and friends.
pub(crate) fn hold_reason_text(reason: WakeHoldReason) -> &'static str {
    match reason {
        WakeHoldReason::Window => "Waiting for batch window",
        WakeHoldReason::Busy => "Waiting: turn running",
        WakeHoldReason::Queued => "Waiting: your queued message",
        WakeHoldReason::Draft => "Waiting: you're typing",
        WakeHoldReason::RecentActivity => "Waiting: you were here recently",
    }
}

/// "1 update held" / "3 updates held".
pub(crate) fn held_chip_text(count: usize) -> String {
    if count == 1 {
        "1 update held".to_owned()
    } else {
        format!("{count} updates held")
    }
}

/// "1 event unseen by the orchestrator" / "3 events unseen by the orchestrator".
pub(crate) fn unseen_rollup_text(count: u32) -> String {
    if count == 1 {
        "1 event unseen by the orchestrator".to_owned()
    } else {
        format!("{count} events unseen by the orchestrator")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeldChip {
    pub text: String,
    pub reason: Option<&'static str>,
}

/// Everything the control draws, from one (possibly not yet fetched) status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WakeControl {
    pub settings: WakeSettings,
    pub held: Option<HeldChip>,
    /// Header roll-up; `None` when the orchestrator is caught up.
    pub unseen: Option<String>,
}

impl WakeControl {
    pub(crate) fn of(status: Option<&WakeStatus>) -> Self {
        let Some(status) = status else {
            return Self {
                settings: WakeSettings::default(),
                held: None,
                unseen: None,
            };
        };
        Self {
            settings: status.settings,
            held: (!status.held.is_empty()).then(|| HeldChip {
                text: held_chip_text(status.held.len()),
                reason: status.hold_reason.map(hold_reason_text),
            }),
            unseen: (status.unseen_count > 0).then(|| unseen_rollup_text(status.unseen_count)),
        }
    }

    /// One line for the Item tab's accordion row.
    pub(crate) fn summary(&self) -> String {
        let mut text = if self.settings.enabled { "On" } else { "Off" }.to_owned();
        if let Some(chip) = &self.held {
            text.push_str(" \u{b7} ");
            text.push_str(&chip.text);
        }
        text
    }
}

/// Worker chat id -> summaries of its events the orchestrator has not seen
/// (oldest first), from `recent` (newest first).
pub(crate) fn unseen_by_worker(status: &WakeStatus) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for event in status.recent.iter().rev().filter(|event| event.unseen) {
        out.entry(event.worker_chat_id.clone())
            .or_default()
            .push(event.summary.clone());
    }
    out
}

/// Decides which user-activity pings reach the engine: a flipped draft flag
/// goes out at once; anything else at most once per [`ACTIVITY_THROTTLE`].
#[derive(Debug, Default)]
pub(crate) struct ActivityReporter {
    last: HashMap<String, Sent>,
}

#[derive(Debug, Clone, Copy)]
struct Sent {
    at: Instant,
    draft: bool,
}

impl ActivityReporter {
    /// The user acted in `chat_id`. `draft_non_empty` is the composer's
    /// current state when the caller knows it.
    pub(crate) fn note(
        &mut self,
        chat_id: &str,
        draft_non_empty: Option<bool>,
        now: Instant,
    ) -> Option<NoteChatUserActivityParams> {
        if chat_id.is_empty() {
            return None;
        }
        let previous = self.last.get(chat_id).copied();
        let known = previous.map(|sent| sent.draft).unwrap_or(false);
        if let Some(draft) = draft_non_empty
            && draft != known
        {
            self.last
                .insert(chat_id.to_owned(), Sent { at: now, draft });
            return Some(NoteChatUserActivityParams {
                chat_id: chat_id.to_owned(),
                draft_non_empty: Some(draft),
            });
        }
        if previous.is_some_and(|sent| now.duration_since(sent.at) < ACTIVITY_THROTTLE) {
            return None;
        }
        self.last.insert(
            chat_id.to_owned(),
            Sent {
                at: now,
                draft: known,
            },
        );
        Some(NoteChatUserActivityParams {
            chat_id: chat_id.to_owned(),
            draft_non_empty: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::{WakeEventKind, WakeEventView};

    fn event(id: &str, worker: &str, unseen: bool) -> WakeEventView {
        WakeEventView {
            id: id.into(),
            worker_chat_id: worker.into(),
            worker_title: None,
            kind: WakeEventKind::Errored,
            summary: format!("summary {id}"),
            at_ms: 1,
            delivered: false,
            unseen,
        }
    }

    #[test]
    fn chip_and_reason_text() {
        assert_eq!(held_chip_text(1), "1 update held");
        assert_eq!(held_chip_text(3), "3 updates held");
        assert_eq!(hold_reason_text(WakeHoldReason::Draft), "Waiting: you're typing");
        assert_eq!(hold_reason_text(WakeHoldReason::Busy), "Waiting: turn running");
        assert_eq!(hold_reason_text(WakeHoldReason::Queued), "Waiting: your queued message");
        assert_eq!(hold_reason_text(WakeHoldReason::Window), "Waiting for batch window");
        assert_eq!(
            hold_reason_text(WakeHoldReason::RecentActivity),
            "Waiting: you were here recently"
        );
        assert_eq!(unseen_rollup_text(3), "3 events unseen by the orchestrator");
        assert_eq!(unseen_rollup_text(1), "1 event unseen by the orchestrator");
    }

    #[test]
    fn control_shows_chip_only_with_held_events() {
        let none = WakeControl::of(None);
        assert_eq!(none.settings, WakeSettings::default());
        assert!(none.held.is_none() && none.unseen.is_none());
        let status = WakeStatus {
            chat_id: "c".into(),
            is_orchestrator: true,
            held: vec![event("1", "w", false), event("2", "w", false)],
            hold_reason: Some(WakeHoldReason::Draft),
            unseen_count: 2,
            ..Default::default()
        };
        let control = WakeControl::of(Some(&status));
        let chip = control.held.unwrap();
        assert_eq!(chip.text, "2 updates held");
        assert_eq!(chip.reason, Some("Waiting: you're typing"));
        assert_eq!(control.unseen.as_deref(), Some("2 events unseen by the orchestrator"));
        let caught_up = WakeStatus { held: vec![], unseen_count: 0, ..status };
        let control = WakeControl::of(Some(&caught_up));
        assert!(control.held.is_none() && control.unseen.is_none());
    }

    #[test]
    fn toggles_flip_one_switch_into_set_params() {
        let base = WakeSettings::default();
        let p = WakeToggle::TurnFinished.params("c", base);
        assert_eq!(p.chat_id, "c");
        assert!(p.settings.turn_finished && p.settings.enabled && p.settings.errors);
        let p = WakeToggle::Enabled.params("c", base);
        assert!(!p.settings.enabled && p.settings.required_actions);
        let p = WakeToggle::Errors.params("c", base);
        assert!(!p.settings.errors);
        assert!(WakeToggle::Prs.get(&WakeToggle::Prs.flipped(base)));
    }

    #[test]
    fn unseen_events_map_to_their_worker() {
        let status = WakeStatus {
            recent: vec![event("3", "w1", true), event("2", "w2", false), event("1", "w1", true)],
            ..Default::default()
        };
        let map = unseen_by_worker(&status);
        assert_eq!(map["w1"], ["summary 1", "summary 3"]);
        assert!(!map.contains_key("w2"));
    }

    #[test]
    fn reporter_throttles_and_sends_draft_flips_at_once() {
        let mut r = ActivityReporter::default();
        let t0 = Instant::now();
        assert_eq!(r.note("", Some(true), t0), None);
        // First ping: plain activity.
        assert_eq!(r.note("c", None, t0).unwrap().draft_non_empty, None);
        // Within the window: dropped, even while typing with an unchanged flag.
        assert_eq!(r.note("c", None, t0 + Duration::from_secs(3)), None);
        assert_eq!(r.note("c", Some(false), t0 + Duration::from_secs(4)), None);
        // Draft becomes non-empty: immediate.
        let sent = r.note("c", Some(true), t0 + Duration::from_secs(5)).unwrap();
        assert_eq!(sent.draft_non_empty, Some(true));
        // Still typing: throttled from the flip.
        assert_eq!(r.note("c", Some(true), t0 + Duration::from_secs(8)), None);
        // Cleared: immediate again.
        assert_eq!(
            r.note("c", Some(false), t0 + Duration::from_secs(9)).unwrap().draft_non_empty,
            Some(false)
        );
        // After the window a plain ping goes out, without a flag.
        let later = r.note("c", Some(false), t0 + Duration::from_secs(30)).unwrap();
        assert_eq!(later.draft_non_empty, None);
        // Other chats are independent.
        assert!(r.note("d", None, t0 + Duration::from_secs(30)).is_some());
    }
}
