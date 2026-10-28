//! Branch / PR range targets ("Diff branch / PR…", contract § "Diff a branch or
//! PR without a checkout"): `merge-base(base, head)...head` read from a
//! repository's refs, no checkout needed.
//!
//! The target is stored beside the checkout targets, on the same
//! `chatDiffTargets` row (`ref_targets` / `selected_ref`). Binding one:
//!
//! 1. parses the input (PR URL, PR number, or branch name);
//! 2. picks the repository among the chat's checkout and its submodules (by
//!    the PR URL's `owner/repo` against `remote.origin.url`, else by where the
//!    branch exists);
//! 3. fetches the head and base refs read-only
//!    (`git fetch origin +refs/heads/<b>:refs/remotes/origin/<b>`, as the PR
//!    launcher does; a PR whose branch is not fetchable falls back to
//!    `refs/pull/<n>/head`). Nothing is pushed and no remote state changes;
//!    only remote-tracking refs of the local repository move;
//! 4. stores fully qualified refs, so the diff itself needs no lookups.
//!
//! The PR's base and head branch come from `gh pr view` (the same fakeable
//! tool the PR launcher uses); without it the base defaults to `origin/main`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::Deserialize;
use mast_proto::{
    Chat, ChatDiffTargetsReply, DiffCheckoutKind, ListChatRefDiffCandidatesReply,
    MAX_CHAT_REF_TARGETS, RefDiffCandidate, RefDiffKind, StoredRefDiffTarget,
};

use super::{
    DiffTargetError, DiffTargets, Family, canonical_lenient, checkout_facts, git_text, hex,
    internal, path_string, repository_id,
};
use crate::chat_links::canonicalize_pr_url;
use crate::pr_review_launch::github_slug;
use crate::pr_ticket_cache::extract_ticket_id;

/// Longest a single `git fetch` may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
/// Most branch candidates one listing returns.
const MAX_BRANCH_CANDIDATES: usize = 30;
const TRUNK_BRANCHES: [&str; 4] = ["main", "master", "develop", "dev"];

/// What [`DiffTargets::bind_ref`] stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefBindOutcome {
    pub reply: ChatDiffTargetsReply,
    pub target: StoredRefDiffTarget,
}

/// A repository checkout of the chat's family (its own checkout or a
/// submodule), the place ref targets are read from.
#[derive(Debug, Clone)]
struct RepoCheckout {
    path: PathBuf,
    name: String,
    /// Lowercase `owner/name` of `remote.origin.url` when it is a GitHub URL.
    slug: Option<String>,
    repo_id: String,
}

/// A parsed "Diff branch / PR…" input.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RefInput {
    /// A PR URL: repository slug (lowercase) and number.
    PrUrl {
        slug: String,
        number: u64,
    },
    /// A bare PR number (`6487`, `#6487`).
    PrNumber(u64),
    Branch(String),
}

fn parse_input(input: &str) -> Result<RefInput, DiffTargetError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(DiffTargetError::Invalid(
            "Enter a PR number, a PR URL, or a branch name.".into(),
        ));
    }
    if trimmed.len() > 512 || trimmed.contains(['\0', '\n', '\r']) {
        return Err(DiffTargetError::Invalid(
            "That is not a PR number, PR URL or branch name.".into(),
        ));
    }
    if trimmed.to_ascii_lowercase().contains("github.com/") {
        let canonical = canonicalize_pr_url(trimmed);
        return pr_url_parts(&canonical)
            .map(|(slug, number)| RefInput::PrUrl { slug, number })
            .ok_or_else(|| {
                DiffTargetError::Invalid(format!(
                    "{trimmed} is not a GitHub pull request URL (https://github.com/<owner>/<repo>/pull/<n>)."
                ))
            });
    }
    let digits = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        return digits
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .map(RefInput::PrNumber)
            .ok_or_else(|| DiffTargetError::Invalid(format!("{trimmed} is not a PR number.")));
    }
    Ok(RefInput::Branch(normalize_branch(trimmed)))
}

