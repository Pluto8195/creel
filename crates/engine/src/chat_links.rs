//! Pure text-mining helpers for durable PR/ticket links (ticket 0xx) — shared
//! by the live per-event tap (`sessions.rs`'s `drive_run`), the transcript
//! backfill repair (`external_import.rs::repair_missing_links`), and the
//! ticket-from-PR stamp (`rpc.rs`'s `CHAT_LINK_STATUS` handler).
//!
//! Hand-rolled, no `regex` crate — same precedent as
//! `pr_ticket_cache::extract_ticket_id`, which this module reuses for the
//! ticket side rather than re-deriving the same pattern.
//!
//! Tool output: `AgentEvent::ToolResult.output` is populated by the ACP
//! harnesses, by claude (`tool_result` block content) and by codex
//! (`commandExecution.aggregatedOutput`); `parse_transcript` keeps a bounded
//! copy for Claude Code transcripts. A chat whose run predates that capture
//! only saw the agent narrate the URL back (`mentioned`); the Work Outline PR
//! link pass (`work_outline_pr_links`) re-reads such chats' stored tool
//! parts once to recover `created_in_chat`.

use crate::workspace_host::{ChatLinkKind, WorkspaceHost};
use std::collections::BTreeSet;
use mast_proto::{ChatLinkSource, ToolCall};

/// A `github.com/<org>/<repo>/pull/<n>` URL appearing anywhere in `text` —
/// matches the design's own vocabulary ("a `github.com/<org>/<repo>/pull/<n>`
/// URL appearing in a tool result..."). The scheme is included in the
/// returned string when the source text had one immediately before the host
/// (`https://github.com/...`); a bare `github.com/...` mention is returned
/// exactly as it appeared, unprefixed. `None` when no PR URL appears at all.
#[cfg(test)]
pub(crate) fn extract_pr_url(text: &str) -> Option<String> {
    extract_pr_urls(text).into_iter().next()
}

/// Every distinct GitHub pull-request URL appearing in `text`, in encounter
/// order. Extraction preserves the spelling used by the text for compatibility
/// with the original single-result helper; durable storage canonicalizes it.
pub(crate) fn extract_pr_urls(text: &str) -> Vec<String> {
    const HOST: &str = "github.com/";
    let lowercase = text.to_ascii_lowercase();
    let mut search_from = 0usize;
    let mut urls = Vec::new();
    let mut seen = BTreeSet::new();
    while let Some(rel) = lowercase[search_from..].find(HOST) {
        let host_start = search_from + rel;
        let rest = &lowercase[host_start + HOST.len()..];
        if let Some(len) = pull_path_len(rest) {
            let scheme_len = ["https://", "http://"]
                .iter()
                .find(|s| lowercase[..host_start].ends_with(**s))
                .map_or(0, |s| s.len());
            let start = host_start - scheme_len;
            let end = host_start + HOST.len() + len;
            let url = text[start..end].to_string();
            let canonical = canonicalize_pr_url(&url);
            if seen.insert(canonical) {
                urls.push(url);
            }
            search_from = end;
        } else {
            search_from = host_start + HOST.len();
        }
    }
    urls
}

/// Stable identity for a GitHub pull-request URL. Recognized URLs are reduced
/// to lowercase `https://github.com/<owner>/<repo>/pull/<n>`; legacy opaque
/// values are trimmed but otherwise retained so older data remains readable.
pub(crate) fn canonicalize_pr_url(url: &str) -> String {
    const HOST: &str = "github.com/";
    let trimmed = url.trim();
    let lowercase = trimmed.to_ascii_lowercase();
    let Some(host_start) = lowercase.find(HOST) else {
        return trimmed.to_string();
    };
    let rest = &lowercase[host_start + HOST.len()..];
    let Some(len) = pull_path_len(rest) else {
        return trimmed.to_string();
    };
    format!("https://{HOST}{}", &rest[..len])
}

