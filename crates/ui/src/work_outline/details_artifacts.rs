//! The **Artifacts** section of Work Item Details: the item's planning
//! artifacts (from all its chats), the pinned plan on top with the item's
//! Linear project next to it, then the rest newest first. Each row shows the
//! kind icon, title, role, revision and age; a click opens the Artifact tab;
//! a right-click opens Pin / Promote / Reveal / Copy link. "Register existing
//! file…" imports a file of the Space (`ArtifactRegister`).
//!
//! A child module of `details_panel`, so it uses the surface's private state.
use gpui::{
    AnyElement, ClipboardItem, Context, Entity, Focusable, SharedString, Window, div, prelude::*,
    px,
};
use mast_proto::{
    ArtifactPinParams, ArtifactPromoteParams, ArtifactRegisterParams, WorkItemView,
    WorkOutlineSnapshot,
};

use super::WorkItemDetails;
use crate::artifacts::model;
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::theme::Theme;

/// Per-surface Artifacts section state.
#[derive(Default)]
pub(crate) struct ArtifactsDetailsState {
    /// `None` = open (the default).
    pub(crate) collapsed: bool,
    /// The row whose action menu is open.
    pub(crate) menu: Option<String>,
    /// A status line (promoted path, errors).
    pub(crate) status: Option<String>,
    /// The path field of "Register existing file…" (remote Spaces, and the
    /// fallback when no native picker is available).
    pub(crate) register_input: Option<Entity<ComposerInput>>,
    pub(crate) busy: bool,
    _register_sub: Option<gpui::Subscription>,
}

/// Why a picked path cannot be registered for this Space. Pure.
pub(crate) fn register_path_refusal(space_path: &str, path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() {
        return Some("Enter a file path inside the project".into());
    }
    if !path.starts_with('/') {
        // Relative paths are relative to the Space folder.
        return None;
    }
    let root = space_path.trim_end_matches('/');
    let inside = path == root || path.starts_with(&format!("{root}/"));
    (!inside).then(|| format!("Pick a file inside {root}"))
}

impl WorkItemDetails {
    fn artifact_params(&self, artifact_id: &str, cx: &gpui::App) -> Option<(bool, String)> {
        let state = self.state.read(cx);
        let artifact = state.artifacts().artifact(artifact_id)?;
        let local = state.local_device_id.as_deref() == Some(artifact.device_id.as_str());
        Some((local, artifact.device_id.clone()))
    }

    pub(super) fn open_artifact(
        &mut self,
        artifact_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.artifacts.menu = None;
        let opened = self
            .shell
            .update(cx, |shell, cx| {
                shell.open_artifact(artifact_id.clone(), window, cx)
            })
            .is_ok();
        #[cfg(test)]
        if !opened {
            self.captured_artifact_open = Some(artifact_id);
        }
        let _ = opened;
    }

    /// The item's canonical (pinned) plan, if it has one.
    pub(super) fn pinned_plan_id(&self, item_id: &str, cx: &gpui::App) -> Option<String> {
        self.state
            .read(cx)
            .artifacts()
            .for_work_item(&self.space_id, item_id)
            .into_iter()
            .find(|artifact| artifact.pinned_as_plan)
            .map(|artifact| artifact.id.clone())
    }

