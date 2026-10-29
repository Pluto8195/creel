//! Pull requests on work items (docs/pr-review.md "PRs on work items").
//!
//! Two sources add a `github` / `pullRequest` reference to a work item, as
//! the system actor, through the ordinary mutation path:
//!
//! - **Linear** (primary, for items linked to a Linear issue): the GitHub PRs
//!   Linear's GitHub integration attached to the issue. Runs inside the
//!   linked-item Linear sync (`linear::sync`), so on its cadence.
//! - **Chats** (fallback, any item): a PR the item's Chat created. Only high
//!   confidence counts: a `manual` or `created_in_chat` durable chat link
//!   (`chatPrLinks`), never a `mentioned` one and never branch inference
//!   (`pr_ticket_cache::fetch_github_pr`, which at a superproject root
//!   resolves every chat to the same PR). Chats whose runs predate tool
//!   output capture get their stored transcript re-read once
//!   ([`created_pr_urls`]) to recover `created_in_chat`. Runs inside the PR
//!   review sync loop (`pr_review_sync`), every 3 minutes.
//!
//! Rules shared by both: the PR's repository must be in the Space
//! (superproject, submodules, nested repositories); references are only ever
//! added (never removed, never moved), one per item and PR (content-derived
//! reference id plus a hashed operation id); a reference the user removed is
//! not added back; item status is never touched. A Chat's PR is linked once:
//! to the item the Chat belongs to when the PR is first seen, and only when
//! no item of the Space links that PR yet. Durable chat links are read, never
//! rewritten, except that the transcript backfill adds `created_in_chat`
//! rows (the same write the live tap makes; precedence keeps `manual`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use mast_doc::{KIND_WORK_ITEM_EXTERNAL_REFERENCES, MessagePart, RowState, SessionMessageEntry};
use mast_proto::pr_review_sync::{GITHUB_PROVIDER, GITHUB_PULL_REQUEST_REFERENCE, PullRequestRef};
use mast_proto::work_outline::{
    CreateWorkItemExternalReferenceParams, WorkItemChatAssignmentState, WorkOutlineErrorCode,
    derive_id, external_reference_id, limits,
};
use mast_proto::{ChatLinkSource, ChatPrLink, WorkOutlineMutation, WorkOutlineSnapshot};

use crate::pr_review_sync::PrReviewSyncService;

/// Reads a Chat's stored transcript (`None` = nothing stored here). Blocking.
pub type TranscriptReader = Arc<dyn Fn(&str) -> Option<Vec<SessionMessageEntry>> + Send + Sync>;

/// Most Chat transcripts one pass re-reads (the one-time backfill).
pub const MAX_TRANSCRIPT_SCANS_PER_PASS: usize = 50;

/// Processed-ledger key: this Chat's transcript was re-read.
const SCAN_MARKER_PREFIX: &str = "work-outline-pr-links:scan:v1:";
/// Processed-ledger key: this Chat's PR was linked (never link it again).
const LINKED_MARKER_PREFIX: &str = "work-outline-pr-links:chat:v1:";

/// A transcript reader over the engine's doc store: a bare snapshot peek
/// (no doc handle, no sync room), like `context_usage`'s live path.
pub fn store_transcript_reader(store: Arc<mast_sync::DocsStore>) -> TranscriptReader {
    Arc::new(move |chat_id: &str| {
        let bytes = store.load_snapshot(chat_id).ok().flatten()?;
        let loro = loro::LoroDoc::new();
        loro.import(&bytes).ok()?;
        mast_doc::SessionDoc::from_doc(loro).read_entries().ok()
    })
}

/// `github.com/<owner>/<repo>/pull/<n>` anywhere in `url` (scheme optional,
/// trailing path/query ignored), keeping the URL's own casing.
pub fn pull_request_from_url(url: &str) -> Option<PullRequestRef> {
    const HOST: &str = "github.com/";
    let at = url.to_ascii_lowercase().find(HOST)?;
    let rest = &url[at + HOST.len()..];
    let mut parts = rest.splitn(4, '/');
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    let owner = parts.next().filter(|p| valid(p))?;
    let repo = parts.next().filter(|p| valid(p))?;
    if !parts.next()?.eq_ignore_ascii_case("pull") {
        return None;
    }
    let tail = parts.next()?;
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    let number: u64 = digits.parse().ok().filter(|n| *n > 0)?;
    Some(PullRequestRef::new(owner, repo, number))
}

/// Lowercased `owner/repo` (the repository-set key).
pub fn repo_slug(pr: &PullRequestRef) -> String {
    format!("{}/{}", pr.owner, pr.repo).to_ascii_lowercase()
}

