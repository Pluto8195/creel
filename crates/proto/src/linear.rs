//! Linear integration wire contract (version 1: read-only, per-Space opt-in).
//!
//! Contract doc: `docs/plans/work-outline/linear-contract.md`. Plan:
//! `docs/plans/work-outline/parked-proposals.md` ("Linear integration").
//!
//! - The engine holds one Linear personal API key per device (macOS
//!   Keychain, else `LINEAR_API_KEY`). The key is never sent to clients,
//!   agents, MCP, or harness environments; [`LinearSetApiKeyParams`] is the
//!   only wire type that carries it and its `Debug` redacts it.
//! - Linear RPCs (method names in `mast_rpc::methods`, `LINEAR_*`) are
//!   local/IPC and never device-forwarded. Failures are strings
//!   `linear/<code>: <message>` ([`LinearError::from_wire_message`]).
//! - Linear is OFF per Space until the user enables it
//!   (`setSpaceLinearEnabled`, a `WorkOutlineMutate` op). Space-scoped
//!   operations (`LinearRefreshLinkedItems`, `createWorkItemFromLinear`,
//!   `linkLinearProject`) fail with `linear/disabledForSpace` in a Space
//!   with Linear off; the account-wide reads work regardless (the UI only
//!   shows them in enabled Spaces).
//! - Nothing is ever written to Linear.
//!
//! Wire conventions follow `work_outline.rs`: camelCase fields, every
//! non-identity field decodes when absent, enums from Linear are lenient.

use serde::{Deserialize, Serialize};

use crate::work_outline::limits;

/// `provider` of a Work Outline external reference that points at Linear.
pub const LINEAR_PROVIDER: &str = "linear";
/// `referenceType` of a Linear issue reference (`externalId` = the issue
/// identifier, e.g. `ENG-123`; `url` = the issue URL).
pub const LINEAR_ISSUE_REFERENCE: &str = "issue";
/// `referenceType` of a Linear project reference (`externalId` = the
/// project id; `url` = the project URL; `cachedTitle` = the project name).
pub const LINEAR_PROJECT_REFERENCE: &str = "project";
/// Entries of `WorkItemView::external_sync_modified_fields`: the user edited
/// this field of an issue-linked item, so Linear no longer updates it.
pub const SYNC_FIELD_TITLE: &str = "title";
pub const SYNC_FIELD_DESCRIPTION: &str = "description";
/// The capability a device advertises when it serves the Linear RPCs and
/// understands the Linear Work Outline operations.
pub const LINEAR_CAPABILITY_V1: &str = "linear-v1";
/// Separator between identifier and title in an issue-linked item's title.
pub const LINEAR_TITLE_SEPARATOR: &str = " · ";

/// Budget for a description copied from Linear, in UTF-16 units of its JSON
/// encoding: keeps the registry op under `limits::MAX_OP_UTF16_UNITS` even
/// when the text is escape-heavy.
const DESCRIPTION_OP_BUDGET_UTF16: usize = 12_000;

/// The title an issue-linked work item carries: `"ENG-123 · <issue title>"`,
/// whitespace-collapsed and cut to `limits::MAX_TITLE_CHARS`.
pub fn linear_item_title(identifier: &str, title: &str) -> String {
    let identifier = identifier.trim();
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let full = if title.is_empty() {
        identifier.to_owned()
    } else if identifier.is_empty() {
        title
    } else {
        format!("{identifier}{LINEAR_TITLE_SEPARATOR}{title}")
    };
    truncate_chars(&full, limits::MAX_TITLE_CHARS)
}

/// The description an issue-linked work item carries: Linear's description
/// (Markdown) cut to `limits::MAX_DESCRIPTION_CHARS` and to a JSON-encoded
/// budget that fits one registry op. `None` for an empty description.
pub fn linear_item_description(description: Option<&str>) -> Option<String> {
    let text = description?.trim_end();
    if text.trim().is_empty() {
        return None;
    }
    let mut out = truncate_chars(text, limits::MAX_DESCRIPTION_CHARS);
    while json_utf16_len(&out) > DESCRIPTION_OP_BUDGET_UTF16 {
        let keep = out.chars().count() * 9 / 10;
        out = truncate_chars(&out, keep);
    }
    Some(out)
}

