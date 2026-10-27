//! The PR page's tab bar and the two pr-dash tabs ("Authored by me",
//! "Review requested"): fetching `PrDashboard`, the toolbar (bucket chips,
//! hide drafts, group by status, text filter) and the row table. Model and
//! per-column strings: `crate::pr_page`; behaviour: pr-dash-spec.md §4-§7.

use chrono::{DateTime, Utc};
use gpui::{AnyElement, Context, ElementId, Hsla, MouseButton, SharedString, div, prelude::*, px};
use zeron_proto::{PrDashBucket, PrDashParams, PrDashReply, PrDashRow, PrDashTab, PrReference};
use zeron_proto::PrReviewAction;
use zeron_rpc::methods;

use zeron_proto::ChatIndicator;

use super::{ChatTypeView, review_button, status_line};
use crate::error_notice::ErrorNotice;
use crate::review_chats::{self, ChatFact, EnterAction, ReviewChat};
use crate::pr_page::{self, DashItem, Filters, PageTab, Tone, tab_index};
use crate::popover;
use crate::theme::Theme;
use crate::work_outline::chips::TextTooltip;

const W_DOT: f32 = 14.0;
const W_REPO: f32 = 92.0;
const W_NUMBER: f32 = 54.0;
const W_CHURN: f32 = 92.0;
const W_PEOPLE: f32 = 148.0;
const W_CI: f32 = 26.0;
const W_OPEN: f32 = 40.0;
const W_UPDATED: f32 = 44.0;
const W_ACTIONS: f32 = 236.0;

fn tone_color(theme: &Theme, tone: Tone) -> Hsla {
    match tone {
        Tone::Plain => theme.text_muted,
        Tone::Ok => theme.success,
        Tone::Bad => theme.danger,
        Tone::Warn => theme.warning,
        Tone::Muted => theme.text_faint,
    }
}

fn bucket_color(theme: &Theme, bucket: PrDashBucket) -> Hsla {
    match bucket {
        PrDashBucket::NeedsYou | PrDashBucket::ToReview | PrDashBucket::ChangesRequested => {
            theme.danger
        }
        PrDashBucket::Comments | PrDashBucket::ReReview | PrDashBucket::CiRunning => theme.warning,
        PrDashBucket::Ready | PrDashBucket::Approved => theme.success,
        _ => theme.text_faint,
    }
}

fn element_id(text: String) -> ElementId {
    ElementId::Name(text.into())
}