    pub(crate) fn pin_artifact(
        &mut self,
        artifact_id: String,
        pinned: bool,
        cx: &mut Context<Self>,
    ) {
        self.artifacts.menu = None;
        let call = self
            .state
            .read(cx)
            .artifact_actions()
            .pin(ArtifactPinParams {
                artifact_id,
                pinned,
            });
        self.artifacts.busy = true;
        cx.spawn(async move |this, cx| {
            let reply = call.await;
            this.update(cx, |this, cx| {
                this.artifacts.busy = false;
                this.artifacts.status = reply.err().map(|error| error.text());
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn promote_artifact(&mut self, artifact_id: String, cx: &mut Context<Self>) {
        self.artifacts.menu = None;
        let target_device_id = self
            .artifact_params(&artifact_id, cx)
            .and_then(|(local, device)| (!local).then_some(device));
        let call = self
            .state
            .read(cx)
            .artifact_actions()
            .promote(ArtifactPromoteParams {
                artifact_id,
                target_device_id,
            });
        self.artifacts.busy = true;
        cx.spawn(async move |this, cx| {
            let reply = call.await;
            this.update(cx, |this, cx| {
                this.artifacts.busy = false;
                this.artifacts.status = Some(match reply {
                    Ok(reply) => format!("Copied to {} (uncommitted)", reply.promoted_path),
                    Err(error) => error.text(),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn reveal_artifact(&mut self, artifact_id: String, cx: &mut Context<Self>) {
        self.artifacts.menu = None;
        let path = self
            .state
            .read(cx)
            .artifacts()
            .artifact(&artifact_id)
            .map(|artifact| artifact.stored_path.clone());
        if let (Some(path), Some((true, _))) = (path, self.artifact_params(&artifact_id, cx)) {
            cx.reveal_path(std::path::Path::new(&path));
        }
        cx.notify();
    }

    fn copy_artifact_link(&mut self, artifact_id: String, cx: &mut Context<Self>) {
        self.artifacts.menu = None;
        let url = format!(
            "{}{artifact_id}",
            mast_proto::artifacts::ARTIFACT_URL_SCHEME
        );
        cx.write_to_clipboard(ClipboardItem::new_string(url));
        self.artifacts.status = Some("Link copied".into());
        cx.notify();
    }

    /// "Register existing file…": a native file picker for a Space on this
    /// device, else a path field (the path is resolved on the Space's
    /// device).
    fn begin_register(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(space) = self.state.read(cx).space_row(&self.space_id).cloned() else {
            return;
        };
        let local =
            self.state.read(cx).local_device_id.as_deref() == Some(space.device_id.as_str());
        if local && !cfg!(test) {
            let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: Some("Register as artifact".into()),
            });
            cx.spawn(async move |this, cx| {
                let path = match receiver.await {
                    Ok(Ok(Some(mut paths))) => paths.pop(),
                    _ => None,
                };
                if let Some(path) = path {
                    this.update(cx, |this, cx| {
                        this.register_existing(path.display().to_string(), cx)
                    })
                    .ok();
                }
            })
            .detach();
            return;
        }
        if self.artifacts.register_input.is_none() {
            let input = cx.new(|cx| {
                ComposerInput::with_context(
                    "Path inside the project (e.g. work/plan.md)",
                    crate::composer::PALETTE_SEARCH_CONTEXT,
                    cx,
                )
                .with_single_line()
                .with_text_metrics(12.0, 18.0)
            });
            let sub = cx.subscribe(&input, |this: &mut Self, input, event, cx| {
                if matches!(event, ComposerInputEvent::Submitted) {
                    let path = input.read(cx).text().to_owned();
                    this.register_existing(path, cx);
                }
            });
            self.artifacts.register_input = Some(input);
            self.artifacts._register_sub = Some(sub);
        }
        if let Some(input) = &self.artifacts.register_input {
            window.focus(&input.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// `ArtifactRegister {spaceId, workItemId, path}` (forwarded to the
    /// Space's device), then open the result.
    pub(crate) fn register_existing(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(space) = self.state.read(cx).space_row(&self.space_id).cloned() else {
            return;
        };
        if let Some(refusal) = register_path_refusal(&space.path, &path) {
            self.artifacts.status = Some(refusal);
            cx.notify();
            return;
        }
        let local =
            self.state.read(cx).local_device_id.as_deref() == Some(space.device_id.as_str());
        let params = ArtifactRegisterParams {
            space_id: self.space_id.clone(),
            work_item_id: (!self.is_space_root()).then(|| self.item_id.clone()),
            chat_id: None,
            path: path.trim().to_owned(),
            title: None,
            kind: None,
            role: None,
            target_device_id: (!local).then(|| space.device_id.clone()),
        };
        let call = self.state.read(cx).artifact_actions().register(params);
        self.artifacts.busy = true;
        cx.spawn(async move |this, cx| {
            let reply = call.await;
            this.update(cx, |this, cx| {
                this.artifacts.busy = false;
                match reply {
                    Ok(reply) => {
                        this.artifacts.register_input = None;
                        this.artifacts._register_sub = None;
                        this.artifacts.status = Some(if reply.changed {
                            format!("Registered as r{}", reply.revision)
                        } else {
                            "Already registered".to_owned()
                        });
                    }
                    Err(error) => this.artifacts.status = Some(error.text()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub(super) fn render_artifacts_section(
        &self,
        item: &WorkItemView,
        snapshot: &WorkOutlineSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = self.state.read(cx);
        let load = state.artifacts().load(&self.space_id);
        let rows: Vec<model::ArtifactRow> = {
            let now = model::now_ms();
            let viewed = crate::settings::current(cx).artifact_last_viewed;
            state
                .artifacts()
                .for_work_item(&self.space_id, &item.id)
                .into_iter()
                .map(|artifact| model::row(artifact, now, &viewed))
                .collect()
        };
        let local_device = state.local_device_id.clone();
        let devices: std::collections::HashMap<String, String> = state
            .artifacts()
            .for_work_item(&self.space_id, &item.id)
            .into_iter()
            .map(|artifact| (artifact.id.clone(), artifact.device_id.clone()))
            .collect();
        let open = !self.artifacts.collapsed;
        let header = Self::section_header(
            "work-item-artifacts-header",
            if rows.is_empty() {
                "Artifacts".to_owned()
            } else {
                format!("Artifacts ({})", rows.len())
            },
            open,
            theme,
        )
        .debug_selector(|| "work-item-artifacts-header".into())
        .on_click(cx.listener(|this, _, _, cx| {
            this.artifacts.collapsed = !this.artifacts.collapsed;
            cx.notify();
        }));
        let mut section = div()
            .debug_selector(|| "work-item-artifacts".into())
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(header);
        if !open {
            return section.into_any_element();
        }
        if matches!(load, crate::artifacts::ArtifactsLoad::Unsupported) {
            return section
                .child(
                    div()
                        .debug_selector(|| "work-item-artifacts-unsupported".into())
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_faint)
                        .child(crate::artifacts::store::UNSUPPORTED_TEXT),
                )
                .into_any_element();
        }
        let linear = snapshot.linear_project_reference(&item.id).cloned();
        for row in rows {
            let id = row.id.clone();
            let selector = format!("work-item-artifact-{id}");
            let open_id = id.clone();
            let menu_id = id.clone();
            let menu_open = self.artifacts.menu.as_deref() == Some(id.as_str());
            let detail = [
                row.role.clone(),
                Some(row.revision.clone()),
                Some(row.age.clone()),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" \u{b7} ");
            let linear_chip = row
                .pinned
                .then(|| linear.as_ref())
                .flatten()
                .map(|reference| {
                    crate::work_outline::outline_view::linear_outline::project_chip(
                        "work-item-artifact-plan-linear".into(),
                        reference
                            .cached_title
                            .clone()
                            .unwrap_or_else(|| "Linear project".into()),
                        reference.url.clone(),
                        Some(self.shell.clone()),
                        theme,
                    )
                });
            let row_el = div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .flex()
                .items_center()
                .gap(px(6.0))
                .px(px(4.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .hover(|el| el.bg(theme.element_hover))
                .when(menu_open, |el| el.bg(theme.element_hover))
                .child(
                    crate::icons::icon(row.icon)
                        .size(px(12.0))
                        .flex_none()
                        .text_color(if row.pinned {
                            theme.accent
                        } else {
                            theme.text_muted
                        }),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text)
                        .child(SharedString::from(row.title.clone())),
                )
                .children(row.changed.map(|(from, to)| {
                    crate::artifacts::questions_ui::changed_dot(
                        format!("work-item-artifact-changed-{id}"),
                        from,
                        to,
                        theme,
                    )
                }))
                .when(row.pinned, |el| {
                    el.child(
                        div()
                            .flex_none()
                            .px(px(4.0))
                            .rounded(px(3.0))
                            .border_1()
                            .border_color(theme.border)
                            .text_size(crate::typography::ui_rems(9.5))
                            .text_color(theme.accent)
                            .child("Plan"),
                    )
                })
                .children(linear_chip)
                .child(
                    div()
                        .flex_none()
                        .text_size(crate::typography::ui_rems(10.5))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(detail)),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_artifact(open_id.clone(), window, cx)
                }))
                .on_mouse_down(
                    gpui::MouseButton::Right,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.artifacts.menu =
                            if this.artifacts.menu.as_deref() == Some(menu_id.as_str()) {
                                None
                            } else {
                                Some(menu_id.clone())
                            };
                        cx.notify();
                    }),
                );
            section = section.child(row_el);
            if row.pinned
                && let Some(reference) = linear.as_ref()
                && crate::linear::space_enabled(Some(snapshot))
            {
                // The canonical plan's Linear project, read-only.
                crate::linear::store::request_project_detail(
                    &self.state,
                    &reference.external_id,
                    false,
                    cx,
                );
                let entry = self
                    .state
                    .read(cx)
                    .linear
                    .project_detail(&reference.external_id)
                    .cloned();
                section = section.child(crate::artifacts::plan_linear::render(
                    "work-item-plan-linear",
                    entry.as_ref(),
                    (
                        reference
                            .cached_title
                            .as_deref()
                            .unwrap_or("Linear project"),
                        &reference.url,
                    ),
                    crate::artifacts::model::now_ms(),
                    Some(self.shell.clone()),
                    theme,
                ));
            }
            if menu_open {
                let local = devices
                    .get(&id)
                    .is_some_and(|device| local_device.as_deref() == Some(device.as_str()));
                section =
                    section.child(self.render_artifact_menu(&id, row.pinned, local, theme, cx));
            }
        }
        let register_input = self.artifacts.register_input.clone();
        section = section.child(
            div().flex().items_center().gap(px(6.0)).child(
                Self::small_button(
                    "work-item-artifacts-register",
                    "Register existing file…",
                    theme,
                )
                .debug_selector(|| "work-item-artifacts-register".into())
                .on_click(cx.listener(|this, _, window, cx| this.begin_register(window, cx))),
            ),
        );
        if let Some(input) = register_input {
            section = section.child(
                div()
                    .debug_selector(|| "work-item-artifacts-register-input".into())
                    .child(Self::input_frame(&input, theme)),
            );
        }
        if let Some(status) = self.artifacts.status.clone() {
            section = section.child(
                div()
                    .debug_selector(|| "work-item-artifacts-status".into())
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(status)),
            );
        }
        section.into_any_element()
    }

    fn render_artifact_menu(
        &self,
        artifact_id: &str,
        pinned: bool,
        local: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entry = |id: &str, label: &'static str| {
            let selector = format!("work-item-artifact-menu-{id}");
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .px(px(6.0))
                .py(px(2.0))
                .rounded(px(3.0))
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text)
                .hover(|el| el.bg(theme.element_hover))
                .child(label)
        };
        let (pin_id, promote_id, reveal_id, copy_id) = (
            artifact_id.to_owned(),
            artifact_id.to_owned(),
            artifact_id.to_owned(),
            artifact_id.to_owned(),
        );
        div()
            .ml(px(18.0))
            .p(px(3.0))
            .flex()
            .flex_col()
            .rounded(px(5.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_raised)
            .child(
                entry("pin", if pinned { "Unpin plan" } else { "Pin as plan" }).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.pin_artifact(pin_id.clone(), !pinned, cx)
                    }),
                ),
            )
            .child(entry("promote", "Promote to docs/plans/").on_click(
                cx.listener(move |this, _, _, cx| this.promote_artifact(promote_id.clone(), cx)),
            ))
            .when(local, |el| {
                el.child(entry("reveal", "Reveal in Finder").on_click(
                    cx.listener(move |this, _, _, cx| this.reveal_artifact(reveal_id.clone(), cx)),
                ))
            })
            .child(entry("copy", "Copy link").on_click(
                cx.listener(move |this, _, _, cx| this.copy_artifact_link(copy_id.clone(), cx)),
            ))
            .into_any_element()
    }
}
