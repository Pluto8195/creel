//! Which chats are a PR's review chats, in one place.
//!
//! A PR's review chat is any live (not archived) chat that is
//!
//! - assigned to the PR's review work item (Start Chat on a synced PR item), or
//! - linked to the PR with source `manual` or `review_launcher`.
//!
//! Chats linked only by mention / branch / ticket / diff target / created in
//! chat stay plain linked-chat chips. A finished or idle review chat still
//! counts: the PR page keeps offering "Open review" so it can be reopened for
//! a re-review.
//!
//! [`resolve`] is the pure rule. The PR page rows, the PR rows of the Outline
//! and the work-context strip all call it, so they never disagree about what
//! is being reviewed. Facts about the chats come from the caller;
//! [`assigned_chat_ids`] reads the assignments from a Work Outline snapshot and
//! [`PrChatLinkIndex`] remembers the links the UI has already seen.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use zeron_proto::{ChatIndicator, ChatLinkSource, PullRequestRef, WorkOutlineSnapshot};

/// Why a chat counts as a review chat. Assignment wins when both apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewReason {
    Assigned,
    Linked(ChatLinkSource),
}

/// What the caller knows about one chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatFact {
    pub id: String,
    pub title: String,
    pub archived: bool,
    pub updated: Option<DateTime<Utc>>,
    pub state: ChatIndicator,
}

/// A chat that reviews the PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewChat {
    pub chat_id: String,
    pub title: String,
    pub state: ChatIndicator,
    pub updated: Option<DateTime<Utc>>,
    pub reason: ReviewReason,
}

/// The link sources that make a chat the PR's review chat.
pub(crate) fn link_makes_review(source: ChatLinkSource) -> bool {
    matches!(
        source,
        ChatLinkSource::Manual | ChatLinkSource::ReviewLauncher
    )
}

/// The review chats of one PR, most recently updated first (ties: chat id).
/// `assigned` are the chats assigned to the PR's work item(s), `links` the
/// chats linked to the PR with their source; `facts` describe the chats. A
/// chat without a fact, or an archived one, never counts.
pub(crate) fn resolve(
    assigned: &[String],
    links: &[(String, ChatLinkSource)],
    facts: &[ChatFact],
) -> Vec<ReviewChat> {
    let mut reasons: HashMap<&str, ReviewReason> = HashMap::new();
    for (chat_id, source) in links {
        if link_makes_review(*source) {
            reasons
                .entry(chat_id.as_str())
                .or_insert(ReviewReason::Linked(*source));
        }
    }
    for chat_id in assigned {
        reasons.insert(chat_id.as_str(), ReviewReason::Assigned);
    }
    let mut out: Vec<ReviewChat> = Vec::new();
    for fact in facts {
        if fact.archived || out.iter().any(|chat| chat.chat_id == fact.id) {
            continue;
        }
        let Some(reason) = reasons.get(fact.id.as_str()) else {
            continue;
        };
        out.push(ReviewChat {
            chat_id: fact.id.clone(),
            title: display_title(&fact.title),
            state: fact.state,
            updated: fact.updated,
            reason: *reason,
        });
    }
    out.sort_by(|a, b| {
        b.updated
            .cmp(&a.updated)
            .then_with(|| a.chat_id.cmp(&b.chat_id))
    });
    out
}

fn display_title(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        "Untitled chat".to_owned()
    } else {
        title.to_owned()
    }
}

/// The chats assigned to a review item of `pr` in `snapshot`: items that link
/// the PR and are review items (see `chat_work_context::is_review_item`).
/// Archived items count, because merged and closed PRs archive their item
/// while the review chat stays open.
pub(crate) fn assigned_chat_ids(
    snapshot: &WorkOutlineSnapshot,
    pr: &PullRequestRef,
) -> Vec<String> {
    let key = pr.key();
    let mut ids: Vec<String> = Vec::new();
    for item in &snapshot.items {
        let links_pr = crate::github::item_pull_requests(snapshot, &item.id)
            .iter()
            .any(|linked| linked.key() == key);
        if links_pr && crate::chat_work_context::is_review_item(snapshot, item, pr) {
            for chat_id in &item.chat_ids {
                if !ids.contains(chat_id) {
                    ids.push(chat_id.clone());
                }
            }
        }
    }
    ids
}

/// The chat the primary action opens: the most recent review chat.
pub(crate) fn primary(chats: &[ReviewChat]) -> Option<&ReviewChat> {
    chats.first()
}

/// The review chats the menu lists besides the primary one.
pub(crate) fn others(chats: &[ReviewChat]) -> &[ReviewChat] {
    chats.get(1..).unwrap_or(&[])
}

/// What Enter does on a PR row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnterAction {
    OpenReview(String),
    StartReview,
}

pub(crate) fn enter_action(chats: &[ReviewChat]) -> EnterAction {
    match primary(chats) {
        Some(chat) => EnterAction::OpenReview(chat.chat_id.clone()),
        None => EnterAction::StartReview,
    }
}

