//! Project templates (contract amendment "project templates"): registry kind
//! `workItemTemplates`, the frozen copy of a project subtree.
//!
//! Row (id = `derive_id(operationId, "template")`): `spaceId`, `name`,
//! `revision` (u32, from 1), `body` (a [`TemplateNode`] object), `createdAt`,
//! `updatedAt`, `lastOperationId` (re-save replay guard), `v`. Projects point
//! at a template with `workItems.templateId` / `templateRevision`.

use std::collections::BTreeMap;

use mast_proto::work_outline::{WorkOutlineErrorCode, WorkOutlineRepair, derive_id, roles};
use mast_proto::work_outline_templates::{
    DeleteWorkItemTemplateParams, SaveWorkItemTemplateParams, TemplateNode, WorkItemTemplateLink,
    WorkItemTemplateView, freeze_project_subtree, template_limits,
};
use serde_json::{Value, json};

use super::materialize::{i64_field, space_of, str_field};
use super::{
    KIND_WORK_ITEMS, Outcome, Planned, WORK_OUTLINE_ROW_VERSION, check_id, check_operation_id, err,
    invalid, not_found,
};
use crate::registry::{RegistryDoc, RegistryRow, RowState};

pub const KIND_WORK_ITEM_TEMPLATES: &str = "workItemTemplates";

/// Decode one template row; `Err` when `spaceId` or the body is unusable.
pub(crate) fn decode_template(
    row: &RegistryRow,
) -> Result<WorkItemTemplateView, WorkOutlineRepair> {
    let malformed = || WorkOutlineRepair::Malformed {
        row_kind: KIND_WORK_ITEM_TEMPLATES.into(),
        id: row.id.clone(),
    };
    let space_id = space_of(row, KIND_WORK_ITEM_TEMPLATES)?;
    let body: TemplateNode = row
        .fields
        .get("body")
        .filter(|v| v.is_object())
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .ok_or_else(malformed)?;
    let created_at = i64_field(row, "createdAt").unwrap_or(0);
    Ok(WorkItemTemplateView {
        id: row.id.clone(),
        space_id,
        name: str_field(row, "name").unwrap_or_default(),
        revision: row
            .fields
            .get("revision")
            .and_then(Value::as_u64)
            .and_then(|r| u32::try_from(r).ok())
            .unwrap_or(1),
        body,
        created_at,
        updated_at: i64_field(row, "updatedAt").unwrap_or(created_at),
    })
}

/// Decoded templates of `space_id` from raw rows, sorted by lowercase name
/// then id; undecodable rows are reported as `malformed` repairs.
pub(crate) fn decode_templates(
    space_id: &str,
    rows: &[RegistryRow],
    repairs: &mut std::collections::BTreeSet<WorkOutlineRepair>,
) -> Vec<WorkItemTemplateView> {
    let mut out: Vec<WorkItemTemplateView> = Vec::new();
    for row in rows {
        match decode_template(row) {
            Ok(template) if template.space_id == space_id => out.push(template),
            Ok(_) => {}
            Err(repair) => {
                if row.fields.get("spaceId").and_then(Value::as_str) == Some(space_id) {
                    repairs.insert(repair);
                }
            }
        }
    }
    out.sort_by(|a, b| (a.name.to_lowercase(), &a.id).cmp(&(b.name.to_lowercase(), &b.id)));
    out
}

fn check_template_name(name: &str) -> Outcome<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(invalid("A template needs a name"));
    }
    if name.chars().count() > template_limits::MAX_TEMPLATE_NAME_CHARS {
        return Err(err(
            WorkOutlineErrorCode::LimitExceeded,
            format!(
                "Template names are limited to {} characters",
                template_limits::MAX_TEMPLATE_NAME_CHARS
            ),
        ));
    }
    Ok(name.to_owned())
}

impl RegistryDoc {
    /// Live, decodable templates of a Space.
    pub fn templates_of_space(&self, space_id: &str) -> Vec<WorkItemTemplateView> {
        let rows = self.overlay_rows(KIND_WORK_ITEM_TEMPLATES);
        decode_templates(space_id, &rows, &mut Default::default())
    }

    /// One live template (any Space).
    fn live_template(&self, id: &str) -> Outcome<WorkItemTemplateView> {
        check_id("template", id)?;
        let row = self
            .overlay_row(KIND_WORK_ITEM_TEMPLATES, id)
            .ok_or_else(|| not_found("Template not found"))?;
        decode_template(&row).map_err(|_| invalid("Template row is malformed"))
    }

    /// Validate a `changes.template` link of an item in `space_id`: the
    /// template is live in the same Space and the revision is not newer than
    /// the template's.
    pub(crate) fn check_template_link(
        &self,
        space_id: &str,
        link: &WorkItemTemplateLink,
    ) -> Outcome<()> {
        let template = self.live_template(&link.template_id)?;
        if template.space_id != space_id {
            return Err(err(
                WorkOutlineErrorCode::CrossSpace,
                "The template belongs to another Space",
            ));
        }
        if link.revision > template.revision {
            return Err(invalid("The template revision is newer than the template"));
        }
        Ok(())
    }

