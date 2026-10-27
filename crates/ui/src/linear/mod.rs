//! Linear integration v1, client side (contract:
//! `docs/plans/work-outline/linear-contract.md`; reviewed UX:
//! `docs/plans/work-outline/parked-proposals.md` "Linear integration").
//!
//! - Linear is OFF per Space until the user turns it on in the Space-root
//!   details. A Space with Linear off shows no Linear UI and makes no Linear
//!   calls for its items; pasted Linear URLs stay plain links there (with a
//!   one-time "Enable Linear for this Space?" hint).
//! - Nothing in the Outline is created automatically: every node and link
//!   comes from an explicit action in the Linear panel, a drag, a paste, or
//!   a menu entry.
//! - The API key is entered in Settings → Linear, sent once with
//!   `LinearSetApiKey`, and never shown, stored, or logged here.
//!
//! Files: [`client`] (typed RPC facade + error mapping), [`store`] (status
//! and issue cache in `AppState`), [`settings_page`], [`masked_input`],
//! [`panel`] (the Overview right-slot panel), [`menu`] (context-menu
//! entries). Outline / details / Overview / Shell hooks live in
//! `work_outline/linear_outline.rs`, `work_outline/details_linear.rs`,
//! `overview/linear_host.rs`, and `shell/linear_ui.rs`.

pub(crate) mod client;
pub(crate) mod masked_input;
pub(crate) mod menu;
pub(crate) mod panel;
pub(crate) mod prefs;
pub(crate) mod project_links;
pub(crate) mod project_picker;
pub(crate) mod settings_page;
pub(crate) mod store;

use gpui::Hsla;
use zeron_proto::WorkOutlineSnapshot;
use zeron_proto::linear::{LINEAR_ISSUE_REFERENCE, LINEAR_PROJECT_REFERENCE, LINEAR_PROVIDER};

#[allow(unused_imports)]
pub(crate) use client::{LinearFailure, LinearUiError, ListRead};

/// What the Linear toggle says while no visible Space has Linear on.
pub(crate) const LINEAR_OFF_NOTICE: &str =
    "Linear is off for every Space \u{2014} turn it on in a Space\u{2019}s details";

/// Where to create a personal API key (opened in the external browser).
pub(crate) const API_KEY_SETTINGS_URL: &str = "https://linear.app/settings/account/security";

/// Whether the Space of `snapshot` has Linear on.
pub(crate) fn space_enabled(snapshot: Option<&WorkOutlineSnapshot>) -> bool {
    snapshot.is_some_and(|snapshot| snapshot.space_settings.linear_enabled)
}

/// A recognized Linear URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinearUrl {
    /// `linear.app/<ws>/issue/<ENG-123>/…`: the identifier, uppercased.
    Issue(String),
    /// `linear.app/<ws>/project/<slug>`: the slug (`name-<shortid>`).
    Project(String),
}

/// Recognize a Linear issue or project URL.
pub(crate) fn parse_linear_url(raw: &str) -> Option<LinearUrl> {
    let url = raw.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let path = rest.split(['?', '#']).next().unwrap_or("");
    let parts: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    match parts.as_slice() {
        ["linear.app", _, "issue", id, ..] if !id.is_empty() => {
            Some(LinearUrl::Issue(id.to_uppercase()))
        }
        ["linear.app", _, "project", slug, ..] if !slug.is_empty() => {
            Some(LinearUrl::Project((*slug).to_string()))
        }
        _ => None,
    }
}

/// The short id at the end of a project slug or URL (`launch-v2-5a8b7c9d0e1f`
/// → `5a8b7c9d0e1f`), used to match a pasted project URL to a project. In a
/// URL it reads the segment after `project/` (so `…/overview` tails don't
/// count).
pub(crate) fn project_short_id(slug_or_url: &str) -> Option<&str> {
    let path = slug_or_url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    let mut segments = path.split('/');
    let slug = if path.contains("/project/") {
        segments
            .by_ref()
            .skip_while(|segment| *segment != "project")
            .nth(1)?
    } else {
        path.rsplit('/').next()?
    };
    let short = slug.rsplit('-').next()?;
    (!short.is_empty()).then_some(short)
}

