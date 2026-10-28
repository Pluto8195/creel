//! Pure Outline view model: flattens per-Space snapshots plus the live Chat
//! list into the visible row list the virtualized Outline renders.
//!
//! Rules (spec "Integrate with the existing overview", m0-contract §7):
//! - More than one Space: one collapsible root per Space. One (or zero)
//!   Space: items sit at depth 0, no root row.
//! - Work items nest by the snapshot's repaired tree; Chats sit under the
//!   item they are associated with, in their own Space's snapshot only.
//! - Child Chats (`parent_chat_id`) nest under their parent Chat when the
//!   parent is shown, otherwise under their own association or
//!   Unassociated Chats. They never disappear.
//! - Every other visible Chat lands under "Unassociated Chats" (per Space;
//!   projectless Chats only in the All-spaces view). A Space whose outline
//!   is not loaded still shows all its Chats there.
//! - An assignment whose Chat row is gone renders as an inspectable
//!   "missing chat" row under its item.
//! - Archived items are excluded unless `show_archived`; completed items are
//!   excluded while `hide_completed`. An excluded item still shows when its
//!   child item has something visible, so nothing below it vanishes. An
//!   archived item's Chats alone do not keep it in view: they move to
//!   Unassociated Chats.
//! - A Space the user hid from the Outline (`OutlineSpace::hidden`, multi-
//!   Space only) renders nothing, and its Chats are skipped rather than
//!   moved to the projectless group. With `show_hidden_spaces` it shows as a
//!   bare (childless) root row so it can be unhidden.
//! - A non-empty query keeps matches, their ancestors, and everything under a
//!   matching item; collapse state is ignored while searching.
//!
//! Rows are keyed by stable strings ([`row_key`]) so selection and collapse
//! survive snapshot refreshes.
use std::collections::{HashMap, HashSet};

use zeron_proto::ChatIndicator;

use super::store::{
    WorkItemEffectiveStatus, WorkItemStatus, WorkItemView, WorkOutlineLoad, WorkOutlineSnapshot,
};

/// Key of the projectless "Unassociated Chats" group (All-spaces view).
pub(crate) const PROJECTLESS_GROUP_KEY: &str = "unassoc:";

/// Stable row keys. Items carry their Space so two Spaces can never collide.
pub(crate) mod row_key {
    pub(crate) fn space(space_id: &str) -> String {
        format!("space:{space_id}")
    }
    pub(crate) fn item(space_id: &str, item_id: &str) -> String {
        format!("item:{space_id}:{item_id}")
    }
    pub(crate) fn chat(chat_id: &str) -> String {
        format!("chat:{chat_id}")
    }
    pub(crate) fn unassociated(space_id: Option<&str>) -> String {
        match space_id {
            Some(space_id) => format!("unassoc:{space_id}"),
            None => super::PROJECTLESS_GROUP_KEY.to_string(),
        }
    }
    pub(crate) fn notice(space_id: &str) -> String {
        format!("notice:{space_id}")
    }
}

/// One Space the Outline shows, with its current load state.
#[derive(Debug, Clone)]
pub(crate) struct OutlineSpace {
    pub id: String,
    pub label: String,
    pub load: WorkOutlineLoad,
    /// Hidden from the Outline by the user (device-local view state). Only
    /// meaningful with more than one Space.
    pub hidden: bool,
}

