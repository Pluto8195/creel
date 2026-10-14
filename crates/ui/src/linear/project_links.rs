//! Project-linked work items (a `linear`/`project` reference on a live item)
//! as the natural home of their project's issues:
//!
//! - [`destination_item`]: where Create puts a node for an issue of a
//!   project some live item links (the most recently linked one wins).
//! - Matching by project id OR by the slug id at the end of the project URL
//!   (`…/project/<name>-<slugId>`): a link made by pasting a URL stores the
//!   slug instead of the id.
//! - [`repair_target`] / [`fix_project_link`]: rewrite such a slug link to
//!   the real project id once "My projects" knows it. There is no update
//!   mutation for references, so Fix link is `linkLinearProject` (new, real
//!   id) then `removeWorkItemExternalReference` (old), with operation ids
//!   derived from the old reference so a retry replays.
use gpui::{App, Entity, Task};
use zeron_proto::linear::{
    LINEAR_PROJECT_REFERENCE, LINEAR_PROVIDER, LinearIssueSummary, LinearProjectRef,
    LinearProjectSummary,
};
use zeron_proto::{
    LinkLinearProjectParams, RemoveWorkItemExternalReferenceParams, WorkItemExternalReferenceView,
    WorkItemView, WorkOutlineSnapshot,
};

use super::client::LinearUiError;
use super::project_short_id;
use crate::state::AppState;

/// Whether `reference` points at project `id` / `url`: the same id, or the
/// same slug id (the reference's external id or URL against `url`).
pub(crate) fn reference_matches(
    reference: &WorkItemExternalReferenceView,
    id: &str,
    url: &str,
) -> bool {
    if !id.is_empty() && reference.external_id == id {
        return true;
    }
    let Some(short) = project_short_id(url) else {
        return false;
    };
    project_short_id(&reference.external_id) == Some(short)
        || project_short_id(&reference.url) == Some(short)
}

/// The Linear project references of live items in `snapshot`.
pub(crate) fn project_references(
    snapshot: &WorkOutlineSnapshot,
) -> impl Iterator<Item = &WorkItemExternalReferenceView> {
    snapshot.external_references.iter().filter(|reference| {
        reference.provider == LINEAR_PROVIDER
            && reference.reference_type == LINEAR_PROJECT_REFERENCE
            && snapshot
                .item(&reference.work_item_id)
                .is_some_and(|item| item.archived_at.is_none())
    })
}

/// The live item that links `project` most recently, if any.
pub(crate) fn destination_item<'a>(
    snapshot: &'a WorkOutlineSnapshot,
    project: &LinearProjectRef,
) -> Option<&'a WorkItemView> {
    project_references(snapshot)
        .filter(|reference| reference_matches(reference, &project.id, &project.url))
        .max_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)))
        .and_then(|reference| snapshot.item(&reference.work_item_id))
}

/// Where a node for `issue` goes by default: its project's linked item.
pub(crate) fn issue_destination<'a>(
    snapshot: &'a WorkOutlineSnapshot,
    issue: &LinearIssueSummary,
) -> Option<&'a WorkItemView> {
    destination_item(snapshot, issue.project.as_ref()?)
}

/// The real project behind a slug-keyed project reference (a pasted URL),
/// from `projects` ("My projects"); `None` when the reference already uses
/// a project id or nothing matches.
pub(crate) fn repair_target<'a>(
    reference: &WorkItemExternalReferenceView,
    projects: &'a [LinearProjectSummary],
) -> Option<&'a LinearProjectSummary> {
    if reference.provider != LINEAR_PROVIDER
        || reference.reference_type != LINEAR_PROJECT_REFERENCE
        || projects
            .iter()
            .any(|project| project.id == reference.external_id)
    {
        return None;
    }
    projects
        .iter()
        .find(|project| reference_matches(reference, &project.id, &project.url))
}

/// The project id to ask Linear about for `reference`: the real id when
/// `projects` resolves a slug link, else the stored external id.
pub(crate) fn resolved_project_id(
    reference: &WorkItemExternalReferenceView,
    projects: Option<&[LinearProjectSummary]>,
) -> String {
    projects
        .and_then(|projects| repair_target(reference, projects))
        .map_or_else(
            || reference.external_id.clone(),
            |project| project.id.clone(),
        )
}

/// Fix link: link `project` (its real id) to the reference's item, then
/// remove the slug-keyed `reference`. One user action; retries replay.
pub(crate) fn fix_project_link(
    state: &Entity<AppState>,
    reference: &WorkItemExternalReferenceView,
    project: &LinearProjectSummary,
    cx: &mut App,
) -> Task<Result<(), LinearUiError>> {
    let actions = state.read(cx).work_outline_actions();
    let link = LinkLinearProjectParams {
        work_item_id: reference.work_item_id.clone(),
        project: project.to_ref(),
        operation_id: zeron_proto::work_outline::derive_id(&reference.id, "fix-link:create"),
    };
    let remove = RemoveWorkItemExternalReferenceParams {
        reference_id: reference.id.clone(),
        operation_id: zeron_proto::work_outline::derive_id(&reference.id, "fix-link:remove"),
    };
    cx.spawn(async move |_| {
        actions.link_linear_project(link).await?;
        actions
            .remove_external_reference(remove)
            .await
            .map_err(LinearUiError::from_outline)?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear::fixture;
    use crate::work_outline::store::fixture_ids::*;

    fn project_ref(
        id: &str,
        item: &str,
        external_id: &str,
        url: &str,
        at: i64,
    ) -> WorkItemExternalReferenceView {
        let mut reference = fixture::reference(id, "s1", item, "project", external_id, url);
        reference.created_at = at;
        reference
    }

    #[test]
    fn destination_is_the_most_recently_linked_live_item() {
        let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
        let launch = fixture::project("p-launch", "Launch");
        snapshot.external_references = vec![
            project_ref("r1", API, "p-launch", &launch.url, 1),
            project_ref("r2", UI, "p-launch", &launch.url, 5),
            project_ref("r3", RETIRED, "p-launch", &launch.url, 9),
        ];
        let found = destination_item(&snapshot, &launch.to_ref()).map(|item| item.id.as_str());
        assert_eq!(
            found,
            Some(UI),
            "newest live link; archived items don't count"
        );
        let other = fixture::project("p-other", "Other");
        assert!(destination_item(&snapshot, &other.to_ref()).is_none());
    }

    #[test]
    fn slug_links_match_and_resolve_to_the_real_id() {
        let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
        let url = "https://linear.app/acme/project/receiveai-ui-upgrades-9f8e7d6c5b4a";
        let slug = project_ref("r1", UI, "receiveai-ui-upgrades-9f8e7d6c5b4a", url, 1);
        snapshot.external_references = vec![slug.clone()];
        let project = LinearProjectSummary {
            url: format!("{url}/overview"),
            ..fixture::project("uuid-receive", "ReceiveAI UI upgrades")
        };
        let found = destination_item(&snapshot, &project.to_ref()).map(|item| item.id.as_str());
        assert_eq!(found, Some(UI));
        let projects = vec![project.clone()];
        assert_eq!(
            repair_target(&slug, &projects).map(|p| p.id.as_str()),
            Some("uuid-receive")
        );
        assert_eq!(resolved_project_id(&slug, Some(&projects)), "uuid-receive");
        let fixed = project_ref("r2", UI, "uuid-receive", &project.url, 2);
        assert!(
            repair_target(&fixed, &projects).is_none(),
            "already the real id"
        );
    }
}