impl ChatTypeView {
    /// Fetch `tab` through `PrDashboard`. A failed call keeps the last good
    /// rows and shows the error inline.
    pub(super) fn fetch_dash(&mut self, tab: PrDashTab, force: bool, cx: &mut Context<Self>) {
        let ix = tab_index(tab);
        if self.dash[ix].pending {
            return;
        }
        let params = PrDashParams { tab, force };
        let params_json = serde_json::to_value(&params).map_err(|e| e.to_string());
        #[cfg(test)]
        if let Some(script) = self
            .shell
            .upgrade()
            .and_then(|shell| shell.read(cx).pr_dash_script.clone())
        {
            let result = params_json.and_then(|value| script(&value)).and_then(|value| {
                serde_json::from_value::<PrDashReply>(value).map_err(|e| e.to_string())
            });
            self.dash[ix].pending = true;
            cx.spawn(async move |this, cx| {
                this.update(cx, |this, cx| {
                    this.dash[ix].apply(result);
                    this.note_dash_links(ix, cx);
                    cx.notify();
                })
                .ok();
            })
            .detach();
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.dash[ix].pending = true;
        cx.spawn(async move |this, cx| {
            let result = match params_json {
                Ok(value) => engine
                    .client()
                    .call(methods::PR_DASHBOARD, value)
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|value| {
                        serde_json::from_value::<PrDashReply>(value).map_err(|e| e.to_string())
                    }),
                Err(error) => Err(error),
            };
            this.update(cx, |this, cx| {
                this.dash[ix].apply(result);
                this.note_dash_links(ix, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Tell the shared link index which chats the engine linked to each PR,
    /// so the Outline rows and the strip can show "being reviewed" too.
    fn note_dash_links(&self, ix: usize, cx: &mut Context<Self>) {
        for row in &self.dash[ix].rows {
            let links: Vec<_> = row
                .chats
                .iter()
                .map(|chat| (chat.chat_id.clone(), chat.source))
                .collect();
            review_chats::note_pr_links(cx, &row.url, &links);
        }
    }

    /// The review chats of a page row: the row's own chat list, the link
    /// index and the assignments of the PR's review item.
    fn review_chats_of(&self, row: &PrDashRow, cx: &gpui::App) -> Vec<ReviewChat> {
        let Some(pr) = crate::chat_work_context::parse_pr_url(&row.url) else {
            return Vec::new();
        };
        let state = self.state.read(cx);
        let links: Vec<_> = row
            .chats
            .iter()
            .map(|chat| (chat.chat_id.clone(), chat.source))
            .collect();
        let snapshot = state
            .selected_space_row()
            .and_then(|space| state.work_outline(&space.id).snapshot().cloned());
        // The row's own list knows chats the app state may not.
        let fact_of = |chat_id: &str| {
            review_chats::fact_from_state(state, chat_id).or_else(|| {
                let chat = row.chats.iter().find(|chat| chat.chat_id == chat_id)?;
                Some(ChatFact {
                    id: chat.chat_id.clone(),
                    title: chat.title.clone(),
                    archived: chat.archived,
                    updated: chat
                        .updated_at
                        .as_deref()
                        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                        .map(|at| at.with_timezone(&Utc)),
                    state: ChatIndicator::Idle,
                })
            })
        };
        review_chats::for_pr(cx, snapshot.as_deref(), &pr, &links, fact_of)
    }

    /// Up / Down move the highlighted row, Enter opens its review chat or,
    /// when it has none, starts a review.
    pub(super) fn dash_key_down(
        &mut self,
        tab: PrDashTab,
        event: &gpui::KeyDownEvent,
        cx: &mut Context<Self>,
    ) {
        let count = self.dash_visible(tab, cx).0.len();
        match event.keystroke.key.as_str() {
            "down" | "up" if count > 0 => {
                let direction = if event.keystroke.key == "down" { 1 } else { -1 };
                self.active = super::move_selection(self.active, count, direction);
                cx.stop_propagation();
                cx.notify();
            }
            "enter" if count > 0 => {
                self.activate_dash_row(cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    /// Enter on the highlighted row of the visible tab.
    pub(super) fn activate_dash_row(&mut self, cx: &mut Context<Self>) {
        let PageTab::Dash(tab) = self.tab else {
            return;
        };
        let (action, reference) = {
            let (visible, _) = self.dash_visible(tab, cx);
            let Some(row) = visible.get(self.active) else {
                return;
            };
            (
                review_chats::enter_action(&self.review_chats_of(row, cx)),
                PrReference::parse(&row.url),
            )
        };
        match action {
            EnterAction::OpenReview(chat_id) => self.open_chat(chat_id, cx),
            EnterAction::StartReview => {
                if let Some(reference) = reference {
                    self.start_review(reference, PrReviewAction::CodeReview, false, cx);
                }
            }
        }
    }

    /// The 60 s timer: refetch the visible tab while the page is on screen.
    pub(super) fn dash_tick(&mut self, cx: &mut Context<Self>) {
        let visible = self
            .shell
            .upgrade()
            .is_some_and(|shell| shell.read(cx).is_pr_review_chats_route());
        if !visible {
            return;
        }
        if let PageTab::Dash(tab) = self.tab {
            self.fetch_dash(tab, false, cx);
        }
        cx.notify();
    }

    pub(super) fn select_tab(&mut self, tab: PageTab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.active = 0;
        if let PageTab::Dash(tab) = tab {
            self.fetch_dash(tab, false, cx);
        }
        cx.notify();
    }

    /// The rows the tab shows (display order) and the admitted total.
    pub(super) fn dash_visible(&self, tab: PrDashTab, cx: &gpui::App) -> (Vec<&PrDashRow>, usize) {
        let ix = tab_index(tab);
        let state = &self.dash[ix];
        let query = self.dash_search[ix].read(cx).text().to_owned();
        let filters = Filters {
            hide_drafts: state.hide_drafts,
            group: state.group,
            buckets: &state.active_buckets,
            query: &query,
        };
        (
            pr_page::visible_rows(tab, &state.rows, &filters),
            pr_page::admitted_count(&state.rows, state.hide_drafts),
        )
    }

    pub(super) fn render_tab_bar(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let tabs = PageTab::ALL.into_iter().map(|page_tab| {
            let selected = self.tab == page_tab;
            let key = page_tab.key();
            let badge = match page_tab {
                PageTab::Dash(tab) => {
                    let state = &self.dash[tab_index(tab)];
                    state
                        .loaded
                        .then(|| pr_page::badge(tab, &state.rows, state.hide_drafts))
                }
                PageTab::Chats => None,
            };
            let selector = format!("pr-page-tab-{key}");
            div()
                .id(element_id(selector.clone()))
                .debug_selector(move || selector.clone())
                .flex()
                .items_center()
                .gap(px(6.0))
                .px(px(12.0))
                .py(px(7.0))
                .cursor_pointer()
                .border_b_2()
                .border_color(if selected {
                    theme.accent
                } else {
                    gpui::transparent_black()
                })
                .text_size(crate::typography::ui_rems(12.0))
                .font_weight(if selected {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::NORMAL
                })
                .text_color(if selected { theme.text } else { theme.text_muted })
                .hover(|el| el.bg(theme.element_hover))
                .on_click(cx.listener(move |this, _, _, cx| this.select_tab(page_tab, cx)))
                .child(page_tab.label())
                .when_some(badge, |el, badge| {
                    let selector = if badge.alert {
                        format!("pr-page-tab-badge-alert-{key}")
                    } else {
                        format!("pr-page-tab-badge-{key}")
                    };
                    el.child(
                        div()
                            .debug_selector(move || selector.clone())
                            .px(px(6.0))
                            .rounded(px(8.0))
                            .text_size(crate::typography::ui_rems(10.5))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .when(badge.alert, |el| {
                                el.bg(theme.danger.opacity(0.16)).text_color(theme.danger)
                            })
                            .when(!badge.alert, |el| {
                                el.bg(theme.element_hover).text_color(theme.text_muted)
                            })
                            .child(SharedString::from(badge.text)),
                    )
                })
        });
        div()
            .id("pr-page-tabs")
            .flex_none()
            .flex()
            .items_center()
            .px(px(8.0))
            .border_b_1()
            .border_color(theme.border)
            .children(tabs)
            .into_any_element()
    }

    pub(super) fn render_dash(
        &mut self,
        tab: PrDashTab,
        theme: &Theme,
        now: DateTime<Utc>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ix = tab_index(tab);
        if !self.dash[ix].fresh() && !self.dash[ix].pending {
            self.fetch_dash(tab, false, cx);
        }
        let (visible, total) = self.dash_visible(tab, cx);
        let state = &self.dash[ix];
        let items = pr_page::items(&visible, state.group);
        let chips = pr_page::chips(tab, &state.rows, state.hide_drafts);
        let key = PageTab::Dash(tab).key();

        let chip_row = chips.iter().map(|(bucket, count)| {
            let bucket = *bucket;
            let active = state.active_buckets.contains(&bucket);
            let selector = format!("pr-dash-chip-{key}-{}", pr_page::bucket_key(bucket));
            div()
                .id(element_id(selector.clone()))
                .debug_selector(move || selector.clone())
                .px(px(8.0))
                .py(px(3.0))
                .rounded(px(10.0))
                .border_1()
                .border_color(if active {
                    bucket_color(theme, bucket).opacity(0.7)
                } else {
                    theme.border
                })
                .when(active, |el| el.bg(bucket_color(theme, bucket).opacity(0.14)))
                .cursor_pointer()
                .hover(|el| el.bg(theme.element_hover))
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(if active { theme.text } else { theme.text_muted })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.dash[ix].toggle_bucket(bucket);
                    cx.notify();
                }))
                .child(SharedString::from(pr_page::chip_text(bucket, *count)))
        });
        let toggle = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .flex()
                .items_center()
                .gap(px(5.0))
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(if on { theme.text } else { theme.text_muted })
                .child(if on { "☑" } else { "☐" })
                .child(label)
        };
        let refreshing = state.pending;
        let toolbar = div()
            .id("pr-dash-toolbar")
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.0))
            .px(px(14.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .debug_selector(|| "pr-dash-search".into())
                    .w(px(220.0))
                    .child(popover::search_input_frame(
                        theme,
                        self.dash_search[ix].clone().into_any_element(),
                    )),
            )
            .children(chip_row)
            .child(div().flex_1())
            .child(
                toggle("pr-dash-hide-drafts", "hide drafts", state.hide_drafts).on_click(
                    cx.listener(move |this, _, _, cx| {
                        let state = &mut this.dash[ix];
                        state.hide_drafts = !state.hide_drafts;
                        cx.notify();
                    }),
                ),
            )
            .child(
                toggle("pr-dash-group", "group by status", state.group).on_click(cx.listener(
                    move |this, _, _, cx| {
                        let state = &mut this.dash[ix];
                        state.group = !state.group;
                        cx.notify();
                    },
                )),
            )
            .child(
                div()
                    .debug_selector(|| "pr-dash-summary".into())
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(pr_page::summary_text(
                        visible.len(),
                        total,
                    ))),
            )
            .child(
                review_button(
                    "pr-dash-refresh".into(),
                    if refreshing { "Refreshing…" } else { "Refresh" },
                    false,
                    refreshing,
                    theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.fetch_dash(tab, true, cx))),
            );

        let header = self.render_dash_header(tab, theme);
        let mut list: Vec<AnyElement> = Vec::with_capacity(items.len());
        let mut row_ix = 0;
        for item in &items {
            match item {
                DashItem::Header { bucket, count } => list.push(
                    div()
                        .debug_selector({
                            let id = format!("pr-dash-group-{}", pr_page::bucket_key(*bucket));
                            move || id.clone()
                        })
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .px(px(14.0))
                        .pt(px(10.0))
                        .pb(px(4.0))
                        .text_size(crate::typography::ui_rems(11.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(bucket_color(theme, *bucket))
                        .child(SharedString::from(pr_page::header_text(*bucket, *count)))
                        .into_any_element(),
                ),
                DashItem::Row(row) => {
                    list.push(self.render_dash_row(row_ix, tab, row, theme, now, cx));
                    row_ix += 1;
                }
            }
        }
        let message = if !state.loaded {
            Some(("pr-dash-loading", "Loading pull requests…"))
        } else if list.is_empty() {
            Some(if total == 0 {
                ("pr-dash-empty", "No pull requests")
            } else {
                ("pr-dash-empty", "No pull requests match the filters")
            })
        } else {
            None
        };
        let error = state.error.clone();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(toolbar)
            .when_some(error, |el, error| {
                el.child(
                    div()
                        .id("pr-dash-error")
                        .debug_selector(|| "pr-dash-error".into())
                        .flex_none()
                        .px(px(14.0))
                        .py(px(6.0))
                        .child(ErrorNotice::new(
                            "pr-dash-error-card",
                            theme,
                            "Couldn't refresh pull requests",
                            error,
                        )),
                )
            })
            .child(header)
            .child(
                div()
                    .id(element_id(format!("pr-dash-rows-{key}")))
                    .debug_selector(|| "pr-dash-rows".into())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when_some(message, |el, (selector, text)| {
                        el.child(
                            div()
                                .debug_selector(move || selector.into())
                                .py(px(28.0))
                                .flex()
                                .justify_center()
                                .text_color(theme.text_muted)
                                .child(text),
                        )
                    })
                    .children(list),
            )
            .into_any_element()
    }

    fn render_dash_header(&self, tab: PrDashTab, theme: &Theme) -> AnyElement {
        let people = match tab {
            PrDashTab::Authored => "REVIEWERS",
            PrDashTab::ReviewRequested => "AUTHOR",
        };
        let cell = |label: &'static str, width: f32| {
            div().flex_none().w(px(width)).truncate().child(label)
        };
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.0))
            .h(px(28.0))
            .px(px(14.0))
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.element_hover.opacity(0.45))
            .text_size(crate::typography::ui_rems(10.0))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme.text_muted)
            .child(div().flex_none().w(px(W_DOT)))
            .child(cell("REPO", W_REPO))
            .child(cell("PR", W_NUMBER))
            .child(div().flex_1().min_w_0().child("TITLE"))
            .child(cell("CHURN", W_CHURN))
            .child(cell(people, W_PEOPLE))
            .child(cell("CI", W_CI))
            .child(cell("OPEN", W_OPEN))
            .child(cell("UPD", W_UPDATED))
            .child(div().flex_none().w(px(W_ACTIONS)))
            .into_any_element()
    }

