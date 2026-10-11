//! Work Outline instruction rows (Milestone 2): `workItemInstructions`,
//! `instructionRevisions`, `instructionRevisionApplications`, and the
//! host-written `executionSession` report on session-status rows.
//!
//! Schemas: `docs/plans/work-outline/m0-registry-persistence.md` 7.2, 7.8,
//! 7.9 as amended by `m0-contract.md` sections 5.2 and 5.6 and the
//! "Milestone 2 contract notes". The write rules are the Milestone 1 ones
//! (create-if-absent that never revives a tombstone, `update` ops without
//! immutable fields, clock observation, size gate, one batch per mutation):
//!
//! - instructions: id `derive_id(operationId, "instruction")`; `spaceId`,
//!   `workItemId` (absent = Space root), `source`, `createdBy`, `createdAt`,
//!   `inputsHash` are written once at create; `text`, `orderKey`, and the
//!   archive pair `archivedAt` + `archivedBy` (one op) are LWW. Ordering is
//!   `(orderKey, id)` within the scope; re-keyed siblings are order-only
//!   writes. Budgets: text, excerpt, live instructions per scope, and every
//!   affected item's effective instructions must still build a revision.
//! - revisions: id `derive_id(operationId, "revision")`, ONE `body` JSON
//!   field written by a single upsert, never updated. Integrity is verified
//!   on every read.
//! - applications: id `derive_id(operationId, "application")`, written once;
//!   `appliedHlc` is the writing op's own HLC; `revisionHash` (additive)
//!   records the verified hash so a selection can be pinned before the
//!   revision row syncs.

use std::collections::HashMap;

use zeron_proto::work_outline::{
    ApplyInstructionRevisionParams, ArchiveWorkItemInstructionParams,
    CreateInstructionRevisionParams, CreateWorkItemInstructionParams, InstructionPlacement,
    MoveWorkItemInstructionParams, UpdateWorkItemInstructionParams, WorkItemView,
};
use zeron_proto::work_outline_instructions::{
    EffectiveInstructions, ExecutionSessionReport, InstructionRevisionIntegrity,
    InstructionRevisionPin, InstructionRevisionView, InstructionScope, InstructionSource,
    SPACE_ROOT_SCOPE_TITLE, WorkItemInstructionView, build_revision_body,
    resolve_effective_instructions,
};
use zeron_proto::work_outline_order::valid_order_key;

use super::materialize::{
    DecodedApplication, RevisionCheck, bare_view, decode_application, decode_instruction,
    decode_revision, resolver_views, select_application,
};
use super::*;

/// Longest accepted source Chat / message id.
const MAX_SOURCE_ID_CHARS: usize = 256;

fn limit(message: impl Into<String>) -> WorkOutlineError {
    err(WorkOutlineErrorCode::LimitExceeded, message)
}

pub(super) fn check_instruction_text(text: &str) -> Outcome<()> {
    if text.trim().is_empty() {
        return Err(invalid("An instruction needs text"));
    }
    let chars = text.chars().count();
    if chars > limits::MAX_INSTRUCTION_TEXT_CHARS {
        return Err(limit(format!(
            "Instructions are limited to {} characters (this one is {chars})",
            limits::MAX_INSTRUCTION_TEXT_CHARS
        )));
    }
    Ok(())
}

/// Validate creation provenance. `contentHash` is recomputed as
/// `sha256(excerpt)` whenever an excerpt is present (contract M2 note 5); a
/// hash without an excerpt is kept as given. An empty source is no source.
pub(super) fn checked_source(
    source: Option<&InstructionSource>,
) -> Outcome<Option<InstructionSource>> {
    let Some(source) = source else {
        return Ok(None);
    };
    if let Some(excerpt) = &source.excerpt {
        let chars = excerpt.chars().count();
        if chars > limits::MAX_INSTRUCTION_EXCERPT_CHARS {
            return Err(limit(format!(
                "Source excerpts are limited to {} characters (this one is {chars})",
                limits::MAX_INSTRUCTION_EXCERPT_CHARS
            )));
        }
    }
    for (what, value) in [
        ("Chat", &source.chat_id),
        ("message", &source.message_id),
        ("content hash", &source.content_hash),
    ] {
        if let Some(value) = value
            && (value.is_empty() || value.chars().count() > MAX_SOURCE_ID_CHARS)
        {
            return Err(invalid(format!("Invalid source {what}")));
        }
    }
    let checked = InstructionSource {
        content_hash: source
            .excerpt
            .as_ref()
            .map(|excerpt| format!("{:x}", Sha256::digest(excerpt.as_bytes())))
            .or_else(|| source.content_hash.clone()),
        ..source.clone()
    };
    Ok((checked != InstructionSource::default()).then_some(checked))
}

