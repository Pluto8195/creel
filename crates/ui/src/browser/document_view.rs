//! Static-document mode of a Browser surface: a host document built by the
//! caller (planning artifacts: the visualize wrapper around a fragment, a
//! sandboxed page, or a Mermaid diagram) shown in the tab's native page. An
//! embedded surface hides the address toolbar; its owner (the Artifact tab)
//! draws its own chrome around it.
use super::{BrowserEvent, BrowserSurface};
use gpui::{Context, Window};

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
