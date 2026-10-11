//! Work Outline execution rows (Milestone 3): `executionConfigurations`
//! (id `s:{spaceId}` / `w:{workItemId}`, registry audit 7.4) and
//! `chatExecutionBindings` (id = Chat id, contract 6.2).
//!
//! Mechanism layer, same write rules as the Milestone 1 and 2 kinds:
//! - `configuration` is ONE JSON field (one LWW register), so a root never
//!   pairs with another writer's bindings; it is shape-checked
//!   ([`check_execution_configuration`]) and size-gated
//!   (`MAX_CONFIGURATION_UTF16_UNITS`) before anything is enqueued;
//! - configuring is compare-before-write (an equal configuration writes
//!   nothing); creation is one `upsert` with the immutable `spaceId` /
//!   `workItemId`; later edits are `update`s without them; a reset tombstones
//!   the row. Re-configuring a reset scope is an explicit user write and
//!   revives the row (the scope id is the only possible key, like
//!   assignment revival in Milestone 1);
//! - a binding is written ONCE (create-if-absent; a tombstoned binding is
//!   never revived) by the launch workflow; a replay with the same operation
//!   id and inputs is a no-op, anything else is `operationConflict`;
//! - every touched row (and the scope's work item) is observed into the
//!   local HLC before a clock is minted (R3).
//!
//! Nothing here touches the filesystem: paths may live on another device.
//! Validation is the engine's (`crates/engine/src/execution_scope.rs`).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use zeron_proto::work_outline::{
    WorkItemView, WorkOutlineActor, WorkOutlineError, WorkOutlineErrorCode, WorkOutlineRepair,
};
use zeron_proto::work_outline_execution::{
    ChatExecutionBindingView, ConfigureExecutionParams, ExecutionConfiguration,
    ExecutionConfigurationView, ExecutionScope, ExecutionSpace, ResolvedExecutionConfiguration,
    WorkItemExecutionException, check_execution_configuration, execution_limits,
    resolve_execution_configuration,
};

use super::materialize::resolver_views;
use super::*;

pub const KIND_EXECUTION_CONFIGURATIONS: &str = "executionConfigurations";
pub const KIND_CHAT_EXECUTION_BINDINGS: &str = "chatExecutionBindings";

fn limit(message: impl Into<String>) -> WorkOutlineError {
    err(WorkOutlineErrorCode::LimitExceeded, message)
}

fn utf16_units(value: &Value) -> usize {
    serde_json::to_string(value).map_or(usize::MAX, |s| s.encode_utf16().count())
}