/// Live, non-archived instructions of one scope as `(id, valid key)`, in
/// the resolver's `(orderKey, id)` order, without `exclude`.
fn scope_siblings(
    instructions: &[WorkItemInstructionView],
    scope: Option<&str>,
    exclude: Option<&str>,
) -> Vec<(String, Option<String>)> {
    let mut scope: Vec<&WorkItemInstructionView> = instructions
        .iter()
        .filter(|i| {
            !i.archived() && i.work_item_id.as_deref() == scope && Some(i.id.as_str()) != exclude
        })
        .collect();
    scope.sort_by(|a, b| (&a.order_key, &a.id).cmp(&(&b.order_key, &b.id)));
    scope
        .into_iter()
        .map(|i| {
            (
                i.id.clone(),
                Some(i.order_key.clone()).filter(|k| valid_order_key(k)),
            )
        })
        .collect()
}

fn instruction_placement_index(
    siblings: &[(String, Option<String>)],
    placement: &InstructionPlacement,
) -> Outcome<usize> {
    let find = |sibling: &str| {
        siblings
            .iter()
            .position(|(id, _)| id == sibling)
            .ok_or_else(|| not_found("The placement sibling is not an instruction of this scope"))
    };
    Ok(match placement {
        InstructionPlacement::First => 0,
        InstructionPlacement::Last => siblings.len(),
        InstructionPlacement::Before {
            sibling_instruction_id,
        } => find(sibling_instruction_id)?,
        InstructionPlacement::After {
            sibling_instruction_id,
        } => find(sibling_instruction_id)? + 1,
    })
}

fn placement_sibling(placement: &InstructionPlacement) -> Option<&str> {
    match placement {
        InstructionPlacement::Before {
            sibling_instruction_id,
        }
        | InstructionPlacement::After {
            sibling_instruction_id,
        } => Some(sibling_instruction_id),
        InstructionPlacement::First | InstructionPlacement::Last => None,
    }
}

/// A new instruction to plan ([`RegistryDoc::plan_new_instruction`]).
pub(super) struct NewInstruction<'a> {
    pub id: &'a str,
    pub space_id: &'a str,
    /// `None` = Space root (already validated).
    pub work_item_id: Option<&'a str>,
    pub text: &'a str,
    pub placement: InstructionPlacement,
    /// Already checked with [`checked_source`].
    pub source: Option<InstructionSource>,
    pub source_proposal_id: Option<&'a str>,
    pub inputs_hash: &'a str,
    /// Leads the budget error ("Adding this instruction would put …").
    pub action: &'a str,
}

/// Order-only writes for re-keyed sibling instructions.
fn plan_instruction_rekeys(rekeys: &[(String, String)], plan: &mut Vec<Planned>) {
    for (id, key) in rekeys {
        plan.push(Planned::update(
            KIND_WORK_ITEM_INSTRUCTIONS,
            id,
            BTreeMap::from([("orderKey".to_owned(), json!(key))]),
        ));
    }
}

/// UTF-16 units of a text once JSON-escaped (what the revision body budget
/// measures).
fn escaped_units(text: &str) -> usize {
    serde_json::to_string(text).map_or(usize::MAX, |s| s.encode_utf16().count())
}

impl RegistryDoc {
    // ── reads ───────────────────────────────────────────────────────────────

