//! Chats linked to PRs for the PR page: the durable `chatPrLinks` rows plus
//! the inferred match rules (PR URL mention, chat branch, diff-target branch,
//! ticket id, review launcher), persisted with one `ChatLinkSource` per rule.

use std::collections::HashMap;

use zeron_proto::PrDashChat;

use crate::workspace_host::WorkspaceHost;

/// What the matcher knows about a PR from the page's fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrLinkCandidate {
    /// GitHub's casing of the PR URL.
    pub url: String,
    pub number: u64,
    /// `owner/name`.
    pub repo_full: String,
    pub head_ref: String,
    pub title: String,
}

impl WorkspaceHost {
    /// Run every match rule for `candidates`, persist new links, and return
    /// the linked chats per candidate keyed by `candidate.url` (best first:
    /// strongest source, live before archived, newest first).
    pub async fn pr_chat_matches(
        &self,
        candidates: &[PrLinkCandidate],
    ) -> HashMap<String, Vec<PrDashChat>> {
        let _ = candidates;
        HashMap::new()
    }
}