fn opt_str(row: &RegistryRow, key: &str) -> Option<String> {
    row.fields
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn opt_actor(row: &RegistryRow, key: &str) -> Option<WorkOutlineActor> {
    let value = row.fields.get(key).filter(|v| v.is_object())?;
    let kind = match value.get("k").and_then(Value::as_str) {
        Some("agent") => zeron_proto::WorkOutlineActorKind::Agent,
        Some("system") => zeron_proto::WorkOutlineActorKind::System,
        _ => zeron_proto::WorkOutlineActorKind::User,
    };
    Some(WorkOutlineActor {
        kind,
        device_id: value
            .get("deviceId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        chat_id: value.get("chatId").and_then(Value::as_str).map(Into::into),
        message_id: value.get("msgId").and_then(Value::as_str).map(Into::into),
    })
}

/// Decode one `executionConfigurations` row leniently (R7). Only a missing
/// `spaceId` or a row id that is not a scope drops the row; an undecodable
/// or ill-shaped `configuration` keeps the row with `invalidReason` (the
/// reader fails closed on it instead of falling through to an ancestor).
pub(crate) fn decode_execution_configuration(
    row: &RegistryRow,
) -> Result<ExecutionConfigurationView, WorkOutlineRepair> {
    let malformed = || WorkOutlineRepair::Malformed {
        row_kind: KIND_EXECUTION_CONFIGURATIONS.into(),
        id: row.id.clone(),
    };
    let Some(space_id) = opt_str(row, "spaceId") else {
        return Err(WorkOutlineRepair::MissingSpace {
            row_kind: KIND_EXECUTION_CONFIGURATIONS.into(),
            id: row.id.clone(),
        });
    };
    let scope = ExecutionScope::from_row_id(&row.id).ok_or_else(malformed)?;
    let work_item_id = match &scope {
        ExecutionScope::Space { space_id: scoped } if *scoped == space_id => None,
        ExecutionScope::Space { .. } => return Err(malformed()),
        ExecutionScope::WorkItem { work_item_id } => Some(work_item_id.clone()),
    };
    let (configuration, invalid_reason) = match row
        .fields
        .get("configuration")
        .cloned()
        .map(serde_json::from_value::<ExecutionConfiguration>)
    {
        Some(Ok(configuration)) => {
            let reason = check_execution_configuration(&configuration).err();
            (Some(configuration), reason)
        }
        Some(Err(e)) => (
            None,
            Some(format!("the stored configuration is malformed ({e})")),
        ),
        None => (None, Some("the configuration is missing".to_owned())),
    };
    Ok(ExecutionConfigurationView {
        id: row.id.clone(),
        space_id,
        work_item_id,
        configuration,
        invalid_reason,
        updated_by: opt_actor(row, "updatedBy"),
        updated_at: row
            .fields
            .get("updatedAt")
            .and_then(Value::as_i64)
            .unwrap_or(0),
    })
}

/// Decode one `chatExecutionBindings` row (exception flag and item are left
/// for the materializer).
pub(crate) fn decode_chat_execution_binding(
    row: &RegistryRow,
) -> Result<ChatExecutionBindingView, WorkOutlineRepair> {
    let Some(space_id) = opt_str(row, "spaceId") else {
        return Err(WorkOutlineRepair::MissingSpace {
            row_kind: KIND_CHAT_EXECUTION_BINDINGS.into(),
            id: row.id.clone(),
        });
    };
    let resolved = row
        .fields
        .get("resolved")
        .cloned()
        .and_then(|value| serde_json::from_value::<ResolvedExecutionConfiguration>(value).ok())
        .ok_or_else(|| WorkOutlineRepair::Malformed {
            row_kind: KIND_CHAT_EXECUTION_BINDINGS.into(),
            id: row.id.clone(),
        })?;
    Ok(ChatExecutionBindingView {
        chat_id: row.id.clone(),
        space_id,
        resolved,
        captured_at: row
            .fields
            .get("capturedAt")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        captured_by: opt_actor(row, "capturedBy"),
        operation_id: opt_str(row, "operationId"),
        work_item_id: None,
        exception: false,
    })
}

/// The execution part of a snapshot.
pub(crate) struct MaterializedExecution {
    pub space: Option<ExecutionSpace>,
    pub configurations: Vec<ExecutionConfigurationView>,
    pub bindings: Vec<ChatExecutionBindingView>,
    pub exceptions: Vec<WorkItemExecutionException>,
}

/// Configurations and bindings of `space_id`, with the exception rule: a
/// Chat associated with item X whose captured configuration differs from X's
/// current resolved configuration (content comparison; a changed source
/// with equal content is not an exception). Unassociated Chats and Chats
/// without a binding are never exceptions.
#[allow(clippy::too_many_arguments)] // one pure step of the materializer
pub(crate) fn materialize_execution(
    space_id: &str,
    space: Option<&ExecutionSpace>,
    configuration_rows: &[RegistryRow],
    binding_rows: &[RegistryRow],
    views: &[WorkItemView],
    associated_item: &BTreeMap<&str, &str>,
    chat_spaces: &BTreeMap<&str, Option<&str>>,
    repairs: &mut BTreeSet<WorkOutlineRepair>,
) -> MaterializedExecution {
    let item_ids: BTreeSet<&str> = views.iter().map(|v| v.id.as_str()).collect();
    let mut configurations = Vec::new();
    for row in configuration_rows {
        match decode_execution_configuration(row) {
            Ok(view) if view.space_id == space_id => configurations.push(view),
            Ok(_) => {}
            Err(repair) => {
                // Attributable only when the row's scope is visibly ours.
                let ours = match ExecutionScope::from_row_id(&row.id) {
                    Some(ExecutionScope::Space { space_id: s }) => s == space_id,
                    Some(ExecutionScope::WorkItem { work_item_id }) => {
                        item_ids.contains(work_item_id.as_str())
                    }
                    None => false,
                };
                if ours {
                    repairs.insert(repair);
                }
            }
        }
    }
    configurations.sort_by(|a, b| a.id.cmp(&b.id));

    let mut bindings = Vec::new();
    let mut exceptions: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in binding_rows {
        let chat_space = chat_spaces.get(row.id.as_str()).copied().flatten();
        let mut view = match decode_chat_execution_binding(row) {
            Ok(view) => view,
            Err(repair) => {
                if chat_space == Some(space_id) {
                    repairs.insert(repair);
                }
                continue;
            }
        };
        if view.space_id != space_id {
            continue;
        }
        if let Some(item) = associated_item.get(view.chat_id.as_str()) {
            view.work_item_id = Some((*item).to_owned());
            let current = space.and_then(|space| {
                resolve_execution_configuration(
                    &ExecutionScope::WorkItem {
                        work_item_id: (*item).to_owned(),
                    },
                    space,
                    views,
                    &configurations,
                )
            });
            view.exception = current.is_some_and(|current| {
                current.configuration != view.resolved.configuration
                    || current.invalid_reason.is_some()
            });
            if view.exception {
                exceptions
                    .entry((*item).to_owned())
                    .or_default()
                    .push(view.chat_id.clone());
            }
        }
        bindings.push(view);
    }
    bindings.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
    MaterializedExecution {
        space: space.cloned(),
        configurations,
        bindings,
        exceptions: exceptions
            .into_iter()
            .map(|(work_item_id, mut chat_ids)| {
                chat_ids.sort();
                WorkItemExecutionException {
                    work_item_id,
                    chat_ids,
                }
            })
            .collect(),
    }
}

/// `inputsHash` of a binding capture: the Chat, the snapshot, and the
/// operation id.
fn binding_inputs_hash(
    chat_id: &str,
    resolved: &ResolvedExecutionConfiguration,
    operation_id: &str,
) -> String {
    inputs_hash(&json!({
        "chatId": chat_id,
        "resolved": resolved,
        "operationId": operation_id,
    }))
}

impl RegistryDoc {
    // ── reads ───────────────────────────────────────────────────────────────

    /// The Space facts behind the implicit execution root.
    pub fn execution_space(&self, space_id: &str) -> Option<ExecutionSpace> {
        let space = self.space(space_id).ok().flatten()?;
        Some(ExecutionSpace {
            space_id: space.id,
            path: space.path,
            device_id: space.device_id,
        })
    }

    /// Execution rows the materializer needs (configurations of `space_id`,
    /// bindings of `space_id`) plus the Space facts.
    pub(super) fn execution_inputs(
        &self,
        space_id: &str,
    ) -> (Vec<RegistryRow>, Vec<RegistryRow>, Option<ExecutionSpace>) {
        let of_space = |kind: &str| -> Vec<RegistryRow> {
            self.overlay_rows(kind)
                .into_iter()
                .filter(|row| {
                    // Keep rows without a decodable space too: the reader
                    // attributes their repair markers.
                    row.fields
                        .get("spaceId")
                        .and_then(Value::as_str)
                        .is_none_or(|s| s == space_id)
                })
                .collect()
        };
        (
            of_space(KIND_EXECUTION_CONFIGURATIONS),
            of_space(KIND_CHAT_EXECUTION_BINDINGS),
            self.execution_space(space_id),
        )
    }

    /// Decoded configuration rows of one Space.
    pub fn execution_configurations_of(&self, space_id: &str) -> Vec<ExecutionConfigurationView> {
        let mut views: Vec<ExecutionConfigurationView> = self
            .overlay_rows(KIND_EXECUTION_CONFIGURATIONS)
            .iter()
            .filter_map(|row| decode_execution_configuration(row).ok())
            .filter(|view| view.space_id == space_id)
            .collect();
        views.sort_by(|a, b| a.id.cmp(&b.id));
        views
    }

    /// The Space a scope belongs to (`notFound` for an unknown item or a
    /// Space that is not live).
    fn scope_space(&self, scope: &ExecutionScope) -> Outcome<String> {
        let space_id = match scope {
            ExecutionScope::Space { space_id } => space_id.clone(),
            ExecutionScope::WorkItem { work_item_id } => {
                check_id("work item", work_item_id)?;
                self.live_item(work_item_id)?.space_id
            }
        };
        self.require_space(&space_id)?;
        Ok(space_id)
    }

    /// Whole-row resolution of `scope` over the repaired tree, exactly as
    /// the snapshot resolves it.
    pub fn resolve_execution(
        &self,
        scope: &ExecutionScope,
    ) -> Outcome<ResolvedExecutionConfiguration> {
        let space_id = self.scope_space(scope)?;
        let space = self
            .execution_space(&space_id)
            .ok_or_else(|| not_found("Space not found"))?;
        let views = resolver_views(&self.outline_tree(&space_id));
        let configurations = self.execution_configurations_of(&space_id);
        resolve_execution_configuration(scope, &space, &views, &configurations)
            .ok_or_else(|| not_found("Work item not found"))
    }

    /// Every decodable captured binding (one registry scan; diff-sync reads
    /// this once per reconcile pass).
    pub fn chat_execution_bindings(&self) -> Vec<ChatExecutionBindingView> {
        self.overlay_rows(KIND_CHAT_EXECUTION_BINDINGS)
            .iter()
            .filter_map(|row| decode_chat_execution_binding(row).ok())
            .collect()
    }

    /// The Chat's captured binding, if any.
    pub fn chat_execution_binding(&self, chat_id: &str) -> Option<ChatExecutionBindingView> {
        self.overlay_row(KIND_CHAT_EXECUTION_BINDINGS, chat_id)
            .and_then(|row| decode_chat_execution_binding(&row).ok())
    }

    // ── writes ──────────────────────────────────────────────────────────────

    /// `configureExecution`: write or reset the configuration of one scope.
    /// Shape-only (no filesystem). Compare-before-write; a reset of an absent
    /// row is a no-op.
    pub fn configure_execution(
        &mut self,
        p: &ConfigureExecutionParams,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<()> {
        check_operation_id(&p.operation_id)?;
        let space_id = self.scope_space(&p.scope)?;
        let mut extra: Vec<(&str, &str)> = Vec::new();
        if let ExecutionScope::WorkItem { work_item_id } = &p.scope {
            let item = self.live_item(work_item_id)?;
            if item.archived() && p.configuration.is_some() {
                return Err(err(
                    WorkOutlineErrorCode::Archived,
                    "The work item is archived",
                ));
            }
            extra.push((KIND_WORK_ITEMS, work_item_id.as_str()));
        }
        let id = p.scope.row_id();
        check_id("execution configuration", &id)?;
        let state = self.row_state(KIND_EXECUTION_CONFIGURATIONS, &id);
        let Some(configuration) = &p.configuration else {
            if state != RowState::Live {
                return Ok(());
            }
            return self.commit(
                vec![Planned::delete(KIND_EXECUTION_CONFIGURATIONS, &id)],
                &extra,
            );
        };
        check_execution_configuration(configuration).map_err(invalid)?;
        let value = serde_json::to_value(configuration).map_err(|e| invalid(e.to_string()))?;
        let units = utf16_units(&value);
        if units > execution_limits::MAX_CONFIGURATION_UTF16_UNITS {
            return Err(limit(format!(
                "The execution configuration is too large to save ({units} of {} units after \
                 encoding)",
                execution_limits::MAX_CONFIGURATION_UTF16_UNITS
            )));
        }
        let planned = match state {
            RowState::Live => {
                let current = self
                    .overlay_row(KIND_EXECUTION_CONFIGURATIONS, &id)
                    .and_then(|row| row.fields.get("configuration").cloned());
                if current.as_ref() == Some(&value) {
                    return Ok(());
                }
                Planned::update(
                    KIND_EXECUTION_CONFIGURATIONS,
                    &id,
                    BTreeMap::from([
                        ("configuration".to_owned(), value),
                        ("updatedBy".to_owned(), actor_row_value(actor)),
                        ("updatedAt".to_owned(), json!(now_ms)),
                    ]),
                )
            }
            RowState::Absent | RowState::Tombstoned => {
                let mut set = BTreeMap::from([
                    ("spaceId".to_owned(), json!(space_id)),
                    ("configuration".to_owned(), value),
                    ("updatedBy".to_owned(), actor_row_value(actor)),
                    ("updatedAt".to_owned(), json!(now_ms)),
                    ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
                ]);
                if let ExecutionScope::WorkItem { work_item_id } = &p.scope {
                    set.insert("workItemId".to_owned(), json!(work_item_id));
                }
                Planned::upsert(KIND_EXECUTION_CONFIGURATIONS, &id, set)
            }
        };
        self.commit(vec![planned], &extra)
    }

    /// Capture the configuration a Chat launches with (contract 6.2): one
    /// create-if-absent upsert keyed by the Chat id. Returns whether a write
    /// was enqueued (`false` for an identical replay). Fails with
    /// `operationConflict` when the Chat already has a binding from another
    /// operation (or other inputs), `notFound` for a missing Chat or a
    /// deleted binding (never revived), `crossSpace` when the snapshot's
    /// Space is not the Chat's, and `projectless` for a Chat without a Space.
    pub fn capture_chat_execution_binding(
        &mut self,
        chat_id: &str,
        resolved: &ResolvedExecutionConfiguration,
        operation_id: &str,
        actor: &WorkOutlineActor,
        now_ms: i64,
    ) -> Outcome<bool> {
        check_operation_id(operation_id)?;
        check_id("Chat", chat_id)?;
        let hash = binding_inputs_hash(chat_id, resolved, operation_id);
        match self.row_state(KIND_CHAT_EXECUTION_BINDINGS, chat_id) {
            RowState::Live => {
                let row = self.overlay_row(KIND_CHAT_EXECUTION_BINDINGS, chat_id);
                let stored = row
                    .as_ref()
                    .and_then(|row| row.fields.get("inputsHash").cloned());
                if stored == Some(json!(hash)) {
                    return Ok(false);
                }
                let same_operation = row
                    .as_ref()
                    .and_then(|row| opt_str(row, "operationId"))
                    .is_some_and(|stored| stored == operation_id);
                return Err(err(
                    WorkOutlineErrorCode::OperationConflict,
                    if same_operation {
                        "operationId was already used for a different execution binding"
                    } else {
                        "This Chat already has an execution binding"
                    },
                ));
            }
            RowState::Tombstoned => {
                return Err(not_found("This Chat's execution binding was deleted"));
            }
            RowState::Absent => {}
        }
        let chat = self
            .chat(chat_id)
            .ok()
            .flatten()
            .ok_or_else(|| not_found("Chat not found"))?;
        let Some(space_id) = chat.space_id.filter(|s| !s.is_empty()) else {
            return Err(err(
                WorkOutlineErrorCode::Projectless,
                "Chats outside a project have no execution configuration",
            ));
        };
        self.require_space(&space_id)?;
        if resolved.space_id != space_id {
            return Err(err(
                WorkOutlineErrorCode::CrossSpace,
                "The execution configuration belongs to another Space",
            ));
        }
        let value = serde_json::to_value(resolved).map_err(|e| invalid(e.to_string()))?;
        let units = utf16_units(&json!(resolved.configuration));
        if units > execution_limits::MAX_CONFIGURATION_UTF16_UNITS {
            return Err(limit(format!(
                "The execution configuration is too large to save ({units} of {} units after \
                 encoding)",
                execution_limits::MAX_CONFIGURATION_UTF16_UNITS
            )));
        }
        let set = BTreeMap::from([
            ("chatId".to_owned(), json!(chat_id)),
            ("spaceId".to_owned(), json!(space_id)),
            ("resolved".to_owned(), value),
            ("capturedAt".to_owned(), json!(now_ms)),
            ("capturedBy".to_owned(), actor_row_value(actor)),
            ("operationId".to_owned(), json!(operation_id)),
            ("inputsHash".to_owned(), json!(hash)),
            ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
        ]);
        self.commit(
            vec![Planned::upsert(KIND_CHAT_EXECUTION_BINDINGS, chat_id, set)],
            &[],
        )?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