    /// Application, revision, and session rows the materializer needs for
    /// one Space (see [`WorkOutlineInputs`]).
    pub(super) fn instruction_inputs(
        &self,
        space_id: &str,
    ) -> (Vec<RegistryRow>, Vec<RegistryRow>, Vec<RegistryRow>) {
        // Only each Chat's newest application decides its selection, so only
        // those rows (and their revisions) are cloned: frames stay the same
        // size however many revisions a Chat has applied over time.
        let mut newest: BTreeMap<String, (DecodedApplication, RegistryRow)> = BTreeMap::new();
        for row in self.overlay_rows(KIND_INSTRUCTION_REVISION_APPLICATIONS) {
            if row.fields.get("spaceId").and_then(Value::as_str) != Some(space_id) {
                continue;
            }
            let Some(decoded) = decode_application(&row) else {
                continue;
            };
            let replace = newest.get(&decoded.view.chat_id).is_none_or(|(kept, _)| {
                (&decoded.view.applied_hlc, &decoded.view.id)
                    > (&kept.view.applied_hlc, &kept.view.id)
            });
            if replace {
                newest.insert(decoded.view.chat_id.clone(), (decoded, row));
            }
        }
        let applications: Vec<RegistryRow> = newest.into_values().map(|(_, row)| row).collect();
        let sessions: Vec<RegistryRow> = self
            .overlay_rows(KIND_SESSIONS)
            .into_iter()
            .filter(|row| {
                row.fields
                    .get("executionSession")
                    .is_some_and(Value::is_object)
            })
            .collect();
        let wanted: BTreeSet<&str> = applications
            .iter()
            .filter_map(|row| row.fields.get("revisionId").and_then(Value::as_str))
            .chain(sessions.iter().filter_map(|row| {
                row.fields
                    .get("executionSession")
                    .and_then(|report| report.get("revisionId"))
                    .and_then(Value::as_str)
            }))
            .collect();
        let revisions = wanted
            .into_iter()
            .filter_map(|id| self.overlay_row(KIND_INSTRUCTION_REVISIONS, id))
            .collect();
        (applications, revisions, sessions)
    }

    /// The live revision `revision_id` with its integrity verified.
    pub fn instruction_revision(&self, revision_id: &str) -> Option<InstructionRevisionView> {
        self.overlay_row(KIND_INSTRUCTION_REVISIONS, revision_id)
            .map(|row| decode_revision(&row))
    }

    /// The Space of a live work item.
    pub fn work_item_space(&self, work_item_id: &str) -> Option<String> {
        self.live_item(work_item_id).ok().map(|item| item.space_id)
    }

    /// The Chat's selected revision: the newest accepted application by
    /// `(appliedHlc, id)` among this Space's applications, never substituted
    /// (the same rule as the snapshot's `chatInstructionStates`). A missing
    /// or corrupt revision still returns its pin (hash as recorded by the
    /// application) so delivery fails closed on the integrity check.
    pub fn selected_instruction_revision(&self, chat_id: &str) -> Option<InstructionRevisionPin> {
        let space_id = self
            .chat(chat_id)
            .ok()
            .flatten()?
            .space_id
            .filter(|s| !s.is_empty())?;
        let applications: Vec<DecodedApplication> = self
            .overlay_rows(KIND_INSTRUCTION_REVISION_APPLICATIONS)
            .iter()
            .filter_map(decode_application)
            .filter(|a| a.view.chat_id == chat_id && a.view.space_id == space_id)
            .collect();
        let (application, _integrity, hash) =
            select_application(&applications, |id| match self.instruction_revision(id) {
                None => RevisionCheck::Missing,
                Some(view)
                    if view.space_id == space_id
                        && view.integrity == InstructionRevisionIntegrity::Ok =>
                {
                    RevisionCheck::Valid(view.body.hash)
                }
                Some(view) => RevisionCheck::Corrupt(view.body.hash),
            })?;
        Some(InstructionRevisionPin {
            revision_id: application.view.revision_id.clone(),
            revision_hash: hash,
        })
    }

    /// Decoded instructions of one Space (archived included).
    fn instructions_of_space(&self, space_id: &str) -> Vec<WorkItemInstructionView> {
        self.overlay_rows(KIND_WORK_ITEM_INSTRUCTIONS)
            .iter()
            .filter_map(|row| decode_instruction(row).ok())
            .filter(|i| i.space_id == space_id)
            .collect()
    }

    fn live_instruction(&self, id: &str) -> Outcome<WorkItemInstructionView> {
        check_id("instruction", id)?;
        let row = self
            .overlay_row(KIND_WORK_ITEM_INSTRUCTIONS, id)
            .ok_or_else(|| not_found("Instruction not found"))?;
        decode_instruction(&row).map_err(|_| invalid("Instruction row is malformed"))
    }

    /// The current effective instructions of a live item, resolved over the
    /// repaired tree exactly as the snapshot resolves them.
    pub fn effective_instructions_of(&self, work_item_id: &str) -> Outcome<EffectiveInstructions> {
        let item = self.live_item(work_item_id)?;
        let tree = self.outline_tree(&item.space_id);
        let views = resolver_views(&tree);
        let by_id: HashMap<&str, &WorkItemView> =
            views.iter().map(|view| (view.id.as_str(), view)).collect();
        let instructions = self.instructions_of_space(&item.space_id);
        Ok(resolve_effective_instructions(
            work_item_id,
            |id| by_id.get(id).copied(),
            &instructions,
        ))
    }

