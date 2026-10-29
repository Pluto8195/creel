//! Required Actions and instruction proposals (Milestone 4): pure, GUI-free
//! logic shared by the Required Actions views, Outline rows, Work Item
//! Details, the chat header, and the Shell's notification detector.
//!
//! - [`collect_rows`] turns snapshots + Chats into Required Actions rows for a
//!   [`RequestScope`] (global, one Space, or one item's subtree): open requests
//!   (duplicates flagged `deduped` are folded into their canonical request)
//!   and the resolved history the snapshot carries.
//! - [`status_totals`] renders the compact descendant totals ("3 active · 1
//!   user action required") from a rollup.
//! - [`UserActionNotifier`] diffs consecutive snapshots per work item and
//!   reports the zero-to-nonzero transition of its unresolved requests, once
//!   per transition (a batch of requests on one item is one event), never for
//!   a request id it already reported.
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use mast_proto::{
    InstructionProposalStatus, ProposalTargetScope, UserActionRequestView,
    WorkItemInstructionProposalView, WorkItemRollup, WorkOutlineActorKind, WorkOutlineSnapshot,
};

use super::model::{self, OutlineChat};

/// Which requests a Required Actions view lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RequestScope {
    /// Every watched Space (the global view).
    Global,
    /// One Space (Space-root details).
    Space(String),
    /// One item and its descendants (Work Item Details).
    Subtree { space_id: String, item_id: String },
}

/// One Space's snapshot as a Required Actions view sees it.
#[derive(Debug, Clone)]
pub(crate) struct SpaceSnapshot {
    pub space_id: String,
    pub label: String,
    pub snapshot: Arc<WorkOutlineSnapshot>,
}

/// Label shown instead of the Chat title when the origin Chat is archived or
/// deleted (the request stays inspectable and resolvable).
pub(crate) const CHAT_UNAVAILABLE: &str = "Chat unavailable";

/// One Required Actions row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequiredActionRow {
    pub space_id: String,
    pub request_id: String,
    pub work_item_id: String,
    pub text: String,
    pub info: Option<String>,
    /// Breadcrumb of the work item (prefixed with the Space in a multi-Space
    /// global view); "Deleted work item" when the item is gone.
    pub path: String,
    pub chat_id: String,
    /// The Chat title, or [`CHAT_UNAVAILABLE`].
    pub chat_label: String,
    /// The origin Chat can be opened.
    pub chat_available: bool,
    /// Raised by the engine itself (PR review sync): no Chat, origin
    /// "GitHub", Resolve only.
    pub system: bool,
    /// A system request's PR (its work item's `github` / `pullRequest`
    /// reference URL): Open goes there when the item is gone.
    pub pr_url: Option<String>,
    pub created_at: i64,
    /// Open duplicates folded into this row (same dedupe key).
    pub duplicates: usize,
    pub resolution: Option<ResolutionLine>,
}

/// How a resolved request was resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolutionLine {
    pub at: i64,
    pub by: &'static str,
    pub note: Option<String>,
}

/// Open rows (oldest first: the longest wait leads) and resolved history
/// (newest resolution first).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RequiredActionRows {
    pub open: Vec<RequiredActionRow>,
    pub resolved: Vec<RequiredActionRow>,
}

fn in_scope(scope: &RequestScope, space_id: &str, subtree: Option<&HashSet<String>>) -> bool {
    match scope {
        RequestScope::Global => true,
        RequestScope::Space(id) => id == space_id,
        RequestScope::Subtree { space_id: id, .. } => id == space_id && subtree.is_some(),
    }
}

fn actor_label(kind: WorkOutlineActorKind) -> &'static str {
    match kind {
        WorkOutlineActorKind::User => "you",
        WorkOutlineActorKind::Agent => "an agent",
        WorkOutlineActorKind::System => "Mast",
    }
}

/// Build the rows of `scope` from `spaces` (sorted as given) and the Overview's
/// Chats.
pub(crate) fn collect_rows(
    spaces: &[SpaceSnapshot],
    chats: &[OutlineChat],
    scope: &RequestScope,
) -> RequiredActionRows {
    let chat_titles: HashMap<&str, &OutlineChat> =
        chats.iter().map(|chat| (chat.id.as_str(), chat)).collect();
    let multi_space = matches!(scope, RequestScope::Global) && spaces.len() > 1;
    let mut rows = RequiredActionRows::default();
    for space in spaces {
        let snapshot = &space.snapshot;
        let subtree: Option<HashSet<String>> = match scope {
            RequestScope::Subtree { space_id, item_id } if *space_id == space.space_id => {
                let mut ids = model::descendants(snapshot, item_id);
                ids.insert(item_id.clone());
                Some(ids)
            }
            _ => None,
        };
        if !in_scope(scope, &space.space_id, subtree.as_ref()) {
            continue;
        }
        // Open duplicates fold into their canonical request.
        let mut duplicates: HashMap<(&str, &str), usize> = HashMap::new();
        for request in snapshot
            .user_action_requests
            .iter()
            .filter(|request| request.is_open() && request.deduped)
        {
            if let Some(key) = request.dedupe_key.as_deref() {
                *duplicates
                    .entry((request.work_item_id.as_str(), key))
                    .or_default() += 1;
            }
        }
        for request in &snapshot.user_action_requests {
            if subtree
                .as_ref()
                .is_some_and(|ids| !ids.contains(&request.work_item_id))
            {
                continue;
            }
            if request.is_open() && request.deduped {
                continue;
            }
            let row = row_for(
                space,
                request,
                &chat_titles,
                multi_space,
                request
                    .dedupe_key
                    .as_deref()
                    .filter(|_| request.is_open())
                    .and_then(|key| duplicates.get(&(request.work_item_id.as_str(), key)))
                    .copied()
                    .unwrap_or(0),
            );
            if request.is_open() {
                rows.open.push(row);
            } else {
                rows.resolved.push(row);
            }
        }
    }
    rows.open
        .sort_by(|a, b| (a.created_at, &a.request_id).cmp(&(b.created_at, &b.request_id)));
    rows.resolved.sort_by(|a, b| {
        let at = |row: &RequiredActionRow| row.resolution.as_ref().map_or(0, |r| r.at);
        (at(b), &b.request_id).cmp(&(at(a), &a.request_id))
    });
    rows
}