    fn render_dash_row(
        &self,
        ix: usize,
        tab: PrDashTab,
        row: &PrDashRow,
        theme: &Theme,
        now: DateTime<Utc>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let reference = PrReference::parse(&row.url);
        let status = reference
            .as_ref()
            .and_then(|reference| self.review_status.get(&reference.url()))
            .cloned();

        // Dot, repo, number.
        let dot_tip: SharedString = row.bucket.label().into();
        let dot = div()
            .id(("pr-dash-dot", ix))
            .flex_none()
            .w(px(W_DOT))
            .flex()
            .items_center()
            .child(
                div()
                    .size(px(8.0))
                    .rounded_full()
                    .bg(bucket_color(theme, row.bucket)),
            )
            .tooltip(move |_, cx| cx.new(|_| TextTooltip(dot_tip.clone())).into());
        let url = row.url.clone();
        let number_shell = self.shell.clone();
        let number = div()
            .id(("pr-dash-number", ix))
            .debug_selector(move || format!("pr-dash-number-{ix}"))
            .flex_none()
            .w(px(W_NUMBER))
            .cursor_pointer()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme.accent)
            .hover(|el| el.underline())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(crate::github::chip::open_pr(Some(number_shell), url))
            .child(SharedString::from(format!("#{}", row.number)));

