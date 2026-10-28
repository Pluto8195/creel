//! Side-by-side options and picking (Artifacts phase 5 F).
//!
//! An `options` artifact opens 2-4 of its fragments in equal columns of one
//! web document (scroll and zoom synced across frames, like compare). Above
//! the columns the tab draws a native "Pick this" button per column: picking
//! records the choice on the artifact (`ArtifactPickOption`, never a file
//! edit), messages the publishing chat like a question answer, and the
//! engine resolves the related Required Action. A pick can be changed or
//! withdrawn. The note field is optional and travels with the pick.
use std::collections::HashMap;

use gpui::{
    AnyElement, Context, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px,
};
use zeron_proto::{
    ArtifactKind, ArtifactOptionPick, ArtifactPickOptionParams, ArtifactView, pick_message,
};

use super::*;
use crate::artifacts::{compare, model, questions_ui};
use crate::composer::ComposerInput;

/// Most options shown at once.
pub(crate) const MAX_COLUMNS: usize = 4;

#[derive(Default)]
pub(crate) struct SideBySide {
    /// Indexes into the manifest's options, in the order shown.
    pub(crate) chosen: Vec<usize>,
    /// Fragment source by option path, once loaded.
    sources: HashMap<String, String>,
    loading: bool,
    error: Option<String>,
    generation: u64,
    note: Option<Entity<ComposerInput>>,
    busy: bool,
    /// A pick made from this tab, shown until the snapshot differs from
    /// what it was when we recorded it: `(value, snapshot's value then)`.
    local: Option<(Option<ArtifactOptionPick>, Option<ArtifactOptionPick>)>,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ArtifactTab {
    fn manifest_options(&self) -> Vec<zeron_proto::artifacts::OptionsManifestEntry> {
        match &self.body {
            Some(ArtifactBody::Options(manifest)) => manifest.options.clone(),
            _ => Vec::new(),
        }
    }

    fn option_label(option: &zeron_proto::artifacts::OptionsManifestEntry) -> String {
        if option.label.is_empty() {
            option.path.clone()
        } else {
            option.label.clone()
        }
    }

    /// The pick on screen: the one recorded here, else the snapshot's.
    pub(crate) fn picked_now(&self, artifact: &ArtifactView) -> Option<ArtifactOptionPick> {
        match &self.side.local {
            Some((value, base)) if artifact.picked == *base => value.clone(),
            _ => artifact.picked.clone(),
        }
    }

    pub(crate) fn side_chosen(&self) -> Vec<usize> {
        self.side.chosen.clone()
    }

    pub(crate) fn side_by_side_shown(&self) -> bool {
        self.mode == Mode::SideBySide
    }

    /// Whether the side-by-side body is the shared web view right now.
    pub(super) fn side_shows_web(&self) -> bool {
        self.mode == Mode::SideBySide
            && !self.side.loading
            && self.side.error.is_none()
            && !self.side.chosen.is_empty()
    }

    // ------------------------------------------------------------------
    // Open / close / choose
    // ------------------------------------------------------------------

    pub(crate) fn open_side_by_side(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        let count = self.manifest_options().len();
        if count < 2 {
            self.notice = Some("There are no two options to show side by side.".to_owned());
            cx.notify();
            return;
        }
        self.mode = Mode::SideBySide;
        self.note_input(cx);
        if self.side.chosen.len() < 2 || self.side.chosen.iter().any(|ix| *ix >= count) {
            self.side.chosen = (0..count.min(MAX_COLUMNS)).collect();
        }
        self.reload_side(window, cx);
    }

    pub(crate) fn close_side_by_side(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == Mode::SideBySide {
            self.mode = Mode::Rendered;
        }
        self.side.generation += 1;
        self.side.loading = false;
        cx.notify();
    }

    /// Add or remove an option from the columns (2-4 stay chosen).
    pub(crate) fn toggle_side_option(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index >= self.manifest_options().len() {
            return;
        }
        let chosen = &mut self.side.chosen;
        if let Some(at) = chosen.iter().position(|ix| *ix == index) {
            if chosen.len() <= 2 {
                return;
            }
            chosen.remove(at);
        } else if chosen.len() < MAX_COLUMNS {
            chosen.push(index);
            chosen.sort_unstable();
        } else {
            return;
        }
        self.reload_side(window, cx);
    }

    /// Fetch the fragments not loaded yet, then build the document.
    fn reload_side(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let options = self.manifest_options();
        let missing: Vec<String> = self
            .side
            .chosen
            .iter()
            .filter_map(|ix| options.get(*ix))
            .map(|option| option.path.clone())
            .filter(|path| !self.side.sources.contains_key(path))
            .collect();
        self.side.error = None;
        self.side.generation += 1;
        let generation = self.side.generation;
        if missing.is_empty() {
            self.side.loading = false;
            self.show_side_document(window, cx);
            return;
        }
        self.side.loading = true;
        let calls: Vec<_> = missing
            .iter()
            .map(|path| {
                let params = self.content_params(None, Some(path.clone()), cx);
                (
                    path.clone(),
                    self.state.read(cx).artifact_actions().content(params),
                )
            })
            .collect();
        let task = cx.spawn_in(window, async move |this, cx| {
            let mut loaded = Vec::new();
            let mut failed = None;
            for (path, call) in calls {
                match call.await {
                    Ok(reply) => {
                        loaded.push((path, model::source_text(&reply).unwrap_or_default()))
                    }
                    Err(error) => {
                        failed = Some(error.text());
                        break;
                    }
                }
            }
            this.update_in(cx, |this, window, cx| {
                if this.side.generation != generation {
                    return;
                }
                this.side.loading = false;
                match failed {
                    Some(error) => this.side.error = Some(error),
                    None => {
                        this.side.sources.extend(loaded);
                        this.show_side_document(window, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        });
        self.side.tasks.push(task);
        cx.notify();
    }

    fn show_side_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let options = self.manifest_options();
        let key = render_key(cx);
        let title = self.document_title(cx);
        let frames: Vec<(crate::artifacts::document::CompareFrame, String)> = self
            .side
            .chosen
            .iter()
            .filter_map(|ix| options.get(*ix))
            .map(|option| {
                let source = self
                    .side
                    .sources
                    .get(&option.path)
                    .cloned()
                    .unwrap_or_default();
                let frame = document::compare_frame(
                    WebKind::Fragment,
                    &source,
                    &title,
                    "text/html",
                    &key.0,
                    key.1,
                    compare::SYNC_FRAME_SCRIPT,
                    "",
                );
                (
                    frame,
                    format!("{} \u{b7} {}", option.id, Self::option_label(option)),
                )
            })
            .collect();
        let panes: Vec<(&crate::artifacts::document::CompareFrame, &str)> = frames
            .iter()
            .map(|(frame, label)| (frame, label.as_str()))
            .collect();
        let html = compare::options_document(&panes, &key.0);
        self.rendered_key = Some(key);
        self.zoom_synced = false;
        if self.web.is_none() {
            let factory = self.web_factory.clone();
            self.web = Some(factory(cx.weak_entity(), window, cx));
        }
        if let Some(web) = &mut self.web {
            web.show(&title, html, window, cx);
        }
    }

    /// The theme changed while the columns are up: rebuild their document.
    pub(super) fn refresh_side_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.side_shows_web() {
            self.show_side_document(window, cx);
        }
    }

    // ------------------------------------------------------------------
    // Picking
    // ------------------------------------------------------------------

    fn note_input(&mut self, cx: &mut Context<Self>) -> Entity<ComposerInput> {
        if let Some(input) = &self.side.note {
            return input.clone();
        }
        let input = cx.new(|cx| {
            ComposerInput::new("Optional note for the agent", cx)
                .with_text_metrics(12.0, 18.0)
                .with_auto_grow()
        });
        self.side.note = Some(input.clone());
        input
    }

    fn typed_note(&self, cx: &gpui::App) -> Option<String> {
        self.side
            .note
            .as_ref()
            .map(|input| input.read(cx).text().trim().to_owned())
            .filter(|note| !note.is_empty())
    }

    #[cfg(test)]
    pub(crate) fn set_pick_note_for_tests(&mut self, text: &str, cx: &mut Context<Self>) {
        let input = self.note_input(cx);
        input.update(cx, |input, cx| input.set_text(text, cx));
    }

    /// Pick option `index` of the manifest (replacing an earlier pick), or
    /// withdraw the pick when it is already the picked one.
    pub(crate) fn pick_option(&mut self, index: usize, cx: &mut Context<Self>) {
        let options = self.manifest_options();
        let Some(option) = options.get(index).cloned() else {
            return;
        };
        let Some(artifact) = self.artifact(cx) else {
            return;
        };
        let withdraw = self
            .picked_now(&artifact)
            .is_some_and(|pick| pick.option_id == option.id);
        self.send_pick(
            artifact,
            (!withdraw).then(|| (option.id.clone(), Self::option_label(&option))),
            cx,
        );
    }

    /// Withdraw whatever is picked.
    pub(crate) fn clear_pick(&mut self, cx: &mut Context<Self>) {
        if let Some(artifact) = self.artifact(cx) {
            self.send_pick(artifact, None, cx);
        }
    }

    fn send_pick(
        &mut self,
        artifact: ArtifactView,
        pick: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        if self.side.busy || artifact.kind != ArtifactKind::Options {
            return;
        }
        let previous = self.picked_now(&artifact);
        let note = pick.as_ref().and_then(|_| self.typed_note(cx));
        self.side.busy = true;
        let call = self
            .state
            .read(cx)
            .artifact_actions()
            .pick_option(ArtifactPickOptionParams {
                artifact_id: artifact.id.clone(),
                option_id: pick.as_ref().map(|(id, _)| id.clone()),
                label: pick.map(|(_, label)| label).unwrap_or_default(),
                note,
            });
        let task = cx.spawn(async move |this, cx| {
            let reply = call.await;
            this.update(cx, |this, cx| {
                this.side.busy = false;
                match reply {
                    Ok(reply) => this.pick_recorded(&artifact, previous, reply.picked, cx),
                    Err(error) => {
                        this.notice =
                            Some(format!("Couldn\u{2019}t save the pick: {}", error.text()));
                        cx.notify();
                    }
                }
            })
            .ok();
        });
        self.side.tasks.push(task);
        cx.notify();
    }

    fn pick_recorded(
        &mut self,
        artifact: &ArtifactView,
        previous: Option<ArtifactOptionPick>,
        now: Option<ArtifactOptionPick>,
        cx: &mut Context<Self>,
    ) {
        self.side.local = Some((now.clone(), artifact.picked.clone()));
        if let Some(input) = &self.side.note {
            input.update(cx, |input, cx| input.set_text("", cx));
        }
        let chat = questions_ui::answer_target_chat(artifact).filter(|chat| {
            self.state
                .read(cx)
                .chats
                .iter()
                .any(|row| row.id == *chat && !row.archived)
        });
        match chat {
            Some(chat) => {
                let delivery = crate::work_outline::required_actions_view::deliver_user_message(
                    self.state.clone(),
                    chat,
                    pick_message(&artifact.title, previous.as_ref(), now.as_ref()),
                    crate::work_outline::actions::new_operation_id(),
                    cx,
                );
                let task = cx.spawn(async move |this, cx| {
                    if let Err(message) = delivery.await {
                        this.update(cx, |this, cx| {
                            this.notice = Some(format!(
                                "Pick saved, but the chat wasn\u{2019}t told: {message}"
                            ));
                            cx.notify();
                        })
                        .ok();
                    }
                });
                self.side.tasks.push(task);
            }
            None => {
                self.notice = Some(
                    "Pick saved. The chat that published these options is unavailable, so it wasn\u{2019}t told."
                        .to_owned(),
                );
            }
        }
        cx.notify();
    }

    // ------------------------------------------------------------------
    // Rendering
    // ------------------------------------------------------------------

    /// "Picked B · Grouped" for the header, when something is picked.
    pub(crate) fn pick_label(pick: &ArtifactOptionPick) -> String {
        if pick.label.is_empty() {
            format!("Picked {}", pick.option_id)
        } else {
            format!("Picked {} \u{b7} {}", pick.option_id, pick.label)
        }
    }

    pub(super) fn render_side_by_side(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let options = self.manifest_options();
        let artifact = self.artifact(cx);
        let picked = artifact
            .as_ref()
            .and_then(|artifact| self.picked_now(artifact));
        let question = match &self.body {
            Some(ArtifactBody::Options(manifest)) => manifest.question.clone(),
            _ => None,
        };
        let busy = self.side.busy;
        let note = self.side.note.clone();
        let text_button = |id: String, label: SharedString, strong: bool| {
            let selector = id.clone();
            div()
                .id(SharedString::from(id))
                .debug_selector(move || selector.clone())
                .flex_none()
                .h(px(24.0))
                .px(px(8.0))
                .rounded(px(5.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(11.5))
                .when(strong, |el| el.bg(theme.accent).text_color(theme.surface))
                .when(!strong, |el| {
                    el.border_1()
                        .border_color(theme.border)
                        .text_color(theme.text_muted)
                        .hover(|el| el.text_color(theme.text).bg(crate::theme::wash(0.10)))
                })
                .when(busy, |el| el.opacity(0.5))
                .child(label)
        };
        let columns = self.side.chosen.iter().copied().filter_map(|ix| {
            let option = options.get(ix)?.clone();
            let is_picked = picked
                .as_ref()
                .is_some_and(|pick| pick.option_id == option.id);
            let selector = format!("artifact-pick-{}", option.id);
            let button = if is_picked {
                text_button(selector, "\u{2713} Picked \u{b7} withdraw".into(), true)
            } else if picked.is_some() {
                text_button(selector, "Change pick to this".into(), false)
            } else {
                text_button(selector, "Pick this".into(), true)
            };
            Some(div().flex_1().min_w_0().flex().justify_center().child(
                button.on_click(cx.listener(move |this, _, _, cx| this.pick_option(ix, cx))),
            ))
        });
        // More than four options: chips choose which ones are shown.
        let chooser = (options.len() > MAX_COLUMNS).then(|| {
            div()
                .flex()
                .flex_wrap()
                .gap(px(4.0))
                .children(options.iter().enumerate().map(|(ix, option)| {
                    let on = self.side.chosen.contains(&ix);
                    let selector = format!("artifact-side-choose-{}", option.id);
                    let label = selector.clone();
                    div()
                        .id(SharedString::from(selector))
                        .debug_selector(move || label.clone())
                        .px(px(6.0))
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .text_size(crate::typography::ui_rems(11.0))
                        .border_1()
                        .border_color(if on { theme.accent } else { theme.border })
                        .text_color(if on { theme.text } else { theme.text_muted })
                        .child(SharedString::from(option.id.clone()))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.toggle_side_option(ix, window, cx)
                        }))
                }))
        });
        let status: Option<AnyElement> = if self.side.loading {
            Some(self.side_message("Loading options\u{2026}", theme))
        } else {
            self.side
                .error
                .clone()
                .map(|error| self.side_message(&error, theme))
        };
        let body = match status {
            Some(message) => message,
            None => div()
                .size_full()
                .debug_selector(|| "artifact-side-web".into())
                .children(self.web.as_ref().map(|web| web.element()))
                .into_any_element(),
        };
        div()
            .id("artifact-side-by-side")
            .debug_selector(|| "artifact-side-by-side".into())
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px(px(12.0))
                    .py(px(6.0))
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .text_size(crate::typography::ui_rems(11.5))
                            .child(
                                div().min_w_0().truncate().text_color(theme.text).child(
                                    SharedString::from(
                                        question
                                            .filter(|q| !q.trim().is_empty())
                                            .unwrap_or_else(|| "Pick an option".to_owned()),
                                    ),
                                ),
                            )
                            .children(picked.clone().map(|pick| {
                                div()
                                    .debug_selector(|| "artifact-picked-chip".into())
                                    .flex_none()
                                    .px(px(6.0))
                                    .rounded(px(4.0))
                                    .border_1()
                                    .border_color(theme.accent)
                                    .text_color(theme.accent)
                                    .child(SharedString::from(Self::pick_label(&pick)))
                            }))
                            .child(div().flex_1())
                            .children(chooser)
                            .child(
                                text_button(
                                    "artifact-side-close".into(),
                                    "All options".into(),
                                    false,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.close_side_by_side(window, cx),
                                )),
                            ),
                    )
                    .child(
                        div()
                            .debug_selector(|| "artifact-pick-note".into())
                            .rounded(px(5.0))
                            .border_1()
                            .border_color(theme.border)
                            .px(px(6.0))
                            .py(px(2.0))
                            .children(note),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "artifact-pick-row".into())
                    .flex_none()
                    .px(px(12.0))
                    .py(px(6.0))
                    .flex()
                    .gap(px(12.0))
                    .children(columns),
            )
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    fn side_message(&self, text: &str, theme: &Theme) -> AnyElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(24.0))
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text_muted)
            .debug_selector(|| "artifact-side-message".into())
            .child(SharedString::from(text.to_owned()))
            .into_any_element()
    }
}
