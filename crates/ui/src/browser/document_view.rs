//! Static-document mode of a Browser surface: a host document built by the
//! caller (planning artifacts: the visualize wrapper around a fragment, a
//! sandboxed page, or a Mermaid diagram) shown in the tab's native page. An
//! embedded surface hides the address toolbar; its owner (the Artifact tab)
//! draws its own chrome around it.
use super::model::allowed_navigation;
use super::{BrowserEvent, BrowserSurface};
use gpui::{Context, Window};

/// Scheme of the bridge the artifact wrapper uses to hand a clicked link to
/// the host: the frame posts the link's raw `href` to the outer document,
/// which activates a hidden `<a href="zeron-link:<encoded href>">`. The
/// navigation delegate cancels it and forwards the decoded `href`
/// ([`DocumentAction::Link`]). Raw `href`s matter: a relative `ui/foo.tsx:120`
/// never resolves to a URL inside an `about:srcdoc` frame.
pub(crate) const LINK_BRIDGE_SCHEME: &str = "zeron-link:";

/// URLs a string-loaded host document and its srcdoc frame commit under.
pub(super) fn document_navigation(url: &str) -> bool {
    url == "about:blank"
        || url
            .strip_prefix("about:srcdoc")
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('#'))
}

/// What the host does besides allowing or cancelling a navigation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DocumentAction {
    None,
    /// A web link: ordinary Browser tab / web-link preference.
    NewTab(String),
    /// Any other activated link, as the page wrote it (the owner routes it).
    Link(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DocumentDecision {
    pub allow: bool,
    pub action: DocumentAction,
}

/// Policy for a navigation in a string-loaded document: only its own `about:`
/// frames ever load; every user-activated link is cancelled and reported.
/// `link_activated` is `WKNavigationTypeLinkActivated`.
pub(super) fn document_decision(url: &str, link_activated: bool) -> DocumentDecision {
    let cancel = |action| DocumentDecision {
        allow: false,
        action,
    };
    if document_navigation(url) {
        return DocumentDecision {
            allow: true,
            action: DocumentAction::None,
        };
    }
    if !link_activated {
        return cancel(DocumentAction::None);
    }
    if let Some(encoded) = url.strip_prefix(LINK_BRIDGE_SCHEME) {
        return cancel(
            crate::workspace_links::percent_decode_path(encoded)
                .map_or(DocumentAction::None, DocumentAction::Link),
        );
    }
    if allowed_navigation(url) {
        cancel(DocumentAction::NewTab(url.to_owned()))
    } else {
        cancel(DocumentAction::Link(url.to_owned()))
    }
}

/// What a static-document surface shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticDocument {
    pub title: String,
    pub html: String,
}

impl BrowserSurface {
    /// The document on screen in static-document mode.
    pub fn static_document(&self) -> Option<&StaticDocument> {
        self.document.as_ref()
    }

    /// Hide the address toolbar (the owner draws its own chrome).
    pub fn set_embedded(&mut self, embedded: bool, cx: &mut Context<Self>) {
        if self.embedded != embedded {
            self.embedded = embedded;
            cx.notify();
        }
    }

    pub fn is_embedded(&self) -> bool {
        self.embedded
    }

    /// Show `html` (a complete host document) in the native page. Like a
    /// visualization, it loads from a string with no base URL, so nothing in
    /// it can address `file://`; link activations become `NewTab` events.
    pub fn show_document(
        &mut self,
        title: String,
        html: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .document
            .as_ref()
            .is_some_and(|doc| doc.title == title && doc.html == html)
            && self.page.error.is_none()
        {
            return;
        }
        self.fragment = None;
        self.page = super::model::PageState {
            title: title.clone(),
            ..Default::default()
        };
        self.document = Some(StaticDocument { title, html });
        self.load_static_document(window, cx);
    }