    /// Display label of an instruction scope for error messages.
    fn scope_label(&self, work_item_id: Option<&str>) -> String {
        match work_item_id {
            None => SPACE_ROOT_SCOPE_TITLE.to_owned(),
            Some(id) => {
                let title = self
                    .live_item(id)
                    .ok()
                    .and_then(|item| item.title)
                    .unwrap_or_else(|| "Untitled".into());
                format!("\"{title}\"")
            }
        }
    }

    /// Every item whose effective instructions include `scope` (all items of
    /// the Space for the Space root, else the item and its descendants) must
    /// still build a revision with `instructions` (this Space's rows after
    /// the change), so no item can be pushed over the budget. Items sharing
    /// the same contributing scopes are checked once.
    fn check_instruction_budget(
        &self,
        space_id: &str,
        scope: Option<&str>,
        instructions: &[WorkItemInstructionView],
        action: &str,
    ) -> Outcome<()> {
        let live: Vec<WorkItemInstructionView> = instructions
            .iter()
            .filter(|i| !i.archived())
            .cloned()
            .collect();
        let tree = self.outline_tree(space_id);
        let mut views = resolver_views(&tree);
        // The Space root alone (a Space with no items still has a budget).
        const ROOT_PROBE: &str = "\u{0}space-root";
        views.push(bare_view(
            ROOT_PROBE,
            space_id,
            None,
            SPACE_ROOT_SCOPE_TITLE.into(),
            false,
        ));
        let by_id: HashMap<&str, &WorkItemView> =
            views.iter().map(|view| (view.id.as_str(), view)).collect();
        let contributing: BTreeSet<&str> = live
            .iter()
            .filter_map(|i| i.work_item_id.as_deref())
            .collect();
        let affected: Vec<String> = match scope {
            None => std::iter::once(ROOT_PROBE.to_owned())
                .chain(tree.items.keys().cloned())
                .collect(),
            Some(item) => std::iter::once(item.to_owned())
                .chain(tree.descendants(item))
                .collect(),
        };
        let mut checked: BTreeSet<Vec<&str>> = BTreeSet::new();
        for id in &affected {
            let mut key = Vec::new();
            let mut seen = BTreeSet::new();
            let mut cursor = by_id.get(id.as_str()).copied();
            while let Some(item) = cursor {
                if !seen.insert(item.id.as_str()) {
                    break;
                }
                if contributing.contains(item.id.as_str()) {
                    key.push(item.id.as_str());
                }
                if !item.inherit_parent_instructions {
                    break;
                }
                cursor = item
                    .parent_work_item_id
                    .as_deref()
                    .and_then(|p| by_id.get(p).copied());
            }
            if !checked.insert(key) {
                continue;
            }
            let effective = resolve_effective_instructions(id, |x| by_id.get(x).copied(), &live);
            if let Some(error) = effective.budget_error {
                let target = if id == ROOT_PROBE {
                    SPACE_ROOT_SCOPE_TITLE.to_owned()
                } else {
                    format!("\"{}\"", by_id[id.as_str()].title)
                };
                return Err(limit(format!(
                    "{action} would put {target} over the instruction budget: {error}"
                )));
            }
        }
        Ok(())
    }

    // ── instruction mutations ──────────────────────────────────────────────