/// The link's operation id: hashed so it fits `MAX_OPERATION_ID_CHARS`.
pub fn link_operation(work_item_id: &str, pr_key: &str) -> String {
    format!(
        "pr-item-link-{}",
        derive_id(&format!("{work_item_id}\0{pr_key}"), "pr-item-link")
    )
}

/// Sources that mean "this Chat's PR": the user said so, or the Chat ran
/// the command that created it.
pub fn is_high_confidence(source: ChatLinkSource) -> bool {
    matches!(
        source,
        ChatLinkSource::Manual | ChatLinkSource::ReviewLauncher | ChatLinkSource::CreatedInChat
    )
}

/// PR URLs a PR-creation tool call (`gh pr create` / `gh pr new` / a
/// `create_pull_request` tool) printed in its own successful output, in
/// transcript order, canonicalized and de-duplicated. Narrated text never
/// counts.
pub fn created_pr_urls(entries: &[SessionMessageEntry]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut urls = Vec::new();
    for entry in entries {
        for part in &entry.parts {
            let MessagePart::Tool {
                call,
                is_error: false,
                output: Some(output),
                ..
            } = part
            else {
                continue;
            };
            let invocation = crate::chat_links::tool_call_invocation_text(call);
            if !crate::chat_links::looks_like_pr_create(&invocation) {
                continue;
            }
            for url in crate::chat_links::extract_pr_urls(output) {
                let url = crate::chat_links::canonicalize_pr_url(&url);
                if seen.insert(url.clone()) {
                    urls.push(url);
                }
            }
        }
    }
    urls
}

/// One reference to add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedLink {
    pub work_item_id: String,
    pub pr: PullRequestRef,
    /// The Chat whose PR this is (Chat source only).
    pub chat_id: Option<String>,
    pub title: Option<String>,
}

/// The live item `chat_id` is associated with in `snapshot`.
pub fn assigned_item<'a>(snapshot: &'a WorkOutlineSnapshot, chat_id: &str) -> Option<&'a str> {
    let assignment = snapshot
        .assignments
        .iter()
        .find(|a| a.chat_id == chat_id && a.state == WorkItemChatAssignmentState::Associated)?;
    let item = snapshot.item(assignment.work_item_id.as_deref()?)?;
    item.archived_at.is_none().then_some(item.id.as_str())
}

/// Chats of `snapshot` associated with a live item, sorted.
pub fn assigned_chats(snapshot: &WorkOutlineSnapshot) -> Vec<String> {
    let mut chats: Vec<String> = snapshot
        .assignments
        .iter()
        .filter(|a| assigned_item(snapshot, &a.chat_id).is_some())
        .map(|a| a.chat_id.clone())
        .collect();
    chats.sort();
    chats.dedup();
    chats
}

/// Keys of the PRs `work_item_id` links (`None` = every item of the Space).
fn linked_keys(snapshot: &WorkOutlineSnapshot, work_item_id: Option<&str>) -> BTreeSet<String> {
    snapshot
        .external_references
        .iter()
        .filter(|r| {
            r.provider == GITHUB_PROVIDER
                && r.reference_type == GITHUB_PULL_REQUEST_REFERENCE
                && work_item_id.is_none_or(|id| r.work_item_id == id)
        })
        .filter_map(|r| PullRequestRef::parse_external_id(&r.external_id))
        .map(|pr| pr.key())
        .collect()
}

/// Chat-created links: each associated Chat's high-confidence PRs in the
/// Space's repositories that no item of the Space links yet (first Chat by
/// id wins a PR two Chats claim).
pub fn plan_chat_links(
    snapshot: &WorkOutlineSnapshot,
    chat_links: &BTreeMap<String, Vec<ChatPrLink>>,
    slugs: &BTreeSet<String>,
) -> Vec<PlannedLink> {
    let mut taken = linked_keys(snapshot, None);
    let mut planned = Vec::new();
    for (chat_id, links) in chat_links {
        let Some(item) = assigned_item(snapshot, chat_id) else {
            continue;
        };
        for link in links.iter().filter(|l| is_high_confidence(l.source)) {
            let Some(pr) = pull_request_from_url(&link.url) else {
                continue;
            };
            if !slugs.contains(&repo_slug(&pr)) || !taken.insert(pr.key()) {
                continue;
            }
            planned.push(PlannedLink {
                work_item_id: item.to_owned(),
                pr,
                chat_id: Some(chat_id.clone()),
                title: None,
            });
        }
    }
    planned
}