/// `<owner>/<repo>/pull/<digits>` immediately at the start of `rest` — the
/// byte length consumed, or `None` if `rest` doesn't continue that shape.
fn pull_path_len(rest: &str) -> Option<usize> {
    let is_seg = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    let owner_end = rest
        .find('/')
        .filter(|&i| i > 0 && rest[..i].chars().all(is_seg))?;
    let after_owner = &rest[owner_end + 1..];
    let repo_end = after_owner
        .find('/')
        .filter(|&i| i > 0 && after_owner[..i].chars().all(is_seg))?;
    let after_repo = &after_owner[repo_end + 1..];
    let digits = after_repo.strip_prefix("pull/")?;
    let digit_len = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    (digit_len > 0).then_some(owner_end + 1 + repo_end + 1 + "pull/".len() + digit_len)
}

/// Heuristic PR-creation detector: substring match on `gh pr create` in a
/// tool invocation's command/input text — a heuristic per the design, not a
/// full shell-argv parse (matches e.g. `gh pr create --title …` and a
/// quoted/piped variant just as well; a comment or string that happens to
/// contain the same words is an accepted false-positive-on-purpose, same
/// spirit as `is_bot_login`'s substring checks in `pr_ticket_cache.rs`).
///
/// Also matches `gh pr new` (the CLI's alias) and MCP/unknown tools whose
/// name says they create a pull request (`create_pull_request`,
/// `createPullRequest`): their result carries the new PR's URL just like
/// `gh pr create`'s stdout.
pub(crate) fn looks_like_pr_create(command_or_input: &str) -> bool {
    if command_or_input.contains("gh pr create") || command_or_input.contains("gh pr new") {
        return true;
    }
    let squashed: String = command_or_input
        .chars()
        .filter(|c| !matches!(c, '_' | '-'))
        .flat_map(char::to_lowercase)
        .collect();
    squashed.contains("createpullrequest")
}

/// The text a [`ToolCall`] invocation is judged by — the shell command for
/// `Exec`, or the tool name plus its raw JSON input for anything MCP/unknown
/// shaped (cursor and others route `gh` through an MCP-wrapped exec tool
/// rather than a native `Exec` call).
pub(crate) fn tool_call_invocation_text(call: &ToolCall) -> String {
    match call {
        ToolCall::Exec { command } => command.clone(),
        ToolCall::Unknown { name, input }
        | ToolCall::Mcp {
            tool: name, input, ..
        } => {
            let mut text = name.clone();
            if let Some(input) = input {
                text.push(' ');
                text.push_str(&input.to_string());
            }
            text
        }
        _ => String::new(),
    }
}

/// Ticket-id mention, delegating to the existing branch-name extractor's
/// `[A-Za-z]{2,6}-\d+` pattern — the same regex, just run over free text
/// instead of a branch name.
pub(crate) fn extract_ticket_mention(text: &str) -> Option<String> {
    crate::pr_ticket_cache::extract_ticket_id(text)
}

/// Stamp a `mentioned` PR link from free text (a message or a tool result),
/// most-recent-mention-wins (write-time precedence: `mentioned` never
/// overwrites `manual`/`created_in_chat`, but DOES overwrite an older
/// `mentioned` value — same source, later write, per
/// `ChatLinkSource::can_overwrite`). Best-effort: a missing chat row or a
/// precedence loss both no-op silently, matching every other workspace write
/// this engine makes off the hot path.
pub(crate) fn mine_pr_mention(workspace: &WorkspaceHost, chat_id: &str, text: &str) {
    for url in extract_pr_urls(text) {
        if let Err(err) = workspace.set_chat_link(
            chat_id,
            ChatLinkKind::Pr,
            Some(&url),
            ChatLinkSource::Mentioned,
        ) {
            tracing::warn!(chat = %chat_id, error = %err, "PR mention link write failed");
        }
    }
}

/// Sibling to [`mine_pr_mention`] for ticket ids.
pub(crate) fn mine_ticket_mention(workspace: &WorkspaceHost, chat_id: &str, text: &str) {
    if let Some(id) = extract_ticket_mention(text)
        && let Err(err) = workspace.set_chat_link(
            chat_id,
            ChatLinkKind::Ticket,
            Some(&id),
            ChatLinkSource::Mentioned,
        )
    {
        tracing::warn!(chat = %chat_id, error = %err, "ticket mention link write failed");
    }
}