    /// `saveWorkItemTemplate`: freeze the live project subtree into a template
    /// row and link the project to it, in one batch. Returns the template id.
    pub fn save_work_item_template(
        &mut self,
        p: &SaveWorkItemTemplateParams,
        now_ms: i64,
    ) -> Outcome<String> {
        check_operation_id(&p.operation_id)?;
        self.require_space(&p.space_id)?;
        check_id("work item", &p.project_work_item_id)?;
        let name = check_template_name(&p.name)?;
        let id = match &p.template_id {
            Some(id) => id.clone(),
            None => derive_id(&p.operation_id, roles::TEMPLATE),
        };
        check_id("template", &id)?;
        let existing = match self.row_state(KIND_WORK_ITEM_TEMPLATES, &id) {
            RowState::Live => Some(self.live_template(&id)?),
            RowState::Absent if p.template_id.is_none() => None,
            RowState::Absent => return Err(not_found("Template not found")),
            RowState::Tombstoned => return Err(not_found("Template was deleted")),
        };
        if let Some(existing) = &existing {
            if existing.space_id != p.space_id {
                return Err(err(
                    WorkOutlineErrorCode::CrossSpace,
                    "The template belongs to another Space",
                ));
            }
            // A replayed operation never bumps the revision twice.
            let last = self
                .overlay_row(KIND_WORK_ITEM_TEMPLATES, &id)
                .and_then(|row| str_field(&row, "lastOperationId"));
            if last.as_deref() == Some(p.operation_id.as_str()) {
                return Ok(id);
            }
        }
        let project = self.live_item(&p.project_work_item_id)?;
        if project.space_id != p.space_id {
            return Err(err(
                WorkOutlineErrorCode::CrossSpace,
                "The project belongs to another Space",
            ));
        }
        if project.archived() {
            return Err(err(
                WorkOutlineErrorCode::Archived,
                "The project is archived",
            ));
        }
        if !project.is_project {
            return Err(invalid("Only a project can be saved as a template"));
        }
        let lowered = name.to_lowercase();
        if self
            .templates_of_space(&p.space_id)
            .iter()
            .any(|t| t.id != id && t.name.to_lowercase() == lowered)
        {
            return Err(invalid(format!(
                "A template named \"{name}\" already exists in this Space"
            )));
        }
        let snapshot = self.work_outline_snapshot(&p.space_id, true);
        let body = freeze_project_subtree(&snapshot, &p.project_work_item_id)
            .ok_or_else(|| not_found("Work item not found"))?;
        body.check_limits()
            .map_err(|message| err(WorkOutlineErrorCode::LimitExceeded, message))?;
        let body_value = serde_json::to_value(&body).map_err(|e| invalid(e.to_string()))?;
        if body_value.to_string().encode_utf16().count()
            > template_limits::MAX_TEMPLATE_BODY_UTF16_UNITS
        {
            return Err(err(
                WorkOutlineErrorCode::LimitExceeded,
                "The project is too large to save as a template",
            ));
        }
        let revision = existing
            .as_ref()
            .map_or(1, |t| t.revision.saturating_add(1));
        let mut set = BTreeMap::from([
            ("name".to_owned(), json!(name)),
            ("revision".to_owned(), json!(revision)),
            ("body".to_owned(), body_value),
            ("updatedAt".to_owned(), json!(now_ms)),
            ("lastOperationId".to_owned(), json!(p.operation_id)),
        ]);
        let template_row = if existing.is_some() {
            Planned::update(KIND_WORK_ITEM_TEMPLATES, &id, set)
        } else {
            set.extend([
                ("spaceId".to_owned(), json!(p.space_id)),
                ("createdAt".to_owned(), json!(now_ms)),
                ("v".to_owned(), json!(WORK_OUTLINE_ROW_VERSION)),
            ]);
            Planned::upsert(KIND_WORK_ITEM_TEMPLATES, &id, set)
        };
        let link = Planned::update(
            KIND_WORK_ITEMS,
            &p.project_work_item_id,
            BTreeMap::from([
                ("templateId".to_owned(), json!(id)),
                ("templateRevision".to_owned(), json!(revision)),
                ("updatedAt".to_owned(), json!(now_ms)),
            ]),
        );
        self.commit(vec![template_row, link], &[])?;
        Ok(id)
    }

    /// `deleteWorkItemTemplate`: tombstone the row (idempotent). Projects keep
    /// their link; it then reads as dangling.
    pub fn delete_work_item_template(&mut self, p: &DeleteWorkItemTemplateParams) -> Outcome<()> {
        check_operation_id(&p.operation_id)?;
        check_id("template", &p.template_id)?;
        if self.row_state(KIND_WORK_ITEM_TEMPLATES, &p.template_id) != RowState::Live {
            return Ok(());
        }
        self.commit(
            vec![Planned::delete(KIND_WORK_ITEM_TEMPLATES, &p.template_id)],
            &[],
        )
    }
}

#[cfg(test)]
mod tests;
