//! The work-context strip at the top of the full chat view: one line that
//! says what the Chat is for, expandable to a small details panel.
//!
//! Three variants, picked by [`resolve`] with a fixed precedence (PR review
//! item > Linear issue > Linear project or pinned plan > none):
//!
//! - **PR review**: the Chat's item, or its nearest ancestor, is a PR review
//!   item (a synced PR item, a child of the sync destination, or titled
//!   `<repo>#<n>`), or a review the launcher created for a PR.
//! - **Implementation**: the item or an ancestor links a Linear issue (or a
//!   PR that is not a review): the ticket and the resulting PR(s).
//! - **Planning**: the item or an ancestor links a Linear project, or has a
//!   pinned plan artifact.
//!
//! A plain Chat gets no strip; transcript PR mentions have their own chips.
//! `resolve` is pure (snapshots in, [`WorkContext`] out). [`strip`] renders
//! from the existing stores (`PrEntry`, `IssueEntry`, the artifact snapshot)
//! and never fetches; the Shell host (`shell/work_context_ui.rs`) requests
//! the data and supplies the click handlers.

use zeron_engine::pr_ticket_cache::ChatLinkStatus;
use zeron_proto::linear::{LINEAR_ISSUE_REFERENCE, LINEAR_PROJECT_REFERENCE};
use zeron_proto::{
    ArtifactView, ArtifactsSnapshot, ChatLinkSource, PullRequestRef, WorkItemChatAssignmentState,
    WorkItemView, WorkOutlineSnapshot,
};

pub(crate) mod strip;
#[cfg(test)]
mod tests;

/// A durable PR link of the Chat (`CHAT_LINK_STATUS`), parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatPr {
    pub pr: PullRequestRef,
    pub source: Option<ChatLinkSource>,
}

impl ChatPr {
    /// Linked on purpose (the review launcher, `gh pr create` in the Chat,
    /// or by hand), as opposed to a PR URL that merely appeared in the
    /// transcript.
    fn deliberate(&self) -> bool {
        !matches!(self.source, Some(ChatLinkSource::Mentioned))
    }
}

/// The Chat's PR links from its link status (unparseable URLs dropped).
pub(crate) fn chat_prs(status: &ChatLinkStatus) -> Vec<ChatPr> {
    let from_links = status.pr_links.iter().filter_map(|link| {
        Some(ChatPr {
            pr: parse_pr_url(&link.url)?,
            source: link.source,
        })
    });
    let mut out: Vec<ChatPr> = from_links.collect();
    if out.is_empty()
        && let Some(pr) = status
            .pr
            .as_ref()
            .and_then(|pr| pr.url.as_deref())
            .and_then(parse_pr_url)
    {
        out.push(ChatPr {
            pr,
            source: status.pr_source,
        });
    }
    out
}

