//! Pure instruction logic for the Work Outline UI (Milestone 2): local
//! instruction lists, provenance labels, effective-instruction hashes, the
//! client-side drift analysis behind the drift confirmation, and the local
//! revision comparison behind **Compare**.
//!
//! Everything here is GUI-free and works over a [`WorkOutlineSnapshot`]
//! with the shared pure resolver (`zeron_proto::resolve_effective_instructions`),
//! so the drift confirmation needs no RPC and works while the engine still
//! answers instruction operations with `unsupported`.
use std::collections::{BTreeSet, HashMap};

use sha2::{Digest, Sha256};
use zeron_proto::{
    ChatInstructionState, INSTRUCTION_ENVELOPE_VERSION, InstructionRevisionBody,
    InstructionRevisionComparison, InstructionRevisionIntegrity, WorkItemChatAssignmentState,
    WorkItemInstructionView, WorkOutlineActorKind, WorkOutlineSnapshot, build_revision_body,
    canonical_revision_hash, compare_instruction_revisions,
};

/// Live instructions of one scope in effective order `(orderKey, id)`:
/// `item_id == None` is the Space root.
pub(crate) fn live_instructions<'a>(
    snapshot: &'a WorkOutlineSnapshot,
    item_id: Option<&str>,
) -> Vec<&'a WorkItemInstructionView> {
    let mut list: Vec<_> = snapshot
        .instructions
        .iter()
        .filter(|instruction| {
            !instruction.archived()
                && instruction.work_item_id.as_deref() == item_id
                && instruction.space_id == snapshot.space_id
        })
        .collect();
    list.sort_by(|a, b| (&a.order_key, &a.id).cmp(&(&b.order_key, &b.id)));
    list
}

/// The compact provenance badge of an instruction, if it did not come from
/// a plain user edit.
pub(crate) fn provenance_badge(instruction: &WorkItemInstructionView) -> Option<&'static str> {
    if instruction
        .source
        .as_ref()
        .is_some_and(|source| source.chat_id.is_some())
    {
        Some("from Chat")
    } else if instruction.source_proposal_id.is_some() {
        Some("from proposal")
    } else if instruction
        .created_by
        .as_ref()
        .is_some_and(|actor| actor.kind == WorkOutlineActorKind::Agent)
    {
        Some("by agent")
    } else {
        None
    }
}

