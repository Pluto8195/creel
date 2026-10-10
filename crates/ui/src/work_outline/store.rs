//! Seam 2: the read side of the Work Outline in the UI.
//!
//! Wire view types are re-exported from `zeron_proto` so presentation code
//! has one import path. [`WorkOutlineStore`] holds the latest composed
//! snapshot per Space plus its load state; `AppState` owns one instance and the
//! data owner feeds it from `WatchWorkOutline` (`state.rs`
//! `reconcile_work_outline_watches`). Frames replace the snapshot wholesale,
//! nothing is merged client-side. Views never subscribe themselves.
//!
//! [`WorkOutlineStore::from_fixture`] builds a deterministic three-level
//! outline so presentation work needs no engine.
use std::collections::HashMap;
use std::sync::Arc;

#[allow(unused_imports)]
pub(crate) use zeron_proto::{
    WorkItemArchiveCause, WorkItemChanges, WorkItemChatAssignmentState, WorkItemChatAssignmentView,
    WorkItemEffectiveStatus, WorkItemExternalReferenceView, WorkItemRollup, WorkItemStatus,
    WorkItemView, WorkOutlineActor, WorkOutlineActorKind, WorkOutlinePlacement, WorkOutlineRepair,
    WorkOutlineSnapshot,
};

use zeron_proto::WorkOutlineErrorCode;

/// Per-Space load state. `Unsupported` is a host that answers `unknown
/// method` to `WatchWorkOutline` (the UI shows an upgrade hint instead of a
/// silently partial outline); `Failed` carries a displayable reason.
#[derive(Debug, Clone)]
pub(crate) enum WorkOutlineLoad {
    Loading,
    Ready(Arc<WorkOutlineSnapshot>),
    Unsupported,
    Failed(String),
}

impl WorkOutlineLoad {
    pub(crate) fn snapshot(&self) -> Option<&Arc<WorkOutlineSnapshot>> {
        match self {
            Self::Ready(snapshot) => Some(snapshot),
            _ => None,
        }
    }

    /// Whether two loads are the same state *and*, for `Ready`, the same
    /// snapshot allocation (a pointer compare lets views skip no-op frames).
    pub(crate) fn same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Loading, Self::Loading) | (Self::Unsupported, Self::Unsupported) => true,
            (Self::Ready(a), Self::Ready(b)) => Arc::ptr_eq(a, b),
            (Self::Failed(a), Self::Failed(b)) => a == b,
            _ => false,
        }
    }

    /// The load state a failed watch maps to: an `unknown method` error is an
    /// unsupported host, everything else is a (retryable) failure.
    pub(crate) fn from_watch_error(message: &str) -> Self {
        let lowered = message.to_ascii_lowercase();
        let coded = zeron_proto::WorkOutlineError::from_wire_message(message)
            .is_some_and(|error| error.code == WorkOutlineErrorCode::Unsupported);
        if coded || lowered.contains("unknown method") {
            Self::Unsupported
        } else {
            Self::Failed(message.to_owned())
        }
    }
}

/// Latest snapshot and load state per Space id.
#[derive(Debug, Default)]
pub(crate) struct WorkOutlineStore {
    loads: HashMap<String, WorkOutlineLoad>,
}

impl WorkOutlineStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The load state for a Space; a Space nobody has started watching is
    /// `Loading`.
    pub(crate) fn load(&self, space_id: &str) -> WorkOutlineLoad {
        self.loads
            .get(space_id)
            .cloned()
            .unwrap_or(WorkOutlineLoad::Loading)
    }

    /// `AppState::work_outline(space_id)` delegates here.
    pub(crate) fn snapshot(&self, space_id: &str) -> Option<&Arc<WorkOutlineSnapshot>> {
        self.loads.get(space_id).and_then(WorkOutlineLoad::snapshot)
    }

    /// Replace a Space's state. Returns whether anything changed, so the
    /// caller notifies only on a real change (a `Ready` frame equal to the
    /// current snapshot, ignoring `revision`, is not a change).
    pub(crate) fn set(&mut self, space_id: &str, load: WorkOutlineLoad) -> bool {
        if let (Some(WorkOutlineLoad::Ready(old)), WorkOutlineLoad::Ready(new)) =
            (self.loads.get(space_id), &load)
        {
            if old.same_content_as(new) {
                return false;
            }
        }
        if self
            .loads
            .get(space_id)
            .is_some_and(|old| old.same_as(&load))
        {
            return false;
        }
        self.loads.insert(space_id.to_owned(), load);
        true
    }

    /// Drop a Space that is no longer watched (deleted, or Outline closed).
    pub(crate) fn remove(&mut self, space_id: &str) -> bool {
        self.loads.remove(space_id).is_some()
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.loads.retain(|space_id, _| keep(space_id));
    }

    pub(crate) fn space_ids(&self) -> impl Iterator<Item = &str> {
        self.loads.keys().map(String::as_str)
    }

    /// A ready store holding [`fixture_snapshot`] for `space_id`.
    pub(crate) fn from_fixture(space_id: &str) -> Self {
        let mut store = Self::new();
        store.set(
            space_id,
            WorkOutlineLoad::Ready(Arc::new(fixture_snapshot(space_id))),
        );
        store
    }
}