fn row_for(
    space: &SpaceSnapshot,
    request: &UserActionRequestView,
    chats: &HashMap<&str, &OutlineChat>,
    multi_space: bool,
    duplicates: usize,
) -> RequiredActionRow {
    let snapshot = &space.snapshot;
    let path = if snapshot.item(&request.work_item_id).is_some() {
        model::path_label(
            model::item_path(snapshot, &request.work_item_id)
                .iter()
                .map(|item| item.title.as_str()),
        )
    } else {
        "Deleted work item".to_owned()
    };
    let path = if multi_space {
        format!("{} › {path}", space.label)
    } else {
        path
    };
    let chat = chats
        .get(request.chat_id.as_str())
        .filter(|chat| !chat.archived);
    let system = request.origin_kind == mast_proto::UserActionOriginKind::System;
    let chat_available = !system && request.origin_chat_available && chat.is_some();
    let pr_url = system
        .then(|| snapshot.github_pull_request_reference(&request.work_item_id))
        .flatten()
        .map(|reference| reference.url.clone());
    RequiredActionRow {
        space_id: space.space_id.clone(),
        request_id: request.id.clone(),
        work_item_id: request.work_item_id.clone(),
        text: request.text.clone(),
        info: request.info.clone().filter(|info| !info.trim().is_empty()),
        path,
        chat_id: request.chat_id.clone(),
        chat_label: match chat {
            _ if system => crate::github::SYSTEM_ORIGIN_LABEL.to_owned(),
            Some(chat) if chat_available => chat.title.clone(),
            _ => CHAT_UNAVAILABLE.to_owned(),
        },
        chat_available,
        system,
        pr_url,
        created_at: request.created_at,
        duplicates,
        resolution: (!request.is_open()).then(|| {
            let resolution = request.resolution.as_ref();
            ResolutionLine {
                at: resolution.map_or(0, |r| r.at),
                by: resolution.map_or("Mast", |r| actor_label(r.by.kind)),
                note: resolution
                    .and_then(|r| r.note.clone())
                    .filter(|note| !note.trim().is_empty()),
            }
        }),
    }
}

/// Compact age ("now", "5m", "3h", "2d", ...) of a unix-millis timestamp,
/// in the app's shared relative-time format.
pub(crate) fn age_label(at_millis: i64, now: chrono::DateTime<chrono::Utc>) -> String {
    let then = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at_millis).unwrap_or(now);
    mast_proto::view::format_time_ago(then, now)
}

/// How much of the request text a reply quotes.
pub(crate) const REPLY_QUOTE_MAX_CHARS: usize = 120;

/// Reply & resolve's role for the resolve operation id, derived from the
/// reply intent's operation id (the reply's message id derives from the same
/// id under `roles::MESSAGE`, host-side).
pub(crate) const REPLY_RESOLVE_ROLE: &str = "reply-resolve";

/// The user message Reply & resolve sends to the origin Chat:
/// `Re: "<request text>": <note>`. The quote is whitespace-collapsed and cut
/// to [`REPLY_QUOTE_MAX_CHARS`] (with an ellipsis); the note is sent as typed.
pub(crate) fn reply_message(request_text: &str, note: &str) -> String {
    let collapsed = request_text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let quote = if collapsed.chars().count() > REPLY_QUOTE_MAX_CHARS {
        let cut: String = collapsed.chars().take(REPLY_QUOTE_MAX_CHARS - 1).collect();
        format!("{}\u{2026}", cut.trim_end())
    } else {
        collapsed
    };
    format!("Re: \"{quote}\": {}", note.trim())
}

/// Why Reply & resolve is unavailable for `row` (`None` when it can send).
/// A missing Chat wins over an empty note.
pub(crate) fn reply_unavailable_reason(
    row: &RequiredActionRow,
    note: &str,
) -> Option<&'static str> {
    if !row.chat_available {
        Some(
            "The Chat that asked was archived or deleted, so a reply can\u{2019}t reach it. \
             Resolve still records your note.",
        )
    } else if note.trim().is_empty() {
        Some("Type a reply first")
    } else {
        None
    }
}