/// `contentHash` of a Chat excerpt: lowercase hex `sha256(excerpt)`
/// (m0-contract "Milestone 2 contract notes" item 5).
pub(crate) fn excerpt_content_hash(excerpt: &str) -> String {
    Sha256::digest(excerpt.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The canonical hash of an item's current effective instructions (what
/// `ChatInstructionState::current_effective_hash` reports). `None` = no
/// item (an unassociated Chat), which hashes like an empty list.
pub(crate) fn effective_hash(snapshot: &WorkOutlineSnapshot, item_id: Option<&str>) -> String {
    let entries = item_id
        .map(|id| snapshot.effective_instructions(id).revision_entries())
        .unwrap_or_default();
    canonical_revision_hash(&entries, INSTRUCTION_ENVELOPE_VERSION)
}

/// Chat id -> assigned item id, for Chats whose assignment is live
/// (`associated`; dangling Chats are listed on items but are not Chats the
/// user can still talk to).
pub(crate) fn assigned_chats(snapshot: &WorkOutlineSnapshot) -> HashMap<String, String> {
    let dangling: BTreeSet<&str> = snapshot
        .assignments
        .iter()
        .filter(|assignment| assignment.state == WorkItemChatAssignmentState::DanglingChat)
        .map(|assignment| assignment.chat_id.as_str())
        .collect();
    snapshot
        .items
        .iter()
        .flat_map(|item| {
            item.chat_ids
                .iter()
                .filter(|chat_id| !dangling.contains(chat_id.as_str()))
                .map(|chat_id| (chat_id.clone(), item.id.clone()))
        })
        .collect()
}

/// `item_id` and every descendant (child links of the materialized tree).
pub(crate) fn subtree(snapshot: &WorkOutlineSnapshot, item_id: &str) -> Vec<String> {
    let by_id: HashMap<&str, &zeron_proto::WorkItemView> = snapshot
        .items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect();
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut stack = vec![item_id];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(item) = by_id.get(id) else {
            continue;
        };
        out.push(id.to_owned());
        stack.extend(item.child_ids.iter().rev().map(String::as_str));
    }
    out
}

/// Live (non-archived) descendants a new instruction on `target` would reach
/// (`None` = the Space root: every live item).
pub(crate) fn affected_descendant_count(
    snapshot: &WorkOutlineSnapshot,
    target: Option<&str>,
) -> usize {
    let live = |id: &str| {
        snapshot
            .item(id)
            .is_some_and(|item| item.archived_at.is_none())
    };
    match target {
        None => snapshot
            .items
            .iter()
            .filter(|item| item.archived_at.is_none())
            .count(),
        Some(id) => subtree(snapshot, id)
            .iter()
            .skip(1)
            .filter(|id| live(id))
            .count(),
    }
}

/// A user change the drift confirmation must evaluate before it is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutlineChange {
    /// Associate or move a Chat to `work_item_id`.
    AssignChat {
        chat_id: String,
        work_item_id: String,
    },
    /// Remove a Chat from its work item (to Unassociated Chats).
    RemoveChat {
        chat_id: String,
    },
    /// Move an item under `new_parent` (`None` = top level).
    MoveItem {
        work_item_id: String,
        new_parent: Option<String>,
    },
    SetInheritance {
        work_item_id: String,
        inherit: bool,
    },
    EditInstruction {
        instruction_id: String,
        text: String,
    },
    ArchiveInstruction {
        instruction_id: String,
    },
    /// A new instruction appended to `work_item_id` (`None` = the Space
    /// root): approving an agent proposal (Milestone 4). Creating an
    /// instruction by hand stays ungated (Milestone 2 discovery 4).
    AddInstruction {
        work_item_id: Option<String>,
        text: String,
    },
}

impl OutlineChange {
    /// Verb phrase for the confirmation title.
    pub(crate) fn title(&self) -> &'static str {
        match self {
            Self::AssignChat { .. } | Self::RemoveChat { .. } => "Change this Chat's instructions?",
            Self::MoveItem { .. } => "Move and change instructions?",
            Self::SetInheritance { .. } => "Change inherited instructions?",
            Self::EditInstruction { .. } => "Save instruction change?",
            Self::ArchiveInstruction { .. } => "Archive instruction?",
            Self::AddInstruction { .. } => "Approve proposed instruction?",
        }
    }

    pub(crate) fn confirm_label(&self) -> &'static str {
        match self {
            Self::AssignChat { .. } => "Associate",
            Self::RemoveChat { .. } => "Remove",
            Self::MoveItem { .. } => "Move",
            Self::SetInheritance { .. } => "Change",
            Self::EditInstruction { .. } => "Save",
            Self::ArchiveInstruction { .. } => "Archive",
            Self::AddInstruction { .. } => "Approve",
        }
    }
}

/// What a change would do to current effective instructions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DriftImpact {
    /// Assigned Chats whose current effective instructions change, sorted.
    pub chat_ids: Vec<String>,
    /// Live work items whose effective instructions change, sorted.
    pub item_ids: Vec<String>,
}

impl DriftImpact {
    /// The confirmation is needed only when an assigned Chat is affected.
    pub(crate) fn needs_confirmation(&self) -> bool {
        !self.chat_ids.is_empty()
    }

    /// "2 Chats and 3 work items" style summary.
    pub(crate) fn summary(&self) -> String {
        let chats = plural(self.chat_ids.len(), "Chat", "Chats");
        if self.item_ids.is_empty() {
            chats
        } else {
            format!(
                "{chats} and {}",
                plural(self.item_ids.len(), "work item", "work items")
            )
        }
    }

