//! Filtered, type-specific chat inboxes. The row model and filter pipeline are
//! intentionally shared so later ticket/debug/implementation inboxes only add
//! a [`ChatViewKind`] specification instead of another one-off page.

use chrono::{DateTime, Utc};
use gpui::{
    Context, Entity, FocusHandle, MouseButton, Render, ScrollHandle, SharedString, Subscription,
    Window, div, prelude::*, px,
};
use mast_engine::pr_ticket_cache::{ChatLinkStatus, PrLinkStatus, PrStatus};
use mast_proto::{Chat, ChatIndicator, ChatLinkSource, PrDashTab, PrReference, PrReviewAction};
use mast_rpc::methods;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::popover;
use crate::pr_page::{self, DashTabState, PageTab};
use crate::shell::Shell;
use crate::state::{AppState, format_time_ago};
use crate::theme::Theme;

mod pr_destination;
mod pr_tabs;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChatViewKind {
    PrReview,
}

impl ChatViewKind {
    fn title(self) -> &'static str {
        match self {
            Self::PrReview => "Pull requests",
        }
    }

    fn empty_message(self) -> &'static str {
        match self {
            Self::PrReview => "No PR review chats match this view",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct RowsKey {
    state: u64,
    links_rev: u64,
    show_archived: bool,
    query: String,
}

#[derive(Clone)]
struct TypeChatRow {
    chat: Chat,
    status: ChatIndicator,
    repo: String,
    pr: Option<PrSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PrSummary {
    number: u64,
    title: Option<String>,
    state: Option<String>,
    branch: Option<String>,
}

impl PrSummary {
    /// The PR the chat was launched to review, else its first linked PR, else
    /// the legacy primary-PR mirror.
    fn from_status(status: &ChatLinkStatus) -> Option<Self> {
        let Some(link) = launcher_link(status).or_else(|| status.pr_links.first()) else {
            return status.pr.as_ref().map(Self::from_pr);
        };
        if let Some(pr) = &link.detail {
            return Some(Self::from_pr(pr));
        }
        Some(Self {
            number: pr_number_from_url(&link.url),
            title: None,
            state: None,
            branch: None,
        })
    }

    fn from_pr(pr: &PrStatus) -> Self {
        Self {
            number: pr.number,
            title: pr.title.clone(),
            state: Some(if pr.is_draft {
                "draft".to_string()
            } else {
                pr.state.clone()
            }),
            branch: pr.branch.clone(),
        }
    }
}

/// The link the PR review launcher created for this chat, if any.
fn launcher_link(status: &ChatLinkStatus) -> Option<&PrLinkStatus> {
    status
        .pr_links
        .iter()
        .find(|link| link.source == Some(ChatLinkSource::ReviewLauncher))
}

fn pr_number_from_url(url: &str) -> u64 {
    url.rsplit_once("/pull/")
        .and_then(|(_, tail)| tail.split(['/', '?', '#']).next())
        .and_then(|number| number.parse().ok())
        .unwrap_or(0)
}

fn repo_label(chat: &Chat, state: &AppState) -> String {
    if let Some(space) = state.space_for_chat(chat) {
        return space.display_name().to_string();
    }
    chat.cwd
        .as_deref()
        .and_then(|cwd| cwd.trim_end_matches('/').rsplit('/').next())
        .filter(|name| !name.is_empty())
        .unwrap_or("No repository")
        .to_string()
}

fn row_matches(row: &TypeChatRow, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let pr = row.pr.as_ref();
    let haystack = format!(
        "{} {} {} {} {} {} {}",
        row.chat.title.as_deref().unwrap_or("New session"),
        row.repo,
        row.chat.branch.as_deref().unwrap_or_default(),
        pr.map(|pr| pr.number.to_string()).unwrap_or_default(),
        pr.and_then(|pr| pr.title.as_deref()).unwrap_or_default(),
        pr.and_then(|pr| pr.state.as_deref()).unwrap_or_default(),
        row.chat.last_message_preview.as_deref().unwrap_or_default(),
    )
    .to_lowercase();
    query.split_whitespace().all(|word| haystack.contains(word))
}

/// Whether the inbox lists `chat` as a launcher review chat: a top-level chat
/// (archived ones only when asked for) that has a PR link whose source is
/// `review_launcher`. `links` is `None` until its link status has loaded.
fn chat_visible(chat: &Chat, links: Option<&ChatLinkStatus>, show_archived: bool) -> bool {
    chat.parent_chat_id.is_none()
        && (show_archived || !chat.archived)
        && links.is_some_and(|status| launcher_link(status).is_some())
}

fn sort_rows(rows: &mut [TypeChatRow]) {
    rows.sort_by(|a, b| {
        let a_updated = a.chat.last_message_at.unwrap_or(a.chat.created_at);
        let b_updated = b.chat.last_message_at.unwrap_or(b.chat.created_at);
        b_updated.cmp(&a_updated).then_with(|| {
            a.chat
                .title
                .as_deref()
                .unwrap_or_default()
                .cmp(b.chat.title.as_deref().unwrap_or_default())
        })
    });
}

fn status_label(status: ChatIndicator) -> &'static str {
    match status {
        ChatIndicator::Working => "Working",
        ChatIndicator::AwaitingInput => "Needs input",
        ChatIndicator::Errored => "Failed",
        ChatIndicator::Completed => "Done",
        ChatIndicator::Idle => "Idle",
    }
}

fn move_selection(active: usize, count: usize, direction: i32) -> usize {
    if count == 0 {
        return 0;
    }
    if direction < 0 {
        active.saturating_sub(1)
    } else {
        (active + 1).min(count - 1)
    }
}

pub(crate) struct ChatTypeView {
    state: Entity<AppState>,
    shell: gpui::WeakEntity<Shell>,
    kind: ChatViewKind,
    search: Entity<ComposerInput>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    show_archived: bool,
    active: usize,
    links: HashMap<String, ChatLinkStatus>,
    /// Bumped whenever `links` changes; part of the row cache key.
    links_rev: u64,
    /// `state_fingerprint` as of the last observed change.
    state_fingerprint: Option<u64>,
    /// The last built list and what it was built from.
    rows_cache: Option<(RowsKey, Rc<Vec<TypeChatRow>>)>,
    links_pending: HashSet<String>,
    /// The visible tab (default "Authored by me").
    tab: PageTab,
    /// Per dash tab (`pr_page::tab_index`): last good rows, fetch state and
    /// filters, kept while another tab is showing.
    dash: [DashTabState; 2],
    /// Per dash tab text filter.
    dash_search: [Entity<ComposerInput>; 2],
    /// "PR URL or owner/repo#number" box.
    review_input: Entity<ComposerInput>,
    /// Inline start state per PR URL, and for the URL box.
    review_status: HashMap<String, ReviewStatus>,
    /// The PR (URL) whose row overflow menu is open.
    review_menu: popover::Popup<String>,
    input_status: Option<ReviewStatus>,
    _state_observation: Subscription,
    _search_events: Subscription,
    _review_input_events: Subscription,
    _dash_search_events: Vec<Subscription>,
    _dash_refresh: gpui::Task<()>,
}

/// A PR review start's inline state on this page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReviewStatus {
    Failed(String),
}