/// Descendant status totals for a work item row: "3 active · 1 user action
/// required". Status counts are stored-status subtree counts (the item
/// included), so the blocked item counts in both. Shown only when the
/// subtree has more than the item itself or something needs the user;
/// `None` otherwise (and when nothing is active or blocked).
pub(crate) fn status_totals(rollup: &WorkItemRollup) -> Option<StatusTotals> {
    let counted = rollup.pending_count + rollup.active_count + rollup.completed_count;
    if counted <= 1 && rollup.user_action_required_count == 0 {
        return None;
    }
    let totals = StatusTotals {
        active: rollup.active_count,
        user_action_required: rollup.user_action_required_count,
    };
    (totals.active > 0 || totals.user_action_required > 0).then_some(totals)
}

/// See [`status_totals`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct StatusTotals {
    pub active: u32,
    pub user_action_required: u32,
}

impl StatusTotals {
    /// "3 active", or `None` when nothing is active.
    pub(crate) fn active_label(&self) -> Option<String> {
        (self.active > 0).then(|| format!("{} active", self.active))
    }

    /// "1 user action required", or `None`.
    pub(crate) fn attention_label(&self) -> Option<String> {
        (self.user_action_required > 0)
            .then(|| format!("{} user action required", self.user_action_required))
    }

    pub(crate) fn label(&self) -> String {
        [self.active_label(), self.attention_label()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" \u{b7} ")
    }
}

/// Space-wide totals: the rollups of the Space's live root items summed.
pub(crate) fn space_totals(snapshot: &WorkOutlineSnapshot) -> Option<StatusTotals> {
    let mut rollup = WorkItemRollup::default();
    for item in snapshot
        .items
        .iter()
        .filter(|item| item.parent_work_item_id.is_none() && item.archived_at.is_none())
    {
        rollup.pending_count += item.rollup.pending_count;
        rollup.active_count += item.rollup.active_count;
        rollup.completed_count += item.rollup.completed_count;
        rollup.user_action_required_count += item.rollup.user_action_required_count;
    }
    status_totals(&rollup)
}

/// Unresolved requests across `spaces` (the global count).
pub(crate) fn open_request_count(spaces: &[SpaceSnapshot]) -> usize {
    spaces
        .iter()
        .flat_map(|space| space.snapshot.user_action_requests.iter())
        .filter(|request| request.is_unresolved())
        .count()
}

// --------------------------------------------------------------------------
// Proposals
// --------------------------------------------------------------------------

/// The work item a proposal targets (`None` = the Space root, or an unknown
/// scope).
pub(crate) fn proposal_target_item(proposal: &WorkItemInstructionProposalView) -> Option<&str> {
    match proposal.target.scope {
        ProposalTargetScope::WorkItem => proposal.target.work_item_id.as_deref(),
        _ => None,
    }
}

pub(crate) fn is_pending(proposal: &WorkItemInstructionProposalView) -> bool {
    proposal.status == InstructionProposalStatus::Pending
}

/// Proposals (any outcome) targeting `item_id` (`None` = the Space root),
/// in snapshot order.
pub(crate) fn proposals_for<'a>(
    snapshot: &'a WorkOutlineSnapshot,
    item_id: Option<&'a str>,
) -> impl Iterator<Item = &'a WorkItemInstructionProposalView> + 'a {
    snapshot
        .instruction_proposals
        .iter()
        .filter(move |proposal| match (proposal.target.scope, item_id) {
            (ProposalTargetScope::SpaceRoot, None) => true,
            (ProposalTargetScope::WorkItem, Some(id)) => {
                proposal.target.work_item_id.as_deref() == Some(id)
            }
            _ => false,
        })
}

/// Pending proposals targeting `item_id` (`None` = the Space root).
pub(crate) fn pending_proposal_count(
    snapshot: &WorkOutlineSnapshot,
    item_id: Option<&str>,
) -> usize {
    proposals_for(snapshot, item_id)
        .filter(|proposal| is_pending(proposal))
        .count()
}

/// Pending proposals a Chat raised, with the item each targets (`None` =
/// Space root).
pub(crate) fn pending_proposals_from_chat<'a>(
    snapshot: &'a WorkOutlineSnapshot,
    chat_id: &'a str,
) -> Vec<&'a WorkItemInstructionProposalView> {
    snapshot
        .instruction_proposals
        .iter()
        .filter(|proposal| {
            is_pending(proposal) && proposal.source_ref.chat_id.as_deref() == Some(chat_id)
        })
        .collect()
}

/// "1 proposed instruction" / "3 proposed instructions".
pub(crate) fn proposals_label(count: usize) -> String {
    super::instructions::plural(count, "proposed instruction", "proposed instructions")
}

// --------------------------------------------------------------------------
// Notification detector
// --------------------------------------------------------------------------

/// One notification event: an item went from zero to at least one
/// unresolved request. Several requests arriving together are one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UserActionNotice {
    pub space_id: String,
    pub work_item_id: String,
    pub item_title: String,
    pub count: usize,
    /// The newest request's text (the banner body when `count == 1`).
    pub text: String,
    /// The newest request's origin Chat when it can be opened (banner click).
    pub chat_id: Option<String>,
}