fn json_utf16_len(text: &str) -> usize {
    serde_json::to_string(text).map_or(usize::MAX, |s| s.encode_utf16().count())
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

// --------------------------------------------------------------------------
// Linear entities
// --------------------------------------------------------------------------

/// A Linear user (the viewer, an assignee, a project lead).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearUser {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// A workflow state's category. Lenient: an unknown type reads as `unknown`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LinearStateType {
    Triage,
    Backlog,
    Unstarted,
    Started,
    Completed,
    Canceled,
    #[default]
    #[serde(other)]
    Unknown,
}

impl LinearStateType {
    /// Backlog, unstarted and triage: the work has not started.
    pub fn is_unstarted(self) -> bool {
        matches!(self, Self::Triage | Self::Backlog | Self::Unstarted)
    }

    /// Completed and canceled issues are closed (not "open").
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Completed | Self::Canceled)
    }
}

/// An issue's workflow state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearState {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "type")]
    pub state_type: LinearStateType,
    /// Hex color, e.g. `#5e6ad2`.
    #[serde(default)]
    pub color: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearLabel {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub color: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearTeamRef {
    #[serde(default)]
    pub id: String,
    /// The identifier prefix, e.g. `ENG`.
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearProjectRef {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub url: String,
}

/// One Linear issue as Mast shows it (chips, the Linear panel, agents).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearIssueSummary {
    /// Linear's UUID.
    pub id: String,
    /// Human identifier, e.g. `ENG-123`.
    pub identifier: String,
    #[serde(default)]
    pub title: String,
    /// Markdown; `None` when the issue has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub state: LinearState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<LinearUser>,
    /// 0 = no priority, 1 = urgent, 2 = high, 3 = medium, 4 = low.
    #[serde(default)]
    pub priority: u8,
    /// Linear's label for `priority` (`"No priority"`, `"Urgent"`, …).
    #[serde(default)]
    pub priority_label: String,
    #[serde(default)]
    pub team: LinearTeamRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<LinearProjectRef>,
    #[serde(default)]
    pub labels: Vec<LinearLabel>,
    /// Linear's suggested git branch name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_name: Option<String>,
    /// ISO-8601 timestamp.
    #[serde(default)]
    pub updated_at: String,
}

impl LinearIssueSummary {
    /// The linked Linear item's rule: assigned to `viewer_id`, state type
    /// backlog, unstarted or triage, and not in any project.
    pub fn is_assigned_unstarted(&self, viewer_id: &str) -> bool {
        self.assignee.as_ref().is_some_and(|a| a.id == viewer_id)
            && self.state.state_type.is_unstarted()
            && self.project.is_none()
    }
}

/// One Linear project (the "My projects" list).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearProjectSummary {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub url: String,
    /// The project's status name (e.g. `"In Progress"`), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead: Option<LinearUser>,
    /// Open issues, when the host counted them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_issue_count: Option<u32>,
    #[serde(default)]
    pub updated_at: String,
}

impl LinearProjectSummary {
    pub fn to_ref(&self) -> LinearProjectRef {
        LinearProjectRef {
            id: self.id.clone(),
            name: self.name.clone(),
            url: self.url.clone(),
        }
    }
}

/// Where the engine's key comes from. Lenient: unknown reads as `none`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LinearKeySource {
    Keychain,
    Environment,
    #[default]
    #[serde(other)]
    None,
}

/// Reply of `LinearStatus`, `LinearSetApiKey`, and `LinearClearApiKey`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearConnectionStatus {
    /// A key is stored (keychain) or present in the environment.
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub source: LinearKeySource,
    /// The key's user, when the host has verified it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer: Option<LinearUser>,
    /// The last verification failure (never contains the key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// --------------------------------------------------------------------------
