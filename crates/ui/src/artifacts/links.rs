//! Where a link inside a rendered artifact goes. Pure: the HTML document's
//! navigation delegate, the Markdown link handler, and the Shell all classify
//! with [`classify`] and act on the result.
//!
//! - `path`, `path:line`, `path:line:col`, `path:line-line`, `path#L12`,
//!   `mast-file:path` open in the file viewer. The path resolves against the
//!   artifact's canonical checkout (the parent of `.mast/` or the legacy `.zeron/`), then the
//!   chat's checkout (absolute paths under either root); a relative path
//!   names the same file in both.
//! - `mast-artifact:` opens that artifact tab.
//! - `http(s):` follows the web-link preference; `mailto:` is external.
//! - `#id` is a same-document anchor and stays in place.
//! - Everything else (`file:`, `javascript:`, `data:`, `..` or `./` paths,
//!   queries, unknown schemes) is blocked.
use crate::workspace_links::resolve_workspace_file_link;

/// Shown when a link is blocked.
pub(crate) const BLOCKED_NOTICE: &str = "That link can\u{2019}t be opened from an artifact";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArtifactLink {
    /// A checkout-relative file, optionally at a line/column.
    File {
        path: String,
        line: Option<u32>,
        column: Option<u32>,
    },
    /// The raw `mast-artifact:` link.
    Artifact(String),
    Web(String),
    External(String),
    /// A same-document `#id`: leave it to the page.
    Anchor,
    Blocked(BlockedReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockedReason {
    /// A scheme Mast never follows from an artifact.
    Scheme,
    /// A path that is outside the checkouts, unsafe, or not a plain path.
    Path,
}

/// Schemes that are never a `path:line` even when a digit follows.
const ALWAYS_SCHEMES: &[&str] = &["javascript", "data", "file", "vbscript", "about", "blob"];

pub(crate) fn classify(
    raw: &str,
    canonical_root: Option<&str>,
    chat_cwd: Option<&str>,
) -> ArtifactLink {
    let raw = raw.trim();
    if raw.is_empty() || raw.chars().any(char::is_control) {
        return ArtifactLink::Blocked(BlockedReason::Path);
    }
    if raw.starts_with('#') {
        return ArtifactLink::Anchor;
    }
    let Some(scheme) = url_scheme(raw) else {
        return resolve_file(raw, canonical_root, chat_cwd);
    };
    match scheme.to_ascii_lowercase().as_str() {
        "mast-file" | "zeron-file" => resolve_file(raw, canonical_root, chat_cwd),
        "mast-artifact" | "zeron-artifact" => {
            if super::model::link_target(raw).is_some() {
                ArtifactLink::Artifact(raw.to_owned())
            } else {
                ArtifactLink::Blocked(BlockedReason::Path)
            }
        }
        "http" | "https" => {
            if crate::browser::model::allowed_navigation(raw) {
                ArtifactLink::Web(raw.to_owned())
            } else {
                ArtifactLink::Blocked(BlockedReason::Scheme)
            }
        }
        "mailto" => {
            if url::Url::parse(raw).is_ok() {
                ArtifactLink::External(raw.to_owned())
            } else {
                ArtifactLink::Blocked(BlockedReason::Scheme)
            }
        }
        _ => ArtifactLink::Blocked(BlockedReason::Scheme),
    }
}

/// The URL scheme of `raw`, or `None` for a path. `ui/foo.tsx:120` is a path:
/// a would-be scheme followed by a digit is a line number.
fn url_scheme(raw: &str) -> Option<&str> {
    let (head, rest) = raw.split_once(':')?;
    let mut chars = head.chars();
    if !chars.next()?.is_ascii_alphabetic()
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    let always = ALWAYS_SCHEMES.contains(&head.to_ascii_lowercase().as_str());
    if !always && rest.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some(head)
}

fn resolve_file(raw: &str, canonical_root: Option<&str>, chat_cwd: Option<&str>) -> ArtifactLink {
    let raw = single_line(raw);
    for root in [canonical_root, chat_cwd].into_iter().flatten() {
        if let Some(link) = resolve_workspace_file_link(&raw, root) {
            return ArtifactLink::File {
                path: link.path,
                line: link.line,
                column: link.column,
            };
        }
    }
    ArtifactLink::Blocked(BlockedReason::Path)
}

/// `path:12-20` and `path#L12-L20` open at their first line.
fn single_line(raw: &str) -> String {
    if let Some((path, fragment)) = raw.rsplit_once('#')
        && let Some(range) = fragment.strip_prefix('L')
        && let Some((first, _)) = range.split_once('-')
        && first.parse::<u32>().is_ok()
    {
        return format!("{path}#L{first}");
    }
    if let Some((path, last)) = raw.rsplit_once(':')
        && let Some((first, second)) = last.split_once('-')
        && first.parse::<u32>().is_ok()
        && second.parse::<u32>().is_ok()
    {
        return format!("{path}:{first}");
    }
    raw.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANONICAL: &str = "/repo/main";
    const CHAT: &str = "/repo/.worktrees/chat";

    fn go(raw: &str) -> ArtifactLink {
        classify(raw, Some(CANONICAL), Some(CHAT))
    }

    fn file(path: &str, line: Option<u32>, column: Option<u32>) -> ArtifactLink {
        ArtifactLink::File {
            path: path.into(),
            line,
            column,
        }
    }

    #[test]
    fn path_line_forms_open_files() {
        assert_eq!(
            go("ui/src/foo.tsx:120"),
            file("ui/src/foo.tsx", Some(120), None)
        );
        assert_eq!(
            go("ui/src/foo.tsx:120:7"),
            file("ui/src/foo.tsx", Some(120), Some(7))
        );
        assert_eq!(
            go("ui/src/foo.tsx:120-148"),
            file("ui/src/foo.tsx", Some(120), None)
        );
        assert_eq!(
            go("flagship/orders.py"),
            file("flagship/orders.py", None, None)
        );
        assert_eq!(
            go("flagship/orders.py#L9"),
            file("flagship/orders.py", Some(9), None)
        );
        assert_eq!(
            go("flagship/orders.py#L9-L20"),
            file("flagship/orders.py", Some(9), None)
        );
        assert_eq!(go("foo.tsx:120"), file("foo.tsx", Some(120), None));
        assert_eq!(
            go("mast-file:src/a%20file.rs"),
            file("src/a file.rs", None, None)
        );
        // The pre-rename scheme still resolves.
        assert_eq!(
            go("zeron-file:src/a%20file.rs"),
            file("src/a file.rs", None, None)
        );
    }

    #[test]
    fn legacy_artifact_scheme_still_opens_an_artifact_tab() {
        assert_eq!(
            go("zeron-artifact:art_123"),
            ArtifactLink::Artifact("zeron-artifact:art_123".into())
        );
        assert_eq!(
            go("zeron-artifact:"),
            ArtifactLink::Blocked(BlockedReason::Path)
        );
    }

    #[test]
    fn absolute_paths_resolve_against_canonical_root_then_chat_checkout() {
        assert_eq!(go("/repo/main/ui/a.rs:3"), file("ui/a.rs", Some(3), None));
        assert_eq!(
            go("/repo/.worktrees/chat/ui/a.rs:3"),
            file("ui/a.rs", Some(3), None)
        );
        assert_eq!(
            classify("/repo/.worktrees/chat/ui/a.rs", None, Some(CHAT)),
            file("ui/a.rs", None, None)
        );
        assert_eq!(
            go("/etc/passwd"),
            ArtifactLink::Blocked(BlockedReason::Path)
        );
        // Without any root nothing resolves, relative or not.
        assert_eq!(
            classify("ui/a.rs", None, None),
            ArtifactLink::Blocked(BlockedReason::Path)
        );
    }

    #[test]
    fn routes_artifacts_web_mail_and_anchors() {
        assert_eq!(
            go("mast-artifact:art_123"),
            ArtifactLink::Artifact("mast-artifact:art_123".into())
        );
        assert_eq!(
            go("https://example.com/a?b=1"),
            ArtifactLink::Web("https://example.com/a?b=1".into())
        );
        assert_eq!(
            go("http://example.com"),
            ArtifactLink::Web("http://example.com".into())
        );
        assert_eq!(
            go("mailto:dev@example.com"),
            ArtifactLink::External("mailto:dev@example.com".into())
        );
        assert_eq!(go("#section-2"), ArtifactLink::Anchor);
    }

    #[test]
    fn everything_else_is_blocked() {
        for raw in [
            "file:///etc/passwd",
            "file:///repo/main/a.rs",
            "javascript:alert(1)",
            "javascript:1",
            "data:text/html,<p>x</p>",
            "blob:null/1",
            "about:blank",
            "ftp://example.com/x",
            "zeron://open/chat/a",
            "https://user:pw@example.com/",
        ] {
            assert_eq!(
                go(raw),
                ArtifactLink::Blocked(BlockedReason::Scheme),
                "{raw}"
            );
        }
        for raw in [
            "../secret.rs",
            "./other.html",
            "src/../../x.rs",
            "a.rs?x=1",
            "",
            "a\\b.rs",
            "mast-artifact:",
        ] {
            assert_eq!(
                go(raw),
                ArtifactLink::Blocked(BlockedReason::Path),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn markdown_routes_files_artifacts_mail_and_blocked_schemes() {
        use crate::artifacts::tab::markdown_link_is_routed;
        let root = Some(CANONICAL);
        for routed in [
            "ui/src/foo.tsx:120",
            "flagship/orders.py",
            "mast-file:src/a.rs",
            "mast-artifact:art_1",
            "mailto:a@example.com",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "/etc/passwd",
        ] {
            assert!(markdown_link_is_routed(routed, root), "{routed}");
        }
        for kept in [
            "#heading",
            "https://example.com",
            "../sibling.md",
            "a.md?x=1",
        ] {
            assert!(!markdown_link_is_routed(kept, root), "{kept}");
        }
        // No canonical root: relative paths keep the preview's own handling.
        assert!(!markdown_link_is_routed("ui/a.rs:3", None));
    }

    #[test]
    fn every_document_kind_carries_the_link_bridge() {
        use crate::artifacts::document::{LINK_FRAME_SCRIPT, LINK_HOST_SCRIPT, web_document};
        use crate::artifacts::model::WebKind;
        let theme = crate::browser::fragment::FragmentTheme {
            dark: false,
            vars: Vec::new(),
            font_sans: "sans-serif".into(),
            font_mono: "monospace".into(),
        };
        for (kind, source) in [
            (WebKind::Fragment, "<a href=\"ui/a.rs:1\">a</a>"),
            (
                WebKind::Page,
                "<!doctype html><html><head></head><body><a href=\"ui/a.rs:1\">a</a></body></html>",
            ),
            (WebKind::Mermaid, "graph TD; A-->B"),
        ] {
            for probe in [false, true] {
                let html = web_document(kind, source, "t", "", &theme, false, probe);
                assert_eq!(html.matches("<iframe").count(), 1, "{kind:?}");
                assert!(html.contains(LINK_HOST_SCRIPT), "{kind:?} host");
                // The frame script lives in the escaped srcdoc attribute.
                assert!(html.contains("__mastLinksInstalled"), "{kind:?} frame");
                assert!(!html.contains(LINK_FRAME_SCRIPT), "{kind:?} unescaped");
            }
        }
    }
}