// --------------------------------------------------------------------------
// Fixture
// --------------------------------------------------------------------------

/// Chat ids used by the fixture, so presentation tests can build matching
/// `Chat` rows (`chat-ghost` deliberately has none).
pub(crate) mod fixture_ids {
    pub(crate) const LAUNCH: &str = "w-launch";
    pub(crate) const API: &str = "w-api";
    pub(crate) const API_AUTH: &str = "w-api-auth";
    pub(crate) const UI: &str = "w-ui";
    pub(crate) const HOUSEKEEPING: &str = "w-housekeeping";
    pub(crate) const RETIRED: &str = "w-retired";
    /// Associated with `w-api`.
    pub(crate) const CHAT_API: &str = "chat-api";
    /// Associated with `w-api-auth` (a depth-2 item).
    pub(crate) const CHAT_AUTH: &str = "chat-auth";
    /// Associated with `w-ui`.
    pub(crate) const CHAT_UI: &str = "chat-ui";
    /// Never assigned.
    pub(crate) const CHAT_LOOSE_A: &str = "chat-loose-a";
    /// Explicitly disassociated.
    pub(crate) const CHAT_LOOSE_B: &str = "chat-loose-b";
    /// Assignment survives but the Chat row is gone (`danglingChat`), kept on `w-api`.
    pub(crate) const CHAT_GHOST: &str = "chat-ghost";
    /// Assigned to an item that no longer exists (`danglingWorkItem`); unassociated.
    pub(crate) const CHAT_ORPHAN: &str = "chat-orphan";
}

const FIXTURE_NOW: i64 = 1_760_000_000_000;

fn fixture_actor() -> WorkOutlineActor {
    WorkOutlineActor {
        kind: WorkOutlineActorKind::User,
        device_id: "device-fixture".into(),
        chat_id: None,
        message_id: None,
    }
}

fn fixture_item(
    id: &str,
    space_id: &str,
    parent: Option<&str>,
    order_key: &str,
    title: &str,
    status: WorkItemStatus,
    child_ids: &[&str],
    chat_ids: &[&str],
) -> WorkItemView {
    WorkItemView {
        id: id.into(),
        space_id: space_id.into(),
        parent_work_item_id: parent.map(Into::into),
        raw_parent_work_item_id: parent.map(Into::into),
        order_key: order_key.into(),
        title: title.into(),
        description: None,
        status,
        effective_status: match status {
            WorkItemStatus::Active => WorkItemEffectiveStatus::Active,
            WorkItemStatus::Completed => WorkItemEffectiveStatus::Completed,
            WorkItemStatus::Pending => WorkItemEffectiveStatus::Pending,
        },
        archived_at: None,
        archived_by: None,
        archive_cause: None,
        inherit_parent_instructions: true,
        created_by: Some(fixture_actor()),
        created_at: FIXTURE_NOW,
        updated_at: FIXTURE_NOW,
        child_ids: child_ids.iter().map(|c| (*c).to_owned()).collect(),
        depth: 0,
        chat_ids: chat_ids.iter().map(|c| (*c).to_owned()).collect(),
        external_reference_ids: Vec::new(),
        rollup: WorkItemRollup::default(),
    }
}