    /// The confirmation body: what changes, and that existing Chats keep
    /// their applied revision until explicitly updated.
    pub(crate) fn body(&self) -> String {
        format!(
            "This changes the current effective instructions for {}. Existing Chats keep their \
             applied revision until you explicitly apply the update.",
            self.summary()
        )
    }
}

pub(crate) fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// Evaluate `change` against `snapshot` by resolving effective instructions
/// before and after with the shared pure resolver (no RPC). Unknown targets
/// yield an empty impact.
pub(crate) fn drift_impact(snapshot: &WorkOutlineSnapshot, change: &OutlineChange) -> DriftImpact {
    let assigned = assigned_chats(snapshot);
    let chat_target = match change {
        OutlineChange::AssignChat {
            chat_id,
            work_item_id,
        } => Some((chat_id, Some(work_item_id.as_str()))),
        // An unassociated Chat resolves like any Chat without an item.
        OutlineChange::RemoveChat { chat_id } => Some((chat_id, None)),
        _ => None,
    };
    if let Some((chat_id, target)) = chat_target {
        let before = effective_hash(snapshot, assigned.get(chat_id).map(String::as_str));
        let after = effective_hash(snapshot, target);
        return DriftImpact {
            chat_ids: if before == after {
                Vec::new()
            } else {
                vec![chat_id.clone()]
            },
            item_ids: Vec::new(),
        };
    }

    let mut after = snapshot.clone();
    let candidates: Vec<String> = match change {
        OutlineChange::AssignChat { .. } | OutlineChange::RemoveChat { .. } => {
            unreachable!("handled above")
        }
        OutlineChange::MoveItem {
            work_item_id,
            new_parent,
        } => {
            let Some(item) = after.items.iter_mut().find(|item| &item.id == work_item_id) else {
                return DriftImpact::default();
            };
            item.parent_work_item_id = new_parent.clone();
            subtree(snapshot, work_item_id)
        }
        OutlineChange::SetInheritance {
            work_item_id,
            inherit,
        } => {
            let Some(item) = after.items.iter_mut().find(|item| &item.id == work_item_id) else {
                return DriftImpact::default();
            };
            item.inherit_parent_instructions = *inherit;
            subtree(snapshot, work_item_id)
        }
        OutlineChange::EditInstruction {
            instruction_id,
            text,
        } => {
            let Some(instruction) = after
                .instructions
                .iter_mut()
                .find(|instruction| &instruction.id == instruction_id)
            else {
                return DriftImpact::default();
            };
            instruction.text = text.clone();
            scope_candidates(snapshot, instruction.work_item_id.as_deref())
        }
        OutlineChange::ArchiveInstruction { instruction_id } => {
            let Some(instruction) = after
                .instructions
                .iter_mut()
                .find(|instruction| &instruction.id == instruction_id)
            else {
                return DriftImpact::default();
            };
            instruction.archived_at = Some(instruction.updated_at.max(1));
            scope_candidates(snapshot, instruction.work_item_id.as_deref())
        }
        OutlineChange::AddInstruction { work_item_id, text } => {
            if work_item_id
                .as_deref()
                .is_some_and(|id| snapshot.item(id).is_none())
            {
                return DriftImpact::default();
            }
            // Appended last in its scope.
            let last_key = after
                .instructions
                .iter()
                .filter(|instruction| instruction.work_item_id == *work_item_id)
                .map(|instruction| instruction.order_key.clone())
                .max()
                .unwrap_or_default();
            after
                .instructions
                .push(zeron_proto::WorkItemInstructionView {
                    id: "\u{0}proposed".to_owned(),
                    space_id: snapshot.space_id.clone(),
                    work_item_id: work_item_id.clone(),
                    text: text.clone(),
                    order_key: format!("{last_key}~"),
                    created_by: None,
                    created_at: 0,
                    updated_at: 0,
                    source: None,
                    source_proposal_id: None,
                    archived_at: None,
                    archived_by: None,
                });
            scope_candidates(snapshot, work_item_id.as_deref())
        }
    };

    let mut impact = DriftImpact::default();
    for id in &candidates {
        if effective_hash(snapshot, Some(id)) == effective_hash(&after, Some(id)) {
            continue;
        }
        if snapshot
            .item(id)
            .is_some_and(|item| item.archived_at.is_none())
        {
            impact.item_ids.push(id.clone());
        }
        impact.chat_ids.extend(
            assigned
                .iter()
                .filter(|(_, item_id)| *item_id == id)
                .map(|(chat_id, _)| chat_id.clone()),
        );
    }
    impact.chat_ids.sort();
    impact.chat_ids.dedup();
    impact.item_ids.sort();
    impact
}

