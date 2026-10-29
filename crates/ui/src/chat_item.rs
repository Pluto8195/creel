//! What the chat header, its hover card and the side panel's "Item" tab show
//! about the selected Chat: one pure fact sheet ([`ChatItemFacts`]) built from
//! snapshots, so every surface reads the same numbers and a change elsewhere in
//! the state is a plain `==` away from "nothing to repaint".
//!
//! The Shell hosts (`shell/chat_item_ui.rs`) gather the snapshots and render;
//! nothing here fetches.

use mast_proto::{
    ArtifactsSnapshot, Chat, ChatModelDisplay, WorkItemEffectiveStatus, WorkOutlineSnapshot,
};

use crate::chat_metadata::{HeaderLinks, HeaderPr, HeaderTicket};
use crate::work_outline::instructions;

/// Dedupe-key prefix of the Required Actions plan questions raise
/// (`artq:<artifactId>:<Q-n>` / `…:rollup`).
const PLAN_QUESTION_PREFIX: &str = "artq:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelFacts {
    /// The model the chat ran on, else the configured one; `None` = harness
    /// default.
    pub label: Option<String>,
    /// The stored pick when it differs from what ran.
    pub configured: Option<String>,
    /// `label` is what the harness reported (not just the configured pick).
    pub ran: bool,
    pub tooltip: String,
}

impl ModelFacts {
    fn of(chat: &Chat) -> Self {
        let (label, tooltip) = crate::chat_metadata::chat_model_summary(chat);
        let (configured, ran) = match chat.model_display() {
            ChatModelDisplay::Actual { configured, .. } => {
                (configured.map(|model| model.to_string()), true)
            }
            _ => (None, false),
        };
        Self {
            label,
            configured,
            ran,
            tooltip,
        }
    }

    /// "Opus 5.5" / "default".
    pub(crate) fn chip(&self) -> String {
        format!("Model {}", self.label.as_deref().unwrap_or("default"))
    }