// RPC params and replies (method names: `mast_rpc::methods::LINEAR_*`)
// --------------------------------------------------------------------------

/// `LinearStatus {}` → [`LinearConnectionStatus`]. Verifies the key with a
/// `viewer` query at most once per engine run (cached).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearStatusParams {
    /// Re-run the `viewer` check instead of using the cached result.
    #[serde(default, skip_serializing_if = "is_false")]
    pub force_refresh: bool,
}

/// `LinearSetApiKey {apiKey}` → [`LinearConnectionStatus`]: validate the key
/// with `viewer { id name email }`, then store it in the Keychain and read
/// it back. An invalid key is `linear/unauthorized` and nothing is stored.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearSetApiKeyParams {
    pub api_key: String,
}

impl std::fmt::Debug for LinearSetApiKeyParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinearSetApiKeyParams")
            .field("api_key", &"<redacted>")
            .finish()
    }
}

/// `LinearClearApiKey {}` → [`LinearConnectionStatus`]: delete the stored
/// key (an environment key stays in effect and is reported).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearClearApiKeyParams {}

/// `LinearIssue {identifier, forceRefresh?}` → [`LinearIssueSummary`].
/// Served from a ~5 minute per-identifier cache unless `forceRefresh`.
/// Unknown identifier: `linear/notFound`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearIssueParams {
    pub identifier: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub force_refresh: bool,
}

/// `LinearMyIssues {}` → `[LinearIssueSummary]`: issues assigned to the
/// viewer whose state type is not completed/canceled, most recently updated
/// first, at most [`MAX_LIST_ISSUES`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearMyIssuesParams {
    /// Skip the engine's list cache.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
    /// Answer from the stored list only, however old (an error if none).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cached_only: bool,
}

/// `LinearMyProjects {}` → `[LinearProjectSummary]`: projects the viewer
/// leads or is a member of that are not completed/canceled.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearMyProjectsParams {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cached_only: bool,
}

/// `LinearProjectIssues {projectId}` → `[LinearIssueSummary]`: the project's
/// open issues (not completed/canceled), at most [`MAX_LIST_ISSUES`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearProjectIssuesParams {
    pub project_id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cached_only: bool,
}

/// `LinearProjectDetail {projectId}` → [`LinearProjectDetail`]: one project's
/// state, progress, target date, lead and latest updates (read-only; the
/// canonical plan panel). Cached like the lists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearProjectDetailParams {
    pub project_id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cached_only: bool,
}

/// A project status update ("Project update" in Linear).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearProjectUpdate {
    #[serde(default)]
    pub id: String,
    /// Markdown body, cut to [`MAX_UPDATE_BODY_CHARS`].
    #[serde(default)]
    pub body: String,
    /// `onTrack`, `atRisk`, `offTrack`, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<LinearUser>,
    /// ISO-8601 timestamp.
    #[serde(default)]
    pub created_at: String,
}

/// Longest project update body kept.
pub const MAX_UPDATE_BODY_CHARS: usize = 600;
/// Latest project updates fetched.
pub const MAX_PROJECT_UPDATES: usize = 3;

/// A Linear project in full (for the canonical plan panel).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearProjectDetail {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub url: String,
    /// The project's status name, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Completed share of the project's scope, `0.0..=1.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
    /// `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead: Option<LinearUser>,
    /// Newest first, at most [`MAX_PROJECT_UPDATES`].
    #[serde(default)]
    pub updates: Vec<LinearProjectUpdate>,
    #[serde(default)]
    pub updated_at: String,
}