impl UserActionNotice {
    pub(crate) fn title(&self) -> String {
        if self.item_title.trim().is_empty() {
            "User action required".to_owned()
        } else {
            self.item_title.clone()
        }
    }

    pub(crate) fn body(&self) -> String {
        if self.count == 1 && !self.text.trim().is_empty() {
            self.text.clone()
        } else {
            format!("{} actions required", self.count)
        }
    }
}

#[derive(Debug, Default)]
struct ItemSeen {
    open: BTreeSet<String>,
}

#[derive(Debug, Default)]
struct SpaceSeen {
    /// The last observed snapshot allocation (identical frames are skipped).
    last: Option<Arc<WorkOutlineSnapshot>>,
    items: HashMap<String, ItemSeen>,
    /// Request ids already reported (or seeded); never reported again.
    notified: HashSet<String>,
}

/// Reported request ids remembered per Space before ids the snapshot no
/// longer lists are forgotten.
const MAX_REMEMBERED_REQUEST_IDS: usize = 4_096;

/// Per-work-item zero-to-nonzero detector over consecutive snapshots.
///
/// - The first snapshot of a Space seeds silently (boot and reconnect stay
///   quiet, like the session detector's first appearance).
/// - An item fires when it had no unresolved request and now has some, at
///   least one of which was never reported (retries and flapping frames of
///   the same ids never re-notify). All of its new requests are one event.
/// - A resolve that brings the item to zero re-arms it: a later new request
///   fires again.
/// - Requests on archived (or deleted) items are seeded as they are seen, so
///   restoring an item with old open requests stays quiet: only requests the
///   detector never saw can fire (an archived item cannot gain new ones).
/// - Unavailable snapshots are ignored (no observation).
#[derive(Debug, Default)]
pub(crate) struct UserActionNotifier {
    spaces: HashMap<String, SpaceSeen>,
}

impl UserActionNotifier {
    /// Observe `snapshot` for `space_id`; the notices it triggers.
    pub(crate) fn observe(
        &mut self,
        space_id: &str,
        snapshot: &Arc<WorkOutlineSnapshot>,
    ) -> Vec<UserActionNotice> {
        if !snapshot.available {
            return Vec::new();
        }
        let first = !self.spaces.contains_key(space_id);
        let seen = self.spaces.entry(space_id.to_owned()).or_default();
        if seen
            .last
            .as_ref()
            .is_some_and(|last| Arc::ptr_eq(last, snapshot))
        {
            return Vec::new();
        }
        seen.last = Some(snapshot.clone());
        // Unresolved requests per live item.
        let mut open: HashMap<&str, Vec<&UserActionRequestView>> = HashMap::new();
        for request in snapshot
            .user_action_requests
            .iter()
            .filter(|request| request.is_unresolved())
        {
            if snapshot
                .item(&request.work_item_id)
                .is_some_and(|item| item.archived_at.is_none())
            {
                open.entry(request.work_item_id.as_str())
                    .or_default()
                    .push(request);
            }
        }
        let mut notices = Vec::new();
        for (item_id, requests) in &open {
            let previous = seen.items.get(*item_id).map_or(0, |item| item.open.len());
            let fresh = requests
                .iter()
                .any(|request| !seen.notified.contains(&request.id));
            if !first && previous == 0 && fresh {
                let newest = requests
                    .iter()
                    .max_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)))
                    .expect("non-empty");
                notices.push(UserActionNotice {
                    space_id: space_id.to_owned(),
                    work_item_id: (*item_id).to_owned(),
                    item_title: snapshot
                        .item(item_id)
                        .map(|item| item.title.clone())
                        .unwrap_or_default(),
                    count: requests.len(),
                    text: newest.text.clone(),
                    chat_id: newest
                        .origin_chat_available
                        .then(|| newest.chat_id.clone())
                        .filter(|id| !id.is_empty()),
                });
            }
        }
        // Remember the open sets and every reported id. Ids are kept across
        // frames that drop them (a flapping frame must not re-notify); only
        // past a generous bound are ids the snapshot no longer lists forgotten
        // (resolution is monotone, so they can never reopen).
        seen.items = open
            .iter()
            .map(|(item_id, requests)| {
                (
                    (*item_id).to_owned(),
                    ItemSeen {
                        open: requests.iter().map(|r| r.id.clone()).collect(),
                    },
                )
            })
            .collect();
        if seen.notified.len() > MAX_REMEMBERED_REQUEST_IDS {
            let listed: HashSet<&str> = snapshot
                .user_action_requests
                .iter()
                .map(|request| request.id.as_str())
                .collect();
            seen.notified.retain(|id| listed.contains(id.as_str()));
        }
        // Every unresolved request seen counts as reported, including those
        // on archived or deleted items (restoring an item is not news).
        seen.notified.extend(
            snapshot
                .user_action_requests
                .iter()
                .filter(|request| request.is_unresolved())
                .map(|request| request.id.clone()),
        );
        notices.sort_by(|a, b| a.work_item_id.cmp(&b.work_item_id));
        notices
    }

    /// Drop Spaces that are no longer watched (re-adding one seeds again).
    pub(crate) fn retain_spaces(&mut self, keep: impl Fn(&str) -> bool) {
        self.spaces.retain(|space_id, _| keep(space_id));
    }
}