/// The Linear issue identifiers referenced by live items of `snapshot`
/// (uppercased) → the earliest such item, and the same for projects (by
/// project id and by URL).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct LinkedIndex {
    pub issues: std::collections::HashMap<String, String>,
    pub projects: std::collections::HashMap<String, String>,
    pub project_urls: std::collections::HashMap<String, String>,
}

impl LinkedIndex {
    pub(crate) fn from_snapshot(snapshot: &WorkOutlineSnapshot) -> Self {
        let mut index = Self::default();
        let mut refs: Vec<_> = snapshot
            .external_references
            .iter()
            .filter(|r| r.provider == LINEAR_PROVIDER)
            .filter(|r| {
                snapshot
                    .item(&r.work_item_id)
                    .is_some_and(|item| item.archived_at.is_none())
            })
            .collect();
        refs.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        for r in refs {
            if r.reference_type == LINEAR_ISSUE_REFERENCE {
                index
                    .issues
                    .entry(r.external_id.to_uppercase())
                    .or_insert_with(|| r.work_item_id.clone());
            } else if r.reference_type == LINEAR_PROJECT_REFERENCE {
                index
                    .projects
                    .entry(r.external_id.clone())
                    .or_insert_with(|| r.work_item_id.clone());
                if let Some(short) = project_short_id(&r.url) {
                    index
                        .project_urls
                        .entry(short.to_string())
                        .or_insert_with(|| r.work_item_id.clone());
                }
            }
        }
        index
    }

    /// The item that links issue `identifier`, if any.
    pub(crate) fn issue_item(&self, identifier: &str) -> Option<&str> {
        self.issues
            .get(&identifier.to_uppercase())
            .map(String::as_str)
    }

    /// The item that links project `id` (or its URL), if any.
    pub(crate) fn project_item(&self, id: &str, url: &str) -> Option<&str> {
        self.projects
            .get(id)
            .or_else(|| project_short_id(url).and_then(|short| self.project_urls.get(short)))
            .map(String::as_str)
    }
}

/// A Linear hex color (`#5e6ad2`) as a gpui color; `fallback` when it does
/// not parse.
pub(crate) fn hex_color(hex: &str, fallback: Hsla) -> Hsla {
    let digits = hex.trim().trim_start_matches('#');
    if digits.len() != 6 {
        return fallback;
    }
    match u32::from_str_radix(digits, 16) {
        Ok(value) => gpui::rgb(value).into(),
        Err(_) => fallback,
    }
}

/// Short priority label (`Urgent`, `High`, …); `None` for no priority.
pub(crate) fn priority_text(priority: u8, label: &str) -> Option<String> {
    if priority == 0 {
        return None;
    }
    if !label.trim().is_empty() {
        return Some(label.trim().to_string());
    }
    Some(
        match priority {
            1 => "Urgent",
            2 => "High",
            3 => "Medium",
            _ => "Low",
        }
        .to_string(),
    )
}

/// The display name of a Linear user.
pub(crate) fn user_name(user: &zeron_proto::linear::LinearUser) -> String {
    user.display_name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| user.name.clone())
}

#[cfg(test)]
pub(crate) mod fixture {
    use zeron_proto::linear::{
        LinearIssueSummary, LinearProjectRef, LinearProjectSummary, LinearState, LinearStateType,
        LinearTeamRef, LinearUser,
    };

    pub(crate) fn issue(identifier: &str, title: &str) -> LinearIssueSummary {
        LinearIssueSummary {
            id: format!("uuid-{}", identifier.to_lowercase()),
            identifier: identifier.into(),
            title: title.into(),
            description: Some(format!("About {title}")),
            url: format!(
                "https://linear.app/acme/issue/{}/slug",
                identifier.to_lowercase()
            ),
            state: LinearState {
                id: "st-1".into(),
                name: "In Progress".into(),
                state_type: LinearStateType::Started,
                color: "#f2c94c".into(),
            },
            assignee: Some(LinearUser {
                id: "u-1".into(),
                name: "Mikey".into(),
                display_name: None,
                email: None,
            }),
            priority: 2,
            priority_label: "High".into(),
            team: LinearTeamRef {
                id: "t".into(),
                key: "ENG".into(),
                name: "Engineering".into(),
            },
            project: Some(LinearProjectRef {
                id: "p-launch".into(),
                name: "Launch".into(),
                url: "https://linear.app/acme/project/launch-abc123".into(),
            }),
            labels: Vec::new(),
            branch_name: None,
            updated_at: "2026-10-01T00:00:00Z".into(),
        }
    }