/// Fill `depth` and the subtree `rollup` bottom-up from `child_ids`, so the
/// fixture is self-consistent. Status counts cover the item and all its
/// descendants; archived items are counted (nothing is filtered).
fn finish_fixture(items: &mut [WorkItemView]) {
    fn visit(items: &mut [WorkItemView], index: usize, depth: u32) -> WorkItemRollup {
        items[index].depth = depth;
        let children: Vec<usize> = items[index]
            .child_ids
            .clone()
            .iter()
            .filter_map(|id| items.iter().position(|item| &item.id == id))
            .collect();
        let mut rollup = WorkItemRollup::default();
        for child in children {
            let sub = visit(items, child, depth + 1);
            rollup.pending_count += sub.pending_count;
            rollup.active_count += sub.active_count;
            rollup.completed_count += sub.completed_count;
            rollup.user_action_required_count += sub.user_action_required_count;
        }
        match items[index].status {
            WorkItemStatus::Pending => rollup.pending_count += 1,
            WorkItemStatus::Active => rollup.active_count += 1,
            WorkItemStatus::Completed => rollup.completed_count += 1,
        }
        if items[index].effective_status == WorkItemEffectiveStatus::UserActionRequired {
            rollup.user_action_required_count += 1;
        }
        rollup.unresolved_user_action_count = items[index].rollup.unresolved_user_action_count;
        rollup.associated_chat_count = items[index].chat_ids.len() as u32;
        items[index].rollup = rollup.clone();
        rollup
    }
    let roots: Vec<usize> = (0..items.len())
        .filter(|i| items[*i].parent_work_item_id.is_none())
        .collect();
    for root in roots {
        visit(items, root, 0);
    }
}

fn fixture_assignment(
    chat_id: &str,
    space_id: &str,
    work_item_id: Option<&str>,
    state: WorkItemChatAssignmentState,
) -> WorkItemChatAssignmentView {
    WorkItemChatAssignmentView {
        chat_id: chat_id.into(),
        space_id: space_id.into(),
        work_item_id: work_item_id.map(Into::into),
        assigned_by: Some(fixture_actor()),
        assigned_at: FIXTURE_NOW,
        state,
    }
}