/// Linear-attached links: the issue's PRs (`(pr, title)`) in the Space's
/// repositories that the live item does not link yet.
pub fn plan_issue_links(
    snapshot: &WorkOutlineSnapshot,
    work_item_id: &str,
    prs: &[(PullRequestRef, Option<String>)],
    slugs: &BTreeSet<String>,
) -> Vec<PlannedLink> {
    if snapshot
        .item(work_item_id)
        .is_none_or(|item| item.archived_at.is_some())
    {
        return Vec::new();
    }
    let mut taken = linked_keys(snapshot, Some(work_item_id));
    prs.iter()
        .filter(|(pr, _)| slugs.contains(&repo_slug(pr)) && taken.insert(pr.key()))
        .map(|(pr, title)| PlannedLink {
            work_item_id: work_item_id.to_owned(),
            pr: pr.clone(),
            chat_id: None,
            title: title.clone(),
        })
        .collect()
}

fn chat_marker(prefix: &str, chat_id: &str, pr_key: Option<&str>) -> String {
    match pr_key {
        Some(key) => format!(
            "{prefix}{}",
            derive_id(&format!("{chat_id}\0{key}"), "pr-item-link-chat")
        ),
        None => format!("{prefix}{chat_id}"),
    }
}

/// Add one planned reference. `true` when this call created it; a live one
/// (any device) or one the user removed (tombstoned) is left alone.
pub(crate) async fn apply_link(service: &PrReviewSyncService, link: &PlannedLink) -> bool {
    let external_id = link.pr.external_id();
    let reference_id = external_reference_id(
        &link.work_item_id,
        GITHUB_PROVIDER,
        GITHUB_PULL_REQUEST_REFERENCE,
        &external_id,
    );
    match service
        .workspace()
        .registry_row_state(KIND_WORK_ITEM_EXTERNAL_REFERENCES, &reference_id)
    {
        RowState::Live => return false,
        RowState::Tombstoned => {
            tracing::debug!(pr = %external_id, "work item PR link: removed by the user, kept off");
            return false;
        }
        RowState::Absent => {}
    }
    let title: Option<String> = link
        .title
        .as_deref()
        .map(|t| {
            t.trim()
                .chars()
                .take(limits::MAX_REFERENCE_TITLE_CHARS)
                .collect()
        })
        .filter(|t: &String| !t.is_empty());
    let mutation = WorkOutlineMutation::CreateWorkItemExternalReference(
        CreateWorkItemExternalReferenceParams {
            work_item_id: link.work_item_id.clone(),
            provider: GITHUB_PROVIDER.into(),
            reference_type: GITHUB_PULL_REQUEST_REFERENCE.into(),
            external_id: external_id.clone(),
            url: link.pr.url.clone(),
            cached_title: title,
            operation_id: link_operation(&link.work_item_id, &link.pr.key()),
        },
    );
    match service.system_mutate(mutation).await {
        Ok(_) => true,
        // Linked earlier with other inputs (title): the reference exists.
        Err(error) if error.code == WorkOutlineErrorCode::OperationConflict => false,
        Err(error) => {
            tracing::warn!(code = error.code.as_str(), pr = %external_id, "work item PR link failed");
            false
        }
    }
}

/// What one Chat pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ChatLinkPass {
    /// References added.
    pub linked: u32,
    /// Transcripts re-read (backfill).
    pub scanned: u32,
    /// `created_in_chat` chat links recovered from those transcripts.
    pub recovered: u32,
}