/// `LinearRefreshLinkedItems {spaceId}` → [`LinearRefreshLinkedItemsReply`]:
/// re-fetch every issue linked from the Space's live items (bypassing the
/// cache) and apply title/description changes the user has not overridden.
/// `linear/disabledForSpace` when the Space has Linear off.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearRefreshLinkedItemsParams {
    pub space_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearRefreshLinkedItemsReply {
    /// Work items whose title or description changed.
    #[serde(default)]
    pub updated: u32,
}

/// Most issues a list RPC returns (pagination stops there).
pub const MAX_LIST_ISSUES: usize = 250;

fn is_false(value: &bool) -> bool {
    !*value
}

// --------------------------------------------------------------------------
// Work Outline operation payloads
// --------------------------------------------------------------------------

/// The part of a [`LinearIssueSummary`] `createWorkItemFromLinear` stores.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearIssueLink {
    /// Linear's UUID.
    #[serde(default)]
    pub id: String,
    pub identifier: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub url: String,
}

impl From<&LinearIssueSummary> for LinearIssueLink {
    fn from(issue: &LinearIssueSummary) -> Self {
        Self {
            id: issue.id.clone(),
            identifier: issue.identifier.clone(),
            title: issue.title.clone(),
            description: issue.description.clone(),
            url: issue.url.clone(),
        }
    }
}

// --------------------------------------------------------------------------
// Errors
// --------------------------------------------------------------------------

/// Machine-readable Linear failure. Lenient: unknown reads as `invalid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LinearErrorCode {
    /// No key in the Keychain or environment: prompt for one.
    MissingKey,
    /// Linear rejected the key (HTTP 401 or an authentication error). The
    /// host has already deleted the stored key: prompt again.
    Unauthorized,
    /// Linear rate-limited the request beyond the host's bounded retries.
    RateLimited,
    /// The issue or project does not exist (or the key cannot see it).
    NotFound,
    /// Transport failure, timeout, or an unexpected Linear response.
    Network,
    /// The Space has Linear turned off.
    DisabledForSpace,
    /// Malformed request (or a Linear GraphQL validation error).
    #[serde(other)]
    Invalid,
}

impl LinearErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingKey => "missingKey",
            Self::Unauthorized => "unauthorized",
            Self::RateLimited => "rateLimited",
            Self::NotFound => "notFound",
            Self::Network => "network",
            Self::DisabledForSpace => "disabledForSpace",
            Self::Invalid => "invalid",
        }
    }

    pub fn parse(code: &str) -> Self {
        match code {
            "missingKey" => Self::MissingKey,
            "unauthorized" => Self::Unauthorized,
            "rateLimited" => Self::RateLimited,
            "notFound" => Self::NotFound,
            "network" => Self::Network,
            "disabledForSpace" => Self::DisabledForSpace,
            _ => Self::Invalid,
        }
    }
}

/// A failed Linear operation, wire-encoded as `linear/<code>: <message>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearError {
    pub code: LinearErrorCode,
    #[serde(default)]
    pub message: String,
}

const ERROR_PREFIX: &str = "linear/";

impl LinearError {
    pub fn new(code: LinearErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn disabled_for_space() -> Self {
        Self::new(
            LinearErrorCode::DisabledForSpace,
            "Linear is turned off for this Space",
        )
    }

    pub fn missing_key() -> Self {
        Self::new(LinearErrorCode::MissingKey, "No Linear API key is set")
    }

    /// `linear/<code>: <message>`.
    pub fn to_wire_message(&self) -> String {
        format!("{ERROR_PREFIX}{}: {}", self.code.as_str(), self.message)
    }

    /// Inverse of [`Self::to_wire_message`]; `None` for any other string.
    pub fn from_wire_message(text: &str) -> Option<Self> {
        let rest = text.strip_prefix(ERROR_PREFIX)?;
        let (code, message) = rest.split_once(": ").unwrap_or((rest, ""));
        Some(Self {
            code: LinearErrorCode::parse(code),
            message: message.to_owned(),
        })
    }
}

impl std::fmt::Display for LinearError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LinearError {}

#[cfg(test)]
mod tests;
