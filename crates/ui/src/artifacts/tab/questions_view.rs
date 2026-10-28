//! The artifact tab's plan question state: answering in place, the
//! questions / drift / "updated since you last viewed" badge row, and the
//! per-artifact visit record. Decisions live in `artifacts::questions_ui`.
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    AnyElement, Context, Entity, Focusable, SharedString, Subscription, Task, Window, div,
    prelude::*, px,
};
use zeron_proto::{
    ArtifactAnswerQuestionParams, ArtifactDriftParams, ArtifactDriftReply, ArtifactKind,
    ArtifactView, PlanQuestion, extract_questions,
};

use super::ArtifactTab;
use crate::artifacts::questions_ui::{self, DriftGate};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::markdown::render::AnswerUi;
use crate::theme::Theme;
use crate::work_outline::chips::TextTooltip;

/// What the tab remembers about the plan's questions.
#[derive(Default)]
pub(crate) struct QuestionState {
    /// The question whose answer input is open (upper-cased id).
    pub(super) answering: Option<String>,
    input: Option<Entity<ComposerInput>>,
    busy: bool,
    /// Answers sent from this tab, shown until the snapshot carries them.
    local: HashMap<String, String>,
    /// What the Markdown preview was last given.
    synced: Option<(Vec<(String, String)>, Option<String>, bool)>,
    cached: Option<(u64, Vec<PlanQuestion>)>,
    drift: Option<ArtifactDriftReply>,
    gate: DriftGate,
    /// The revision last recorded as viewed, and the one the previous
    /// visit left (the marker's `from`).
    visit_recorded: u32,
    visit_base: Option<u32>,
    pub(super) changed: Option<(u32, u32)>,
    tasks: Vec<Task<()>>,
    subscriptions: Vec<Subscription>,
}

impl ArtifactTab {
    /// The plan's questions from the shown document, cached per load.
    pub(crate) fn plan_questions(&mut self, kind: ArtifactKind) -> Vec<PlanQuestion> {
        if self.loading {
            return Vec::new();
        }
        if let Some((generation, questions)) = &self.questions.cached {
            if *generation == self.generation {
                return questions.clone();
            }
        }
        let questions = self
            .shown_text()
            .map(|text| extract_questions(kind, &text))
            .unwrap_or_default();
        self.questions.cached = Some((self.generation, questions.clone()));
        questions
    }

    /// Everything the tab keeps in step with the snapshot; cheap when
    /// nothing changed. Called from render.
    pub(super) fn track_questions(
        &mut self,
        artifact: &ArtifactView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.note_visit(artifact, cx);
        self.maybe_fetch_drift(artifact, cx);
        self.sync_answers(artifact, window, cx);
    }

    // ------------------------------------------------------------------
    // Changed since you last viewed
    // ------------------------------------------------------------------

    /// Compute the marker from the stored visit, then record the current
    /// revision so the marker survives until the next visit.
    fn note_visit(&mut self, artifact: &ArtifactView, cx: &mut Context<Self>) {
        let current = artifact.current_revision;
        if current == 0 || current <= self.questions.visit_recorded {
            return;
        }
        if self.questions.visit_recorded == 0 {
            self.questions.visit_base = crate::settings::current(cx)
                .artifact_last_viewed
                .get(&artifact.id)
                .copied();
        }
        self.questions.changed = questions_ui::changed_since(self.questions.visit_base, current);
        self.questions.visit_recorded = current;
        let id = artifact.id.clone();
        cx.defer(move |cx| {
            crate::settings::update(crate::settings::SavePolicy::Debounced, cx, |settings| {
                settings.artifact_last_viewed.insert(id, current);
            });
        });
    }

