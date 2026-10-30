//! "Template changed": the chip and the per-project review in the Project
//! section of Work Item Details.
//!
//! A project whose template link is behind shows the chip and **Review
//! changes…**. The review lists what differs (the pure
//! `diff_template_against_project`); each change has **Accept** (sends its
//! mutations, see `template_review.rs`) and **Skip** (sends nothing). The
//! footer has **Accept all** and **Mark reviewed** (only the template link
//! moves to the current revision). Removed items are information, with no
//! Accept. Nothing is applied by itself, and none of it is an undo step.
//!
//! A child module of `details_panel`, so it uses the surface's private
//! state directly.
use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};
use mast_proto::{TemplateChange, WorkItemView, WorkOutlineMutation, diff_template_against_project};

use super::super::actions::new_operation_id;
use super::super::chips::template_chip;
use super::super::template_review as review;
use super::{DetailsEvent, WorkItemDetails};
use crate::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowState {
    Pending,
    Applied,
    Skipped,
    Failed(String),
}

#[derive(Debug, Clone)]
pub(crate) struct ReviewRow {
    pub change: TemplateChange,
    pub state: RowState,
}

/// The open review of one project.
#[derive(Debug, Clone)]
pub(crate) struct ReviewState {
    pub item_id: String,
    pub template_id: String,
    pub rows: Vec<ReviewRow>,
    pub error: Option<String>,
}

/// What a press asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewRun {
    /// Accept one row.
    One(usize),
    /// Accept every pending row.
    All,
    /// Only the link.
    MarkReviewed,
}

impl ReviewState {
    /// Applicable rows that are not applied yet (skipped ones count).
    fn unfinished(&self, except: &[usize]) -> bool {
        self.rows.iter().enumerate().any(|(ix, row)| {
            review::applicable(&row.change)
                && row.state != RowState::Applied
                && !except.contains(&ix)
        })
    }
}

impl WorkItemDetails {
    /// "Review changes…": compute the list (nothing is written).
    pub(crate) fn begin_template_review(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let Some(item) = self.item().cloned() else {
            return;
        };
        let Some(template) = snapshot
            .templates
            .iter()
            .find(|template| Some(&template.id) == item.template_id.as_ref())
        else {
            return;
        };
        let rows = diff_template_against_project(&snapshot, template, &item.id)
            .into_iter()
            .map(|change| ReviewRow {
                change,
                state: RowState::Pending,
            })
            .collect();
        self.templates.review = Some(ReviewState {
            item_id: item.id.clone(),
            template_id: template.id.clone(),
            rows,
            error: None,
        });
        cx.notify();
    }

    pub(crate) fn close_template_review(&mut self, cx: &mut Context<Self>) {
        if self.templates.review.take().is_some() {
            cx.notify();
        }
    }

    /// Skip: the row stays as it is; nothing is sent.
    pub(crate) fn skip_template_change(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(row) = self
            .templates
            .review
            .as_mut()
            .and_then(|review| review.rows.get_mut(ix))
        {
            if row.state == RowState::Pending || matches!(row.state, RowState::Failed(_)) {
                row.state = RowState::Skipped;
                cx.notify();
            }
        }
    }