/// Which outputs a notice may produce under the user's settings: the
/// `Request` chime (master sound + input sound) and the desktop banner
/// (notifications on, and not suppressed by background-only while a Mast
/// window is focused). Required Actions list every request regardless.
pub(crate) fn notice_outputs(
    settings: &crate::settings::UiSettings,
    app_focused: bool,
) -> (bool, bool) {
    let sound = settings.session_sound_enabled(crate::sound::Sound::Request);
    let banner =
        settings.notifications_enabled && !(settings.notifications_background_only && app_focused);
    (sound, banner)
}

// --------------------------------------------------------------------------
// Fixture
// --------------------------------------------------------------------------

/// The store fixture plus Milestone 4 rows:
///
/// ```text
/// UI          open "Approve the color palette" (chat-ui, info)        -> 1 unresolved
///             open duplicate of it (same dedupe key)                   (folded)
/// API         open "Pick the auth provider" from chat-ghost (deleted Chat)
/// Auth        resolved "Rotate the staging key" (note "Done")
/// proposals:  pending on UI from chat-ui; pending on the Space root from chat-api;
///             rejected on API.
/// ```
#[cfg(test)]
pub(crate) mod fixture {
    use super::super::store::fixture_ids::*;
    use super::super::store::fixture_snapshot;
    use super::*;
    use mast_proto::{
        InstructionProposalOrigin, InstructionProposalReview, InstructionProposalTarget,
        ProposalReviewState, UserActionRequestState, UserActionResolution, WorkItemEffectiveStatus,
        WorkOutlineActor,
    };

    pub(crate) const R_UI: &str = "req-ui";
    pub(crate) const R_UI_DUP: &str = "req-ui-dup";
    pub(crate) const R_API: &str = "req-api";
    pub(crate) const R_AUTH: &str = "req-auth";
    pub(crate) const P_UI: &str = "prop-ui";
    pub(crate) const P_ROOT: &str = "prop-root";
    pub(crate) const P_API: &str = "prop-api";
    pub(crate) const NOW: i64 = 1_760_000_000_000;

    pub(crate) fn request(
        id: &str,
        space_id: &str,
        item: &str,
        chat: &str,
        text: &str,
    ) -> UserActionRequestView {
        UserActionRequestView {
            id: id.into(),
            space_id: space_id.into(),
            work_item_id: item.into(),
            chat_id: chat.into(),
            origin_kind: Default::default(),
            message_id: None,
            tool_call_id: None,
            text: text.into(),
            info: None,
            dedupe_key: None,
            created_by: Some(agent(chat)),
            created_at: NOW,
            state: UserActionRequestState::Open,
            resolution: None,
            deduped: false,
            origin_chat_available: true,
        }
    }

    pub(crate) fn agent(chat: &str) -> WorkOutlineActor {
        WorkOutlineActor {
            kind: WorkOutlineActorKind::Agent,
            device_id: "device-fixture".into(),
            chat_id: Some(chat.into()),
            message_id: None,
        }
    }

    pub(crate) fn resolve(request: &mut UserActionRequestView, at: i64, note: Option<&str>) {
        request.state = UserActionRequestState::Resolved;
        request.resolution = Some(UserActionResolution {
            by: WorkOutlineActor {
                kind: WorkOutlineActorKind::User,
                device_id: "device-fixture".into(),
                chat_id: None,
                message_id: None,
            },
            at,
            note: note.map(Into::into),
        });
    }

    pub(crate) fn proposal(
        id: &str,
        space_id: &str,
        item: Option<&str>,
        chat: &str,
        text: &str,
    ) -> WorkItemInstructionProposalView {
        WorkItemInstructionProposalView {
            id: id.into(),
            space_id: space_id.into(),
            target: match item {
                Some(item) => InstructionProposalTarget::work_item(item),
                None => InstructionProposalTarget::space_root(),
            },
            text: text.into(),
            proposed_by: Some(agent(chat)),
            source_ref: InstructionProposalOrigin {
                chat_id: Some(chat.into()),
                message_id: Some(format!("m-{id}")),
                tool_call_id: None,
            },
            created_at: NOW,
            review: None,
            status: InstructionProposalStatus::Pending,
        }
    }

