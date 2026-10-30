//! Orchestrator wakes in the shell: the per-chat `WakeStatus` cache, its
//! refresh while the Team / Item tab is visible, the settings control (Team
//! tab header, compact in the Item tab) and the "unseen by the orchestrator"
//! marks on worker rows. Pure text/mapping logic lives in
//! `crate::orchestrator_wakes`.
use super::*;

use std::collections::{HashMap, HashSet};

use mast_proto::{WakeSettings, WakeStatus};

use crate::orchestrator_wakes::{WAKE_REFRESH, WakeControl, WakeToggle};
use crate::shell::chat_item_ui::{ChipTone, chip_palette};

/// Wake state owned by the shell.
#[derive(Default)]
pub(super) struct WakeUi {
    pub statuses: HashMap<String, WakeStatus>,
    /// Chats with a live refresh loop.
    looping: HashSet<String>,
}

impl WakeUi {
    /// A status for tests and for the RPC answers.
    pub(super) fn set(&mut self, status: WakeStatus) -> bool {
        if self.statuses.get(&status.chat_id) == Some(&status) {
            return false;
        }
        self.statuses.insert(status.chat_id.clone(), status);
        true
    }
}

impl Shell {
    /// Is the Team or Item tab of `chat_id` on screen?
    fn wake_tabs_visible(&self, chat_id: &str, cx: &App) -> bool {
        self.active_chat == chat_id
            && self.right_pane_open(cx)
            && matches!(
                self.resolved_right_active(cx),
                RightSurface::Item | RightSurface::Team
            )
    }