/// The PR links one tool result earns: every URL of a SUCCESSFUL PR-create
/// call's output is `created_in_chat` (stacked PRs, a loop of `gh pr create`);
/// a failed create ("a pull request ... already exists: <url>") created
/// nothing, so its URLs only degrade to `mentioned`; any other tool result's
/// URLs are mentions.
pub(crate) fn tool_result_pr_links(
    is_pr_create: bool,
    is_error: bool,
    output: &str,
) -> Vec<(String, ChatLinkSource)> {
    let source = if is_pr_create && !is_error {
        ChatLinkSource::CreatedInChat
    } else {
        ChatLinkSource::Mentioned
    };
    extract_pr_urls(output)
        .into_iter()
        .map(|url| (url, source))
        .collect()
}

/// Persist [`tool_result_pr_links`] for one tool result. Best-effort.
pub(crate) fn mine_tool_result_links(
    workspace: &WorkspaceHost,
    chat_id: &str,
    is_pr_create: bool,
    is_error: bool,
    output: &str,
) {
    for (url, source) in tool_result_pr_links(is_pr_create, is_error, output) {
        if let Err(err) = workspace.set_chat_link(chat_id, ChatLinkKind::Pr, Some(&url), source) {
            tracing::warn!(chat = %chat_id, error = %err, "PR tool-result link write failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_pr_url_finds_a_scheme_qualified_mention() {
        let text = "Created pull request: https://github.com/acme/widgets/pull/42\nDone.";
        assert_eq!(
            extract_pr_url(text),
            Some("https://github.com/acme/widgets/pull/42".to_string())
        );
    }

    #[test]
    fn extract_pr_url_finds_a_bare_host_mention_unprefixed() {
        assert_eq!(
            extract_pr_url("see github.com/acme/widgets/pull/7 for details"),
            Some("github.com/acme/widgets/pull/7".to_string())
        );
    }

    #[test]
    fn extract_pr_url_ignores_non_pull_github_urls() {
        assert_eq!(
            extract_pr_url("https://github.com/acme/widgets/issues/7"),
            None
        );
        assert_eq!(extract_pr_url("https://github.com/acme/widgets"), None);
        assert_eq!(extract_pr_url("no url here"), None);
    }

    #[test]
    fn extract_pr_url_stops_at_trailing_punctuation_and_path() {
        // A trailing `/files` (the "Files changed" tab) must not get folded
        // into the digits, and a `.` following the number must not either.
        assert_eq!(
            extract_pr_url("https://github.com/acme/widgets/pull/42/files"),
            Some("https://github.com/acme/widgets/pull/42".to_string())
        );
        assert_eq!(
            extract_pr_url("(https://github.com/acme/widgets/pull/42)."),
            Some("https://github.com/acme/widgets/pull/42".to_string())
        );
    }

    #[test]
    fn extract_pr_urls_finds_and_deduplicates_every_pull_request() {
        let text = "Compare https://github.com/Acme/Widgets/pull/42/files with \
                    github.com/acme/widgets/pull/43 and \
                    http://github.com/acme/widgets/pull/42.";
        assert_eq!(
            extract_pr_urls(text),
            vec![
                "https://github.com/Acme/Widgets/pull/42".to_string(),
                "github.com/acme/widgets/pull/43".to_string(),
            ]
        );
    }

    #[test]
    fn canonical_pr_urls_share_one_storage_identity() {
        for raw in [
            "https://github.com/Acme/Widgets/pull/42",
            "http://github.com/acme/widgets/pull/42/files",
            "github.com/ACME/WIDGETS/pull/42?tab=checks",
        ] {
            assert_eq!(
                canonicalize_pr_url(raw),
                "https://github.com/acme/widgets/pull/42"
            );
        }
        assert_eq!(canonicalize_pr_url("legacy-pr-url"), "legacy-pr-url");
    }

    #[test]
    fn looks_like_pr_create_matches_the_gh_invocation() {
        assert!(looks_like_pr_create(
            "gh pr create --title 'Fix thing' --body '…'"
        ));
        assert!(!looks_like_pr_create("gh pr view 42"));
        assert!(!looks_like_pr_create("gh pr merge 42"));
        assert!(looks_like_pr_create("gh pr new --fill"));
        assert!(looks_like_pr_create(
            "mcp__github__create_pull_request {\"owner\":\"acme\"}"
        ));
        assert!(looks_like_pr_create("createPullRequest {}"));
        assert!(!looks_like_pr_create("mcp__github__get_pull_request {}"));
    }

    #[test]
    fn tool_call_invocation_text_covers_exec_and_mcp_shapes() {
        assert_eq!(
            tool_call_invocation_text(&ToolCall::Exec {
                command: "gh pr create --title x".into()
            }),
            "gh pr create --title x"
        );
        let mcp = ToolCall::Mcp {
            server: "github".into(),
            tool: "gh".into(),
            input: Some(serde_json::json!({"args": ["pr", "create"]})),
        };
        assert!(tool_call_invocation_text(&mcp).contains("pr"));
        assert_eq!(
            tool_call_invocation_text(&ToolCall::Glob {
                pattern: "*.rs".into()
            }),
            ""
        );
    }

    #[test]
    fn tool_result_links_stamp_every_created_url() {
        let out = "https://github.com/Cofactr/flagship/pull/6487\nhttps://github.com/Cofactr/flagship/pull/6488\n";
        let links = tool_result_pr_links(true, false, out);
        assert_eq!(links.len(), 2);
        assert!(
            links
                .iter()
                .all(|(_, s)| *s == ChatLinkSource::CreatedInChat)
        );
        assert!(links[1].0.ends_with("/6488"));
    }

    #[test]
    fn tool_result_links_do_not_credit_a_failed_create() {
        let out =
            "a pull request for branch \"x\" already exists:\nhttps://github.com/acme/ui/pull/4";
        let links = tool_result_pr_links(true, true, out);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].1, ChatLinkSource::Mentioned);
        let other = tool_result_pr_links(false, false, out);
        assert_eq!(other[0].1, ChatLinkSource::Mentioned);
    }

    #[test]
    fn extract_ticket_mention_delegates_to_the_shared_regex() {
        assert_eq!(
            extract_ticket_mention("see ENG-2715 for context"),
            Some("ENG-2715".into())
        );
        assert_eq!(extract_ticket_mention("nothing here"), None);
    }

    #[tokio::test]
    async fn the_live_tap_persists_every_created_url_and_ignores_a_failed_create() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let host = WorkspaceHost::open(
            Arc::new(mast_sync::DocsStore::open(dir.path()).unwrap()),
            crate::workspace_host::WorkspaceHostConfig {
                device_id: "dev".into(),
                device_name: "Dev".into(),
                platform: "macos".into(),
                org_id: "org".into(),
                user_id: "user".into(),
                edge: None,
            },
        )
        .unwrap();
        host.create_chat("c1", None, Some("dev"), None, None)
            .unwrap();
        mine_tool_result_links(
            &host,
            "c1",
            true,
            false,
            "https://github.com/Cofactr/flagship/pull/6487\nhttps://github.com/Cofactr/flagship/pull/6488",
        );
        mine_tool_result_links(
            &host,
            "c1",
            true,
            true,
            "already exists: https://github.com/Cofactr/flagship/pull/9",
        );
        let links = host.chat_pr_links("c1").unwrap();
        let source_of = |n: u64| {
            links
                .iter()
                .find(|l| l.url.ends_with(&format!("/{n}")))
                .map(|l| l.source)
        };
        assert_eq!(source_of(6487), Some(ChatLinkSource::CreatedInChat));
        assert_eq!(source_of(6488), Some(ChatLinkSource::CreatedInChat));
        assert_eq!(source_of(9), Some(ChatLinkSource::Mentioned));
    }
}
