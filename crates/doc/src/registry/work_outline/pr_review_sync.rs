//! PR review sync rows and operations (contract:
//! `docs/plans/work-outline/pr-sync-contract.md`).
//!
//! - `workOutlineSpaceSettings.prReviewSyncWorkItemId` (LWW string or null):
//!   the Space's sync destination. Written by `setPrReviewSyncDestination`
//!   (create-if-absent upsert of the settings row, else an update).
//! - `workItems.externalSyncState` (LWW JSON object): what the sync last
//!   synced on an item. Only the system actor writes it (through
//!   `updateWorkItem.changes.externalSyncState`); a user or agent change
//!   carrying it is `unauthorized`.
//! - System-origin user action requests (`originKind: "system"`, empty
//!   `chatId`) are in `user_actions`.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use zeron_proto::pr_review_sync::WorkItemExternalSyncState;
use zeron_proto::work_outline::{SetPrReviewSyncDestinationParams, WorkOutlineErrorCode};

use super::linear::{KIND_WORK_OUTLINE_SPACE_SETTINGS, decode_space_settings};
use super::materialize::str_field;
use super::{
    KIND_SPACES, KIND_WORK_ITEMS, Outcome, Planned, WORK_OUTLINE_ROW_VERSION, check_id,
    check_operation_id, err, invalid, not_found,
};
use crate::registry::{RegistryDoc, RegistryRow, RowState};

/// The `workItems` field holding the sync record.
pub const SYNC_STATE_FIELD: &str = "externalSyncState";
/// The settings field holding the destination.
pub const DESTINATION_FIELD: &str = "prReviewSyncWorkItemId";

/// The sync record of an item row (`None` when absent or not an object).
pub(crate) fn decode_sync_state(row: &RegistryRow) -> Option<WorkItemExternalSyncState> {
    row.fields
        .get(SYNC_STATE_FIELD)
        .filter(|value| value.is_object())
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

/// The destination recorded on a settings row (`None` when unset/empty).
pub(crate) fn decode_destination(row: Option<&RegistryRow>) -> Option<String> {
    row.and_then(|row| row.fields.get(DESTINATION_FIELD))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

impl RegistryDoc {
    /// The Space's PR review sync destination, when set.
    pub fn pr_review_sync_destination(&self, space_id: &str) -> Option<String> {
        decode_space_settings(
            self.overlay_row(KIND_WORK_OUTLINE_SPACE_SETTINGS, space_id)
                .as_ref(),
        )
        .pr_review_sync_work_item_id
    }

    /// Live Spaces with a PR review sync destination (sorted).
    pub fn pr_review_sync_space_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .overlay_rows(KIND_WORK_OUTLINE_SPACE_SETTINGS)
            .into_iter()
            .filter(|row| decode_destination(Some(row)).is_some())
            .filter_map(|row| str_field(&row, "spaceId").or(Some(row.id)))
            .filter(|space| self.row_state(KIND_SPACES, space) == RowState::Live)
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// `setPrReviewSyncDestination`: LWW write of the Space's settings row
    /// (no write when the value already holds). The item must be live, not
    /// archived, and in the Space.
    pub fn set_pr_review_sync_destination(
        &mut self,
        p: &SetPrReviewSyncDestinationParams,
        now_ms: i64,
    ) -> Outcome<()> {
        check_operation_id(&p.operation_id)?;
        self.require_space(&p.space_id)?;
        let destination = p.work_item_id.as_deref().filter(|id| !id.is_empty());
        if let Some(id) = destination {
            check_id("work item", id)?;
            let item = self.live_item(id)?;
            if item.space_id != p.space_id {
                return Err(err(
                    WorkOutlineErrorCode::CrossSpace,
                    "The destination work item belongs to another Space",
                ));
            }
            if item.archived() {
                return Err(err(
                    WorkOutlineErrorCode::Archived,
                    "The destination work item is archived",
                ));
            }
        }
        let value = destination.map_or(Value::Null, |id| json!(id));
        let planned = match self.row_state(KIND_WORK_OUTLINE_SPACE_SETTINGS, &p.space_id) {
            RowState::Live => {
                if self.pr_review_sync_destination(&p.space_id).as_deref() == destination {
                    return Ok(());
                }
                Planned::update(
                    KIND_WORK_OUTLINE_SPACE_SETTINGS,
                    &p.space_id,
                    BTreeMap::from([
                        (DESTINATION_FIELD.to_owned(), value),
                        ("updatedAt".to_owned(), json!(now_ms)),
                    ]),
                )
            }
            RowState::Absent => {
                if destination.is_none() {
                    return Ok(());
                }
                Planned::upsert(
                    KIND_WORK_OUTLINE_SPACE_SETTINGS,
                    &p.space_id,
                    BTreeMap::from([
                        ("spaceId".to_owned(), json!(p.space_id)),
                        (DESTINATION_FIELD.to_owned(), value),
                        ("updatedAt".to_owned(), json!(now_ms)),
                        ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
                    ]),
                )
            }
            RowState::Tombstoned => return Err(not_found("This Space's settings were deleted")),
        };
        let mut extra: Vec<(&str, &str)> = vec![(KIND_SPACES, p.space_id.as_str())];
        if let Some(id) = destination {
            extra.push((KIND_WORK_ITEMS, id));
        }
        self.commit(vec![planned], &extra)
    }
}

/// Validate a sync record before it is written.
pub(crate) fn check_sync_state(state: &WorkItemExternalSyncState) -> Outcome<Value> {
    let value = serde_json::to_value(state).map_err(|e| invalid(e.to_string()))?;
    if value.to_string().len() > 2_048 {
        return Err(invalid("The sync record is too large"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests;
