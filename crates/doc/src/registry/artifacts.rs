//! Planning artifact rows (`artifacts`; contract `docs/plans/artifacts-contract.md`).
//!
//! Metadata only: one row per artifact whose fields are exactly
//! `mast_proto::ArtifactView`. Content lives in the owning device's profile
//! store. Write rules (registry R1–R6):
//! - creation is one `upsert` after a [`RowState`] check that never revives a
//!   tombstone; later writes are `update`s without the immutable fields
//!   (`id`, `spaceId`, `deviceId`, `workItemId`, `chatId`, `folder`,
//!   `createdAt`);
//! - the row is observed into the local HLC before a clock is minted;
//! - every op passes the size gate: revisions are trimmed (oldest first)
//!   until the op fits, so a long history can never wedge the queue;
//! - a pin change is one batch (the pinned row and every row it unpins).

use std::collections::BTreeMap;

use mast_proto::artifacts::{ArtifactView, KIND_ARTIFACTS, PlanScope, artifact_limits};
use serde_json::Value;

use super::{OpKind, RegistryDoc, RowOp, RowState, check_op_size};
use crate::schema::DocError;

/// Fields an update never rewrites.
const IMMUTABLE_FIELDS: [&str; 7] = [
    "id",
    "spaceId",
    "deviceId",
    "workItemId",
    "chatId",
    "folder",
    "createdAt",
];

/// Optional fields an update clears (writes `null`) when absent.
const CLEARABLE_FIELDS: [&str; 6] = [
    "role",
    "originPath",
    "previewPath",
    "promotedPath",
    "archivedAt",
    "companions",
];

fn decode(row: &super::RegistryRow) -> Option<ArtifactView> {
    let value = Value::Object(row.fields.clone().into_iter().collect());
    match serde_json::from_value::<ArtifactView>(value) {
        Ok(view) if !view.id.is_empty() => Some(view),
        Ok(_) => None,
        Err(err) => {
            tracing::warn!(row = %row.id, error = %err, "skipping malformed artifact row");
            None
        }
    }
}

fn fields_of(view: &ArtifactView, update: bool) -> Result<BTreeMap<String, Value>, DocError> {
    let Value::Object(map) = serde_json::to_value(view)? else {
        return Err(DocError::Schema(
            "artifact did not serialize to an object".into(),
        ));
    };
    let mut fields: BTreeMap<String, Value> = map.into_iter().collect();
    if update {
        for field in IMMUTABLE_FIELDS {
            fields.remove(field);
        }
        for field in CLEARABLE_FIELDS {
            fields.entry(field.to_owned()).or_insert(Value::Null);
        }
    }
    Ok(fields)
}

impl RegistryDoc {
    /// A live artifact.
    pub fn artifact(&self, id: &str) -> Option<ArtifactView> {
        self.overlay_row(KIND_ARTIFACTS, id)
            .as_ref()
            .and_then(decode)
    }

    /// Every live artifact (all Spaces).
    pub fn all_artifacts(&self) -> Vec<ArtifactView> {
        self.overlay_rows(KIND_ARTIFACTS)
            .iter()
            .filter_map(decode)
            .collect()
    }

    /// Live artifacts of one Space, sorted by `(workItemId, title, id)`.
    pub fn artifacts_of_space(&self, space_id: &str) -> Vec<ArtifactView> {
        let mut out: Vec<ArtifactView> = self
            .all_artifacts()
            .into_iter()
            .filter(|a| a.space_id == space_id)
            .collect();
        out.sort_by(|a, b| {
            (&a.work_item_id, &a.title, &a.id).cmp(&(&b.work_item_id, &b.title, &b.id))
        });
        out
    }

    /// Change marker of the artifact rows (plus a full `state` frame).
    pub fn artifacts_generation(&self) -> u64 {
        self.kinds_generation(&[KIND_ARTIFACTS])
    }