        // Title, tags, linked chats.
        let tag_shell = self.shell.clone();
        let tags = pr_page::title_tags(row)
            .into_iter()
            .enumerate()
            .map(|(j, tag)| {
                let color = tone_color(theme, tag.tone);
                let pill = div()
                    .id(element_id(format!("pr-dash-tag-{ix}-{j}")))
                    .px(px(5.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(color.opacity(0.4))
                    .text_size(crate::typography::ui_rems(10.0))
                    .text_color(color)
                    .child(SharedString::from(tag.text));
                let tag_shell = tag_shell.clone();
                match tag.url {
                    Some(url) => pill
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(crate::github::chip::open_pr(Some(tag_shell), url))
                        .into_any_element(),
                    None => pill.into_any_element(),
                }
            });
        let chat_chips = row.chats.iter().enumerate().map(|(j, chat)| {
            let chat_id = chat.chat_id.clone();
            let mut tip = pr_page::link_source_text(chat.source).to_owned();
            if chat.archived {
                tip.push_str(" · archived");
            }
            let tip: SharedString = tip.into();
            let selector = format!("pr-dash-chat-{ix}-{j}");
            div()
                .id(element_id(selector.clone()))
                .debug_selector(move || selector.clone())
                .flex()
                .items_center()
                .gap(px(4.0))
                .px(px(6.0))
                .py(px(1.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(theme.border)
                .bg(theme.element_hover)
                .cursor_pointer()
                .hover(|el| el.bg(theme.element_active))
                .when(chat.archived, |el| el.opacity(0.5))
                .text_size(crate::typography::ui_rems(10.5))
                .text_color(theme.text_muted)
                .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| this.open_chat(chat_id.clone(), cx)))
                .child(crate::icons::icon(crate::icons::CHAT_ROUND_LINE).size(px(10.0)))
                .child(SharedString::from(pr_page::chat_chip_label(chat)))
        });
        let title = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(3.0))
            .child(
                div()
                    .truncate()
                    .text_color(theme.text)
                    .child(SharedString::from(row.title.clone())),
            )
            .child(div().flex().flex_wrap().gap(px(4.0)).children(tags))
            .when(!row.chats.is_empty(), |el| {
                el.child(div().flex().flex_wrap().gap(px(4.0)).children(chat_chips))
            });

        // Churn, reviewers / author, CI, open, updated.
        let (churn, excluded) = pr_page::churn(&row.diff);
        let churn = div()
            .flex_none()
            .w(px(W_CHURN))
            .flex()
            .items_start()
            .gap(px(2.0))
            .text_color(theme.text)
            .child(SharedString::from(churn))
            .when_some(excluded, |el, excluded| {
                el.child(
                    div()
                        .text_size(crate::typography::ui_rems(9.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(excluded)),
                )
            });
        let people = div()
            .flex_none()
            .w(px(W_PEOPLE))
            .flex()
            .flex_wrap()
            .gap(px(5.0))
            .text_size(crate::typography::ui_rems(11.0))
            .map(|el| match tab {
                PrDashTab::Authored if row.reviewers.is_empty() => {
                    el.text_color(theme.text_faint).child("—")
                }
                PrDashTab::Authored => el.children(row.reviewers.iter().map(|reviewer| {
                    let (glyph, tone) = pr_page::reviewer_glyph(&reviewer.state);
                    div()
                        .text_color(tone_color(theme, tone))
                        .child(SharedString::from(format!("{glyph} {}", reviewer.login)))
                })),
                PrDashTab::ReviewRequested => el
                    .text_color(theme.text_muted)
                    .child(SharedString::from(format!("@{}", row.author))),
            });
        let (ci_glyph, ci_tone) = pr_page::ci_glyph(row.ci.as_deref());
        let ci = div()
            .flex_none()
            .w(px(W_CI))
            .text_color(tone_color(theme, ci_tone))
            .child(ci_glyph);
        let (open, extra) = pr_page::open_count(row);
        let open_cell = div()
            .flex_none()
            .w(px(W_OPEN))
            .flex()
            .items_start()
            .when(open > 0, |el| {
                el.text_color(theme.warning)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(SharedString::from(open.to_string()))
            })
            .when(open == 0, |el| el.text_color(theme.text_faint).child("·"))
            .when_some(extra, |el, extra| {
                el.child(
                    div()
                        .text_size(crate::typography::ui_rems(9.0))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(format!("+{extra}"))),
                )
            });
        let updated = div()
            .flex_none()
            .w(px(W_UPDATED))
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text_faint)
            .child(SharedString::from(pr_page::relative_time(
                &row.updated_at,
                now,
            )));

