//! Pure helpers of the chat diff target UI (`m0-contract.md` § chat diff
//! targets): ticket-first ranking of the "Diff checkout…" candidates, row
//! labels, and the test-only RPC fake. The state lives in `AppState`
//! (`chat_diff_targets`), the picker in `changes.rs`.

use zeron_proto::{Chat, DiffCheckoutCandidate, DiffCheckoutKind};

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
}