    /// Create or update one artifact row. Revisions are sorted newest first,
    /// capped at `MAX_REVISIONS`, and trimmed further (oldest first) until
    /// the op fits the size budget. A tombstoned id is never revived.
    /// Returns the row as written.
    pub fn put_artifact(&mut self, view: &ArtifactView) -> Result<ArtifactView, DocError> {
        let state = self.row_state(KIND_ARTIFACTS, &view.id);
        if state == RowState::Tombstoned {
            return Err(DocError::Schema(format!(
                "artifact {} was deleted",
                view.id
            )));
        }
        let mut view = view.clone();
        view.revisions.sort_by(|a, b| b.id.cmp(&a.id));
        view.revisions.dedup_by_key(|r| r.id);
        view.revisions.truncate(artifact_limits::MAX_REVISIONS);
        let update = state == RowState::Live;
        self.observe_row(KIND_ARTIFACTS, &view.id);
        let hlc = self.next_hlc();
        loop {
            let op = RowOp {
                kind: KIND_ARTIFACTS.to_owned(),
                id: view.id.clone(),
                op: if update {
                    OpKind::Update
                } else {
                    OpKind::Upsert
                },
                set: Some(fields_of(&view, update)?),
                hlc: hlc.clone(),
                clocks: None,
            };
            match check_op_size(&op) {
                Ok(()) => {
                    self.write_ops(vec![op])?;
                    return Ok(view);
                }
                // Keep at least the current revision.
                Err(error) if view.revisions.len() <= 1 => return Err(error),
                Err(_) => {
                    view.revisions.pop();
                }
            }
        }
    }

    /// Pin (`pinned = true`: unpinning every other pinned artifact of the
    /// same plan scope) or unpin one artifact, as one batch. Returns the ids
    /// this call unpinned besides the target. `None` when the artifact is
    /// not live.
    pub fn set_artifact_pin(
        &mut self,
        id: &str,
        pinned: bool,
        now_ms: i64,
    ) -> Result<Option<Vec<String>>, DocError> {
        let Some(target) = self.artifact(id) else {
            return Ok(None);
        };
        let mut unpin: Vec<String> = Vec::new();
        if pinned {
            let scope = target.plan_scope();
            unpin = self
                .artifacts_of_space(&target.space_id)
                .into_iter()
                .filter(|a| a.id != target.id && a.pinned_as_plan)
                .filter(|a| match (scope, a.plan_scope()) {
                    (Some(PlanScope::WorkItem(x)), Some(PlanScope::WorkItem(y))) => x == y,
                    (Some(PlanScope::Chat(x)), Some(PlanScope::Chat(y))) => x == y,
                    _ => false,
                })
                .map(|a| a.id)
                .collect();
        }
        if target.pinned_as_plan == pinned && unpin.is_empty() {
            return Ok(Some(unpin));
        }
        let mut changes: Vec<(String, bool)> = vec![(target.id.clone(), pinned)];
        changes.extend(unpin.iter().map(|id| (id.clone(), false)));
        for (id, _) in &changes {
            self.observe_row(KIND_ARTIFACTS, id);
        }
        let hlc = self.next_hlc();
        let ops: Vec<RowOp> = changes
            .into_iter()
            .map(|(id, pinned)| RowOp {
                kind: KIND_ARTIFACTS.to_owned(),
                id,
                op: OpKind::Update,
                set: Some(BTreeMap::from([
                    ("pinnedAsPlan".to_owned(), Value::Bool(pinned)),
                    ("updatedAt".to_owned(), Value::from(now_ms)),
                ])),
                hlc: hlc.clone(),
                clocks: None,
            })
            .collect();
        self.write_ops(ops)?;
        Ok(Some(unpin))
    }

    /// Replace the artifact's recorded question answers (the `answers` field
    /// only, so a device that does not own the content never rewrites its
    /// revisions). Oldest answers are dropped until the op fits. `None` when
    /// the artifact is not live.
    pub fn set_artifact_answers(
        &mut self,
        id: &str,
        answers: &[mast_proto::artifacts::ArtifactQuestionAnswer],
        now_ms: i64,
    ) -> Result<Option<Vec<mast_proto::artifacts::ArtifactQuestionAnswer>>, DocError> {
        if self.artifact(id).is_none() {
            return Ok(None);
        }
        let mut answers = answers.to_vec();
        self.observe_row(KIND_ARTIFACTS, id);
        let hlc = self.next_hlc();
        loop {
            let value = serde_json::to_value(&answers)
                .map_err(|e| DocError::Schema(format!("answers: {e}")))?;
            let op = RowOp {
                kind: KIND_ARTIFACTS.to_owned(),
                id: id.to_owned(),
                op: OpKind::Update,
                set: Some(BTreeMap::from([
                    ("answers".to_owned(), value),
                    ("updatedAt".to_owned(), Value::from(now_ms)),
                ])),
                hlc: hlc.clone(),
                clocks: None,
            };
            match check_op_size(&op) {
                Ok(()) => {
                    self.write_ops(vec![op])?;
                    return Ok(Some(answers));
                }
                Err(error) if answers.len() <= 1 => return Err(error),
                Err(_) => {
                    answers.remove(0);
                }
            }
        }
    }

