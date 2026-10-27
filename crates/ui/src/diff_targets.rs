//! Pure helpers of the chat diff target UI (`m0-contract.md` § chat diff
//! targets): ticket-first ranking of the "Diff checkout…" candidates, row
//! labels, and the test-only RPC fake. The state lives in `AppState`
//! (`chat_diff_targets`), the picker in `changes.rs`.

use zeron_proto::{Chat, DiffCheckoutCandidate, DiffCheckoutKind, RefDiffCandidate, RefDiffKind};

use crate::work_outline::execution::ref_label;

/// One picker row: the engine's candidate plus whether it matches the chat's
/// ticket (shown with a "suggested" badge, sorted first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RankedCandidate {
    pub candidate: DiffCheckoutCandidate,
    pub suggested: bool,
}

impl RankedCandidate {
    /// "<repo name> \u{b7} <branch or HEAD (sha7)>".
    pub(crate) fn label(&self) -> String {
        format!(
            "{} \u{b7} {}",
            self.candidate.repo_name,
            ref_label(
                self.candidate.branch.as_deref(),
                self.candidate.head_sha.as_deref()
            )
        )
    }

    pub(crate) fn kind_tag(&self) -> &'static str {
        kind_tag(self.candidate.kind)
    }
}

pub(crate) fn kind_tag(kind: DiffCheckoutKind) -> &'static str {
    match kind {
        DiffCheckoutKind::Own => "this checkout",
        DiffCheckoutKind::Submodule => "submodule",
        DiffCheckoutKind::Worktree => "worktree",
    }
}

/// The ticket a chat is about: its durable link, else the id in its branch
/// name (the same inference the PR/ticket chips use).
pub(crate) fn chat_ticket_id(chat: &Chat) -> Option<String> {
    chat.linked_ticket_id
        .clone()
        .filter(|ticket| !ticket.trim().is_empty())
        .or_else(|| {
            chat.branch
                .as_deref()
                .and_then(crate::workspace_chip::extract_ticket_id)
        })
}

/// Rank the engine's candidates: those whose branch or path contains
/// `ticket` (case-insensitive, e.g. `eng-3012`) first and flagged suggested;
/// everything else keeps the engine's order (own checkout first). Stable.
/// The chat's own checkout is never "suggested": it is already what the pane
/// shows by default.
pub(crate) fn rank_candidates(
    candidates: Vec<DiffCheckoutCandidate>,
    ticket: Option<&str>,
) -> Vec<RankedCandidate> {
    let needle = ticket
        .map(|ticket| ticket.trim().to_lowercase())
        .filter(|ticket| !ticket.is_empty());
    let mut ranked: Vec<RankedCandidate> = candidates
        .into_iter()
        .map(|candidate| {
            let suggested = candidate.kind != DiffCheckoutKind::Own
                && needle.as_deref().is_some_and(|needle| {
                    candidate
                        .branch
                        .as_deref()
                        .is_some_and(|branch| branch.to_lowercase().contains(needle))
                        || candidate.path.to_lowercase().contains(needle)
                });
            RankedCandidate {
                candidate,
                suggested,
            }
        })
        .collect();
    ranked.sort_by_key(|row| !row.suggested);
    ranked
}

/// The path shortened for a row: the last two components, with a leading
/// ellipsis when anything was cut (the tooltip carries the full path).
pub(crate) fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    if parts.len() <= 2 {
        return path.to_owned();
    }
    format!("\u{2026}/{}", parts[parts.len() - 2..].join("/"))
}

// ── "Diff branch / PR…" ─────────────────────────────────────────────────────

/// What a free-text query means: the row the picker offers when nothing
/// listed matches ("Diff PR #6487" / "Diff branch foo").
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FreeText {
    /// Sent to the engine as the `bindRef` input.
    pub input: String,
    pub kind: RefDiffKind,
    /// "PR #6487" / "branch foo".
    pub label: String,
}

/// One row of the "Diff branch / PR…" picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RefRow {
    /// Index into the engine's candidate list.
    Candidate(usize),
    FreeText(FreeText),
}