/// Items an instruction of this scope can reach.
fn scope_candidates(snapshot: &WorkOutlineSnapshot, item_id: Option<&str>) -> Vec<String> {
    match item_id {
        Some(id) => subtree(snapshot, id),
        None => snapshot.items.iter().map(|item| item.id.clone()).collect(),
    }
}

/// Short revision label from a hash or id: `r` plus the first 7 characters.
pub(crate) fn revision_label(value: &str) -> String {
    let short: String = value.chars().take(7).collect();
    format!("r{short}")
}

/// "Selected r… · Delivered r…" when the host reports a delivery that
/// differs from the selection (or none yet). `None` when they agree or
/// nothing is selected. Delivery is never inferred from selection.
pub(crate) fn delivery_line(state: &ChatInstructionState) -> Option<String> {
    let selected_id = state.selected_revision_id.as_deref()?;
    if state.delivered_revision_id.as_deref() == Some(selected_id) {
        return None;
    }
    let label = |hash: Option<&str>, id: &str| revision_label(hash.unwrap_or(id));
    let selected = label(state.selected_revision_hash.as_deref(), selected_id);
    Some(match state.delivered_revision_id.as_deref() {
        Some(delivered_id) => format!(
            "Selected {selected} \u{b7} Delivered {}",
            label(state.delivered_revision_hash.as_deref(), delivered_id)
        ),
        None => format!("Selected {selected} \u{b7} Not delivered yet"),
    })
}

/// Why a local comparison could not be computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CompareUnavailable {
    /// The Chat has no selected revision or no live assigned item.
    NothingToCompare,
    /// The applied revision is not in the snapshot (fetch it with
    /// `getInstructionRevision`).
    MissingRevision(String),
    /// A displayable reason (corrupt revision, over budget).
    Failed(String),
}

/// The current effective instructions of `item_id` as a revision body (what
/// **Apply** would snapshot).
pub(crate) fn current_body(
    snapshot: &WorkOutlineSnapshot,
    item_id: &str,
) -> Result<InstructionRevisionBody, String> {
    build_revision_body(snapshot.effective_instructions(item_id).revision_entries())
        .map_err(|error| error.message)
}

/// Compare `source` (the applied revision) with `item_id`'s current
/// effective instructions.
pub(crate) fn compare_with_current(
    snapshot: &WorkOutlineSnapshot,
    item_id: &str,
    source: &zeron_proto::InstructionRevisionView,
) -> Result<InstructionRevisionComparison, CompareUnavailable> {
    if source.integrity == InstructionRevisionIntegrity::Corrupt {
        return Err(CompareUnavailable::Failed(
            "The applied revision failed its integrity check".into(),
        ));
    }
    let target = current_body(snapshot, item_id).map_err(CompareUnavailable::Failed)?;
    compare_instruction_revisions(&source.body, &target)
        .map_err(|error| CompareUnavailable::Failed(error.to_string()))
}

/// The local comparison for an assigned Chat: its selected revision (from
/// the snapshot) against its item's current effective instructions.
pub(crate) fn chat_comparison(
    snapshot: &WorkOutlineSnapshot,
    chat_id: &str,
) -> Result<InstructionRevisionComparison, CompareUnavailable> {
    let state = snapshot
        .chat_instruction_state(chat_id)
        .ok_or(CompareUnavailable::NothingToCompare)?;
    let revision_id = state
        .selected_revision_id
        .as_deref()
        .ok_or(CompareUnavailable::NothingToCompare)?;
    let item_id = assigned_chats(snapshot)
        .remove(chat_id)
        .ok_or(CompareUnavailable::NothingToCompare)?;
    let source = snapshot
        .revision(revision_id)
        .ok_or_else(|| CompareUnavailable::MissingRevision(revision_id.to_owned()))?;
    compare_with_current(snapshot, &item_id, source)
}