/// `(owner/name, number)` of a canonical `https://github.com/o/r/pull/<n>`.
fn pr_url_parts(canonical: &str) -> Option<(String, u64)> {
    let rest = canonical.strip_prefix("https://github.com/")?;
    let mut parts = rest.split('/');
    let (owner, name, pull, number) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if pull != "pull" || parts.next().is_some() {
        return None;
    }
    Some((
        format!("{owner}/{name}").to_ascii_lowercase(),
        number.parse().ok()?,
    ))
}

/// A branch name as typed: `refs/heads/x`, `refs/remotes/origin/x` and
/// `origin/x` all mean `x`.
fn normalize_branch(name: &str) -> String {
    let name = name.trim();
    for prefix in ["refs/heads/", "refs/remotes/origin/", "origin/"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            return rest.to_owned();
        }
    }
    name.to_owned()
}

/// Whether `name` can be a branch name (git's own rules, plus no leading `-`).
fn valid_branch(dir: &Path, name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('-')
        && git_text(dir, &["check-ref-format", &format!("refs/heads/{name}")]).is_some()
}

fn remote_ref(branch: &str) -> String {
    format!("refs/remotes/origin/{branch}")
}

fn local_ref(branch: &str) -> String {
    format!("refs/heads/{branch}")
}

fn pr_head_ref(number: u64) -> String {
    format!("refs/zeron/pr/{number}/head")
}

fn ref_exists(repo: &Path, full_ref: &str) -> bool {
    git_text(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{full_ref}^{{commit}}"),
        ],
    )
    .is_some_and(|sha| !sha.is_empty())
}

/// `git fetch origin <refspec>` in `repo` (read-only: only the local
/// repository's refs change). Bounded by [`FETCH_TIMEOUT`].
fn fetch(repo: &Path, refspec: &str) -> Result<(), String> {
    let mut child = mast_harness::git_command()
        .args([
            "fetch",
            "--quiet",
            "--no-recurse-submodules",
            "origin",
            refspec,
        ])
        .current_dir(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git fetch: {e}"))?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > FETCH_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "git fetch origin {refspec} took longer than {} seconds",
                    FETCH_TIMEOUT.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("git fetch: {e}")),
        }
    };
    if status.success() {
        return Ok(());
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    Err(stderr.trim().to_owned())
}