/// The Chat facts a row needs, copied in by the Overview (which owns link
/// status and staleness) so this module stays GUI-free.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OutlineChat {
    pub id: String,
    pub space_id: Option<String>,
    pub parent_chat_id: Option<String>,
    pub title: String,
    pub status: ChatIndicator,
    pub archived: bool,
    pub stale: bool,
    pub branch: Option<String>,
    /// Model/harness label (same text as the Overview's model badge).
    pub harness: Option<String>,
    pub pr_number: Option<u64>,
    /// Whether the Overview's Chat filters (archived, stale, repo) let it
    /// show. Hidden Chats are skipped silently; Chats missing from the list
    /// altogether are "dangling".
    pub visible: bool,
    /// Last activity, unix millis; newer sorts first.
    pub activity: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OutlineOptions {
    pub hide_completed: bool,
    pub show_archived: bool,
    /// Trimmed, lowercased search text; empty matches everything.
    pub query: String,
    /// The All-spaces view: projectless Chats are listed.
    pub all_spaces: bool,
    /// Show hidden Spaces as bare, dimmed root rows (to unhide them).
    pub show_hidden_spaces: bool,
    /// Fold an item's only live Chat into the item's row. The Outline view
    /// turns it on; model-level callers get the plain item + Chat rows.
    pub compact_single_chat: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoticeTone {
    Muted,
    Warning,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ItemRow {
    pub space_id: String,
    pub item_id: String,
    pub title: String,
    pub status: WorkItemStatus,
    pub effective_status: WorkItemEffectiveStatus,
    pub unresolved_actions: u32,
    pub chat_count: u32,
    pub has_references: bool,
    /// Associated Chats whose captured execution configuration differs from
    /// the item's current one (`executionExceptions`).
    pub execution_exceptions: u32,
    /// Subtree rollup (descendant status totals, Milestone 4).
    pub rollup: zeron_proto::WorkItemRollup,
    /// Pending instruction proposals targeting this item (Milestone 4).
    pub pending_proposals: usize,
    pub archived: bool,
    /// Shown only because something below it is visible.
    pub kept_for_descendants: bool,
    /// The item's only Chat, folded into this row (see [`compactable_chat`]).
    /// Its own row is not emitted; its key resolves to this row.
    pub compact_chat: Option<ChatRow>,
}

impl ItemRow {
    pub(crate) fn needs_user_action(&self) -> bool {
        self.effective_status == WorkItemEffectiveStatus::UserActionRequired
            || self.unresolved_actions > 0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChatRow {
    pub chat: OutlineChat,
    /// `(space id, item id)` the Chat is associated with, if any.
    pub work_item: Option<(String, String)>,
    /// The assignment survives but the Chat row is gone.
    pub dangling: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum OutlineRowKind {
    Space {
        space_id: String,
        label: String,
        /// Hidden from the Outline, shown only while hidden projects are
        /// revealed (dimmed, no children).
        hidden: bool,
    },
    Item(ItemRow),
    Chat(ChatRow),
    Unassociated {
        space_id: Option<String>,
        label: String,
    },
    Notice {
        space_id: String,
        text: String,
        tone: NoticeTone,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OutlineRow {
    pub key: String,
    pub depth: u16,
    /// Index of the parent row in [`OutlineModel::rows`].
    pub parent: Option<usize>,
    pub has_children: bool,
    pub collapsed: bool,
    /// Direct children (items, chats, notices) whether or not collapsed.
    pub child_count: usize,
    pub kind: OutlineRowKind,
}

impl OutlineRow {
    pub(crate) fn item(&self) -> Option<&ItemRow> {
        match &self.kind {
            OutlineRowKind::Item(item) => Some(item),
            _ => None,
        }
    }

    pub(crate) fn chat(&self) -> Option<&ChatRow> {
        match &self.kind {
            OutlineRowKind::Chat(chat) => Some(chat),
            _ => None,
        }
    }

    /// The Chat a compact item row stands for.
    pub(crate) fn compact_chat(&self) -> Option<&ChatRow> {
        self.item().and_then(|item| item.compact_chat.as_ref())
    }

    /// The Chat Enter / click opens: a Chat row's own, or a compact item
    /// row's folded one.
    pub(crate) fn open_chat(&self) -> Option<&ChatRow> {
        self.chat().or_else(|| self.compact_chat())
    }

    /// The Space this row belongs to (`None` for the projectless group and
    /// its Chats).
    pub(crate) fn space_id(&self) -> Option<&str> {
        match &self.kind {
            OutlineRowKind::Space { space_id, .. } | OutlineRowKind::Notice { space_id, .. } => {
                Some(space_id)
            }
            OutlineRowKind::Item(item) => Some(&item.space_id),
            OutlineRowKind::Chat(chat) => chat.chat.space_id.as_deref(),
            OutlineRowKind::Unassociated { space_id, .. } => space_id.as_deref(),
        }
    }
}

/// The flattened, visible rows plus lookups.
#[derive(Debug, Clone, Default)]
pub(crate) struct OutlineModel {
    pub rows: Vec<OutlineRow>,
    index: HashMap<String, usize>,
    /// Completed items hidden by `hide_completed` (the reveal control's count).
    pub hidden_completed: usize,
    pub multi_space: bool,
    /// Spaces hidden from the Outline (multi-Space only; the toolbar count).
    pub hidden_spaces: usize,
}

impl OutlineModel {
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub(crate) fn position(&self, key: &str) -> Option<usize> {
        self.index.get(key).copied()
    }

    pub(crate) fn row(&self, key: &str) -> Option<&OutlineRow> {
        self.position(key).map(|ix| &self.rows[ix])
    }

    /// The key of the row that shows `key`: itself, or for a Chat folded
    /// into its item's compact row, the item's.
    pub(crate) fn canonical_key(&self, key: &str) -> Option<&str> {
        self.row(key).map(|row| row.key.as_str())
    }

    /// Keys of `ix`'s ancestors, nearest first.
    pub(crate) fn ancestor_keys(&self, ix: usize) -> Vec<&str> {
        let mut out = Vec::new();
        let mut cursor = self.rows.get(ix).and_then(|row| row.parent);
        while let Some(parent) = cursor {
            out.push(self.rows[parent].key.as_str());
            cursor = self.rows[parent].parent;
        }
        out
    }
}

// --------------------------------------------------------------------------
// Flatten
// --------------------------------------------------------------------------

/// Build tree node (pre-collapse). Rows are emitted from these.
struct Node {
    key: String,
    kind: OutlineRowKind,
    children: Vec<Node>,
}

struct Ctx<'a> {
    options: &'a OutlineOptions,
    chats: HashMap<&'a str, &'a OutlineChat>,
    /// Visible Chats per home row key (an item, a parent Chat, or an
    /// Unassociated group), newest first. Each home is consumed once.
    by_home: HashMap<String, Vec<&'a OutlineChat>>,
    hidden_completed: usize,
}

fn text_matches(query: &str, fields: &[Option<&str>]) -> bool {
    query.is_empty()
        || fields
            .iter()
            .flatten()
            .any(|field| field.to_lowercase().contains(query))
}

/// Flatten `spaces` and `chats` into visible rows. `collapsed` holds row
/// keys the user folded.
pub(crate) fn flatten(
    spaces: &[OutlineSpace],
    chats: &[OutlineChat],
    collapsed: &HashSet<String>,
    options: &OutlineOptions,
) -> OutlineModel {
    let chat_by_id: HashMap<&str, &OutlineChat> =
        chats.iter().map(|chat| (chat.id.as_str(), chat)).collect();
    let multi_space = spaces.len() > 1;
    // Hiding only applies to Space roots, which exist with several Spaces.
    let is_hidden = |space: &OutlineSpace| multi_space && space.hidden;
    let hidden_ids: HashSet<&str> = spaces
        .iter()
        .filter(|space| is_hidden(space))
        .map(|space| space.id.as_str())
        .collect();
    let space_ids: HashSet<&str> = spaces
        .iter()
        .filter(|space| !is_hidden(space))
        .map(|space| space.id.as_str())
        .collect();

    // chat id -> item id, per Space, from each ready snapshot.
    let mut associated: HashMap<&str, HashMap<&str, &str>> = HashMap::new();
    for space in spaces {
        if let Some(snapshot) = ready_snapshot(&space.load) {
            let map = associated.entry(space.id.as_str()).or_default();
            for item in &snapshot.items {
                for chat_id in &item.chat_ids {
                    map.insert(chat_id.as_str(), item.id.as_str());
                }
            }
        }
    }

    let mut by_home: HashMap<String, Vec<&OutlineChat>> = HashMap::new();
    for chat in chats.iter().filter(|chat| chat.visible) {
        if let Some(home) = chat_home(
            chat,
            &chat_by_id,
            &associated,
            &space_ids,
            &hidden_ids,
            options,
        ) {
            by_home.entry(home).or_default().push(chat);
        }
    }
    for list in by_home.values_mut() {
        list.sort_by(|a, b| b.activity.cmp(&a.activity).then_with(|| a.id.cmp(&b.id)));
    }

    let mut ctx = Ctx {
        options,
        chats: chat_by_id,
        by_home,
        hidden_completed: 0,
    };
    let root_matched = options.query.is_empty();
    let mut roots: Vec<Node> = Vec::new();
    for space in spaces {
        if is_hidden(space) {
            if options.show_hidden_spaces && options.query.is_empty() {
                roots.push(Node {
                    key: row_key::space(&space.id),
                    kind: OutlineRowKind::Space {
                        space_id: space.id.clone(),
                        label: space.label.clone(),
                        hidden: true,
                    },
                    children: Vec::new(),
                });
            }
            continue;
        }
        let mut children = space_children(space, &mut ctx, root_matched);
        if !multi_space {
            // Single Space: projectless Chats share its Unassociated group.
            if options.all_spaces {
                let extra = unassociated_chats(&mut ctx, None, root_matched);
                if !extra.is_empty() {
                    let key = row_key::unassociated(Some(&space.id));
                    if let Some(group) = children.iter_mut().find(|n| n.key == key) {
                        group.children.extend(extra);
                    } else {
                        children.push(unassociated_group(Some(&space.id), extra));
                    }
                }
            }
            roots.extend(children);
        } else if !children.is_empty() || options.query.is_empty() {
            roots.push(Node {
                key: row_key::space(&space.id),
                kind: OutlineRowKind::Space {
                    space_id: space.id.clone(),
                    label: space.label.clone(),
                    hidden: false,
                },
                children,
            });
        }
    }
    if (multi_space || spaces.is_empty()) && options.all_spaces {
        let extra = unassociated_chats(&mut ctx, None, root_matched);
        if !extra.is_empty() {
            roots.push(unassociated_group(None, extra));
        }
    }

    let searching = !options.query.is_empty();
    let mut model = OutlineModel {
        rows: Vec::new(),
        index: HashMap::new(),
        hidden_completed: ctx.hidden_completed,
        multi_space,
        hidden_spaces: hidden_ids.len(),
    };
    for node in roots {
        emit(node, 0, None, collapsed, searching, &mut model);
    }
    model
}

fn ready_snapshot(load: &WorkOutlineLoad) -> Option<&WorkOutlineSnapshot> {
    load.snapshot()
        .map(|snapshot| snapshot.as_ref())
        .filter(|snapshot| snapshot.available)
}

/// The row key a visible Chat nests under, or `None` when it is out of
/// scope (projectless outside the All-spaces view).
fn chat_home(
    chat: &OutlineChat,
    chats: &HashMap<&str, &OutlineChat>,
    associated: &HashMap<&str, HashMap<&str, &str>>,
    space_ids: &HashSet<&str>,
    hidden_ids: &HashSet<&str>,
    options: &OutlineOptions,
) -> Option<String> {
    // A hidden Space's Chats are not in the Outline at all (List and the
    // sidebar still show them).
    if chat
        .space_id
        .as_deref()
        .is_some_and(|space_id| hidden_ids.contains(space_id))
    {
        return None;
    }
    let item_of = |c: &OutlineChat| {
        c.space_id
            .as_deref()
            .and_then(|space_id| associated.get(space_id))
            .and_then(|map| map.get(c.id.as_str()).copied())
    };
    if let Some(parent) = chat.parent_chat_id.as_deref() {
        // Nest under the parent only when the parent itself is shown; a
        // parent cycle (bad data) falls back to the Chat's own home. A child
        // started on a different item than its parent (a coordinator's child)
        // lives under that item instead.
        let elsewhere = item_of(chat)
            .is_some_and(|own| chats.get(parent).and_then(|p| item_of(p)) != Some(own));
        let parent_shown = chats.get(parent).is_some_and(|p| {
            p.visible
                && match p.space_id.as_deref() {
                    Some(space_id) if hidden_ids.contains(space_id) => false,
                    Some(space_id) => space_ids.contains(space_id) || options.all_spaces,
                    None => options.all_spaces,
                }
        });
        if parent != chat.id && parent_shown && !elsewhere && !parent_chain_cycles(chat, chats) {
            return Some(row_key::chat(parent));
        }
    }
    match chat.space_id.as_deref() {
        Some(space_id) if space_ids.contains(space_id) => Some(
            match associated
                .get(space_id)
                .and_then(|map| map.get(chat.id.as_str()))
            {
                Some(item) => row_key::item(space_id, item),
                None => row_key::unassociated(Some(space_id)),
            },
        ),
        // Projectless, or a Space the Outline does not show.
        _ => options.all_spaces.then(|| row_key::unassociated(None)),
    }
}

fn parent_chain_cycles(chat: &OutlineChat, chats: &HashMap<&str, &OutlineChat>) -> bool {
    let mut seen: HashSet<&str> = HashSet::new();
    seen.insert(chat.id.as_str());
    let mut cursor = chat.parent_chat_id.as_deref();
    while let Some(id) = cursor {
        if !seen.insert(id) {
            return true;
        }
        cursor = chats.get(id).and_then(|c| c.parent_chat_id.as_deref());
    }
    false
}

fn space_children(space: &OutlineSpace, ctx: &mut Ctx<'_>, root_matched: bool) -> Vec<Node> {
    let mut children = Vec::new();
    let notice = match &space.load {
        WorkOutlineLoad::Loading => Some(("Loading outline…".to_string(), NoticeTone::Muted)),
        WorkOutlineLoad::Unsupported => Some((
            "This host needs an update to use the Work Outline".to_string(),
            NoticeTone::Warning,
        )),
        WorkOutlineLoad::Failed(reason) => Some((
            format!("Outline unavailable: {reason}"),
            NoticeTone::Warning,
        )),
        WorkOutlineLoad::Ready(snapshot) if !snapshot.available => {
            if snapshot.synced {
                Some((
                    "This Space is no longer available".to_string(),
                    NoticeTone::Muted,
                ))
            } else {
                Some(("Loading outline…".to_string(), NoticeTone::Muted))
            }
        }
        WorkOutlineLoad::Ready(_) => None,
    };
    if let Some((text, tone)) = notice.filter(|_| ctx.options.query.is_empty()) {
        children.push(Node {
            key: row_key::notice(&space.id),
            kind: OutlineRowKind::Notice {
                space_id: space.id.clone(),
                text,
                tone,
            },
            children: Vec::new(),
        });
    }
    if let Some(snapshot) = ready_snapshot(&space.load) {
        let items: HashMap<&str, &WorkItemView> = snapshot
            .items
            .iter()
            .map(|item| (item.id.as_str(), item))
            .collect();
        let mut seen: HashSet<&str> = HashSet::new();
        for root in &snapshot.root_work_item_ids {
            if let Some(node) = item_node(
                &space.id,
                root,
                &items,
                snapshot,
                ctx,
                root_matched,
                &mut seen,
            ) {
                children.push(node);
            }
        }
    }
    let unassociated = unassociated_chats(ctx, Some(&space.id), root_matched);
    if !unassociated.is_empty() {
        children.push(unassociated_group(Some(&space.id), unassociated));
    }
    children
}

fn item_node<'s>(
    space_id: &str,
    item_id: &'s str,
    items: &HashMap<&'s str, &'s WorkItemView>,
    snapshot: &WorkOutlineSnapshot,
    ctx: &mut Ctx<'_>,
    ancestor_matched: bool,
    seen: &mut HashSet<&'s str>,
) -> Option<Node> {
    let item = items.get(item_id)?;
    // Defensive: the materializer already breaks cycles.
    if !seen.insert(item.id.as_str()) {
        return None;
    }
    let options = ctx.options;
    let under_match = ancestor_matched
        || (!options.query.is_empty() && text_matches(&options.query, &[Some(&item.title)]));
    let mut children: Vec<Node> = Vec::new();
    for child in &item.child_ids {
        if let Some(node) = item_node(space_id, child, items, snapshot, ctx, under_match, seen) {
            children.push(node);
        }
    }
    // Associated Chats (newest first), then dangling assignments.
    let mut homed = ctx
        .by_home
        .remove(&row_key::item(space_id, &item.id))
        .unwrap_or_default();
    // An archived item with no visible child item stays hidden like any other
    // archived item: its Chats move to Unassociated Chats instead of pinning
    // the item in view (dimmed, and immovable since archived items cannot
    // move). The assignment itself is untouched.
    let archived_hidden =
        item.archived_at.is_some() && !options.show_archived && children.is_empty();
    if archived_hidden && !homed.is_empty() {
        let unassociated = ctx
            .by_home
            .entry(row_key::unassociated(Some(space_id)))
            .or_default();
        unassociated.extend(std::mem::take(&mut homed));
        unassociated.sort_by(|a, b| b.activity.cmp(&a.activity).then_with(|| a.id.cmp(&b.id)));
    }
    let mut chat_nodes: Vec<Node> = homed
        .into_iter()
        .filter_map(|chat| {
            chat_node(
                chat,
                Some((space_id.to_string(), item.id.clone())),
                ctx,
                under_match,
            )
        })
        .collect();
    for chat_id in &item.chat_ids {
        if !archived_hidden && !ctx.chats.contains_key(chat_id.as_str()) && options.query.is_empty()
        {
            chat_nodes.push(Node {
                key: row_key::chat(chat_id),
                kind: OutlineRowKind::Chat(ChatRow {
                    chat: missing_chat(chat_id, space_id),
                    work_item: Some((space_id.to_string(), item.id.clone())),
                    dangling: true,
                }),
                children: Vec::new(),
            });
        }
    }
    let completed = item.effective_status == WorkItemEffectiveStatus::Completed;
    let archived = item.archived_at.is_some();
    let self_visible =
        (!archived || options.show_archived) && (!completed || !options.hide_completed);
    let has_visible_below = !children.is_empty() || !chat_nodes.is_empty();
    if !(has_visible_below || (under_match && self_visible)) {
        if completed && !archived && options.hide_completed && options.query.is_empty() {
            ctx.hidden_completed += 1;
        }
        return None;
    }
    let compact_chat = options
        .compact_single_chat
        .then(|| compactable_chat(&children, &chat_nodes))
        .flatten();
    if compact_chat.is_some() {
        chat_nodes.clear();
    }
    children.extend(chat_nodes);
    Some(Node {
        key: row_key::item(space_id, &item.id),
        kind: OutlineRowKind::Item(ItemRow {
            space_id: space_id.to_string(),
            item_id: item.id.clone(),
            title: if item.title.trim().is_empty() {
                "Untitled".to_string()
            } else {
                item.title.clone()
            },
            status: item.status,
            effective_status: item.effective_status,
            unresolved_actions: item.rollup.unresolved_user_action_count,
            chat_count: item
                .rollup
                .associated_chat_count
                .max(item.chat_ids.len() as u32),
            has_references: !item.external_reference_ids.is_empty(),
            execution_exceptions: snapshot.execution_exception_chats(&item.id).len() as u32,
            rollup: item.rollup.clone(),
            pending_proposals: super::required_actions::pending_proposal_count(
                snapshot,
                Some(&item.id),
            ),
            archived,
            kept_for_descendants: !self_visible,
            compact_chat,
        }),
        children,
    })
}

/// A lone, live Chat is folded into its item's row. Anything that needs its
/// own row keeps the item and Chat rows apart: child items, a second Chat, a
/// sub-chat nested under the Chat, an archived Chat (shown only with
/// "show archived") or a missing one. Required Actions and proposals do not
/// expand it: the row already carries the attention indicator and chips.
fn compactable_chat(item_children: &[Node], chat_nodes: &[Node]) -> Option<ChatRow> {
    match (item_children, chat_nodes) {
        ([], [only]) if only.children.is_empty() => match &only.kind {
            OutlineRowKind::Chat(chat) if !chat.dangling && !chat.chat.archived => {
                Some(chat.clone())
            }
            _ => None,
        },
        _ => None,
    }
}

fn missing_chat(chat_id: &str, space_id: &str) -> OutlineChat {
    OutlineChat {
        id: chat_id.to_string(),
        space_id: Some(space_id.to_string()),
        parent_chat_id: None,
        title: "Missing chat".to_string(),
        status: ChatIndicator::Idle,
        archived: false,
        stale: false,
        branch: None,
        harness: None,
        pr_number: None,
        visible: true,
        activity: 0,
    }
}

fn chat_node(
    chat: &OutlineChat,
    work_item: Option<(String, String)>,
    ctx: &mut Ctx<'_>,
    ancestor_matched: bool,
) -> Option<Node> {
    let options = ctx.options;
    let matched = ancestor_matched
        || text_matches(&options.query, &[Some(&chat.title), chat.branch.as_deref()]);
    let kids = ctx
        .by_home
        .remove(&row_key::chat(&chat.id))
        .unwrap_or_default();
    let children: Vec<Node> = kids
        .into_iter()
        .filter_map(|child| chat_node(child, work_item.clone(), ctx, matched))
        .collect();
    if !matched && children.is_empty() {
        return None;
    }
    Some(Node {
        key: row_key::chat(&chat.id),
        kind: OutlineRowKind::Chat(ChatRow {
            chat: chat.clone(),
            work_item,
            dangling: false,
        }),
        children,
    })
}

fn unassociated_chats(ctx: &mut Ctx<'_>, space_id: Option<&str>, matched: bool) -> Vec<Node> {
    let list = ctx
        .by_home
        .remove(&row_key::unassociated(space_id))
        .unwrap_or_default();
    list.into_iter()
        .filter_map(|chat| chat_node(chat, None, ctx, matched))
        .collect()
}

fn unassociated_group(space_id: Option<&str>, children: Vec<Node>) -> Node {
    Node {
        key: row_key::unassociated(space_id),
        kind: OutlineRowKind::Unassociated {
            space_id: space_id.map(str::to_string),
            label: if space_id.is_some() {
                "Unassociated Chats".to_string()
            } else {
                "Unassociated Chats · No project".to_string()
            },
        },
        children,
    }
}

fn emit(
    node: Node,
    depth: u16,
    parent: Option<usize>,
    collapsed: &HashSet<String>,
    searching: bool,
    model: &mut OutlineModel,
) {
    let has_children = !node.children.is_empty();
    let is_collapsed = has_children && !searching && collapsed.contains(&node.key);
    let ix = model.rows.len();
    model.index.insert(node.key.clone(), ix);
    if let OutlineRowKind::Item(ItemRow {
        compact_chat: Some(chat),
        ..
    }) = &node.kind
    {
        model.index.insert(row_key::chat(&chat.chat.id), ix);
    }
    model.rows.push(OutlineRow {
        key: node.key,
        depth,
        parent,
        has_children,
        collapsed: is_collapsed,
        child_count: node.children.len(),
        kind: node.kind,
    });
    if !is_collapsed {
        for child in node.children {
            emit(child, depth + 1, Some(ix), collapsed, searching, model);
        }
    }
}

// --------------------------------------------------------------------------
// Tree queries over one snapshot
// --------------------------------------------------------------------------

/// `item_id`'s ancestors and itself, root first. Cycle-guarded.
pub(crate) fn item_path<'a>(
    snapshot: &'a WorkOutlineSnapshot,
    item_id: &str,
) -> Vec<&'a WorkItemView> {
    let items: HashMap<&str, &WorkItemView> = snapshot
        .items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect();
    let mut path = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = items.get(item_id).copied();
    while let Some(item) = cursor {
        if !seen.insert(item.id.as_str()) {
            break;
        }
        path.push(item);
        cursor = item
            .parent_work_item_id
            .as_deref()
            .and_then(|parent| items.get(parent).copied());
    }
    path.reverse();
    path
}

/// Joined breadcrumb text.
pub(crate) fn path_label<'a>(titles: impl IntoIterator<Item = &'a str>) -> String {
    titles
        .into_iter()
        .map(|title| {
            if title.trim().is_empty() {
                "Untitled"
            } else {
                title
            }
        })
        .collect::<Vec<_>>()
        .join(" › ")
}

/// Every descendant id of `item_id` (not including itself).
pub(crate) fn descendants(snapshot: &WorkOutlineSnapshot, item_id: &str) -> HashSet<String> {
    let items: HashMap<&str, &WorkItemView> = snapshot
        .items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect();
    let mut out = HashSet::new();
    let mut stack: Vec<&str> = items
        .get(item_id)
        .map(|item| item.child_ids.iter().map(String::as_str).collect())
        .unwrap_or_default();
    while let Some(id) = stack.pop() {
        if id == item_id || !out.insert(id.to_string()) {
            continue;
        }
        if let Some(item) = items.get(id) {
            stack.extend(item.child_ids.iter().map(String::as_str));
        }
    }
    out
}

/// A destination in the move / associate pickers. `item_id == None` is the
/// Space root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Destination {
    pub item_id: Option<String>,
    pub title: String,
    pub depth: u32,
}

/// Valid parents for moving `moving` (or, with `None`, any live item): the
/// Space root plus every non-archived item except `moving` and its
/// descendants, in outline order.
pub(crate) fn destinations(
    snapshot: &WorkOutlineSnapshot,
    moving: Option<&str>,
) -> Vec<Destination> {
    let excluded: HashSet<String> = moving
        .map(|id| {
            let mut set = descendants(snapshot, id);
            set.insert(id.to_string());
            set
        })
        .unwrap_or_default();
    let mut out = Vec::new();
    if moving.is_some() {
        out.push(Destination {
            item_id: None,
            title: "Top level".to_string(),
            depth: 0,
        });
    }
    let offset = u32::from(moving.is_some());
    for item in &snapshot.items {
        if item.archived_at.is_some() || excluded.contains(&item.id) {
            continue;
        }
        out.push(Destination {
            item_id: Some(item.id.clone()),
            title: if item.title.trim().is_empty() {
                "Untitled".to_string()
            } else {
                item.title.clone()
            },
            depth: item.depth + offset,
        });
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use super::super::store::fixture_ids::*;
    use super::super::store::{WorkItemChatAssignmentState, fixture_snapshot};
    use super::*;

    pub(crate) fn chat(id: &str, space: Option<&str>) -> OutlineChat {
        OutlineChat {
            id: id.into(),
            space_id: space.map(Into::into),
            parent_chat_id: None,
            title: format!("Chat {id}"),
            status: ChatIndicator::Idle,
            archived: false,
            stale: false,
            branch: None,
            harness: None,
            pr_number: None,
            visible: true,
            activity: 0,
        }
    }

    pub(crate) fn ready(space: &str, label: &str, snapshot: WorkOutlineSnapshot) -> OutlineSpace {
        OutlineSpace {
            id: space.into(),
            label: label.into(),
            load: WorkOutlineLoad::Ready(Arc::new(snapshot)),
            hidden: false,
        }
    }

    /// The fixture's live chats (all but `chat-ghost`) in `space`.
    pub(crate) fn fixture_chats(space: &str) -> Vec<OutlineChat> {
        [
            CHAT_API,
            CHAT_AUTH,
            CHAT_UI,
            CHAT_LOOSE_A,
            CHAT_LOOSE_B,
            CHAT_ORPHAN,
        ]
        .iter()
        .map(|id| chat(id, Some(space)))
        .collect()
    }

    fn keys(model: &OutlineModel) -> Vec<(u16, String)> {
        model
            .rows
            .iter()
            .map(|row| (row.depth, row.key.clone()))
            .collect()
    }

    fn opts() -> OutlineOptions {
        OutlineOptions {
            all_spaces: true,
            ..Default::default()
        }
    }

    #[test]
    fn single_space_flattens_three_levels_with_chats_dangling_and_unassociated() {
        let spaces = [ready("s1", "Repo", fixture_snapshot("s1"))];
        let model = flatten(&spaces, &fixture_chats("s1"), &HashSet::new(), &opts());
        let item = |id: &str| row_key::item("s1", id);
        let chat = |id: &str| row_key::chat(id);
        assert!(!model.multi_space);
        assert_eq!(
            keys(&model),
            vec![
                (0, item(LAUNCH)),
                (1, item(API)),
                (2, item(API_AUTH)),
                (3, chat(CHAT_AUTH)),
                (2, chat(CHAT_API)),
                (2, chat(CHAT_GHOST)),
                (1, item(UI)),
                (2, chat(CHAT_UI)),
                (0, item(HOUSEKEEPING)),
                (0, row_key::unassociated(Some("s1"))),
                (1, chat(CHAT_LOOSE_A)),
                (1, chat(CHAT_LOOSE_B)),
                (1, chat(CHAT_ORPHAN)),
            ],
            "archived `Retired experiments` is excluded by default"
        );
        let ghost = model.row(&chat(CHAT_GHOST)).unwrap().chat().unwrap();
        assert!(ghost.dangling, "a missing chat stays inspectable");
        let ui = model.row(&item(UI)).unwrap().item().unwrap();
        assert!(ui.needs_user_action());
        assert_eq!(ui.chat_count, 1);
        let api = model.row(&item(API)).unwrap().item().unwrap();
        assert!(api.has_references);
        // Parent links point at the right rows.
        let auth = model.position(&chat(CHAT_AUTH)).unwrap();
        assert_eq!(
            model.ancestor_keys(auth),
            vec![
                item(API_AUTH).as_str(),
                item(API).as_str(),
                item(LAUNCH).as_str()
            ]
        );
    }

    #[test]
    fn multi_space_renders_one_collapsible_root_per_space_and_projectless_group() {
        let mut chats = fixture_chats("s1");
        chats.push(chat("b-chat", Some("s2")));
        chats.push(chat("loose", None));
        let spaces = [
            ready("s1", "Alpha", fixture_snapshot("s1")),
            OutlineSpace {
                id: "s2".into(),
                label: "Beta".into(),
                load: WorkOutlineLoad::Loading,
                hidden: false,
            },
        ];
        let model = flatten(&spaces, &chats, &HashSet::new(), &opts());
        assert!(model.multi_space);
        let roots: Vec<&str> = model
            .rows
            .iter()
            .filter(|row| row.depth == 0)
            .map(|row| row.key.as_str())
            .collect();
        assert_eq!(roots, ["space:s1", "space:s2", PROJECTLESS_GROUP_KEY]);
        // An unloaded Space still shows its chats, unassociated, with a notice.
        let s2 = model.position("space:s2").unwrap();
        assert_eq!(model.rows[s2 + 1].key, row_key::notice("s2"));
        assert_eq!(model.rows[s2 + 2].key, row_key::unassociated(Some("s2")));
        assert_eq!(model.rows[s2 + 3].key, row_key::chat("b-chat"));
        assert_eq!(model.rows[s2 + 3].depth, 2);
        // Projectless only in the All-spaces view.
        assert!(model.row(&row_key::chat("loose")).is_some());
        let scoped = flatten(&spaces, &chats, &HashSet::new(), &OutlineOptions::default());
        assert!(scoped.row(&row_key::chat("loose")).is_none());
        assert!(scoped.row(PROJECTLESS_GROUP_KEY).is_none());

        // Collapsing a Space root hides its rows but keeps the root.
        let collapsed: HashSet<String> = ["space:s1".to_string()].into();
        let folded = flatten(&spaces, &chats, &collapsed, &opts());
        let root = folded.row("space:s1").unwrap();
        assert!(root.collapsed && root.has_children);
        assert!(folded.row(&row_key::item("s1", LAUNCH)).is_none());
        assert!(folded.row("space:s2").is_some());
    }

    #[test]
    fn hidden_spaces_render_nothing_unless_revealed_and_never_leak_chats() {
        let mut chats = fixture_chats("s1");
        let mut b_child = chat("b-child", Some("s2"));
        b_child.parent_chat_id = Some(CHAT_API.into());
        chats.push(chat("b-chat", Some("s2")));
        chats.push(b_child);
        chats.push(chat("loose", None));
        let mut s1 = ready("s1", "Alpha", fixture_snapshot("s1"));
        s1.hidden = true;
        let spaces = [s1.clone(), ready("s2", "Beta", fixture_snapshot("s2"))];
        let model = flatten(&spaces, &chats, &HashSet::new(), &opts());
        assert!(model.multi_space, "a hidden Space still counts");
        assert_eq!(model.hidden_spaces, 1);
        assert!(model.row("space:s1").is_none());
        assert!(model.row(&row_key::item("s1", LAUNCH)).is_none());
        // s1's Chats are gone from the Outline, not moved to the projectless
        // group; a child Chat of a hidden parent falls back to its own home.
        assert!(model.row(&row_key::chat(CHAT_API)).is_none());
        assert!(model.row(&row_key::chat(CHAT_LOOSE_A)).is_none());
        let child = model.position(&row_key::chat("b-child")).unwrap();
        assert_eq!(
            model.rows[model.rows[child].parent.unwrap()].key,
            row_key::unassociated(Some("s2"))
        );
        assert!(model.row(&row_key::chat("loose")).is_some());
        assert!(model.row("space:s2").is_some());

        // Revealed: a bare, childless, flagged root row.
        let revealed = OutlineOptions {
            show_hidden_spaces: true,
            ..opts()
        };
        let model = flatten(&spaces, &chats, &HashSet::new(), &revealed);
        let row = model.row("space:s1").unwrap();
        assert!(!row.has_children);
        assert!(matches!(
            row.kind,
            OutlineRowKind::Space { hidden: true, .. }
        ));
        assert!(model.row(&row_key::chat(CHAT_API)).is_none());

        // One Space: hiding does not apply (there is no root row).
        let model = flatten(&[s1], &fixture_chats("s1"), &HashSet::new(), &opts());
        assert_eq!(model.hidden_spaces, 0);
        assert!(model.row(&row_key::item("s1", LAUNCH)).is_some());
    }

    #[test]
    fn single_space_folds_projectless_chats_into_its_unassociated_group() {
        let mut chats = fixture_chats("s1");
        chats.push(chat("loose", None));
        let spaces = [ready("s1", "Repo", fixture_snapshot("s1"))];
        let model = flatten(&spaces, &chats, &HashSet::new(), &opts());
        let loose = model.position(&row_key::chat("loose")).unwrap();
        assert_eq!(
            model.rows[model.rows[loose].parent.unwrap()].key,
            row_key::unassociated(Some("s1"))
        );
        // No spaces at all: a lone projectless group.
        let model = flatten(&[], &[chat("loose", None)], &HashSet::new(), &opts());
        assert_eq!(
            keys(&model),
            vec![
                (0, PROJECTLESS_GROUP_KEY.to_string()),
                (1, row_key::chat("loose"))
            ]
        );
    }

    #[test]
    fn a_child_started_on_another_item_lives_under_that_item() {
        let mut chats = fixture_chats("s1");
        // A coordinator's child: parent on w-api, the child itself on w-ui.
        chats
            .iter_mut()
            .find(|c| c.id == CHAT_UI)
            .unwrap()
            .parent_chat_id = Some(CHAT_API.into());
        // A subagent on its parent's own item still nests under the parent.
        let mut same_item = chat(CHAT_GHOST, Some("s1"));
        same_item.parent_chat_id = Some(CHAT_API.into());
        chats.push(same_item);
        let spaces = [ready("s1", "Repo", fixture_snapshot("s1"))];
        let model = flatten(&spaces, &chats, &HashSet::new(), &opts());
        let parent_of = |key: String| {
            let ix = model.position(&key).unwrap();
            model.rows[model.rows[ix].parent.unwrap()].key.clone()
        };
        assert_eq!(parent_of(row_key::chat(CHAT_UI)), row_key::item("s1", UI));
        assert_eq!(
            parent_of(row_key::chat(CHAT_GHOST)),
            row_key::chat(CHAT_API)
        );
    }

    #[test]
    fn child_chats_nest_under_a_shown_parent_otherwise_under_their_own_home() {
        let mut chats = fixture_chats("s1");
        let mut child = chat("child", Some("s1"));
        child.parent_chat_id = Some(CHAT_API.into());
        chats.push(child);
        let spaces = [ready("s1", "Repo", fixture_snapshot("s1"))];
        let model = flatten(&spaces, &chats, &HashSet::new(), &opts());
        let ix = model.position(&row_key::chat("child")).unwrap();
        assert_eq!(
            model.rows[model.rows[ix].parent.unwrap()].key,
            row_key::chat(CHAT_API)
        );
        assert!(model.row(&row_key::chat(CHAT_API)).unwrap().has_children);

        // Parent hidden (archived + filtered): the child falls back to
        // Unassociated rather than vanishing.
        chats.iter_mut().find(|c| c.id == CHAT_API).unwrap().visible = false;
        let model = flatten(&spaces, &chats, &HashSet::new(), &opts());
        let ix = model.position(&row_key::chat("child")).unwrap();
        assert_eq!(
            model.rows[model.rows[ix].parent.unwrap()].key,
            row_key::unassociated(Some("s1"))
        );
        // A filtered (not missing) chat is not shown as dangling.
        assert!(model.row(&row_key::chat(CHAT_API)).is_none());
    }

    #[test]
    fn hide_completed_hides_leaf_completed_items_and_counts_them_but_keeps_ones_with_chats() {
        let spaces = [ready("s1", "Repo", fixture_snapshot("s1"))];
        let mut chats = fixture_chats("s1");
        let options = OutlineOptions {
            hide_completed: true,
            ..opts()
        };
        // `Auth endpoints` is completed but has a visible chat: kept.
        let model = flatten(&spaces, &chats, &HashSet::new(), &options);
        let auth = model.row(&row_key::item("s1", API_AUTH)).unwrap();
        assert!(auth.item().unwrap().kept_for_descendants);
        assert_eq!(model.hidden_completed, 0);
        // Its chat filtered out (stale): the completed item hides and counts.
        chats
            .iter_mut()
            .find(|c| c.id == CHAT_AUTH)
            .unwrap()
            .visible = false;
        let model = flatten(&spaces, &chats, &HashSet::new(), &options);
        assert!(model.row(&row_key::item("s1", API_AUTH)).is_none());
        assert_eq!(model.hidden_completed, 1);
        // Reveal: shown again.
        let revealed = flatten(&spaces, &chats, &HashSet::new(), &opts());
        assert!(revealed.row(&row_key::item("s1", API_AUTH)).is_some());
        // Archived items appear only with show_archived.
        let archived = flatten(
            &spaces,
            &chats,
            &HashSet::new(),
            &OutlineOptions {
                show_archived: true,
                ..opts()
            },
        );
        let retired = archived.row(&row_key::item("s1", RETIRED)).unwrap();
        assert!(retired.item().unwrap().archived);
    }

    /// The fixture with `RETIRED` (archived) acting as a synced PR item that
    /// the sync archived, with `CHAT_PR` assigned to it.
    fn archived_synced_pr_spaces() -> ([OutlineSpace; 1], Vec<OutlineChat>) {
        let mut snapshot = fixture_snapshot("s1");
        let retired = snapshot
            .items
            .iter_mut()
            .find(|item| item.id == RETIRED)
            .unwrap();
        retired.chat_ids = vec![CHAT_PR.into()];
        retired.external_sync_state = Some(zeron_proto::WorkItemExternalSyncState {
            provider: "github".into(),
            external_id: "acme/launch#42".into(),
            round: 1,
            request_state: zeron_proto::PrReviewRequestState::Merged,
            archived_by_sync: Some(zeron_proto::PrReviewRequestState::Merged),
            ..Default::default()
        });
        let mut chats = fixture_chats("s1");
        chats.push(chat(CHAT_PR, Some("s1")));
        ([ready("s1", "Repo", snapshot)], chats)
    }

    const CHAT_PR: &str = "chat-pr";

    #[test]
    fn an_archived_synced_pr_item_with_a_live_chat_is_hidden_and_its_chat_stays_reachable() {
        let (spaces, chats) = archived_synced_pr_spaces();
        let model = flatten(&spaces, &chats, &HashSet::new(), &opts());
        assert!(
            model.row(&row_key::item("s1", RETIRED)).is_none(),
            "the live chat no longer pins the archived item in view"
        );
        let chat_row = model.row(&row_key::chat(CHAT_PR)).expect("chat not lost");
        assert!(!chat_row.chat().unwrap().dangling);
        let parent = &model.rows[chat_row.parent.unwrap()];
        assert_eq!(parent.key, row_key::unassociated(Some("s1")));
        assert!(chat_row.chat().unwrap().work_item.is_none());
    }

    #[test]
    fn showing_archived_items_brings_the_synced_pr_item_back_with_its_chat() {
        let (spaces, chats) = archived_synced_pr_spaces();
        let model = flatten(
            &spaces,
            &chats,
            &HashSet::new(),
            &OutlineOptions {
                show_archived: true,
                ..opts()
            },
        );
        let item = model.row(&row_key::item("s1", RETIRED)).unwrap();
        assert!(item.item().unwrap().archived);
        let chat_row = model.row(&row_key::chat(CHAT_PR)).unwrap();
        assert_eq!(model.rows[chat_row.parent.unwrap()].key, item.key);
    }

    #[test]
    fn a_hidden_archived_item_drops_its_missing_chat_rows_and_keeps_search_working() {
        let (spaces, mut chats) = archived_synced_pr_spaces();
        chats.retain(|c| c.id != CHAT_PR);
        let model = flatten(&spaces, &chats, &HashSet::new(), &opts());
        assert!(model.row(&row_key::item("s1", RETIRED)).is_none());
        assert!(model.row(&row_key::chat(CHAT_PR)).is_none());
        // A search that matches the chat finds it under Unassociated Chats.
        let (spaces, chats) = archived_synced_pr_spaces();
        let model = flatten(
            &spaces,
            &chats,
            &HashSet::new(),
            &OutlineOptions {
                query: "chat chat-pr".into(),
                ..opts()
            },
        );
        assert!(model.row(&row_key::chat(CHAT_PR)).is_some());
        assert!(model.row(&row_key::item("s1", RETIRED)).is_none());
    }

    #[test]
    fn an_archived_item_with_a_visible_child_item_stays_with_its_chats() {
        let (spaces, chats) = archived_synced_pr_spaces();
        let WorkOutlineLoad::Ready(snapshot) = &spaces[0].load else {
            unreachable!()
        };
        let mut snapshot = (**snapshot).clone();
        let retired = snapshot
            .items
            .iter_mut()
            .find(|item| item.id == RETIRED)
            .unwrap();
        retired.child_ids = vec![HOUSEKEEPING.into()];
        snapshot
            .items
            .iter_mut()
            .find(|item| item.id == HOUSEKEEPING)
            .unwrap()
            .parent_work_item_id = Some(RETIRED.into());
        snapshot.root_work_item_ids.retain(|id| id != HOUSEKEEPING);
        let spaces = [ready("s1", "Repo", snapshot)];
        let model = flatten(&spaces, &chats, &HashSet::new(), &opts());
        let retired = model.row(&row_key::item("s1", RETIRED)).unwrap();
        assert!(retired.item().unwrap().kept_for_descendants);
        let chat_row = model.row(&row_key::chat(CHAT_PR)).unwrap();
        assert_eq!(model.rows[chat_row.parent.unwrap()].key, retired.key);
    }

    #[test]
    fn search_keeps_matches_with_ancestors_and_subtrees_and_ignores_collapse() {
        let spaces = [ready("s1", "Repo", fixture_snapshot("s1"))];
        let collapsed: HashSet<String> = [row_key::item("s1", LAUNCH)].into();
        let model = flatten(
            &spaces,
            &fixture_chats("s1"),
            &collapsed,
            &OutlineOptions {
                query: "auth".into(),
                ..opts()
            },
        );
        assert_eq!(
            keys(&model),
            vec![
                (0, row_key::item("s1", LAUNCH)),
                (1, row_key::item("s1", API)),
                (2, row_key::item("s1", API_AUTH)),
                (3, row_key::chat(CHAT_AUTH)),
            ]
        );
    }

    #[test]
    fn dangling_work_item_and_space_mismatch_chats_land_unassociated() {
        let mut snapshot = fixture_snapshot("s1");
        // A chat the snapshot lists on an item but which belongs to another Space.
        snapshot.items[1].chat_ids.push("foreign".into());
        snapshot
            .assignments
            .push(zeron_proto::WorkItemChatAssignmentView {
                chat_id: "foreign".into(),
                space_id: "s1".into(),
                work_item_id: Some(API.into()),
                assigned_by: None,
                assigned_at: 0,
                state: WorkItemChatAssignmentState::SpaceMismatch,
            });
        let mut chats = fixture_chats("s1");
        chats.push(chat("foreign", Some("s2")));
        let spaces = [
            ready("s1", "A", snapshot),
            ready(
                "s2",
                "B",
                WorkOutlineSnapshot {
                    space_id: "s2".into(),
                    available: true,
                    synced: true,
                    ..Default::default()
                },
            ),
        ];
        let model = flatten(&spaces, &chats, &HashSet::new(), &opts());
        let ix = model.position(&row_key::chat("foreign")).unwrap();
        assert_eq!(
            model.rows[model.rows[ix].parent.unwrap()].key,
            row_key::unassociated(Some("s2"))
        );
        let orphan = model.position(&row_key::chat(CHAT_ORPHAN)).unwrap();
        assert_eq!(
            model.rows[model.rows[orphan].parent.unwrap()].key,
            row_key::unassociated(Some("s1"))
        );
    }

    #[test]
    fn paths_descendants_and_move_destinations() {
        let snapshot = fixture_snapshot("s1");
        let path: Vec<&str> = item_path(&snapshot, API_AUTH)
            .iter()
            .map(|item| item.title.as_str())
            .collect();
        assert_eq!(path, ["Launch v2", "API", "Auth endpoints"]);
        assert_eq!(
            path_label(path.iter().copied()),
            "Launch v2 › API › Auth endpoints"
        );
        assert_eq!(
            descendants(&snapshot, LAUNCH),
            [API, API_AUTH, UI]
                .iter()
                .map(|s| s.to_string())
                .collect::<HashSet<String>>()
        );
        let dests = destinations(&snapshot, Some(API));
        let ids: Vec<Option<&str>> = dests.iter().map(|d| d.item_id.as_deref()).collect();
        // Not itself, not its descendant, not the archived item.
        assert_eq!(ids, [None, Some(LAUNCH), Some(UI), Some(HOUSEKEEPING)]);
    }

    /// A synthetic outline of `roots × per_root` items, each with one chat.
    pub(crate) fn large_snapshot(
        space: &str,
        roots: usize,
        per_root: usize,
    ) -> (WorkOutlineSnapshot, Vec<OutlineChat>) {
        let mut items = Vec::new();
        let mut root_ids = Vec::new();
        let mut chats = Vec::new();
        let base = fixture_snapshot(space).items[0].clone();
        for r in 0..roots {
            let root_id = format!("r{r}");
            let child_ids: Vec<String> = (0..per_root).map(|c| format!("r{r}c{c}")).collect();
            let mut root = base.clone();
            root.id = root_id.clone();
            root.title = format!("Root {r}");
            root.parent_work_item_id = None;
            root.child_ids = child_ids.clone();
            root.chat_ids = Vec::new();
            root.depth = 0;
            items.push(root);
            root_ids.push(root_id.clone());
            for child_id in child_ids {
                let mut child = base.clone();
                child.id = child_id.clone();
                child.title = format!("Item {child_id}");
                child.parent_work_item_id = Some(root_id.clone());
                child.child_ids = Vec::new();
                let chat_id = format!("chat-{child_id}");
                child.chat_ids = vec![chat_id.clone()];
                child.depth = 1;
                items.push(child);
                chats.push(chat(&chat_id, Some(space)));
            }
        }
        (
            WorkOutlineSnapshot {
                space_id: space.into(),
                available: true,
                synced: true,
                revision: 1,
                root_work_item_ids: root_ids,
                items,
                ..Default::default()
            },
            chats,
        )
    }
}

#[cfg(test)]
mod compact_tests {
    use std::collections::HashSet;

    use super::super::nav::{NavCommand, navigate, reconcile_selection};
    use super::super::store::fixture_ids::*;
    use super::super::store::fixture_snapshot;
    use super::tests::{fixture_chats, ready};
    use super::*;

    fn options(compact: bool) -> OutlineOptions {
        OutlineOptions {
            all_spaces: true,
            compact_single_chat: compact,
            ..Default::default()
        }
    }

    fn build(
        snapshot: WorkOutlineSnapshot,
        chats: &[OutlineChat],
        options: &OutlineOptions,
    ) -> OutlineModel {
        flatten(
            &[ready("s1", "Repo", snapshot)],
            chats,
            &HashSet::new(),
            options,
        )
    }

    fn item(id: &str) -> String {
        row_key::item("s1", id)
    }

    fn chat_key(id: &str) -> String {
        row_key::chat(id)
    }

    fn compact_of<'a>(model: &'a OutlineModel, id: &str) -> Option<&'a ChatRow> {
        model.row(&item(id)).and_then(OutlineRow::compact_chat)
    }

    #[test]
    fn a_lone_live_chat_is_folded_into_its_item_row() {
        let model = build(fixture_snapshot("s1"), &fixture_chats("s1"), &options(true));
        assert!(model.position(&chat_key(CHAT_AUTH)).is_some());
        // Its row is gone; its key resolves to the item's row.
        let auth = model.position(&item(API_AUTH)).unwrap();
        assert_eq!(model.position(&chat_key(CHAT_AUTH)), Some(auth));
        assert_eq!(
            model.canonical_key(&chat_key(CHAT_AUTH)),
            Some(item(API_AUTH).as_str())
        );
        assert!(
            !model
                .rows
                .iter()
                .any(|row| row.key == chat_key(CHAT_AUTH) || row.key == chat_key(CHAT_UI))
        );
        assert_eq!(compact_of(&model, API_AUTH).unwrap().chat.id, CHAT_AUTH);
        assert_eq!(compact_of(&model, UI).unwrap().chat.id, CHAT_UI);
        let row = &model.rows[auth];
        assert!(!row.has_children && row.child_count == 0);
        assert_eq!(row.open_chat().unwrap().chat.id, CHAT_AUTH);
        // One row fewer per folded Chat than the expanded outline.
        let expanded = build(
            fixture_snapshot("s1"),
            &fixture_chats("s1"),
            &options(false),
        );
        assert_eq!(model.len() + 2, expanded.len());
    }

    #[test]
    fn items_that_need_their_own_chat_rows_stay_expanded() {
        // Two chats (w-api: one live, one dangling), and child items.
        let model = build(fixture_snapshot("s1"), &fixture_chats("s1"), &options(true));
        assert!(compact_of(&model, API).is_none());
        assert!(model.row(&chat_key(CHAT_API)).unwrap().chat().is_some());
        assert!(model.row(&chat_key(CHAT_GHOST)).unwrap().chat().is_some());
        // Items without chats are untouched.
        assert!(compact_of(&model, LAUNCH).is_none());
    }

    #[test]
    fn a_dangling_lone_chat_keeps_its_row() {
        let mut snapshot = fixture_snapshot("s1");
        // w-api: drop the live chat, leaving only the ghost.
        let api = snapshot.items.iter_mut().find(|i| i.id == API).unwrap();
        api.chat_ids.retain(|id| id != CHAT_API);
        let chats: Vec<_> = fixture_chats("s1")
            .into_iter()
            .filter(|c| c.id != CHAT_API)
            .collect();
        let model = build(snapshot, &chats, &options(true));
        assert!(compact_of(&model, API).is_none());
        assert!(
            model
                .row(&chat_key(CHAT_GHOST))
                .unwrap()
                .chat()
                .unwrap()
                .dangling
        );
    }

    #[test]
    fn a_second_chat_expands_the_row_and_selection_stays_on_the_item() {
        let snapshot = fixture_snapshot("s1");
        let before = build(snapshot.clone(), &fixture_chats("s1"), &options(true));
        assert!(compact_of(&before, UI).is_some());

        let mut snapshot = snapshot;
        let ui = snapshot.items.iter_mut().find(|i| i.id == UI).unwrap();
        ui.chat_ids.push(CHAT_LOOSE_A.into());
        let after = build(snapshot, &fixture_chats("s1"), &options(true));
        assert!(compact_of(&after, UI).is_none());
        let ui_ix = after.position(&item(UI)).unwrap();
        let mut below = [CHAT_UI, CHAT_LOOSE_A].map(|id| after.position(&chat_key(id)).unwrap());
        below.sort_unstable();
        assert_eq!(below, [ui_ix + 1, ui_ix + 2]);

        // Selecting the compact row keeps it (the item) once it expands.
        assert_eq!(
            reconcile_selection(&before, &after, &item(UI)),
            Some(item(UI))
        );
    }

    #[test]
    fn a_child_item_or_a_sub_chat_expands_the_row() {
        let mut snapshot = fixture_snapshot("s1");
        // Move w-housekeeping under w-ui.
        snapshot.root_work_item_ids.retain(|id| id != HOUSEKEEPING);
        snapshot
            .items
            .iter_mut()
            .find(|i| i.id == UI)
            .unwrap()
            .child_ids
            .push(HOUSEKEEPING.into());
        let model = build(snapshot, &fixture_chats("s1"), &options(true));
        assert!(compact_of(&model, UI).is_none());
        assert!(model.row(&chat_key(CHAT_UI)).unwrap().chat().is_some());

        // A sub-chat nested under the item's chat.
        let mut chats = fixture_chats("s1");
        chats
            .iter_mut()
            .find(|c| c.id == CHAT_LOOSE_A)
            .unwrap()
            .parent_chat_id = Some(CHAT_UI.into());
        let model = build(fixture_snapshot("s1"), &chats, &options(true));
        assert!(compact_of(&model, UI).is_none());
        let ui = model.position(&chat_key(CHAT_UI)).unwrap();
        assert_eq!(model.rows[ui + 1].key, chat_key(CHAT_LOOSE_A));
    }

    #[test]
    fn an_archived_chat_keeps_its_own_row() {
        let mut chats = fixture_chats("s1");
        chats.iter_mut().find(|c| c.id == CHAT_UI).unwrap().archived = true;
        let model = build(fixture_snapshot("s1"), &chats, &options(true));
        assert!(compact_of(&model, UI).is_none());
        assert!(
            model
                .row(&chat_key(CHAT_UI))
                .unwrap()
                .chat()
                .unwrap()
                .chat
                .archived
        );
    }

    #[test]
    fn selection_survives_compact_and_expanded_in_both_directions() {
        let chats = fixture_chats("s1");
        let expanded = build(fixture_snapshot("s1"), &chats, &options(false));
        let compact = build(fixture_snapshot("s1"), &chats, &options(true));
        // The Chat row was selected, then folded: the item row takes it.
        assert_eq!(
            reconcile_selection(&expanded, &compact, &chat_key(CHAT_UI)),
            Some(item(UI))
        );
        // The item row was selected, then expanded: still the item.
        assert_eq!(
            reconcile_selection(&compact, &expanded, &item(UI)),
            Some(item(UI))
        );
    }

    #[test]
    fn keyboard_navigation_treats_the_compact_row_as_one_row() {
        let model = build(fixture_snapshot("s1"), &fixture_chats("s1"), &options(true));
        let step = |from: &str, command| navigate(&model, Some(from), command).select;
        // Down from w-api-auth lands on the next row, not a Chat row below it.
        assert_eq!(
            step(&item(API_AUTH), NavCommand::Next),
            Some(chat_key(CHAT_API))
        );
        assert_eq!(
            step(&chat_key(CHAT_API), NavCommand::Previous),
            Some(item(API_AUTH))
        );
        // No child to descend into.
        assert_eq!(step(&item(UI), NavCommand::Child), None);
        assert_eq!(step(&item(UI), NavCommand::Parent), Some(item(LAUNCH)));
    }
}