    /// Start (once) a refresh loop for `chat_id`: fetch now, then every
    /// [`WAKE_REFRESH`] while the tabs stay visible. No engine, no loop.
    pub(super) fn ensure_wake_status(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        if self.wakes.looping.contains(chat_id) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let id = chat_id.to_owned();
        self.wakes.looping.insert(id.clone());
        cx.spawn(async move |this, cx| {
            loop {
                let status = engine
                    .client()
                    .call_as::<WakeStatus>(
                        methods::GET_WAKE_STATUS,
                        serde_json::json!({ "chatId": id }),
                    )
                    .await
                    .ok();
                let keep = this
                    .update(cx, |shell, cx| {
                        if let Some(status) = status
                            && shell.wakes.set(status)
                        {
                            cx.notify();
                        }
                        let keep = shell.wake_tabs_visible(&id, cx);
                        if !keep {
                            shell.wakes.looping.remove(&id);
                        }
                        keep
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
                cx.background_executor().timer(WAKE_REFRESH).await;
            }
        })
        .detach();
    }

    /// Flip one switch: the cache changes at once, the engine's answer wins.
    pub(crate) fn toggle_wake(&mut self, chat_id: &str, toggle: WakeToggle, cx: &mut Context<Self>) {
        let current = self
            .wakes
            .statuses
            .get(chat_id)
            .map(|status| status.settings)
            .unwrap_or_default();
        let params = toggle.params(chat_id, current);
        self.wakes
            .statuses
            .entry(chat_id.to_owned())
            .or_insert_with(|| WakeStatus {
                chat_id: chat_id.to_owned(),
                is_orchestrator: true,
                ..Default::default()
            })
            .settings = params.settings;
        cx.notify();
        let Ok(value) = serde_json::to_value(&params) else {
            return;
        };
        self.call_wake(methods::SET_WAKE_SETTINGS, value, cx);
    }

    /// "Deliver now": the engine sends the held batch.
    pub(crate) fn deliver_wake_now(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        self.call_wake(
            methods::WAKE_DELIVER_NOW,
            serde_json::json!({ "chatId": chat_id }),
            cx,
        );
    }

    fn call_wake(&mut self, method: &'static str, params: serde_json::Value, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            if let Ok(status) = engine.client().call_as::<WakeStatus>(method, params).await {
                this.update(cx, |shell, cx| {
                    if shell.wakes.set(status) {
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// Summaries of the events of `worker_chat_id` the active orchestrator
    /// has not seen (empty when caught up).
    pub(super) fn unseen_for_worker(&self, worker_chat_id: &str) -> Vec<String> {
        self.wakes
            .statuses
            .get(&self.active_chat)
            .map(crate::orchestrator_wakes::unseen_by_worker)
            .and_then(|mut map| map.remove(worker_chat_id))
            .unwrap_or_default()
    }

    fn wake_toggle_chip(
        &self,
        chat_id: &str,
        prefix: &str,
        toggle: WakeToggle,
        settings: &WakeSettings,
        dimmed: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let on = toggle.get(settings);
        let (border, text) = chip_palette(theme, if on { ChipTone::Link } else { ChipTone::Dim });
        let selector = format!("{prefix}-toggle-{}", toggle.key());
        let chat_id = chat_id.to_owned();
        let label = if on {
            format!("\u{2713} {}", toggle.label())
        } else {
            toggle.label().to_owned()
        };
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .flex_none()
            .h(px(20.0))
            .px(px(7.0))
            .flex()
            .items_center()
            .rounded(px(5.0))
            .border_1()
            .border_color(border)
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(text)
            .when(dimmed, |el| el.opacity(0.5))
            .cursor_pointer()
            .hover(|el| el.bg(theme.element_hover.opacity(0.5)))
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(format!(
                "{}: {}",
                toggle.label(),
                if on { "on" } else { "off" }
            )))
            .child(SharedString::from(label))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_wake(&chat_id, toggle, cx);
            }))
            .into_any_element()
    }

    /// The held chip: "N updates held", why, and Deliver now.
    fn render_wake_held(
        &self,
        chat_id: &str,
        prefix: &str,
        control: &WakeControl,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let chip = control.held.as_ref()?;
        let (border, text) = chip_palette(theme, ChipTone::Warn);
        let chat_id = chat_id.to_owned();
        let deliver = format!("{prefix}-deliver");
        Some(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.0))
                .child(
                    div()
                        .debug_selector({
                            let selector = format!("{prefix}-held");
                            move || selector.clone()
                        })
                        .flex_none()
                        .h(px(18.0))
                        .px(px(6.0))
                        .flex()
                        .items_center()
                        .rounded(px(5.0))
                        .border_1()
                        .border_color(border)
                        .text_size(crate::typography::ui_rems(10.5))
                        .text_color(text)
                        .child(SharedString::from(chip.text.clone())),
                )
                .children(chip.reason.map(|reason| {
                    div()
                        .debug_selector({
                            let selector = format!("{prefix}-held-reason");
                            move || selector.clone()
                        })
                        .min_w_0()
                        .text_size(crate::typography::ui_rems(10.5))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(reason))
                }))
                .child(
                    div()
                        .id(SharedString::from(deliver.clone()))
                        .debug_selector(move || deliver.clone())
                        .flex_none()
                        .px(px(6.0))
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(theme.accent.opacity(0.5))
                        .text_size(crate::typography::ui_rems(10.5))
                        .text_color(theme.accent)
                        .cursor_pointer()
                        .hover(|el| el.bg(theme.accent.opacity(0.12)))
                        .child("Deliver now")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.deliver_wake_now(&chat_id, cx);
                        })),
                )
                .into_any_element(),
        )
    }

    /// The switches: the master toggle, then one per event kind.
    fn render_wake_toggles(
        &self,
        chat_id: &str,
        prefix: &str,
        control: &WakeControl,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let enabled = control.settings.enabled;
        let mut kinds = div().flex().flex_wrap().items_center().gap(px(4.0));
        for toggle in WakeToggle::KINDS {
            kinds = kinds.child(self.wake_toggle_chip(
                chat_id, prefix, toggle, &control.settings, !enabled, theme, cx,
            ));
        }
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div().flex().child(self.wake_toggle_chip(
                    chat_id,
                    prefix,
                    WakeToggle::Enabled,
                    &control.settings,
                    false,
                    theme,
                    cx,
                )),
            )
            .child(kinds)
            .into_any_element()
    }

    /// The Team tab header: switches, held chip, unseen roll-up.
    pub(super) fn render_team_wake_header(
        &mut self,
        chat_id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.ensure_wake_status(chat_id, cx);
        let control = WakeControl::of(self.wakes.statuses.get(chat_id));
        let prefix = "team-wake";
        let mut block = div()
            .debug_selector(|| "team-wake".into())
            .flex()
            .flex_col()
            .gap(px(5.0))
            .child(self.render_wake_toggles(chat_id, prefix, &control, theme, cx));
        block = block.children(self.render_wake_held(chat_id, prefix, &control, theme, cx));
        if let Some(unseen) = &control.unseen {
            block = block.child(
                div()
                    .debug_selector(|| "team-wake-unseen".into())
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.warning)
                    .child(SharedString::from(unseen.clone())),
            );
        }
        block.into_any_element()
    }

    /// The Item tab's compact version: an accordion row with the switches,
    /// and the held chip (with Deliver now) always visible below it.
    pub(super) fn render_item_tab_wake(
        &mut self,
        chat_id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.ensure_wake_status(chat_id, cx);
        let control = WakeControl::of(self.wakes.statuses.get(chat_id));
        let prefix = "chat-item-wake";
        let body = self.render_wake_toggles(chat_id, prefix, &control, theme, cx);
        let accordion = self.item_accordion(
            "wakes",
            "Wakes",
            control.summary(),
            control.held.is_some(),
            Some(body),
            theme,
            cx,
        );
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(accordion)
            .children(
                self.render_wake_held(chat_id, prefix, &control, theme, cx)
                    .map(|held| div().pl(px(15.0)).child(held)),
            )
            .into_any_element()
    }
}
