//! A local file path linked from a chat that lies outside the chat's project.
//! The workspace file viewer refuses such paths, so the owning device offers
//! to import the file as an artifact (`ArtifactRegister`), and a remote
//! viewer only gets a message (the file is never read for a peer).
use super::*;

/// What the inline "outside the project" strip shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutsideFileState {
    /// Owning device: offer "Open as artifact".
    Offer,
    /// `ArtifactRegister` is in flight.
    Registering,
    /// The engine refused (deny-list or otherwise); the reason is shown.
    Refused(String),
    /// A remote viewer: message only, nothing is read.
    Remote { device: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutsideFilePrompt {
    pub(crate) chat_id: String,
    pub(crate) path: String,
    pub(crate) state: OutsideFileState,
}

impl OutsideFilePrompt {
    pub(crate) fn file_name(&self) -> &str {
        self.path
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.path)
    }

    pub(crate) fn message(&self) -> String {
        match &self.state {
            OutsideFileState::Offer | OutsideFileState::Registering => {
                "This file is outside the project".to_owned()
            }
            OutsideFileState::Refused(reason) => reason.clone(),
            OutsideFileState::Remote { device } => {
                format!("This file is outside the project on {device}")
            }
        }
    }
}

impl Shell {
    /// Handle a click on a local file path outside the active chat's project.
    /// `false` when `target` is not such a path.
    pub(super) fn show_outside_file_prompt(
        &mut self,
        target: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((root, local)) = self.active_checkout(cx) else {
            return false;
        };
        let Some(path) = crate::workspace_links::resolve_outside_file_link(target, &root) else {
            return false;
        };
        let state = if local {
            OutsideFileState::Offer
        } else {
            let state = self.state.read(cx);
            let device = state
                .chats
                .iter()
                .find(|chat| chat.id == self.active_chat)
                .map(|chat| {
                    state
                        .device_name(&chat.device_id)
                        .unwrap_or("another device")
                        .to_owned()
                })
                .unwrap_or_else(|| "another device".to_owned());
            OutsideFileState::Remote { device }
        };
        self.outside_file_prompt = Some(OutsideFilePrompt {
            chat_id: self.active_chat.clone(),
            path,
            state,
        });
        cx.notify();
        true
    }

    pub(super) fn dismiss_outside_file_prompt(&mut self, cx: &mut Context<Self>) {
        self.outside_file_prompt = None;
        cx.notify();
    }

    /// "Open as artifact": register the file into the chat's scope (title =
    /// file name), then open the Artifact tab. A refusal replaces the choice
    /// with its reason.
    pub(crate) fn confirm_outside_file_prompt(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(prompt) = self.outside_file_prompt.clone() else {
            return;
        };
        if prompt.state != OutsideFileState::Offer || prompt.chat_id != self.active_chat {
            return;
        }
        let Some(mut params) = self.register_params_for_active_chat(&prompt.path, cx) else {
            self.outside_file_prompt = Some(OutsideFilePrompt {
                state: OutsideFileState::Refused("This chat is not in a project".into()),
                ..prompt
            });
            cx.notify();
            return;
        };
        params.title = Some(prompt.file_name().to_owned());
        self.outside_file_prompt = Some(OutsideFilePrompt {
            state: OutsideFileState::Registering,
            ..prompt.clone()
        });
        let call = self.state.read(cx).artifact_actions().register(params);
        cx.spawn_in(window, async move |this, cx| {
            let reply = call.await;
            this.update_in(cx, |this, window, cx| {
                // The prompt may have been dismissed or replaced meanwhile.
                let current = this.outside_file_prompt.as_ref().is_some_and(|current| {
                    current.path == prompt.path && current.chat_id == prompt.chat_id
                });
                match reply {
                    Ok(reply) => {
                        if current {
                            this.outside_file_prompt = None;
                        }
                        this.open_artifact(reply.artifact_id, window, cx);
                    }
                    Err(error) if current => {
                        this.outside_file_prompt = Some(OutsideFilePrompt {
                            state: OutsideFileState::Refused(error.text()),
                            ..prompt
                        });
                    }
                    Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The inline strip above the composer for the active chat's prompt.
    pub(super) fn render_outside_file_prompt(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let prompt = self
            .outside_file_prompt
            .as_ref()
            .filter(|prompt| prompt.chat_id == self.active_chat)?
            .clone();
        let refused = matches!(prompt.state, OutsideFileState::Refused(_));
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .flex_none()
                .px(px(Theme::SPACE_SM))
                .py(px(2.0))
                .rounded(px(Theme::CONTROL_RADIUS))
                .border_1()
                .border_color(theme.border_strong)
                .text_color(theme.text)
                .cursor_pointer()
                .hover(|style| style.bg(theme.surface_raised_hover))
                .child(label)
        };
        Some(
            div()
                .id("outside-file-prompt")
                .flex_none()
                .w_full()
                .max_w(px(768.0))
                .mx_auto()
                .px(px(Theme::SPACE_LG + 8.0))
                .pb(px(Theme::SPACE_SM))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(Theme::SPACE_SM))
                        .px(px(Theme::SPACE_SM))
                        .py(px(4.0))
                        .rounded(px(Theme::CONTROL_RADIUS))
                        .bg(theme.surface_raised)
                        .border_1()
                        .border_color(if refused { theme.danger } else { theme.border })
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(if refused {
                            theme.danger
                        } else {
                            theme.text_muted
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .child(prompt.message()),
                        )
                        .when(prompt.state == OutsideFileState::Offer, |row| {
                            row.child(button("outside-file-open", "Open as artifact").on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.confirm_outside_file_prompt(window, cx)
                                }),
                            ))
                        })
                        .child(button("outside-file-dismiss", "Dismiss").on_click(
                            cx.listener(|this, _, _, cx| this.dismiss_outside_file_prompt(cx)),
                        )),
                )
                .into_any_element(),
        )
    }
}
