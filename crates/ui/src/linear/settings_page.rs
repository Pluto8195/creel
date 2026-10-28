//! Settings → Linear: connection status, the masked API-key field with Save
//! (`LinearSetApiKey`, which validates with Linear before storing),
//! Disconnect (`LinearClearApiKey`), and a link to Linear's key page. The
//! key is never displayed or logged; it is sent once and the field clears.
use gpui::{Context, Entity, SharedString, Subscription, Window, div, prelude::*, px};
use mast_proto::linear::{LinearConnectionStatus, LinearErrorCode, LinearKeySource};

use super::client::LinearUiError;
use super::masked_input::{MaskedKeyEvent, MaskedKeyInput};
use super::store;
use crate::observe_gate::ObserveGate;
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

/// What the status card says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConnectionView {
    Checking,
    Connected {
        name: String,
        from_environment: bool,
    },
    /// A key is set but the host could not verify it.
    Unverified(String),
    NotConfigured,
    NotAvailable,
    Error(String),
}

pub(crate) fn connection_view(
    status: Option<&Result<LinearConnectionStatus, LinearUiError>>,
) -> ConnectionView {
    match status {
        None => ConnectionView::Checking,
        Some(Ok(status)) if status.configured => match &status.viewer {
            Some(viewer) => ConnectionView::Connected {
                name: super::user_name(viewer),
                from_environment: status.source == LinearKeySource::Environment,
            },
            None => ConnectionView::Unverified(
                status
                    .error
                    .clone()
                    .unwrap_or_else(|| "Not verified yet".to_string()),
            ),
        },
        Some(Ok(_)) => ConnectionView::NotConfigured,
        Some(Err(error)) if error.not_available() => ConnectionView::NotAvailable,
        Some(Err(error)) if error.is(LinearErrorCode::MissingKey) => ConnectionView::NotConfigured,
        Some(Err(error)) => ConnectionView::Error(error.user_text()),
    }
}

/// The message shown under the key field when Save fails.
pub(crate) fn save_error_text(error: &LinearUiError) -> String {
    if error.is(LinearErrorCode::Unauthorized) {
        "Linear rejected this key. Check it and try again.".to_string()
    } else {
        error.user_text()
    }
}

pub(crate) struct LinearSettingsPage {
    state: Entity<AppState>,
    pub(crate) key_input: Entity<MaskedKeyInput>,
    scroll: widgets::PageScroll,
    pub(crate) saving: bool,
    pub(crate) disconnecting: bool,
    pub(crate) save_error: Option<String>,
    seen: ObserveGate<ConnectionView>,
    _subscriptions: Vec<Subscription>,
}

