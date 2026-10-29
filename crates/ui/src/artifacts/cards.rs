//! Transcript artifact cards: each `[label](mast-artifact:…)` link of a
//! settled assistant message gets a card (kind icon, title, role, revision,
//! Open) resolved against the watched snapshots at render time. Activation
//! goes through the chat's link handler, so it opens the Artifact tab (and
//! registers an unknown path) exactly like clicking the inline link.
use gpui::{AnyElement, SharedString, div, prelude::*, px};
use mast_proto::artifacts::ARTIFACT_URL_SCHEME;

use super::model;
use super::store::ArtifactStore;
use crate::markdown::render;
use crate::theme::Theme;

/// What a card shows. Pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CardModel {
    pub(crate) title: String,
    pub(crate) icon: &'static str,
    /// "Plan · plan · r3", or the path for a not-yet-registered file.
    pub(crate) detail: String,
    /// The link the card activates (`mast-artifact:<target>`).
    pub(crate) url: String,
    pub(crate) registered: bool,
}

pub(crate) fn card_model(
    label: &str,
    target: &str,
    store: &ArtifactStore,
    cwd: Option<&str>,
) -> CardModel {
    let url = format!("{ARTIFACT_URL_SCHEME}{target}");
    let artifact = if mast_proto::artifacts::is_artifact_id(target) {
        store.artifact(target)
    } else {
        store.artifact_by_path(target, cwd, None)
    };
    match artifact {
        Some(artifact) => {
            let detail = [
                Some(model::kind_label(artifact.kind).to_owned()),
                artifact.role.clone().filter(|role| !role.trim().is_empty()),
                Some(format!("r{}", artifact.current_revision)),
                artifact.pinned_as_plan.then(|| "the plan".to_owned()),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" \u{b7} ");
            CardModel {
                title: artifact.title.clone(),
                icon: model::kind_icon(artifact.kind),
                detail,
                url: format!("{ARTIFACT_URL_SCHEME}{}", artifact.id),
                registered: true,
            }
        }
        None => {
            let title = if label.trim().is_empty() {
                target.rsplit('/').next().unwrap_or(target).to_owned()
            } else {
                label.trim().to_owned()
            };
            let detail = if mast_proto::artifacts::is_artifact_id(target) {
                "Artifact".to_owned()
            } else {
                target.to_owned()
            };
            CardModel {
                title,
                icon: model::kind_icon(mast_proto::ArtifactKind::from_path(target)),
                detail,
                url,
                registered: false,
            }
        }
    }
}

pub(crate) fn artifact_card(
    row_id: &str,
    card: CardModel,
    link_ui: Option<render::LinkUi>,
    theme: &Theme,
) -> AnyElement {
    let selector = format!("{row_id}#artifact");
    let title: SharedString = card.title.clone().into();
    let open = {
        let (url, title, ui) = (card.url.clone(), card.title.clone(), link_ui.clone());
        move |window: &mut gpui::Window, cx: &mut gpui::App| {
            render::activate_link(
                render::LinkTarget::new(&title, &url),
                render::LinkAction::Primary,
                ui.as_ref(),
                window,
                cx,
            )
        }
    };
    let open_button = open.clone();
    div()
        .py(px(4.0))
        .w_full()
        .child(
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(10.0))
                .rounded(px(10.0))
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface_raised)
                .px(px(10.0))
                .py(px(9.0))
                .cursor_pointer()
                .role(gpui::Role::Button)
                .aria_label(format!("Open artifact {title}"))
                .hover(|card| card.bg(theme.element_hover))
                .child(
                    div()
                        .flex_none()
                        .size(px(28.0))
                        .rounded(px(7.0))
                        .bg(theme.accent.opacity(0.10))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            crate::icons::icon(card.icon)
                                .size(px(15.0))
                                .text_color(theme.accent),
                        ),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .truncate()
                                .text_size(crate::typography::ui_rems(13.0))
                                .text_color(theme.text)
                                .child(title.clone()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(theme.text_muted)
                                .child(SharedString::from(card.detail.clone())),
                        ),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("{row_id}#artifact-open")))
                        .flex_none()
                        .px(px(8.0))
                        .py(px(3.0))
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(theme.border)
                        .text_size(crate::typography::ui_rems(11.5))
                        .text_color(theme.text)
                        .hover(|el| el.bg(theme.element_hover))
                        .child("Open")
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            open_button(window, cx)
                        }),
                )
                .on_click(move |_, window, cx| open(window, cx)),
        )
        .into_any_element()
}