/// `working`, `idle`, `awaiting input`, `finished` (or `errored`).
pub(crate) fn state_label(state: ChatIndicator) -> &'static str {
    match state {
        ChatIndicator::Working => "working",
        ChatIndicator::AwaitingInput => "awaiting input",
        ChatIndicator::Errored => "errored",
        ChatIndicator::Completed => "finished",
        ChatIndicator::Idle => "idle",
    }
}

/// The "being reviewed" marker text shared by the Outline row and the strip.
pub(crate) fn marker_label(chats: &[ReviewChat]) -> Option<String> {
    let chat = primary(chats)?;
    let extra = chats.len() - 1;
    let more = if extra > 0 {
        format!(" +{extra}")
    } else {
        String::new()
    };
    Some(format!(
        "Being reviewed · {} · {}{more}",
        chat.title,
        state_label(chat.state)
    ))
}

/// The primary button's label: the chat title and its live state.
pub(crate) fn open_review_detail(chat: &ReviewChat) -> String {
    format!("{} · {}", chat.title, state_label(chat.state))
}

/// What the UI has learned about chat → PR links, keyed by `PullRequestRef::key`.
/// Each source is the strongest known for that chat and PR. Fed by the PR
/// page's fetches and by the chat link status replies; read by the Outline
/// and strip, which have no per-PR chat list of their own.
#[derive(Default)]
pub(crate) struct PrChatLinkIndex {
    by_pr: HashMap<String, HashMap<String, ChatLinkSource>>,
}

impl gpui::Global for PrChatLinkIndex {}

impl PrChatLinkIndex {
    fn key_of(url: &str) -> Option<String> {
        crate::chat_work_context::parse_pr_url(url).map(|pr| pr.key())
    }

    /// Replace everything known about `pr_url`'s linked chats.
    pub(crate) fn set_pr(&mut self, pr_url: &str, links: &[(String, ChatLinkSource)]) {
        let Some(key) = Self::key_of(pr_url) else {
            return;
        };
        let mut chats: HashMap<String, ChatLinkSource> = HashMap::new();
        for (chat_id, source) in links {
            strongest(&mut chats, chat_id, *source);
        }
        if chats.is_empty() {
            self.by_pr.remove(&key);
        } else {
            self.by_pr.insert(key, chats);
        }
    }

    /// Replace everything known about `chat_id`'s PR links.
    pub(crate) fn set_chat(&mut self, chat_id: &str, links: &[(String, ChatLinkSource)]) {
        self.by_pr.retain(|_, chats| {
            chats.remove(chat_id);
            !chats.is_empty()
        });
        for (url, source) in links {
            if let Some(key) = Self::key_of(url) {
                strongest(self.by_pr.entry(key).or_default(), chat_id, *source);
            }
        }
    }

    pub(crate) fn links(&self, pr: &PullRequestRef) -> Vec<(String, ChatLinkSource)> {
        self.by_pr
            .get(&pr.key())
            .map(|chats| {
                chats
                    .iter()
                    .map(|(chat_id, source)| (chat_id.clone(), *source))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn strongest(chats: &mut HashMap<String, ChatLinkSource>, chat_id: &str, source: ChatLinkSource) {
    let slot = chats.entry(chat_id.to_owned()).or_insert(source);
    if source.rank() > slot.rank() {
        *slot = source;
    }
}

/// Record the PR links of `chat_id` (a `CHAT_LINK_STATUS` reply).
pub(crate) fn note_chat_links(
    cx: &mut gpui::App,
    chat_id: &str,
    links: &[(String, ChatLinkSource)],
) {
    cx.default_global::<PrChatLinkIndex>().set_chat(chat_id, links);
}

/// Record the chats the PR page's fetch linked to `pr_url`.
pub(crate) fn note_pr_links(cx: &mut gpui::App, pr_url: &str, links: &[(String, ChatLinkSource)]) {
    cx.default_global::<PrChatLinkIndex>().set_pr(pr_url, links);
}

/// The review chats of `pr` from everything the UI knows: `extra_links`
/// (the caller's own list, e.g. the page row's chats), the link index, the
/// snapshot's assignments, and the chats' facts.
pub(crate) fn for_pr(
    cx: &gpui::App,
    snapshot: Option<&WorkOutlineSnapshot>,
    pr: &PullRequestRef,
    extra_links: &[(String, ChatLinkSource)],
    facts: &[ChatFact],
) -> Vec<ReviewChat> {
    let mut links: Vec<(String, ChatLinkSource)> = extra_links.to_vec();
    if let Some(index) = cx.try_global::<PrChatLinkIndex>() {
        links.extend(index.links(pr));
    }
    let assigned = snapshot
        .map(|snapshot| assigned_chat_ids(snapshot, pr))
        .unwrap_or_default();
    resolve(&assigned, &links, facts)
}

/// Facts for every chat the app state knows.
pub(crate) fn facts_from_state(state: &crate::state::AppState) -> Vec<ChatFact> {
    let now = Utc::now();
    state
        .chats
        .iter()
        .map(|chat| ChatFact {
            id: chat.id.clone(),
            title: chat.title.clone().unwrap_or_default(),
            archived: chat.archived,
            updated: Some(chat.last_message_at.unwrap_or(chat.created_at)),
            state: state.display_status_for(chat, now),
        })
        .collect()
}

#[cfg(test)]
mod tests;