        let review_chats = self.review_chats_of(row, cx);
        let primary = review_chats::primary(&review_chats).cloned();
        let open_review = primary
            .as_ref()
            .map(|chat| self.open_review_button(ix, chat, theme, cx));
        let overflow = primary.is_some().then(|| {
            self.review_overflow(ix, row, &review_chats, reference.clone(), theme, cx)
        });
        // Actions: Review / Open review on every row; Fix CI always on review
        // rows, only while CI fails on an authored PR.
        let start = |label: &'static str, kind: &'static str, action: PrReviewAction, primary| {
            let reference = reference.clone();
            review_button(
                format!("pr-dash-{kind}-{ix}").into(),
                label,
                primary,
                reference.is_none(),
                theme,
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                if let Some(reference) = reference.clone() {
                    this.start_review(reference, action, false, cx);
                }
            }))
        };
        let review_side = matches!(tab, PrDashTab::ReviewRequested);
        let actions = div()
            .flex_none()
            .w(px(W_ACTIONS))
            .flex()
            .justify_end()
            .items_start()
            .gap(px(6.0))
            .children(open_review)
            .when(primary.is_none(), |el| {
                el.child(start("Review", "review", PrReviewAction::CodeReview, true))
            })
            .when(review_side || pr_page::ci_failing(row), |el| {
                el.child(start("Fix CI", "fix-ci", PrReviewAction::FixCi, false))
            })
            .children(overflow);

        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .id(("pr-dash-row", ix))
                    .debug_selector(move || format!("pr-dash-row-{ix}"))
                    .flex()
                    .items_start()
                    .gap(px(8.0))
                    .px(px(14.0))
                    .py(px(6.0))
                    .border_b_1()
                    .border_color(theme.border.opacity(0.5))
                    .text_size(crate::typography::ui_rems(12.0))
                    .hover(|el| el.bg(theme.element_hover.opacity(0.5)))
                    .when(ix == self.active, |el| {
                        el.bg(theme.element_active.opacity(0.5))
                    })
                    .child(dot)
                    .child(
                        div()
                            .flex_none()
                            .w(px(W_REPO))
                            .truncate()
                            .text_color(theme.text_muted)
                            .child(SharedString::from(row.repo.clone())),
                    )
                    .child(number)
                    .child(title)
                    .child(churn)
                    .child(people)
                    .child(ci)
                    .child(open_cell)
                    .child(updated)
                    .child(actions),
            )
            .when_some(status, |el, status| {
                el.child(
                    div()
                        .px(px(14.0))
                        .child(status_line(format!("pr-dash-status-{ix}"), &status, theme)),
                )
            })
            .into_any_element()
    }

    /// The primary action of a row that is being reviewed: the chat's title
    /// and live state in the accent color.
    fn open_review_button(
        &self,
        ix: usize,
        chat: &ReviewChat,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chat_id = chat.chat_id.clone();
        let selector = format!("pr-dash-open-review-{ix}");
        let tip: SharedString = format!(
            "Open the review chat: {}",
            review_chats::open_review_detail(chat)
        )
        .into();
        div()
            .id(element_id(selector.clone()))
            .debug_selector(move || selector.clone())
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .px(px(9.0))
            .py(px(3.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.accent.opacity(0.5))
            .bg(theme.accent.opacity(0.08))
            .cursor_pointer()
            .hover(|el| el.bg(theme.accent.opacity(0.16)))
            .text_color(theme.accent)
            .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.open_chat(chat_id.clone(), cx);
            }))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .child("Open review"),
            )
            .child(
                div()
                    .debug_selector({
                        let selector = format!("pr-dash-open-review-detail-{ix}");
                        move || selector.clone()
                    })
                    .truncate()
                    .text_size(crate::typography::ui_rems(9.5))
                    .child(SharedString::from(review_chats::open_review_detail(chat))),
            )
            .into_any_element()
    }

    /// The row's "…" menu: "Start another review", then the other review
    /// chats of the PR.
    #[allow(clippy::too_many_arguments)]
    fn review_overflow(
        &self,
        ix: usize,
        row: &PrDashRow,
        review_chats: &[ReviewChat],
        reference: Option<PrReference>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selector = format!("pr-dash-more-{ix}");
        let url = row.url.clone();
        let trigger = div()
            .id(element_id(selector.clone()))
            .debug_selector({
                let selector = selector.clone();
                move || selector.clone()
            })
            .flex_none()
            .px(px(7.0))
            .py(px(3.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text_muted)
            .cursor_pointer()
            .hover(|el| el.bg(theme.element_hover))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.review_menu.note_trigger_press();
                }),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                if this.review_menu.take_press_was_open() {
                    this.close_review_menu(cx);
                } else {
                    this.review_menu.open(url.clone());
                    cx.notify();
                }
            }))
            .child("⋯");
        if self.review_menu.get().is_none_or(|url| *url != row.url) {
            return trigger.into_any_element();
        }
        let theme = &theme.for_popup();
        let closing = self.review_menu.closing_since();
        let start_another = {
            let id = format!("pr-dash-start-another-{ix}");
            popover::menu_row(theme, false, id.clone())
                .id(element_id(id.clone()))
                .debug_selector(move || id.clone())
                .when(reference.is_none(), |el| el.opacity(0.5))
                .child("Start another review")
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.close_review_menu(cx);
                    if let Some(reference) = reference.clone() {
                        this.start_review(reference, PrReviewAction::CodeReview, false, cx);
                    }
                }))
        };
        let others = review_chats::others(review_chats).iter().enumerate().map(|(j, chat)| {
            let id = format!("pr-dash-review-other-{ix}-{j}");
            let chat_id = chat.chat_id.clone();
            popover::menu_row(theme, false, id.clone())
                .id(element_id(id.clone()))
                .debug_selector(move || id.clone())
                .child(SharedString::from(format!(
                    "Open {}",
                    review_chats::open_review_detail(chat)
                )))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.close_review_menu(cx);
                    this.open_chat(chat_id.clone(), cx);
                }))
        });
        let has_others = review_chats.len() > 1;
        let card = popover::popover_card(theme)
            .id(element_id(format!("pr-dash-menu-{ix}")))
            .debug_selector(move || format!("pr-dash-menu-{ix}"))
            .flex()
            .flex_col()
            .min_w(px(220.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_review_menu(cx)))
            .child(start_another)
            .when(has_others, |el| {
                el.child(popover::menu_heading(theme, "Other review chats"))
            })
            .children(others)
            .into_any_element();
        trigger
            .relative()
            .child(popover::anchored_menu_below_end(
                format!("pr-dash-menu-layer-{ix}"),
                card,
                closing,
            ))
            .into_any_element()
    }

    fn close_review_menu(&mut self, cx: &mut Context<Self>) {
        if self.review_menu.begin_close() {
            popover::reap_popup(cx, |this: &mut Self| &mut this.review_menu);
        }
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn set_tab(&mut self, tab: PageTab, cx: &mut Context<Self>) {
        self.select_tab(tab, cx);
    }

    #[cfg(test)]
    pub(crate) fn dash_badge(&self, tab: PrDashTab) -> Option<pr_page::Badge> {
        let state = &self.dash[tab_index(tab)];
        state
            .loaded
            .then(|| pr_page::badge(tab, &state.rows, state.hide_drafts))
    }

    /// What the tab lists, top to bottom: group headers as `glyph label ·
    /// count` and rows as `#number`.
    #[cfg(test)]
    pub(crate) fn dash_lines(&self, tab: PrDashTab, cx: &gpui::App) -> Vec<String> {
        let (visible, _) = self.dash_visible(tab, cx);
        pr_page::items(&visible, self.dash[tab_index(tab)].group)
            .into_iter()
            .map(|item| match item {
                DashItem::Header { bucket, count } => pr_page::header_text(bucket, count),
                DashItem::Row(row) => format!("#{}", row.number),
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn dash_summary(&self, tab: PrDashTab, cx: &gpui::App) -> String {
        let (visible, total) = self.dash_visible(tab, cx);
        pr_page::summary_text(visible.len(), total)
    }

    #[cfg(test)]
    pub(crate) fn dash_search_input(&self, tab: PrDashTab) -> gpui::Entity<crate::composer::ComposerInput> {
        self.dash_search[tab_index(tab)].clone()
    }
}