    /// `createWorkItemInstruction`: id = `derive_id(operationId,
    /// "instruction")`; a replay returns the same id without writing, other
    /// inputs are `OperationConflict`, a deleted id is never revived.
    pub fn create_work_item_instruction(
        &mut self,
        p: &CreateWorkItemInstructionParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<String> {
        check_operation_id(&p.operation_id)?;
        check_instruction_text(&p.text)?;
        let source = checked_source(p.source.as_ref())?;
        let id = derive_id(&p.operation_id, roles::INSTRUCTION);
        let hash = inputs_hash(p);
        match self.row_state(KIND_WORK_ITEM_INSTRUCTIONS, &id) {
            RowState::Live => {
                let stored = self
                    .overlay_row(KIND_WORK_ITEM_INSTRUCTIONS, &id)
                    .and_then(|row| row.fields.get("inputsHash").cloned());
                return if stored == Some(json!(hash)) {
                    Ok(id)
                } else {
                    Err(err(
                        WorkOutlineErrorCode::OperationConflict,
                        "operationId was already used for a different instruction",
                    ))
                };
            }
            RowState::Tombstoned => return Err(not_found("This instruction was deleted")),
            RowState::Absent => {}
        }
        self.require_space(&p.space_id)?;
        let work_item_id = self.instruction_scope_item(&p.space_id, &p.scope)?;
        let plan = self.plan_new_instruction(
            NewInstruction {
                id: &id,
                space_id: &p.space_id,
                work_item_id: work_item_id.as_deref(),
                text: &p.text,
                placement: p.placement.clone().unwrap_or_default(),
                source,
                source_proposal_id: None,
                inputs_hash: &hash,
                action: "Adding this instruction",
            },
            actor,
            now_ms,
        )?;
        let extra: Vec<(&str, &str)> = work_item_id
            .as_deref()
            .map(|w| (KIND_WORK_ITEMS, w))
            .into_iter()
            .collect();
        self.commit(plan, &extra)?;
        Ok(id)
    }

    /// The work item of an instruction scope, validated for a new
    /// instruction: live, in `space_id`, not archived (`None` = Space root).
    pub(super) fn instruction_scope_item(
        &self,
        space_id: &str,
        scope: &InstructionScope,
    ) -> Outcome<Option<String>> {
        match scope {
            InstructionScope::SpaceRoot => Ok(None),
            InstructionScope::WorkItem { work_item_id } => {
                check_id("work item", work_item_id)?;
                let item = self.live_item(work_item_id)?;
                if item.space_id != space_id {
                    return Err(err(
                        WorkOutlineErrorCode::CrossSpace,
                        "The work item belongs to another Space",
                    ));
                }
                if item.archived() {
                    return Err(err(
                        WorkOutlineErrorCode::Archived,
                        "The work item is archived",
                    ));
                }
                Ok(Some(work_item_id.clone()))
            }
        }
    }

    /// Plan the writes of a new instruction (placement re-keys, then the
    /// create upsert) after the per-scope cap and every affected item's
    /// budget check. Shared by `createWorkItemInstruction` and proposal
    /// approval (Milestone 4), which commits it in one batch with the review.
    pub(super) fn plan_new_instruction(
        &self,
        new: NewInstruction<'_>,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<Vec<Planned>> {
        let scope = new.work_item_id;
        let mut instructions = self.instructions_of_space(new.space_id);
        let siblings = scope_siblings(&instructions, scope, None);
        if siblings.len() >= limits::MAX_INSTRUCTIONS_PER_SCOPE {
            return Err(limit(format!(
                "{} already has {} instructions; archive one before adding another",
                self.scope_label(scope),
                limits::MAX_INSTRUCTIONS_PER_SCOPE
            )));
        }
        let index = instruction_placement_index(&siblings, &new.placement)?;
        let placed = place(&siblings, index, 1)?;
        instructions.push(WorkItemInstructionView {
            id: new.id.to_owned(),
            space_id: new.space_id.to_owned(),
            work_item_id: scope.map(str::to_owned),
            text: new.text.to_owned(),
            order_key: placed.keys[0].clone(),
            created_by: Some(actor.clone()),
            created_at: now_ms,
            updated_at: now_ms,
            source: new.source.clone(),
            source_proposal_id: new.source_proposal_id.map(str::to_owned),
            archived_at: None,
            archived_by: None,
        });
        self.check_instruction_budget(new.space_id, scope, &instructions, new.action)?;

        let mut plan = Vec::new();
        plan_instruction_rekeys(&placed.rekeys, &mut plan);
        let mut set = BTreeMap::from([
            ("spaceId".to_owned(), json!(new.space_id)),
            ("text".to_owned(), json!(new.text)),
            ("orderKey".to_owned(), json!(placed.keys[0])),
            ("createdBy".to_owned(), actor_row_value(actor)),
            ("createdAt".to_owned(), json!(now_ms)),
            ("updatedAt".to_owned(), json!(now_ms)),
            ("inputsHash".to_owned(), json!(new.inputs_hash)),
            ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
        ]);
        if let Some(work_item_id) = scope {
            set.insert("workItemId".into(), json!(work_item_id));
        }
        if let Some(source) = &new.source {
            set.insert(
                "source".into(),
                serde_json::to_value(source).unwrap_or(Value::Null),
            );
        }
        if let Some(proposal_id) = new.source_proposal_id {
            set.insert("sourceProposalId".into(), json!(proposal_id));
        }
        plan.push(Planned::upsert(KIND_WORK_ITEM_INSTRUCTIONS, new.id, set));
        Ok(plan)
    }

    /// `updateWorkItemInstruction`: text only (scope and source are
    /// immutable). Unchanged text writes nothing; growing text is checked
    /// against every affected item's budget.
    pub fn update_work_item_instruction(
        &mut self,
        p: &UpdateWorkItemInstructionParams,
        now_ms: i64,
    ) -> Outcome<()> {
        let instruction = self.live_instruction(&p.instruction_id)?;
        self.require_space(&instruction.space_id)?;
        if instruction.archived() {
            return Err(err(
                WorkOutlineErrorCode::Archived,
                "Archived instructions cannot be edited",
            ));
        }
        check_instruction_text(&p.text)?;
        if instruction.text == p.text {
            return Ok(());
        }
        let grows = p.text.chars().count() > instruction.text.chars().count()
            || escaped_units(&p.text) > escaped_units(&instruction.text);
        if grows {
            let instructions: Vec<WorkItemInstructionView> = self
                .instructions_of_space(&instruction.space_id)
                .into_iter()
                .map(|mut i| {
                    if i.id == instruction.id {
                        i.text = p.text.clone();
                    }
                    i
                })
                .collect();
            self.check_instruction_budget(
                &instruction.space_id,
                instruction.work_item_id.as_deref(),
                &instructions,
                "This edit",
            )?;
        }
        self.commit(
            vec![Planned::update(
                KIND_WORK_ITEM_INSTRUCTIONS,
                &p.instruction_id,
                BTreeMap::from([
                    ("text".to_owned(), json!(p.text)),
                    ("updatedAt".to_owned(), json!(now_ms)),
                ]),
            )],
            &[],
        )
    }

    /// `moveWorkItemInstruction`: reorder within the instruction's own scope
    /// (an instruction already at the requested position writes nothing).
    pub fn move_work_item_instruction(
        &mut self,
        p: &MoveWorkItemInstructionParams,
        now_ms: i64,
    ) -> Outcome<()> {
        let instruction = self.live_instruction(&p.instruction_id)?;
        self.require_space(&instruction.space_id)?;
        if instruction.archived() {
            return Err(err(
                WorkOutlineErrorCode::Archived,
                "Archived instructions cannot be moved",
            ));
        }
        if placement_sibling(&p.placement) == Some(p.instruction_id.as_str()) {
            return Err(invalid(
                "An instruction cannot be placed relative to itself",
            ));
        }
        let scope = instruction.work_item_id.as_deref();
        let instructions = self.instructions_of_space(&instruction.space_id);
        let siblings = scope_siblings(&instructions, scope, Some(&p.instruction_id));
        let index = instruction_placement_index(&siblings, &p.placement)?;
        let current = scope_siblings(&instructions, scope, None)
            .iter()
            .position(|(id, _)| *id == p.instruction_id);
        if current == Some(index) && valid_order_key(&instruction.order_key) {
            return Ok(()); // already there
        }
        let placed = place(&siblings, index, 1)?;
        let mut plan = Vec::new();
        plan_instruction_rekeys(&placed.rekeys, &mut plan);
        plan.push(Planned::update(
            KIND_WORK_ITEM_INSTRUCTIONS,
            &p.instruction_id,
            BTreeMap::from([
                ("orderKey".to_owned(), json!(placed.keys[0])),
                ("updatedAt".to_owned(), json!(now_ms)),
            ]),
        ));
        self.commit(plan, &[])
    }

    /// `archiveWorkItemInstruction`: the archive pair in one op. Existing
    /// revisions keep the text they snapshotted.
    pub fn archive_work_item_instruction(
        &mut self,
        p: &ArchiveWorkItemInstructionParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<()> {
        let instruction = self.live_instruction(&p.instruction_id)?;
        self.require_space(&instruction.space_id)?;
        if instruction.archived() {
            return Ok(());
        }
        self.commit(
            vec![Planned::update(
                KIND_WORK_ITEM_INSTRUCTIONS,
                &p.instruction_id,
                BTreeMap::from([
                    ("archivedAt".to_owned(), json!(now_ms)),
                    ("archivedBy".to_owned(), actor_row_value(actor)),
                    ("updatedAt".to_owned(), json!(now_ms)),
                ]),
            )],
            &[],
        )
    }

    // ── revisions and applications ─────────────────────────────────────────

    /// Shared create-if-absent check of the write-once kinds: `Some(id)` when
    /// the row already exists with the same inputs.
    fn existing_write_once(
        &self,
        kind: &str,
        id: &str,
        hash: &str,
        what: &str,
    ) -> Outcome<Option<String>> {
        match self.row_state(kind, id) {
            RowState::Live => {
                let stored = self
                    .overlay_row(kind, id)
                    .and_then(|row| row.fields.get("inputsHash").cloned());
                if stored == Some(json!(hash)) {
                    Ok(Some(id.to_owned()))
                } else {
                    Err(err(
                        WorkOutlineErrorCode::OperationConflict,
                        format!("operationId was already used for a different {what}"),
                    ))
                }
            }
            RowState::Tombstoned => Err(not_found(format!("This {what} was deleted"))),
            RowState::Absent => Ok(None),
        }
    }

    /// `createInstructionRevision`: snapshot the item's current effective
    /// instructions into an immutable revision (one upsert of the whole
    /// `body`). Over budget fails before any write. A new operation id over
    /// unchanged content writes a new row with the same hash.
    pub fn create_instruction_revision(
        &mut self,
        p: &CreateInstructionRevisionParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<String> {
        check_operation_id(&p.operation_id)?;
        let id = derive_id(&p.operation_id, roles::REVISION);
        let hash = inputs_hash(p);
        if let Some(id) =
            self.existing_write_once(KIND_INSTRUCTION_REVISIONS, &id, &hash, "revision")?
        {
            return Ok(id);
        }
        check_id("work item", &p.work_item_id)?;
        let item = self.live_item(&p.work_item_id)?;
        self.require_space(&item.space_id)?;
        let effective = self.effective_instructions_of(&p.work_item_id)?;
        let body = build_revision_body(effective.revision_entries())?;
        let set = BTreeMap::from([
            ("spaceId".to_owned(), json!(item.space_id)),
            ("workItemId".to_owned(), json!(p.work_item_id)),
            (
                "body".to_owned(),
                serde_json::to_value(&body).map_err(|e| invalid(e.to_string()))?,
            ),
            ("createdBy".to_owned(), actor_row_value(actor)),
            ("createdAt".to_owned(), json!(now_ms)),
            ("inputsHash".to_owned(), json!(hash)),
            ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
        ]);
        self.commit(
            vec![Planned::upsert(KIND_INSTRUCTION_REVISIONS, &id, set)],
            &[(KIND_WORK_ITEMS, p.work_item_id.as_str())],
        )?;
        Ok(id)
    }

    /// `applyInstructionRevision`: record that a revision is selected for a
    /// Chat (an immutable application row whose `appliedHlc` is its op's
    /// HLC). The Chat must be assigned to a live work item of its Space and
    /// the revision must belong to that Space and verify. Every existing
    /// application of the Chat is observed first, so an application made
    /// after seeing another device's always sorts after it.
    pub fn apply_instruction_revision(
        &mut self,
        p: &ApplyInstructionRevisionParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<String> {
        check_operation_id(&p.operation_id)?;
        let id = derive_id(&p.operation_id, roles::APPLICATION);
        let hash = inputs_hash(p);
        if let Some(id) = self.existing_write_once(
            KIND_INSTRUCTION_REVISION_APPLICATIONS,
            &id,
            &hash,
            "application",
        )? {
            return Ok(id);
        }
        check_id("Chat", &p.chat_id)?;
        check_id("revision", &p.revision_id)?;
        if let Some(message_id) = p.turn_boundary.as_ref().and_then(|t| t.message_id.as_ref())
            && (message_id.is_empty() || message_id.chars().count() > MAX_SOURCE_ID_CHARS)
        {
            return Err(invalid("Invalid turn boundary message id"));
        }
        let chat = self
            .chat(&p.chat_id)
            .ok()
            .flatten()
            .ok_or_else(|| not_found("Chat not found"))?;
        let Some(space_id) = chat.space_id.filter(|s| !s.is_empty()) else {
            return Err(err(
                WorkOutlineErrorCode::Projectless,
                "Chats outside a project have no Work Outline instructions",
            ));
        };
        self.require_space(&space_id)?;
        let unassigned = || invalid("Assign the Chat to a work item before applying instructions");
        let assignment = self
            .overlay_row(KIND_WORK_ITEM_CHAT_ASSIGNMENTS, &p.chat_id)
            .ok_or_else(unassigned)?;
        if assignment.fields.get("spaceId").and_then(Value::as_str) != Some(space_id.as_str()) {
            return Err(err(
                WorkOutlineErrorCode::CrossSpace,
                "The Chat's assignment belongs to another Space",
            ));
        }
        let work_item_id = assignment
            .fields
            .get("workItemId")
            .and_then(Value::as_str)
            .filter(|w| !w.is_empty())
            .ok_or_else(unassigned)?
            .to_owned();
        let item = self.live_item(&work_item_id).map_err(|_| unassigned())?;
        if item.space_id != space_id {
            return Err(err(
                WorkOutlineErrorCode::CrossSpace,
                "The Chat's work item belongs to another Space",
            ));
        }
        let revision = self
            .instruction_revision(&p.revision_id)
            .ok_or_else(|| not_found("Instruction revision not found"))?;
        if revision.space_id != space_id {
            return Err(err(
                WorkOutlineErrorCode::CrossSpace,
                "The instruction revision belongs to another Space",
            ));
        }
        if revision.integrity != InstructionRevisionIntegrity::Ok {
            return Err(err(
                WorkOutlineErrorCode::RevisionIntegrity,
                "The instruction revision failed its integrity check and cannot be applied",
            ));
        }
        let set = BTreeMap::from([
            ("spaceId".to_owned(), json!(space_id)),
            ("chatId".to_owned(), json!(p.chat_id)),
            ("workItemId".to_owned(), json!(work_item_id)),
            ("revisionId".to_owned(), json!(p.revision_id)),
            ("revisionHash".to_owned(), json!(revision.body.hash)),
            (
                "turnBoundary".to_owned(),
                serde_json::to_value(p.turn_boundary.clone().unwrap_or_default())
                    .unwrap_or(Value::Null),
            ),
            (
                "reason".to_owned(),
                serde_json::to_value(p.reason).unwrap_or(Value::Null),
            ),
            ("idempotencyKey".to_owned(), json!(p.operation_id)),
            ("result".to_owned(), json!("accepted")),
            ("createdBy".to_owned(), actor_row_value(actor)),
            ("createdAt".to_owned(), json!(now_ms)),
            ("inputsHash".to_owned(), json!(hash)),
            ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
        ]);
        let mut planned = Planned::upsert(KIND_INSTRUCTION_REVISION_APPLICATIONS, &id, set);
        planned.stamp_hlc = Some("appliedHlc");
        let earlier: Vec<String> = self
            .overlay_rows(KIND_INSTRUCTION_REVISION_APPLICATIONS)
            .into_iter()
            .filter(|row| {
                row.fields.get("chatId").and_then(Value::as_str) == Some(p.chat_id.as_str())
            })
            .map(|row| row.id)
            .collect();
        let mut extra: Vec<(&str, &str)> = vec![
            (KIND_INSTRUCTION_REVISIONS, p.revision_id.as_str()),
            (KIND_WORK_ITEM_CHAT_ASSIGNMENTS, p.chat_id.as_str()),
        ];
        extra.extend(
            earlier
                .iter()
                .map(|id| (KIND_INSTRUCTION_REVISION_APPLICATIONS, id.as_str())),
        );
        self.commit(vec![planned], &extra)?;
        Ok(id)
    }

    // ── host-reported Execution Session (contract 5.6) ──────────────────────

    /// Write `report` into the additive `executionSession` field of the
    /// Chat's session-status row (host-written). An equal report writes
    /// nothing; the row's clocks are observed first (R3); a deleted row is
    /// never revived. When no status row exists yet one is created as
    /// `idle` (the next status write overwrites it). Returns whether a write
    /// was enqueued.
    pub fn report_execution_session(
        &mut self,
        chat_id: &str,
        report: &ExecutionSessionReport,
        now_ms: i64,
    ) -> Result<bool, DocError> {
        let value = serde_json::to_value(report)?;
        let (op, set) = match self.row_state(KIND_SESSIONS, chat_id) {
            RowState::Tombstoned => return Ok(false),
            RowState::Live => {
                let current = self
                    .overlay_row(KIND_SESSIONS, chat_id)
                    .and_then(|row| row.fields.get("executionSession").cloned());
                if current.as_ref() == Some(&value) {
                    return Ok(false);
                }
                (
                    OpKind::Update,
                    BTreeMap::from([("executionSession".to_owned(), value)]),
                )
            }
            RowState::Absent => (
                OpKind::Upsert,
                BTreeMap::from([
                    ("chatId".to_owned(), json!(chat_id)),
                    ("deviceId".to_owned(), json!(self.device_id)),
                    ("status".to_owned(), json!("idle")),
                    ("updatedAt".to_owned(), json!(now_ms)),
                    ("executionSession".to_owned(), value),
                ]),
            ),
        };
        self.observe_row(KIND_SESSIONS, chat_id);
        let hlc = self.next_hlc();
        self.write_ops(vec![RowOp {
            kind: KIND_SESSIONS.to_owned(),
            id: chat_id.to_owned(),
            op,
            set: Some(set),
            hlc,
            clocks: None,
        }])?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