    /// Open Compare from the last viewed revision to the current one.
    pub(crate) fn open_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((from, _)) = self.questions.changed.take() else {
            return;
        };
        self.compare_since(from, window, cx);
    }

    // ------------------------------------------------------------------
    // Drift
    // ------------------------------------------------------------------

    fn maybe_fetch_drift(&mut self, artifact: &ArtifactView, cx: &mut Context<Self>) {
        if artifact.current_revision == 0
            || self.loading
            || !self
                .questions
                .gate
                .due(artifact.current_revision, Instant::now())
        {
            return;
        }
        let state = self.state.read(cx);
        let target_device_id = (state.local_device_id.as_deref() != Some(&artifact.device_id)
            && !artifact.device_id.is_empty())
        .then(|| artifact.device_id.clone());
        let call = state.artifact_actions().drift(ArtifactDriftParams {
            artifact_id: artifact.id.clone(),
            target_device_id,
        });
        let task = cx.spawn(async move |this, cx| {
            let reply = call.await;
            this.update(cx, |this, cx| {
                // Failures (an older host) just leave no commits badge.
                this.questions.drift = reply.ok();
                cx.notify();
            })
            .ok();
        });
        self.questions.tasks.push(task);
    }

    // ------------------------------------------------------------------
    // Answering
    // ------------------------------------------------------------------

    fn answer_map(&self, artifact: &ArtifactView) -> HashMap<String, String> {
        let mut map = self.questions.local.clone();
        map.extend(questions_ui::answer_texts(artifact));
        map
    }

    /// Give the Markdown preview the answers and the Answer affordance
    /// (only while the current revision is on screen).
    fn sync_answers(
        &mut self,
        artifact: &ArtifactView,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(preview) = self.markdown.clone() else {
            return;
        };
        let live = self.shown_or_current_revision(artifact) == artifact.current_revision;
        let answers = self.answer_map(artifact);
        let mut sig: Vec<(String, String)> = answers.clone().into_iter().collect();
        sig.sort();
        let editing = live.then(|| self.questions.answering.clone()).flatten();
        let signature = (sig, editing.clone(), live);
        if self.questions.synced.as_ref() == Some(&signature) {
            return;
        }
        self.questions.synced = Some(signature);
        let ui = live.then(|| {
            let owner = cx.weak_entity();
            let begin = owner.clone();
            let send = owner.clone();
            let cancel = owner;
            AnswerUi {
                answers: Rc::new(answers),
                editing,
                input: self
                    .questions
                    .input
                    .clone()
                    .map(|input| gpui::AnyView::from(input)),
                begin: Rc::new(move |id, window, cx| {
                    begin
                        .update(cx, |this, cx| this.begin_answer(id, window, cx))
                        .ok();
                }),
                send: Rc::new(move |id, _, cx| {
                    send.update(cx, |this, cx| this.send_answer(id, cx)).ok();
                }),
                cancel: Rc::new(move |_, cx| {
                    cancel.update(cx, |this, cx| this.cancel_answer(cx)).ok();
                }),
            }
        });
        preview.update(cx, |preview, cx| preview.set_question_answers(ui, cx));
    }

    pub(crate) fn begin_answer(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let input = match self.questions.input.clone() {
            Some(input) => input,
            None => {
                let input = cx.new(|cx| {
                    ComposerInput::new("Your answer", cx)
                        .with_text_metrics(12.0, 18.0)
                        .with_auto_grow()
                });
                let subscription =
                    cx.subscribe(&input, |this: &mut Self, _, event, cx| match event {
                        ComposerInputEvent::Submitted | ComposerInputEvent::ModifiedSubmitted => {
                            if let Some(id) = this.questions.answering.clone() {
                                this.send_answer(id, cx);
                            }
                        }
                        _ => {}
                    });
                self.questions.subscriptions.push(subscription);
                self.questions.input = Some(input.clone());
                input
            }
        };
        input.update(cx, |input, cx| input.set_text("", cx));
        let handle = input.focus_handle(cx);
        window.focus(&handle, cx);
        self.questions.answering = Some(id.to_ascii_uppercase());
        self.questions.synced = None;
        cx.notify();
    }

    pub(crate) fn cancel_answer(&mut self, cx: &mut Context<Self>) {
        self.questions.answering = None;
        self.questions.synced = None;
        cx.notify();
    }

    /// The typed text of the answer input (tests, Send).
    fn typed_answer(&self, cx: &gpui::App) -> String {
        self.questions
            .input
            .as_ref()
            .map(|input| input.read(cx).text().trim().to_owned())
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn set_answer_text_for_tests(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(input) = self.questions.input.clone() {
            input.update(cx, |input, cx| input.set_text(text, cx));
        }
    }

    /// Record the typed answer, then tell the chat that published the plan.
    pub(crate) fn send_answer(&mut self, id: String, cx: &mut Context<Self>) {
        let id = id.to_ascii_uppercase();
        let answer = self.typed_answer(cx);
        if answer.is_empty() || self.questions.busy {
            return;
        }
        let Some(artifact) = self.artifact(cx) else {
            return;
        };
        let question_text = self
            .plan_questions(artifact.kind)
            .into_iter()
            .find(|question| question.id.eq_ignore_ascii_case(&id))
            .map(|question| question.text)
            .unwrap_or_default();
        self.questions.busy = true;
        let call =
            self.state
                .read(cx)
                .artifact_actions()
                .answer_question(ArtifactAnswerQuestionParams {
                    artifact_id: artifact.id.clone(),
                    question_id: id.clone(),
                    answer: answer.clone(),
                });
        let task = cx.spawn(async move |this, cx| {
            let reply = call.await;
            this.update(cx, |this, cx| {
                this.questions.busy = false;
                match reply {
                    Ok(_) => this.answer_recorded(&artifact, id, question_text, answer, cx),
                    Err(error) => {
                        this.notice =
                            Some(format!("Couldn\u{2019}t save the answer: {}", error.text()));
                        cx.notify();
                    }
                }
            })
            .ok();
        });
        self.questions.tasks.push(task);
    }

    fn answer_recorded(
        &mut self,
        artifact: &ArtifactView,
        id: String,
        question_text: String,
        answer: String,
        cx: &mut Context<Self>,
    ) {
        self.questions.local.insert(id.clone(), answer.clone());
        self.questions.answering = None;
        self.questions.synced = None;
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
                    questions_ui::answer_message(&id, &question_text, &answer),
                    crate::work_outline::actions::new_operation_id(),
                    cx,
                );
                let task = cx.spawn(async move |this, cx| {
                    if let Err(message) = delivery.await {
                        this.update(cx, |this, cx| {
                            this.notice = Some(format!(
                                "Answer saved, but the chat wasn\u{2019}t told: {message}"
                            ));
                            cx.notify();
                        })
                        .ok();
                    }
                });
                self.questions.tasks.push(task);
            }
            // The publishing chat is gone: the answer is still on the row.
            None => {
                self.notice = Some(
                    "Answer saved. The chat that published this plan is unavailable, so it wasn\u{2019}t told."
                        .to_owned(),
                );
            }
        }
        cx.notify();
    }

    // ------------------------------------------------------------------
    // Badge row
    // ------------------------------------------------------------------

    /// "N open questions", the drift badge and the changed-since chip.
    pub(super) fn render_badges(
        &mut self,
        artifact: &ArtifactView,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let questions = self.plan_questions(artifact.kind);
        let answers = self.answer_map(artifact);
        let answer_rows: Vec<_> = answers
            .iter()
            .map(|(id, text)| zeron_proto::ArtifactQuestionAnswer {
                id: id.clone(),
                answer: text.clone(),
                at: i64::MAX,
                ..Default::default()
            })
            .collect();
        // Snapshot answers keep their own times; local-only ones are newer
        // than any revision.
        let snapshot_ids: Vec<&str> = artifact.answers.iter().map(|a| a.id.as_str()).collect();
        let rows: Vec<_> = artifact
            .answers
            .iter()
            .cloned()
            .chain(
                answer_rows
                    .into_iter()
                    .filter(|a| !snapshot_ids.iter().any(|id| id.eq_ignore_ascii_case(&a.id))),
            )
            .collect();
        let open = questions_ui::open_questions_label(&questions, &rows);
        let revision_at = artifact.current().map(|revision| revision.at).unwrap_or(0);
        let drift = questions_ui::drift_badge(
            &questions,
            &rows,
            artifact.current_revision,
            revision_at,
            self.questions.drift.as_ref(),
            crate::artifacts::model::now_ms(),
        );
        let changed = self.questions.changed;
        if open.is_none() && drift.is_none() && changed.is_none() {
            return None;
        }
        let mut row = div()
            .flex_none()
            .px(px(12.0))
            .py(px(5.0))
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(px(6.0))
            .border_b_1()
            .border_color(theme.border);
        if let Some(label) = open {
            row = row.child(
                badge(
                    "artifact-open-questions",
                    label,
                    None,
                    theme.text_muted,
                    theme,
                )
                .into_any_element(),
            );
        }
        if let Some(badge_model) = drift {
            row = row.child(
                badge(
                    "artifact-drift-badge",
                    badge_model.label,
                    Some(badge_model.tooltip),
                    theme.warning,
                    theme,
                )
                .into_any_element(),
            );
        }
        if let Some((from, to)) = changed {
            let note = artifact
                .current()
                .map(|revision| revision.note.clone())
                .filter(|note| !note.trim().is_empty());
            row = row.child(
                badge(
                    "artifact-changed-since",
                    questions_ui::changed_since_label(from, to),
                    note,
                    theme.accent,
                    theme,
                )
                .cursor_pointer()
                .on_click(cx.listener(|this, _, window, cx| this.open_changes(window, cx)))
                .into_any_element(),
            );
        }
        Some(row.into_any_element())
    }
}

fn badge(
    selector: &'static str,
    label: String,
    tooltip: Option<String>,
    color: gpui::Hsla,
    _theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let tooltip = tooltip.map(SharedString::from);
    div()
        .id(selector)
        .debug_selector(move || selector.to_owned())
        .flex_none()
        .px(px(7.0))
        .h(px(18.0))
        .flex()
        .items_center()
        .rounded(px(9.0))
        .border_1()
        .border_color(color.opacity(0.5))
        .bg(color.opacity(0.10))
        .text_size(crate::typography::ui_rems(10.5))
        .text_color(color)
        .child(SharedString::from(label))
        .when_some(tooltip, |el, tip| {
            el.tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
        })
}