fn fetch_branch(repo: &Path, branch: &str) -> Result<(), String> {
    fetch(
        repo,
        &format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"),
    )
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrFacts {
    #[serde(default)]
    head_ref_name: String,
    #[serde(default)]
    base_ref_name: String,
    #[serde(default)]
    is_cross_repository: bool,
}

/// Where a bind resolved to, before it is stored.
struct Resolved {
    repo: RepoCheckout,
    kind: RefDiffKind,
    head_branch: String,
    head_ref: String,
    base_branch: String,
    base_ref: String,
    pr_number: Option<u64>,
    pr_url: Option<String>,
}

impl DiffTargets {
    /// The repository checkouts of a chat's family: its own checkout and its
    /// submodules, one per repository.
    fn repo_checkouts(&self, family: &Family) -> Vec<RepoCheckout> {
        let mut out: Vec<RepoCheckout> = Vec::new();
        for checkout in &family.checkouts {
            if checkout.kind == DiffCheckoutKind::Worktree {
                continue;
            }
            let Some(facts) = checkout_facts(&checkout.path) else {
                continue;
            };
            let repo_id = repository_id(&self.device_id, &facts.common_dir);
            if out.iter().any(|repo| repo.repo_id == repo_id) {
                continue;
            }
            let slug = git_text(&checkout.path, &["config", "--get", "remote.origin.url"])
                .and_then(|url| github_slug(&url));
            out.push(RepoCheckout {
                path: checkout.path.clone(),
                name: checkout.repo_name.clone(),
                slug,
                repo_id,
            });
        }
        out
    }

    /// `gh pr view` for a PR; `None` when `gh` is missing, signed out, offline
    /// or the PR is unknown (the caller then uses the `refs/pull` fallback).
    fn gh_pr_facts(&self, slug: &str, number: u64) -> Option<PrFacts> {
        let output = self
            .tools
            .gh()
            .ok()?
            .args([
                "pr",
                "view",
                &number.to_string(),
                "--repo",
                slug,
                "--json",
                "headRefName,baseRefName,isCrossRepository",
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        serde_json::from_slice(&output.stdout).ok()
    }

    /// Resolve and store a branch / PR range target for the chat and select
    /// it. `base_branch` overrides the PR's base (else `main`); `repo_hint` is
    /// a repository checkout path (the picker knows which repository a branch
    /// candidate belongs to).
    pub fn bind_ref(
        &self,
        chat_id: &str,
        input: &str,
        base_branch: Option<&str>,
        repo_hint: Option<&str>,
    ) -> Result<RefBindOutcome, DiffTargetError> {
        let parsed = parse_input(input)?;
        let chat = self.local_chat(chat_id)?;
        let family = Family::resolve(&chat, false)?;
        let repos = self.repo_checkouts(&family);
        if repos.is_empty() {
            return Err(DiffTargetError::NoCheckout(
                "This chat has no git repository to read branches from.".into(),
            ));
        }
        let base_override = match base_branch.map(normalize_branch).filter(|b| !b.is_empty()) {
            Some(base) if valid_branch(&family.own, &base) => Some(base),
            Some(base) => {
                return Err(DiffTargetError::Invalid(format!(
                    "{base} is not a valid base branch name."
                )));
            }
            None => None,
        };
        let resolved = match parsed {
            RefInput::PrUrl { slug, number } => {
                let repo = repos
                    .iter()
                    .find(|repo| repo.slug.as_deref() == Some(slug.as_str()))
                    .ok_or_else(|| no_repo_for(&slug, &repos))?;
                self.resolve_pr(repo, number, base_override.as_deref())?
            }
            RefInput::PrNumber(number) => {
                let repo = self.repo_for_pr_number(&chat, &repos, number)?;
                self.resolve_pr(&repo, number, base_override.as_deref())?
            }
            RefInput::Branch(name) => self.resolve_branch(
                &family.own,
                &repos,
                &name,
                repo_hint,
                base_override.as_deref(),
            )?,
        };
        let id = ref_target_id(&resolved);
        let target = StoredRefDiffTarget {
            id: id.clone(),
            kind: resolved.kind,
            path: path_string(&canonical_lenient(&resolved.repo.path)),
            repo_id: resolved.repo.repo_id.clone(),
            head_branch: resolved.head_branch,
            head_ref: resolved.head_ref,
            base_branch: resolved.base_branch,
            base_ref: resolved.base_ref,
            pr_number: resolved.pr_number,
            pr_url: resolved.pr_url,
            bound_at: crate::now_ms(),
        };
        let stored = target.clone();
        let changed = self
            .workspace
            .update_chat_diff_targets(chat_id, |row| {
                match row.ref_targets.iter_mut().find(|t| t.id == id) {
                    Some(existing) => *existing = stored.clone(),
                    None => {
                        if row.ref_targets.len() >= MAX_CHAT_REF_TARGETS {
                            // Make room by dropping the oldest unselected one.
                            let keep = row.selected_ref.clone();
                            if let Some(ix) = row
                                .ref_targets
                                .iter()
                                .position(|t| keep.as_deref() != Some(t.id.as_str()))
                            {
                                row.ref_targets.remove(ix);
                            }
                        }
                        row.ref_targets.push(stored.clone());
                    }
                }
                row.selected_ref = Some(id.clone());
                row.selected = None;
            })
            .map_err(internal)?;
        if changed {
            self.bump();
        }
        Ok(RefBindOutcome {
            reply: self.stored(chat_id),
            target,
        })
    }

    /// Select a stored ref target (clears the checkout selection).
    pub fn select_ref(
        &self,
        chat_id: &str,
        ref_id: &str,
    ) -> Result<ChatDiffTargetsReply, DiffTargetError> {
        self.local_chat(chat_id)?;
        let mut missing = false;
        let changed = self
            .workspace
            .update_chat_diff_targets(chat_id, |row| {
                if row.ref_targets.iter().any(|t| t.id == ref_id) {
                    row.selected_ref = Some(ref_id.to_owned());
                    row.selected = None;
                } else {
                    missing = true;
                }
            })
            .map_err(internal)?;
        if missing {
            return Err(DiffTargetError::Invalid(format!(
                "No branch or PR target {ref_id} on this chat."
            )));
        }
        if changed {
            self.bump();
        }
        Ok(self.stored(chat_id))
    }

    /// Forget a stored ref target (a no-op for an unknown id).
    pub fn unbind_ref(
        &self,
        chat_id: &str,
        ref_id: &str,
    ) -> Result<ChatDiffTargetsReply, DiffTargetError> {
        self.local_chat(chat_id)?;
        let changed = self
            .workspace
            .update_chat_diff_targets(chat_id, |row| {
                row.ref_targets.retain(|t| t.id != ref_id);
                if row.selected_ref.as_deref() == Some(ref_id) {
                    row.selected_ref = None;
                }
            })
            .map_err(internal)?;
        if changed {
            self.bump();
        }
        Ok(self.stored(chat_id))
    }

    /// The repository a bare PR number belongs to: the chat's linked PRs with
    /// that number, else the only GitHub repository of the family.
    fn repo_for_pr_number(
        &self,
        chat: &Chat,
        repos: &[RepoCheckout],
        number: u64,
    ) -> Result<RepoCheckout, DiffTargetError> {
        let linked = self.workspace.chat_pr_links(&chat.id).unwrap_or_default();
        let mut matches: Vec<&RepoCheckout> = Vec::new();
        for link in &linked {
            let Some((slug, n)) = pr_url_parts(&canonicalize_pr_url(&link.url)) else {
                continue;
            };
            if n != number {
                continue;
            }
            if let Some(repo) = repos
                .iter()
                .find(|repo| repo.slug.as_deref() == Some(slug.as_str()))
                && !matches.iter().any(|known| known.repo_id == repo.repo_id)
            {
                matches.push(repo);
            }
        }
        if let [only] = matches.as_slice() {
            return Ok((*only).clone());
        }
        let github: Vec<&RepoCheckout> = repos.iter().filter(|repo| repo.slug.is_some()).collect();
        match (matches.len(), github.as_slice()) {
            (0, [only]) => Ok((*only).clone()),
            _ => Err(DiffTargetError::Invalid(format!(
                "PR #{number} could be in more than one repository ({}). Paste the PR URL instead.",
                repos
                    .iter()
                    .filter_map(|repo| repo.slug.as_deref())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    fn resolve_pr(
        &self,
        repo: &RepoCheckout,
        number: u64,
        base_override: Option<&str>,
    ) -> Result<Resolved, DiffTargetError> {
        let slug = repo.slug.clone().unwrap_or_default();
        let facts = self.gh_pr_facts(&slug, number);
        let known_head = facts
            .as_ref()
            .filter(|f| !f.is_cross_repository && valid_branch(&repo.path, &f.head_ref_name))
            .map(|f| f.head_ref_name.clone());
        let (head_branch, head_ref) = match known_head {
            Some(branch) => match head_of_branch(&repo.path, &branch) {
                Ok(head_ref) => (branch, head_ref),
                Err(branch_error) => {
                    pull_head(&repo.path, number, facts.as_ref()).map_err(|pull_error| {
                        head_fetch_failed(
                            &repo.name,
                            &format!("PR #{number}"),
                            &branch_error,
                            &pull_error,
                        )
                    })?
                }
            },
            None => pull_head(&repo.path, number, facts.as_ref()).map_err(|pull_error| {
                head_fetch_failed(&repo.name, &format!("PR #{number}"), "", &pull_error)
            })?,
        };
        let base = base_override.map(str::to_owned).or_else(|| {
            facts
                .as_ref()
                .map(|f| f.base_ref_name.clone())
                .filter(|b| valid_branch(&repo.path, b))
        });
        let (base_branch, base_ref) = resolve_base(repo, base.as_deref())?;
        Ok(Resolved {
            repo: repo.clone(),
            kind: RefDiffKind::Pr,
            head_branch,
            head_ref,
            base_branch,
            base_ref,
            pr_number: Some(number),
            pr_url: Some(format!("https://github.com/{slug}/pull/{number}")),
        })
    }

    fn resolve_branch(
        &self,
        any_repo: &Path,
        repos: &[RepoCheckout],
        name: &str,
        repo_hint: Option<&str>,
        base_override: Option<&str>,
    ) -> Result<Resolved, DiffTargetError> {
        if !valid_branch(any_repo, name) {
            return Err(DiffTargetError::Invalid(format!(
                "{name:?} is not a branch name, a PR number or a PR URL."
            )));
        }
        let candidates: Vec<&RepoCheckout> = match repo_hint {
            Some(hint) => {
                let wanted = canonical_lenient(Path::new(hint));
                let found: Vec<&RepoCheckout> = repos
                    .iter()
                    .filter(|repo| canonical_lenient(&repo.path) == wanted)
                    .collect();
                if found.is_empty() {
                    return Err(DiffTargetError::OutsideRepository(format!(
                        "{hint} is not a repository of this chat or its submodules."
                    )));
                }
                found
            }
            None => repos.iter().collect(),
        };
        // Where the branch already exists, else where origin has it.
        let present = candidates.iter().find(|repo| {
            ref_exists(&repo.path, &remote_ref(name)) || ref_exists(&repo.path, &local_ref(name))
        });
        let mut fetch_errors: Vec<String> = Vec::new();
        let repo: &RepoCheckout = match present {
            Some(repo) => {
                // Refresh quietly; the stored ref is whatever exists after.
                let _ = fetch_branch(&repo.path, name);
                repo
            }
            None => {
                let mut fetched = None;
                for repo in &candidates {
                    match fetch_branch(&repo.path, name) {
                        Ok(()) => {
                            fetched = Some(*repo);
                            break;
                        }
                        Err(error) => fetch_errors.push(format!("{}: {error}", repo.name)),
                    }
                }
                fetched.ok_or_else(|| {
                    DiffTargetError::Invalid(format!(
                        "Branch {name} was not found in {} (git fetch said: {}).",
                        candidates
                            .iter()
                            .map(|repo| repo.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                        fetch_errors.join("; ")
                    ))
                })?
            }
        };
        let head_ref = if ref_exists(&repo.path, &remote_ref(name)) {
            remote_ref(name)
        } else if ref_exists(&repo.path, &local_ref(name)) {
            local_ref(name)
        } else {
            return Err(DiffTargetError::Invalid(format!(
                "Branch {name} does not exist in {}.",
                repo.name
            )));
        };
        let (base_branch, base_ref) = resolve_base(repo, base_override)?;
        Ok(Resolved {
            repo: repo.clone(),
            kind: RefDiffKind::Branch,
            head_branch: name.to_owned(),
            head_ref,
            base_branch,
            base_ref,
            pr_number: None,
            pr_url: None,
        })
    }

    /// The "Diff branch / PR…" candidates: the chat's linked PRs first (link
    /// order), then branches whose name contains the chat's ticket id (newest
    /// commit first). Reads local refs only; it never fetches.
    pub fn list_ref_candidates(
        &self,
        chat_id: &str,
    ) -> Result<ListChatRefDiffCandidatesReply, DiffTargetError> {
        let chat = self.local_chat(chat_id)?;
        let family = Family::resolve(&chat, false)?;
        let repos = self.repo_checkouts(&family);
        let row = self
            .workspace
            .chat_diff_targets(chat_id)
            .map_err(internal)?
            .unwrap_or_default();
        let mut candidates: Vec<RefDiffCandidate> = Vec::new();

        let links = self.workspace.chat_pr_links(chat_id).unwrap_or_default();
        for link in links {
            let canonical = canonicalize_pr_url(&link.url);
            let Some((slug, number)) = pr_url_parts(&canonical) else {
                continue;
            };
            let Some(repo) = repos
                .iter()
                .find(|repo| repo.slug.as_deref() == Some(slug.as_str()))
            else {
                continue;
            };
            if candidates
                .iter()
                .any(|c| c.kind == RefDiffKind::Pr && c.input == canonical)
            {
                continue;
            }
            candidates.push(RefDiffCandidate {
                kind: RefDiffKind::Pr,
                label: format!("PR #{number}"),
                detail: canonical.clone(),
                input: canonical,
                path: path_string(&repo.path),
                repo_name: repo.name.clone(),
                linked: true,
                suggested: false,
                bound: row
                    .ref_targets
                    .iter()
                    .any(|t| t.repo_id == repo.repo_id && t.pr_number == Some(number)),
            });
        }

        let ticket = chat
            .linked_ticket_id
            .clone()
            .filter(|ticket| !ticket.trim().is_empty())
            .or_else(|| chat.branch.as_deref().and_then(extract_ticket_id))
            .map(|ticket| ticket.trim().to_lowercase());
        if let Some(ticket) = ticket {
            let mut branches = 0usize;
            for repo in &repos {
                let Some(listing) = git_text(
                    &repo.path,
                    &[
                        "for-each-ref",
                        "--sort=-committerdate",
                        "--format=%(refname)",
                        "refs/heads",
                        "refs/remotes/origin",
                    ],
                ) else {
                    continue;
                };
                let mut seen: Vec<String> = Vec::new();
                for full in listing.lines() {
                    let name = full
                        .strip_prefix("refs/remotes/origin/")
                        .or_else(|| full.strip_prefix("refs/heads/"))
                        .unwrap_or(full);
                    if name == "HEAD"
                        || TRUNK_BRANCHES.contains(&name)
                        || !name.to_lowercase().contains(&ticket)
                        || seen.iter().any(|known| known == name)
                    {
                        continue;
                    }
                    seen.push(name.to_owned());
                    if branches >= MAX_BRANCH_CANDIDATES {
                        break;
                    }
                    branches += 1;
                    candidates.push(RefDiffCandidate {
                        kind: RefDiffKind::Branch,
                        label: format!("branch {name}"),
                        detail: repo.name.clone(),
                        input: name.to_owned(),
                        path: path_string(&repo.path),
                        repo_name: repo.name.clone(),
                        linked: false,
                        suggested: true,
                        bound: row
                            .ref_targets
                            .iter()
                            .any(|t| t.repo_id == repo.repo_id && t.head_branch == name),
                    });
                }
            }
        }
        Ok(ListChatRefDiffCandidatesReply { candidates })
    }
}

fn no_repo_for(slug: &str, repos: &[RepoCheckout]) -> DiffTargetError {
    DiffTargetError::OutsideRepository(format!(
        "No checkout of {slug} among this chat's repository and its submodules ({}).",
        repos
            .iter()
            .map(|repo| repo.slug.as_deref().unwrap_or(repo.name.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn head_fetch_failed(
    repo: &str,
    what: &str,
    branch_error: &str,
    pull_error: &str,
) -> DiffTargetError {
    let mut detail = pull_error.to_owned();
    if !branch_error.is_empty() {
        detail = format!("{branch_error}; {pull_error}");
    }
    DiffTargetError::Invalid(format!("Could not fetch {what} in {repo}: {detail}"))
}

/// Fetch `branch` and return the ref to read it from (remote-tracking,
/// else an existing local branch).
fn head_of_branch(repo: &Path, branch: &str) -> Result<String, String> {
    let fetched = fetch_branch(repo, branch);
    if ref_exists(repo, &remote_ref(branch)) {
        return Ok(remote_ref(branch));
    }
    if ref_exists(repo, &local_ref(branch)) {
        return Ok(local_ref(branch));
    }
    Err(fetched
        .err()
        .unwrap_or_else(|| format!("branch {branch} does not exist")))
}

/// The PR's head through GitHub's `refs/pull/<n>/head` (forks, deleted
/// branches, or no `gh`).
fn pull_head(
    repo: &Path,
    number: u64,
    facts: Option<&PrFacts>,
) -> Result<(String, String), String> {
    let head_ref = pr_head_ref(number);
    let fetched = fetch(repo, &format!("+refs/pull/{number}/head:{head_ref}"));
    if !ref_exists(repo, &head_ref) {
        return Err(fetched
            .err()
            .unwrap_or_else(|| format!("{head_ref} does not exist")));
    }
    let branch = facts
        .map(|f| f.head_ref_name.clone())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| format!("pr-{number}"));
    Ok((branch, head_ref))
}

/// The base: `wanted`, else `main`, else `master`. Fetches it read-only
/// (best effort) and reads it from `origin`, else from a local branch.
fn resolve_base(
    repo: &RepoCheckout,
    wanted: Option<&str>,
) -> Result<(String, String), DiffTargetError> {
    let defaults = ["main", "master"];
    let names: Vec<&str> = match wanted {
        Some(name) => vec![name],
        None => defaults.to_vec(),
    };
    for name in &names {
        let _ = fetch_branch(&repo.path, name);
        if ref_exists(&repo.path, &remote_ref(name)) {
            return Ok(((*name).to_owned(), remote_ref(name)));
        }
        if ref_exists(&repo.path, &local_ref(name)) {
            return Ok(((*name).to_owned(), local_ref(name)));
        }
    }
    Err(DiffTargetError::Invalid(format!(
        "Base branch {} was not found in {} (origin/{} is missing). Name another base branch.",
        names.join(" or "),
        repo.name,
        names.first().copied().unwrap_or("main"),
    )))
}

/// Stable id: PR number or head branch within the repository.
fn ref_target_id(resolved: &Resolved) -> String {
    use sha2::{Digest, Sha256};
    let (prefix, key) = match (resolved.kind, resolved.pr_number) {
        (RefDiffKind::Pr, Some(number)) => ("pr", number.to_string()),
        _ => ("br", resolved.head_branch.clone()),
    };
    let mut hasher = Sha256::new();
    hasher.update(resolved.repo.repo_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(key.as_bytes());
    format!("{prefix}-{}", &hex(&hasher.finalize())[..16])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_parse_as_pr_url_number_or_branch() {
        assert_eq!(
            parse_input("https://github.com/Cofactr/Flagship/pull/6487/files").unwrap(),
            RefInput::PrUrl {
                slug: "cofactr/flagship".into(),
                number: 6487
            }
        );
        assert_eq!(parse_input("6487").unwrap(), RefInput::PrNumber(6487));
        assert_eq!(parse_input(" #6487 ").unwrap(), RefInput::PrNumber(6487));
        assert_eq!(
            parse_input("origin/michael/eng-3012-x").unwrap(),
            RefInput::Branch("michael/eng-3012-x".into())
        );
        assert_eq!(
            parse_input("refs/heads/feat").unwrap(),
            RefInput::Branch("feat".into())
        );
        assert!(parse_input("  ").is_err());
        assert!(parse_input("https://github.com/o/r/issues/3").is_err());
        assert!(parse_input("0").is_err());
    }
}
