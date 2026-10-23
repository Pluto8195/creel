//! "Review PR" launcher: the wire types of `StartPrReview`, which checks out
//! someone's pull request the way the Cofactr workspace's
//! `tools/cursor-claude-launcher/launch.sh` does and starts a Chat at the
//! workspace root seeded with `/code-review` (or `/fix-ci`).
//!
//! The Chat id derives from `operationId` (`derive_id(op, "chat")`, plus
//! `message`/`command` for the first Turn), so a retry with the same
//! `operationId` returns the same Chat; a retry with other inputs is
//! `operationConflict`. Failures are strings `pr-review/<code>: <message>`
//! ([`PrReviewError::to_wire_message`]).

use serde::{Deserialize, Serialize};

use crate::entities::ChatConfig;

/// Which launcher skill the Chat runs (launch.sh's `fix-ci | code-review`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PrReviewAction {
    CodeReview,
    FixCi,
}

impl PrReviewAction {
    /// The slash command / skill name.
    pub fn skill(self) -> &'static str {
        match self {
            Self::CodeReview => "code-review",
            Self::FixCi => "fix-ci",
        }
    }
}

/// Params of `StartPrReview`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartPrReviewParams {
    pub repo_owner: String,
    pub repo_name: String,
    pub number: u64,
    pub action: PrReviewAction,
    /// Pin the Space (else the engine finds the Space whose folder holds a
    /// checkout whose `origin` is `github.com/<owner>/<name>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
    /// Associate the new Chat with this work item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    /// Harness/model/etc. `None` = Claude Code with the host's defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<ChatConfig>,
    pub operation_id: String,
    /// Routing hint set by the forwarding engine. Callers leave it unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<String>,
}

/// Where the PR's branch ended up (launch.sh's three cases).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PrCheckoutKind {
    /// The branch was already checked out (canonical checkout or a worktree).
    Existing,
    /// The canonical checkout was on `main` with no tracked changes:
    /// `gh pr checkout` ran there.
    CanonicalCheckout,
    /// A new worktree at `<worktrees root>/<repo>-pr-<n>`.
    NewWorktree,
}

/// Reply of `StartPrReview`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartPrReviewReply {
    pub chat_id: String,
    pub space_id: String,
    pub device_id: String,
    pub title: String,
    pub pr_url: String,
    pub branch: String,
    /// The checkout the branch is at.
    pub checkout_path: String,
    pub checkout_kind: PrCheckoutKind,
    /// The Chat's cwd: the Space root (the workspace root).
    pub cwd: String,
    /// Commits the checkout is behind `origin/<branch>` (0 when current).
    #[serde(default)]
    pub behind_by: u64,
    /// The seeded first message.
    pub prompt: String,
    /// Human-readable notes to show next to the result (behind-origin
    /// warning, which fallback was taken and why).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// An existing live review Chat for this PR and action was returned; no
    /// Chat, checkout or Run was created. `branch`, `checkout_path`, `cwd`
    /// and `prompt` are the values recorded when that Chat was launched.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reused: bool,
}

/// Stable failure codes of `StartPrReview`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PrReviewErrorCode {
    Invalid,
    /// The PR's head is in a fork: check it out by hand.
    Fork,
    /// `gh pr view` could not find the PR (or `gh` failed).
    PrNotFound,
    /// `gh` is not installed or not signed in on the host.
    GhUnavailable,
    /// No Space on any reachable device holds a checkout of the repository.
    RepoNotFound,
    /// `<root>/<repo>-pr-<n>` exists but is not on the PR's branch.
    WorktreePathExists,
    /// A git step (fetch, worktree add, checkout) failed.
    GitFailed,
    /// The Space's device could not be reached or is too old.
    DeviceOffline,
    /// `operationId` was already used with other inputs.
    OperationConflict,
}