    pub(crate) fn snapshot(space_id: &str) -> WorkOutlineSnapshot {
        let mut snapshot = fixture_snapshot(space_id);
        let mut ui = request(R_UI, space_id, UI, CHAT_UI, "Approve the color palette");
        ui.info = Some("Two candidates are in the design doc.".into());
        ui.dedupe_key = Some("palette".into());
        ui.created_at = NOW - 3_600_000;
        let mut dup = request(R_UI_DUP, space_id, UI, CHAT_UI, "Approve the color palette");
        dup.dedupe_key = Some("palette".into());
        dup.deduped = true;
        let mut api = request(R_API, space_id, API, CHAT_GHOST, "Pick the auth provider");
        api.origin_chat_available = false;
        let mut auth = request(
            R_AUTH,
            space_id,
            API_AUTH,
            CHAT_AUTH,
            "Rotate the staging key",
        );
        resolve(&mut auth, NOW + 10, Some("Done"));
        snapshot.user_action_requests = vec![ui, api, dup, auth];
        // API now has an unresolved request too.
        for item in &mut snapshot.items {
            if item.id == API {
                item.effective_status = WorkItemEffectiveStatus::UserActionRequired;
                item.rollup.unresolved_user_action_count = 1;
            }
        }
        let mut rejected = proposal(P_API, space_id, Some(API), CHAT_API, "Use REST only.");
        rejected.status = InstructionProposalStatus::Rejected;
        rejected.review = Some(InstructionProposalReview {
            state: ProposalReviewState::Rejected,
            by: WorkOutlineActor::default(),
            at: NOW + 5,
            reason: Some("GraphQL is planned".into()),
            instruction_id: None,
        });
        snapshot.instruction_proposals = vec![
            proposal(
                P_UI,
                space_id,
                Some(UI),
                CHAT_UI,
                "Use the design tokens for colors.",
            ),
            proposal(
                P_ROOT,
                space_id,
                None,
                CHAT_API,
                "Run the linters before committing.",
            ),
            rejected,
        ];
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;
    use crate::work_outline::store::fixture_ids::*;
    use mast_proto::ChatIndicator;

    fn chat(id: &str, archived: bool) -> OutlineChat {
        OutlineChat {
            id: id.into(),
            space_id: Some("s1".into()),
            parent_chat_id: None,
            title: format!("Chat {id}"),
            status: ChatIndicator::Idle,
            archived,
            stale: false,
            branch: None,
            harness: None,
            pr_number: None,
            visible: true,
            activity: 0,
        }
    }

    fn spaces(snapshot: WorkOutlineSnapshot) -> Vec<SpaceSnapshot> {
        vec![SpaceSnapshot {
            space_id: "s1".into(),
            label: "alpha".into(),
            snapshot: Arc::new(snapshot),
        }]
    }

    #[test]
    fn reply_message_quotes_a_short_request() {
        assert_eq!(
            reply_message("Approve the\n  color palette", "  Palette B  "),
            "Re: \"Approve the color palette\": Palette B"
        );
        let long = "word ".repeat(60);
        let message = reply_message(&long, "ok");
        let quote = message
            .strip_prefix("Re: \"")
            .and_then(|rest| rest.strip_suffix("\": ok"))
            .unwrap();
        assert!(quote.chars().count() <= REPLY_QUOTE_MAX_CHARS);
        assert!(quote.ends_with('\u{2026}'));
    }

    #[test]
    fn reply_is_unavailable_without_a_chat_or_a_note() {
        let chats = vec![chat(CHAT_UI, false)];
        let rows = collect_rows(&spaces(snapshot("s1")), &chats, &RequestScope::Global);
        let ui = rows.open.iter().find(|r| r.request_id == R_UI).unwrap();
        let api = rows.open.iter().find(|r| r.request_id == R_API).unwrap();
        assert_eq!(reply_unavailable_reason(ui, "yes"), None);
        assert_eq!(
            reply_unavailable_reason(ui, "  "),
            Some("Type a reply first")
        );
        assert!(
            reply_unavailable_reason(api, "yes").is_some_and(|reason| reason.contains("archived"))
        );
    }

    #[test]
    fn rows_fold_duplicates_and_flag_missing_chats() {
        let chats = vec![chat(CHAT_UI, false), chat(CHAT_AUTH, false)];
        let rows = collect_rows(&spaces(snapshot("s1")), &chats, &RequestScope::Global);
        let open: Vec<_> = rows.open.iter().map(|r| r.request_id.as_str()).collect();
        // Oldest first; the duplicate is folded into the canonical row.
        assert_eq!(open, [R_UI, R_API]);
        assert_eq!(rows.open[0].duplicates, 1);
        assert_eq!(rows.open[0].path, "Launch v2 › UI");
        assert_eq!(rows.open[0].chat_label, "Chat chat-ui");
        assert!(rows.open[0].chat_available);
        assert_eq!(
            rows.open[0].info.as_deref(),
            Some("Two candidates are in the design doc.")
        );
        // chat-ghost is deleted: inspectable, not openable.
        assert_eq!(rows.open[1].chat_label, CHAT_UNAVAILABLE);
        assert!(!rows.open[1].chat_available);
        assert_eq!(rows.resolved.len(), 1);
        let resolution = rows.resolved[0].resolution.as_ref().unwrap();
        assert_eq!(
            (resolution.by, resolution.note.as_deref()),
            ("you", Some("Done"))
        );
    }

    #[test]
    fn archived_origin_chat_reads_unavailable() {
        let chats = vec![chat(CHAT_UI, true)];
        let rows = collect_rows(&spaces(snapshot("s1")), &chats, &RequestScope::Global);
        assert_eq!(rows.open[0].chat_label, CHAT_UNAVAILABLE);
        assert!(!rows.open[0].chat_available);
    }

    #[test]
    fn scopes_limit_rows_to_a_space_or_a_subtree() {
        let spaces = spaces(snapshot("s1"));
        let api = RequestScope::Subtree {
            space_id: "s1".into(),
            item_id: API.into(),
        };
        let rows = collect_rows(&spaces, &[], &api);
        assert_eq!(rows.open.len(), 1);
        assert_eq!(rows.open[0].request_id, R_API);
        // Auth endpoints sits under API: its resolved request is history here.
        assert_eq!(rows.resolved[0].request_id, R_AUTH);
        let launch = RequestScope::Subtree {
            space_id: "s1".into(),
            item_id: LAUNCH.into(),
        };
        assert_eq!(collect_rows(&spaces, &[], &launch).open.len(), 2);
        assert!(
            collect_rows(&spaces, &[], &RequestScope::Space("other".into()))
                .open
                .is_empty()
        );
        assert_eq!(open_request_count(&spaces), 2);
    }

    #[test]
    fn multi_space_global_rows_name_the_space() {
        let mut both = spaces(snapshot("s1"));
        both.push(SpaceSnapshot {
            space_id: "s2".into(),
            label: "beta".into(),
            snapshot: Arc::new(WorkOutlineSnapshot::default()),
        });
        let rows = collect_rows(&both, &[], &RequestScope::Global);
        assert_eq!(rows.open[0].path, "alpha › Launch v2 › UI");
    }

    #[test]
    fn status_totals_read_like_the_spec() {
        let rollup = WorkItemRollup {
            active_count: 3,
            pending_count: 1,
            user_action_required_count: 1,
            ..Default::default()
        };
        let totals = status_totals(&rollup).unwrap();
        assert_eq!(totals.label(), "3 active \u{b7} 1 user action required");
        // A lone item shows nothing; a blocked lone item shows its total.
        let lone = WorkItemRollup {
            active_count: 1,
            ..Default::default()
        };
        assert_eq!(status_totals(&lone), None);
        let blocked = WorkItemRollup {
            active_count: 1,
            user_action_required_count: 1,
            ..Default::default()
        };
        assert_eq!(
            status_totals(&blocked).unwrap().label(),
            "1 active \u{b7} 1 user action required"
        );
        // Only pending/completed below: nothing to say.
        let quiet = WorkItemRollup {
            pending_count: 2,
            completed_count: 2,
            ..Default::default()
        };
        assert_eq!(status_totals(&quiet), None);
    }

    #[test]
    fn proposals_are_counted_per_target_and_chat() {
        let snapshot = snapshot("s1");
        assert_eq!(pending_proposal_count(&snapshot, Some(UI)), 1);
        assert_eq!(pending_proposal_count(&snapshot, None), 1);
        assert_eq!(pending_proposal_count(&snapshot, Some(API)), 0);
        assert_eq!(proposals_for(&snapshot, Some(API)).count(), 1);
        assert_eq!(pending_proposals_from_chat(&snapshot, CHAT_API).len(), 1);
        assert_eq!(proposals_label(1), "1 proposed instruction");
        assert_eq!(proposals_label(3), "3 proposed instructions");
    }

    fn base() -> WorkOutlineSnapshot {
        let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
        snapshot.user_action_requests.clear();
        snapshot
    }

    fn with(requests: Vec<UserActionRequestView>) -> Arc<WorkOutlineSnapshot> {
        let mut snapshot = base();
        snapshot.user_action_requests = requests;
        Arc::new(snapshot)
    }

    #[test]
    fn notifier_fires_once_on_zero_to_one_and_seeds_silently() {
        let mut notifier = UserActionNotifier::default();
        // First observation seeds silently, even with open requests.
        let boot = with(vec![request("a", "s1", API, CHAT_API, "Boot")]);
        assert!(notifier.observe("s1", &boot).is_empty());
        let empty = with(vec![]);
        assert!(notifier.observe("s1", &empty).is_empty());
        let one = with(vec![request("r1", "s1", UI, CHAT_UI, "Approve it")]);
        let notices = notifier.observe("s1", &one);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].work_item_id, UI);
        assert_eq!(notices[0].count, 1);
        assert_eq!(notices[0].title(), "UI");
        assert_eq!(notices[0].body(), "Approve it");
        assert_eq!(notices[0].chat_id.as_deref(), Some(CHAT_UI));
        // The same frame again, and an equal open set in a new frame: quiet.
        assert!(notifier.observe("s1", &one).is_empty());
        let same = with(vec![request("r1", "s1", UI, CHAT_UI, "Approve it")]);
        assert!(notifier.observe("s1", &same).is_empty());
        // A second request while one is open: still nonzero, quiet.
        let two = with(vec![
            request("r1", "s1", UI, CHAT_UI, "Approve it"),
            request("r2", "s1", UI, CHAT_UI, "And this"),
        ]);
        assert!(notifier.observe("s1", &two).is_empty());
    }

    #[test]
    fn notifier_coalesces_a_batch_per_item() {
        let mut notifier = UserActionNotifier::default();
        notifier.observe("s1", &with(vec![]));
        let mut later = request("r2", "s1", UI, CHAT_UI, "Second");
        later.created_at = NOW + 1;
        let batch = with(vec![
            request("r1", "s1", UI, CHAT_UI, "First"),
            later,
            request("r3", "s1", API, CHAT_API, "Other item"),
        ]);
        let notices = notifier.observe("s1", &batch);
        assert_eq!(notices.len(), 2, "one event per item");
        let ui = notices.iter().find(|n| n.work_item_id == UI).unwrap();
        assert_eq!(ui.count, 2);
        assert_eq!(ui.body(), "2 actions required");
        assert_eq!(ui.text, "Second");
    }

    #[test]
    fn notifier_ignores_retries_duplicates_and_flapping_but_rearms_after_zero() {
        let mut notifier = UserActionNotifier::default();
        notifier.observe("s1", &with(vec![]));
        let r1 = request("r1", "s1", UI, CHAT_UI, "Approve");
        assert_eq!(notifier.observe("s1", &with(vec![r1.clone()])).len(), 1);
        // A deduped retry (same key, new id) never counts.
        let mut dup = request("r1b", "s1", UI, CHAT_UI, "Approve");
        dup.deduped = true;
        assert!(
            notifier
                .observe("s1", &with(vec![r1.clone(), dup]))
                .is_empty()
        );
        // A flapping frame that drops and restores the same id: quiet.
        assert!(notifier.observe("s1", &with(vec![])).is_empty());
        assert!(notifier.observe("s1", &with(vec![r1.clone()])).is_empty());
        // Resolved -> zero; a new request fires again.
        let mut resolved = r1.clone();
        resolve(&mut resolved, NOW + 1, None);
        assert!(
            notifier
                .observe("s1", &with(vec![resolved.clone()]))
                .is_empty()
        );
        let r2 = request("r2", "s1", UI, CHAT_UI, "Approve again");
        let notices = notifier.observe("s1", &with(vec![resolved, r2]));
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].body(), "Approve again");
    }

    #[test]
    fn notifier_skips_unavailable_snapshots_and_unavailable_chats() {
        let mut notifier = UserActionNotifier::default();
        notifier.observe("s1", &with(vec![]));
        let gap = Arc::new(WorkOutlineSnapshot::unavailable("s1", true, 2));
        assert!(notifier.observe("s1", &gap).is_empty());
        let mut r = request("r1", "s1", UI, CHAT_GHOST, "Gone chat");
        r.origin_chat_available = false;
        let notices = notifier.observe("s1", &with(vec![r]));
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].chat_id, None);
        // Dropping the Space and re-adding it seeds again (quiet).
        notifier.retain_spaces(|_| false);
        let r3 = request("r3", "s1", API, CHAT_API, "New");
        assert!(notifier.observe("s1", &with(vec![r3])).is_empty());
    }

    #[test]
    fn restoring_an_archived_item_with_old_requests_stays_quiet() {
        let archived_ui = |requests: Vec<UserActionRequestView>, archived: bool| {
            let mut snapshot = base();
            for item in &mut snapshot.items {
                if item.id == UI {
                    item.archived_at = archived.then_some(NOW);
                }
            }
            snapshot.user_action_requests = requests;
            Arc::new(snapshot)
        };
        let mut notifier = UserActionNotifier::default();
        notifier.observe("s1", &with(vec![]));
        let r1 = request("r1", "s1", UI, CHAT_UI, "Old question");
        // Seen while the item is archived: no alert, and none on restore.
        assert!(
            notifier
                .observe("s1", &archived_ui(vec![r1.clone()], true))
                .is_empty()
        );
        assert!(
            notifier
                .observe("s1", &archived_ui(vec![r1.clone()], false))
                .is_empty(),
            "a restore is not a new request"
        );
        // Back at zero, a genuinely new request on the restored item fires.
        let mut resolved = r1.clone();
        resolve(&mut resolved, NOW + 1, None);
        assert!(
            notifier
                .observe("s1", &archived_ui(vec![resolved.clone()], false))
                .is_empty()
        );
        let r2 = request("r2", "s1", UI, CHAT_UI, "New question");
        let notices = notifier.observe("s1", &archived_ui(vec![resolved, r2], false));
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].body(), "New question");
    }

    #[test]
    fn notice_outputs_respect_the_settings() {
        let mut settings = crate::settings::UiSettings::default();
        settings.sound_enabled = true;
        settings.sound_input_enabled = true;
        settings.notifications_enabled = true;
        settings.notifications_background_only = false;
        assert_eq!(notice_outputs(&settings, true), (true, true));
        settings.notifications_background_only = true;
        assert_eq!(notice_outputs(&settings, true), (true, false));
        assert_eq!(notice_outputs(&settings, false), (true, true));
        settings.sound_input_enabled = false;
        settings.notifications_enabled = false;
        assert_eq!(notice_outputs(&settings, false), (false, false));
        settings.sound_input_enabled = true;
        settings.sound_enabled = false;
        assert_eq!(notice_outputs(&settings, false).0, false);
    }

    #[test]
    fn age_uses_the_shared_relative_format() {
        let now = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(NOW).unwrap();
        assert_eq!(age_label(NOW - 3 * 3_600_000, now), "3h");
        assert_eq!(age_label(NOW, now), "now");
    }
}
