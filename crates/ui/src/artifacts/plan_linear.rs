//! The canonical plan's Linear project panel (Artifacts phase 5 H): the
//! linked project's name, state, progress, target date, lead and latest
//! updates, shown next to the pinned plan. Read-only: the data is the host's
//! cached `LinearProjectDetail`; nothing is ever written to Linear.
//!
//! [`model`] turns the detail into display strings (pure, tested);
//! [`render`] draws the panel for the artifact tab and the item details.
use chrono::{NaiveDate, TimeZone, Utc};
use gpui::{
    AnyElement, IntoElement, ParentElement, SharedString, Styled, WeakEntity, div, prelude::*, px,
};
use zeron_proto::linear::{LinearProjectDetail, LinearUser};

use crate::linear::store::ProjectDetailEntry;
use crate::theme::Theme;

/// How a state or health reads, mapped to a theme colour at render time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    Neutral,
    Good,
    Warn,
    Bad,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpdateLine {
    /// "3d ago".
    pub(crate) when: String,
    pub(crate) health: Option<(String, Tone)>,
    pub(crate) author: Option<String>,
    /// The first lines of the body, cut short.
    pub(crate) text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PanelModel {
    pub(crate) name: String,
    pub(crate) url: String,
    pub(crate) state: Option<(String, Tone)>,
    /// `0.0..=1.0`.
    pub(crate) progress: Option<f32>,
    pub(crate) progress_label: Option<String>,
    /// "Dec 1, 2026", with " (overdue)" appended when it has passed.
    pub(crate) target: Option<(String, bool)>,
    pub(crate) lead: Option<String>,
    pub(crate) updates: Vec<UpdateLine>,
}

fn user_name(user: &LinearUser) -> String {
    user.display_name
        .as_deref()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or(&user.name)
        .to_owned()
}

/// Linear's project status string as words, plus its tone.
pub(crate) fn state_label(state: &str) -> (String, Tone) {
    match state.trim().to_ascii_lowercase().as_str() {
        "started" | "in progress" | "inprogress" => ("In progress".into(), Tone::Good),
        "planned" => ("Planned".into(), Tone::Neutral),
        "backlog" => ("Backlog".into(), Tone::Neutral),
        "paused" => ("Paused".into(), Tone::Warn),
        "completed" => ("Completed".into(), Tone::Good),
        "canceled" | "cancelled" => ("Canceled".into(), Tone::Bad),
        other => {
            let mut chars = other.chars();
            let label = chars
                .next()
                .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default();
            (label, Tone::Neutral)
        }
    }
}

fn health_label(health: &str) -> (String, Tone) {
    match health {
        "onTrack" => ("On track".into(), Tone::Good),
        "atRisk" => ("At risk".into(), Tone::Warn),
        "offTrack" => ("Off track".into(), Tone::Bad),
        other => (other.to_owned(), Tone::Neutral),
    }
}

/// "Dec 1, 2026" and whether it is before `now_ms`'s day.
pub(crate) fn target_label(date: &str, now_ms: i64) -> Option<(String, bool)> {
    let day = NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").ok()?;
    let today = Utc.timestamp_millis_opt(now_ms).single()?.date_naive();
    Some((day.format("%b %-d, %Y").to_string(), day < today))
}

fn first_lines(body: &str) -> String {
    let text = body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    if text.chars().count() > 200 {
        text.chars().take(200).collect::<String>() + "\u{2026}"
    } else {
        text
    }
}

pub(crate) fn model(detail: &LinearProjectDetail, now_ms: i64) -> PanelModel {
    PanelModel {
        name: detail.name.clone(),
        url: detail.url.clone(),
        state: detail.state.as_deref().map(state_label),
        progress: detail.progress,
        progress_label: detail
            .progress
            .map(|p| format!("{}%", (p * 100.0).round() as u32)),
        target: detail
            .target_date
            .as_deref()
            .and_then(|d| target_label(d, now_ms)),
        lead: detail.lead.as_ref().map(user_name),
        updates: detail
            .updates
            .iter()
            .map(|update| UpdateLine {
                when: chrono::DateTime::parse_from_rfc3339(&update.created_at)
                    .map(|at| super::questions_ui::age_label(at.timestamp_millis(), now_ms))
                    .unwrap_or_default(),
                health: update.health.as_deref().map(health_label),
                author: update.author.as_ref().map(user_name),
                text: first_lines(&update.body),
            })
            .collect(),
    }
}

fn tone_color(tone: Tone, theme: &Theme) -> gpui::Hsla {
    match tone {
        Tone::Neutral => theme.text_muted,
        Tone::Good => theme.success,
        Tone::Warn => theme.warning,
        Tone::Bad => theme.danger,
    }
}

/// The panel. `id` prefixes every debug selector. `fallback` is the linked
/// reference's cached title and URL, used until the detail arrives.
pub(crate) fn render(
    id: &str,
    entry: Option<&ProjectDetailEntry>,
    fallback: (&str, &str),
    now_ms: i64,
    shell: Option<WeakEntity<crate::shell::Shell>>,
    theme: &Theme,
) -> AnyElement {
    let selector = |suffix: &str| -> SharedString { format!("{id}-{suffix}").into() };
    let frame = |sel: SharedString| {
        div()
            .id(sel.clone())
            .debug_selector(move || sel.to_string())
            .flex()
            .flex_col()
            .gap(px(4.0))
            .px(px(10.0))
            .py(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_card)
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text_muted)
    };
    let note = |sel: SharedString, text: String| {
        frame(sel)
            .child(div().min_w_0().child(SharedString::from(text)))
            .into_any_element()
    };
    let detail = match entry {
        Some(ProjectDetailEntry::Ready { detail, .. }) => detail,
        Some(ProjectDetailEntry::Failed { error, .. }) => {
            return note(
                selector("error"),
                format!("{}: {}", fallback.0, error.user_text()),
            );
        }
        _ => {
            return note(
                selector("loading"),
                format!("Loading {}\u{2026}", fallback.0),
            );
        }
    };
    let model = model(detail, now_ms);
    let url = if model.url.is_empty() {
        fallback.1.to_owned()
    } else {
        model.url.clone()
    };
    let title = div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(
            crate::icons::icon(crate::icons::CHECKLIST)
                .size(px(12.0))
                .flex_none()
                .text_color(theme.text_faint),
        )
        .child(
            div()
                .id(selector("name"))
                .debug_selector({
                    let sel = selector("name");
                    move || sel.to_string()
                })
                .min_w_0()
                .truncate()
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(12.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .hover(|el| el.text_color(theme.accent))
                .child(SharedString::from(model.name.clone()))
                .on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    crate::github::chip::open_link(
                        shell.as_ref(),
                        &url,
                        crate::github::chip::click_forces_external(event),
                        window,
                        cx,
                    );
                }),
        )
        .when_some(model.state.clone(), |el, (label, tone)| {
            el.child(
                div()
                    .debug_selector({
                        let sel = selector("state");
                        move || sel.to_string()
                    })
                    .flex_none()
                    .px(px(5.0))
                    .rounded(px(3.0))
                    .border_1()
                    .border_color(theme.border)
                    .text_size(crate::typography::ui_rems(10.0))
                    .text_color(tone_color(tone, theme))
                    .child(SharedString::from(label)),
            )
        });
    let progress = model.progress.map(|fraction| {
        div()
            .debug_selector({
                let sel = selector("progress");
                move || sel.to_string()
            })
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .flex_1()
                    .h(px(4.0))
                    .rounded(px(2.0))
                    .bg(crate::theme::wash(0.12))
                    .child(
                        div()
                            .h_full()
                            .rounded(px(2.0))
                            .bg(theme.accent)
                            .w(gpui::relative(fraction.clamp(0.0, 1.0))),
                    ),
            )
            .child(SharedString::from(
                model.progress_label.clone().unwrap_or_default(),
            ))
    });
    let facts = div()
        .flex()
        .flex_wrap()
        .gap_x(px(12.0))
        .text_color(theme.text_muted)
        .when_some(model.target.clone(), |el, (date, overdue)| {
            el.child(
                div()
                    .debug_selector({
                        let sel = selector("target");
                        move || sel.to_string()
                    })
                    .text_color(if overdue {
                        theme.warning
                    } else {
                        theme.text_muted
                    })
                    .child(SharedString::from(if overdue {
                        format!("Target {date} (overdue)")
                    } else {
                        format!("Target {date}")
                    })),
            )
        })
        .when_some(model.lead.clone(), |el, lead| {
            el.child(
                div()
                    .debug_selector({
                        let sel = selector("lead");
                        move || sel.to_string()
                    })
                    .child(SharedString::from(format!("Lead {lead}"))),
            )
        });
    let updates = model.updates.iter().enumerate().map(|(ix, update)| {
        let sel = selector(&format!("update-{ix}"));
        let mut head: Vec<String> = Vec::new();
        if let Some(author) = &update.author {
            head.push(author.clone());
        }
        if !update.when.is_empty() {
            head.push(update.when.clone());
        }
        div()
            .debug_selector(move || sel.to_string())
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_color(theme.text_faint)
                    .when_some(update.health.clone(), |el, (label, tone)| {
                        el.child(
                            div()
                                .text_color(tone_color(tone, theme))
                                .child(SharedString::from(label)),
                        )
                    })
                    .child(SharedString::from(head.join(" \u{b7} "))),
            )
            .child(
                div()
                    .text_color(theme.text)
                    .child(SharedString::from(update.text.clone())),
            )
    });
    frame(selector("panel"))
        .child(title)
        .children(progress)
        .child(facts)
        .when(!model.updates.is_empty(), |el| {
            el.child(
                div()
                    .pt(px(2.0))
                    .text_color(theme.text_faint)
                    .child("Recent updates"),
            )
            .children(updates)
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeron_proto::linear::LinearProjectUpdate;

    // 2026-10-28T12:00:00Z
    const NOW: i64 = 1_793_188_800_000;

    fn detail() -> LinearProjectDetail {
        LinearProjectDetail {
            id: "p1".into(),
            name: "Receiving".into(),
            url: "https://linear.app/x/project/p1".into(),
            state: Some("started".into()),
            progress: Some(0.426),
            target_date: Some("2026-12-01".into()),
            lead: Some(LinearUser {
                id: "u".into(),
                name: "Ada Lovelace".into(),
                display_name: Some("ada".into()),
                email: None,
            }),
            updates: vec![LinearProjectUpdate {
                id: "up".into(),
                body: "\nShipped login.\n\nNext: billing\nthird line".into(),
                health: Some("atRisk".into()),
                author: Some(LinearUser {
                    id: "u".into(),
                    name: "Ada".into(),
                    ..Default::default()
                }),
                created_at: "2026-10-25T12:00:00Z".into(),
            }],
            updated_at: String::new(),
        }
    }

    #[test]
    fn the_model_reads_state_progress_target_lead_and_updates() {
        let m = model(&detail(), NOW);
        assert_eq!(m.state, Some(("In progress".into(), Tone::Good)));
        assert_eq!(m.progress_label.as_deref(), Some("43%"));
        assert_eq!(m.target, Some(("Dec 1, 2026".into(), false)));
        assert_eq!(m.lead.as_deref(), Some("ada"));
        assert_eq!(m.updates.len(), 1);
        assert_eq!(m.updates[0].when, "3d ago");
        assert_eq!(m.updates[0].health, Some(("At risk".into(), Tone::Warn)));
        assert_eq!(m.updates[0].text, "Shipped login. Next: billing");
    }

    #[test]
    fn a_passed_target_is_overdue_and_odd_states_read_plainly() {
        assert_eq!(
            target_label("2026-10-01", NOW),
            Some(("Oct 1, 2026".into(), true))
        );
        assert_eq!(
            target_label("2026-10-28", NOW),
            Some(("Oct 28, 2026".into(), false))
        );
        assert_eq!(target_label("soon", NOW), None);
        assert_eq!(state_label("canceled"), ("Canceled".into(), Tone::Bad));
        assert_eq!(state_label("on hold"), ("On hold".into(), Tone::Neutral));
    }
}