    pub(crate) fn project(id: &str, name: &str) -> LinearProjectSummary {
        LinearProjectSummary {
            id: id.into(),
            name: name.into(),
            url: format!(
                "https://linear.app/acme/project/{}-{}",
                name.to_lowercase(),
                id.trim_start_matches("p-")
            ),
            state: Some("In Progress".into()),
            lead: Some(LinearUser {
                id: "u-2".into(),
                name: "Tod".into(),
                display_name: None,
                email: None,
            }),
            open_issue_count: Some(3),
            updated_at: String::new(),
        }
    }

    /// A Work Outline external reference view (provider `linear`).
    pub(crate) fn reference(
        id: &str,
        space_id: &str,
        item_id: &str,
        kind: &str,
        external_id: &str,
        url: &str,
    ) -> zeron_proto::WorkItemExternalReferenceView {
        zeron_proto::WorkItemExternalReferenceView {
            id: id.into(),
            space_id: space_id.into(),
            work_item_id: item_id.into(),
            provider: "linear".into(),
            reference_type: kind.into(),
            external_id: external_id.into(),
            url: url.into(),
            cached_title: None,
            created_by: None,
            created_at: 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_urls_parse_issue_and_project() {
        assert_eq!(
            parse_linear_url("https://linear.app/acme/issue/eng-12/fix-it"),
            Some(LinearUrl::Issue("ENG-12".into()))
        );
        assert_eq!(
            parse_linear_url(" https://linear.app/acme/project/launch-v2-5a8b7c9d0e1f/overview "),
            Some(LinearUrl::Project("launch-v2-5a8b7c9d0e1f".into()))
        );
        assert_eq!(parse_linear_url("https://linear.app/acme/team/ENG"), None);
        assert_eq!(parse_linear_url("https://example.com/issue/x"), None);
        assert_eq!(
            project_short_id("https://linear.app/acme/project/launch-v2-5a8b7c9d0e1f"),
            Some("5a8b7c9d0e1f")
        );
        assert_eq!(
            project_short_id("https://linear.app/acme/project/launch-v2-5a8b7c9d0e1f/overview"),
            Some("5a8b7c9d0e1f")
        );
        assert_eq!(
            project_short_id("launch-v2-5a8b7c9d0e1f"),
            Some("5a8b7c9d0e1f")
        );
    }

    #[test]
    fn linked_index_ignores_archived_items_and_other_providers() {
        let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
        use crate::work_outline::store::fixture_ids::*;
        snapshot.external_references = vec![
            fixture::reference("r1", "s1", API, "issue", "eng-1", "https://x"),
            fixture::reference(
                "r2",
                "s1",
                UI,
                "project",
                "p-1",
                "https://linear.app/acme/project/ui-abc",
            ),
            fixture::reference("r3", "s1", RETIRED, "issue", "ENG-9", "https://x"),
        ];
        let index = LinkedIndex::from_snapshot(&snapshot);
        assert_eq!(index.issue_item("ENG-1"), Some(API));
        assert_eq!(
            index.issue_item("ENG-9"),
            None,
            "archived items don't count"
        );
        assert_eq!(index.project_item("p-1", ""), Some(UI));
        assert_eq!(
            index.project_item("other", "https://linear.app/acme/project/ui-abc"),
            Some(UI)
        );
    }

    #[test]
    fn priority_and_color_helpers() {
        assert_eq!(priority_text(0, "No priority"), None);
        assert_eq!(priority_text(1, ""), Some("Urgent".into()));
        let fallback = gpui::Hsla::default();
        assert_ne!(hex_color("#5e6ad2", fallback), fallback);
        assert_eq!(hex_color("nope", fallback), fallback);
    }
}
