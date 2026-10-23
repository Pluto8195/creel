//! Chats linked to PRs for the PR page: the durable `chatPrLinks` rows plus
//! the inferred match rules (PR URL mention, chat branch, diff-target branch,
//! ticket id, review launcher), persisted with one `ChatLinkSource` per rule.
//!
//! The rules are pure functions over plain data ([`ChatFacts`] +
//! [`PrLinkCandidate`] + [`GitFacts`] -> [`Plan`]); [`WorkspaceHost::pr_chat_matches`]
//! is the thin async wrapper that gathers the facts, runs the (injectable,
//! bounded) git probes and persists the new links. Links are only ever added,
//! a stronger stored source is never downgraded, and a second pass over the
//! same data writes nothing.
//!
//! Rules (each persisted with its own source):
//! 1. A durable `chatPrLinks` row (`mentioned`, `created_in_chat`, `manual`,
//!    `review_launcher`) for the PR, compared through `canonicalize_pr_url`.
//! 2. `branch`: the chat's own branch is the PR's head branch.
//! 3. `diff_target`: the HEAD branch of the chat's selected diff-target
//!    checkout is the PR's head branch.
//! 4. `ticket`: a ticket id of the chat (its linked ticket, or the Linear
//!    identifier of its work item) is a whole token of the PR's head branch
//!    or title.
//!
//! Rules 2-4 skip archived chats and chats that already hold more than
//! [`MAX_LINKS_FOR_INFERENCE`] links (mention noise), never match a trunk
//! branch name, and check the repository when it is cheaply known.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Utc};
use zeron_proto::{Chat, ChatLinkSource, ChatPrLink, PrDashChat};

use crate::chat_links::canonicalize_pr_url;
use crate::workspace_host::WorkspaceHost;

/// Chats shown per PR.
pub const MAX_CHATS_PER_PR: usize = 8;
/// A chat holding more links than this is mention noise: no inference.
pub const MAX_LINKS_FOR_INFERENCE: usize = 20;
/// Distinct paths probed per git question per pass.
pub const MAX_GIT_PROBES: usize = 16;
const GIT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const TRUNK_BRANCHES: [&str; 4] = ["main", "master", "develop", "dev"];

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

/// Everything the rules read about one chat.
#[derive(Debug, Clone, Default)]
pub struct ChatFacts {
    pub id: String,
    pub title: Option<String>,
    pub archived: bool,
    pub updated: Option<DateTime<Utc>>,
    pub branch: Option<String>,
    pub cwd: Option<String>,
    pub repo_root: Option<String>,
    /// Ticket ids of the chat: its linked ticket and its work item's Linear
    /// identifiers.
    pub tickets: Vec<String>,
    /// Path of the selected diff-target checkout, if any.
    pub diff_target: Option<String>,
    /// Durable PR links (effective: rows merged with the legacy slot).
    pub links: Vec<ChatPrLink>,
}

impl ChatFacts {
    fn inference_eligible(&self) -> bool {
        !self.archived && self.links.len() <= MAX_LINKS_FOR_INFERENCE
    }
}

/// Answers of the git probes, keyed by checkout path. A missing key or a
/// `None` value means "unknown".
#[derive(Debug, Clone, Default)]
pub struct GitFacts {
    /// Current HEAD branch (`None` = detached, unreadable or not probed).
    pub heads: HashMap<String, Option<String>>,
    /// Lowercase `owner/name` of the checkout's `origin`.
    pub slugs: HashMap<String, Option<String>>,
}

/// One link the pass wants stored (rules 2-4 only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedLink {
    pub chat_id: String,
    /// `PrLinkCandidate::url` of the PR.
    pub url: String,
    pub source: ChatLinkSource,
}

/// The pure result of a pass.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// Linked chats per `PrLinkCandidate::url`, best first, capped.
    pub by_url: HashMap<String, Vec<PrDashChat>>,
    /// Inferred links not stored yet (or stored with a weaker source).
    pub new_links: Vec<ProposedLink>,
}

/// A usable (non-empty, non-trunk) branch name.
pub fn usable_branch(branch: Option<&str>) -> Option<&str> {
    let branch = branch?.trim();
    (!branch.is_empty()
        && !TRUNK_BRANCHES
            .iter()
            .any(|t| t.eq_ignore_ascii_case(branch)))
    .then_some(branch)
}