/// The Chat pass for one Space this device owns: link each associated
/// Chat's created PRs to its item. `github/invalid` for a Space elsewhere.
pub async fn link_chat_pull_requests(
    service: &PrReviewSyncService,
    space_id: &str,
) -> Result<ChatLinkPass, mast_proto::GithubError> {
    let mut pass = ChatLinkPass::default();
    let snapshot = service.outline_snapshot(space_id).await?;
    if !snapshot.available {
        return Ok(pass);
    }
    let chats = assigned_chats(&snapshot);
    if chats.is_empty() {
        return Ok(pass);
    }
    let slugs = service.space_repo_slugs(space_id).await?;
    if slugs.is_empty() {
        return Ok(pass);
    }
    let workspace = service.workspace().clone();
    let reader = service.transcript_reader();
    // Durable links, plus the one-time transcript re-read for Chats with no
    // high-confidence link yet (bounded per pass; the rest next pass).
    let chat_links = {
        let workspace = workspace.clone();
        tokio::task::spawn_blocking(move || {
            let mut out: BTreeMap<String, Vec<ChatPrLink>> = BTreeMap::new();
            let (mut scanned, mut recovered) = (0u32, 0u32);
            for chat_id in chats {
                let mut links = workspace.chat_pr_links(&chat_id).unwrap_or_default();
                let marker = chat_marker(SCAN_MARKER_PREFIX, &chat_id, None);
                if let Some(read) = &reader
                    && !links.iter().any(|l| is_high_confidence(l.source))
                    && (scanned as usize) < MAX_TRANSCRIPT_SCANS_PER_PASS
                    && !workspace.ledger_holds(&marker)
                {
                    scanned += 1;
                    let urls = read(&chat_id)
                        .map(|e| created_pr_urls(&e))
                        .unwrap_or_default();
                    for url in &urls {
                        if workspace
                            .add_chat_pr_link(&chat_id, url, ChatLinkSource::CreatedInChat)
                            .unwrap_or(false)
                        {
                            recovered += 1;
                        }
                    }
                    if !urls.is_empty() {
                        links = workspace.chat_pr_links(&chat_id).unwrap_or_default();
                    }
                    let _ = workspace.ledger_mark(&marker);
                }
                out.insert(chat_id, links);
            }
            (out, scanned, recovered)
        })
        .await
        .map_err(|e| {
            mast_proto::GithubError::new(mast_proto::GithubErrorCode::Network, e.to_string())
        })?
    };
    let (chat_links, scanned, recovered) = chat_links;
    pass.scanned = scanned;
    pass.recovered = recovered;
    for link in plan_chat_links(&snapshot, &chat_links, &slugs) {
        let chat_id = link.chat_id.as_deref().unwrap_or_default();
        let marker = chat_marker(LINKED_MARKER_PREFIX, chat_id, Some(&link.pr.key()));
        if workspace.ledger_holds(&marker) {
            continue;
        }
        if apply_link(service, &link).await {
            pass.linked += 1;
            tracing::info!(
                space = %space_id,
                chat = %chat_id,
                pr = %link.pr.external_id(),
                "work item PR link: the item's Chat created this PR"
            );
        }
        let _ = workspace.ledger_mark(&marker);
    }
    if pass.scanned > 0 {
        tracing::info!(
            space = %space_id,
            scanned = pass.scanned,
            recovered = pass.recovered,
            "work item PR link: chat transcripts re-read for created PRs"
        );
    }
    Ok(pass)
}

