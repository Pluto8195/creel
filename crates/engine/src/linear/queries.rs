//! Every Linear GraphQL document Zeron sends, and the parsers for their
//! responses. Read-only: no mutations exist here.
//!
//! Schema notes (Linear public API, `https://api.linear.app/graphql`):
//! - `issue(id:)` accepts the human identifier (`ENG-123`) as well as the
//!   UUID.
//! - `User.assignedIssues(filter: IssueFilter)`; `IssueFilter.state` is a
//!   `WorkflowStateFilter` whose `type` is a `StringComparator` (`nin`).
//! - `projects(filter: ProjectFilter)`: `lead` is a `NullableUserFilter` and
//!   `members` a `UserCollectionFilter` (hence `some`), both with
//!   `isMe: BooleanComparator`. Project status filtering is done on the
//!   client from `Project.state` (a plain string such as `planned`,
//!   `started`, `completed`, `canceled`) so the query does not depend on the
//!   newer `ProjectStatus` filter shape.
//! - `Project.issues(filter: IssueFilter)` for a project's open issues.
//! - Lists are cursor-paginated (`first`/`after`, `pageInfo { hasNextPage
//!   endCursor }`), most recently updated first (`orderBy: updatedAt`).

use serde_json::{Value, json};
use zeron_proto::linear::{
    LinearIssueSummary, LinearLabel, LinearProjectRef, LinearProjectSummary, LinearState,
    LinearTeamRef, LinearUser,
};

/// Page size of every list query.
pub const PAGE_SIZE: usize = 50;

const ISSUE_FIELDS: &str = "id identifier title description url priority priorityLabel \
branchName updatedAt state { id name type color } assignee { id name displayName email } \
team { id key name } project { id name url } labels(first: 20) { nodes { name color } }";

const USER_FIELDS: &str = "id name displayName email";

pub fn viewer() -> String {
    format!("query ZeronLinearViewer {{ viewer {{ {USER_FIELDS} }} }}")
}

pub fn issue() -> String {
    format!("query ZeronLinearIssue($id: String!) {{ issue(id: $id) {{ {ISSUE_FIELDS} }} }}")
}

pub fn my_issues() -> String {
    format!(
        "query ZeronLinearMyIssues($first: Int!, $after: String) {{ viewer {{ \
         assignedIssues(first: $first, after: $after, orderBy: updatedAt, \
         filter: {{ state: {{ type: {{ nin: [\"completed\", \"canceled\"] }} }} }}) {{ \
         nodes {{ {ISSUE_FIELDS} }} pageInfo {{ hasNextPage endCursor }} }} }} }}"
    )
}

pub fn my_projects() -> String {
    format!(
        "query ZeronLinearMyProjects($first: Int!, $after: String) {{ \
         projects(first: $first, after: $after, orderBy: updatedAt, \
         filter: {{ or: [{{ lead: {{ isMe: {{ eq: true }} }} }}, \
         {{ members: {{ some: {{ isMe: {{ eq: true }} }} }} }}] }}) {{ \
         nodes {{ id name url state updatedAt lead {{ {USER_FIELDS} }} }} \
         pageInfo {{ hasNextPage endCursor }} }} }}"
    )
}

pub fn project_issues() -> String {
    format!(
        "query ZeronLinearProjectIssues($id: String!, $first: Int!, $after: String) {{ \
         project(id: $id) {{ issues(first: $first, after: $after, orderBy: updatedAt, \
         filter: {{ state: {{ type: {{ nin: [\"completed\", \"canceled\"] }} }} }}) {{ \
         nodes {{ {ISSUE_FIELDS} }} pageInfo {{ hasNextPage endCursor }} }} }} }}"
    )
}

/// Most attachments read per issue.
pub const MAX_ATTACHMENTS: usize = 50;

/// An issue's attachments (Linear's GitHub integration attaches the PRs
/// whose branch or title names the issue). Kept apart from [`issue`] so a
/// wrong guess at this shape never breaks the issue query: these fields
/// were not verified against the real API, and the parser is lenient.
pub fn issue_attachments() -> String {
    format!(
        "query ZeronLinearIssueAttachments($id: String!) {{ issue(id: $id) {{ identifier \
         attachments(first: {MAX_ATTACHMENTS}) {{ nodes {{ url title subtitle sourceType \
         metadata updatedAt }} }} }} }}"
    )
}

