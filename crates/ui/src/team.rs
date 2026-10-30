//! What the side panel's "Team" tab (and the Item tab's roll-up) shows about
//! the workers of an orchestrating chat: one pure fact sheet ([`TeamFacts`])
//! built from snapshots. Nothing here fetches; activity lines come from the
//! chat's last message preview (the transcript tail the engine already keeps),
//! PR checks/review and diff stats from the existing link-status cache.
//!
//! A *worker* is any other chat that is a child chat of the selected chat
//! (`parent_chat_id`) or that is assigned to a descendant of the selected
//! chat's work item.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use mast_engine::pr_ticket_cache::{ChatLinkStatus, ChecksStatus, PrStatus};
use mast_proto::{Chat, ChatIndicator, WorkItemEffectiveStatus, WorkOutlineSnapshot};

use crate::chat_metadata::HeaderTicket;
use crate::status_light::is_ready_to_merge;

/// Longest activity line, in characters.
pub(crate) const ACTIVITY_MAX_CHARS: usize = 110;
/// Workers the Item tab lists before pointing at the Team tab.
pub(crate) const ITEM_TAB_WORKERS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkerState {
    /// Needs an answer from the user (awaiting input, or an open ask).
    Waiting,
    Errored,
    Working,
    Idle,
    /// Its item is completed or the chat is archived.
    Done,
}

impl WorkerState {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Errored => "errored",
            Self::Working => "working",
            Self::Idle => "idle",
            Self::Done => "done",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkerPr {
    pub number: u64,
    pub url: String,
    pub checks: Option<ChecksStatus>,
    /// "approved" / "changes requested" / "review required", when known.
    pub review: Option<&'static str>,
    pub draft: bool,
    pub state: String,
}

impl WorkerPr {
    fn of(number: u64, url: String, detail: Option<&PrStatus>) -> Self {
        Self {
            number,
            url,
            checks: detail.and_then(|pr| pr.checks),
            review: detail.and_then(|pr| match pr.review_decision.as_deref() {
                Some("APPROVED") => Some("approved"),
                Some("CHANGES_REQUESTED") => Some("changes requested"),
                Some("REVIEW_REQUIRED") => Some("review required"),
                _ => None,
            }),
            draft: detail.is_some_and(|pr| pr.is_draft),
            state: detail.map(|pr| pr.state.clone()).unwrap_or_default(),
        }
    }