impl ChatTypeView {
    pub(crate) fn new(
        state: Entity<AppState>,
        shell: gpui::WeakEntity<Shell>,
        kind: ChatViewKind,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            ComposerInput::with_context(
                "Search PR review chats…",
                crate::composer::PALETTE_SEARCH_CONTEXT,
                cx,
            )
            .with_single_line()
            .with_accessibility_role(gpui::Role::SearchInput)
        });
        let search_events = cx.subscribe(&search, |this: &mut Self, _, event, cx| match event {
            ComposerInputEvent::Edited => {
                this.active = 0;
                this.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                cx.notify();
            }
            ComposerInputEvent::Submitted => {
                let rows = this.rows(cx);
                if let Some(row) = rows.get(this.active) {
                    this.open_chat(row.chat.id.clone(), cx);
                }
            }
            _ => {}
        });
        let review_input = cx.new(|cx| {
            ComposerInput::with_context(
                "PR URL or owner/repo#number",
                crate::composer::PALETTE_SEARCH_CONTEXT,
                cx,
            )
            .with_single_line()
        });
        let review_input_events =
            cx.subscribe(&review_input, |this: &mut Self, _, event, cx| match event {
                ComposerInputEvent::Submitted => this.submit_review_input(cx),
                ComposerInputEvent::Edited => {
                    if matches!(this.input_status, Some(ReviewStatus::Failed(_))) {
                        this.input_status = None;
                        cx.notify();
                    }
                }
                _ => {}
            });
        let dash_search: [Entity<ComposerInput>; 2] = std::array::from_fn(|_| {
            cx.new(|cx| {
                ComposerInput::with_context(
                    "Filter PRs…",
                    crate::composer::PALETTE_SEARCH_CONTEXT,
                    cx,
                )
                .with_single_line()
                .with_accessibility_role(gpui::Role::SearchInput)
            })
        });
        let dash_search_events = dash_search
            .iter()
            .map(|input| {
                cx.subscribe(input, |this: &mut Self, _, event, cx| match event {
                    ComposerInputEvent::Edited => {
                        this.active = 0;
                        cx.notify();
                    }
                    ComposerInputEvent::Submitted => this.activate_dash_row(cx),
                    _ => {}
                })
            })
            .collect();
        // Refetch the visible tab every minute while the page is on screen.
        let dash_refresh = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(pr_page::DASH_REFRESH).await;
                if this.update(cx, |this, cx| this.dash_tick(cx)).is_err() {
                    break;
                }
            }
        });
        // Repaint only when the chat list, Spaces, Work Outline loads or PR
        // statuses it shows changed (the same fingerprint the Overview uses,
        // plus the PR chips' store).
        let state_observation = cx.observe(&state, |this: &mut Self, state, cx| {
            let fingerprint = Self::state_fingerprint(state.read(cx));
            if this.state_fingerprint != Some(fingerprint) {
                this.state_fingerprint = Some(fingerprint);
                cx.notify();
            }
        });
        Self {
            state,
            shell,
            kind,
            search,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            show_archived: false,
            active: 0,
            links: HashMap::new(),
            links_rev: 0,
            state_fingerprint: None,
            rows_cache: None,
            links_pending: HashSet::new(),
            tab: PageTab::Dash(PrDashTab::Authored),
            dash: Default::default(),
            dash_search,
            review_input,
            review_status: HashMap::new(),
            review_menu: popover::Popup::default(),
            input_status: None,
            _state_observation: state_observation,
            _search_events: search_events,
            _review_input_events: review_input_events,
            _dash_search_events: dash_search_events,
            _dash_refresh: dash_refresh,
        }
    }

    /// Palette "Review PR…": focus the URL box.
    pub(crate) fn focus_review_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(
            &gpui::Focusable::focus_handle(self.review_input.read(cx), cx),
            cx,
        );
    }

    /// Seed a chat's link status as if `CHAT_LINK_STATUS` had answered.
    #[cfg(test)]
    pub(crate) fn set_links_for_test(&mut self, chat_id: &str, status: ChatLinkStatus) {
        self.links.insert(chat_id.to_string(), status);
        self.links_rev += 1;
    }

    /// The chat ids the launcher review chat list shows, in order.
    #[cfg(test)]
    pub(crate) fn listed_chat_ids(&mut self, cx: &mut Context<Self>) -> Vec<String> {
        self.rows(cx)
            .iter()
            .map(|row| row.chat.id.clone())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn review_input(&self) -> Entity<ComposerInput> {
        self.review_input.clone()
    }

    #[cfg(test)]
    pub(crate) fn input_status(&self) -> Option<ReviewStatus> {
        self.input_status.clone()
    }

    #[cfg(test)]
    pub(crate) fn review_status(&self, url: &str) -> Option<ReviewStatus> {
        self.review_status.get(url).cloned()
    }

    /// Start a review of `reference`: opens the Start Chat dialog through the
    /// shell (the new Chat opens once Start is confirmed there; refusals show
    /// in the dialog). `from_input`: the status belongs to the URL box rather
    /// than a row.
    pub(crate) fn start_review(
        &mut self,
        reference: PrReference,
        action: PrReviewAction,
        from_input: bool,
        cx: &mut Context<Self>,
    ) {
        let url = reference.url();
        let Ok(opened) = self.shell.update(cx, |shell, cx| {
            shell.open_start_pr_review(reference, action, None, cx)
        }) else {
            return;
        };
        let status = opened
            .err()
            .map(|notice| ReviewStatus::Failed(notice.to_owned()));
        if from_input {
            if status.is_none() {
                self.review_input
                    .update(cx, |input, cx| input.set_text("", cx));
            }
            self.input_status = status;
        } else {
            match status {
                Some(status) => {
                    self.review_status.insert(url, status);
                }
                None => {
                    self.review_status.remove(&url);
                }
            }
        }
        cx.notify();
    }

    /// The URL box's Enter / Review button.
    pub(crate) fn submit_review_input(&mut self, cx: &mut Context<Self>) {
        let text = self.review_input.read(cx).text().trim().to_owned();
        match PrReference::parse(&text) {
            Some(reference) => self.start_review(reference, PrReviewAction::CodeReview, true, cx),
            None => {
                self.input_status = Some(ReviewStatus::Failed(
                    crate::pr_review_launch::REFERENCE_HINT.to_owned(),
                ));
                cx.notify();
            }
        }
    }

    pub(crate) fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        let input = match self.tab {
            PageTab::Dash(tab) => &self.dash_search[pr_page::tab_index(tab)],
            PageTab::Chats => &self.search,
        };
        window.focus(&gpui::Focusable::focus_handle(input.read(cx), cx), cx);
    }

    pub(crate) fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    fn ensure_links(&mut self, chat_id: String, cx: &mut Context<Self>) {
        if self.links.contains_key(&chat_id) || self.links_pending.contains(&chat_id) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.links_pending.insert(chat_id.clone());
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::CHAT_LINK_STATUS,
                    serde_json::json!({ "chatId": chat_id }),
                )
                .await;
            this.update(cx, |this, cx| {
                this.links_pending.remove(&chat_id);
                if let Ok(value) = result
                    && let Ok(status) = serde_json::from_value::<ChatLinkStatus>(value)
                {
                    crate::review_chats::note_chat_links(
                        cx,
                        &chat_id,
                        &crate::review_chats::links_of_status(&status),
                    );
                    this.links.insert(chat_id.clone(), status);
                    this.links_rev += 1;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// What the observer and the row cache key on: everything the Overview
    /// fingerprint covers (chats, Spaces, Outline loads, selection) plus the
    /// PR status store.
    fn state_fingerprint(state: &AppState) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        state.overview_fingerprint(Utc::now()).hash(&mut h);
        state.github.fingerprint().hash(&mut h);
        h.finish()
    }

    /// The visible chat rows. Cached: rebuilt (cloning every chat) only when
    /// the state fingerprint, the link statuses, the archived toggle or the
    /// search query changed, not on every render or streaming frame.
    fn rows(&mut self, cx: &mut Context<Self>) -> Rc<Vec<TypeChatRow>> {
        let query = self.search.read(cx).text().trim().to_lowercase();
        let key = RowsKey {
            state: Self::state_fingerprint(self.state.read(cx)),
            links_rev: self.links_rev,
            show_archived: self.show_archived,
            query: query.clone(),
        };
        if let Some((cached, rows)) = &self.rows_cache
            && *cached == key
        {
            return rows.clone();
        }
        let rows = Rc::new(self.build_rows(&query, cx));
        self.rows_cache = Some((key, rows.clone()));
        rows
    }

    fn build_rows(&mut self, query: &str, cx: &mut Context<Self>) -> Vec<TypeChatRow> {
        let now = Utc::now();
        let chats: Vec<Chat> = self.state.read(cx).chats.clone();
        let mut rows = Vec::new();
        for chat in chats {
            // Whether a chat is a launcher review chat is only known from its
            // link status, so fetch it for every chat the toggle could list.
            if chat.parent_chat_id.is_some() || (chat.archived && !self.show_archived) {
                continue;
            }
            self.ensure_links(chat.id.clone(), cx);
            if !chat_visible(&chat, self.links.get(&chat.id), self.show_archived) {
                continue;
            }
            let state = self.state.read(cx);
            let row = TypeChatRow {
                status: state.display_status_for(&chat, now),
                repo: repo_label(&chat, state),
                pr: self.links.get(&chat.id).and_then(PrSummary::from_status),
                chat,
            };
            if row_matches(&row, query) {
                rows.push(row);
            }
        }
        sort_rows(&mut rows);
        rows
    }

    fn open_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let _ = self
            .shell
            .update(cx, |shell, cx| shell.open_chat(chat_id, cx));
    }

    fn on_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The URL box owns its keys (Enter starts a review, not a chat).
        if gpui::Focusable::focus_handle(self.review_input.read(cx), cx).is_focused(window) {
            return;
        }
        // Arrow/Enter navigation belongs to the visible list.
        if let PageTab::Dash(tab) = self.tab {
            self.dash_key_down(tab, event, cx);
            return;
        }
        if let Some(view) = self.destination(cx) {
            self.destination_key_down(event, view, cx);
            return;
        }
        let count = self.rows(cx).len();
        match event.keystroke.key.as_str() {
            "down" if count > 0 => {
                self.active = move_selection(self.active, count, 1);
                self.scroll.scroll_to_item(self.active);
                cx.stop_propagation();
                cx.notify();
            }
            "up" if count > 0 => {
                self.active = move_selection(self.active, count, -1);
                self.scroll.scroll_to_item(self.active);
                cx.stop_propagation();
                cx.notify();
            }
            "enter" if count > 0 => {
                let rows = self.rows(cx);
                if let Some(row) = rows.get(self.active) {
                    self.open_chat(row.chat.id.clone(), cx);
                }
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    fn render_row(
        &self,
        row: &TypeChatRow,
        index: usize,
        theme: &Theme,
        now: DateTime<Utc>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let active = index == self.active;
        let chat_id = row.chat.id.clone();
        let title: SharedString = row
            .chat
            .title
            .clone()
            .unwrap_or_else(|| "New session".to_string())
            .into();
        let (pr_number, pr_title, pr_state, branch) = row.pr.as_ref().map_or_else(
            || {
                (
                    "—".to_string(),
                    "No linked PR detail".to_string(),
                    "—".to_string(),
                    row.chat.branch.clone().unwrap_or_else(|| "—".to_string()),
                )
            },
            |pr| {
                (
                    (pr.number > 0)
                        .then(|| format!("#{}", pr.number))
                        .unwrap_or_else(|| "PR".to_string()),
                    pr.title.clone().unwrap_or_else(|| "Linked PR".to_string()),
                    pr.state.clone().unwrap_or_else(|| "linked".to_string()),
                    pr.branch
                        .clone()
                        .or_else(|| row.chat.branch.clone())
                        .unwrap_or_else(|| "—".to_string()),
                )
            },
        );
        let updated = row.chat.last_message_at.unwrap_or(row.chat.created_at);
        div()
            .id(("pr-review-chat-row", index))
            .min_h(px(52.0))
            .grid()
            .grid_cols(7)
            .items_center()
            .gap(px(12.0))
            .px(px(14.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(theme.border.opacity(0.65))
            .cursor_pointer()
            .when(active, |element| element.bg(theme.element_active))
            .hover(|element| element.bg(theme.element_hover))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.active = index;
                this.open_chat(chat_id.clone(), cx);
            }))
            .child(
                div()
                    .col_span(2)
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .truncate()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(title),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(pr_title)),
                    ),
            )
            .child(div().truncate().child(SharedString::from(row.repo.clone())))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.accent)
                            .child(SharedString::from(pr_number)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(crate::typography::ui_rems(10.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(pr_state)),
                    ),
            )
            .child(
                div()
                    .truncate()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(branch)),
            )
            .child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(status_label(row.status))),
            )
            .child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(format_time_ago(updated, now))),
            )
            .into_any_element()
    }
}