impl LinearSettingsPage {
    pub(crate) fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let key_input = cx.new(|cx| MaskedKeyInput::new("Paste a Linear personal API key", cx));
        let subscriptions = vec![
            // The page reads only the connection card's view of the status.
            cx.observe(&state, |this: &mut Self, state, cx| {
                let view = connection_view(state.read(cx).linear.status.as_ref());
                if this.seen.changed(view) {
                    cx.notify();
                }
            }),
            cx.subscribe(&key_input, |this: &mut Self, _, event, cx| match event {
                MaskedKeyEvent::Submitted => this.save(cx),
                MaskedKeyEvent::Edited => {
                    if this.save_error.take().is_some() {
                        cx.notify();
                    }
                }
            }),
        ];
        store::refresh_status(&state, false, cx);
        let seen = ObserveGate::seeded(connection_view(state.read(cx).linear.status.as_ref()));
        Self {
            seen,
            state,
            key_input,
            scroll: widgets::PageScroll::default(),
            saving: false,
            disconnecting: false,
            save_error: None,
            _subscriptions: subscriptions,
        }
    }

    /// Send the typed key with `LinearSetApiKey` (the field clears either way).
    pub(crate) fn save(&mut self, cx: &mut Context<Self>) {
        if self.saving || self.key_input.read(cx).is_empty() {
            return;
        }
        let key = self.key_input.update(cx, |input, cx| input.take(cx));
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .linear_set_api_key(key);
        self.saving = true;
        self.save_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.saving = false;
                match result {
                    Ok(status) => store::set_status(&this.state, Ok(status), cx),
                    Err(error) => this.save_error = Some(save_error_text(&error)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn disconnect(&mut self, cx: &mut Context<Self>) {
        if self.disconnecting {
            return;
        }
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .linear_clear_api_key();
        self.disconnecting = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.disconnecting = false;
                match result {
                    Ok(status) => store::set_status(&this.state, Ok(status), cx),
                    Err(error) => this.save_error = Some(error.user_text()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn button(
        id: &'static str,
        label: &'static str,
        primary: bool,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .debug_selector(move || id.into())
            .flex_none()
            .px(px(12.0))
            .py(px(5.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(12.0))
            .when(primary, |el| el.bg(theme.element_active))
            .text_color(theme.text)
            .hover(|el| el.bg(theme.element_hover))
            .child(label)
    }
}

impl crate::popover::ScrollRailHost for LinearSettingsPage {
    fn rail_bar(&mut self) -> &mut crate::popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

impl Render for LinearSettingsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let view = connection_view(self.state.read(cx).linear.status.as_ref());
        let configured = matches!(
            view,
            ConnectionView::Connected { .. } | ConnectionView::Unverified(_)
        );
        let (status_title, status_detail, status_color): (String, Option<String>, gpui::Hsla) =
            match &view {
                ConnectionView::Checking => ("Checking\u{2026}".into(), None, theme.text_muted),
                ConnectionView::Connected {
                    name,
                    from_environment,
                } => (
                    format!("Connected as {name}"),
                    from_environment.then(|| {
                        "Using LINEAR_API_KEY from the environment. Disconnect removes only a saved key."
                            .to_string()
                    }),
                    theme.success,
                ),
                ConnectionView::Unverified(detail) => (
                    "Key saved, not verified".into(),
                    Some(detail.clone()),
                    theme.warning,
                ),
                ConnectionView::NotConfigured => (
                    "Not configured".into(),
                    Some("Paste a personal API key to connect.".into()),
                    theme.text_muted,
                ),
                ConnectionView::NotAvailable => (
                    "Not available on this host".into(),
                    Some("This engine predates the Linear integration.".into()),
                    theme.text_muted,
                ),
                ConnectionView::Error(message) => {
                    ("Connection error".into(), Some(message.clone()), theme.warning)
                }
            };
        let status_row = widgets::card_row(&theme, true)
            .child(widgets::row_tile(&theme, crate::icons::KEY_MINIMALISTIC))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(div().size(px(7.0)).rounded_full().bg(status_color))
                            .child(
                                widgets::row_title(&theme, status_title)
                                    .debug_selector(|| "linear-settings-status".into()),
                            ),
                    )
                    .when_some(status_detail, |el, detail| {
                        el.child(
                            div()
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.text_muted)
                                .child(SharedString::from(detail)),
                        )
                    }),
            )
            .child(
                Self::button("linear-settings-recheck", "Check again", false, &theme).on_click(
                    cx.listener(|this, _, _, cx| store::refresh_status(&this.state, true, cx)),
                ),
            )
            .when(configured, |el| {
                el.child(
                    Self::button(
                        "linear-settings-disconnect",
                        if self.disconnecting {
                            "Disconnecting\u{2026}"
                        } else {
                            "Disconnect"
                        },
                        false,
                        &theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx))),
                )
            });
        let key_row = widgets::card_row(&theme, false)
            .flex_col()
            .items_start()
            .child(widgets::field_label(
                &theme,
                if configured {
                    "Replace API key"
                } else {
                    "API key"
                },
            ))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(self.key_input.clone())
                    .child(
                        Self::button("linear-settings-paste", "Paste", false, &theme).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.key_input.update(cx, |input, cx| input.paste(cx))
                            }),
                        ),
                    )
                    .child(
                        Self::button(
                            "linear-settings-save",
                            if self.saving {
                                "Checking\u{2026}"
                            } else {
                                "Save"
                            },
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
            .when_some(self.save_error.clone(), |el, error| {
                el.child(
                    div()
                        .debug_selector(|| "linear-settings-save-error".into())
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.danger)
                        .child(SharedString::from(error)),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child("Create one in Linear under Settings \u{2192} Security & access.")
                    .child(
                        div()
                            .id("linear-settings-open-keys")
                            .debug_selector(|| "linear-settings-open-keys".into())
                            .flex()
                            .items_center()
                            .gap(px(2.0))
                            .cursor_pointer()
                            .text_color(theme.accent)
                            .child("Open Linear")
                            .child(
                                crate::icons::icon(crate::icons::ARROW_UP_RIGHT)
                                    .size(px(11.0))
                                    .text_color(theme.accent),
                            )
                            .on_click(|_, _, cx| cx.open_url(super::API_KEY_SETTINGS_URL)),
                    ),
            );
        let scrollbar = crate::popover::rail(self, "linear-settings-scrollbar", &theme, cx);
        div()
            .id("linear-settings-host")
            .relative()
            .size_full()
            .child(
                div()
                    .id("linear-settings")
                    .debug_selector(|| "linear-settings".into())
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .child(
                        widgets::page_column()
                            .child(widgets::page_header(&theme, "Linear", None))
                            .child(widgets::page_subtitle(
                                &theme,
                                "Read-only. Turn Linear on per project in its Space details; nothing is created in the Outline or written to Linear automatically. The key stays in this Mac\u{2019}s Keychain.",
                            ))
                            .child(widgets::section_card(&theme).child(status_row).child(key_row)),
                    ),
            )
            .children(scrollbar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use mast_proto::linear::LinearUser;

    type Calls = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

    fn page(
        cx: &mut gpui::TestAppContext,
        reply: impl Fn(&str, &serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError>
        + Send
        + Sync
        + 'static,
    ) -> (
        Entity<LinearSettingsPage>,
        Entity<AppState>,
        Calls,
        &mut gpui::VisualTestContext,
    ) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let calls: Calls = Default::default();
        let sink = calls.clone();
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| {
            state.work_outline_actions_override =
                Some(crate::work_outline::WorkOutlineActions::scripted(Arc::new(
                    move |method, params| {
                        sink.lock()
                            .unwrap()
                            .push((method.to_string(), params.clone()));
                        reply(method, &params)
                    },
                )));
        });
        let (page, vcx) = cx.add_window_view(|_, cx| LinearSettingsPage::new(state.clone(), cx));
        vcx.run_until_parked();
        (page, state, calls, vcx)
    }

    fn connected() -> serde_json::Value {
        serde_json::to_value(LinearConnectionStatus {
            configured: true,
            source: LinearKeySource::Keychain,
            viewer: Some(LinearUser {
                id: "u".into(),
                name: "Mikey".into(),
                display_name: None,
                email: None,
            }),
            error: None,
        })
        .unwrap()
    }

    fn not_configured() -> serde_json::Value {
        serde_json::json!({"configured": false, "source": "none"})
    }

    #[gpui::test]
    fn save_validates_then_shows_connected_and_disconnect_clears(cx: &mut gpui::TestAppContext) {
        let (page, state, calls, vcx) = page(cx, |method, params| match method {
            "LinearStatus" => Ok(not_configured()),
            "LinearSetApiKey" if params["apiKey"] == "lin_good" => Ok(connected()),
            "LinearSetApiKey" => Err(mast_rpc::RpcError::Failed(
                "linear/unauthorized: bad key".into(),
            )),
            "LinearClearApiKey" => Ok(not_configured()),
            _ => Ok(serde_json::json!({})),
        });
        state.read_with(vcx, |state, _| {
            assert_eq!(
                connection_view(state.linear.status.as_ref()),
                ConnectionView::NotConfigured
            )
        });
        // A rejected key: validation error, nothing connected, field cleared.
        page.update(vcx, |page, cx| {
            page.key_input
                .update(cx, |input, cx| input.insert("lin_bad", cx));
            page.save(cx);
        });
        vcx.run_until_parked();
        page.read_with(vcx, |page, cx| {
            assert_eq!(
                page.save_error.as_deref(),
                Some("Linear rejected this key. Check it and try again.")
            );
            assert!(page.key_input.read(cx).is_empty());
        });
        // A good key: connected as the viewer.
        page.update(vcx, |page, cx| {
            page.key_input
                .update(cx, |input, cx| input.insert("lin_good", cx));
            page.save(cx);
        });
        vcx.run_until_parked();
        state.read_with(vcx, |state, _| {
            assert_eq!(
                connection_view(state.linear.status.as_ref()),
                ConnectionView::Connected {
                    name: "Mikey".into(),
                    from_environment: false
                }
            )
        });
        page.read_with(vcx, |page, _| assert!(page.save_error.is_none()));
        // The key is sent once, only to LinearSetApiKey.
        let sent: Vec<String> = calls.lock().unwrap().iter().map(|c| c.0.clone()).collect();
        assert_eq!(sent, ["LinearStatus", "LinearSetApiKey", "LinearSetApiKey"]);
        // Disconnect.
        page.update(vcx, |page, cx| page.disconnect(cx));
        vcx.run_until_parked();
        state.read_with(vcx, |state, _| {
            assert_eq!(
                connection_view(state.linear.status.as_ref()),
                ConnectionView::NotConfigured
            )
        });
        assert_eq!(calls.lock().unwrap().last().unwrap().0, "LinearClearApiKey");
    }

    #[gpui::test]
    fn unknown_method_reads_as_not_available(cx: &mut gpui::TestAppContext) {
        let (_page, state, _calls, vcx) = page(cx, |method, _| {
            Err(mast_rpc::RpcError::UnknownMethod(method.to_string()))
        });
        state.read_with(vcx, |state, _| {
            assert_eq!(
                connection_view(state.linear.status.as_ref()),
                ConnectionView::NotAvailable
            )
        });
    }

    #[test]
    fn status_errors_and_rate_limits_read_plainly() {
        let rate = LinearUiError::new(
            super::super::LinearFailure::Linear(LinearErrorCode::RateLimited),
            "slow down",
        );
        assert_eq!(
            connection_view(Some(&Err(rate))),
            ConnectionView::Error(
                "Linear is rate-limiting requests. Wait a moment, then retry.".into()
            )
        );
        let unverified = LinearConnectionStatus {
            configured: true,
            source: LinearKeySource::Environment,
            viewer: None,
            error: Some("network down".into()),
        };
        assert_eq!(
            connection_view(Some(&Ok(unverified))),
            ConnectionView::Unverified("network down".into())
        );
    }
}