    /// "PR #12 · checks passing · approved".
    pub(crate) fn label(&self) -> String {
        let mut out = if self.number > 0 {
            format!("PR #{}", self.number)
        } else {
            "PR".to_owned()
        };
        if self.draft {
            out.push_str(" \u{b7} draft");
        }
        match self.checks {
            Some(ChecksStatus::Passing) => out.push_str(" \u{b7} checks passing"),
            Some(ChecksStatus::Pending) => out.push_str(" \u{b7} checks running"),
            Some(ChecksStatus::Failing) => out.push_str(" \u{b7} checks failing"),
            None => {}
        }
        if let Some(review) = self.review {
            out.push_str(" \u{b7} ");
            out.push_str(review);
        }
        out
    }
}

/// An open "Ready to merge: <branch>" request a worker raised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadyAction {
    pub space_id: String,
    pub item_id: String,
    pub request_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkerFacts {
    pub chat_id: String,
    /// The worker's item title, else its chat title.
    pub title: String,
    pub state: WorkerState,
    pub last_active: Option<DateTime<Utc>>,
    /// One line: the last message's first line, clipped.
    pub activity: Option<String>,
    pub branch: Option<String>,
    pub prs: Vec<WorkerPr>,
    pub ticket: Option<HeaderTicket>,
    /// `(added, removed)` lines from the worker checkout's diff stat.
    pub diff: Option<(u32, u32)>,
    pub ready: Option<ReadyAction>,
}

impl WorkerFacts {
    /// Sort rank: waiting on you, ready to merge, errored, working, idle, done.
    fn rank(&self) -> u8 {
        match (self.state, self.ready.is_some()) {
            (WorkerState::Waiting, _) => 0,
            (WorkerState::Done, _) => 5,
            (_, true) => 1,
            (WorkerState::Errored, _) => 2,
            (WorkerState::Working, _) => 3,
            (WorkerState::Idle, _) => 4,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TeamFacts {
    pub workers: Vec<WorkerFacts>,
}

impl TeamFacts {
    pub(crate) fn is_empty(&self) -> bool {
        self.workers.is_empty()
    }

    fn count(&self, pred: impl Fn(&WorkerFacts) -> bool) -> usize {
        self.workers.iter().filter(|w| pred(w)).count()
    }

    pub(crate) fn working(&self) -> usize {
        self.count(|w| w.state == WorkerState::Working)
    }

    pub(crate) fn waiting(&self) -> usize {
        self.count(|w| w.state == WorkerState::Waiting)
    }

    pub(crate) fn ready(&self) -> usize {
        self.count(|w| w.ready.is_some() && w.state != WorkerState::Done)
    }

    /// "4 working · 1 waiting on you · 2 ready to merge" (parts at zero are
    /// left out; "No active workers" when none shows).
    pub(crate) fn roll_up(&self) -> String {
        let mut parts = Vec::new();
        let working = self.working();
        if working > 0 {
            parts.push(format!("{working} working"));
        }
        let waiting = self.waiting();
        if waiting > 0 {
            parts.push(format!("{waiting} waiting on you"));
        }
        let ready = self.ready();
        if ready > 0 {
            parts.push(format!("{ready} ready to merge"));
        }
        let errored = self.count(|w| w.state == WorkerState::Errored);
        if errored > 0 {
            parts.push(format!("{errored} errored"));
        }
        if parts.is_empty() {
            let done = self.count(|w| w.state == WorkerState::Done);
            return if done == self.workers.len() {
                format!("{done} done")
            } else {
                "No active workers".to_owned()
            };
        }
        parts.join(" \u{b7} ")
    }
}

/// Everything [`team`] reads.
pub(crate) struct TeamInput<'a> {
    pub chat_id: &'a str,
    pub chats: &'a [Chat],
    pub indicators: &'a HashMap<String, ChatIndicator>,
    pub snapshot: Option<&'a WorkOutlineSnapshot>,
    pub links: &'a HashMap<String, ChatLinkStatus>,
}

/// One line from a message preview: first non-empty line, whitespace
/// collapsed, clipped with an ellipsis.
pub(crate) fn activity_line(preview: &str) -> Option<String> {
    activity_line_max(preview, ACTIVITY_MAX_CHARS)
}

/// [`activity_line`] with an explicit length.
pub(crate) fn activity_line_max(preview: &str, max: usize) -> Option<String> {
    let line = preview.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = line.chars().take(max).collect();
    if line.chars().count() > max {
        out.push('\u{2026}');
    }
    Some(out)
}

pub(crate) fn team(input: &TeamInput<'_>) -> TeamFacts {
    let Some(me) = input.chats.iter().find(|c| c.id == input.chat_id) else {
        return TeamFacts::default();
    };
    // Items below the chat's own item.
    let below: HashSet<String> = input
        .snapshot
        .and_then(|snapshot| {
            let item = crate::chat_work_context::assigned_item(snapshot, input.chat_id)?;
            Some(crate::work_outline::model::descendants(snapshot, item))
        })
        .unwrap_or_default();
    let mut workers = Vec::new();
    for chat in input.chats {
        if chat.id == input.chat_id {
            continue;
        }
        let item_id = input
            .snapshot
            .filter(|_| chat.space_id == me.space_id)
            .and_then(|snapshot| crate::chat_work_context::assigned_item(snapshot, &chat.id));
        let under_item = item_id.is_some_and(|id| below.contains(id));
        let child_chat = chat.parent_chat_id.as_deref() == Some(input.chat_id);
        if !(under_item || child_chat) {
            continue;
        }
        workers.push(worker(chat, item_id, input));
    }
    workers.sort_by(|a, b| {
        (a.rank(), std::cmp::Reverse(a.last_active), &a.chat_id).cmp(&(
            b.rank(),
            std::cmp::Reverse(b.last_active),
            &b.chat_id,
        ))
    });
    TeamFacts { workers }
}

fn worker(chat: &Chat, item_id: Option<&str>, input: &TeamInput<'_>) -> WorkerFacts {
    let snapshot = input.snapshot;
    let item = item_id.and_then(|id| snapshot.and_then(|s| s.item(id)));
    let mut ready = None;
    let mut asks = false;
    if let Some(snapshot) = snapshot {
        for request in snapshot
            .user_action_requests
            .iter()
            .filter(|r| r.is_open() && !r.deduped && r.chat_id == chat.id)
        {
            if is_ready_to_merge(&request.text) {
                ready.get_or_insert_with(|| ReadyAction {
                    space_id: snapshot.space_id.clone(),
                    item_id: request.work_item_id.clone(),
                    request_id: request.id.clone(),
                    text: request.text.clone(),
                });
            } else if !request
                .dedupe_key
                .as_deref()
                .is_some_and(|key| key.ends_with(":rollup"))
            {
                asks = true;
            }
        }
    }
    let indicator = input
        .indicators
        .get(&chat.id)
        .copied()
        .unwrap_or(ChatIndicator::Idle);
    let item_done = item.is_some_and(|i| matches!(i.effective_status, WorkItemEffectiveStatus::Completed));
    let state = if item_done || chat.archived {
        WorkerState::Done
    } else if asks || indicator == ChatIndicator::AwaitingInput {
        WorkerState::Waiting
    } else {
        match indicator {
            ChatIndicator::Working => WorkerState::Working,
            ChatIndicator::Errored => WorkerState::Errored,
            _ => WorkerState::Idle,
        }
    };
    let status = input.links.get(&chat.id);
    let mut prs: Vec<WorkerPr> = status
        .map(|s| {
            if s.pr_links.is_empty() {
                s.pr.iter()
                    .map(|pr| WorkerPr::of(pr.number, pr.url.clone().unwrap_or_default(), Some(pr)))
                    .collect()
            } else {
                s.pr_links
                    .iter()
                    .map(|l| {
                        let number = l.detail.as_ref().map_or(0, |d| d.number);
                        WorkerPr::of(number, l.url.clone(), l.detail.as_ref())
                    })
                    .collect()
            }
        })
        .unwrap_or_default();
    if prs.is_empty()
        && let Some(url) = chat.linked_pr_url.clone()
    {
        prs.push(WorkerPr::of(0, url, None));
    }
    let ticket = status
        .and_then(|s| s.ticket.as_ref())
        .map(|t| HeaderTicket {
            identifier: t.identifier.clone(),
            title: t.title.clone(),
            url: t.url.clone(),
            state: t.status.clone(),
        })
        .or_else(|| {
            let snapshot = snapshot?;
            let found = crate::chat_work_context::ticket(snapshot, item_id?)?;
            Some(HeaderTicket {
                identifier: found.identifier,
                title: None,
                url: Some(found.url),
                state: None,
            })
        });
    let title = item
        .map(|i| i.title.trim())
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .or_else(|| chat.title.as_deref().map(crate::transcript::single_line))
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "New session".to_owned());
    WorkerFacts {
        chat_id: chat.id.clone(),
        title,
        state,
        last_active: chat.last_message_at.or(Some(chat.created_at)),
        activity: chat.last_message_preview.as_deref().and_then(activity_line),
        branch: chat
            .branch
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .map(str::to_owned),
        prs,
        ticket,
        diff: status
            .and_then(|s| s.diff_stat.as_ref())
            .map(|d| (d.lines_added, d.lines_removed)),
        ready,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts_of(states: &[(WorkerState, bool)]) -> TeamFacts {
        TeamFacts {
            workers: states
                .iter()
                .enumerate()
                .map(|(i, (state, ready))| WorkerFacts {
                    chat_id: format!("c{i}"),
                    title: format!("w{i}"),
                    state: *state,
                    last_active: None,
                    activity: None,
                    branch: None,
                    prs: Vec::new(),
                    ticket: None,
                    diff: None,
                    ready: ready.then(|| ReadyAction {
                        space_id: "s".into(),
                        item_id: "i".into(),
                        request_id: "r".into(),
                        text: "Ready to merge: b".into(),
                    }),
                })
                .collect(),
        }
    }

    #[test]
    fn the_roll_up_counts_and_skips_zero_parts() {
        use WorkerState::*;
        let team = facts_of(&[
            (Working, false),
            (Working, false),
            (Working, false),
            (Working, false),
            (Waiting, false),
            (Idle, true),
            (Idle, true),
            (Done, true),
        ]);
        assert_eq!(team.roll_up(), "4 working \u{b7} 1 waiting on you \u{b7} 2 ready to merge");
        assert_eq!(facts_of(&[(Done, false)]).roll_up(), "1 done");
        assert_eq!(facts_of(&[(Idle, false)]).roll_up(), "No active workers");
    }

    #[test]
    fn activity_is_the_first_line_collapsed_and_clipped() {
        assert_eq!(activity_line("  \n Running   cargo test\nmore").as_deref(), Some("Running cargo test"));
        assert_eq!(activity_line("   "), None);
        let long = activity_line(&"x".repeat(300)).unwrap();
        assert_eq!(long.chars().count(), ACTIVITY_MAX_CHARS + 1);
        assert!(long.ends_with('\u{2026}'));
    }

    #[test]
    fn pr_chips_carry_checks_and_review() {
        let pr = WorkerPr {
            number: 12,
            url: "u".into(),
            checks: Some(ChecksStatus::Passing),
            review: Some("approved"),
            draft: false,
            state: "OPEN".into(),
        };
        assert_eq!(pr.label(), "PR #12 \u{b7} checks passing \u{b7} approved");
    }
}
