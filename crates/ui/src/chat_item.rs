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
        self.actions.iter().filter(|a| !a.plan_question).count() + usize::from(self.instructions.drift)
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
        },
        actions,
        plan,
        children,
    })
}