    pub(super) fn load_static_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(document) = self.document.as_ref().map(|doc| doc.html.clone()) else {
            return;
        };
        self.page.error = None;
        #[cfg(target_os = "macos")]
        {
            let result = if let Some(native) = &self.native {
                native.load_document(&document)
            } else {
                super::native::NativePage::new(window, &self.context.data, self.native_tx.clone())
                    .map(|mut native| {
                        native.present(self.presentation);
                        native.set_extra_forwarded_keys(self.extra_forwarded_keys.clone());
                        self.native = Some(native);
                    })
                    .and_then(|_| self.native.as_ref().unwrap().load_document(&document))
            };
            self.page.loading = result.is_ok();
            if let Err(error) = result {
                self.page.error = Some(format!("Could not show this artifact: {error}"));
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (window, document);
            self.page.error =
                Some("Rendered artifacts aren\u{2019}t available on this platform yet.".into());
        }
        cx.emit(BrowserEvent::Changed);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::links::{ArtifactLink, BlockedReason, classify};

    /// What the Zeron side does for a fake navigation action: `None` for an
    /// allowed (in-page) navigation, else the classification of the link the
    /// delegate reports.
    fn route(url: &str, link_activated: bool) -> (bool, Option<ArtifactLink>) {
        let decision = document_decision(url, link_activated);
        let link = match decision.action {
            DocumentAction::None => None,
            DocumentAction::NewTab(url) | DocumentAction::Link(url) => {
                Some(classify(&url, Some("/repo/main"), Some("/repo/chat")))
            }
        };
        (decision.allow, link)
    }

    fn bridged(href: &str) -> String {
        let mut out = String::from(LINK_BRIDGE_SCHEME);
        for byte in href.bytes() {
            if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
                out.push(byte as char);
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
        out
    }

    #[test]
    fn in_page_navigation_is_allowed_untouched() {
        for url in ["about:blank", "about:srcdoc", "about:srcdoc#totals"] {
            assert_eq!(route(url, false), (true, None), "{url}");
            assert_eq!(route(url, true), (true, None), "{url}");
        }
    }

    #[test]
    fn every_scheme_cancels_with_its_zeron_action() {
        let file = |path: &str, line| ArtifactLink::File {
            path: path.into(),
            line,
            column: None,
        };
        let cases: Vec<(String, ArtifactLink)> = vec![
            (
                bridged("ui/src/foo.tsx:120"),
                file("ui/src/foo.tsx", Some(120)),
            ),
            (
                bridged("ui/src/foo.tsx:120-148"),
                file("ui/src/foo.tsx", Some(120)),
            ),
            (
                bridged("flagship/orders.py"),
                file("flagship/orders.py", None),
            ),
            (bridged("zeron-file:src/a%20b.rs"), file("src/a b.rs", None)),
            (
                bridged("zeron-artifact:art_1"),
                ArtifactLink::Artifact("zeron-artifact:art_1".into()),
            ),
            (
                bridged("https://example.com/x?y=1"),
                ArtifactLink::Web("https://example.com/x?y=1".into()),
            ),
            (
                "https://example.com/x".into(),
                ArtifactLink::Web("https://example.com/x".into()),
            ),
            (
                bridged("mailto:a@example.com"),
                ArtifactLink::External("mailto:a@example.com".into()),
            ),
            (bridged("#later"), ArtifactLink::Anchor),
            (
                bridged("file:///etc/passwd"),
                ArtifactLink::Blocked(BlockedReason::Scheme),
            ),
            (
                bridged("javascript:alert(1)"),
                ArtifactLink::Blocked(BlockedReason::Scheme),
            ),
            (
                bridged("data:text/html,<p>x</p>"),
                ArtifactLink::Blocked(BlockedReason::Scheme),
            ),
            (
                "file:///etc/passwd".into(),
                ArtifactLink::Blocked(BlockedReason::Scheme),
            ),
            (
                bridged("../other.html"),
                ArtifactLink::Blocked(BlockedReason::Path),
            ),
        ];
        for (url, expected) in cases {
            assert_eq!(route(&url, true), (false, Some(expected)), "{url}");
        }
    }

    #[test]
    fn navigations_that_are_not_link_clicks_are_cancelled_silently() {
        for url in [
            "https://example.com/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "zeron-link:ui%2Ffoo.tsx",
            "data:text/html,x",
        ] {
            assert_eq!(route(url, false), (false, None), "{url}");
        }
        // A malformed bridge payload is dropped rather than guessed at.
        assert_eq!(route("zeron-link:%FF%FE", true), (false, None));
    }
}