/// `token` occurs in `haystack` as a whole token, case-insensitively: the
/// characters around the match are not alphanumeric (`ENG-3012` is in
/// `michael/eng-3012-kit` but not in `ENG-30121` or `XENG-3012`).
pub fn contains_token(haystack: &str, token: &str) -> bool {
    let token = token.trim().to_ascii_lowercase();
    if token.is_empty() {
        return false;
    }
    let haystack = haystack.to_ascii_lowercase();
    let mut from = 0;
    while let Some(rel) = haystack[from..].find(&token) {
        let start = from + rel;
        let end = start + token.len();
        let before_ok = haystack[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        let after_ok = haystack[end..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

/// Diff-target checkouts whose HEAD must be probed: the selected target of
/// each inference-eligible chat, newest chat first, bounded.
pub fn heads_needed(chats: &[ChatFacts], candidates: &[PrLinkCandidate]) -> Vec<String> {
    if candidates.iter().all(|c| c.head_ref.trim().is_empty()) {
        return Vec::new();
    }
    let mut eligible: Vec<&ChatFacts> = chats
        .iter()
        .filter(|c| c.inference_eligible() && c.diff_target.is_some())
        .collect();
    eligible.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
    let mut seen = HashSet::new();
    eligible
        .into_iter()
        .filter_map(|c| c.diff_target.clone())
        .filter(|path| seen.insert(path.clone()))
        .take(MAX_GIT_PROBES)
        .collect()
}

/// Checkouts whose `origin` is worth asking about: only those of chats whose
/// branch or diff-target HEAD already equals some candidate's head branch.
pub fn slugs_needed(
    chats: &[ChatFacts],
    candidates: &[PrLinkCandidate],
    git: &GitFacts,
) -> Vec<String> {
    let heads: HashSet<&str> = candidates
        .iter()
        .map(|c| c.head_ref.trim())
        .filter(|h| !h.is_empty())
        .collect();
    let mut paths: Vec<String> = Vec::new();
    for chat in chats.iter().filter(|c| c.inference_eligible()) {
        if usable_branch(chat.branch.as_deref()).is_some_and(|b| heads.contains(b)) {
            paths.extend(chat.cwd.iter().chain(chat.repo_root.iter()).cloned());
        }
        if let Some(path) = &chat.diff_target
            && let Some(Some(head)) = git.heads.get(path)
            && usable_branch(Some(head)).is_some_and(|h| heads.contains(h))
        {
            paths.push(path.clone());
        }
    }
    let mut seen = HashSet::new();
    paths.retain(|p| seen.insert(p.clone()));
    paths.truncate(MAX_GIT_PROBES);
    paths
}

/// Whether the PR's repository agrees with the checkouts at `paths`: true
/// when no slug is known for any of them (skip the check), else when one of
/// the known slugs is the PR's.
fn repo_agrees(paths: &[&str], git: &GitFacts, repo_full: &str) -> bool {
    let wanted = repo_full.trim().to_ascii_lowercase();
    let known: Vec<&str> = paths
        .iter()
        .filter_map(|p| git.slugs.get(*p).and_then(|s| s.as_deref()))
        .collect();
    known.is_empty() || wanted.is_empty() || known.iter().any(|slug| *slug == wanted)
}

/// Run every rule: the chats linked to each candidate (best first, capped)
/// and the inferred links to persist.
pub fn plan_matches(chats: &[ChatFacts], candidates: &[PrLinkCandidate], git: &GitFacts) -> Plan {
    struct Hit<'a> {
        chat: &'a ChatFacts,
        source: ChatLinkSource,
        /// Best inferred source whose rank beats the stored link (if any).
        proposal: Option<ChatLinkSource>,
    }
    let mut plan = Plan::default();
    for candidate in candidates {
        let canonical = canonicalize_pr_url(&candidate.url);
        let head = candidate.head_ref.trim();
        let mut hits: Vec<Hit> = Vec::new();
        for chat in chats {
            // Rule 1: a durable row.
            let stored = chat
                .links
                .iter()
                .filter(|l| canonicalize_pr_url(&l.url) == canonical)
                .map(|l| l.source)
                .max_by_key(|s| s.rank());
            // Rules 2-4.
            let mut inferred: Option<ChatLinkSource> = None;
            let mut consider = |source: ChatLinkSource| {
                if inferred.is_none_or(|current| source.rank() > current.rank()) {
                    inferred = Some(source);
                }
            };
            if chat.inference_eligible() && !head.is_empty() {
                // Rule 2: the chat's own branch.
                if usable_branch(chat.branch.as_deref()) == Some(head) {
                    let paths: Vec<&str> = chat
                        .cwd
                        .iter()
                        .chain(chat.repo_root.iter())
                        .map(String::as_str)
                        .collect();
                    if repo_agrees(&paths, git, &candidate.repo_full) {
                        consider(ChatLinkSource::Branch);
                    }
                }
                // Rule 3: the diff-target checkout's HEAD.
                if let Some(path) = &chat.diff_target
                    && let Some(Some(checked_out)) = git.heads.get(path)
                    && usable_branch(Some(checked_out)) == Some(head)
                    && repo_agrees(&[path.as_str()], git, &candidate.repo_full)
                {
                    consider(ChatLinkSource::DiffTarget);
                }
            }
            // Rule 4: a ticket id of the chat in the PR's branch or title.
            if chat.inference_eligible()
                && chat.tickets.iter().any(|ticket| {
                    contains_token(&candidate.head_ref, ticket)
                        || contains_token(&candidate.title, ticket)
                })
            {
                consider(ChatLinkSource::Ticket);
            }
            let best = match (stored, inferred) {
                (None, None) => continue,
                (Some(s), None) => s,
                (None, Some(i)) => i,
                (Some(s), Some(i)) => {
                    if i.rank() > s.rank() {
                        i
                    } else {
                        s
                    }
                }
            };
            let proposal =
                inferred.filter(|i| stored.is_none_or(|s| i.rank() > s.rank()) && *i == best);
            hits.push(Hit {
                chat,
                source: best,
                proposal,
            });
        }
        // Live before archived, strongest source, newest, then id.
        hits.sort_by(|a, b| {
            a.chat
                .archived
                .cmp(&b.chat.archived)
                .then_with(|| b.source.rank().cmp(&a.source.rank()))
                .then_with(|| b.chat.updated.cmp(&a.chat.updated))
                .then_with(|| a.chat.id.cmp(&b.chat.id))
        });
        hits.truncate(MAX_CHATS_PER_PR);
        for hit in &hits {
            if let Some(source) = hit.proposal {
                plan.new_links.push(ProposedLink {
                    chat_id: hit.chat.id.clone(),
                    url: candidate.url.clone(),
                    source,
                });
            }
        }
        plan.by_url.insert(
            candidate.url.clone(),
            hits.iter()
                .map(|hit| PrDashChat {
                    chat_id: hit.chat.id.clone(),
                    title: hit
                        .chat
                        .title
                        .as_deref()
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .unwrap_or("Untitled chat")
                        .to_owned(),
                    source: hit.source,
                    archived: hit.chat.archived,
                    is_review: false,
                    updated_at: hit.chat.updated.map(|at| at.to_rfc3339()),
                })
                .collect(),
        );
    }
    plan
}

/// The git questions the matcher asks. Production shells out through
/// `zeron_harness::git_command_async`; tests answer from fixtures.
pub trait GitProbe: Send + Sync {
    /// Current HEAD branch of the checkout at `path` (`None` = detached or
    /// unreadable).
    fn head_branch(&self, path: &Path) -> impl Future<Output = Option<String>> + Send;
    /// Lowercase `owner/name` of the checkout's `origin` remote.
    fn origin_slug(&self, path: &Path) -> impl Future<Output = Option<String>> + Send;
}

/// [`GitProbe`] over the `git` CLI.
pub struct GitCliProbe;

async fn git_stdout(path: &Path, args: &[&str]) -> Option<String> {
    let mut command = zeron_harness::git_command_async();
    command
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(GIT_PROBE_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

impl GitProbe for GitCliProbe {
    async fn head_branch(&self, path: &Path) -> Option<String> {
        let head = git_stdout(path, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
        (head != "HEAD").then_some(head)
    }

    async fn origin_slug(&self, path: &Path) -> Option<String> {
        let url = git_stdout(path, &["config", "--get", "remote.origin.url"]).await?;
        crate::pr_review_launch::github_slug(&url)
    }
}

/// `path` is an absolute directory that exists (only local checkouts are
/// probed).
fn probeable(path: &str) -> bool {
    let path = Path::new(path);
    path.is_absolute() && path.is_dir()
}

impl WorkspaceHost {
    /// Run every match rule for `candidates`, persist new links, and return
    /// the linked chats per candidate keyed by `candidate.url` (best first:
    /// live before archived, strongest source, newest first; at most
    /// [`MAX_CHATS_PER_PR`]).
    pub async fn pr_chat_matches(
        &self,
        candidates: &[PrLinkCandidate],
    ) -> HashMap<String, Vec<PrDashChat>> {
        self.pr_chat_matches_with(candidates, &GitCliProbe).await
    }

    pub(crate) async fn pr_chat_matches_with<P: GitProbe>(
        &self,
        candidates: &[PrLinkCandidate],
        probe: &P,
    ) -> HashMap<String, Vec<PrDashChat>> {
        if candidates.is_empty() {
            return HashMap::new();
        }
        let chats = match self.pr_chat_facts() {
            Ok(chats) => chats,
            Err(error) => {
                tracing::warn!(%error, "PR chat matching could not read chats");
                return HashMap::new();
            }
        };

        let mut git = GitFacts::default();
        for path in heads_needed(&chats, candidates) {
            let head = if probeable(&path) {
                probe.head_branch(Path::new(&path)).await
            } else {
                None
            };
            git.heads.insert(path, head);
        }
        for path in slugs_needed(&chats, candidates, &git) {
            let slug = if probeable(&path) {
                probe.origin_slug(Path::new(&path)).await
            } else {
                None
            };
            git.slugs.insert(path, slug);
        }

        let mut plan = plan_matches(&chats, candidates, &git);
        for link in &plan.new_links {
            if let Err(error) = self.add_chat_pr_link(&link.chat_id, &link.url, link.source) {
                tracing::warn!(chat = %link.chat_id, %error, "inferred PR link write failed");
            }
        }
        let store_root = self.store_root();
        for chat in plan.by_url.values_mut().flatten() {
            chat.is_review =
                crate::external_import::category_in_store(store_root.clone(), &chat.chat_id)
                    .is_some_and(|category| category == "pr_review");
        }
        plan.by_url
    }

    /// The facts of every chat, from the registry (no git, no network).
    fn pr_chat_facts(&self) -> Result<Vec<ChatFacts>, crate::EngineError> {
        let chats = self.read_chats()?;
        let diff_targets: HashMap<String, String> = self
            .all_chat_diff_targets()?
            .into_iter()
            .filter_map(|row| row.selected.map(|path| (row.chat_id, path)))
            .collect();
        let items = self.linear_identifiers_by_chat(&chats);
        Ok(chats
            .into_iter()
            .map(|chat| {
                let links = self.chat_pr_links(&chat.id).unwrap_or_default();
                facts_of(&chat, links, diff_targets.get(&chat.id).cloned(), &items)
            })
            .collect())
    }

    /// Linear issue identifiers of the work item each chat is assigned to,
    /// read per Space from the Work Outline rows.
    fn linear_identifiers_by_chat(&self, chats: &[Chat]) -> HashMap<String, Vec<String>> {
        let spaces: HashSet<&str> = chats
            .iter()
            .filter(|c| !c.archived)
            .filter_map(|c| c.space_id.as_deref())
            .collect();
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        for space_id in spaces {
            let inputs = self.work_outline_inputs(space_id);
            let text = |row: &zeron_doc::RegistryRow, key: &str| {
                row.fields
                    .get(key)
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            };
            let mut by_item: HashMap<String, Vec<String>> = HashMap::new();
            for row in &inputs.references {
                if text(row, "provider").as_deref() == Some(zeron_proto::LINEAR_PROVIDER)
                    && text(row, "refType").as_deref() == Some(zeron_proto::LINEAR_ISSUE_REFERENCE)
                    && let (Some(item), Some(identifier)) =
                        (text(row, "workItemId"), text(row, "externalId"))
                    && !identifier.trim().is_empty()
                {
                    by_item.entry(item).or_default().push(identifier);
                }
            }
            for row in &inputs.assignments {
                if let Some(item) = text(row, "workItemId").filter(|i| !i.is_empty())
                    && let Some(identifiers) = by_item.get(&item)
                {
                    out.insert(row.id.clone(), identifiers.clone());
                }
            }
        }
        out
    }
}

fn facts_of(
    chat: &Chat,
    links: Vec<ChatPrLink>,
    diff_target: Option<String>,
    items: &HashMap<String, Vec<String>>,
) -> ChatFacts {
    let mut tickets: Vec<String> = chat.linked_ticket_id.iter().cloned().collect();
    if let Some(identifiers) = items.get(&chat.id) {
        tickets.extend(identifiers.iter().cloned());
    }
    ChatFacts {
        id: chat.id.clone(),
        title: chat.title.clone(),
        archived: chat.archived,
        updated: Some(chat.last_message_at.unwrap_or(chat.created_at)),
        branch: chat.branch.clone(),
        cwd: chat.cwd.clone(),
        repo_root: chat
            .source_context
            .as_ref()
            .map(|context| context.repo_root.clone()),
        tickets,
        diff_target,
        links,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLAGSHIP: &str = "https://github.com/Cofactr/flagship/pull";

    fn candidate(number: u64, head: &str, title: &str) -> PrLinkCandidate {
        PrLinkCandidate {
            url: format!("{FLAGSHIP}/{number}"),
            number,
            repo_full: "Cofactr/flagship".into(),
            head_ref: head.into(),
            title: title.into(),
        }
    }

    fn link(number: u64, source: ChatLinkSource) -> ChatPrLink {
        ChatPrLink {
            url: format!("https://github.com/cofactr/flagship/pull/{number}"),
            source,
        }
    }

    fn chat(id: &str) -> ChatFacts {
        ChatFacts {
            id: id.into(),
            title: Some(format!("chat {id}")),
            updated: DateTime::from_timestamp(1_000, 0),
            ..Default::default()
        }
    }

    fn git_head(path: &str, head: Option<&str>) -> GitFacts {
        let mut git = GitFacts::default();
        git.heads.insert(path.into(), head.map(str::to_owned));
        git
    }

    fn sources(plan: &Plan, url: &str) -> Vec<(String, ChatLinkSource)> {
        plan.by_url[url]
            .iter()
            .map(|c| (c.chat_id.clone(), c.source))
            .collect()
    }

    #[test]
    fn rule_1_matches_stored_links_case_insensitively() {
        let mut c = chat("a");
        c.links = vec![link(6487, ChatLinkSource::CreatedInChat)];
        let cand = candidate(6487, "x", "t");
        let plan = plan_matches(&[c], &[cand.clone()], &GitFacts::default());
        assert_eq!(
            sources(&plan, &cand.url),
            vec![("a".into(), ChatLinkSource::CreatedInChat)]
        );
        assert!(plan.new_links.is_empty(), "stored links are not rewritten");
    }

    #[test]
    fn rule_2_branch_matches_head_ref_but_never_a_trunk_name() {
        let mut c = chat("a");
        c.branch = Some("michael/eng-1-thing".into());
        let hit = candidate(1, "michael/eng-1-thing", "");
        let miss = candidate(2, "main", "");
        let plan = plan_matches(
            &[c.clone()],
            &[hit.clone(), miss.clone()],
            &GitFacts::default(),
        );
        assert_eq!(
            sources(&plan, &hit.url),
            vec![("a".into(), ChatLinkSource::Branch)]
        );
        assert_eq!(
            plan.new_links,
            vec![ProposedLink {
                chat_id: "a".into(),
                url: hit.url.clone(),
                source: ChatLinkSource::Branch
            }]
        );

        for trunk in ["main", "master", "develop", "dev", " ", ""] {
            let mut c = chat("t");
            c.branch = Some(trunk.into());
            let cand = candidate(3, trunk.trim(), "");
            let plan = plan_matches(&[c], &[cand.clone()], &GitFacts::default());
            assert!(plan.by_url[&cand.url].is_empty(), "{trunk:?}");
        }
    }

    #[test]
    fn rule_2_skips_a_chat_in_another_repository_when_known() {
        let mut c = chat("a");
        c.branch = Some("feature-x".into());
        c.cwd = Some("/work/other".into());
        let cand = candidate(1, "feature-x", "");
        let mut git = GitFacts::default();
        git.slugs
            .insert("/work/other".into(), Some("acme/other".into()));
        let plan = plan_matches(&[c.clone()], &[cand.clone()], &git);
        assert!(plan.by_url[&cand.url].is_empty());

        git.slugs
            .insert("/work/other".into(), Some("cofactr/flagship".into()));
        let plan = plan_matches(&[c.clone()], &[cand.clone()], &git);
        assert_eq!(plan.by_url[&cand.url].len(), 1);

        // Unknown repository: the check is skipped.
        let plan = plan_matches(&[c], &[cand.clone()], &GitFacts::default());
        assert_eq!(plan.by_url[&cand.url].len(), 1);
    }

    #[test]
    fn rule_3_diff_target_head_matches_and_detached_head_does_not() {
        let mut c = chat("a");
        c.diff_target = Some("/ws/flagship".into());
        let cand = candidate(6488, "michael/eng-3012-kit-line", "");
        let plan = plan_matches(
            &[c.clone()],
            &[cand.clone()],
            &git_head("/ws/flagship", Some("michael/eng-3012-kit-line")),
        );
        assert_eq!(
            sources(&plan, &cand.url),
            vec![("a".into(), ChatLinkSource::DiffTarget)]
        );
        for head in [None, Some("main"), Some("other")] {
            let plan = plan_matches(
                &[c.clone()],
                &[cand.clone()],
                &git_head("/ws/flagship", head),
            );
            assert!(plan.by_url[&cand.url].is_empty(), "{head:?}");
        }
        // A trunk head never matches even a trunk-named head_ref.
        let trunk = candidate(9, "main", "");
        let plan = plan_matches(
            &[c],
            &[trunk.clone()],
            &git_head("/ws/flagship", Some("main")),
        );
        assert!(plan.by_url[&trunk.url].is_empty());
    }

    #[test]
    fn rule_4_ticket_must_be_a_whole_token_in_head_ref_or_title() {
        assert!(contains_token("michael/eng-3012-kit", "ENG-3012"));
        assert!(contains_token("Fix ENG-3012: kit", "eng-3012"));
        assert!(contains_token("ENG-3012", "ENG-3012"));
        assert!(!contains_token("michael/eng-30121-kit", "ENG-3012"));
        assert!(!contains_token("xeng-3012", "ENG-3012"));
        assert!(!contains_token("anything", ""));

        let mut c = chat("a");
        c.tickets = vec!["ENG-3012".into()];
        let near = candidate(1, "michael/eng-30121-other", "ENG-30121 other");
        let by_branch = candidate(2, "michael/eng-3012-kit", "no id");
        let by_title = candidate(3, "plain", "ENG-3012 kit");
        let plan = plan_matches(
            &[c],
            &[near.clone(), by_branch.clone(), by_title.clone()],
            &GitFacts::default(),
        );
        assert!(plan.by_url[&near.url].is_empty());
        assert_eq!(
            plan.by_url[&by_branch.url][0].source,
            ChatLinkSource::Ticket
        );
        assert_eq!(plan.by_url[&by_title.url][0].source, ChatLinkSource::Ticket);
    }

    #[test]
    fn archived_and_noisy_chats_get_no_inference_but_keep_stored_links() {
        let cand = candidate(1, "feat/eng-1-x", "ENG-1");
        let mut archived = chat("arch");
        archived.archived = true;
        archived.branch = Some("feat/eng-1-x".into());
        archived.tickets = vec!["ENG-1".into()];
        let mut noisy = chat("noisy");
        noisy.tickets = vec!["ENG-1".into()];
        noisy.links = (100..100 + MAX_LINKS_FOR_INFERENCE as u64 + 1)
            .map(|n| link(n, ChatLinkSource::Mentioned))
            .collect();
        let mut stored = chat("stored");
        stored.archived = true;
        stored.links = vec![link(1, ChatLinkSource::Mentioned)];
        let plan = plan_matches(
            &[archived, noisy, stored],
            &[cand.clone()],
            &GitFacts::default(),
        );
        assert_eq!(
            sources(&plan, &cand.url),
            vec![("stored".into(), ChatLinkSource::Mentioned)]
        );
        assert!(plan.new_links.is_empty());
    }

    #[test]
    fn strongest_source_wins_and_only_upgrades_are_proposed() {
        let cand = candidate(1, "feat/eng-1-x", "ENG-1");
        // Stored created_in_chat beats a ticket inference: nothing to write.
        let mut strong = chat("strong");
        strong.links = vec![link(1, ChatLinkSource::CreatedInChat)];
        strong.tickets = vec!["ENG-1".into()];
        // Stored mentioned is upgraded by a ticket inference.
        let mut weak = chat("weak");
        weak.links = vec![link(1, ChatLinkSource::Mentioned)];
        weak.tickets = vec!["ENG-1".into()];
        // Branch and ticket both match: branch (stronger) wins.
        let mut both = chat("both");
        both.branch = Some("feat/eng-1-x".into());
        both.tickets = vec!["ENG-1".into()];
        let plan = plan_matches(&[strong, weak, both], &[cand.clone()], &GitFacts::default());
        let got = sources(&plan, &cand.url);
        assert_eq!(
            got,
            vec![
                ("strong".into(), ChatLinkSource::CreatedInChat),
                ("both".into(), ChatLinkSource::Branch),
                ("weak".into(), ChatLinkSource::Ticket),
            ]
        );
        let mut written: Vec<_> = plan
            .new_links
            .iter()
            .map(|l| (l.chat_id.as_str(), l.source))
            .collect();
        written.sort_by_key(|(id, _)| *id);
        assert_eq!(
            written,
            vec![
                ("both", ChatLinkSource::Branch),
                ("weak", ChatLinkSource::Ticket)
            ]
        );
    }

    #[test]
    fn ordering_is_live_first_then_rank_then_newest_and_capped() {
        let cand = candidate(1, "x", "t");
        let mut chats = Vec::new();
        let mut archived = chat("archived-manual");
        archived.archived = true;
        archived.links = vec![link(1, ChatLinkSource::Manual)];
        chats.push(archived);
        for (id, source, ts) in [
            ("mentioned-new", ChatLinkSource::Mentioned, 900),
            ("created-old", ChatLinkSource::CreatedInChat, 100),
            ("created-new", ChatLinkSource::CreatedInChat, 500),
            ("launcher", ChatLinkSource::ReviewLauncher, 50),
        ] {
            let mut c = chat(id);
            c.updated = DateTime::from_timestamp(ts, 0);
            c.links = vec![link(1, source)];
            chats.push(c);
        }
        let plan = plan_matches(&chats, &[cand.clone()], &GitFacts::default());
        let order: Vec<_> = plan.by_url[&cand.url]
            .iter()
            .map(|c| c.chat_id.as_str())
            .collect();
        assert_eq!(
            order,
            vec![
                "launcher",
                "created-new",
                "created-old",
                "mentioned-new",
                "archived-manual"
            ]
        );
        assert!(plan.by_url[&cand.url][4].archived);

        let many: Vec<ChatFacts> = (0..20)
            .map(|i| {
                let mut c = chat(&format!("c{i:02}"));
                c.links = vec![link(1, ChatLinkSource::Mentioned)];
                c
            })
            .collect();
        let plan = plan_matches(&many, &[cand.clone()], &GitFacts::default());
        assert_eq!(plan.by_url[&cand.url].len(), MAX_CHATS_PER_PR);
    }

    #[test]
    fn untitled_chats_get_a_fallback_title() {
        let mut c = chat("a");
        c.title = Some("  ".into());
        c.links = vec![link(1, ChatLinkSource::Mentioned)];
        let cand = candidate(1, "x", "t");
        let plan = plan_matches(&[c], &[cand.clone()], &GitFacts::default());
        assert_eq!(plan.by_url[&cand.url][0].title, "Untitled chat");
        assert!(plan.by_url[&cand.url][0].updated_at.is_some());
    }

    #[test]
    fn probe_requests_are_bounded_and_targeted() {
        let cand = candidate(1, "feat/x", "t");
        let chats: Vec<ChatFacts> = (0..40)
            .map(|i| {
                let mut c = chat(&format!("c{i}"));
                c.diff_target = Some(format!("/ws/repo{i}"));
                c
            })
            .collect();
        assert_eq!(heads_needed(&chats, &[cand.clone()]).len(), MAX_GIT_PROBES);
        // No head_ref anywhere: nothing to probe.
        assert!(heads_needed(&chats, &[candidate(2, "", "t")]).is_empty());
        // Slugs are only asked about checkouts whose branch/HEAD matched.
        let mut matching = chat("m");
        matching.branch = Some("feat/x".into());
        matching.cwd = Some("/ws/m".into());
        let mut other = chat("o");
        other.branch = Some("feat/other".into());
        other.cwd = Some("/ws/o".into());
        assert_eq!(
            slugs_needed(&[matching, other], &[cand], &GitFacts::default()),
            vec!["/ws/m".to_owned()]
        );
    }

    // ── host-level: the ENG-3012-shaped chat, persisted ─────────────────────

    use std::sync::Arc;
    use zeron_proto::{
        AssignChatToWorkItemParams, ChatDiffTargetSource, CreateWorkItemExternalReferenceParams,
        CreateWorkItemParams, StoredChatDiffTarget, WorkOutlineActor, WorkOutlineActorKind,
        WorkOutlineMutation,
    };
    use zeron_sync::DocsStore;

    use crate::workspace_host::{ChatLinkKind, WorkspaceHostConfig};

    struct FakeGit {
        heads: HashMap<String, Option<String>>,
        slugs: HashMap<String, Option<String>>,
        head_calls: std::sync::atomic::AtomicUsize,
    }

    impl GitProbe for FakeGit {
        async fn head_branch(&self, path: &Path) -> Option<String> {
            self.head_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.heads.get(path.to_str()?).cloned().flatten()
        }
        async fn origin_slug(&self, path: &Path) -> Option<String> {
            self.slugs.get(path.to_str()?).cloned().flatten()
        }
    }

    fn host() -> (tempfile::TempDir, WorkspaceHost) {
        let dir = tempfile::tempdir().unwrap();
        let host = WorkspaceHost::open(
            Arc::new(DocsStore::open(dir.path()).unwrap()),
            WorkspaceHostConfig {
                device_id: "dev".into(),
                device_name: "Dev".into(),
                platform: "macos".into(),
                org_id: "org".into(),
                user_id: "user".into(),
                edge: None,
            },
        )
        .unwrap();
        (dir, host)
    }

    fn actor() -> WorkOutlineActor {
        WorkOutlineActor {
            kind: WorkOutlineActorKind::User,
            device_id: "dev".into(),
            chat_id: None,
            message_id: None,
        }
    }

    #[tokio::test]
    async fn the_eng_3012_chat_links_both_prs_and_persists_the_inferred_sources() {
        let (dir, host) = host();
        let checkout = dir.path().join("workspace").join("flagship");
        std::fs::create_dir_all(&checkout).unwrap();
        let checkout = checkout.to_string_lossy().into_owned();
        let root = dir.path().join("workspace").to_string_lossy().into_owned();

        host.create_space("s1", "dev", &root, None, true).unwrap();
        host.create_chat("c1", Some("s1"), Some("dev"), None, Some(root.clone()))
            .unwrap();
        host.rename_chat("c1", "Begin workspace environment initialization")
            .unwrap();
        host.set_chat_branch("c1", "main").unwrap();
        host.set_chat_link(
            "c1",
            ChatLinkKind::Ticket,
            Some("ENG-3103"),
            ChatLinkSource::Mentioned,
        )
        .unwrap();
        host.add_chat_pr_link(
            "c1",
            "https://github.com/cofactr/flagship/pull/6487",
            ChatLinkSource::CreatedInChat,
        )
        .unwrap();
        host.add_chat_pr_link(
            "c1",
            "https://github.com/cofactr/flagship/pull/6488",
            ChatLinkSource::Mentioned,
        )
        .unwrap();
        host.update_chat_diff_targets("c1", |row| {
            row.targets.push(StoredChatDiffTarget {
                path: checkout.clone(),
                source: ChatDiffTargetSource::Manual,
                repo_id: "repo:x".into(),
                worktree_id: "wt:x".into(),
                bound_at: 1,
            });
            row.selected = Some(checkout.clone());
        })
        .unwrap();

        // The chat's work item is ENG-3012 (a Linear issue reference).
        let created = host
            .work_outline_mutate(
                &WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
                    space_id: "s1".into(),
                    parent_work_item_id: None,
                    placement: Default::default(),
                    title: "Kit line estimates".into(),
                    description: None,
                    status: None,
                    operation_id: "op-item".into(),
                }),
                &actor(),
                &[],
            )
            .unwrap();
        let item = created.work_item_id.unwrap();
        host.work_outline_mutate(
            &WorkOutlineMutation::CreateWorkItemExternalReference(
                CreateWorkItemExternalReferenceParams {
                    work_item_id: item.clone(),
                    provider: "linear".into(),
                    reference_type: "issue".into(),
                    external_id: "ENG-3012".into(),
                    url: "https://linear.app/cofactr/issue/ENG-3012".into(),
                    cached_title: None,
                    operation_id: "op-ref".into(),
                },
            ),
            &actor(),
            &[],
        )
        .unwrap();
        host.work_outline_mutate(
            &WorkOutlineMutation::AssignChatToWorkItem(AssignChatToWorkItemParams {
                chat_id: "c1".into(),
                work_item_id: item,
                operation_id: "op-assign".into(),
            }),
            &actor(),
            &[],
        )
        .unwrap();

        let candidates = vec![
            PrLinkCandidate {
                url: "https://github.com/Cofactr/flagship/pull/6487".into(),
                number: 6487,
                repo_full: "Cofactr/flagship".into(),
                head_ref: "michael/eng-3012-kit-ship-estimates".into(),
                title: "Ship estimates".into(),
            },
            PrLinkCandidate {
                url: "https://github.com/Cofactr/flagship/pull/6488".into(),
                number: 6488,
                repo_full: "Cofactr/flagship".into(),
                head_ref: "michael/eng-3012-kit-line-estimate-calculation".into(),
                title: "Calculate kit line ship estimates".into(),
            },
            PrLinkCandidate {
                url: "https://github.com/Cofactr/flagship/pull/1".into(),
                number: 1,
                repo_full: "Cofactr/flagship".into(),
                head_ref: "someone/eng-30121-unrelated".into(),
                title: "ENG-30121 unrelated".into(),
            },
        ];
        let probe = FakeGit {
            heads: HashMap::from([(
                checkout.clone(),
                Some("michael/eng-3012-kit-line-estimate-calculation".to_owned()),
            )]),
            slugs: HashMap::from([(checkout.clone(), Some("cofactr/flagship".to_owned()))]),
            head_calls: Default::default(),
        };

        let matches = host.pr_chat_matches_with(&candidates, &probe).await;
        let first = &matches[&candidates[0].url];
        let second = &matches[&candidates[1].url];
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].chat_id, "c1");
        assert_eq!(first[0].title, "Begin workspace environment initialization");
        // Created in the chat stays the strongest source for #6487.
        assert_eq!(first[0].source, ChatLinkSource::CreatedInChat);
        // #6488 was only `mentioned`; the diff target's HEAD links it.
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].source, ChatLinkSource::DiffTarget);
        assert!(matches[&candidates[2].url].is_empty());

        let stored = host.chat_pr_links("c1").unwrap();
        let source_of = |n: u64| {
            stored
                .iter()
                .find(|l| l.url.ends_with(&format!("/{n}")))
                .map(|l| l.source)
        };
        assert_eq!(source_of(6487), Some(ChatLinkSource::CreatedInChat));
        assert_eq!(source_of(6488), Some(ChatLinkSource::DiffTarget));
        assert_eq!(stored.len(), 2, "no link for the unrelated PR");

        // Idempotent: nothing is written the second time.
        let written = host.registry_batches_written();
        let again = host.pr_chat_matches_with(&candidates, &probe).await;
        assert_eq!(
            again[&candidates[1].url][0].source,
            ChatLinkSource::DiffTarget
        );
        assert_eq!(host.registry_batches_written(), written);

        // Without a diff target the item's Linear identifier (not the
        // mentioned ENG-3103) still links #6488 by ticket.
        host.update_chat_diff_targets("c1", |row| row.selected = None)
            .unwrap();
        host.clear_chat_pr_links("c1").unwrap();
        host.add_chat_pr_link(
            "c1",
            "https://github.com/cofactr/flagship/pull/6488",
            ChatLinkSource::Mentioned,
        )
        .unwrap();
        let matches = host.pr_chat_matches_with(&candidates, &probe).await;
        assert_eq!(
            matches[&candidates[1].url][0].source,
            ChatLinkSource::Ticket
        );
        assert_eq!(
            matches[&candidates[0].url][0].source,
            ChatLinkSource::Ticket
        );
    }
}