/// Classify a typed query: a PR URL or number is a PR, anything else
/// without whitespace is a branch name (`origin/` and `refs/heads/` prefixes
/// are the same branch). The engine re-parses and refuses what is not a ref.
pub(crate) fn classify_free_text(query: &str) -> Option<FreeText> {
    let input = query.trim();
    if input.is_empty() || input.len() > 300 || input.contains(char::is_whitespace) {
        return None;
    }
    let pr = |number: &str| {
        number
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .map(|n| FreeText {
                input: input.to_owned(),
                kind: RefDiffKind::Pr,
                label: format!("PR #{n}"),
            })
    };
    if input.to_ascii_lowercase().contains("github.com/") {
        let after = input.split_once("/pull/")?.1;
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        return pr(&digits);
    }
    let bare = input.strip_prefix('#').unwrap_or(input);
    if !bare.is_empty() && bare.bytes().all(|b| b.is_ascii_digit()) {
        return pr(bare);
    }
    let name = ["refs/heads/", "refs/remotes/origin/", "origin/"]
        .iter()
        .find_map(|prefix| input.strip_prefix(prefix))
        .unwrap_or(input);
    Some(FreeText {
        input: input.to_owned(),
        kind: RefDiffKind::Branch,
        label: format!("branch {name}"),
    })
}

/// The picker's rows for `query`: the engine's candidates (linked PRs, then
/// ticket branches) filtered by it, then a free-text row for what was typed
/// unless a candidate already is that.
pub(crate) fn ref_rows(candidates: &[RefDiffCandidate], query: &str) -> Vec<RefRow> {
    let haystacks: Vec<String> = candidates
        .iter()
        .map(|c| format!("{} {} {} {}", c.label, c.detail, c.input, c.repo_name))
        .collect();
    let mut rows: Vec<RefRow> = if query.trim().is_empty() {
        (0..candidates.len()).map(RefRow::Candidate).collect()
    } else {
        crate::popover::filter_indices(query, &haystacks)
            .into_iter()
            .map(RefRow::Candidate)
            .collect()
    };
    if let Some(free) = classify_free_text(query) {
        let already = candidates.iter().any(|c| {
            c.kind == free.kind
                && (c.input.eq_ignore_ascii_case(&free.input) || c.label == free.label)
        });
        if !already {
            rows.push(RefRow::FreeText(free));
        }
    }
    rows
}