// --------------------------------------------------------------------------
// Fixture
// --------------------------------------------------------------------------

/// Instruction fixture over [`super::store::fixture_snapshot`]:
///
/// ```text
/// Space root      "Prefer small, reviewable changes."
/// Launch v2       "Ship behind the launch flag."
///   API           "Document every endpoint."            chat-api: drifted (applied an older API text)
///     Auth        "Never log tokens."                   chat-auth: current
///   UI            (none)                                chat-ui: no selection
/// ```
#[cfg(test)]
pub(crate) mod fixture {
    use super::super::store::fixture_ids::*;
    use super::super::store::fixture_snapshot;
    use super::*;
    use zeron_proto::{InstructionRevisionView, InstructionSource, WorkOutlineActor};

    pub(crate) const I_ROOT: &str = "i-root";
    pub(crate) const I_LAUNCH: &str = "i-launch";
    pub(crate) const I_API: &str = "i-api";
    pub(crate) const I_AUTH: &str = "i-auth";
    pub(crate) const R_API_OLD: &str = "r-api-old";
    pub(crate) const R_AUTH: &str = "r-auth";

    pub(crate) fn instruction(
        id: &str,
        space_id: &str,
        item: Option<&str>,
        order_key: &str,
        text: &str,
    ) -> WorkItemInstructionView {
        WorkItemInstructionView {
            id: id.into(),
            space_id: space_id.into(),
            work_item_id: item.map(Into::into),
            text: text.into(),
            order_key: order_key.into(),
            created_by: Some(WorkOutlineActor {
                kind: WorkOutlineActorKind::User,
                device_id: "device-fixture".into(),
                chat_id: None,
                message_id: None,
            }),
            created_at: 1,
            updated_at: 1,
            source: None,
            source_proposal_id: None,
            archived_at: None,
            archived_by: None,
        }
    }

    fn revision(
        id: &str,
        space_id: &str,
        item: &str,
        body: InstructionRevisionBody,
    ) -> InstructionRevisionView {
        InstructionRevisionView {
            id: id.into(),
            space_id: space_id.into(),
            work_item_id: item.into(),
            body,
            created_by: None,
            created_at: 1,
            integrity: InstructionRevisionIntegrity::Ok,
        }
    }