/// The Linear pass for one Space's issue-linked items: `(item, identifier)`
/// pairs, with each issue's attachments fetched through `fetch`. Returns the
/// references added. Stops (logged) at the first unreadable attachment list.
pub(crate) async fn link_issue_pull_requests<F, Fut>(
    service: &PrReviewSyncService,
    snapshot: &WorkOutlineSnapshot,
    space_id: &str,
    linked: &[(String, String)],
    mut fetch: F,
) -> Result<u32, mast_proto::LinearError>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<
            Output = Result<Vec<crate::linear::queries::IssueAttachment>, mast_proto::LinearError>,
        >,
{
    use mast_proto::LinearErrorCode;
    if linked.is_empty() {
        return Ok(0);
    }
    let slugs = match service.space_repo_slugs(space_id).await {
        Ok(slugs) if !slugs.is_empty() => slugs,
        Ok(_) => return Ok(0),
        Err(error) => {
            tracing::debug!(code = error.code.as_str(), "linear sync: PR links skipped");
            return Ok(0);
        }
    };
    let mut fetched: BTreeMap<String, Vec<(PullRequestRef, Option<String>)>> = BTreeMap::new();
    let mut added = 0;
    for (work_item_id, identifier) in linked {
        if !fetched.contains_key(identifier) {
            let prs = match fetch(identifier.clone()).await {
                Ok(attachments) => attachments
                    .iter()
                    .filter_map(|a| pull_request_from_url(&a.url).map(|pr| (pr, a.title.clone())))
                    .collect(),
                Err(error)
                    if matches!(
                        error.code,
                        LinearErrorCode::MissingKey | LinearErrorCode::Unauthorized
                    ) =>
                {
                    return Err(error);
                }
                Err(error) if error.code == LinearErrorCode::NotFound => Vec::new(),
                Err(error) => {
                    tracing::warn!(
                        target: "linear",
                        code = error.code.as_str(),
                        message = %error.message,
                        "linear: issue attachments could not be read; PR links skipped this pass"
                    );
                    break;
                }
            };
            fetched.insert(identifier.clone(), prs);
        }
        let prs = &fetched[identifier];
        for link in plan_issue_links(snapshot, work_item_id, prs, &slugs) {
            if apply_link(service, &link).await {
                added += 1;
                tracing::info!(
                    target: "linear",
                    space = %space_id,
                    issue = %identifier,
                    pr = %link.pr.external_id(),
                    "linear sync: PR attached to the issue linked to its item"
                );
            }
        }
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_doc::MessageRole;
    use mast_proto::ToolCall;
    use mast_proto::work_outline::limits::MAX_OPERATION_ID_CHARS;

    #[test]
    fn urls_parse_to_pull_requests_with_their_casing() {
        let pr = pull_request_from_url("https://github.com/Cofactr/UI/pull/42/files?x=1").unwrap();
        assert_eq!(pr.external_id(), "Cofactr/UI#42");
        assert_eq!(pr.key(), "cofactr/ui#42");
        assert_eq!(repo_slug(&pr), "cofactr/ui");
        assert!(pull_request_from_url("github.com/acme/ui/pull/7").is_some());
        assert!(pull_request_from_url("https://github.com/acme/ui/issues/7").is_none());
        assert!(pull_request_from_url("https://github.com/acme/ui/pull/0").is_none());
        assert!(pull_request_from_url("https://gitlab.com/acme/ui/pull/7").is_none());
    }

    #[test]
    fn link_operations_fit_and_differ() {
        let item = "0123456789abcdef0123456789abcdef";
        let op = link_operation(item, "cofactr/receive-ai-workflows-and-tooling#123456");
        assert!(op.chars().count() <= MAX_OPERATION_ID_CHARS, "{op}");
        assert_ne!(op, link_operation(item, "acme/ui#1"));
        assert_ne!(
            op,
            link_operation("other", "cofactr/receive-ai-workflows-and-tooling#123456")
        );
    }

    fn tool(command: &str, output: Option<&str>, is_error: bool) -> MessagePart {
        MessagePart::Tool {
            id: "t".into(),
            call: ToolCall::Exec {
                command: command.into(),
            },
            is_error,
            resolved: true,
            output: output.map(str::to_owned),
            diff: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: None,
            subagent_ref: None,
            subagent_status: None,
            subagent_tail: None,
        }
    }

    fn entry(parts: Vec<MessagePart>) -> SessionMessageEntry {
        SessionMessageEntry {
            id: "m".into(),
            role: MessageRole::Assistant,
            parts,
            created_at: 0,
            device_id: "d".into(),
            status: None,
            continuation_of: None,
        }
    }

    #[test]
    fn only_pr_creation_output_counts() {
        let entries = vec![entry(vec![
            MessagePart::Text {
                id: "x".into(),
                text: "Opened https://github.com/acme/ui/pull/1".into(),
            },
            tool(
                "gh pr view 2",
                Some("https://github.com/acme/ui/pull/2"),
                false,
            ),
            tool(
                "gh pr create --fill",
                Some(
                    "Creating pull request for x into main\n\nhttps://github.com/Acme/UI/pull/3\n",
                ),
                false,
            ),
            tool(
                "gh pr create --fill",
                Some(
                    "a pull request for branch \"x\" already exists:\nhttps://github.com/acme/ui/pull/4",
                ),
                true,
            ),
            tool(
                "gh pr new",
                Some("https://github.com/acme/flagship/pull/5"),
                false,
            ),
            tool(
                "gh pr create",
                Some("https://github.com/acme/ui/pull/3"),
                false,
            ),
        ])];
        assert_eq!(
            created_pr_urls(&entries),
            vec![
                "https://github.com/acme/ui/pull/3".to_owned(),
                "https://github.com/acme/flagship/pull/5".to_owned(),
            ]
        );
    }

    #[test]
    fn every_url_of_a_create_output_counts_and_the_launcher_is_high_confidence() {
        let entries = vec![entry(vec![tool(
            "PR1=$(gh pr create --fill) && PR2=$(gh pr create --fill)",
            Some("https://github.com/Acme/UI/pull/3\nhttps://github.com/Acme/UI/pull/4\n"),
            false,
        )])];
        assert_eq!(
            created_pr_urls(&entries),
            vec![
                "https://github.com/acme/ui/pull/3".to_owned(),
                "https://github.com/acme/ui/pull/4".to_owned(),
            ]
        );
        assert!(is_high_confidence(ChatLinkSource::ReviewLauncher));
        for source in [
            ChatLinkSource::Mentioned,
            ChatLinkSource::Branch,
            ChatLinkSource::DiffTarget,
            ChatLinkSource::Ticket,
        ] {
            assert!(!is_high_confidence(source), "{source:?}");
        }
    }
}