fn review_button(
    id: SharedString,
    label: &'static str,
    primary: bool,
    disabled: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let selector = id.clone();
    div()
        .id(gpui::ElementId::Name(id))
        .debug_selector(move || selector.to_string())
        .flex_none()
        .px(px(9.0))
        .py(px(3.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(if primary {
            theme.accent.opacity(0.5)
        } else {
            theme.border
        })
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(if primary {
            theme.accent
        } else {
            theme.text_muted
        })
        .when(disabled, |el| el.opacity(0.5))
        .when(!disabled, |el| {
            el.cursor_pointer().hover(|el| el.bg(theme.element_hover))
        })
        .child(label)
}

/// Title of the failure card a refused or failed review start shows.
pub(crate) const REVIEW_FAILURE_TITLE: &str = "Couldn't start the PR review";

fn status_line(selector: String, status: &ReviewStatus, theme: &Theme) -> gpui::Div {
    // Failures (git stderr and all) get the shared persistent, copyable
    // error card instead of a one-line red caption.
    let ReviewStatus::Failed(text) = status;
    let card_id = SharedString::from(format!("{selector}-error"));
    div()
        .debug_selector(move || selector.clone())
        .w_full()
        .child(crate::error_notice::ErrorNotice::new(
            card_id,
            theme,
            REVIEW_FAILURE_TITLE,
            text.clone(),
        ))
}

impl ChatTypeView {
    /// The URL / `owner/repo#n` box, shown above every tab.
    fn render_review_box(&mut self, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let header = div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .flex_1()
                    .text_size(crate::typography::ui_rems(10.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text_muted)
                    .child("REVIEW ANY PR"),
            )
            .child(
                div()
                    .id("pr-review-url-input")
                    .debug_selector(|| "pr-review-url-input".into())
                    .w(px(280.0))
                    .child(popover::search_input_frame(
                        theme,
                        self.review_input.clone().into_any_element(),
                    )),
            )
            .child(
                review_button("pr-review-url-submit".into(), "Review", true, false, theme)
                    .on_click(cx.listener(|this, _, _, cx| this.submit_review_input(cx))),
            );
        div()
            .id("pr-review-box")
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .px(px(14.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(theme.border)
            .child(header)
            .when_some(self.input_status.as_ref(), |el, status| {
                el.child(status_line("pr-review-url-status".into(), status, theme))
            })
            .into_any_element()
    }
}

impl Render for ChatTypeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let now = Utc::now();
        let on_chats = matches!(self.tab, PageTab::Chats);
        // With a PR review sync destination in the current Space, its PR
        // items replace the launcher review chat list.
        let destination = if on_chats { self.destination(cx) } else { None };
        let rows = if on_chats && destination.is_none() {
            self.rows(cx)
        } else {
            Rc::new(Vec::new())
        };
        let count_label = match &destination {
            Some(view) => format!("{} PRs", view.items.len()),
            None => format!("{} chats", rows.len()),
        };
        let count = match self.tab {
            PageTab::Dash(tab) => self.dash_visible(tab, cx).0.len(),
            PageTab::Chats => destination
                .as_ref()
                .map_or(rows.len(), |view| view.items.len()),
        };
        self.active = self.active.min(count.saturating_sub(1));
        let destination_list =
            destination.map(|view| self.render_destination(view, &theme, now, cx));
        let destination_mode = destination_list.is_some();
        let shortcut = crate::settings::badge_combo(crate::shell::OPEN_PR_REVIEW_CHATS_COMBO);
        let dash_body = match self.tab {
            PageTab::Dash(tab) => Some(self.render_dash(tab, &theme, now, cx)),
            PageTab::Chats => None,
        };

        div()
            .id("chat-type-view")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.surface)
            .child(
                div()
                    .flex_none()
                    .h(px(54.0))
                    .px(px(18.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .text_size(crate::typography::ui_rems(15.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(SharedString::from(self.kind.title())),
                            )
                            .when(on_chats, |el| {
                                el.child(
                                    div()
                                        .text_size(crate::typography::ui_rems(11.0))
                                        .text_color(theme.text_muted)
                                        .child(SharedString::from(count_label)),
                                )
                            })
                            .child(popover::kbd_hint(&theme, &shortcut)),
                    )
                    .when(on_chats, |el| {
                        el.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .child(div().w(px(280.0)).child(popover::search_input_frame(
                                    &theme,
                                    self.search.clone().into_any_element(),
                                )))
                                .child(
                                    div()
                                        .id("chat-type-show-archived")
                                        .debug_selector(|| "chat-type-show-archived".into())
                                        .px(px(9.0))
                                        .py(px(5.0))
                                        .rounded(px(6.0))
                                        .border_1()
                                        .border_color(theme.border)
                                        .cursor_pointer()
                                        .text_size(crate::typography::ui_rems(11.0))
                                        .text_color(if self.show_archived {
                                            theme.text
                                        } else {
                                            theme.text_muted
                                        })
                                        .when(self.show_archived, |element| {
                                            element.bg(theme.element_active)
                                        })
                                        .hover(|element| element.bg(theme.element_hover))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.show_archived = !this.show_archived;
                                            this.active = 0;
                                            cx.notify();
                                        }))
                                        .child("Show archived"),
                                ),
                        )
                    }),
            )
            .child(self.render_review_box(&theme, cx))
            .child(self.render_tab_bar(&theme, cx))
            .when_some(dash_body, |el, body| el.child(body))
            .when_some(destination_list, |el, list| el.child(list))
            .when(on_chats && !destination_mode, |el| {
                el.child(
                    div()
                        .h(px(34.0))
                        .flex_none()
                        .grid()
                        .grid_cols(7)
                        .items_center()
                        .gap(px(12.0))
                        .px(px(14.0))
                        .border_b_1()
                        .border_color(theme.border)
                        .bg(theme.element_hover.opacity(0.45))
                        .text_size(crate::typography::ui_rems(10.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text_muted)
                        .child(div().col_span(2).child("CHAT / PR TITLE"))
                        .child("REPOSITORY")
                        .child("PR")
                        .child("BRANCH")
                        .child("STATUS")
                        .child("UPDATED"),
                )
                .child(
                    div()
                        .id("chat-type-rows")
                        .debug_selector(|| "chat-type-rows".into())
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .when(rows.is_empty(), |element| {
                            element.child(
                                div()
                                    .h_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(self.kind.empty_message())),
                            )
                        })
                        .children(
                            rows.iter()
                                .enumerate()
                                .map(|(index, row)| self.render_row(row, index, &theme, now, cx)),
                        ),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(id: &str, title: &str, updated: i64, archived: bool) -> Chat {
        let at = DateTime::from_timestamp(updated, 0).unwrap();
        Chat {
            id: id.into(),
            device_id: "device".into(),
            title: Some(title.into()),
            archived,
            cwd: Some(format!("/work/{id}")),
            branch: Some(format!("branch-{id}")),
            checkout_id: None,
            source_context: None,
            config: None,
            last_message_preview: None,
            last_message_at: Some(at),
            created_at: at,
            harness_session_id: None,
            harness_session_cwd: None,
            actual_model: None,
            space_id: None,
            last_seen_at: None,
            room_gen: None,
            parent_chat_id: None,
            linked_pr_url: None,
            linked_pr_source: None,
            linked_ticket_id: None,
            linked_ticket_source: None,
        }
    }

    fn row(id: &str, title: &str, updated: i64, number: u64) -> TypeChatRow {
        TypeChatRow {
            chat: chat(id, title, updated, false),
            status: ChatIndicator::Idle,
            repo: format!("repo-{id}"),
            pr: Some(PrSummary {
                number,
                title: Some(format!("Fix {title}")),
                state: Some("open".into()),
                branch: None,
            }),
        }
    }

    /// A link status with one PR link per `(number, source)`.
    fn status_with(links: &[(u64, &str)]) -> ChatLinkStatus {
        serde_json::from_value(serde_json::json!({
            "pr": null,
            "prLinks": links.iter().map(|(number, source)| serde_json::json!({
                "url": format!("https://github.com/acme/app/pull/{number}"),
                "source": source,
                "detail": null
            })).collect::<Vec<_>>(),
            "ticket": null,
            "isWorktree": false,
            "diffStat": null,
            "prSource": null,
            "ticketSource": null
        }))
        .unwrap()
    }

    #[test]
    fn only_chats_with_a_review_launcher_link_are_listed() {
        let chat = chat("c", "Chat", 1, false);
        let launched = status_with(&[(7, "review_launcher")]);
        assert!(chat_visible(&chat, Some(&launched), false));
        // The launcher link may sit next to other links.
        let mixed = status_with(&[(5, "branch"), (7, "review_launcher")]);
        assert!(chat_visible(&chat, Some(&mixed), false));
        // A hand-linked or inferred PR is not a launcher review chat.
        for source in ["manual", "branch", "mentioned", "created_in_chat", "ticket"] {
            let other = status_with(&[(7, source)]);
            assert!(!chat_visible(&chat, Some(&other), false), "{source}");
        }
        assert!(!chat_visible(&chat, Some(&status_with(&[])), false));
        // Not known yet: not listed.
        assert!(!chat_visible(&chat, None, false));
    }

    #[test]
    fn filtering_uses_archived_toggle_and_skips_subagent_chats() {
        let launched = status_with(&[(7, "review_launcher")]);
        let active = chat("active", "Active", 1, false);
        let archived = chat("archived", "Archived", 1, true);
        let mut child = chat("child", "Child", 1, false);
        child.parent_chat_id = Some("active".into());
        assert!(chat_visible(&active, Some(&launched), false));
        assert!(!chat_visible(&archived, Some(&launched), false));
        assert!(chat_visible(&archived, Some(&launched), true));
        assert!(!chat_visible(&child, Some(&launched), true));
    }

    #[gpui::test]
    fn rows_are_cached_until_something_they_show_changes(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| {
            state.chats = vec![chat("a", "Alpha", 2, false), chat("b", "Beta", 1, false)];
        });
        let (view, vcx) = cx.add_window_view(|_, cx| {
            ChatTypeView::new(
                state.clone(),
                gpui::WeakEntity::new_invalid(),
                ChatViewKind::PrReview,
                cx,
            )
        });
        let launched = status_with(&[(7, "review_launcher")]);
        view.update(vcx, |view, _| {
            view.set_links_for_test("a", launched.clone());
            view.set_links_for_test("b", launched.clone());
        });
        let first = view.update(vcx, |view, cx| view.rows(cx));
        assert_eq!(first.len(), 2);
        let again = view.update(vcx, |view, cx| view.rows(cx));
        assert!(
            Rc::ptr_eq(&first, &again),
            "unchanged inputs reuse the list"
        );

        // Streaming into the open chat touches nothing the list shows.
        state.update(vcx, |state, cx| {
            state.transcript_revision += 1;
            cx.notify();
        });
        let streamed = view.update(vcx, |view, cx| view.rows(cx));
        assert!(Rc::ptr_eq(&first, &streamed));

        // A visible change (a title) rebuilds it.
        state.update(vcx, |state, cx| {
            state.chats[0].title = Some("Renamed".into());
            cx.notify();
        });
        let renamed = view.update(vcx, |view, cx| view.rows(cx));
        assert!(!Rc::ptr_eq(&first, &renamed));
        assert!(
            renamed
                .iter()
                .any(|row| row.chat.title.as_deref() == Some("Renamed"))
        );

        // So does the archived toggle.
        view.update(vcx, |view, _| view.show_archived = true);
        let toggled = view.update(vcx, |view, cx| view.rows(cx));
        assert!(!Rc::ptr_eq(&renamed, &toggled));
    }

    #[test]
    fn the_listed_pr_is_the_launched_one_even_when_another_link_comes_first() {
        let status = status_with(&[(5, "branch"), (7, "review_launcher")]);
        assert_eq!(PrSummary::from_status(&status).unwrap().number, 7);
        let no_launcher = status_with(&[(5, "branch"), (6, "manual")]);
        assert_eq!(PrSummary::from_status(&no_launcher).unwrap().number, 5);
    }

    #[test]
    fn search_covers_chat_repo_pr_and_branch_fields() {
        let row = row("alpha", "Review cache race", 2, 418);
        assert!(row_matches(&row, "cache"));
        assert!(row_matches(&row, "repo-alpha 418"));
        assert!(row_matches(&row, "branch-alpha"));
        assert!(!row_matches(&row, "unrelated"));
    }

    #[test]
    fn inbox_order_is_most_recent_first_with_stable_title_tie_break() {
        let mut rows = vec![
            row("older", "Zulu", 1, 1),
            row("b", "Beta", 2, 2),
            row("a", "Alpha", 2, 3),
        ];
        sort_rows(&mut rows);
        assert_eq!(
            rows.iter()
                .map(|row| row.chat.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "older"]
        );
    }

    #[test]
    fn linked_pr_number_falls_back_to_url_while_detail_loads() {
        let status: ChatLinkStatus = serde_json::from_value(serde_json::json!({
            "pr": null,
            "prLinks": [{
                "url": "https://github.com/acme/app/pull/77",
                "source": "manual",
                "detail": null
            }],
            "ticket": null,
            "isWorktree": false,
            "diffStat": null,
            "prSource": null,
            "ticketSource": null
        }))
        .unwrap();
        assert_eq!(PrSummary::from_status(&status).unwrap().number, 77);
    }

    #[test]
    fn keyboard_selection_clamps_at_both_ends() {
        assert_eq!(move_selection(0, 3, -1), 0);
        assert_eq!(move_selection(0, 3, 1), 1);
        assert_eq!(move_selection(2, 3, 1), 2);
        assert_eq!(move_selection(4, 0, 1), 0);
    }
}