    /// Send what `run` asks for: the rows' mutations in order, then (when
    /// every applicable change is applied, or for Mark reviewed) the link.
    pub(crate) fn run_template_review(&mut self, run: ReviewRun, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let Some(state) = self.templates.review.clone() else {
            return;
        };
        let Some(template) = snapshot
            .templates
            .iter()
            .find(|template| template.id == state.template_id)
            .cloned()
        else {
            self.template_review_error("That template no longer exists", cx);
            return;
        };
        let project_id = state.item_id.clone();
        let chosen: Vec<usize> = match run {
            ReviewRun::One(ix) => vec![ix],
            ReviewRun::All => state
                .rows
                .iter()
                .enumerate()
                .filter(|(_, row)| {
                    review::applicable(&row.change)
                        && matches!(row.state, RowState::Pending | RowState::Failed(_))
                })
                .map(|(ix, _)| ix)
                .collect(),
            ReviewRun::MarkReviewed => Vec::new(),
        };
        if let ReviewRun::One(ix) = run {
            let ok = state.rows.get(ix).is_some_and(|row| {
                review::applicable(&row.change)
                    && matches!(row.state, RowState::Pending | RowState::Failed(_))
            });
            if !ok {
                return;
            }
        }
        // Plan everything now, against the snapshot the user is looking at.
        let key = new_operation_id();
        let mut overlay = review::Overlay::new();
        let mut planned: Vec<(usize, Vec<WorkOutlineMutation>)> = Vec::new();
        for ix in &chosen {
            let change_key = mast_proto::derive_id(&key, &format!("review-row:{ix}"));
            match review::plan_change(
                &snapshot,
                &project_id,
                &state.rows[*ix].change,
                &change_key,
                &mut overlay,
            ) {
                Ok(mutations) => planned.push((*ix, mutations)),
                Err(message) => {
                    if let Some(row) = self
                        .templates
                        .review
                        .as_mut()
                        .and_then(|review| review.rows.get_mut(*ix))
                    {
                        row.state = RowState::Failed(message);
                    }
                }
            }
        }
        let planned_ix: Vec<usize> = planned.iter().map(|(ix, _)| *ix).collect();
        let advance = match run {
            ReviewRun::MarkReviewed => true,
            _ => chosen.len() == planned.len() && !state.unfinished(&planned_ix),
        };
        if planned.is_empty() && !advance {
            cx.notify();
            return;
        }
        let in_flight = "template-review".to_owned();
        if !self.in_flight.insert(in_flight.clone()) {
            return;
        }
        if let Some(open) = self.templates.review.as_mut() {
            open.error = None;
        }
        let link = review::link_mutation(&project_id, &template, &key);
        let actions = self.state.read(cx).work_outline_actions();
        let name = template.name.clone();
        let revision = template.revision;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let mut failed = false;
            for (ix, mutations) in planned {
                let mut failure = None;
                for mutation in mutations {
                    if let Err(error) = actions.mutate(mutation).await {
                        failure = Some(error.message);
                        break;
                    }
                }
                failed |= failure.is_some();
                this.update(cx, |this, cx| {
                    if let Some(row) = this
                        .templates
                        .review
                        .as_mut()
                        .and_then(|review| review.rows.get_mut(ix))
                    {
                        row.state = match failure {
                            None => RowState::Applied,
                            Some(message) => RowState::Failed(message),
                        };
                    }
                    cx.notify();
                })
                .ok();
                if failed {
                    break;
                }
            }
            let mut link_error = None;
            if advance && !failed {
                if let Err(error) = actions.mutate(link).await {
                    link_error = Some(error.message);
                }
            }
            this.update(cx, |this, cx| {
                this.in_flight.remove(&in_flight);
                if let Some(message) = link_error {
                    this.template_review_error(&message, cx);
                } else if failed {
                    this.template_review_error(
                        "Some changes could not be applied. The rest are unchanged",
                        cx,
                    );
                } else if advance {
                    this.templates.review = None;
                    cx.emit(DetailsEvent::Notice {
                        text: format!(
                            "Reviewed against template \u{2018}{name}\u{2019} \u{b7} rev {revision}"
                        ),
                        error: false,
                    });
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn template_review_error(&mut self, message: &str, cx: &mut Context<Self>) {
        if let Some(open) = self.templates.review.as_mut() {
            open.error = Some(message.to_owned());
        }
        cx.emit(DetailsEvent::Notice {
            text: format!("Template review\n{message}"),
            error: true,
        });
        cx.notify();
    }

    /// The chip and **Review changes…** (and the open review) for `item`.
    pub(super) fn render_template_review(
        &self,
        item: &WorkItemView,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut out = Vec::new();
        let Some(snapshot) = self.snapshot.as_deref() else {
            return out;
        };
        let Some((from, to)) = review::chip_revisions(snapshot, item) else {
            return out;
        };
        let open = self
            .templates
            .review
            .as_ref()
            .filter(|open| open.item_id == item.id);
        out.push(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(template_chip("work-item-template-changed".to_owned(), from, to, theme))
                .when(open.is_none(), |row| {
                    row.child(
                        Self::small_button(
                            "work-item-review-template",
                            "Review changes\u{2026}",
                            theme,
                        )
                        .debug_selector(|| "work-item-review-template".into())
                        .on_click(cx.listener(|this, _, _, cx| this.begin_template_review(cx))),
                    )
                })
                .into_any_element(),
        );
        let Some(open) = open else {
            return out;
        };
        if open.rows.is_empty() {
            out.push(
                div()
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_muted)
                    .child("Nothing differs in structure or instructions. Mark reviewed to clear the chip.")
                    .into_any_element(),
            );
        }
        for (ix, row) in open.rows.iter().enumerate() {
            out.push(self.render_review_row(ix, row, theme, cx));
        }
        if let Some(error) = &open.error {
            out.push(
                div()
                    .debug_selector(|| "work-item-review-error".into())
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(error.clone()))
                    .into_any_element(),
            );
        }
        out.push(
            div()
                .flex()
                .gap(px(6.0))
                .child(
                    Self::small_button("work-item-review-accept-all", "Accept all", theme)
                        .debug_selector(|| "work-item-review-accept-all".into())
                        .on_click(
                            cx.listener(|this, _, _, cx| this.run_template_review(ReviewRun::All, cx)),
                        ),
                )
                .child(
                    Self::small_button("work-item-review-mark-reviewed", "Mark reviewed", theme)
                        .debug_selector(|| "work-item-review-mark-reviewed".into())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.run_template_review(ReviewRun::MarkReviewed, cx)
                        })),
                )
                .child(
                    Self::small_button("work-item-review-close", "Close", theme)
                        .on_click(cx.listener(|this, _, _, cx| this.close_template_review(cx))),
                )
                .into_any_element(),
        );
        out
    }

    fn render_review_row(
        &self,
        ix: usize,
        row: &ReviewRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (sign, text) = review::describe(&row.change);
        let selector = format!("work-item-review-row-{ix}");
        let status: Option<SharedString> = match &row.state {
            RowState::Pending => None,
            RowState::Applied => Some("Applied".into()),
            RowState::Skipped => Some("Skipped".into()),
            RowState::Failed(message) => Some(format!("Failed: {message}").into()),
        };
        let actionable = review::applicable(&row.change)
            && matches!(row.state, RowState::Pending | RowState::Failed(_));
        let mut line = div()
            .debug_selector(move || selector.clone())
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .flex_none()
                    .w(px(10.0))
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text_muted)
                    .child(sign),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(crate::typography::ui_rems(10.5))
                    .text_color(theme.text)
                    .child(SharedString::from(text)),
            );
        if let Some(status) = status {
            line = line.child(
                div()
                    .flex_none()
                    .text_size(crate::typography::ui_rems(10.0))
                    .text_color(theme.text_muted)
                    .child(status),
            );
        }
        if actionable {
            let accept = format!("work-item-review-accept-{ix}");
            let skip = format!("work-item-review-skip-{ix}");
            line = line
                .child(
                    Self::small_button(("work-item-review-accept", ix), "Accept", theme)
                        .debug_selector(move || accept.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.run_template_review(ReviewRun::One(ix), cx)
                        })),
                )
                .child(
                    Self::small_button(("work-item-review-skip", ix), "Skip", theme)
                        .debug_selector(move || skip.clone())
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.skip_template_change(ix, cx)),
                        ),
                );
        }
        line.into_any_element()
    }
}