/// A deterministic three-level outline in materialization order (depth-first
/// pre-order, roots in order key order):
///
/// ```text
/// Launch v2                       active          (root, depth 0)
///   API                           active          chat-api, chat-ghost (dangling chat)
///     Auth endpoints              completed       chat-auth
///   UI                            active, user-action-required   chat-ui
/// Housekeeping                    pending
/// Retired experiments             completed, archived
/// ```
///
/// plus unassociated chats `chat-loose-a` (never assigned), `chat-loose-b`
/// (explicitly disassociated), `chat-orphan` (its work item is gone), one
/// external reference on `w-api`, and the matching repair markers.
pub(crate) fn fixture_snapshot(space_id: &str) -> WorkOutlineSnapshot {
    use fixture_ids::*;
    let mut items = vec![
        fixture_item(
            LAUNCH,
            space_id,
            None,
            "a0",
            "Launch v2",
            WorkItemStatus::Active,
            &[API, UI],
            &[],
        ),
        fixture_item(
            API,
            space_id,
            Some(LAUNCH),
            "a0",
            "API",
            WorkItemStatus::Active,
            &[API_AUTH],
            &[CHAT_API, CHAT_GHOST],
        ),
        fixture_item(
            API_AUTH,
            space_id,
            Some(API),
            "a0",
            "Auth endpoints",
            WorkItemStatus::Completed,
            &[],
            &[CHAT_AUTH],
        ),
        fixture_item(
            UI,
            space_id,
            Some(LAUNCH),
            "a1",
            "UI",
            WorkItemStatus::Active,
            &[],
            &[CHAT_UI],
        ),
        fixture_item(
            HOUSEKEEPING,
            space_id,
            None,
            "a1",
            "Housekeeping",
            WorkItemStatus::Pending,
            &[],
            &[],
        ),
        fixture_item(
            RETIRED,
            space_id,
            None,
            "a2",
            "Retired experiments",
            WorkItemStatus::Completed,
            &[],
            &[],
        ),
    ];
    items[3].effective_status = WorkItemEffectiveStatus::UserActionRequired;
    items[3].rollup.unresolved_user_action_count = 1;
    items[1].description = Some("Public HTTP API for the launch.".into());
    items[5].archived_at = Some(FIXTURE_NOW + 1_000);
    items[5].archived_by = Some(fixture_actor());
    items[5].archive_cause = Some(WorkItemArchiveCause::SelfArchived);
    let reference_id =
        zeron_proto::external_reference_id(API, "github", "pullRequest", "acme/launch#42");
    items[1].external_reference_ids = vec![reference_id.clone()];
    finish_fixture(&mut items);

    let assignments = vec![
        fixture_assignment(
            CHAT_API,
            space_id,
            Some(API),
            WorkItemChatAssignmentState::Associated,
        ),
        fixture_assignment(
            CHAT_AUTH,
            space_id,
            Some(API_AUTH),
            WorkItemChatAssignmentState::Associated,
        ),
        fixture_assignment(
            CHAT_GHOST,
            space_id,
            Some(API),
            WorkItemChatAssignmentState::DanglingChat,
        ),
        fixture_assignment(
            CHAT_LOOSE_B,
            space_id,
            None,
            WorkItemChatAssignmentState::Unassociated,
        ),
        fixture_assignment(
            CHAT_ORPHAN,
            space_id,
            Some("w-deleted"),
            WorkItemChatAssignmentState::DanglingWorkItem,
        ),
        fixture_assignment(
            CHAT_UI,
            space_id,
            Some(UI),
            WorkItemChatAssignmentState::Associated,
        ),
    ];

    WorkOutlineSnapshot {
        space_id: space_id.into(),
        available: true,
        synced: true,
        revision: 1,
        root_work_item_ids: vec![LAUNCH.into(), HOUSEKEEPING.into(), RETIRED.into()],
        items,
        assignments,
        unassociated_chat_ids: vec![CHAT_LOOSE_A.into(), CHAT_LOOSE_B.into(), CHAT_ORPHAN.into()],
        external_references: vec![WorkItemExternalReferenceView {
            id: reference_id,
            space_id: space_id.into(),
            work_item_id: API.into(),
            provider: "github".into(),
            reference_type: "pullRequest".into(),
            external_id: "acme/launch#42".into(),
            url: "https://github.com/acme/launch/pull/42".into(),
            cached_title: Some("Add auth endpoints".into()),
            created_by: Some(fixture_actor()),
            created_at: FIXTURE_NOW,
        }],
        repairs: vec![
            WorkOutlineRepair::DanglingChat {
                chat_id: CHAT_GHOST.into(),
            },
            WorkOutlineRepair::DanglingWorkItem {
                chat_id: CHAT_ORPHAN.into(),
                work_item_id: "w-deleted".into(),
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::fixture_ids::*;
    use super::*;

    #[test]
    fn fixture_is_three_levels_deep_in_preorder() {
        let snapshot = fixture_snapshot("space-1");
        let order: Vec<&str> = snapshot.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(order, [LAUNCH, API, API_AUTH, UI, HOUSEKEEPING, RETIRED]);
        let depths: Vec<u32> = snapshot.items.iter().map(|i| i.depth).collect();
        assert_eq!(depths, [0, 1, 2, 1, 0, 0]);
        assert_eq!(snapshot.root_work_item_ids, [LAUNCH, HOUSEKEEPING, RETIRED]);
    }

    #[test]
    fn fixture_links_are_consistent() {
        let snapshot = fixture_snapshot("space-1");
        for item in &snapshot.items {
            assert_eq!(item.space_id, "space-1");
            for child in &item.child_ids {
                let child = snapshot.item(child).expect("child exists");
                assert_eq!(child.parent_work_item_id.as_deref(), Some(item.id.as_str()));
                assert_eq!(child.depth, item.depth + 1);
            }
            for chat in &item.chat_ids {
                assert!(
                    snapshot
                        .assignments
                        .iter()
                        .any(|a| &a.chat_id == chat && a.work_item_id.as_deref() == Some(&item.id)),
                    "{chat} has an assignment to {}",
                    item.id
                );
            }
        }
        for assignment in &snapshot.assignments {
            let listed = snapshot.unassociated_chat_ids.contains(&assignment.chat_id);
            // Unassociated and dangling-work-item chats are listed as
            // unassociated; associated and dangling-chat ones sit on an item.
            assert_eq!(
                listed,
                matches!(
                    assignment.state,
                    WorkItemChatAssignmentState::Unassociated
                        | WorkItemChatAssignmentState::DanglingWorkItem
                )
            );
        }
        let api = snapshot.item(API).unwrap();
        assert!(
            snapshot
                .external_references
                .iter()
                .all(|r| api.external_reference_ids.contains(&r.id))
        );
    }

    #[test]
    fn fixture_rollups_cover_subtrees() {
        let snapshot = fixture_snapshot("space-1");
        let launch = &snapshot.item(LAUNCH).unwrap().rollup;
        // launch(active) + api(active) + auth(completed) + ui(active)
        assert_eq!(launch.active_count, 3);
        assert_eq!(launch.completed_count, 1);
        assert_eq!(launch.pending_count, 0);
        assert_eq!(launch.user_action_required_count, 1);
        assert_eq!(launch.associated_chat_count, 0);
        let api = &snapshot.item(API).unwrap().rollup;
        assert_eq!(api.associated_chat_count, 2);
        assert_eq!(
            snapshot
                .item(UI)
                .unwrap()
                .rollup
                .unresolved_user_action_count,
            1
        );
    }

    #[test]
    fn fixture_covers_every_chat_relationship() {
        let snapshot = fixture_snapshot("space-1");
        let states: Vec<_> = snapshot.assignments.iter().map(|a| a.state).collect();
        assert!(states.contains(&WorkItemChatAssignmentState::Associated));
        assert!(states.contains(&WorkItemChatAssignmentState::Unassociated));
        assert!(states.contains(&WorkItemChatAssignmentState::DanglingChat));
        assert!(states.contains(&WorkItemChatAssignmentState::DanglingWorkItem));
        assert!(
            snapshot
                .unassociated_chat_ids
                .contains(&CHAT_LOOSE_A.to_owned())
        );
        assert!(snapshot.item(RETIRED).unwrap().archived_at.is_some());
        assert_eq!(
            snapshot.item(UI).unwrap().effective_status,
            WorkItemEffectiveStatus::UserActionRequired
        );
    }

    #[test]
    fn fixture_survives_a_wire_round_trip() {
        let snapshot = fixture_snapshot("space-1");
        let value = serde_json::to_value(&snapshot).unwrap();
        let back: WorkOutlineSnapshot = serde_json::from_value(value).unwrap();
        assert_eq!(back, snapshot);
    }

    #[test]
    fn store_defaults_to_loading_and_serves_the_fixture() {
        let store = WorkOutlineStore::from_fixture("space-1");
        assert!(matches!(store.load("space-1"), WorkOutlineLoad::Ready(_)));
        assert!(matches!(store.load("other"), WorkOutlineLoad::Loading));
        assert_eq!(store.snapshot("space-1").unwrap().items.len(), 6);
        assert!(store.snapshot("other").is_none());
    }

    #[test]
    fn set_reports_real_changes_only() {
        let mut store = WorkOutlineStore::new();
        assert!(store.set("s", WorkOutlineLoad::Unsupported));
        assert!(!store.set("s", WorkOutlineLoad::Unsupported));
        let mut snapshot = fixture_snapshot("s");
        assert!(store.set("s", WorkOutlineLoad::Ready(Arc::new(snapshot.clone()))));
        // Same content, newer revision: not a change.
        snapshot.revision += 1;
        assert!(!store.set("s", WorkOutlineLoad::Ready(Arc::new(snapshot.clone()))));
        snapshot.items[0].title = "Renamed".into();
        snapshot.revision += 1;
        assert!(store.set("s", WorkOutlineLoad::Ready(Arc::new(snapshot))));
        assert!(store.remove("s"));
        assert!(!store.remove("s"));
    }

    #[test]
    fn watch_errors_map_to_load_states() {
        assert!(matches!(
            WorkOutlineLoad::from_watch_error("unknown method: WatchWorkOutline"),
            WorkOutlineLoad::Unsupported
        ));
        assert!(matches!(
            WorkOutlineLoad::from_watch_error("work-outline/unsupported: old host"),
            WorkOutlineLoad::Unsupported
        ));
        assert!(matches!(
            WorkOutlineLoad::from_watch_error("transport: closed"),
            WorkOutlineLoad::Failed(message) if message == "transport: closed"
        ));
    }
}