/// One issue attachment (only what the PR links use).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueAttachment {
    pub url: String,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    /// `github`, `slack`, ... (free-form; informational).
    pub source_type: Option<String>,
    pub updated_at: Option<String>,
}

/// An `issue_attachments` response's list. `None` when the response does
/// not have the expected shape (no `issue.attachments.nodes`); a missing
/// issue reads as `Some(empty)` only when `issue` is explicitly null.
pub fn parse_issue_attachments(data: &Value) -> Option<Vec<IssueAttachment>> {
    let issue = data.get("issue")?;
    if issue.is_null() {
        return Some(Vec::new());
    }
    let nodes = issue.pointer("/attachments/nodes")?.as_array()?;
    Some(
        nodes
            .iter()
            .filter_map(|node| {
                let url = opt_s(node, "url")?;
                // `sourceType` was an enum-like string; tolerate an object.
                let source_type = opt_s(node, "sourceType").or_else(|| {
                    node.pointer("/sourceType/type")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                });
                Some(IssueAttachment {
                    url,
                    title: opt_s(node, "title"),
                    subtitle: opt_s(node, "subtitle"),
                    source_type,
                    updated_at: opt_s(node, "updatedAt"),
                })
            })
            .collect(),
    )
}

/// Variables of a list page.
pub fn page_variables(extra: Value, after: Option<&str>) -> Value {
    let mut variables = match extra {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    variables.insert("first".into(), json!(PAGE_SIZE));
    variables.insert("after".into(), after.map_or(Value::Null, |a| json!(a)));
    Value::Object(variables)
}

/// One page of a connection: its nodes and the next cursor (when there is
/// a next page).
pub fn connection_page(connection: Option<&Value>) -> (Vec<Value>, Option<String>) {
    let Some(connection) = connection else {
        return (Vec::new(), None);
    };
    let nodes = connection
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let info = connection.get("pageInfo");
    let next = info
        .filter(|i| i.get("hasNextPage").and_then(Value::as_bool) == Some(true))
        .and_then(|i| i.get("endCursor"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    (nodes, next)
}

fn s(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn opt_s(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub fn user(value: &Value) -> Option<LinearUser> {
    let id = opt_s(value, "id")?;
    Some(LinearUser {
        id,
        name: s(value, "name"),
        display_name: opt_s(value, "displayName"),
        email: opt_s(value, "email"),
    })
}

/// A `viewer` response's user.
pub fn parse_viewer(data: &Value) -> Option<LinearUser> {
    data.get("viewer").and_then(user)
}

pub fn issue_summary(node: &Value) -> Option<LinearIssueSummary> {
    let identifier = opt_s(node, "identifier")?;
    let state = node.get("state").cloned().unwrap_or(Value::Null);
    let team = node.get("team").cloned().unwrap_or(Value::Null);
    Some(LinearIssueSummary {
        id: s(node, "id"),
        identifier,
        title: s(node, "title"),
        description: opt_s(node, "description").filter(|d| !d.trim().is_empty()),
        url: s(node, "url"),
        state: LinearState {
            id: s(&state, "id"),
            name: s(&state, "name"),
            state_type: state
                .get("type")
                .and_then(|t| serde_json::from_value(t.clone()).ok())
                .unwrap_or_default(),
            color: s(&state, "color"),
        },
        assignee: node.get("assignee").and_then(user),
        priority: node
            .get("priority")
            .and_then(Value::as_f64)
            .map_or(0, |p| p.clamp(0.0, 4.0) as u8),
        priority_label: s(node, "priorityLabel"),
        team: LinearTeamRef {
            id: s(&team, "id"),
            key: s(&team, "key"),
            name: s(&team, "name"),
        },
        project: node.get("project").filter(|p| p.is_object()).and_then(|p| {
            Some(LinearProjectRef {
                id: opt_s(p, "id")?,
                name: s(p, "name"),
                url: s(p, "url"),
            })
        }),
        labels: node
            .pointer("/labels/nodes")
            .and_then(Value::as_array)
            .map(|labels| {
                labels
                    .iter()
                    .map(|l| LinearLabel {
                        name: s(l, "name"),
                        color: s(l, "color"),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        branch_name: opt_s(node, "branchName"),
        updated_at: s(node, "updatedAt"),
    })
}

/// Project states that are not open.
pub fn project_is_closed(state: Option<&str>) -> bool {
    matches!(state, Some("completed" | "canceled" | "cancelled"))
}

pub fn project_summary(node: &Value) -> Option<LinearProjectSummary> {
    Some(LinearProjectSummary {
        id: opt_s(node, "id")?,
        name: s(node, "name"),
        url: s(node, "url"),
        state: opt_s(node, "state"),
        lead: node.get("lead").and_then(user),
        open_issue_count: None,
        updated_at: s(node, "updatedAt"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_are_read_only_named_queries() {
        for doc in [
            viewer(),
            issue(),
            issue_attachments(),
            my_issues(),
            my_projects(),
            project_issues(),
        ] {
            assert!(doc.starts_with("query ZeronLinear"), "{doc}");
            assert!(!doc.contains("mutation"), "{doc}");
            assert_eq!(doc.matches('{').count(), doc.matches('}').count(), "{doc}");
        }
        assert!(my_issues().contains("nin: [\"completed\", \"canceled\"]"));
        assert!(my_projects().contains("members: { some: { isMe: { eq: true } } }"));
        assert!(my_projects().contains("lead: { isMe: { eq: true } }"));
    }

    #[test]
    fn issue_nodes_parse_leniently() {
        let node = json!({
            "id": "u", "identifier": "ENG-1", "title": "T", "description": "  ",
            "url": "https://linear.app/a/issue/ENG-1", "priority": 2.0, "priorityLabel": "High",
            "state": {"id": "s", "name": "Todo", "type": "unstarted", "color": "#fff"},
            "assignee": null, "team": {"id": "t", "key": "ENG", "name": "Eng"},
            "project": null, "labels": {"nodes": [{"name": "bug", "color": "#f00"}]},
            "branchName": "", "updatedAt": "2026-10-13T00:00:00.000Z",
        });
        let issue = issue_summary(&node).unwrap();
        assert_eq!(issue.identifier, "ENG-1");
        assert_eq!(issue.description, None, "blank description reads as none");
        assert_eq!(issue.priority, 2);
        assert_eq!(issue.labels.len(), 1);
        assert!(issue.assignee.is_none() && issue.project.is_none() && issue.branch_name.is_none());
        assert!(issue_summary(&json!({"title": "no identifier"})).is_none());
    }

    #[test]
    fn attachments_parse_leniently() {
        let data = json!({ "issue": { "identifier": "ENG-1", "attachments": { "nodes": [
            { "url": "https://github.com/acme/ui/pull/9", "title": "Fix", "sourceType": "github",
              "subtitle": "Open", "metadata": { "state": "open" }, "updatedAt": "2026-10-13" },
            { "url": "https://acme.slack.com/archives/C1/p1", "sourceType": { "type": "slack" } },
            { "title": "no url" },
        ]}}});
        let attachments = parse_issue_attachments(&data).unwrap();
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].title.as_deref(), Some("Fix"));
        assert_eq!(attachments[0].source_type.as_deref(), Some("github"));
        assert_eq!(attachments[1].source_type.as_deref(), Some("slack"));
        assert_eq!(
            parse_issue_attachments(&json!({ "issue": null })),
            Some(vec![])
        );
        assert_eq!(
            parse_issue_attachments(&json!({ "issue": { "identifier": "ENG-1" } })),
            None
        );
        assert_eq!(parse_issue_attachments(&json!({})), None);
    }

    #[test]
    fn pages_report_the_next_cursor_only_when_there_is_one() {
        let (nodes, next) = connection_page(Some(&json!({
            "nodes": [{"id": 1}], "pageInfo": {"hasNextPage": true, "endCursor": "c1"}
        })));
        assert_eq!(nodes.len(), 1);
        assert_eq!(next.as_deref(), Some("c1"));
        let (_, next) = connection_page(Some(&json!({
            "nodes": [], "pageInfo": {"hasNextPage": false, "endCursor": "c2"}
        })));
        assert_eq!(next, None);
        assert_eq!(
            page_variables(json!({"id": "p"}), Some("c")),
            json!({"id": "p", "first": PAGE_SIZE, "after": "c"})
        );
    }
}