/// Tests: scripted transport for the diff target RPCs, `(method, params) ->
/// reply or the engine's error message`.
#[cfg(test)]
pub(crate) type FakeRpc = std::sync::Arc<
    dyn Fn(&str, serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync,
>;

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        path: &str,
        kind: DiffCheckoutKind,
        branch: Option<&str>,
    ) -> DiffCheckoutCandidate {
        DiffCheckoutCandidate {
            path: path.into(),
            kind,
            repo_name: "flagship".into(),
            branch: branch.map(Into::into),
            head_sha: Some("0123456789abcdef".into()),
            bound: false,
            selected: false,
        }
    }

    #[test]
    fn ticket_matches_sort_first_and_the_rest_keep_engine_order() {
        let rows = rank_candidates(
            vec![
                candidate("/w", DiffCheckoutKind::Own, Some("main")),
                candidate("/w/flagship", DiffCheckoutKind::Submodule, Some("main")),
                candidate(
                    "/w/.worktrees/flagship/a",
                    DiffCheckoutKind::Worktree,
                    Some("michael/eng-9-other"),
                ),
                candidate(
                    "/w/.worktrees/flagship/b",
                    DiffCheckoutKind::Worktree,
                    Some("michael/eng-3012-kit-line-ship-estimates"),
                ),
                candidate(
                    "/w/.worktrees/flagship/ENG-3012-detached",
                    DiffCheckoutKind::Worktree,
                    None,
                ),
            ],
            Some("ENG-3012"),
        );
        let order: Vec<_> = rows
            .iter()
            .map(|row| (row.candidate.path.as_str(), row.suggested))
            .collect();
        assert_eq!(
            order,
            vec![
                ("/w/.worktrees/flagship/b", true),
                ("/w/.worktrees/flagship/ENG-3012-detached", true),
                ("/w", false),
                ("/w/flagship", false),
                ("/w/.worktrees/flagship/a", false),
            ]
        );
    }

    #[test]
    fn no_ticket_or_an_empty_one_keeps_engine_order_and_suggests_nothing() {
        let input = vec![
            candidate("/w", DiffCheckoutKind::Own, Some("eng-1")),
            candidate("/x", DiffCheckoutKind::Worktree, Some("eng-1")),
        ];
        for ticket in [None, Some(""), Some("  ")] {
            let rows = rank_candidates(input.clone(), ticket);
            assert_eq!(rows[0].candidate.path, "/w");
            assert!(rows.iter().all(|row| !row.suggested));
        }
    }

    #[test]
    fn the_own_checkout_is_never_suggested() {
        let rows = rank_candidates(
            vec![candidate(
                "/w/eng-3012",
                DiffCheckoutKind::Own,
                Some("eng-3012"),
            )],
            Some("eng-3012"),
        );
        assert!(!rows[0].suggested);
    }

    #[test]
    fn labels_and_short_paths() {
        let rows = rank_candidates(
            vec![
                candidate("/a/b/c/d", DiffCheckoutKind::Worktree, Some("feat/x")),
                candidate("/a", DiffCheckoutKind::Own, None),
            ],
            None,
        );
        assert_eq!(rows[0].label(), "flagship \u{b7} feat/x");
        assert_eq!(rows[1].label(), "flagship \u{b7} HEAD (0123456)");
        assert_eq!(rows[0].kind_tag(), "worktree");
        assert_eq!(rows[1].kind_tag(), "this checkout");
        assert_eq!(short_path("/a/b/c/d"), "\u{2026}/c/d");
        assert_eq!(short_path("/a/b"), "/a/b");
    }

    fn ref_candidate(
        kind: RefDiffKind,
        label: &str,
        input: &str,
        linked: bool,
    ) -> RefDiffCandidate {
        RefDiffCandidate {
            kind,
            label: label.into(),
            detail: String::new(),
            input: input.into(),
            path: "/w/flagship".into(),
            repo_name: "flagship".into(),
            linked,
            suggested: !linked,
            bound: false,
        }
    }

    #[test]
    fn free_text_is_a_pr_for_numbers_and_urls_and_a_branch_otherwise() {
        let pr = |text: &str| classify_free_text(text).map(|f| (f.kind, f.label));
        assert_eq!(pr("6487"), Some((RefDiffKind::Pr, "PR #6487".into())));
        assert_eq!(pr(" #6487 "), Some((RefDiffKind::Pr, "PR #6487".into())));
        assert_eq!(
            pr("https://github.com/cofactr/flagship/pull/6488/files"),
            Some((RefDiffKind::Pr, "PR #6488".into()))
        );
        assert_eq!(
            pr("michael/eng-3012-x"),
            Some((RefDiffKind::Branch, "branch michael/eng-3012-x".into()))
        );
        assert_eq!(
            pr("origin/feature"),
            Some((RefDiffKind::Branch, "branch feature".into()))
        );
        // Not a ref: empty, spaced, a non-PR GitHub link, PR #0.
        assert_eq!(pr("  "), None);
        assert_eq!(pr("two words"), None);
        assert_eq!(pr("https://github.com/o/r/issues/3"), None);
        assert_eq!(pr("0"), None);
    }

    #[test]
    fn ref_rows_keep_linked_prs_first_and_offer_free_text_last() {
        let candidates = vec![
            ref_candidate(
                RefDiffKind::Pr,
                "PR #6487",
                "https://github.com/cofactr/flagship/pull/6487",
                true,
            ),
            ref_candidate(
                RefDiffKind::Pr,
                "PR #6488",
                "https://github.com/cofactr/flagship/pull/6488",
                true,
            ),
            ref_candidate(
                RefDiffKind::Branch,
                "branch michael/eng-3012-kit-line",
                "michael/eng-3012-kit-line",
                false,
            ),
        ];
        // No query: the engine's order, no free-text row.
        assert_eq!(
            ref_rows(&candidates, ""),
            vec![
                RefRow::Candidate(0),
                RefRow::Candidate(1),
                RefRow::Candidate(2)
            ]
        );
        // A query that matches a candidate does not add a duplicate row.
        assert_eq!(ref_rows(&candidates, "6488"), vec![RefRow::Candidate(1)]);
        assert_eq!(
            ref_rows(&candidates, "https://github.com/cofactr/flagship/pull/6487"),
            vec![RefRow::Candidate(0)]
        );
        // Typing a PR that is not listed, or a branch, offers it.
        let rows = ref_rows(&candidates, "7000");
        assert_eq!(rows.len(), 1);
        assert!(matches!(&rows[0], RefRow::FreeText(f) if f.label == "PR #7000"));
        let rows = ref_rows(&candidates, "kit-line");
        assert_eq!(rows[0], RefRow::Candidate(2));
        assert!(matches!(&rows[1], RefRow::FreeText(f) if f.label == "branch kit-line"));
    }
}