    /// Replace the artifact's comments (the `comments` field only). When the
    /// op does not fit, the oldest resolved comment is dropped first, else
    /// the oldest. `None` when the artifact is not live.
    pub fn set_artifact_comments(
        &mut self,
        id: &str,
        comments: &[mast_proto::artifacts::ArtifactComment],
        now_ms: i64,
    ) -> Result<Option<Vec<mast_proto::artifacts::ArtifactComment>>, DocError> {
        if self.artifact(id).is_none() {
            return Ok(None);
        }
        let mut comments = comments.to_vec();
        self.observe_row(KIND_ARTIFACTS, id);
        let hlc = self.next_hlc();
        loop {
            let value = serde_json::to_value(&comments)
                .map_err(|e| DocError::Schema(format!("comments: {e}")))?;
            let op = RowOp {
                kind: KIND_ARTIFACTS.to_owned(),
                id: id.to_owned(),
                op: OpKind::Update,
                set: Some(BTreeMap::from([
                    ("comments".to_owned(), value),
                    ("updatedAt".to_owned(), Value::from(now_ms)),
                ])),
                hlc: hlc.clone(),
                clocks: None,
            };
            match check_op_size(&op) {
                Ok(()) => {
                    self.write_ops(vec![op])?;
                    return Ok(Some(comments));
                }
                Err(error) if comments.len() <= 1 => return Err(error),
                Err(_) => {
                    let at = comments.iter().position(|c| !c.is_open()).unwrap_or(0);
                    comments.remove(at);
                }
            }
        }
    }