/// `https://github.com/<owner>/<repo>/pull/<n>` (any trailing path, query or
/// fragment) as a [`PullRequestRef`].
pub(crate) fn parse_pr_url(url: &str) -> Option<PullRequestRef> {
    let rest = url
        .trim()
        .strip_prefix("https://github.com/")
        .or_else(|| url.trim().strip_prefix("http://github.com/"))?;
    let mut parts = rest.split('/');
    let (owner, repo, kind, number) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if kind != "pull" || owner.is_empty() || repo.is_empty() {
        return None;
    }
    let number: u64 = number
        .split(['?', '#'])
        .next()?
        .parse()
        .ok()
        .filter(|n| *n > 0)?;
    Some(PullRequestRef::new(owner, repo, number))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IssueRef {
    pub identifier: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectRef {
    pub id: String,
    pub name: String,
    pub url: String,
}

/// The slice of an artifact the planning strip shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArtifactBrief {
    pub id: String,
    pub title: String,
    pub kind: &'static str,
    pub revision: u32,
    /// Epoch milliseconds of the newest revision.
    pub at: i64,
    pub pinned: bool,
}

impl ArtifactBrief {
    fn from_view(artifact: &ArtifactView) -> Self {
        Self {
            id: artifact.id.clone(),
            title: artifact.title.clone(),
            kind: artifact.kind.as_str(),
            revision: artifact.current_revision,
            at: artifact
                .revisions
                .first()
                .map_or(artifact.updated_at, |revision| revision.at),
            pinned: artifact.pinned_as_plan,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrReviewContext {
    /// The item that links the PRs (the Chat's own, or an ancestor); `None`
    /// for a launcher-created review with no PR item.
    pub pr_item_id: Option<String>,
    /// Earliest linked first; the strip shows the first and `+N`.
    pub prs: Vec<PullRequestRef>,
    pub ticket: Option<IssueRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImplementationContext {
    pub pr_item_id: Option<String>,
    pub ticket: Option<IssueRef>,
    /// Empty: "No PR yet".
    pub prs: Vec<PullRequestRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanningContext {
    pub project: Option<ProjectRef>,
    /// The pinned plan of the item or its nearest ancestor with one.
    pub plan: Option<ArtifactBrief>,
    /// Every live artifact of the Chat's item and its descendants, the plan
    /// included: pinned first, then newest first.
    pub artifacts: Vec<ArtifactBrief>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Variant {
    PrReview(PrReviewContext),
    Implementation(ImplementationContext),
    Planning(PlanningContext),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkContext {
    /// The Chat's own work item, when it has one.
    pub item_id: Option<String>,
    pub variant: Variant,
}

pub(crate) struct ResolveInput<'a> {
    pub chat_id: &'a str,
    pub snapshot: Option<&'a WorkOutlineSnapshot>,
    pub chat_prs: &'a [ChatPr],
    /// The Chat is classified `pr_review` (the launcher pre-classifies the
    /// Chats it creates).
    pub review_classified: bool,
    pub artifacts: Option<&'a ArtifactsSnapshot>,
}

/// The work item a Chat is associated with (dangling assignments excluded).
pub(crate) fn assigned_item<'a>(snapshot: &'a WorkOutlineSnapshot, chat_id: &str) -> Option<&'a str> {
    let dangling = snapshot.assignments.iter().any(|assignment| {
        assignment.chat_id == chat_id
            && assignment.state == WorkItemChatAssignmentState::DanglingChat
    });
    if dangling {
        return None;
    }
    snapshot
        .items
        .iter()
        .find(|item| item.chat_ids.iter().any(|id| id == chat_id))
        .map(|item| item.id.as_str())
}

/// `item_id`, then its ancestors up to the root (cycle-guarded).
fn lineage<'a>(snapshot: &'a WorkOutlineSnapshot, item_id: &str) -> Vec<&'a WorkItemView> {
    let mut out: Vec<&WorkItemView> = Vec::new();
    let mut cursor = snapshot.item(item_id);
    while let Some(item) = cursor {
        if out.iter().any(|seen| seen.id == item.id) {
            break;
        }
        out.push(item);
        cursor = item
            .parent_work_item_id
            .as_deref()
            .and_then(|id| snapshot.item(id));
    }
    out
}

/// Whether `item` is a PR review item. Like `github::pr_review_item`, but an
/// archived item still counts: merged and closed PRs auto-archive their item
/// while the review Chat stays open.
fn is_review_item(snapshot: &WorkOutlineSnapshot, item: &WorkItemView, pr: &PullRequestRef) -> bool {
    let synced = item
        .external_sync_state
        .as_ref()
        .is_some_and(|state| state.provider == zeron_proto::GITHUB_PROVIDER);
    let destination = snapshot.pr_review_sync_destination();
    let under_destination =
        destination.is_some() && item.parent_work_item_id.as_deref() == destination;
    let titled = item
        .title
        .to_lowercase()
        .starts_with(&format!("{}#{}", pr.repo, pr.number).to_lowercase());
    synced || under_destination || titled
}

/// The first item of `lineage` that links PRs, with those PRs.
fn pr_item<'a>(
    snapshot: &WorkOutlineSnapshot,
    lineage: &[&'a WorkItemView],
) -> Option<(&'a WorkItemView, Vec<PullRequestRef>)> {
    lineage.iter().find_map(|item| {
        let prs = crate::github::item_pull_requests(snapshot, &item.id);
        (!prs.is_empty()).then_some((*item, prs))
    })
}

fn ticket(snapshot: &WorkOutlineSnapshot, item_id: &str) -> Option<IssueRef> {
    let reference = snapshot.nearest_linear_reference(item_id, LINEAR_ISSUE_REFERENCE)?;
    Some(IssueRef {
        identifier: reference.external_id.clone(),
        url: reference.url.clone(),
    })
}

fn project(snapshot: &WorkOutlineSnapshot, item_id: &str) -> Option<ProjectRef> {
    let reference = snapshot.nearest_linear_reference(item_id, LINEAR_PROJECT_REFERENCE)?;
    Some(ProjectRef {
        id: reference.external_id.clone(),
        name: reference
            .cached_title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| "Linear project".to_owned()),
        url: reference.url.clone(),
    })
}

fn push_unique(prs: &mut Vec<PullRequestRef>, pr: PullRequestRef) {
    if !prs.iter().any(|known| known.key() == pr.key()) {
        prs.push(pr);
    }
}

/// Which strip, if any, the Chat shows. Pure.
pub(crate) fn resolve(input: &ResolveInput<'_>) -> Option<WorkContext> {
    let deliberate: Vec<&ChatPr> = input.chat_prs.iter().filter(|link| link.deliberate()).collect();
    let item_id = input
        .snapshot
        .and_then(|snapshot| assigned_item(snapshot, input.chat_id));
    let chain = match (input.snapshot, item_id) {
        (Some(snapshot), Some(id)) => lineage(snapshot, id),
        _ => Vec::new(),
    };
    let linear = input.snapshot.filter(|s| crate::linear::space_enabled(Some(s)));
    let item_ticket = |snapshot: Option<&WorkOutlineSnapshot>| {
        item_id
            .zip(snapshot)
            .and_then(|(id, snapshot)| ticket(snapshot, id))
    };
    let linked = input
        .snapshot
        .and_then(|snapshot| pr_item(snapshot, &chain));
    let context = |variant| {
        Some(WorkContext {
            item_id: item_id.map(str::to_owned),
            variant,
        })
    };

    // 1. PR review: the item (or an ancestor) is a review item, else a
    //    review the launcher created.
    if let (Some(snapshot), Some((item, prs))) = (input.snapshot, &linked)
        && is_review_item(snapshot, item, &prs[0])
    {
        return context(Variant::PrReview(PrReviewContext {
            pr_item_id: Some(item.id.clone()),
            prs: prs.clone(),
            ticket: item_ticket(linear),
        }));
    }
    if input.review_classified && !deliberate.is_empty() {
        return context(Variant::PrReview(PrReviewContext {
            pr_item_id: None,
            prs: deliberate.iter().map(|link| link.pr.clone()).collect(),
            ticket: item_ticket(linear),
        }));
    }

    // 2. Implementation: a Linear issue, or a PR the item links itself.
    let issue = item_ticket(linear);
    if issue.is_some() || linked.is_some() {
        let (pr_item_id, mut prs) = match linked {
            Some((item, prs)) => (Some(item.id.clone()), prs),
            None => (None, Vec::new()),
        };
        for link in &deliberate {
            push_unique(&mut prs, link.pr.clone());
        }
        return context(Variant::Implementation(ImplementationContext {
            pr_item_id,
            ticket: issue,
            prs,
        }));
    }

    // 3. Planning: a Linear project or a pinned plan.
    let snapshot = input.snapshot?;
    let item_id = item_id?;
    let project = linear.and_then(|snapshot| project(snapshot, item_id));
    let plan = input.artifacts.and_then(|artifacts| {
        chain
            .iter()
            .find_map(|item| artifacts.plan_of(&item.id))
            .map(ArtifactBrief::from_view)
    });
    if project.is_none() && plan.is_none() {
        return None;
    }
    let mut artifacts = item_artifacts(snapshot, input.artifacts, item_id);
    if let Some(plan) = &plan
        && !artifacts.iter().any(|artifact| artifact.id == plan.id)
    {
        artifacts.insert(0, plan.clone());
    }
    context(Variant::Planning(PlanningContext {
        project,
        plan,
        artifacts,
    }))
}

/// Live artifacts of `item_id` and its descendants: pinned plans first, then
/// newest first.
fn item_artifacts(
    snapshot: &WorkOutlineSnapshot,
    artifacts: Option<&ArtifactsSnapshot>,
    item_id: &str,
) -> Vec<ArtifactBrief> {
    let Some(artifacts) = artifacts else {
        return Vec::new();
    };
    let views: Vec<&ArtifactView> = crate::work_outline::instructions::subtree(snapshot, item_id)
        .iter()
        .flat_map(|id| artifacts.for_work_item(id))
        .collect();
    crate::artifacts::store::details_order(views)
        .into_iter()
        .map(ArtifactBrief::from_view)
        .collect()
}