    /// "ran · configured Fable 5.1" style second line of the Item tab.
    pub(crate) fn detail(&self) -> String {
        match (&self.configured, self.ran) {
            (Some(configured), true) => format!("ran \u{b7} configured {configured}"),
            (None, true) => "ran".to_owned(),
            (_, false) if self.label.is_some() => "configured; nothing reported yet".to_owned(),
            _ => "harness default; nothing reported yet".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathSegment {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstructionFacts {
    pub drift: bool,
    /// "Selected r… · Delivered r…" when delivery differs from the selection.
    pub delivery: Option<String>,
    /// Live instructions on the item itself.
    pub own: usize,
    /// Live instructions inherited from ancestors / the project.
    pub inherited: usize,
    pub has_state: bool,
    /// Instruction proposals this chat made that still await review.
    pub proposals: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionFacts {
    pub request_id: String,
    pub text: String,
    /// Raised by a plan question (the ● badge) rather than an agent ask.
    pub plan_question: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanFacts {
    pub artifact_id: String,
    pub title: String,
    pub revision: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChildFacts {
    pub id: String,
    pub title: String,
    pub done: bool,
}

/// The part of the sheet that exists only for a Chat on a work item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ItemSection {
    pub space_id: String,
    pub item_id: String,
    /// Root first, the item last.
    pub path: Vec<PathSegment>,
    pub instructions: InstructionFacts,
    pub actions: Vec<ActionFacts>,
    pub plan: Option<PlanFacts>,
    pub children: Vec<ChildFacts>,
}

impl ItemSection {
    /// Open asks that are not plan questions (the ⚠ badge, with drift).
    pub(crate) fn attention_count(&self) -> usize {
        self.actions.iter().filter(|a| !a.plan_question).count()
            + usize::from(self.instructions.drift)
    }

    /// Open plan questions (the ● badge).
    pub(crate) fn question_count(&self) -> usize {
        self.actions.iter().filter(|a| a.plan_question).count()
    }

    /// The last `n` path titles (the header breadcrumb), with a leading
    /// ellipsis flag when something was trimmed.
    pub(crate) fn trimmed_path(&self, n: usize) -> (bool, &[PathSegment]) {
        let skip = self.path.len().saturating_sub(n);
        (skip > 0, &self.path[skip..])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatItemFacts {
    pub chat_id: String,
    pub title: String,
    pub model: ModelFacts,
    pub account: Option<String>,
    /// Where the chat runs (its working directory, else the project folder).
    pub runs_in: Option<String>,
    pub branch: Option<String>,
    pub prs: Vec<HeaderPr>,
    pub ticket: Option<HeaderTicket>,
    pub item: Option<ItemSection>,
}

/// Everything [`facts`] reads.
pub(crate) struct FactsInput<'a> {
    pub chat: &'a Chat,
    pub space_path: Option<&'a str>,
    pub account: Option<String>,
    pub links: Option<&'a HeaderLinks>,
    pub snapshot: Option<&'a WorkOutlineSnapshot>,
    pub artifacts: Option<&'a ArtifactsSnapshot>,
}

pub(crate) fn facts(input: &FactsInput<'_>) -> ChatItemFacts {
    let chat = input.chat;
    let title = chat
        .title
        .as_deref()
        .map(crate::transcript::single_line)
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "New session".to_owned());
    let branch = chat
        .source_context
        .as_ref()
        .map(|context| context.branch.as_str())
        .or(chat.branch.as_deref())
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
        .map(str::to_owned);
    let runs_in = chat
        .cwd
        .as_deref()
        .or(input.space_path)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_owned);
    let mut prs = input.links.map(|l| l.prs.clone()).unwrap_or_default();
    let mut ticket = input.links.and_then(|l| l.ticket.clone());
    let item = input.snapshot.and_then(|snapshot| {
        let item_id = crate::chat_work_context::assigned_item(snapshot, &chat.id)?;
        let section = item_section(snapshot, input.artifacts, &chat.id, item_id)?;
        if ticket.is_none()
            && let Some(found) = crate::chat_work_context::ticket(snapshot, item_id)
        {
            ticket = Some(HeaderTicket {
                identifier: found.identifier,
                title: None,
                url: Some(found.url),
            });
        }
        if prs.is_empty() {
            for pr in crate::github::item_pull_requests(snapshot, item_id) {
                prs.push(HeaderPr {
                    url: pr.url.clone(),
                    number: pr.number,
                    title: None,
                    source: None,
                });
            }
        }
        Some(section)
    });
    ChatItemFacts {
        chat_id: chat.id.clone(),
        title,
        model: ModelFacts::of(chat),
        account: input.account.clone(),
        runs_in,
        branch,
        prs,
        ticket,
        item,
    }
}

fn item_section(
    snapshot: &WorkOutlineSnapshot,
    artifacts: Option<&ArtifactsSnapshot>,
    chat_id: &str,
    item_id: &str,
) -> Option<ItemSection> {
    let item = snapshot.item(item_id)?;
    let path = crate::work_outline::model::item_path(snapshot, item_id)
        .iter()
        .map(|item| PathSegment {
            id: item.id.clone(),
            title: if item.title.trim().is_empty() {
                "Untitled".to_owned()
            } else {
                item.title.clone()
            },
        })
        .collect();
    let state = snapshot.chat_instruction_state(chat_id);
    let own = instructions::live_instructions(snapshot, Some(item_id)).len();
    let inherited = if item.inherit_parent_instructions {
        crate::chat_work_context::lineage(snapshot, item_id)
            .iter()
            .skip(1)
            .map(|ancestor| instructions::live_instructions(snapshot, Some(&ancestor.id)).len())
            .sum::<usize>()
            + instructions::live_instructions(snapshot, None).len()
    } else {
        0
    };
    let actions = snapshot
        .user_action_requests
        .iter()
        .filter(|request| request.is_open() && !request.deduped && request.work_item_id == item_id)
        .map(|request| ActionFacts {
            request_id: request.id.clone(),
            text: request.text.clone(),
            plan_question: request
                .dedupe_key
                .as_deref()
                .is_some_and(|key| key.starts_with(PLAN_QUESTION_PREFIX)),
        })
        .collect();
    let plan = artifacts.and_then(|artifacts| {
        crate::chat_work_context::lineage(snapshot, item_id)
            .iter()
            .find_map(|item| artifacts.plan_of(&item.id))
            .map(|plan| PlanFacts {
                artifact_id: plan.id.clone(),
                title: plan.title.clone(),
                revision: plan.current_revision,
            })
    });
    let children = item
        .child_ids
        .iter()
        .filter_map(|id| snapshot.item(id))
        .filter(|child| child.archived_at.is_none())
        .map(|child| ChildFacts {
            id: child.id.clone(),
            title: child.title.clone(),
            done: matches!(child.effective_status, WorkItemEffectiveStatus::Completed),
        })
        .collect();
    Some(ItemSection {
        space_id: snapshot.space_id.clone(),
        item_id: item_id.to_owned(),
        path,
        instructions: InstructionFacts {
            drift: state.is_some_and(|s| s.drift),
            delivery: state.and_then(instructions::delivery_line),
            own,
            inherited,
            has_state: state.is_some(),
            proposals: crate::work_outline::required_actions::pending_proposals_from_chat(
                snapshot, chat_id,
            )
            .len(),
        },
        actions,
        plan,
        children,
    })
}

/// Hover delay before the context card appears.
pub(crate) const HOVER_DELAY_MS: u64 = 400;
/// Grace before it hides once the pointer left both header and card, so
/// crossing the gap between them does not flicker.
pub(crate) const HIDE_GRACE_MS: u64 = 160;

/// A timer the host must run, then hand back to [`CardState::fire`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CardTimer {
    Show(u64),
    Hide(u64),
}

impl CardTimer {
    pub(crate) fn delay_ms(self) -> u64 {
        match self {
            Self::Show(_) => HOVER_DELAY_MS,
            Self::Hide(_) => HIDE_GRACE_MS,
        }
    }
}

/// The hover card's show / hide / pin state machine. Pure: hover events and
/// timer expiries go in, "does the card show now" comes out. Epochs make a
/// stale timer (the pointer moved again meanwhile) a no-op.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct CardState {
    shown: bool,
    pinned: bool,
    header_hover: bool,
    card_hover: bool,
    epoch: u64,
}

impl CardState {
    pub(crate) fn visible(&self) -> bool {
        self.shown || self.pinned
    }

    pub(crate) fn pinned(&self) -> bool {
        self.pinned
    }

    fn hovered(&self) -> bool {
        self.header_hover || self.card_hover
    }

    fn settle(&mut self) -> Option<CardTimer> {
        self.epoch += 1;
        if self.hovered() {
            (!self.shown && !self.pinned).then_some(CardTimer::Show(self.epoch))
        } else {
            (self.shown && !self.pinned).then_some(CardTimer::Hide(self.epoch))
        }
    }

    pub(crate) fn header_hover(&mut self, on: bool) -> Option<CardTimer> {
        if self.header_hover == on {
            return None;
        }
        self.header_hover = on;
        self.settle()
    }

    pub(crate) fn card_hover(&mut self, on: bool) -> Option<CardTimer> {
        if self.card_hover == on {
            return None;
        }
        self.card_hover = on;
        self.settle()
    }

    /// A timer ran out; true when the card's visibility changed.
    pub(crate) fn fire(&mut self, timer: CardTimer) -> bool {
        let before = self.visible();
        match timer {
            CardTimer::Show(epoch) if epoch == self.epoch && self.hovered() => self.shown = true,
            CardTimer::Hide(epoch) if epoch == self.epoch && !self.hovered() => self.shown = false,
            _ => {}
        }
        before != self.visible()
    }

    /// ⌥⌘I: pin the card open, or release it (it then follows the pointer).
    pub(crate) fn toggle_pin(&mut self) {
        self.epoch += 1;
        self.pinned = !self.pinned;
        self.shown = self.hovered();
    }

    /// Hide at once and forget the pin (a different chat, Esc).
    pub(crate) fn dismiss(&mut self) -> bool {
        let before = self.visible();
        *self = Self {
            epoch: self.epoch + 1,
            ..Self::default()
        };
        before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_outline::required_actions::fixture::request;
    use crate::work_outline::store::{fixture_ids::*, fixture_snapshot};

    fn chat(id: &str) -> Chat {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "deviceId": "device",
            "title": "A title",
            "archived": false,
            "cwd": "/work/app",
            "branch": "feature/x",
            "checkoutId": null,
            "config": {"harness": "claude-code", "model": "fable-5-1", "reasoning": null,
                       "modelOptions": {}, "sandbox": "workspace-write", "autoApprove": false},
            "lastMessagePreview": null,
            "lastMessageAt": null,
            "createdAt": "2026-01-01T00:00:00Z",
            "actualModel": "opus-5-5"
        }))
        .unwrap()
    }

    fn sheet(chat: &Chat, snapshot: &WorkOutlineSnapshot) -> ChatItemFacts {
        facts(&FactsInput {
            chat,
            space_path: Some("/project"),
            account: Some("Work".into()),
            links: None,
            snapshot: Some(snapshot),
            artifacts: None,
        })
    }

    #[test]
    fn model_shows_what_ran_and_what_was_configured() {
        let chat = chat(CHAT_API);
        let sheet = sheet(&chat, &fixture_snapshot("s1"));
        assert_eq!(sheet.model.label.as_deref(), Some("opus-5-5"));
        assert_eq!(sheet.model.configured.as_deref(), Some("fable-5-1"));
        assert_eq!(sheet.model.chip(), "Model opus-5-5");
        assert_eq!(sheet.model.detail(), "ran \u{b7} configured fable-5-1");
        assert_eq!(sheet.account.as_deref(), Some("Work"));
        assert_eq!(sheet.runs_in.as_deref(), Some("/work/app"));
        assert_eq!(sheet.branch.as_deref(), Some("feature/x"));
    }

    #[test]
    fn an_item_chat_has_a_path_and_a_plain_chat_has_no_item_section() {
        let snapshot = fixture_snapshot("s1");
        let with_item = sheet(&chat(CHAT_API), &snapshot);
        let item = with_item.item.expect("CHAT_API is on an item");
        assert_eq!(item.item_id, API);
        assert!(item.path.len() >= 2);
        assert_eq!(item.path.last().unwrap().id, API);
        let (trimmed, last_two) = item.trimmed_path(2);
        assert_eq!((trimmed, last_two.len()), (item.path.len() > 2, 2));
        assert!(sheet(&chat(CHAT_LOOSE_A), &snapshot).item.is_none());
    }

    #[test]
    fn attention_counts_asks_and_drift_and_questions_count_plan_questions() {
        let mut snapshot = fixture_snapshot("s1");
        snapshot
            .user_action_requests
            .push(request("r1", "s1", API, CHAT_API, "Pick one"));
        let mut question = request("r2", "s1", API, CHAT_API, "Q-1");
        question.dedupe_key = Some("artq:a:Q-1".into());
        snapshot.user_action_requests.push(question);
        // Another item's ask is not this chat's.
        snapshot
            .user_action_requests
            .push(request("r3", "s1", UI, CHAT_UI, "Elsewhere"));
        let item = sheet(&chat(CHAT_API), &snapshot).item.unwrap();
        assert_eq!((item.attention_count(), item.question_count()), (1, 1));
        assert_eq!(item.actions.len(), 2);
    }

    #[test]
    fn an_unrelated_change_leaves_the_sheet_equal() {
        let before = fixture_snapshot("s1");
        let mut after = before.clone();
        after
            .user_action_requests
            .push(request("r9", "s1", UI, CHAT_UI, "Another chat's ask"));
        let chat = chat(CHAT_API);
        assert_eq!(sheet(&chat, &before), sheet(&chat, &after));
        // Its own item's ask does change it.
        after
            .user_action_requests
            .push(request("r10", "s1", API, CHAT_API, "Its own"));
        assert_ne!(sheet(&chat, &before), sheet(&chat, &after));
    }

    #[test]
    fn the_card_waits_to_show_and_to_hide_and_stale_timers_do_nothing() {
        let mut card = CardState::default();
        let show = card.header_hover(true).expect("hover starts a show timer");
        assert_eq!(show.delay_ms(), HOVER_DELAY_MS);
        assert!(!card.visible());
        assert!(card.fire(show));
        assert!(card.visible());
        // Leaving and re-entering before the grace ends cancels the hide.
        let hide = card.header_hover(false).unwrap();
        assert_eq!(hide.delay_ms(), HIDE_GRACE_MS);
        card.header_hover(true);
        assert!(!card.fire(hide));
        assert!(card.visible());
        // Moving from the header onto the card keeps it open.
        let hide = card.header_hover(false).unwrap();
        card.card_hover(true);
        assert!(!card.fire(hide));
        let hide = card.card_hover(false).unwrap();
        assert!(card.fire(hide));
        assert!(!card.visible());
        // A pass shorter than the delay never shows it.
        let show = card.header_hover(true).unwrap();
        card.header_hover(false);
        assert!(!card.fire(show));
    }

    #[test]
    fn pinning_keeps_the_card_until_released() {
        let mut card = CardState::default();
        card.toggle_pin();
        assert!(card.visible() && card.pinned());
        assert!(card.header_hover(true).is_none());
        assert!(card.header_hover(false).is_none());
        assert!(card.visible());
        card.toggle_pin();
        assert!(!card.visible(), "released with the pointer away");
        // Released while hovering: it stays until the pointer leaves.
        card.toggle_pin();
        card.header_hover(true);
        card.toggle_pin();
        assert!(card.visible());
        let hide = card.header_hover(false).unwrap();
        assert!(card.fire(hide));
        // A different chat drops everything.
        card.toggle_pin();
        assert!(card.dismiss());
        assert!(!card.visible() && !card.pinned());
    }
}