    /// Record (or, with `None`, clear) the artifact's picked option. Writes
    /// the `picked` field alone. `None` when the artifact is not live.
    pub fn set_artifact_pick(
        &mut self,
        id: &str,
        pick: Option<&mast_proto::artifacts::ArtifactOptionPick>,
        now_ms: i64,
    ) -> Result<Option<()>, DocError> {
        if self.artifact(id).is_none() {
            return Ok(None);
        }
        let value = match pick {
            Some(pick) => {
                serde_json::to_value(pick).map_err(|e| DocError::Schema(format!("pick: {e}")))?
            }
            None => Value::Null,
        };
        self.observe_row(KIND_ARTIFACTS, id);
        let hlc = self.next_hlc();
        let op = RowOp {
            kind: KIND_ARTIFACTS.to_owned(),
            id: id.to_owned(),
            op: OpKind::Update,
            set: Some(BTreeMap::from([
                ("picked".to_owned(), value),
                ("updatedAt".to_owned(), Value::from(now_ms)),
            ])),
            hlc,
            clocks: None,
        };
        check_op_size(&op)?;
        self.write_ops(vec![op])?;
        Ok(Some(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_proto::artifacts::{ArtifactKind, ArtifactRevision};

    fn view(id: &str, item: &str, revisions: u32) -> ArtifactView {
        ArtifactView {
            id: id.into(),
            space_id: "space-1".into(),
            device_id: "dev-1".into(),
            work_item_id: Some(item.into()),
            chat_id: Some("chat-1".into()),
            title: format!("Artifact {id}"),
            kind: ArtifactKind::Markdown,
            role: Some("plan".into()),
            folder: "f".into(),
            stored_path: "/repo/.zeron/artifacts/f/plan.md".into(),
            current_revision: revisions,
            revisions: (1..=revisions)
                .map(|n| ArtifactRevision {
                    id: n,
                    hash: format!("{n:064x}"),
                    note: "n".repeat(200),
                    at: n as i64,
                    size_bytes: 10,
                    ..Default::default()
                })
                .collect(),
            created_at: 1,
            updated_at: 1,
            ..Default::default()
        }
    }

    #[test]
    fn put_creates_then_updates_and_trims_to_the_op_budget() {
        let mut doc = RegistryDoc::new("dev-1");
        let written = doc.put_artifact(&view("a1", "item-1", 3)).unwrap();
        assert_eq!(written.revisions[0].id, 3, "newest first");
        assert_eq!(doc.artifact("a1").unwrap().current_revision, 3);

        // 50 revisions with 200-char notes do not fit 15k units: trimmed.
        let mut big = view("a1", "item-1", 80);
        big.origin_path = None;
        let written = doc.put_artifact(&big).unwrap();
        assert!(written.revisions.len() < artifact_limits::MAX_REVISIONS);
        assert_eq!(written.revisions[0].id, 80);
        let stored = doc.artifact("a1").unwrap();
        assert_eq!(stored.revisions.len(), written.revisions.len());
        assert_eq!(stored.created_at, 1);
    }

    #[test]
    fn updates_clear_optional_fields_and_keep_immutable_ones() {
        let mut doc = RegistryDoc::new("dev-1");
        let mut first = view("a1", "item-1", 1);
        first.promoted_path = Some("/repo/docs/plans/plan.md".into());
        doc.put_artifact(&first).unwrap();
        let mut second = view("a1", "item-1", 2);
        second.work_item_id = Some("other".into());
        second.promoted_path = None;
        doc.put_artifact(&second).unwrap();
        let stored = doc.artifact("a1").unwrap();
        assert_eq!(stored.promoted_path, None);
        assert_eq!(stored.work_item_id.as_deref(), Some("item-1"));
    }

    #[test]
    fn pin_unpins_the_scope_s_other_plans_in_one_batch() {
        let mut doc = RegistryDoc::new("dev-1");
        for (id, item) in [("a1", "item-1"), ("a2", "item-1"), ("a3", "item-2")] {
            doc.put_artifact(&view(id, item, 1)).unwrap();
        }
        assert_eq!(doc.set_artifact_pin("a1", true, 5).unwrap(), Some(vec![]));
        doc.set_artifact_pin("a3", true, 5).unwrap();
        let before = doc.batches_written();
        let unpinned = doc.set_artifact_pin("a2", true, 6).unwrap().unwrap();
        assert_eq!(unpinned, vec!["a1".to_owned()]);
        assert_eq!(doc.batches_written(), before + 1);
        assert!(!doc.artifact("a1").unwrap().pinned_as_plan);
        assert!(doc.artifact("a2").unwrap().pinned_as_plan);
        assert!(
            doc.artifact("a3").unwrap().pinned_as_plan,
            "other item untouched"
        );
        assert_eq!(doc.set_artifact_pin("missing", true, 7).unwrap(), None);
    }

    #[test]
    fn tombstoned_artifacts_are_not_revived() {
        let mut doc = RegistryDoc::new("dev-1");
        doc.put_artifact(&view("a1", "item-1", 1)).unwrap();
        doc.delete_row_ops(&[(KIND_ARTIFACTS, "a1")]);
        assert!(doc.put_artifact(&view("a1", "item-1", 2)).is_err());
        assert!(doc.artifact("a1").is_none());
    }

    #[test]
    fn set_artifact_comments_roundtrips_and_trims_resolved_first() {
        use mast_proto::artifacts::{ArtifactComment, CommentAnchor, CommentStatus};
        let mut doc = RegistryDoc::new("dev-1");
        doc.put_artifact(&view("a1", "item-1", 1)).unwrap();
        let mk = |n: u32, status: CommentStatus| ArtifactComment {
            id: format!("C-{n}"),
            anchor: CommentAnchor {
                id: Some("intake".into()),
                ..Default::default()
            },
            text: "x".repeat(600),
            quote: "q".repeat(240),
            status,
            ..Default::default()
        };
        let one = vec![mk(1, CommentStatus::Open)];
        doc.set_artifact_comments("a1", &one, 5).unwrap().unwrap();
        assert_eq!(doc.artifact("a1").unwrap().comments, one);
        assert_eq!(doc.artifact("a1").unwrap().current_revision, 1);
        assert!(
            doc.set_artifact_comments("nope", &one, 5)
                .unwrap()
                .is_none()
        );

        let mut many = vec![mk(1, CommentStatus::Open), mk(2, CommentStatus::Resolved)];
        many.extend((3..=40).map(|n| mk(n, CommentStatus::Open)));
        let kept = doc.set_artifact_comments("a1", &many, 6).unwrap().unwrap();
        assert!(
            kept.len() < many.len(),
            "40 maximal comments do not fit one op"
        );
        assert!(kept.iter().all(|c| c.id != "C-2"), "resolved dropped first");
        assert_eq!(kept.last().unwrap().id, "C-40", "newest kept");
    }
}