impl PrReviewErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::Fork => "fork",
            Self::PrNotFound => "prNotFound",
            Self::GhUnavailable => "ghUnavailable",
            Self::RepoNotFound => "repoNotFound",
            Self::WorktreePathExists => "worktreePathExists",
            Self::GitFailed => "gitFailed",
            Self::DeviceOffline => "deviceOffline",
            Self::OperationConflict => "operationConflict",
        }
    }

    fn parse(code: &str) -> Option<Self> {
        Some(match code {
            "invalid" => Self::Invalid,
            "fork" => Self::Fork,
            "prNotFound" => Self::PrNotFound,
            "ghUnavailable" => Self::GhUnavailable,
            "repoNotFound" => Self::RepoNotFound,
            "worktreePathExists" => Self::WorktreePathExists,
            "gitFailed" => Self::GitFailed,
            "deviceOffline" => Self::DeviceOffline,
            "operationConflict" => Self::OperationConflict,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrReviewError {
    pub code: PrReviewErrorCode,
    pub message: String,
}

impl PrReviewError {
    pub const WIRE_PREFIX: &'static str = "pr-review/";

    pub fn new(code: PrReviewErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn to_wire_message(&self) -> String {
        format!(
            "{}{}: {}",
            Self::WIRE_PREFIX,
            self.code.as_str(),
            self.message
        )
    }

    /// Parse `pr-review/<code>: <message>` anywhere in `text` (transport
    /// layers may prefix it).
    pub fn from_wire_message(text: &str) -> Option<Self> {
        let at = text.find(Self::WIRE_PREFIX)?;
        let rest = &text[at + Self::WIRE_PREFIX.len()..];
        let (code, message) = rest.split_once(": ")?;
        Some(Self::new(PrReviewErrorCode::parse(code)?, message))
    }
}

impl std::fmt::Display for PrReviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PrReviewError {}

/// A pull request named by a user: `https://github.com/o/r/pull/n` (any
/// trailing path/query/fragment) or `o/r#n`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrReference {
    pub owner: String,
    pub name: String,
    pub number: u64,
}

impl PrReference {
    pub fn parse(input: &str) -> Option<Self> {
        let text = input.trim();
        let valid_part = |part: &str| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        let number = |text: &str| -> Option<u64> {
            let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
            let rest = &text[digits.len()..];
            if !(rest.is_empty() || rest.starts_with(['/', '?', '#'])) {
                return None;
            }
            digits.parse().ok().filter(|n| *n > 0)
        };
        let url = text
            .strip_prefix("https://")
            .or_else(|| text.strip_prefix("http://"))
            .unwrap_or(text);
        let url = url.strip_prefix("www.").unwrap_or(url);
        if let Some(path) = url.strip_prefix("github.com/") {
            let mut parts = path.splitn(4, '/');
            let (owner, name, pull, tail) =
                (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
            if pull != "pull" || !valid_part(owner) || !valid_part(name) {
                return None;
            }
            return Some(Self {
                owner: owner.into(),
                name: name.into(),
                number: number(tail)?,
            });
        }
        let (repo, n) = text.split_once('#')?;
        let (owner, name) = repo.split_once('/')?;
        if !valid_part(owner) || !valid_part(name) || !n.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        Some(Self {
            owner: owner.into(),
            name: name.into(),
            number: n.parse().ok().filter(|n| *n > 0)?,
        })
    }

    /// `https://github.com/<owner>/<name>/pull/<n>`.
    pub fn url(&self) -> String {
        format!(
            "https://github.com/{}/{}/pull/{}",
            self.owner, self.name, self.number
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_sources_use_their_snake_case_wire_names_and_round_trip() {
        use crate::ChatLinkSource::*;
        for (source, wire) in [
            (Manual, "manual"),
            (ReviewLauncher, "review_launcher"),
            (CreatedInChat, "created_in_chat"),
            (Branch, "branch"),
            (DiffTarget, "diff_target"),
            (Ticket, "ticket"),
            (Mentioned, "mentioned"),
        ] {
            let value = serde_json::to_value(source).unwrap();
            assert_eq!(value, wire);
            assert_eq!(
                serde_json::from_value::<crate::ChatLinkSource>(value).unwrap(),
                source
            );
        }
    }

    #[test]
    fn reply_reused_is_additive_and_omitted_when_false() {
        let mut value = serde_json::json!({
            "chatId": "c", "spaceId": "s", "deviceId": "d", "title": "t",
            "prUrl": "u", "branch": "b", "checkoutPath": "/p",
            "checkoutKind": "canonicalCheckout", "cwd": "/w", "prompt": "p"
        });
        let reply: StartPrReviewReply = serde_json::from_value(value.clone()).unwrap();
        assert!(!reply.reused, "an old reply decodes with reused = false");
        assert!(
            serde_json::to_value(&reply)
                .unwrap()
                .get("reused")
                .is_none()
        );
        value["reused"] = true.into();
        let reused: StartPrReviewReply = serde_json::from_value(value).unwrap();
        assert!(reused.reused);
        assert_eq!(serde_json::to_value(&reused).unwrap()["reused"], true);
    }

    #[test]
    fn parses_urls_and_short_references() {
        let expected = PrReference {
            owner: "Cofactr".into(),
            name: "flagship".into(),
            number: 4321,
        };
        for input in [
            "https://github.com/Cofactr/flagship/pull/4321",
            "  https://github.com/Cofactr/flagship/pull/4321/files ",
            "https://github.com/Cofactr/flagship/pull/4321?w=1",
            "https://github.com/Cofactr/flagship/pull/4321#discussion_r1",
            "github.com/Cofactr/flagship/pull/4321",
            "Cofactr/flagship#4321",
        ] {
            assert_eq!(PrReference::parse(input), Some(expected.clone()), "{input}");
        }
        assert_eq!(
            expected.url(),
            "https://github.com/Cofactr/flagship/pull/4321"
        );
        for input in [
            "",
            "flagship#12",
            "Cofactr/flagship#",
            "Cofactr/flagship#0",
            "Cofactr/flagship#12a",
            "https://github.com/Cofactr/flagship/issues/12",
            "https://github.com/Cofactr/flagship/pull/x",
            "https://gitlab.com/Cofactr/flagship/pull/12",
            "o w/r#1",
        ] {
            assert_eq!(PrReference::parse(input), None, "{input}");
        }
    }

    #[test]
    fn errors_round_trip_through_the_wire_string() {
        let error = PrReviewError::new(PrReviewErrorCode::Fork, "PR 3 comes from a fork");
        let wire = error.to_wire_message();
        assert_eq!(wire, "pr-review/fork: PR 3 comes from a fork");
        assert_eq!(
            PrReviewError::from_wire_message(&format!("transport: {wire}")),
            Some(error)
        );
        assert_eq!(PrReviewError::from_wire_message("work-outline/x: y"), None);
    }

    #[test]
    fn params_wire_shape_is_pinned() {
        let params = StartPrReviewParams {
            repo_owner: "o".into(),
            repo_name: "r".into(),
            number: 7,
            action: PrReviewAction::FixCi,
            space_id: None,
            work_item_id: None,
            config: None,
            operation_id: "op".into(),
            target_device_id: None,
        };
        assert_eq!(
            serde_json::to_value(&params).unwrap(),
            serde_json::json!({
                "repoOwner": "o", "repoName": "r", "number": 7,
                "action": "fixCi", "operationId": "op"
            })
        );
    }
}