    pub(crate) fn snapshot(space_id: &str) -> WorkOutlineSnapshot {
        let mut snapshot = fixture_snapshot(space_id);
        let mut auth = instruction(I_AUTH, space_id, Some(API_AUTH), "a0", "Never log tokens.");
        auth.source = Some(InstructionSource {
            chat_id: Some(CHAT_AUTH.into()),
            message_id: Some("m-auth".into()),
            excerpt: Some("We must never log bearer tokens.".into()),
            content_hash: Some(excerpt_content_hash("We must never log bearer tokens.")),
        });
        snapshot.instructions = vec![
            instruction(
                I_ROOT,
                space_id,
                None,
                "a0",
                "Prefer small, reviewable changes.",
            ),
            instruction(
                I_LAUNCH,
                space_id,
                Some(LAUNCH),
                "a0",
                "Ship behind the launch flag.",
            ),
            instruction(I_API, space_id, Some(API), "a0", "Document every endpoint."),
            auth,
        ];
        // chat-api applied a revision where the API instruction read
        // differently: it drifted.
        let mut old = snapshot.clone();
        old.instructions[2].text = "Document public endpoints.".into();
        let old_body = current_body(&old, API).unwrap();
        let auth_body = current_body(&snapshot, API_AUTH).unwrap();
        let api_hash = effective_hash(&snapshot, Some(API));
        snapshot.chat_instruction_states = vec![
            ChatInstructionState {
                chat_id: CHAT_API.into(),
                selected_revision_id: Some(R_API_OLD.into()),
                selected_revision_hash: Some(old_body.hash.clone()),
                current_effective_hash: Some(api_hash),
                drift: true,
                delivered_revision_id: Some(R_API_OLD.into()),
                delivered_revision_hash: Some(old_body.hash.clone()),
                selected_revision_integrity: None,
            },
            ChatInstructionState {
                chat_id: CHAT_AUTH.into(),
                selected_revision_id: Some(R_AUTH.into()),
                selected_revision_hash: Some(auth_body.hash.clone()),
                current_effective_hash: Some(auth_body.hash.clone()),
                drift: false,
                delivered_revision_id: None,
                delivered_revision_hash: None,
                selected_revision_integrity: None,
            },
        ];
        snapshot.revisions = vec![
            revision(R_API_OLD, space_id, API, old_body),
            revision(R_AUTH, space_id, API_AUTH, auth_body),
        ];
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;
    use crate::work_outline::store::fixture_ids::*;
    use zeron_proto::TextDiffKind;

    #[test]
    fn effective_order_marks_inherited_entries_and_budget() {
        let snapshot = snapshot("s1");
        let effective = snapshot.effective_instructions(API_AUTH);
        let ids: Vec<_> = effective
            .entries
            .iter()
            .map(|e| (e.instruction_id.as_str(), e.inherited))
            .collect();
        assert_eq!(
            ids,
            [
                (I_ROOT, true),
                (I_LAUNCH, true),
                (I_API, true),
                (I_AUTH, false)
            ]
        );
        assert!(effective.within_budget);
        assert_eq!(effective.entry_count, 4);
    }

    #[test]
    fn local_lists_and_provenance() {
        let snapshot = snapshot("s1");
        let root: Vec<_> = live_instructions(&snapshot, None)
            .iter()
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(root, [I_ROOT]);
        let auth = live_instructions(&snapshot, Some(API_AUTH));
        assert_eq!(provenance_badge(auth[0]), Some("from Chat"));
        assert_eq!(
            provenance_badge(live_instructions(&snapshot, Some(API))[0]),
            None
        );
        assert_eq!(
            excerpt_content_hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn assigning_a_chat_drifts_only_when_effective_instructions_differ() {
        let snapshot = snapshot("s1");
        // chat-api: API -> UI loses the API instruction.
        let impact = drift_impact(
            &snapshot,
            &OutlineChange::AssignChat {
                chat_id: CHAT_API.into(),
                work_item_id: UI.into(),
            },
        );
        assert_eq!(impact.chat_ids, [CHAT_API]);
        assert!(impact.needs_confirmation());
        // A loose chat to Housekeeping (no work-item instructions) still gains
        // the Space-root instruction.
        let impact = drift_impact(
            &snapshot,
            &OutlineChange::AssignChat {
                chat_id: CHAT_LOOSE_A.into(),
                work_item_id: HOUSEKEEPING.into(),
            },
        );
        assert!(impact.needs_confirmation());
        // Without any instructions nothing changes.
        let mut bare = snapshot.clone();
        bare.instructions.clear();
        let impact = drift_impact(
            &bare,
            &OutlineChange::AssignChat {
                chat_id: CHAT_LOOSE_A.into(),
                work_item_id: HOUSEKEEPING.into(),
            },
        );
        assert!(!impact.needs_confirmation());
    }

    #[test]
    fn removing_a_chat_drifts_only_when_it_had_instructions() {
        let snapshot = snapshot("s1");
        let remove = OutlineChange::RemoveChat {
            chat_id: CHAT_UI.into(),
        };
        assert_eq!(drift_impact(&snapshot, &remove).chat_ids, [CHAT_UI]);
        let mut bare = snapshot.clone();
        bare.instructions.clear();
        assert!(!drift_impact(&bare, &remove).needs_confirmation());
    }

    #[test]
    fn moving_toggling_editing_and_archiving_report_affected_chats_and_items() {
        let snapshot = snapshot("s1");
        // Moving API (with Auth below) to top level drops Launch's instruction.
        let impact = drift_impact(
            &snapshot,
            &OutlineChange::MoveItem {
                work_item_id: API.into(),
                new_parent: None,
            },
        );
        assert_eq!(impact.chat_ids, [CHAT_API, CHAT_AUTH]);
        assert_eq!(impact.item_ids, [API, API_AUTH]);
        assert_eq!(impact.summary(), "2 Chats and 2 work items");
        // Housekeeping (no Chats) under Launch gains Launch's instruction.
        let impact = drift_impact(
            &snapshot,
            &OutlineChange::MoveItem {
                work_item_id: HOUSEKEEPING.into(),
                new_parent: Some(LAUNCH.into()),
            },
        );
        assert_eq!(impact.item_ids, [HOUSEKEEPING]);
        assert!(!impact.needs_confirmation(), "no Chat is assigned there");
        // Turning inheritance off on Auth drops Launch and API.
        let impact = drift_impact(
            &snapshot,
            &OutlineChange::SetInheritance {
                work_item_id: API_AUTH.into(),
                inherit: false,
            },
        );
        assert_eq!(impact.chat_ids, [CHAT_AUTH]);
        // Re-enabling what is already on changes nothing.
        let impact = drift_impact(
            &snapshot,
            &OutlineChange::SetInheritance {
                work_item_id: API_AUTH.into(),
                inherit: true,
            },
        );
        assert!(!impact.needs_confirmation());
        // Editing the Space-root instruction reaches every assigned Chat.
        let impact = drift_impact(
            &snapshot,
            &OutlineChange::EditInstruction {
                instruction_id: I_ROOT.into(),
                text: "Prefer tiny changes.".into(),
            },
        );
        assert_eq!(impact.chat_ids, [CHAT_API, CHAT_AUTH, CHAT_UI]);
        // Saving identical text is not a change.
        let impact = drift_impact(
            &snapshot,
            &OutlineChange::EditInstruction {
                instruction_id: I_ROOT.into(),
                text: "Prefer small, reviewable changes.".into(),
            },
        );
        assert!(!impact.needs_confirmation());
        // Archiving the API instruction affects API and Auth Chats only.
        let impact = drift_impact(
            &snapshot,
            &OutlineChange::ArchiveInstruction {
                instruction_id: I_API.into(),
            },
        );
        assert_eq!(impact.chat_ids, [CHAT_API, CHAT_AUTH]);
        assert!(impact.body().contains("keep their applied revision"));
    }

    #[test]
    fn local_comparison_and_delivery_labels() {
        let snapshot = snapshot("s1");
        let comparison = chat_comparison(&snapshot, CHAT_API).unwrap();
        assert!(!comparison.identical);
        assert_eq!(comparison.changed.len(), 1);
        assert_eq!(comparison.changed[0].after_text, "Document every endpoint.");
        assert!(
            comparison
                .line_diff
                .iter()
                .any(|l| l.kind == TextDiffKind::Added)
        );
        let auth = chat_comparison(&snapshot, CHAT_AUTH).unwrap();
        assert!(auth.identical);
        assert_eq!(
            chat_comparison(&snapshot, CHAT_UI),
            Err(CompareUnavailable::NothingToCompare)
        );
        let mut missing = snapshot.clone();
        missing.revisions.clear();
        assert_eq!(
            chat_comparison(&missing, CHAT_API),
            Err(CompareUnavailable::MissingRevision(R_API_OLD.into()))
        );

        let api = snapshot.chat_instruction_state(CHAT_API).unwrap();
        assert_eq!(delivery_line(api), None, "delivered == selected");
        let auth = snapshot.chat_instruction_state(CHAT_AUTH).unwrap();
        let line = delivery_line(auth).unwrap();
        assert!(line.starts_with("Selected r") && line.ends_with("Not delivered yet"));
        assert_eq!(affected_descendant_count(&snapshot, Some(LAUNCH)), 3);
        assert_eq!(affected_descendant_count(&snapshot, None), 5);
    }
}
